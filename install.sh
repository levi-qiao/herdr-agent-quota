#!/usr/bin/env bash
# Build, link, enable, and configure herdr-agent-quota in one step.
#
# Usage:
#   ./install.sh
#   ./install.sh --agent claude,codex
#   ./install.sh --watch-interval-seconds 300
#   ./install.sh --sidebar-layout stacked
#   ./install.sh --row-gap 0
#
# --agent installs only the agents you name (all, claude, codex, grok, agy,
# opencode, pi). Anything you leave out gets no sidebar row, no statusLine entry
# and no hook file. The default is every supported agent.
#
# --sidebar-layout packed (default) joins cache/TTL and 5h/7d on one row.
# stacked puts provider, model, cache, TTL, context, 5h, and 7d on their own rows.
#
# --row-gap 1 (default) leaves one blank row between agent panes; 0 packs them
# flush. Herdr only accepts whole rows.
#
# Layout and gap are written to the plugin config directory before configure
# runs, because Herdr plugin actions use a fixed command line.
#
# The matching uninstall.sh first restores every configuration entry owned by
# this plugin and only then unlinks it from Herdr.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
WATCH_INTERVAL_SECONDS=""
AGENTS=""
SIDEBAR_LAYOUT=""
ROW_GAP=""

while (($# > 0)); do
  case "$1" in
    --watch-interval-seconds)
      (($# >= 2)) || { printf 'error: missing value for %s\n' "$1" >&2; exit 1; }
      WATCH_INTERVAL_SECONDS="$2"
      shift 2
      ;;
    --agent)
      (($# >= 2)) || { printf 'error: missing value for %s\n' "$1" >&2; exit 1; }
      AGENTS="$2"
      shift 2
      ;;
    --sidebar-layout)
      (($# >= 2)) || { printf 'error: missing value for %s\n' "$1" >&2; exit 1; }
      SIDEBAR_LAYOUT="$2"
      shift 2
      ;;
    --row-gap)
      (($# >= 2)) || { printf 'error: missing value for %s\n' "$1" >&2; exit 1; }
      ROW_GAP="$2"
      shift 2
      ;;
    -h|--help)
      sed -n '2,25p' "$0"
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
command -v cargo >/dev/null 2>&1 || die "Rust/Cargo is not installed or not on PATH"

case "$SIDEBAR_LAYOUT" in
  ""|packed|stacked) ;;
  *) die "sidebar-layout must be packed or stacked" ;;
esac
case "$ROW_GAP" in
  ""|0|1) ;;
  *) die "row-gap must be 0 or 1" ;;
esac

printf '%s\n' '→ building herdr-agent-quota'
cargo build --release --locked --manifest-path "$ROOT/Cargo.toml"

printf '%s\n' '→ linking and enabling the Herdr plugin'
herdr plugin link "$ROOT" --enabled

write_plugin_pref() {
  local name="$1" value="$2"
  [[ -z "$value" ]] && return 0
  local directory
  directory="$(herdr plugin config-dir herdr-agent-quota)" \
    || die "cannot resolve plugin config directory"
  mkdir -p "$directory"
  printf '%s\n' "$value" > "$directory/$name"
}

write_plugin_pref sidebar-layout "$SIDEBAR_LAYOUT"
write_plugin_pref row-gap "$ROW_GAP"

printf '%s\n' '→ installing reversible sidebar and provider collectors'
env ${WATCH_INTERVAL_SECONDS:+HERDR_AGENT_QUOTA_WATCH_INTERVAL_SECONDS="$WATCH_INTERVAL_SECONDS"} \
    ${AGENTS:+HERDR_AGENT_QUOTA_AGENTS="$AGENTS"} \
    ${SIDEBAR_LAYOUT:+HERDR_AGENT_QUOTA_SIDEBAR_LAYOUT="$SIDEBAR_LAYOUT"} \
    ${ROW_GAP:+HERDR_AGENT_QUOTA_ROW_GAP="$ROW_GAP"} \
  herdr plugin action invoke herdr-agent-quota.configure

printf '%s\n' 'Installed. Restart already-running agent sessions once so they load the refreshed hooks.'
