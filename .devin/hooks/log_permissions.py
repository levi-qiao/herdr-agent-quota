#!/usr/bin/env python3
"""Log PermissionRequest events to a local file for auditing.

This script is invoked by the Devin CLI PermissionRequest hook. It reads the
hook event JSON from stdin and appends a JSON Lines record to
~/.devin-permission-requests.log.

This captures every time an agent or sub-agent needs permission to perform
an action (e.g. running a command that isn't on the allowlist).
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

    # Debug mode: log full event data
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

    # Extract all available context from the event data
    agent_id = data.get("agent_id") or os.environ.get("DEVIN_AGENT_ID")
    agent_type = data.get("agent_type") or os.environ.get("DEVIN_AGENT_TYPE")
    session_id = data.get("session_id") or os.environ.get("DEVIN_SESSION_ID")
    cwd = data.get("cwd") or os.environ.get("DEVIN_PROJECT_DIR")
    task_id = data.get("task_id") or os.environ.get("DEVIN_TASK_ID")
    parent_task_id = data.get("parent_task_id") or os.environ.get(
        "DEVIN_PARENT_TASK_ID"
    )

    # For exec tool, extract the command; for other tools, log only the
    # tool name. The full tool_input may contain entire file contents,
    # private keys, source code, or credentials, so it must NOT be logged.
    tool_name = data.get("tool_name", "")
    if tool_name == "exec":
        command = tool_input.get("command", "")
    else:
        command = f"<{tool_name}>"

    # Extract only safe, non-sensitive fields for audit purposes.
    _safe_fields = {
        "file_path",
        "shell_id",
        "timeout",
        "idle_timeout",
        "pattern",
        "path",
    }
    safe_args = {}
    for field in _safe_fields:
        if field in tool_input:
            val = tool_input[field]
            # Truncate long values to avoid logging excessive data
            if isinstance(val, str) and len(val) > 200:
                val = val[:200] + "...[truncated]"
            safe_args[field] = val

    record = {
        "timestamp": datetime.datetime.now(datetime.UTC).isoformat(),
        "hook_event_name": data.get("hook_event_name", "PermissionRequest"),
        "tool_name": tool_name,
        "tool_use_id": data.get("tool_use_id"),
        "command": command,
        "cwd": cwd,
        "task_id": task_id,
        "parent_task_id": parent_task_id,
        "agent_id": agent_id,
        "agent_type": agent_type,
        "session_id": session_id,
        "safe_args": safe_args if safe_args else None,
        # NO tool_input field -- removed to prevent credential leakage
    }

    log_path = Path.home() / ".devin-permission-requests.log"
    try:
        _append_record(log_path, record)
    except Exception as e:
        print(f"devin hook: failed to write audit record: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
