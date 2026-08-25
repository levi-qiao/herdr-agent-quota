# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Context now follows the provider name, while cache diagnostics use two
  dedicated rows: a one-decimal cumulative session hit rate and the elapsed
  time since the latest cache-bearing response plus an explicitly approximate
  TTL estimate when the provider exposes a cache bucket. Claude/Agy collectors
  use local statusLine/transcript data only; they do not log in or start model
  requests.
- Quota rows now show compact `5h`/`7d` window labels with minutes below one
  hour, hours and minutes below one day, and days plus hours for longer windows.
- Cache hit rate and remaining cache TTL now share one short, color-separated
  sidebar row; the verbose last-activity text is no longer shown.
- Claude's plugin-owned statusLine now receives the configured global watcher
  interval as its native `refreshInterval`, keeping idle-session reset times
  fresh without an API call or model request; existing user-owned intervals are
  preserved.
- Active turns now start one short-lived global refresh watcher for Claude,
  Codex, Grok, and Agy. It reads the working provider set once per poll,
  publishes statusLine cache updates, keeps active fetches debounced, stops
  when all agents settle, and performs final debounced passes per provider.
  Polling defaults to 60 seconds and is configurable from 30 seconds to one
  hour. `install.sh` and `uninstall.sh` provide a build/link/configure and
  restore/unlink workflow for downloaded checkouts.

### Fixed

- Grok no longer sticks at `week 0%` after `grok login` switches accounts. A
  fresh SuperGrok week omits `creditUsagePercent` (proto3 JSON drops zeros),
  which the parser treated as an unsupported response and then kept the previous
  login's exhausted snapshot. Omitted/null percent is 0% used (100% remaining),
  snapshots are stamped with the signed-in `user_id`, and a cache from another
  account is not published. Codex has the same per-provider cache and now
  stamps `tokens.account_id` from `~/.codex/auth.json` so a ChatGPT account
  switch cannot keep the previous user's weekly percent. Claude and Agy read
  the running CLI's statusLine, so they were not affected.
- Claude/Agy statusLine collectors now publish atomic observations without
  waiting on refresh work. Provider refreshes use independent non-blocking
  leases, and chained user statusLine commands run in a bounded process group
  so a stalled command cannot leak processes or wedge later invocations.
- Topic extraction now reads the pane's visible screen instead of rebuilding its
  wrapped scrollback. The old `--source recent` read took 4.45s and repainted the
  pane once per call, which is what the user saw as scrolling; `--source visible`
  costs 0.006s and repaints nothing. The prompt is on screen when a turn starts,
  which is when the topic changes.
- Agent events no longer repaint every pane of a provider. Reading a pane makes
  Herdr repaint it, which the user sees as the agent's terminal scrolling up and
  snapping back to the bottom, once on agent detection and twice per turn
  thereafter. An event now reads only the pane it names and publishes once
  instead of twice; the remaining panes keep the topic they last published.
- A failed or empty topic read now preserves the last published topic instead of
  clearing it, so it no longer churns the token and forces a write on the next
  refresh.
- The legacy per-tool Grok response hook is no longer installed. Existing
  plugin-owned copies are removed during configure because the single global
  watcher now covers active and settled turns without spawning one command per
  tool call.
- Expired cache TTL estimates now render as a red `no cached` diagnostic, and
  Claude payloads without quota fields clear stale window values instead of
  leaving an old weekly reset on the sidebar.
- Weekly-only providers now use the readable `week ... reset ...` label; the
  compact `5h`/`7d` form remains for providers that expose both windows.

- Agent topics now come only from the latest user prompt in pane output. Native
  `Thinking`/`Executing` titles and other AI status text are no longer published
  as the user's topic, including Grok's `❯` prompt format.
- Codex Unix timestamps and Claude's Unix/RFC 3339 statusLine variants now
  normalize correctly; Grok RFC 3339 period ends and Agy relative reset seconds
  use the same cached absolute time.

- The Claude collector stays visually silent when there was no previous
  `statusLine`, avoiding a plugin-owned line that Claude repaints after each
  interaction. Existing custom status lines are still chained unchanged.
- A pane that exits between `herdr agent list` and the metadata report no
  longer aborts the whole publish, so the remaining live panes still update.
- Closed a race in the Codex app-server watchdog that could signal an unrelated
  process after the child had been reaped and its pid recycled. The watchdog
  and the request thread now share the child and terminate it at most once.
- A failed cache rename no longer leaves its scratch file behind.

- Claude and Agy statusLine hooks now only update the local cache, so repainting
  an agent's own status line cannot synchronously call back into Herdr or move
  the terminal viewport. Metadata reports are skipped when every displayed
  token is unchanged.
- Focus changes now use a dedicated provider-only, 60-second-debounced refresh.
  This path never reads pane content or refreshes topics, and metadata writes
  remain suppressed while the selected pane is in scrollback.
- Agent detection and status events now refresh and read topics only for the
  affected provider. Pane exit no longer starts a refresh after its metadata
  consumer is already gone; incomplete event payloads still fall back to all
  providers.
- `configure --apply` now binds `prefix+shift+r` to the force-refresh action
  when that key is free, while preserving an existing user-owned binding.
  `configure --uninstall` removes only the plugin action binding.
- Grok now invokes a silent, provider-only refresh directly from `PostToolUse`
  during long-running turns, with turn-end hooks covering final, failed, and
  cancelled replies. It no longer routes these refreshes through a Herdr action,
  and remains debounced to avoid request storms.
- Quota-only refreshes no longer read every agent pane before publishing. They
  preserve the last topic token, update the sidebar as soon as quota collection
  finishes, and leave full topic extraction to agent lifecycle events.
- Agy's statusLine collector is now installed, repaired, chained, and removed
  by the same configuration lifecycle as Claude's, and remains silent when no
  user-owned status line existed.
- Configuration actions now install or uninstall all plugin-owned integrations
  in one pass and reload Herdr automatically. They also repair legacy
  collectors that pointed at a different cache directory while preserving any
  previous user statusLine backup.
- Metadata publication now skips panes whose viewport is in scrollback, so a
  Herdr repaint cannot pull the user back to the bottom. The next refresh after
  returning to the bottom catches the sidebar up.

### Changed

- The context row now uses a dedicated violet accent immediately after the
  provider name. Cache hit rate and remaining TTL share one row with separate
  teal and amber accents, while metadata publication remains capped at sixteen
  tokens.
- Quota formatting is centralized in one presentation module shared by the
  sidebar, dashboard, and statusLine fallbacks. Codex remains weekly-only.
- Five-hour and weekly quota windows now share one compact sidebar row. Herdr
  elides missing tokens and their separators, while each window keeps its own
  dynamic health color.
- Sidebar agent cards default to one blank row of separation, while preserving
  an existing `row_gap`. The latest user prompt now precedes compact,
  single-spaced quota rows, and percentages render as whole numbers.
- Default sidebar styling compares quota remaining with window time remaining:
  on-pace usage is bold green, behind-pace usage is brighter amber, and
  behind-pace usage below 20% remaining is bold red.
- Provider labels now use separate brand-aware `rows_by_agent` styling: Claude
  soft orange, Codex pastel blue, soft white for Grok, and Antigravity-inspired
  mint for Agy. Quota health colors use the same low-strain pastel palette.
  Existing user-owned agent row overrides remain untouched.

- Dropped the unmaintained `fs2` dependency in favour of the standard library's
  file locking, and made `libc` a Unix-only dependency.
- Removed the redundant `pkill` shell-out when tearing down the Codex
  app-server; killing the process group already covers its children.

## [0.1.0]

### Added

- Live Claude Code, Codex, Grok, and Agy/Antigravity subscription quotas in
  Herdr's agent sidebar, as five-hour and weekly remaining percentages.
- `configure --apply` / `--check` / `--uninstall` for a reversible, idempotent
  sidebar and Claude `statusLine` setup.
- A popup dashboard pane, event-driven refresh, and a local snapshot cache that
  survives provider failures.

[Unreleased]: https://github.com/levi-qiao/herdr-agent-quota/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/levi-qiao/herdr-agent-quota/releases/tag/v0.1.0
