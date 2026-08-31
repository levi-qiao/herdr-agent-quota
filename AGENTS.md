# Agent guide

Notes for agents working on `herdr-agent-quota`. Read this before touching
anything that talks to Herdr.

## Working method

1. Establish the exact requested scope and inspect the current diff before
   editing. Treat unrelated worktree changes as user-owned.
2. Separate observed facts, inferences, and unknowns. When evidence is missing,
   name the cheapest useful verification instead of guessing.
3. Prefer the smallest surgical change that creates a checkable behavior. Add
   abstractions only when two real callers or adapters need the same seam.
4. Give every implementation step a verification condition and run the
   repository gates before calling it complete.
5. For multi-goal work, keep the decision record and dispatch prompts under
   the ignored `.agents/` directory. Public documentation must describe shipped
   behavior, not private execution state.

Before changing dependencies, inspect `Cargo.toml`, `Cargo.lock`, and
`rust-toolchain.toml`. Use the pinned Rust toolchain and repository-local Cargo
artifacts; do not install project tooling globally.

## The rule that matters most: reading or writing a pane is not free

This plugin's whole job is to put quota numbers in Herdr's sidebar. Every
Herdr call it makes to do that lands on a pane that a human is actively
watching. Two calls are visible to the user:

- `herdr pane read <id> --source recent` (and `recent-unwrapped`) — rebuilds the
  pane's wrapped scrollback. Measured at **4.45s per call** against 0.006s for
  `--source visible`, and it repaints the pane: the agent's TUI redraws its whole
  frame, which the user sees as the terminal scrolling up and snapping back to
  the bottom. **One read, one scroll** — confirmed 1:1 by burst-reading a live
  pane while the user watched (2 `recent` reads → 2 scrolls; 13 `visible` reads
  → none).
- `herdr pane report-metadata <id>` — repaint risk when the tokens actually
  change, though it was never reproduced as a scroll on its own.

Use `--source visible` (or `detection`; both return the current screen). The
prompt is on screen at the moment `idle->working` fires, which is exactly when
the topic changes. Later in the turn it may have scrolled off — then extraction
returns `None` and the caller must keep the topic it already published.

| `--source` | cost | repaints |
|---|---|---|
| `visible` | 0.006s | no |
| `detection` | 0.004s | no |
| `recent` | 4.452s | **yes** |
| `recent-unwrapped` | 4.448s | **yes** |

None of this is detectable from `pane get`: `offset_from_bottom` and
`max_offset_from_bottom` stay `0` throughout, because full-screen agent TUIs
have no Herdr scrollback. The viewport never moves. What moves is the agent's
own repaint. **Do not conclude "no scroll happened" from the scroll offsets.**

Comparing a pane's content hash before and after a call does not work either:
the repaint ends with the pane back exactly as it was, so the hashes match and
the scroll is invisible to sampling. This produced a false "pane read is
harmless" result during diagnosis. **A human has to watch the pane.** The only
reliable instrument is a burst of N calls with the user counting scrolls.

Concretely, this means:

1. **Never read every pane of a provider.** An event names one pane; read only
   that one. Fanning out across panes multiplies the repaints by the number of
   panes the user has open for that agent.
2. **Publish once per invocation.** Two `publish` passes in a row means each
   pane can take two metadata writes for one user action.
3. **Keep `metadata_matches` honest** (`src/herdr.rs`). It is the only thing
   stopping a no-op refresh from repainting every pane. If you add a token,
   add it to `METADATA_TOKEN_NAMES` too, or the comparison silently stops
   covering it and every refresh becomes a write.
4. **Preserve, don't clear.** When a topic read fails or finds nothing, keep
   the previously published topic. Clearing it churns the token and triggers
   a write on the next refresh, which triggers a repaint.

## Event paths, and what each is allowed to do

| Entry point | Fired by | Allowed to read panes? |
|---|---|---|
| `refresh` | startup, manual action | No |
| `event` | `pane.agent_detected`, `pane.agent_status_changed` | Only the pane named in `HERDR_PLUGIN_EVENT_JSON` |
| `focus` | `pane.focused` | No |
| `watch` | detached from a working status event | No (agent metadata only) |

`pane.agent_status_changed` fires **twice per turn** (idle→working on submit,
working→idle on completion). Anything `event` does, the user pays for twice
every time they press Enter. Budget accordingly.

The working event starts one global `watch` pulse. It calls `herdr agent list`
once per configured interval, refreshes every working provider in that pass,
publishes without reading pane output, and exits after all agents settle. The
interval defaults to 60 seconds and is bounded to 30 seconds–1 hour. Uninstall
writes a stop marker so a detached watcher cannot survive a restore.

## A plugin action cannot see the caller's environment

Herdr runs `[[actions]]` with a fixed command line **in the server's own
environment**. A variable exported around `herdr plugin action invoke` does not
reach the action. Measured with a temporary `printenv` action: of 61 variables,
the only Herdr-related ones present were `HERDR_PLUGIN_STATE_DIR` and
`HERDR_PLUGIN_CONFIG_DIR`, both injected by Herdr; neither the probe marker nor
`HERDR_AGENT_QUOTA_AGENTS` survived.

So `src/prefs.rs` — small files under `HERDR_PLUGIN_CONFIG_DIR` — is the only
channel an installer has for passing a choice to `configure`. Environment
variables still work for a **direct CLI run** and are read first, but anything
that must survive `install.sh` / `uninstall.sh` has to be written as a
preference. This bit once: `./uninstall.sh --agent grok` passed the selection
through `env`, it never arrived, and the default selection is *every* agent, so
a partial uninstall removed everything.

To re-check this on a new Herdr version, append a throwaway action running
`printenv > /tmp/probe.txt`, reload with `herdr plugin disable && herdr plugin
enable`, invoke it with a marker variable set, and read the file.

## Event payload shapes

`HERDR_PLUGIN_EVENT_JSON` is nested and not uniform across events. `pane.focused`
carries no `agent` at all, which is why `focus` has to call `herdr pane current`:

```json
{"event":"pane_focused","data":{"type":"pane_focused","pane_id":"w1:p9","workspace_id":"w1"}}
```

`find_agent` and `find_pane_id` in `src/refresh.rs` walk the tree rather than
assuming a fixed path. Keep them tolerant — the shapes differ per event and are
not part of a stable contract.

## Debugging a "the panes are scrolling" report

Bisect from the outside in. Each step needs the user to reproduce once, so do
them in this order and don't skip ahead:

1. `herdr plugin disable herdr-agent-quota` **and** remove the `statusLine`
   entry from `~/.claude/settings.json`, then **restart the agent pane**.
   `herdr plugin disable` alone is not enough — Claude Code runs the statusLine
   command itself, independent of Herdr, and reads the setting at startup.
2. Restore the statusLine only. Scrolls → the statusLine hook is at fault.
3. Re-enable the plugin, then remove event hooks from `herdr-plugin.toml` one
   at a time, reloading with `herdr plugin disable && herdr plugin enable`
   (needed to re-read the manifest; `herdr server reload-config` does not).

To capture a real event payload, temporarily point a hook at
`sh -c "printf '%s' \"$HERDR_PLUGIN_EVENT_JSON\" > /tmp/ev.json; exec <real command>"`
so the plugin keeps working while you collect the shape.

Beware of instrumenting with a polling probe: polling `herdr pane read` at
several Hz is itself a repaint source and will contaminate whatever the user
reports while it runs.

## Verifying

```
cargo fmt
cargo test
cargo clippy --release
```

Reloading the plugin after a rebuild:

```
herdr plugin disable herdr-agent-quota && herdr plugin enable herdr-agent-quota
```
