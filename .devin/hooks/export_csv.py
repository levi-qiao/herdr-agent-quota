#!/usr/bin/env python3
"""Export permission request, exec command, and task lifecycle logs to CSV.

Usage:
    python3 .devin/hooks/export_csv.py                    # Export all logs
    python3 .devin/hooks/export_csv.py --permissions      # Export permission requests only
    python3 .devin/hooks/export_csv.py --exec             # Export exec commands only
    python3 .devin/hooks/export_csv.py --tasks            # Export task lifecycle only
    python3 .devin/hooks/export_csv.py --output /tmp/log.csv  # Custom output path

Outputs:
    - ~/.devin-permission-requests.csv  (permission requests)
    - ~/.devin-exec-commands.csv        (exec commands)
    - ~/.devin-tasks.csv                (task lifecycle)
"""

import argparse
import csv
import json
import sys
from pathlib import Path


def _sanitize_csv(value) -> str:
    """Sanitize a value for CSV output to prevent formula injection.

    Prefixes dangerous leading characters (=, +, -, @, tab, CR) with a
    single quote so spreadsheet applications don't interpret them as formulas.
    """
    if value is None:
        return ""
    s = str(value)
    if s and s[0] in ("=", "+", "-", "@", "\t", "\r"):
        return "'" + s
    return s


def _ensure_private_perms(path: Path) -> None:
    """Set file permissions to 0600 (owner read/write only)."""
    try:
        path.chmod(0o600)
    except OSError:
        pass  # non-fatal — permissions may not be changeable on all systems


def load_log(path: Path) -> list[dict]:
    if not path.exists():
        return []
    text = path.read_text(encoding="utf-8").strip()
    if not text:
        return []
    try:
        records = json.loads(text)
        if not isinstance(records, list):
            records = [records]
        return records
    except json.JSONDecodeError:
        records = []
        for line in text.splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError:
                continue
        return records


def export_permissions(output_path: Path | None = None) -> int:
    log_path = Path.home() / ".devin-permission-requests.log"
    records = load_log(log_path)
    if not records:
        print(f"No permission request records found in {log_path}")
        return 0

    out = output_path or (Path.home() / ".devin-permission-requests.csv")
    fields = [
        "timestamp",
        "agent_id",
        "agent_type",
        "session_id",
        "task_id",
        "parent_task_id",
        "tool_name",
        "command",
        "cwd",
    ]

    # Ensure existing file has private permissions before writing
    if out.exists():
        _ensure_private_perms(out)

    with out.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fields, extrasaction="ignore")
        writer.writeheader()
        for r in records:
            writer.writerow({k: _sanitize_csv(r.get(k)) for k in fields})

    _ensure_private_perms(out)
    print(f"Exported {len(records)} permission request records to {out}")
    return len(records)


def export_exec(output_path: Path | None = None) -> int:
    log_path = Path.home() / ".devin-exec-commands.log"
    records = load_log(log_path)
    if not records:
        print(f"No exec command records found in {log_path}")
        return 0

    out = output_path or (Path.home() / ".devin-exec-commands.csv")
    fields = [
        "timestamp",
        "agent_id",
        "agent_type",
        "session_id",
        "task_id",
        "parent_task_id",
        "tool_name",
        "command",
        "success",
        "cwd",
    ]

    # Ensure existing file has private permissions before writing
    if out.exists():
        _ensure_private_perms(out)

    with out.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fields, extrasaction="ignore")
        writer.writeheader()
        for r in records:
            writer.writerow({k: _sanitize_csv(r.get(k)) for k in fields})

    _ensure_private_perms(out)
    print(f"Exported {len(records)} exec command records to {out}")
    return len(records)


def export_tasks(output_path: Path | None = None) -> int:
    log_path = Path.home() / ".devin-tasks.log"
    records = load_log(log_path)
    if not records:
        print(f"No task lifecycle records found in {log_path}")
        return 0

    out = output_path or (Path.home() / ".devin-tasks.csv")
    fields = [
        "timestamp",
        "task_id",
        "parent_task_id",
        "task_name",
        "agent_id",
        "agent_type",
        "agent_name",
        "session_id",
        "event_type",
        "status",
        "progress",
        "cwd",
    ]

    if out.exists():
        _ensure_private_perms(out)

    with out.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fields, extrasaction="ignore")
        writer.writeheader()
        for r in records:
            writer.writerow({k: _sanitize_csv(r.get(k)) for k in fields})

    _ensure_private_perms(out)
    print(f"Exported {len(records)} task lifecycle records to {out}")
    return len(records)


def main() -> int:
    parser = argparse.ArgumentParser(description="Export Devin hook logs to CSV")
    parser.add_argument(
        "--permissions", action="store_true", help="Export permission requests only"
    )
    parser.add_argument("--exec", action="store_true", help="Export exec commands only")
    parser.add_argument(
        "--tasks", action="store_true", help="Export task lifecycle only"
    )
    parser.add_argument(
        "--output", "-o", type=str, default=None, help="Custom output CSV path"
    )
    args = parser.parse_args()

    output_path = Path(args.output) if args.output else None

    # If no specific log flag is set, export all logs
    do_all = not args.permissions and not args.exec and not args.tasks

    count = 0
    if args.permissions or do_all:
        count += export_permissions(output_path if args.permissions else None)
    if args.exec or do_all:
        count += export_exec(output_path if args.exec else None)
    if args.tasks or do_all:
        count += export_tasks(output_path if args.tasks else None)

    if count == 0:
        print("No records to export.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
