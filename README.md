# herdr-agent-quota

**Never hit a quota limit mid-task.** Live Claude Code, Codex, Grok, and
Agy/Antigravity subscription usage, in Herdr's agent sidebar.

[![CI](https://github.com/levi-qiao/herdr-agent-quota/actions/workflows/ci.yml/badge.svg)](https://github.com/levi-qiao/herdr-agent-quota/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/built%20with-Rust-dea584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Herdr plugin](https://img.shields.io/badge/Herdr-plugin-supported-5b6ee1)](https://herdr.dev/plugins/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![GitHub stars](https://img.shields.io/github/stars/levi-qiao/herdr-agent-quota?style=social)](https://github.com/levi-qiao/herdr-agent-quota)

中文文档：[README.zh-CN.md](README.zh-CN.md)

```text
● Owner · Claude/Sonnet
  hi                     ← what that pane is actually working on
  cache 99.6% · ttl≈58m    ← session hit rate · remaining cache TTL
  context 23%             ← provider-native context percentage
  5h 100% 3h07m · 7d 31% 2d3h
```

![Live Herdr agent sidebar](docs/screenshots/herdr-sidebar-live.png)

*A current Herdr workspace: Claude and Codex show five-hour and weekly reset
ETAs on one compact row, Grok keeps its `7d` window clean when Herdr has no
session id, and Agy shows model/context/cache data from its matching statusLine
session. Each card uses the latest user prompt rather than an AI-generated status.*

- **Four providers, one sidebar** — Claude Code, OpenAI Codex, Grok, and
  Agy/Antigravity.
- **Compact, capability-aware cards** — provider, prompt/session summary,
  supported context usage, and quota windows. Unsupported rows are hidden.
- **Local only** — no usage data uploaded, no browser cookies, no keychain
  scraping, and credentials are never written or refreshed.
- **Never lies to you** — a failed refresh keeps the last good number instead
  of flashing `unavailable`, and API-key auth is never shown as a subscription
  quota.
- **Fully reversible** — one action sets it up, one action puts your config
  back exactly as it was.

For a downloaded checkout, one command applies every reversible integration
([quick start](#quick-start)):

```sh
./install.sh
```

The screenshot is a real local Herdr session. The values and topic text are
examples from that session; they are not hard-coded in the plugin.

### Time-aware quota health

Quota colors answer “will this allowance last until reset?” instead of applying
a fixed percentage threshold. For each available 5-hour or 7-day window, the
plugin computes:

```text
time_left  = (reset_at - now) / window_duration
quota_left = remaining_percent / 100
health     = quota_left / time_left
```

- **Green** — `health >= 1`: quota is being consumed no faster than time.
- **Amber** — `health < 1`: current usage is ahead of the sustainable pace.
- **Red** — `health < 1` and less than 20% quota remains: exhaustion risk is
  both immediate and material.
- **Amber fallback** — reset data is missing or expired, so the plugin avoids
  claiming that the quota is safe.

This explains the screenshot: Claude's 5-hour 89% is amber because slightly
more than 89% of that window remains; its weekly 24% is green because only about
13% of the week remains. Grok's weekly 17% is red because about 69% of its
window remains and the quota is already below 20%. The calculation is shared by
every provider adapter; only the window data differs.

## Quick start

Requirements: Herdr, a Rust toolchain, macOS or Linux, and at least one
supported provider CLI. If you downloaded this repository, the one-step installer does
the build, link, enable, and reversible configuration for you:

```sh
./install.sh
```

To restore the previous sidebar/statusLine configuration and unlink the plugin:

```sh
./uninstall.sh
```

The scripts are idempotent. They leave the local quota snapshots in Herdr's
plugin state directory; those contain no credentials and can be removed later
if desired.

The equivalent Herdr commands are:

```sh
herdr plugin link . --enabled
herdr plugin action invoke herdr-agent-quota.configure
```

The configure action consistently uses Herdr's plugin state, applies the
sidebar rows, installs or repairs the reversible Claude and Agy statusLine
collectors, and reloads Herdr's config. Claude's native statusLine refresh is
also aligned with the same watcher interval (60 seconds by default), so an
idle session receives fresh reset timestamps without a provider login or model
request. You can
run it again safely from Herdr's action menu as **Install / repair agent quota**.
Use **Refresh agent quota** for a one-shot refresh, or
press `prefix+shift+r` after configuration. The shortcut force-fetches Codex
and Grok, then republishes the latest Claude and Agy statusLine snapshots. Run
the same action from a shell with:

```sh
herdr plugin action invoke herdr-agent-quota.refresh
```

### Installing only the agents you use

By default `configure` installs every supported agent. If you only use some of
them, name them and nothing else is written:

```sh
herdr-agent-quota configure --apply --agent claude,codex
```

Accepted values are `all` (the default), `claude`, `codex`, `grok`, `agy` and
`opencode`; repeat the flag or comma-separate the values. An agent you do not
select gets no sidebar row, no statusLine entry and no hook file — nothing of
that agent's is created or started on your machine.

Removal works the same way, and removing one agent leaves the others working:

```sh
herdr-agent-quota configure --uninstall --agent grok   # just Grok
herdr-agent-quota configure --uninstall                # everything
```

Only a full `--uninstall` touches shared state: the background watcher, the
saved poll interval and the config backup that makes the sidebar changes
reversible. A `--agent` uninstall removes just that agent's own rows and files,
so it is safe to run while other agents stay installed, and it can be repeated
without effect. Both forms only ever remove entries this plugin wrote; a row or
hook you wrote yourself is left alone.

`--agent` also narrows `--check`, which reports what would change without
writing anything.

### OpenCode Go is best-effort and looking for a maintainer

**The maintainer of this repository does not have an OpenCode Go subscription.**
Every other provider here was built and checked against a live account; OpenCode
Go could not be, and it is the only part of this plugin in that position.

What *is* verified first hand:

- OpenCode's local storage: the read-only `opencode.db` session lookup, the
  `auth.json` credential shapes, and the `opencode-go` vs `opencode` (Zen)
  distinction, all checked against a real opencode 1.18.20 install.
- That `https://opencode.ai/zen/go/v1/usage` exists and rejects a bad token with
  `401` rather than `404`.

What is taken from a second source rather than observed:

- The success response shape and the meaning of its `percent` field. These come
  from [CodexBar](https://github.com/steipete/CodexBar)'s implementation and its
  own test fixtures, cited line by line in
  [`docs/research/opencode-go-usage.md`](docs/research/opencode-go-usage.md).

Because of that, the collector is written to fail closed: a missing, malformed
or unexpected field produces no window instead of a guessed number, an absent
optional window is omitted rather than reported as `0%`, and `401`/`403` never
becomes a zero-percent reading. A pane keeps its last good value rather than
being cleared when a fetch fails.

**None of this can affect the other providers.** OpenCode Go is deliberately
absent from `Provider::ALL`, so `refresh --provider all`, the active-turn
watcher and the original four's cache files behave exactly as before. It is only
ever fetched for a pane that resolved to it, through its own credential-scoped
cache and refresh lease. If you do not use OpenCode, nothing here runs; if you
do not have a Go key, no request is ever made. Both are covered by tests.

If you have a Go subscription and the numbers look wrong — or right — please
open an issue or a PR. A sanitized real response would let the fixtures be
replaced with observed data, and help with this provider is very welcome.

### Herdr's agent integration is a prerequisite

Quota is attributed to a pane through the session id Herdr reports for it, and
Herdr only knows that id once **its own** integration for that agent is
installed. That integration ships inside the `herdr` binary and lives in the
agent's config directory; this plugin never installs or modifies it.

Check yours with:

```sh
herdr integration status
```

An agent listed as `not installed` is detected by Herdr but reports no session,
so this plugin cannot attribute it and its pane simply stays blank. Install the
one you need:

```sh
herdr integration install opencode      # then restart that agent's pane
```

`configure --check` and `configure --apply` print this hint for any selected
agent whose integration is missing, so a fresh install does not fail silently.

The Herdr plugin API does not currently let plugins add buttons to the native agent
group header, so the shortcut is the closest stable one-step entry point.
Selecting a pane also runs a provider-only refresh, debounced to once per
minute. While any agent is working, one global background watcher polls the
working provider set once per minute, publishes fresh cache values, and performs
one final debounced pass when each provider settles. These pulses never read
terminal content, and a pane currently viewing scrollback receives no metadata
write until it returns to the bottom.

The default poll interval is 60 seconds. To persist another value (30 seconds to
one hour), pass it during installation:

```sh
./install.sh --watch-interval-seconds 300
```

Or update an existing installation with the same environment override:

```sh
HERDR_AGENT_QUOTA_WATCH_INTERVAL_SECONDS=300 \
  herdr plugin action invoke herdr-agent-quota.configure
```

Each poll uses one non-blocking watcher lease and one `herdr agent list` call.
Provider refreshes use independent non-blocking leases, so a slow provider
cannot stall another provider or a statusLine collector. Network fetches are
independently capped at one per 60 seconds by the existing refresh markers, even
if a shorter custom poll interval is requested.
The watcher never sends prompts, starts a new login, refreshes credentials, or
consumes model/chat tokens; a manual `--force` refresh is the explicit exception.

Preview the changes without writing anything:

```sh
./target/release/herdr-agent-quota configure --check
```

Remove every plugin-owned config edit and restore previous Claude/Agy
statusLine commands with the one-click **Uninstall agent quota configuration**
action, or invoke it from a shell (the `./uninstall.sh` wrapper above also
unlinks the plugin):

```sh
herdr plugin action invoke herdr-agent-quota.uninstall
```

After that action finishes, `herdr plugin unlink herdr-agent-quota` can remove
the local plugin registration too. Configuration writes intentionally run
through plugin actions so all collectors use the same Herdr state directory.

The setup preserves Herdr's native state dot and plane/tab label. It only adds
the provider, usage, and topic tokens, so the original Herdr agent indicator is
not removed. Uninstall removes the plugin-owned rows and restores previous
Claude and Agy statusLine commands. Older plugin-owned Grok response hooks are
removed during configure; the single global watcher now covers Grok as well, so
long turns no longer start one refresh command per tool call.

## Supported providers

| Provider | Sidebar windows | Local collection path | Extra setup |
| --- | --- | --- | --- |
| Claude Code | model + `5h` + `7d` + context + cache hit/approx. TTL | Official `statusLine` JSON: `model`, `rate_limits`, `context_window`, and `transcript_path` | The configure action installs/chains it and keeps its refresh interval current |
| OpenAI Codex | model + `5h` + `7d` + context + cache + approx. TTL + local session summary | One-shot local `codex app-server --stdio` plus a bounded tail read of matching `~/.codex` rollout JSONL | ChatGPT subscription login; API-key mode is shown as unavailable |
| Grok CLI / Grok Build | model + `7d` + context + cache | Local `~/.grok/auth.json` billing plus bounded reads of `signals.json`/`updates.jsonl` session metadata | Covered by the unified watcher; no response hook is installed |
| Agy / Antigravity CLI | model + `5h` + `7d` + context + cache hit | Official `statusLine` JSON: `model`, `quota`, and `context_window` | The configure action installs and chains it automatically |

The sidebar shows **percentage remaining** and the time until each quota reset,
not quota token counts. The two Claude windows use compact `5h` and `7d`
labels on one row; each still keeps its own dynamic health color. Claude and
Agy also show the provider-reported model display name and context percentage. When a statusLine transcript
and session id are available,
`cache N.N%` is the cumulative main-session ratio
(`read / (fresh + creation + read)`), not the latest turn. The same row shows,
for Claude, a `ttl≈...` estimate from the provider's 5-minute/1-hour bucket.
This is local diagnostic math, not a server-confirmed expiry; the first session
update reads the existing transcript once, then later updates read only
appended bytes.
Codex supplements its five-hour/weekly windows and short session preview with
the latest `last_token_usage` context from the matching rollout tail and a
cumulative cache ratio from its local token counters. When a cache-bearing
rollout event has a timestamp, it also shows an explicitly approximate
one-hour upper-bound TTL; Codex does not persist an exact expiry timestamp.
Grok supplements its weekly billing window with model/context signals and the
latest cache counters from the matching local session files. During a working
turn, one short-lived global watcher polls once per configured interval,
coalesces active fetches, and exits when all selected providers settle. The
sidebar does not run a permanent daemon.

Grok has only a weekly quota window (`7d`), so its provider-specific row places that
limit beside context instead of leaving an empty slot on the right.

A failed refresh never replaces a successful cached value with `unavailable`;
a provider without any successful snapshot is shown as `N/A` until its first
usable event.

## Agy / Antigravity collection

Agy sends its quota snapshot to the plugin through its native one-shot
`statusLine` hook. The configure action installs it automatically, backs up and
chains an existing command, and restores that command on uninstall. The plugin
collector itself emits no status-line text: it reads JSON from stdin, writes
only sanitized percentages to the local plugin cache, and exits. It is not a
resident process and does not use browser cookies or a private API.

## What the sidebar rows mean

The default rows are deliberately compact and keep the provider name only
once:

```toml
[ui.sidebar.agents]
row_gap = 1 # herdr-agent-quota
rows = [
  ["state_icon", "tab", { token = "$quota_provider_model", bold = true, dim = false }],
  [{ token = "$quota_topic", dim = false }],
  [
    { token = "$quota_cache", fg = "#9aa7b8", bold = true, dim = false },
    { token = "$quota_cache_ttl", fg = "#9aa7b8", bold = true, dim = false },
    { token = "$quota_error", fg = "#ca6470", bold = true, dim = false },
  ],
  [
    { token = "$quota_context", fg = "#9aa7b8", bold = true, dim = false },
    { token = "$quota_week_inline_normal", fg = "#84b084", bold = true, dim = false },
    { token = "$quota_week_inline_warning", fg = "#cdaa65", bold = true, dim = false },
    { token = "$quota_week_inline_danger", fg = "#ca6470", bold = true, dim = false },
  ],
  [
    { token = "$quota_5h_normal", fg = "#84b084", bold = true, dim = false },
    { token = "$quota_5h_warning", fg = "#cdaa65", bold = true, dim = false },
    { token = "$quota_5h_danger", fg = "#ca6470", bold = true, dim = false },
    { token = "$quota_week_normal", fg = "#84b084", bold = true, dim = false },
    { token = "$quota_week_warning", fg = "#cdaa65", bold = true, dim = false },
    { token = "$quota_week_danger", fg = "#ca6470", bold = true, dim = false },
  ],
]
```

- `state_icon` and `tab` are Herdr's built-in status and plane labels.
- `$quota_provider_model` is the compact identity label `Provider/Model`, for
  example `Claude/Sonnet`. When the model is unavailable it contains only the
  provider name. `$quota_provider` and `$quota_model` remain available for
  custom layouts and older configurations.
- Model and provider values are tracked per session, so same-provider panes can
  show different models. If Herdr cannot identify a pane's session, the
  provider-level model may remain visible, but context/cache diagnostics stay
  hidden instead of broadcasting another session's values.
- Default provider labels use recognizable brand colors without affecting quota
  health: Claude soft orange, Codex pastel blue, Grok soft white, and an
  Antigravity-inspired mint for Agy.
- `$quota_topic` comes before the quota rows so the card reads as agent, task,
  then resource status.
- For Codex, an empty/default prompt falls back to the short thread preview from
  the local app-server state database; other providers keep the prompt empty.
- Codex context uses the latest rollout `last_token_usage` against its reported
  model window; Codex cache uses the session token counters. If the latest
  cache-bearing rollout event has a timestamp, `$quota_cache_ttl` shows an
  explicitly approximate one-hour upper-bound estimate based on that activity;
  it is not an exact server expiry. Grok context comes from `signals.json`, and
  Grok cache from the latest usage update. These are local diagnostics, not
  quota-window percentages; missing session fields stay hidden.
- `$quota_context` is the provider-reported context **used** percentage and is
  the penultimate row, immediately above the quota limits. `$quota_cache` is the cumulative hit rate
  for the main session transcript, not a per-turn value; it is shown to one
  decimal place so `99.6%` is not rounded to `100%`. `$quota_cache_ttl` is the
  remaining approximate TTL when Claude exposes a 5m/1h bucket or Codex has a
  timestamped cache-bearing rollout event; when it reaches zero, the red
  `$quota_error` token says `no cached`. Both cache values share one row;
  missing fields are hidden instead of guessed.
- The provider and model share each provider's brand color so same-provider
  cards are easy to scan. Cache, TTL, and context share one muted diagnostic
  color (`#9aa7b8`); only quota runway health and explicit errors use green,
  amber, and red.
- If a five-hour window is present, 5h and 7d stay on the limits row, never
  on the same line as context. If 5h is empty, the weekly token is published
  on the context row instead (`$quota_week_inline_*`) and the empty limits
  row disappears, so the card reads `context · 7d`. This is decided from the
  tokens, not the provider name: Codex splits when OpenAI returns 5h and
  folds when it does not; Grok stays compact; Claude and Agy keep a dedicated
  limits row, including the `5h N/A` placeholder.
- Claude and Agy statusLine diagnostics are keyed by session. A new session
  starts without the previous session's cache/context values; Codex and Grok
  read only visible session files. If Herdr exposes no session id for a pane,
  local context/cache values are hidden until one is available. Per-session
  diagnostic maps are capped at 128 entries, and the watcher uses one
  metadata-only Herdr inventory call per poll, so historical sessions cannot
  grow memory without bound.
- Each window publishes exactly one styled variant. Herdr renders adjacent
  values on the same row with `·` separators and removes separators for missing
  values, so 5h/7d stay compact while retaining independent colors. Color
  follows runway rather than a fixed quota threshold: remaining quota is
  compared with the percentage of window time still left. At or ahead of pace
  is green; behind pace is amber; behind pace with less than 20% quota remaining
  is red. Missing or expired reset data uses the warning color.
- `row_gap = 1` adds one blank row between agent cards. An existing explicit
  `row_gap` value is preserved.
- `$quota_5h`, `$quota_week`, and `$quota_summary` remain available for custom
  unstyled layouts. `$quota_summary` is the compact quota-window summary, not a
  cache-expiry value. Herdr reports no more than sixteen metadata tokens; old
  `$quota_icon`/`$quota_status` fields are cleaned up during migration.

Herdr accepts fixed hex colors for styled tokens, not semantic theme colors.
The default palette uses soft, high-luminance green, amber, and red
tones to reduce eye strain on Herdr's dark sidebar while keeping each health
state easy to scan.

Provider styling uses Herdr's static `rows_by_agent` projection, while quota
health remains dynamic metadata. This keeps branding and health logic separate
and avoids spending additional metadata-token capacity on static labels.

The Herdr plugin API accepts text tokens, not provider image components. For that
reason the default layout uses the readable provider name and keeps Herdr's
native dot instead of adding low-recognition Unicode or SVG markers. The
checked-in [`docs/icons/`](docs/icons/) assets are optional visual references;
they are not injected into the native sidebar.

The topic reader is event-driven: it scans recent pane output after an agent
event and extracts the latest user prompt. It deliberately leaves the topic
empty when no prompt is found instead of showing an AI-generated terminal title
such as `Thinking` or `Executing`. It does not show the working directory.

## Data sources and privacy

- **Codex:** the local official [app-server JSON-RPC](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)
  rate-limit response plus one bounded, state-database-only `thread/list` for
  session previews. For those returned thread ids, the plugin reads only the
  tail of the matching `~/.codex/sessions`/`archived_sessions` rollout JSONL:
  `last_token_usage` supplies current context and the cumulative token bucket
  supplies cache hit rate. It never resumes a thread or starts a model turn.
  API-key authentication is intentionally not mislabeled as a ChatGPT quota;
  only the first non-empty line of at most 50 previews is retained.
- **Grok:** the local `~/.grok/auth.json` login key is read in memory and sent
  to the weekly billing endpoint used by the Grok CLI. The response is accepted
  only when it identifies a weekly period. A bounded scan of the newest local
  session metadata (`signals.json` and the tail of `updates.jsonl`) supplements
  model, context, and cache counters. This is SuperGrok usage, not xAI
  developer/API-team billing. The unified watcher and the existing 60-second
  debounce limit active requests; it never logs in or refreshes the key.
- **Claude Code:** the official [`statusLine` JSON hook](https://code.claude.com/docs/en/statusline)
  supplies the five-hour, seven-day, context-used percentage, and latest
  cache counters. Configure sets Claude's native `refreshInterval` to the
  unified watcher interval when it is plugin-owned (60 seconds by default),
  so an idle session refreshes its absolute reset timestamps; an existing
  user-owned interval is preserved. A previous statusLine command is backed
  up, chained, and restored by the uninstall action. When a transcript path
  and session id are present, the hook accumulates the main session's
  assistant usage using a byte offset; the first update reads the existing
  transcript once and later updates read only appended lines. A cache bucket
  gives the explicitly approximate `ttl≈...`; there is no network request or
  model turn.
- **OpenAI Codex:** the matching rollout tail supplies token counters and event
  timestamps. The adapter uses the latest cache-bearing event to show a local
  approximate one-hour upper-bound TTL, following [OpenAI's prompt-cache
  retention guidance](https://openai.com/index/api-prompt-caching/); the rollout
  does not expose an exact expiry timestamp.
- **Agy/Antigravity:** the official [`/usage` and statusline docs](https://antigravity.google/docs/cli/commands/usage?app=antigravity-ide)
  supply Gemini and third-party pools plus context-used percentage and cache
  counters. When the active model can be identified, the sidebar shows that
  model's pool (`gemini-*` or `3p-*`); unrecognised names still use the lowest
  remaining percentage across both pools. Agy has no reliable TTL field; its
  cache rows appear only when a session transcript/id is supplied.

Snapshots and refresh markers stay in Herdr's plugin state directory. No usage
data is uploaded, browser cookies or browser keychains are read, and provider
credentials are never refreshed or written. Provider failures leave the last
successful local value visible.

The Grok CLI billing endpoint is an internal CLI contract, not a public xAI
developer API stability promise. If it changes, the plugin keeps the previous
weekly value instead of clearing the sidebar.

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| The rows do not appear | Run `herdr server reload-config`, then **Refresh agent quota**. |
| Claude or Agy is `N/A` | Start a conversation so the native `statusLine` emits JSON; then refresh. |
| Claude reset time stays stale while the pane is idle | Run **Install / repair agent quota**, then restart the already-running Claude pane once so it loads the native statusLine refresh interval. |
| Claude briefly changes while switching panes | The cached value is retained; run one prompt or a manual refresh if no snapshot exists yet. |
| Agy has no quota | Run **Install / repair agent quota**, start one Agy turn, then manually refresh. |
| A running Grok goal stays stale | Run **Install / repair agent quota**, then start the next turn; the unified watcher will pick up working sessions. |
| The topic is blank or old | Send a prompt in that pane; topic extraction runs on agent events and needs recent output. |
| Existing Claude statusLine is not changed | Run `configure --check`; the plugin refuses unsafe non-command settings instead of overwriting them. |

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features --locked
cargo build --release --locked
```

CI runs these on Linux and macOS for every pull request.

[`CONTRIBUTING.md`](CONTRIBUTING.md) covers the design rules every parser
follows and how to add a provider. Security reporting is in
[`SECURITY.md`](SECURITY.md), and released changes are in
[`CHANGELOG.md`](CHANGELOG.md).

The cache/context field investigation and open-source comparison are documented
in [`docs/research/cache-observability-open-source.md`](docs/research/cache-observability-open-source.md).
The Grok source investigation is documented in
[`docs/research/codexbar-grok-usage.md`](docs/research/codexbar-grok-usage.md),
and the Codex/Grok local context-cache comparison is in
[`docs/research/codex-grok-context-cache.md`](docs/research/codex-grok-context-cache.md),
the issue-22 display/session design is in
[`docs/research/issue-22-model-display.md`](docs/research/issue-22-model-display.md),
and the implementation contract is in
[`docs/plans/herdr-agent-quota-implementation.md`](docs/plans/herdr-agent-quota-implementation.md).

## Contributing

Adding a CLI is deliberately small: a pure `parse_*` function, a redacted
fixture, and a test. The rules it has to satisfy are in
[`CONTRIBUTING.md`](CONTRIBUTING.md).

If this saved you a pane switch, a ⭐ helps other Herdr users find it. A bug
report with your CLI version is even better — it decides which provider parser
gets fixed next.

## License

MIT. This project is not affiliated with Herdr, OpenAI, Anthropic, xAI, or
Google.
