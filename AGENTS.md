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
| `event` | `pane.agent_detected`, `pane.agent_status_changed` | Only the pane named in `HERDR_PLUGIN_EVENT_JSON`, and never a Pi, omp, Muse, or Cursor pane — their transcripts carry the evidence |
| `focus` | `pane.focused`, `workspace.focused`, `tab.focused` | No |
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
OpenCode, Muse, and Cursor. Event-spawned watchers defer their first poll. They resolve local
billing targets, refresh active/settling targets, and publish to siblings with
the same target without reading terminal output. A finishing target stays in
the pass until the 60-second debounce has elapsed. The interval defaults to
60 seconds and is bounded to 30 seconds–1 hour. While a pane is working or
has an unseen completion, the watcher also checks the metadata-only Herdr
snapshot once per second. Herdr 0.9 can miss TUI focus hooks; the snapshot
reconciles those changes without reading pane output or writing unchanged
metadata. The watcher stays alive for unseen completions until they are seen.
Local stop/connection checks interrupt sleeps. Uninstall writes a stop marker.

## omp's quota does not come from a provider endpoint

Every other collector either reads a local credential and calls the provider
(`codex`, `grok`, `opencode_go`, `devin`, `muse`, `cursor`) or waits for a
statusLine hook (`claude`, `agy`). omp is the exception: it keeps its own credential store and
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
- Cursor stamps `sha256("cursor\0" || access token)`. Included is
  `planUsage.totalPercentUsed` when present — the CLI usage panel's Included
  row — and only then `includedSpend / limit`. The IDE `state.vscdb` mtime is
  not a credential gate.

## Devin's per-session model is local SQLite, not the quota API

`~/.local/share/devin/cli/sessions.db` is CLI session state. Open it
read-only and select only `id, model` — the same discipline as omp
`models.db`, not `agent.db`. A missing, locked, or unexpected schema skips
per-session attribution. `config.json` `agent.model` stays on
`snapshot.model` as the fallback and is never copied into `session_models`.

## Muse panes are matched through Muse's session lock

Herdr has no Muse session integration, so a Muse pane arrives without an
`agent_session`. `herdr::list_agent_state` fills it in from evidence Muse
writes itself: the `muse-bin` process inherits its pane's `HERDR_PANE_ID`, and
`sessions/<yyyy>/<mm>/<dd>/<id>/.session.lock` holds that process's
`pid=<n>`. Read only `comm` and the `HERDR_PANE_ID` entry of a process
environment, never anything else from it. A session Herdr does report always
wins. No `/proc` (macOS) means no session, never a guessed one.

The quota call (`muse-code/key`) also returns the account's API key and
identity. Only `subs_usage` is read. A `storage: "keychain"` login keeps the
OAuth token out of `auth.json`; the collector then reads that one item through
`security find-generic-password` (service `ai.meta.dev.credentials`, account
`meta`). Background processes never prompt: without a recorded approval marker
the keychain branch is skipped outright, and the user approves once via
`refresh --provider muse --keychain-approve` (click **Always Allow**, not
Allow). The marker lives beside the Muse config dir so every process — herdr
hook, daemon, or plain terminal — resolves the same path. A successful token
is kept in the watch process until the auth file's identity changes or
`muse-code/key` returns 401/403; a failed lookup is not cached and does not
clear the marker, so a transient failure retries on the next refresh. Only
`access_token` is taken from the payload; file-storage logins are unchanged.
No stored account
login (an API-key login) or an inactive subscription yields a snapshot without
windows, but only while a Muse session is refreshed, so its local fields still
publish. A rejected token or failed request stays an error, which keeps the
cached quota. Session-local fields come from the
bounded tail of `session.jsonl`: the last `model_completed` usage against the
`model-catalog` context limit, and the last prompt as topic: a main-surface
chat `runtime.user_intent.accepted`, with `user_prompt_display` accepted too
because Muse writes it only for some submits.
Muse publishes no prompt-cache lifetime, so there is no TTL estimate.

## `crate::herdr` is a wrapper over the real `herdr.rs`

`src/lib.rs` maps `pub mod herdr` to `src/herdr_wrapper.rs` via `#[path =
...]`, and separately maps a private `mod herdr_base` to `src/herdr.rs`. The
wrapper glob-imports everything from `herdr_base` and then shadows
`list_agent_state`/`list_agent_panes`/`find_agent_pane` with its own
definitions that post-process panes with plugin-local evidence Herdr's own
inventory does not carry — Agy's pane-id-as-session binding, and Claude's
self-reported session fallback below. Read `src/herdr.rs` for the inventory
parsing and publish machinery; read `src/herdr_wrapper.rs` for what gets
bolted onto a pane's session before anything else sees it.

## Claude panes can self-report their session when Herdr's integration is unwired

Herdr ships its own Claude SessionStart integration
(`~/.claude/hooks/herdr-agent-state.sh`, installed by `herdr integration
install claude`), but that hook only fires if `hooks.SessionStart` is wired
into `settings.json` — `herdr integration status` can show it "current" while
the wiring is still missing, and a stale/duplicate `herdr server` process (a
separate, unrelated failure mode) can also leave a pane's `agent_session`
unset. When that happens, `windows_for_session`/`context_for_session` see no
session id and correctly render nothing (`session_quota_only` fails closed by
design, since #60 — never borrow another session's numbers), which looks
identical to a stale cache from the outside but isn't one.

The plugin does not depend on that external hook: `run_statusline_hook` in
`src/configure/claude.rs` already reads this pane's exact session id from
stdin on every tick, and `HERDR_PANE_ID` is in its environment (the same var
Herdr's own hook reads). It self-reports `pane_id -> session_id` via
`CacheStore::save_pane_session`, and `herdr_wrapper::attach_claude_pane_session`
fills a pane's missing `agent_session` from that map — never overriding a
session Herdr *does* report. Diagnose a Claude pane stuck on `5h N/A` by
checking whether `herdr agent list` includes `agent_session` for it at all
before assuming a cache or lookup bug.

## Cursor's quota is DashboardService, not a browser cookie

Cursor Agent CLI is a separate install from the desktop app. Herdr's kind and
PATH command are `cursor` (alias `cursor-agent`). Never call a bare `agent` —
that name is Grok's on machines that have both. Herdr has a session
integration (`herdr integration install cursor`). Event does not read the
pane: the generated session title (`meta.json` `title`, else `store.db`
`name`) is the topic. Placeholder `New Agent` falls back to the last
`<user_query>` in the session jsonl.

Credentials, in order: `accessToken` in the CLI auth file (`$CURSOR_AUTH_FILE`,
else `~/.cursor/auth.json` on macOS, else `$XDG_CONFIG_HOME/cursor/auth.json`),
then `cursorAuth/accessToken` in the desktop `state.vscdb` (`$CURSOR_STATE_DB`
or the platform Cursor config path). Open that SQLite file read-only and
select only that one key. Never copy it, never use its mtime as a gate, never
read `refreshToken`, never open Keychain, never send a `WorkosCursorSessionToken`
cookie. The collector does not write, refresh, or exchange tokens; a 401
re-reads the current files once.

Quota is `POST https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage`
with `Connect-Protocol-Version: 1`, the same call the CLI makes. Included is
`planUsage.totalPercentUsed` when present — the CLI usage panel's "Included"
row — and only then `includedSpend / limit`. The three bars map onto at
(`autoPercentUsed`, 5h), api (`apiPercentUsed`, 7d), and 30d (Included).
`billingCycleEnd` is Unix milliseconds. Model is
`cli-config.json` `model.displayName`, overridden per session by store.db meta
`lastUsedModel` (never the encrypted blobs). Turn token counts are not in the
jsonl. Cache and context come from the interactive CLI's `afterAgentResponse`,
`stop`, and `preCompact` hooks: token counts map the same way the CLI
statusLine `current_usage` does (`fresh = input - cache_read - cache_write`);
Context percent is `store.db` `token_details.used_tokens / max_tokens`, the
same numbers the CLI footer prints (`Auto · 8.1%`). Only that protobuf field
is read. Cache still comes from the hooks; `context_usage_percent` wins when
present, otherwise last `input_tokens` against `context_window_size`,
Composer 2.x's documented 200k window, or Auto/`default`'s 256k window.
`configure` writes `herdr-agent-quota-hooks.sh` next to `hooks.json` and
merges `bash '<script>'` into `afterAgentResponse`, `stop`, and `preCompact`.
It never replaces Herdr's `sessionStart`. Cursor CLI loads user hooks at
session start, so an already-running pane must be restarted. Do not install a
Cursor `statusLine` — that setting replaces the native CLI footer. Cursor publishes no prompt-cache lifetime, so there is
no TTL. Cache identity is `sha256("cursor\0" || token)`.

## Herdr state this plugin owns outside a pane

Two things reach past the pane metadata, and both are global to the Herdr
session rather than scoped to a pane. Low-quota notifications stay off until
the user sets a threshold. The Agent view is on by default (`--agent-order
quota`): Space grouping plus least-headroom ranking inside each space.

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
4. **Quota order keeps Spaces contiguous.** The sort is
   `workspace_order` ascending, then `quota_headroom` ascending — never a
   flat headroom list that scatters one project's panes across the panel.
   `$quota_group` names the Space on the tightest pane in that workspace;
   `$quota_icon` / `_working` / `_done` is the vendor mark on every identity
   row (bundled icon font; Muse uses a text glyph). Colour replaces Herdr's `state_icon`
   ring: yellow while working, teal for an unseen completion, white after
   focusing that pane or moving focus away from it. Do not trust CLI `agent_status` for the teal
   step — same-tab siblings finish as server `idle` while the TUI ring is
   still teal. Persist working/unseen pane ids in plugin state
   (`icon-attention.json`) and never call `herdr pane current` from
   `event`: status hooks set `HERDR_PANE_ID` to the finisher. Focus hooks
   mark only the previous and newly focused panes seen. A workspace or Tab
   switch may not emit `pane.focused`; resolve its pane from that location's
   layout in `herdr api snapshot`. Ignore a delayed event whose workspace or
   Tab is no longer focused.

**`quota_headroom`** is the token that view sorts on: the remaining percent of
the tightest of the pane's 5h, 7d, and 30d windows, zero-padded to three digits
so Herdr's ordering of the text is its numeric ordering. Two properties are
load bearing:

- It is published **unconditionally**, not only when the order is enabled. No
  sidebar row renders it, so it costs no screen space; publishing it always is
  what makes toggling the order a Herdr-side change instead of a metadata write
  to every pane, and it adds no writes, because it only moves when a quota
  token beside it moves anyway.
- It is scoped to the windows the sidebar actually **shows**. A window without
  a token never decides the sort or an alert.

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
`herdr pane current`. Workspace and Tab focus events carry no pane ID, so
`focus` resolves the matching layout from `herdr api snapshot`. This keeps
delayed events from redirecting a refresh to another pane:

```json
{"event":"pane_focused","data":{"type":"pane_focused","pane_id":"w1:p9","workspace_id":"w1"}}
```

`find_agent` and `find_pane_id` in `src/refresh.rs` walk the tree rather than
assuming a fixed path. Keep them tolerant — the shapes differ per event and are
not part of a stable contract.

## Adding a harness

Append to `AgentSelection::SUPPORTED`. Never insert. A saved complete agent
list is a proper prefix of that array, and `parse_list` still reads an unmarked
prefix of length ≥ 6 as every agent. That is what #81 was: Muse grew
`SUPPORTED`, a settings-saved
`claude,codex,grok,agy,opencode,pi,omp,devin` became "partial", `ensure_omp`
took the hard-failure path, and `configure` aborted on a machine without omp.
The first six entries are the first complete list the settings pane wrote; do
not reorder them.

You do not add a historical snapshot by hand when you append — the prefix
rule covers the new tail. Settings and `install.sh --agent` write `all` or
`only,<names>` so a later "everything except the newest one" is not mistaken
for a legacy full list.

Wiring the new name is not enough. Also:

1. `Harness`, `from_agent_name`, `AgentSelection` (the enum, `parse`,
   `harness`, `harness_name`), clap `--agent` help, `install.sh` comments,
   both READMEs, and the plugin description.
2. A `PROVIDER_STYLES` row in `src/configure/herdr.rs`, in `SUPPORTED` order.
3. Settings popup `height` in `herdr-plugin.toml` — one more row. The
   `rows().len()` check fails if this is skipped.
4. If it has a subscription collector: `Provider`, `Provider::ALL`,
   `ProviderSelection`, the fetch path, and a cache identity. If Herdr has
   no integration for it, `integration_id` returns `None` (Agy, Muse).
5. If its own transcript is the evidence, `event` must not read the pane
   (Pi, omp, Muse, Cursor).
6. Tests that name agents must walk `SUPPORTED`, not a copied list. A copied
   list is how Muse missed the watcher-alive check and the "installs
   everything" sidebar assertions.

Adding a **sidebar field** is the same shape as #76: a saved "everything on"
list will not name the new field. `FieldSet::parse` has to keep reading that
exact legacy list as `all()`, and `as_list` needs a marker for the one new
selection that would collide with it.

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

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.
