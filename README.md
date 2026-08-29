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

![Live Herdr agent sidebar](docs/screenshots/herdr-sidebar-live.png)

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

Refresh or uninstall:

```sh
herdr plugin action invoke herdr-agent-quota.refresh
./uninstall.sh
```

## Coverage

| Harness | Subscription quota | Exact-session diagnostics | Attribution rule |
| --- | --- | --- | --- |
| Claude Code | 5h + 7d | model, context, cache, recorded 5m/1h TTL | Claude statusLine session |
| OpenAI Codex | 5h + 7d | model, context, cache, session summary | Canonical ChatGPT login; API keys are not subscription quota |
| Grok CLI | 7d | model, context, cache | Canonical Grok login |
| Agy / Antigravity | 5h + 7d | model, context, cache | Agy statusLine session and active model pool |
| OpenCode | OpenCode Go 5h + 7d; 30d in dashboard | model and context for the exact local session | Go only for `opencode-go` with its matching key; other backends never borrow it |
| Pi | Existing Codex quota when safely matched | model, context, cache; Anthropic recorded TTL | Only `openai-codex` OAuth with the same canonical Codex account id |

Quota values are percentages remaining plus reset time. Context is percentage
used. Cache is a session hit ratio where the upstream session format supports
one. TTL is shown only when a recorded provider bucket makes it defensible:
Claude and Pi/Anthropic 5-minute or 1-hour cache creation. Codex and other Pi
providers keep cache hit rate but leave TTL blank.

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

- Rows whose tokens are all empty collapse; plugin-owned layouts use
  `row_gap = 0`. User-owned spacing is preserved.
- Provider/model, topic, cache/TTL, context, and limits appear only when known.
- 5h and 7d colors compare remaining quota with remaining time: green is on
  pace, amber is ahead of pace, and red is ahead of pace with under 20% left.
- Events read only their named pane with `--source visible`. Startup, focus,
  refresh, and the active-turn watcher never read pane output.
- Metadata is written only when a token changed and remains under Herdr's
  16-token limit.

The default watcher interval is 60 seconds and can be changed during install:

```sh
./install.sh --watch-interval-seconds 300
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
