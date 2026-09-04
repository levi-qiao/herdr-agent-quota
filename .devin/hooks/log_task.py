#!/usr/bin/env python3
"""Log sub-agent task lifecycle events to a local JSON Lines file.

This script is invoked by the coordinator or sub-agent to record task
lifecycle events (started, progress, completed, failed) to
~/.devin-tasks.log. It can be called with CLI arguments or environment
variables.
"""

import argparse
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


def _redact_details(details: str | dict | None) -> str | dict | None:
    """Redact secrets from a string or JSON-serializable details field."""
    if details is None:
        return None
    if isinstance(details, dict):
        text = json.dumps(details, ensure_ascii=False)
    else:
        text = str(details)
    text = _redact(text)
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return text


def _append_record(path: Path, record: dict) -> None:
    """Append a single record as one JSON line (atomic, O(1))."""
    _rotate_if_needed(path)
    _ensure_private_perms(path)
    line = json.dumps(record, ensure_ascii=False, default=str) + "\n"
    with path.open("a", encoding="utf-8") as f, _file_lock(f):
        f.write(line)
    _ensure_private_perms(path)


def _status_from_event(event_type: str) -> str:
    """Derive a task status from the event type."""
    mapping = {
        "started": "processing",
        "progress": "processing",
        "completed": "completed",
        "failed": "failed",
        "coordinator_action": "coordinator",
    }
    return mapping.get(event_type, "unknown")


def _parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Log a sub-agent task lifecycle event."
    )
    parser.add_argument(
        "--event",
        required=True,
        choices=["started", "progress", "completed", "failed", "coordinator_action"],
        help="Lifecycle event type. Use 'coordinator_action' for coordinator-level decisions.",
    )
    parser.add_argument("--task-id", default=None, help="Task identifier.")
    parser.add_argument("--task-name", default=None, help="Human-readable task name.")
    parser.add_argument(
        "--parent-task-id", default=None, help="Parent task identifier (if any)."
    )
    parser.add_argument("--agent-id", default=None, help="Agent identifier.")
    parser.add_argument("--agent-type", default=None, help="Agent type/profile.")
    parser.add_argument("--agent-name", default=None, help="Agent name.")
    parser.add_argument("--status", default=None, help="Override status.")
    parser.add_argument(
        "--progress",
        type=int,
        default=None,
        help="Progress percentage (0-100).",
    )
    parser.add_argument(
        "--details",
        default=None,
        help="JSON or plain-text details. Secrets are redacted.",
    )
    parser.add_argument(
        "--log-path",
        default=None,
        help="Override log file path (default: ~/.devin-tasks.log).",
    )
    return parser.parse_args(argv)


def _resolve_value(
    arg_value: str | None, env_var: str, default: str | None = None
) -> str | None:
    """Resolve a value from CLI arg or environment variable."""
    if arg_value is not None:
        return arg_value
    return os.environ.get(env_var, default) or None


def _parse_details(details: str | None) -> str | dict | None:
    """Parse details string into a string or dict."""
    if details is None:
        return None
    try:
        return json.loads(details)
    except json.JSONDecodeError:
        return details


def _validate_progress(progress: int | None) -> int | None:
    if progress is None:
        return None
    if progress < 0 or progress > 100:
        raise ValueError("progress must be between 0 and 100")
    return progress


def main(argv: list[str] | None = None) -> int:
    try:
        args = _parse_args(argv)
    except SystemExit as e:
        return e.code if isinstance(e.code, int) else 1

    task_id = _resolve_value(args.task_id, "DEVIN_TASK_ID")
    task_name = _resolve_value(args.task_name, "DEVIN_TASK_NAME")
    parent_task_id = _resolve_value(args.parent_task_id, "DEVIN_PARENT_TASK_ID")
    agent_id = _resolve_value(args.agent_id, "DEVIN_AGENT_ID")
    agent_type = _resolve_value(args.agent_type, "DEVIN_AGENT_TYPE")
    agent_name = _resolve_value(args.agent_name, "DEVIN_AGENT_NAME")
    session_id = _resolve_value(None, "DEVIN_SESSION_ID")
    cwd = _resolve_value(None, "DEVIN_PROJECT_DIR")

    if not task_id:
        print(
            "devin hook: task_id is required (use --task-id or DEVIN_TASK_ID)",
            file=sys.stderr,
        )
        return 1

    try:
        progress = _validate_progress(args.progress)
    except ValueError as e:
        print(f"devin hook: invalid progress: {e}", file=sys.stderr)
        return 1

    event_type = args.event
    status = args.status if args.status else _status_from_event(event_type)
    details = _parse_details(args.details)
    details = _redact_details(details)

    record = {
        "timestamp": datetime.datetime.now(datetime.UTC).isoformat(),
        "hook_event_name": "TaskLifecycle",
        "task_id": task_id,
        "parent_task_id": parent_task_id,
        "task_name": task_name,
        "agent_id": agent_id,
        "agent_type": agent_type,
        "agent_name": agent_name,
        "session_id": session_id,
        "cwd": cwd,
        "event_type": event_type,
        "status": status,
        "progress": progress,
        "details": details,
    }

    log_path = (
        Path(args.log_path) if args.log_path else Path.home() / ".devin-tasks.log"
    )
    try:
        _append_record(log_path, record)
    except Exception as e:
        print(
            f"devin hook: failed to write task lifecycle record: {e}",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
