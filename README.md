# herdr-agent-quota

**Never hit a quota limit mid-task.** Live Claude Code, Codex, Grok, and
Agy/Antigravity subscription usage, in Herdr's agent sidebar.

[![CI](https://github.com/levi-qiao/herdr-agent-quota/actions/workflows/ci.yml/badge.svg)](https://github.com/levi-qiao/herdr-agent-quota/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/built%20with-Rust-dea584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Herdr plugin](https://img.shields.io/badge/Herdr-plugin-0.8%2B-5b6ee1)](https://herdr.dev/docs/plugins/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![GitHub stars](https://img.shields.io/github/stars/levi-qiao/herdr-agent-quota?style=social)](https://github.com/levi-qiao/herdr-agent-quota)

中文文档：[README.zh-CN.md](README.zh-CN.md)

```text
● Owner · Claude
  hi                     ← what that pane is actually working on
  context 23%             ← provider-native context percentage
  cache 99.6% · ttl≈58m    ← session hit rate · remaining cache TTL
  5h 100% 3h07m · 7d 31% 2d3h
```

![Live Herdr agent sidebar](docs/screenshots/herdr-sidebar-live.png)

*A real Herdr workspace: Claude shows five-hour and weekly reset ETAs on one
compact row, Codex and Grok show their weekly windows, and each agent card uses
the latest user prompt rather than an AI-generated status.*

- **Four CLIs, one sidebar** — Claude Code, Codex, Grok, Agy/Antigravity.
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

Requirements: Herdr `0.8.0+`, Rust `1.95+`, macOS or Linux, and at least one
supported CLI. If you downloaded this repository, the one-step installer does
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

Herdr plugin v1 does not currently let plugins add buttons to the native agent
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

## Supported CLIs

| CLI | Sidebar windows | Local collection path | Extra setup |
| --- | --- | --- | --- |
| Claude Code `2.1.233` | `5h` + `7d` + context + cache hit/approx. TTL | Official `statusLine` JSON: `rate_limits`, `context_window`, and `transcript_path` | The configure action installs/chains it and keeps its refresh interval current |
| OpenAI Codex `0.147.0` | `week` + local session summary | One-shot local `codex app-server --stdio`: quota and bounded `thread/list` | ChatGPT subscription login; API-key mode is shown as unavailable |
| Grok CLI / Grok Build `1.0.4` | `week` | Local `~/.grok/auth.json` and the billing contract used by the official CLI | Covered by the unified watcher; no response hook is installed |
| Agy / Antigravity CLI `1.1.13` | `5h` + `week` + context + cache hit | Official `statusLine` JSON: `quota` and `context_window` | The configure action installs and chains it automatically |

Versions above were checked on the development machine on 2026-08-15. The
parser follows the provider fields rather than hard-coding these version
strings, so newer compatible CLI releases can continue to work.

The sidebar shows **percentage remaining** and the time until each quota reset,
not quota token counts. The two Claude windows use compact `5h` and `7d`
labels on one row; each still keeps its own dynamic health color. Claude and
Agy also show provider-reported context percentage. When a statusLine transcript
and session id are available,
`cache N.N%` is the cumulative main-session ratio
(`read / (fresh + creation + read)`), not the latest turn. The same row shows,
for Claude, a `ttl≈...` estimate from the provider's 5-minute/1-hour bucket.
This is local diagnostic math, not a server-confirmed expiry; the first session
update reads the existing transcript once, then later updates read only
appended bytes.
Codex shows a short session preview from its local state database; its live
context and cache fields are not queried because the current safe app-server
connection is quota-only and does not attach to an active thread. Grok's
current billing source has neither context nor cache fields. During a working
turn, one short-lived global watcher polls once per configured interval,
coalesces active fetches, and exits when all selected providers settle. The
sidebar does not run a permanent daemon.

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
  ["state_icon", "tab", { token = "$quota_provider", bold = true, dim = false }, { token = "$quota_context", fg = "#9b8fd8", bold = true, dim = false }],
  [{ token = "$quota_topic", dim = false }],
  [
    { token = "$quota_cache", fg = "#6fb5b7", bold = true, dim = false },
    { token = "$quota_cache_ttl", fg = "#cdaa65", bold = true, dim = false },
    { token = "$quota_error", fg = "#ca6470", bold = true, dim = false },
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
- `$quota_provider` is `Claude`, `Codex`, `Grok`, or `Agy`.
- Default provider labels use recognizable brand colors without affecting quota
  health: Claude soft orange, Codex pastel blue, Grok soft white, and an
  Antigravity-inspired mint for Agy.
- `$quota_topic` comes before the quota rows so the card reads as agent, task,
  then resource status.
- For Codex, an empty/default prompt falls back to the short thread preview from
  the local app-server state database; other providers keep the prompt empty.
- `$quota_context` is the provider-reported context **used** percentage and sits
  directly after the provider name. `$quota_cache` is the cumulative hit rate
  for the main session transcript, not a per-turn value; it is shown to one
  decimal place so `99.6%` is not rounded to `100%`. `$quota_cache_ttl` is the
  remaining approximate TTL when Claude exposes a 5m/1h bucket; when it reaches
  zero, the red `$quota_error` token says `no cached`. Both cache values share
  one row; missing fields are hidden instead of guessed.
- The context row uses a violet accent (`#9b8fd8`) so context pressure is easy
  to distinguish from the green/amber/red quota runway colors.
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

Herdr 0.8 only accepts fixed hex colors for styled tokens, not semantic theme
colors. The default palette uses soft, high-luminance green, amber, and red
tones to reduce eye strain on Herdr's dark sidebar while keeping each health
state easy to scan.

Provider styling uses Herdr's static `rows_by_agent` projection, while quota
health remains dynamic metadata. This keeps branding and health logic separate
and avoids spending additional metadata-token capacity on static labels.

Herdr plugin v1 accepts text tokens, not provider image components. For that
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
  session previews. The plugin accepts the seven-day window by duration, rather
  than assuming which field is primary. API-key authentication is intentionally
  not mislabeled as a ChatGPT subscription quota. It does not resume threads or
  read rollout JSONL, so it cannot claim a live context percentage. Only the
  first non-empty line of at most 50 previews is retained, truncated to 80
  characters.
- **Grok:** the local `~/.grok/auth.json` login key is read in memory and sent
  to the weekly billing endpoint used by the Grok CLI. The response is accepted
  only when it identifies a weekly period. This is SuperGrok usage, not xAI
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
- **Agy/Antigravity:** the official [`/usage` and statusline docs](https://antigravity.google/docs/cli/commands/usage?app=antigravity-ide)
  supply Gemini and third-party pools plus context-used percentage and cache
  counters. When both pools exist, the sidebar uses the lowest remaining
  percentage so the single Agy row is conservative. Agy has no reliable TTL
  field; its cache rows appear only when a session transcript/id is supplied.

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
