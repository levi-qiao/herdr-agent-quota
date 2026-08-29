#!/usr/bin/env bash
# Build, link, enable, and configure herdr-agent-quota in one step.
#
# Usage:
#   ./install.sh
#   ./install.sh --agent claude,codex
#   ./install.sh --watch-interval-seconds 300
#
# --agent installs only the agents you name (all, claude, codex, grok, agy,
# opencode). Anything you leave out gets no sidebar row, no statusLine entry
# and no hook file. The default is every supported agent.
#
# The matching uninstall.sh first restores every configuration entry owned by
# this plugin and only then unlinks it from Herdr.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
WATCH_INTERVAL_SECONDS=""
AGENTS=""

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
    -h|--help)
      sed -n '2,13p' "$0"
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

printf '%s\n' '→ building herdr-agent-quota'
cargo build --release --locked --manifest-path "$ROOT/Cargo.toml"

printf '%s\n' '→ linking and enabling the Herdr plugin'
herdr plugin link "$ROOT" --enabled

printf '%s\n' '→ installing reversible sidebar and provider collectors'
env ${WATCH_INTERVAL_SECONDS:+HERDR_AGENT_QUOTA_WATCH_INTERVAL_SECONDS="$WATCH_INTERVAL_SECONDS"} \
    ${AGENTS:+HERDR_AGENT_QUOTA_AGENTS="$AGENTS"} \
  herdr plugin action invoke herdr-agent-quota.configure

printf '%s\n' 'Installed. Restart already-running agent sessions once so they load the refreshed hooks.'
