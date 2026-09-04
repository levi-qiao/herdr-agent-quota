#!/usr/bin/env bash
# PreToolUse hook: Block the coordinator from editing/writing code files.
# The coordinator MUST NOT implement code directly (per AGENTS.md).
# Only allows edits to tasks/** and .devin/** (spec files and config).
#
# Input (stdin): JSON with hook_event_name, tool_name, tool_input
# Output (stdout): JSON with decision and reason, OR exit code 2 to block

set -euo pipefail

input=$(cat)
tool_name=$(echo "$input" | python3 -c "import sys,json; print(json.load(sys.stdin).get('tool_name',''))" 2>/dev/null || echo "")

# Only check edit and write tools
if [[ "$tool_name" != "edit" && "$tool_name" != "write" ]]; then
  exit 0
fi

# Extract file_path from tool_input
file_path=$(echo "$input" | python3 -c "import sys,json; print(json.load(sys.stdin).get('tool_input',{}).get('file_path',''))" 2>/dev/null || echo "")

# If no file_path, allow (shouldn't happen for edit/write)
if [[ -z "$file_path" ]]; then
  exit 0
fi

# Get the project root from DEVIN_PROJECT_DIR or fall back to cwd
project_root="${DEVIN_PROJECT_DIR:-$(pwd)}"

# Convert to relative path for checking
rel_path="${file_path#"$project_root"/}"

# Allowed paths for coordinator writes (spec files, config, hooks)
# tasks/**  — spec files
# .devin/** — Devin config, hooks, scripts
allowed_patterns=(
  "tasks/"
  ".devin/"
)

for pattern in "${allowed_patterns[@]}"; do
  if [[ "$rel_path" == "$pattern"* ]]; then
    exit 0
  fi
done

# Block the edit/write — this is a code file
cat <<'EOF'
{
  "decision": "block",
  "reason": "Coordinator is NOT allowed to edit code files. Per AGENTS.md, the coordinator MUST NOT implement code directly — it plans, delegates, integrates, and verifies. Delegate this edit to a specialist subagent (rust-developer, etc.). Only tasks/** and .devin/** files are allowed for coordinator edits."
}
EOF
exit 0
