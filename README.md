# herdr-agent-quota

Model, context, prompt-cache usage, and subscription quota in Herdr's Agent sidebar.

[![CI](https://github.com/levi-qiao/herdr-agent-quota/actions/workflows/ci.yml/badge.svg)](https://github.com/levi-qiao/herdr-agent-quota/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[简体中文](README.zh-CN.md)

<table>
<tr><th>packed (default)</th><th>stacked</th></tr>
<tr>
<td valign="top"><img src="docs/screenshots/sidebar-packed.png" alt="Packed sidebar" width="284"></td>
<td valign="top"><img src="docs/screenshots/sidebar-stacked.png" alt="Stacked sidebar" width="177"></td>
</tr>
</table>

The plugin preserves Herdr's native machine/workspace/tab row, custom styles,
and worktree grouping. The branded provider/model line is the agent identity;
the native `agent` row is omitted so `grok` does not sit above `Grok/grok-4.6`.
Optional quota ordering and low-quota notifications are disabled by default.
Empty fields collapse; percentages can show remaining or used quota.

## Install and upgrade

Requires **Herdr 0.9.0+**, the Rust toolchain pinned in `rust-toolchain.toml`,
macOS or Linux, and a supported agent CLI.

```sh
git clone https://github.com/levi-qiao/herdr-agent-quota.git
cd herdr-agent-quota
./install.sh
```

To enable a subset, use `./install.sh --agent claude,codex,omp`.
Existing sessions need restarting only when newly installed hooks or Herdr
integrations must be loaded.

Upgrade from the repository directory:

```sh
git pull --ff-only
./install.sh
```

Upgrades retain saved preferences, repair managed configuration, refresh quota,
and restore background updates automatically. No cache deletion or watcher
management is required. Changes to the Herdr server connection are adopted by
the watcher automatically.

## Settings

Press `prefix+shift+q`, or run the following if that key is already assigned:

```sh
herdr plugin pane open --plugin herdr-agent-quota --entrypoint settings --focus
```

<img src="docs/screenshots/settings.png" alt="Agent quota settings" width="760">

| Setting | Options |
| --- | --- |
| Percentages | Remaining or used; colors always indicate remaining headroom |
| Layout | `packed` groups related fields; `stacked` gives each field a row |
| Row gap | Zero or one blank line between agents |
| Watch interval | 30 seconds–1 hour; default 60 seconds |
| Fields | Topic, model, cache, TTL, context, short/long quota |
| Brand colors | On or off |
| Agent order | Herdr default or lowest remaining quota first |
| Low quota alert | Off or a threshold from 1% to 100% |
| Agents | Claude, Codex, Grok, Agy, OpenCode, Pi, OMP, Devin |

Use arrows or Space to edit, `a` to apply, and `q` to close.
Installer options are also available through `./install.sh --help`.

## Data sources and limits

| Agent | Quota source | Attribution |
| --- | --- | --- |
| Codex | Codex app-server; 5h and/or 7d | Current login in the plugin's `CODEX_HOME` |
| Grok | CLI billing endpoint; 7d or 30d | Current CLI credentials |
| Devin | CLI usage endpoint; 1d and 7d | Current CLI credentials |
| Claude Code | StatusLine; 5h and 7d | Exact session observation |
| Agy / Antigravity | StatusLine; 5h and 7d | Exact session and identifiable model pool |
| OpenCode | OpenCode Go usage endpoint | Go credential; confirmed PAYG routes have no subscription quota |
| Pi | Canonical Codex quota | Only when the recorded account matches |
| OMP | `omp usage --json --provider <id>` | Reported account matching the session's credential pin |

Quota windows retain their provider's meaning. Model, context, and cache data
come from the identified session when available. `ttl≈` marks an estimated
prompt-cache lifetime, not a guaranteed expiry. Topic extraction uses only the
named pane's visible screen and preserves the last topic when it scrolls away.

All supported working agents participate in one background watcher. Requests
are debounced for 60 seconds, including a final refresh after a turn settles.
OMP additionally retains its own five-minute usage cache. Idle panes sharing a
verified quota source receive the same reading.

Native Codex, Grok, and Devin collectors follow the plugin's current login,
not separate accounts for each pane. Claude/Agy do not report a reliable serving
account ID, so their observations are not shared across sessions. Unknown
identity or model-pool attribution does not produce a guessed quota. Failed
requests preserve the last verified reading for that same account; they do not
turn failures into zero usage.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Session data is missing | Run `herdr integration status`; load missing integrations before restarting the affected agent |
| Claude/Agy quota is missing | Send a turn so the session's StatusLine produces an observation |
| OMP quota is missing | Check `omp usage --json --redact --provider <id>` |
| Devin quota is missing | Check the CLI login and `DEVIN_CREDENTIALS_FILE` if customized |
| Rows are missing | Run the configure action below to repair managed configuration |
| Packed rows are truncated | Select `stacked` |

```sh
herdr plugin action invoke refresh --plugin herdr-agent-quota
herdr plugin action invoke configure --plugin herdr-agent-quota
```

Uninstall everything with `./uninstall.sh`, or remove a subset with
`./uninstall.sh --agent grok`. Configuration changes are reversible; user-owned
settings and other agents remain intact.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development and validation,
[SECURITY.md](SECURITY.md) for data handling and vulnerability reports, and
[CHANGELOG.md](CHANGELOG.md) for release notes. Dated investigations are indexed
in [docs/README.md](docs/README.md).

## License

[MIT](LICENSE). Not affiliated with Herdr or the supported AI providers.
