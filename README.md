# herdr-agent-quota

Live, credential-scoped AI quota and context in Herdr's agent sidebar.

[![CI](https://github.com/levi-qiao/herdr-agent-quota/actions/workflows/ci.yml/badge.svg)](https://github.com/levi-qiao/herdr-agent-quota/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/built%20with-Rust-dea584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

中文文档：[README.zh-CN.md](README.zh-CN.md)

```text
● Owner · Claude/Sonnet
  fix the release check
  cache 99.6% · ttl≈58m
  context 23%
  5h 100% 3h07m · 7d 31% 2d3h
```

That packed layout is the default. `--sidebar-layout stacked` puts provider, model,
and each quota field on its own row so a narrow sidebar does not truncate
`tab · Claude/Sonnet`, `cache · ttl`, or `5h · 7d`:

```text
● Owner
  Claude
  Sonnet
  fix the release check
  cache 99.6%
  ttl≈58m
  context 23%
  5h 100% 3h07m
  7d 31% 2d3h
```

<table>
<tr>
<th>packed (default)</th>
<th>stacked</th>
</tr>
<tr>
<td valign="top"><img src="docs/screenshots/sidebar-packed.png" alt="Packed sidebar" width="284"></td>
<td valign="top"><img src="docs/screenshots/sidebar-stacked.png" alt="Stacked sidebar" width="177"></td>
</tr>
</table>

The plugin shows only data it can attribute to the pane's exact session and
credential scope. Missing fields collapse automatically; failed refreshes keep
the last good quota; confirmed pay-as-you-go sessions clear stale subscription
numbers.

## Install

Requirements: Herdr 0.8.0+, Rust 1.95+, macOS or Linux, and at least one
supported agent CLI.

```sh
./install.sh
```

Already-running agent panes should be restarted once after installation.

### Required Herdr integrations

Herdr must know a pane's session before quota can be attributed. Check its
built-in integrations:

```sh
herdr integration status
```

Install any missing integration, then restart that agent pane:

```sh
herdr integration install opencode
herdr integration install pi
```

If an integration is missing, the pane may be detected but remain blank.
`configure --check` and `configure --apply` also print the exact repair command.

### Install only selected agents

```sh
herdr-agent-quota configure --apply --agent claude,codex,pi
```

Accepted values are `all`, `claude`, `codex`, `grok`, `agy`, `opencode`, and
`pi`. The installer never replaces user-owned sidebar rows or statusLine hooks.

### Change settings without reinstalling

Open the **Agent quota settings** plugin pane. Everything the installer flags
set can be changed there, including which agents are installed:

```
Agent quota settings
====================
  ─ Display ───
>   Percentages     <   used    > how much quota is spent
    Sidebar layout  <  packed   > cache·ttl and 5h·7d share a row
    Row gap         <     1     > one blank row between panes
    Watch interval  <    1m     > polled while an agent is working
    Brand colors    <    on     > provider and model in agent hues
    Agent order     <  default  > Herdr sorts the agent panel
    Low quota alert <    off    > no notification
  ─ Fields ───
    [x] topic   [x] model   [x] cache   [x] ttl
    [x] context [x] 5h      [x] 7d
  ─ Agents ───
    [x] claude  [x] codex   [x] grok    …

  ↑↓ row  ←→/space value  a apply  q quit
```

`*` marks an edit that has not been applied. `a` runs the same
`configure --apply` the "Install / repair" action runs, reloads Herdr's
configuration, and forces one refresh, so the new rows and the values inside
them both appear immediately.

Unchecking an agent **uninstalls** that agent's collector and gives its own
statusLine entry back, so it asks for a second `a` before doing it. The last
agent cannot be unchecked.

Fields are the optional quota values. The provider name and the error token are
always shown: a row that cannot say which subscription it belongs to, or that
hides the reason quota is missing, is worse than no row.

**Agent order** `quota` puts the agent with the least quota left at the top of
Herdr's Agent panel. It works by handing Herdr a declarative agent view sorted
by a hidden `quota_headroom` token this plugin publishes, so the ordering
follows the same number the sidebar shows — the tighter of the 5h and 7d
windows. Herdr keeps one such view, so turning this on replaces your own
`ui.agent_panel_sort` policy until you set it back to `default`; uninstalling
hands the panel back too. The view does not survive a Herdr restart, and the
plugin's startup hook puts it back.

**Low quota alert** notifies once, per provider, when its remaining quota falls
to the chosen percentage or below. It stays quiet for as long as the quota
stays low and warns again only after the quota has recovered above the
threshold — a window that resets and is spent again warns again. `off` is the
default and never notifies.

The same choices from the command line:

```sh
./install.sh --fields topic,model,context,5h,7d
./install.sh --brand-colors off
./install.sh --agent-order quota --low-quota-alert 10
herdr-agent-quota configure --apply --fields all --brand-colors on
```

Refresh or uninstall:

```sh
herdr plugin action invoke herdr-agent-quota.refresh
./uninstall.sh
```

## Coverage

| Harness | Subscription quota | Exact-session diagnostics | Attribution rule |
| --- | --- | --- | --- |
| Claude Code | 5h + 7d | model, context, cache, statusLine `prompt_cache` expiry | Claude statusLine session |
| OpenAI Codex | 5h + 7d | model, context, cache, estimated cache TTL, session summary | Canonical ChatGPT login; API keys are not subscription quota |
| Grok CLI | 7d, or 30d on a monthly plan | model, context, cache | Canonical Grok login |
| Agy / Antigravity | 5h + 7d | model, context, cache | Agy statusLine session and active model pool |
| OpenCode | OpenCode Go 5h + 7d; 30d in the dashboard pane | model and context for the exact local session | Go only for `opencode-go` with its matching key; other backends never borrow it |
| Pi | Existing Codex quota when safely matched | model, context, cache; Anthropic recorded TTL, Codex estimated TTL | Only `openai-codex` OAuth with the same canonical Codex account id |

Quota values are percentages remaining plus reset time. The sidebar shows a 5h
window and one longer window; the longer slot carries the weekly allowance, or
the monthly one when a plan has no weekly bucket. The period label travels with
the value (`7d 69% 6d0h`, `30d 70% 17d8h`), so a monthly number is never shown
as a weekly one, and a weekly window always wins the slot when both exist. The
dashboard pane lists every window a plan has, including 30d alongside 7d.
Context is percentage used. Cache is a session hit ratio where the upstream session format supports
one. TTL comes from a recorded expiry where one exists: Claude Code
`prompt_cache.expires_at` (v2.1.251+) and Pi/Anthropic `cacheWrite1h`. Codex
records neither a TTL nor an expiry - its rollout JSONL carries only the cache
token counts and the request timestamp - so the sidebar estimates it as the
30 minute `prompt_cache_options.ttl` that the Responses API documents as its
default and only supported value, anchored to the last recorded request. It is
labelled `ttl≈` like every other countdown and it is an estimate: a changed
prefix, a compaction, or a changed tool/system definition can drop the hit rate
before that timer runs out. Grok, Agy, OpenCode, and other Pi backends have no
cache-entry expiry and no documented TTL in their local contracts, so they keep
cache hit rate and leave TTL blank.

Pi reads only the absolute JSONL path reported by Herdr under `~/.pi/agent` or
`PI_CODING_AGENT_DIR`. It does not scan every session. API-key sessions are
confirmed PAYG and clear old quota; missing, malformed, unsupported, or
different-account evidence preserves prior quota.

OpenCode reads one exact session id from `opencode.db` and the matching provider
entry in `auth.json`. Model/context comes from that session and OpenCode's
bounded local model cache even when the backend has no supported subscription
collector.

### OpenCode Go needs real-world verification

The maintainer does **not** have an OpenCode Go subscription. The local session,
credential, request, error, and fail-closed paths are tested against OpenCode
1.18.20. The successful usage response shape comes from
[CodexBar](https://github.com/steipete/CodexBar) and is documented in
[`docs/research/opencode-go-usage.md`](docs/research/opencode-go-usage.md), not
from a response observed by this repository.

Unexpected or malformed responses publish nothing, and 401/403 never become
zero usage. If you have OpenCode Go, a sanitized response or a report that the
numbers match would be very helpful. Issues and PRs are welcome.

## Sidebar behavior

- Two layouts: `packed` joins tab with provider/model, cache with TTL, and 5h
  with 7d; empty 5h still folds 7d onto context. `stacked` gives provider, model,
  cache, TTL, context, 5h, and 7d their own rows. Empty tokens collapse in both.
- Rows whose tokens are all empty collapse. Plugin-owned layouts use
  `row_gap = 1` (one blank row between panes). Pass `--row-gap 0` to pack them
  flush. Herdr only accepts whole rows; a user-owned `row_gap` is left alone.
- Every quota field except the provider name and the error token can be turned
  off with `--fields`, and `--brand-colors off` drops the per-agent hues while
  keeping the severity colors.
- Quota percentages read as remaining quota by default. `--quota-percent used`
  flips every 5h/7d/30d number to what has been consumed instead; the sidebar
  token keeps its width, and the color still follows what is left, so red
  always means "little runway".
- Provider/model, topic, cache/TTL, context, and limits appear only when known.
- Tab names use primary text (`#eceef2`). The prompt is body text
  (`#c8cdd6`). Cache, TTL, and context stay muted (`#969eae`). Brand color is
  only on provider; model uses the dim sibling. Selected state may change the
  card background, never the provider hue.
  Each 5h/7d window is one compact token (`5h 0% 1h18m`), space-separated,
  because Herdr joins sibling tokens with ` · `. The remaining-percent color
  is green at 50%+, amber at 20–49%, and red below 20%. Packed still puts a
  ` · ` between the two windows. A lapsed prompt cache shows `no cached` in
  the same amber, in its own token: it is a normal state, and never shares a
  token with the error raised when quota cannot be read at all.
- Events read only their named pane with `--source visible`. Startup, focus,
  refresh, and the active-turn watcher never read pane output.
- Metadata is written only when a token changed and remains under Herdr's
  16-token limit.

The default watcher interval is 60 seconds. Sidebar layout is `packed`, row gap
is `1`, and percentages are `remaining` unless you pass something else; every
choice persists across a later repair:

```sh
./install.sh --watch-interval-seconds 300
./install.sh --sidebar-layout stacked
./install.sh --row-gap 0
./install.sh --quota-percent used
herdr-agent-quota configure --apply --sidebar-layout packed --row-gap 1
herdr-agent-quota configure --apply --quota-percent remaining
```

## Data and privacy

- Credentials are read in memory from the agent's own store and are never
  logged, cached, refreshed, or placed in command arguments.
- Local session reads are exact and bounded. OpenCode uses a read-only SQLite
  lookup; Pi and other JSONL readers enforce byte and line limits.
- No browser cookies or keychains are scraped. No prompt is sent and no model
  request is started.
- Snapshots contain sanitized percentages and reset times only and remain in
  Herdr's plugin state directory.
- Network requests are limited to each supported CLI's billing endpoint and
  fail closed without replacing a last good snapshot.

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| OpenCode or Pi is blank | Run `herdr integration status`, install the missing integration, then restart that pane. |
| Rows do not appear | Run **Install / repair agent quota** and `herdr server reload-config`. |
| Claude or Agy is `N/A` | Send one turn so its statusLine emits a snapshot. |
| Pi model/context is old | Send one Pi turn; a model selection is confirmed after a successful assistant message. |
| OpenCode has model/context but no quota | This is expected for Zen, PAYG, OAuth, or an unverified/missing Go key. |
| A refresh fails | The last good same-account value is intentionally retained. |
| Codex 5h is from a previous ChatGPT login | `codex login` rewrites `~/.codex/auth.json`; the next refresh should hide 5h when the new account has only 7d. Send one turn if the sidebar still looks stale. |
| `cache · ttl` or `5h · 7d` is truncated | Herdr does not wrap sidebar tokens. Reinstall with `./install.sh --sidebar-layout stacked`. |

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features --locked
cargo build --release --locked
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for parser and fixture rules,
[`SECURITY.md`](SECURITY.md) for security reporting, and
[`CHANGELOG.md`](CHANGELOG.md) for released changes.

## License

MIT. This project is not affiliated with Herdr, OpenAI, Anthropic, xAI,
Google, or OpenCode.
