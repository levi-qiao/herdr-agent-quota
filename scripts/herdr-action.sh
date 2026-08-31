# Invoke a Herdr plugin action and wait for it to finish.
#
# `herdr plugin action invoke` starts the action and returns immediately with
# `"status":"running"`. Both installer scripts need the action to have actually
# completed before they continue: uninstall.sh unlinks the plugin next, and
# unlinking while the restore action is still running can leave a statusLine
# entry pointing at a plugin that is no longer there.
#
# Sourced by install.sh and uninstall.sh; not a standalone script.

HERDR_ACTION_PLUGIN_ID="herdr-agent-quota"
# Configuration writes touch a handful of small files. A minute is far beyond
# any legitimate run and still bounds a hung action.
HERDR_ACTION_TIMEOUT_SECONDS="${HERDR_ACTION_TIMEOUT_SECONDS:-60}"

# Extract the first "<key>":"<value>" string field from a JSON blob.
herdr_action_json_field() {
  sed -n "s/.*\"$2\":\"\([^\"]*\)\".*/\1/p" <<<"$1" | head -1
}

# Status of one log entry, or empty when Herdr no longer lists it.
herdr_action_status() {
  herdr plugin log list --plugin "$HERDR_ACTION_PLUGIN_ID" --limit 50 2>/dev/null \
    | tr '{' '\n' \
    | grep -F "\"log_id\":\"$1\"" \
    | sed -n 's/.*"status":"\([a-z_]*\)".*/\1/p' \
    | head -1
}

# invoke_action_and_wait <action-id>
#
# Returns non-zero when the action reports a failure. An action whose log entry
# cannot be found is treated as finished rather than hung: older Herdr builds
# may not list it, and blocking the installer on a missing log helps nobody.
invoke_action_and_wait() {
  # `status` is a read-only special parameter in zsh, so this stays `state`
  # even though the scripts themselves run under bash.
  local action="$1" output log_id state waited=0

  output="$(herdr plugin action invoke "$HERDR_ACTION_PLUGIN_ID.$action")" || return 1
  log_id="$(herdr_action_json_field "$output" log_id)"
  if [[ -z "$log_id" ]]; then
    return 0
  fi

  while ((waited < HERDR_ACTION_TIMEOUT_SECONDS)); do
    state="$(herdr_action_status "$log_id")"
    case "$state" in
      running|"") ;;
      succeeded) return 0 ;;
      *)
        printf 'error: plugin action %s %s\n' "$action" "$state" >&2
        printf 'inspect it with: herdr plugin log list --plugin %s\n' \
          "$HERDR_ACTION_PLUGIN_ID" >&2
        return 1
        ;;
    esac
    # An entry that never appears is not worth waiting a minute for.
    if [[ -z "$state" ]] && ((waited >= 3)); then
      return 0
    fi
    sleep 1
    waited=$((waited + 1))
  done

  printf 'error: plugin action %s did not finish within %ss\n' \
    "$action" "$HERDR_ACTION_TIMEOUT_SECONDS" >&2
  return 1
}
