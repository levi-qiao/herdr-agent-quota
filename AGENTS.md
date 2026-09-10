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

Read pane output only with `--source visible` or `detection`; `recent` and
`recent-unwrapped` rebuild scrollback and visibly repaint the agent TUI.
Metadata writes also carry repaint risk, so avoid no-op writes. Extract the topic
from the event pane while visible; if extraction fails, preserve its existing topic.

For scrolling reports or changes to pane-read behavior, read
[pane repaint diagnosis](docs/pane-repaint-diagnosis.md) before probing.
Scroll offsets and before/after content hashes cannot detect the transient repaint;
use the documented human observation rather than polling live panes.

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
| `startup` | Herdr's `[[startup]]` hook | No |
| `refresh` | manual action, `startup` | No |
| `event` | `pane.agent_detected`, `pane.agent_status_changed` | Only the pane named in `HERDR_PLUGIN_EVENT_JSON`, and never a Pi or omp pane — their transcripts carry the evidence |
| `focus` | `pane.focused` | No |
| `watch` | detached from a working status event | No (agent metadata only) |

`startup` exists because Herdr drops plugin-owned Agent views when the server
exits, and startup hooks run again after a restart or a live handoff. It
restores plugin-owned views, forces one quota refresh, and restores the watcher.
Plugin enable alone does not run startup; the configure action runs it after
repair. Server-owned event/refresh paths also record the current Herdr binary
and socket so an older watcher can adopt the new connection.

`pane.agent_status_changed` fires **twice per turn** (idle→working on submit,
working→idle on completion). Anything `event` does, the user pays for twice
every time they press Enter. Budget accordingly.

The working event starts one global `watch` pulse. It calls `herdr agent list`
once per configured interval for every supported harness, including Pi, OMP,
and OpenCode. Event-spawned watchers defer their first poll. They resolve local
billing targets, refresh active/settling targets, and publish to siblings with
the same target without reading terminal output. A finishing target stays in
the pass until the 60-second debounce has elapsed. The interval defaults to
60 seconds and is bounded to 30 seconds–1 hour. Local stop/connection checks
interrupt sleeps without polling Herdr. Uninstall writes a stop marker.

## omp's quota does not come from a provider endpoint

Every other collector either reads a local credential and calls the provider
(`codex`, `grok`, `opencode_go`, `devin`) or waits for a statusLine hook
(`claude`, `agy`). omp is the exception: it keeps its own credential store and
ships its own usage layer, so `src/providers/omp.rs` shells out to
`omp usage --json --provider <id>` and reads the answer.

Three properties hold that together, and each one is load bearing:

1. **One provider, never the pool.** The call always names the provider the
   pane's transcript is talking to. Asking for everything would poll every
   subscription the user has in omp, on a pane event.
2. **Two caches, deliberately.** omp answers from its own five-minute usage
   cache in `agent.db`; on top of that this plugin debounces to 60 seconds per
   target and stores the sanitized report for all accounts returned by that one provider. Neither layer may be removed on
   the theory that the other covers it — omp's cache is what stops a provider
   request, ours is what stops a process spawn.
3. **`agent.db` is never opened.** It holds live OAuth tokens. Everything
   needed — the account identity and the quota — is in the CLI's output.
   `models.db` is opened read-only, because the context window is the one thing
   the CLI cannot give cheaply.

An omp pane is billed in `CredentialScope::OMP_STORE`, not the canonical scope.
An omp Claude pane and a Claude Code pane can be two different subscriptions,
so they must never share a cache file; `BillingTarget::cache_identity` is what
keeps them apart, and it is the reason that function appends a scope.

Attribution is by omp's `credential_pin`: the transcript records
`sha256(provider\0accountId\0email\0orgId\0projectId)` of the serving
account, and `providers::omp::account_pin` recomputes it from the usage
report's identity. That digest is omp's persisted contract — if it changes
upstream, every pin is orphaned and multi-account panes silently fall back to
"no quota". The pinned-digest test exists to make that a test failure rather
than a wrong number.

## Quota attribution and cache upgrades

- Direct API snapshots carry an account ID or credential hash. Unstamped old
  caches cannot prove a current login. A failed attempt is debounced by the
  attempted identity; a different login can refresh immediately.
- Codex rollouts provide diagnostics only. Fresh API windows replace old
  windows, including ones an older plugin borrowed from a rollout.
- Claude/Agy StatusLine has no reliable serving-account ID. New observations
  carry `session_quota_only`; they never share windows by profile directory.
  Rebuild old mailboxes from their raw payload, not merged profile windows.
- Agy must identify the active pool or receive only one possible pool. Do not
  combine Gemini and third-party quotas for an unknown model.
- OMP stores all accounts in one sanitized provider report so a second pin
  does not lose its quota during debounce. Select by pin; keep a failed
  account's old reading only while the report still identifies that account.

## Devin's per-session model is local SQLite, not the quota API

`~/.local/share/devin/cli/sessions.db` is CLI session state. Open it
read-only and select only `id, model` — the same discipline as omp
`models.db`, not `agent.db`. A missing, locked, or unexpected schema skips
per-session attribution. `config.json` `agent.model` stays on
`snapshot.model` as the fallback and is never copied into `session_models`.

## Herdr state this plugin owns outside a pane

Two things reach past the pane metadata, and both are global to the Herdr
session rather than scoped to a pane. Neither is on by default.

**The Agent view** (`agent.view.set`, `src/herdr.rs`). Herdr keeps exactly
one, and setting it replaces the user's own `ui.agent_panel_sort`. Rules:

1. **Always scope a clear to `plugin:herdr-agent-quota`.** An unscoped
   `agent.view.clear` would drop a view another plugin owns. `startup` goes
   further and does not call clear at all when the order is `default` — there
   is nothing of ours to restore, and silence is the only way to be sure a
   foreign view survives.
2. **Re-apply it from `startup`, never from `refresh`.** `refresh` runs on
   every event path; the view only needs putting back when the server restarted.
3. It is the only thing in the plugin that speaks the raw socket protocol
   (`HERDR_SOCKET_PATH`), because `agent.view.*` has no CLI subcommand in
   Herdr 0.8. One request, one reply, one connection — nothing subscribes, so
   the `events.subscribe` replay and focus-storm problems do not apply.

**`quota_headroom`** is the token that view sorts on: the remaining percent of
the tighter of the pane's 5h and 7d windows, zero-padded to three digits so
Herdr's ordering of the text is its numeric ordering. Two properties are load
bearing:

- It is published **unconditionally**, not only when the order is enabled. No
  sidebar row renders it, so it costs no screen space; publishing it always is
  what makes toggling the order a Herdr-side change instead of a metadata write
  to every pane, and it adds no writes, because it only moves when a quota
  token beside it moves anyway.
- It is scoped to the two windows the sidebar actually **shows**. A monthly
  window has no sidebar token, so letting it decide the sort or an alert would
  produce an ordering the user cannot explain from the screen.

**Low quota notifications** fire from both publish paths (`publish_resolved`
and `handle_named_pane`) so a warning lands at the end of the turn that spent
the quota. The state is a set of provider names, not a timestamp: a provider
stays quiet while it stays low and is re-armed only by recovering above the
threshold. A provider with **no pane in the pass keeps its entry** — dropping
it would make closing and reopening a pane a way to be warned twice.

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
carries no `agent`: `focus` uses its pane ID and resolves the harness from one
agent inventory read. Only a direct `focus` invocation without event JSON uses
`herdr pane current`. This keeps delayed events and Herdr 0.9's independent
clients from redirecting a refresh to another pane:

```json
{"event":"pane_focused","data":{"type":"pane_focused","pane_id":"w1:p9","workspace_id":"w1"}}
```

`find_agent` and `find_pane_id` in `src/refresh.rs` walk the tree rather than
assuming a fixed path. Keep them tolerant — the shapes differ per event and are
not part of a stable contract.

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
