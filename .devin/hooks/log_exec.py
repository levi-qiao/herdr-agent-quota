#!/usr/bin/env python3
"""Log exec tool calls to a local file for command history auditing.

This script is invoked by the Devin CLI PostToolUse hook. It reads the hook
event JSON from stdin and appends a JSON Lines record containing the command,
metadata, and outcome to ~/.devin-exec-commands.log.
"""

import datetime
import json
import os
import re
import sys
from contextlib import contextmanager
from pathlib import Path

try:
    import fcntl

    @contextmanager
    def _file_lock(file):
        try:
            fcntl.flock(file.fileno(), fcntl.LOCK_EX)
            yield
        finally:
            fcntl.flock(file.fileno(), fcntl.LOCK_UN)

except ImportError:
    # Windows fallback: no advisory locking available in stdlib
    @contextmanager
    def _file_lock(file):
        yield


MAX_LOG_BYTES = 10 * 1024 * 1024  # 10 MB per file


def _rotate_if_needed(path: Path) -> None:
    """Rotate the log file if it exceeds the max size."""
    try:
        if path.exists() and path.stat().st_size >= MAX_LOG_BYTES:
            backup = path.with_suffix(path.suffix + ".1")
            if backup.exists():
                backup.unlink()
            path.rename(backup)
    except OSError:
        pass  # rotation failure is non-fatal


def _ensure_private_perms(path: Path) -> None:
    """Set file permissions to 0600 (owner read/write only)."""
    try:
        path.chmod(0o600)
    except OSError:
        pass  # non-fatal


_SECRET_RE = re.compile(
    r"""(?i)(api[_-]?key|secret[_-]?key|password|passwd|token|auth[_-]?token|access[_-]?token|bearer|private[_-]?key|authorization)\s*[:=]\s*[^\s&"'|,;]+"""
)
_AUTH_BEARER_RE = re.compile(r"(?i)(Authorization:\s*Bearer\s+)[\w\-_./+=]+")


def _redact(text: str) -> str:
    """Redact common secret patterns from text before logging."""
    if not text:
        return text
    text = _SECRET_RE.sub(r"\1=***REDACTED***", text)
    text = _AUTH_BEARER_RE.sub(r"\1***REDACTED***", text)
    return text


def _append_record(path: Path, record: dict) -> None:
    """Append a single record as one JSON line (atomic, O(1))."""
    _rotate_if_needed(path)
    _ensure_private_perms(path)  # tighten perms on existing file
    line = json.dumps(record, ensure_ascii=False, default=str) + "\n"
    with path.open("a", encoding="utf-8") as f, _file_lock(f):
        f.write(line)
    _ensure_private_perms(path)  # ensure newly created file is private


def main() -> int:
    try:
        data = json.load(sys.stdin)
    except json.JSONDecodeError as e:
        print(f"devin hook: invalid JSON on stdin: {e}", file=sys.stderr)
        return 1

    # Debug mode: log full event data to verify available fields
    # NOTE: Devin CLI hook events only contain: hook_event_name, tool_name,
    # tool_input, tool_use_id, tool_response. There is NO agent_id, agent_type,
    # session_id, or cwd in the event JSON. Only DEVIN_PROJECT_DIR env var is set.
    if os.environ.get("DEVIN_HOOK_DEBUG"):
        debug_path = Path.home() / ".devin-hook-debug.log"
        try:
            _ensure_private_perms(debug_path)
            redacted = _redact(json.dumps(data, ensure_ascii=False, indent=2))
            with debug_path.open("a", encoding="utf-8") as f:
                f.write(redacted + "\n---\n")
            _ensure_private_perms(debug_path)
        except Exception:
            pass

    tool_input = data.get("tool_input", {})
    tool_response = data.get("tool_response", {})

    command = tool_input.get("command", "")
    if not command:
        return 0

    output_text = tool_response.get("output", "") or ""
    error_text = tool_response.get("error", "") or ""

    # Limit output/error to keep the log readable and bounded
    max_snippet = 2000
    output_snippet = output_text[:max_snippet]
    error_snippet = error_text[:max_snippet]

    # Extract metadata from the event data (stdin JSON).
    # Verified Devin CLI hook event fields (as of 2026-07-07):
    #   - hook_event_name: present (e.g. "PostToolUse")
    #   - tool_name: present (e.g. "exec")
    #   - tool_input: present (tool arguments)
    #   - tool_use_id: present (correlates PreToolUse/PostToolUse events)
    #   - tool_response: present for PostToolUse (success, output, error)
    #   - cwd: NOT in event JSON — use DEVIN_PROJECT_DIR env var
    #   - agent_id: NOT available (Devin CLI does not provide this)
    #   - agent_type: NOT available (Devin CLI does not provide this)
    #   - session_id: NOT available (Devin CLI does not provide this)
    # These agent/session fields are kept in the record for forward-compatibility
    # in case Devin CLI adds them in the future.
    agent_id = data.get("agent_id") or os.environ.get("DEVIN_AGENT_ID")
    agent_type = data.get("agent_type") or os.environ.get("DEVIN_AGENT_TYPE")
    session_id = data.get("session_id") or os.environ.get("DEVIN_SESSION_ID")
    cwd = data.get("cwd") or os.environ.get("DEVIN_PROJECT_DIR")
    task_id = data.get("task_id") or os.environ.get("DEVIN_TASK_ID")
    parent_task_id = data.get("parent_task_id") or os.environ.get(
        "DEVIN_PARENT_TASK_ID"
    )
    hook_event_name = data.get("hook_event_name")
    tool_use_id = data.get("tool_use_id")

    record = {
        "timestamp": datetime.datetime.now(datetime.UTC).isoformat(),
        "hook_event_name": hook_event_name,
        "tool_name": data.get("tool_name"),
        "tool_use_id": tool_use_id,
        "command": command,
        "shell_id": tool_input.get("shell_id"),
        "timeout": tool_input.get("timeout"),
        "idle_timeout": tool_input.get("idle_timeout"),
        "run_in_background": tool_input.get("run_in_background"),
        "interactive_shell": tool_input.get("interactive_shell"),
        "output_processing": tool_input.get("output_processing"),
        "cwd": cwd,
        "task_id": task_id,
        "parent_task_id": parent_task_id,
        "agent_id": agent_id,
        "agent_type": agent_type,
        "session_id": session_id,
        "success": tool_response.get("success"),
        "output_length": len(output_text),
        "output_snippet": output_snippet,
        "error_length": len(error_text),
        "error_snippet": error_snippet,
    }

    log_path = Path.home() / ".devin-exec-commands.log"
    try:
        _append_record(log_path, record)
    except Exception as e:
        print(f"devin hook: failed to write audit record: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
