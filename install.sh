#!/usr/bin/env bash
# Build, link, enable, and configure herdr-agent-usage in one step.
#
# Usage:
#   ./install.sh
#   ./install.sh --agent claude,codex
#   ./install.sh --watch-interval-seconds 300
#   ./install.sh --sidebar-layout packed
#   ./install.sh --row-gap 0
#   ./install.sh --quota-percent used
#   ./install.sh --sidebar-pacing on
#   ./install.sh --statusline-pace off
#   ./install.sh --fields topic,model,context,5h,7d
#   ./install.sh --agent-order default
#   ./install.sh --low-quota-alert 10
#
# --agent installs only the agents you name (all, claude, codex, grok, agy,
# opencode, pi, omp, devin, muse, cursor). Anything you leave out gets no sidebar row, no
# statusLine entry and no hook file. The default is every supported agent.
#
# --sidebar-layout gauges (default) draws a meter beside each quota number.
# packed joins cache/TTL and 5h/7d on one row. stacked puts provider, model,
# cache, TTL, context, 5h, and 7d on their own rows without meters.
#
# --row-gap 1 (default) leaves one blank row between agent panes; 0 packs them
# flush. Herdr only accepts whole rows.
#
# --quota-percent remaining (default) shows how much quota is left; used shows
# how much has been consumed. The colour always follows what is left.
#
# --sidebar-pacing off (default) keeps quota percentages and gauges. on shows
# 5h/7d windows as a signed pace delta plus time remaining, such as
# `5h -6% 45 min`. Negative means usage is ahead of the clock.
#
# --statusline-pace on (default) preserves the plugin's existing binding-window
# pace segment. off leaves Claude's own statusLine output unchanged; quota
# observations are collected either way.
#
# --fields picks the quota fields the sidebar shows: all, none, or a
# comma-separated list of provider, topic, model, cache, ttl, context, 5h, 7d,
# 30d. Default is provider, topic, model, context, 5h, 7d, 30d (cache and TTL
# off). The error token is always shown.
#
# --brand-colors is accepted for compatibility with older installs but has no
# effect; identity text follows the sidebar theme and icons show status colour.
#
# --agent-order quota (default) keeps each Space contiguous and ranks by
# least quota left inside the space; it replaces the panel's sort until it
# is set back to default. default leaves Herdr's own agent panel ordering
# alone (also Space-grouped unless the user set priority).
#
# --low-quota-alert off (default) never notifies. A percentage notifies once,
# per provider, when its remaining quota falls to that number or below, and
# again only after it has recovered above it.
#
# Everything here can also be changed later in the Agent quota settings pane.
#
# Every option is written to the plugin config directory before configure runs.
# Herdr executes a plugin action with a fixed command line in the server's own
# environment, so exported variables never reach it; the config directory is
# the only channel that does.
#
# The matching uninstall.sh first restores every configuration entry owned by
# this plugin and only then unlinks it from Herdr.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=scripts/herdr-action.sh
source "$ROOT/scripts/herdr-action.sh"
WATCH_INTERVAL_SECONDS=""
AGENTS=""
SIDEBAR_LAYOUT=""
ROW_GAP=""
QUOTA_PERCENT=""
SIDEBAR_PACING=""
STATUSLINE_PACE=""
FIELDS=""
AGENT_ORDER=""
LOW_QUOTA_ALERT=""

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
    --quota-percent)
      (($# >= 2)) || { printf 'error: missing value for %s\n' "$1" >&2; exit 1; }
      QUOTA_PERCENT="$2"
      shift 2
      ;;
    --sidebar-pacing)
      (($# >= 2)) || { printf 'error: missing value for %s\n' "$1" >&2; exit 1; }
      SIDEBAR_PACING="$2"
      shift 2
      ;;
    --statusline-pace)
      (($# >= 2)) || { printf 'error: missing value for %s\n' "$1" >&2; exit 1; }
      STATUSLINE_PACE="$2"
      shift 2
      ;;
    --fields)
      (($# >= 2)) || { printf 'error: missing value for %s\n' "$1" >&2; exit 1; }
      FIELDS="$2"
      shift 2
      ;;
    --brand-colors)
      (($# >= 2)) || { printf 'error: missing value for %s\n' "$1" >&2; exit 1; }
      case "$2" in
        on|off) ;;
        *) die "brand-colors must be on or off" ;;
      esac
      shift 2
      ;;
    --agent-order)
      (($# >= 2)) || { printf 'error: missing value for %s\n' "$1" >&2; exit 1; }
      AGENT_ORDER="$2"
      shift 2
      ;;
    --low-quota-alert)
      (($# >= 2)) || { printf 'error: missing value for %s\n' "$1" >&2; exit 1; }
      LOW_QUOTA_ALERT="$2"
      shift 2
      ;;
    -h|--help)
      sed -n '2,63p' "$0"
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
  ""|packed|stacked|gauges) ;;
  *) die "sidebar-layout must be packed, stacked, or gauges" ;;
esac
case "$ROW_GAP" in
  ""|0|1) ;;
  *) die "row-gap must be 0 or 1" ;;
esac
case "$QUOTA_PERCENT" in
  ""|remaining|used) ;;
  *) die "quota-percent must be remaining or used" ;;
esac
case "$SIDEBAR_PACING" in
  ""|off|on) ;;
  *) die "sidebar-pacing must be off or on" ;;
esac
case "$STATUSLINE_PACE" in
  ""|off|on) ;;
  *) die "statusline-pace must be off or on" ;;
esac
case "$AGENT_ORDER" in
  ""|default|quota) ;;
  *) die "agent-order must be default or quota" ;;
esac
# `0` is accepted as a spelling of off, the same as configure reads it.
case "$LOW_QUOTA_ALERT" in
  ""|off) ;;
  *[!0-9]*) die "low-quota-alert must be off or a percentage from 0 to 100" ;;
  *) ((LOW_QUOTA_ALERT <= 100)) \
    || die "low-quota-alert must be off or a percentage from 0 to 100" ;;
esac
# The field list is validated by configure, which owns the field names.

printf '%s\n' '→ building herdr-agent-usage'
cargo build --release --locked --manifest-path "$ROOT/Cargo.toml"

printf '%s\n' '→ linking and enabling the Herdr plugin'
collect_alias_plugin_dirs
herdr plugin link "$ROOT" --enabled
adopt_alias_plugin_dirs || die "cannot resolve plugin config directory"
unlink_alias_plugins

# Herdr runs a plugin action with a fixed command line, in the server's own
# environment: variables exported around `herdr plugin action invoke` never
# reach it. The plugin config directory is the only channel that does, so every
# option travels through a file there rather than through `env`.
write_plugin_pref() {
  local name="$1" value="$2"
  [[ -z "$value" ]] && return 0
  local directory
  directory="$(herdr plugin config-dir "$PLUGIN_ID")" \
    || die "cannot resolve plugin config directory"
  mkdir -p "$directory"
  printf '%s\n' "$value" > "$directory/$name"
}

write_plugin_pref agents "$(agents_pref_value "$AGENTS")"
write_plugin_pref watch-interval-seconds "$WATCH_INTERVAL_SECONDS"
write_plugin_pref sidebar-layout "$SIDEBAR_LAYOUT"
write_plugin_pref row-gap "$ROW_GAP"
write_plugin_pref quota-percent "$QUOTA_PERCENT"
write_plugin_pref sidebar-pacing "$SIDEBAR_PACING"
write_plugin_pref statusline-pace "$STATUSLINE_PACE"
write_plugin_pref fields "$FIELDS"
write_plugin_pref agent-order "$AGENT_ORDER"
write_plugin_pref low-quota-alert "$LOW_QUOTA_ALERT"

printf '%s\n' '→ installing reversible sidebar and provider collectors'
invoke_action_and_wait configure || die "configuration action failed"

printf '%s\n' 'Installed / updated. Existing preferences are retained and quota refresh is restored. Restart sessions only to load newly added hooks or integrations.'
