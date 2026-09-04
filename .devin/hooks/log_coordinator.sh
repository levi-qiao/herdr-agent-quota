#!/usr/bin/env bash
# Coordinator action logger — wraps log_task.py for one-line logging.
#
# Usage:
#   .devin/hooks/log_coordinator.sh <action> <description> [details_json]
#
# Examples:
#   .devin/hooks/log_coordinator.sh plan "Created PLAN.md for UI test fixes"
#   .devin/hooks/log_coordinator.sh delegate "Delegated to rust-developer" '{"agent":"rust-developer","task":"Add omp provider test","background":true}'
#   .devin/hooks/log_coordinator.sh integrate "Merged omp provider test, verified all gates pass"
#   .devin/hooks/log_coordinator.sh commit "Committed a8532b5 on feature/omp-test"
#   .devin/hooks/log_coordinator.sh verify "Full suite: cargo test pass, clippy clean, audit clean"
#   .devin/hooks/log_coordinator.sh decision "Chose to fix source bug over mocking" '{"reason":"real bug in else branch"}'
#   .devin/hooks/log_coordinator.sh recovery "Recovered lost sub-agent work from detailed report"

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TASK_ID="coord-$(date +%Y%m%d)-$(basename "$(pwd)")"

ACTION="${1:?Usage: log_coordinator.sh <action> <description> [details_json]}"
DESCRIPTION="${2:?Usage: log_coordinator.sh <action> <description> [details_json]}"
DETAILS="${3:-}"

# Use Python to safely construct valid JSON with the action embedded
DETAILS_JSON=$(python3 -c "
import json, sys
action = sys.argv[1]
description = sys.argv[2]
raw = sys.argv[3] if len(sys.argv) > 3 else ''
try:
    context = json.loads(raw) if raw else {}
except json.JSONDecodeError:
    context = {'raw': raw}
result = {'action': action, 'description': description}
if context:
    result['context'] = context
print(json.dumps(result, ensure_ascii=False))
" "$ACTION" "$DESCRIPTION" "$DETAILS")

python3 "${SCRIPT_DIR}/log_task.py" \
    --event coordinator_action \
    --task-id "${TASK_ID}" \
    --task-name "${DESCRIPTION}" \
    --agent-type "coordinator" \
    --agent-name "coordinator" \
    --details "${DETAILS_JSON}" \
    2>&1 || true
