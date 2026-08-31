#!/usr/bin/env bash
# Restore plugin-owned configuration, then unlink herdr-agent-quota.
#
# Usage:
#   ./uninstall.sh                    # restore config, then unlink
#   ./uninstall.sh --agent grok       # remove only that agent, stay installed
#
# A full uninstall also drops the saved sidebar-layout and row-gap prefs.
#
# The configuration action is intentionally run before unlinking: Herdr owns
# the plugin state directory used to restore Claude/Agy statusLine backups.
set -euo pipefail

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

command -v herdr >/dev/null 2>&1 || {
  printf 'error: Herdr is not installed or not on PATH\n' >&2
  exit 1
}

if herdr plugin list 2>/dev/null | grep -q 'herdr-agent-quota'; then
  # An earlier interrupted uninstall may have disabled the plugin. Enable it
  # long enough for Herdr to provide the state directory to the restore action.
  herdr plugin enable herdr-agent-quota >/dev/null 2>&1 || true
  printf '%s\n' '→ restoring plugin-owned configuration'
  env ${AGENTS:+HERDR_AGENT_QUOTA_AGENTS="$AGENTS"} \
    herdr plugin action invoke herdr-agent-quota.uninstall

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
