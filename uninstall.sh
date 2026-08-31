#!/usr/bin/env bash
# Restore plugin-owned configuration, then unlink herdr-agent-quota.
#
# Usage:
#   ./uninstall.sh                    # restore config, then unlink
#   ./uninstall.sh --agent grok       # remove only that agent, stay installed
#
# A full uninstall also drops the saved sidebar-layout, row-gap,
# quota-percent, fields, brand-colors, agent-order, and low-quota-alert prefs,
# and hands Herdr's agent panel back its own ordering.
#
# The restore action runs, and is waited for, before unlinking: Herdr owns the
# plugin state directory holding the Claude/Agy statusLine backups, and
# `herdr plugin action invoke` returns before the action has finished.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=scripts/herdr-action.sh
source "$ROOT/scripts/herdr-action.sh"

AGENTS=""
while (($# > 0)); do
  case "$1" in
    --agent)
      (($# >= 2)) || { printf 'error: missing value for %s\n' "$1" >&2; exit 1; }
      AGENTS="$2"
      shift 2
      ;;
    -h|--help)
      sed -n '2,10p' "$0"
      exit 0
      ;;
    *)
      printf 'error: unknown argument: %s\n' "$1" >&2
      exit 1
      ;;
  esac
done

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

command -v herdr >/dev/null 2>&1 || die "Herdr is not installed or not on PATH"

# Herdr runs the uninstall action with a fixed command line, in the server's own
# environment, so `env AGENTS=... herdr plugin action invoke` is silently
# ignored — and an ignored selection here means removing everything instead of
# one agent. The selection therefore travels through the plugin config
# directory, and is restored afterwards so that removing one agent never
# narrows a later repair of the agents that are still installed.
AGENTS_PREF=""
AGENTS_PREF_SAVED=""
AGENTS_PREF_EXISTED=0

restore_agents_pref() {
  [[ -z "$AGENTS_PREF" ]] && return 0
  if ((AGENTS_PREF_EXISTED)); then
    printf '%s\n' "$AGENTS_PREF_SAVED" > "$AGENTS_PREF"
  else
    rm -f "$AGENTS_PREF"
  fi
}

select_agents() {
  [[ -z "$AGENTS" ]] && return 0
  local directory
  directory="$(herdr plugin config-dir herdr-agent-quota)" \
    || die "cannot resolve plugin config directory"
  mkdir -p "$directory"
  AGENTS_PREF="$directory/agents"
  if [[ -f "$AGENTS_PREF" ]]; then
    AGENTS_PREF_SAVED="$(cat "$AGENTS_PREF")"
    AGENTS_PREF_EXISTED=1
  fi
  trap restore_agents_pref EXIT
  printf '%s\n' "$AGENTS" > "$AGENTS_PREF"
}

if herdr plugin list 2>/dev/null | grep -q 'herdr-agent-quota'; then
  # An earlier interrupted uninstall may have disabled the plugin. Enable it
  # long enough for Herdr to provide the state directory to the restore action.
  herdr plugin enable herdr-agent-quota >/dev/null 2>&1 || true
  select_agents
  printf '%s\n' '→ restoring plugin-owned configuration'
  # Waiting matters twice here: the selection file below must stay in place
  # until the action has read it, and unlinking before the restore finishes
  # can strand a statusLine entry pointing at a plugin that is gone.
  invoke_action_and_wait uninstall || die "restore action failed; nothing was unlinked"

  # Removing one agent is not uninstalling the plugin; the rest still need it.
  if [[ -n "$AGENTS" && "$AGENTS" != "all" ]]; then
    printf '%s\n' "Removed $AGENTS. The plugin stays linked for the other agents."
    exit 0
  fi

  printf '%s\n' '→ disabling and unlinking the Herdr plugin'
  herdr plugin disable herdr-agent-quota || true
  herdr plugin unlink herdr-agent-quota
  printf '%s\n' 'Uninstalled and restored.'
else
  printf '%s\n' 'herdr-agent-quota is not linked; no configuration was changed.'
fi
