# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Devin CLI is a supported harness: `--agent devin`, its own settings row,
  and a sidebar row with a Devin brand color. Quota comes from the same
  Connect RPC `GetUserStatus` contract the Devin CLI uses, with the key read
  from `~/.local/share/devin/credentials.toml` (or `$DEVIN_CREDENTIALS_FILE`).
  Daily and weekly remaining percentages are flipped to used. The configured
  default model comes from `~/.config/devin/config.json` `agent.model` when
  present, then mapped through local `devin-models.json` for a display name.
  New sessions that never run `/model` use this same value. It is published
  as `snapshot.model` and shown on every Devin pane. The API
  `planInfo.planName` is the subscription plan and is not used as a model.
  A missing or malformed models catalog leaves the raw id and does not fail
  the quota fetch.
  Snapshots are stamped with `sha256("devin\0" || key)`
  so a credential swap cannot keep the previous account's last-good value.
  The API key is never logged, stored, or included in error messages.
- `configure --apply` rewrites the shared `ui.sidebar.agents.rows` array only
  when it is empty, already managed by this plugin, or matches Herdr's default
  `["state_icon", "agent"]` row. Rows from another plugin or the user are
  left intact; `rows_by_agent`, managed `row_gap`, and keybindings are still
  added or updated. `workspace` and `pane` tokens are not treated as a safe
  default, so they cannot be silently replaced with `tab`.

### Changed

- Devin private orchestration under `.devin/` is not part of the published
  tree. Local Devin state stays gitignored, matching `.agents/`.

### Fixed

- `configure` no longer runs `normalize_official_row` when generating
  `rows_by_agent` from user-owned shared rows, so tokens such as `pane` and
  `terminal_title_stripped` stay intact. With brand colors off, those custom
  shared rows still get plugin-managed per-agent quota rows — only the brand
  hue is omitted. Default Herdr rows are unchanged: brand off still writes no
  `rows_by_agent` copies.
- `configure` prints which user-owned `rows_by_agent` entries it left alone,
  instead of succeeding silently without installing quota for those agents.
- Sidebar tab names, topics, cache details, context, and unknown quota states
  now inherit Herdr's active theme instead of using text colors tuned for a
  dark background. Provider brand hues and quota severity colors remain
  plugin-owned because they carry plugin-specific meaning.

## [1.3.0] - 2026-09-01

### Added

- omp (oh-my-pi) is a supported harness: `--agent omp`, its own settings row,
  and automatic installation of Herdr's `omp` integration when selected. Model,
  context, and cache come from the same transcript reader Pi uses — omp is a
  fork of Pi and still writes JSONL v3 — with two field renames handled in the
  shared parser (`cttl.ephemeral1h` for Anthropic's one-hour cache writes, and
  omp's authoritative `contextTokens`). The agent directory is recovered from
  the absolute session path Herdr reports rather than from this process's
  environment, so a pane started under `PI_CONFIG_DIR` or `--profile` is read
  against its own state.
- omp quota comes from omp's own usage layer, `omp usage --json --provider
  <id>`, and is cached in a credential scope of its own: an omp pane billed to
  Claude never reads (or writes) the canonical Claude snapshot, because the two
  can be different subscriptions. The call is debounced to once a minute per
  provider and omp answers it from its own five-minute usage cache, so it is
  not a provider request per event. `omp usage` is asked only about the one
  provider the pane is talking to, never about the whole credential pool.
- omp records the account that served a session as a `credential_pin`; the same
  digest is recomputed from the usage report's identity, so two accounts on one
  provider each get their own quota. With several accounts and no pin the pane
  shows no quota rather than a peer account's, and a provider that only holds
  an API key in omp is confirmed pay-as-you-go and clears stale quota.
- `HERDR_AGENT_QUOTA_OMP_BIN` overrides the `omp` executable.
- An omp OAuth login that is attributable to the pane but has no usage report
  now renders quota as `N/A` instead of looking unsupported. A last-good
  snapshot for that same account is still preserved, and failed first fetches
  now honor the one-minute debounce instead of spawning `omp usage` again on
  every pane event.
- OMP quota windows are rendered with OMP's normalized labels instead of a
  second set of per-provider rules. Daily and monthly reports such as `1d` and
  `Monthly` now reach the existing short/long sidebar rows rather than being
  dropped.
- **Open agent quota settings** is a plugin action bound to
  `prefix+shift+q`; Herdr 0.8 does not expose extension points for its built-in
  Settings tabs or bottom-right menu.

### Changed

- OMP now uses the previous OpenCode violet brand color; OpenCode uses the
  neutral identity color OMP previously inherited.
- Both READMEs were reduced to feature, support, settings, integration, and
  troubleshooting tables, and now include the full settings-pane screenshot
  and copy-ready commands.

### Fixed

- OMP providers such as `google-antigravity` now route through OMP's generic
  usage collector instead of being excluded by the legacy provider allowlist.
  Provider ids have isolated hashed cache/debounce keys.
- The settings popup no longer repeats its title inside Herdr's titled pane.

## [1.2.0] - 2026-09-01

### Added

- `--agent-order quota` sorts Herdr's Agent panel by the least quota left, so
  the agent closest to its limit is at the top. It is a Herdr `agent.view.set`
  owned by this plugin, sorted on a new `quota_headroom` token — the remaining
  percentage of the tighter of the pane's 5h and 7d windows, zero-padded so
  Herdr's ordering of the token text is also its numeric ordering. The token is
  published for every pane whose quota is known, whether or not the order is
  enabled, because nothing renders it: that makes changing the order a
  Herdr-side toggle rather than a metadata write to every pane, and it costs no
  extra writes, since the value only moves when a quota token beside it moves
  anyway. Herdr keeps one Agent view, so this replaces the user's own
  `ui.agent_panel_sort` until it is set back to `default`; a full uninstall
  hands the panel back. The view does not survive a Herdr restart, so the
  plugin's startup hook re-applies it — and only when it is this plugin's to
  re-apply, leaving a view someone else owns alone.
- `--low-quota-alert <percent>` shows one Herdr notification when a provider's
  remaining quota falls to that percentage or below. One warning per provider,
  not per pane; silent for as long as the quota stays low; re-armed only by
  recovering above the threshold, so a window that resets and is spent again
  warns again. A provider with no pane in a pass keeps its state, so closing
  and reopening a pane is not a way to be warned twice. `off` is the default:
  a plugin that starts notifying after an upgrade is a plugin people turn off.
- A `startup` subcommand, now the plugin's startup hook. It restores the Herdr
  state this plugin owns before running the refresh the hook used to run on its
  own. Startup hooks run again after a server restart or a live handoff, which
  is exactly when Herdr has dropped the Agent view.
- An **Agent quota settings** popup pane. It edits the percentage style, the
  sidebar layout, the row gap, the watcher interval, the brand colours, the
  visible fields, and the installed agents, and applies them by re-invoking
  `configure` with every value named explicitly, then reloading Herdr's
  configuration and forcing one refresh — the same path the "Install / repair"
  action takes, so there is still one writer for the sidebar rows and the
  statusLine entries. Herdr injects `HERDR_PLUGIN_STATE_DIR`,
  `HERDR_PLUGIN_CONFIG_DIR`, and `HERDR_BIN_PATH` into a pane, which is what
  makes this possible; it was verified with a throwaway `printenv` pane.
  Unchecking an agent uninstalls that agent's collector and restores its own
  statusLine, so it asks for a second keypress first, and the last agent
  cannot be unchecked.
- `--fields` chooses which quota fields the sidebar shows: `all` (default),
  `none`, or a comma-separated list of `topic`, `model`, `cache`, `ttl`,
  `context`, `5h`, `7d`. The provider name and the error token are not
  optional — a row that cannot say which subscription it belongs to, or that
  hides why quota is missing, is worse than no row. Hiding the model degrades
  the packed identity token from `$quota_provider_model` to `$quota_provider`
  rather than leaving the row nameless.
- `--brand-colors off` drops the per-agent hues on provider and model. Severity
  colours are unaffected: they are information, not decoration. With the hues
  off the plugin writes no `rows_by_agent` entries at all, rather than copies
  of the shared rows.
- Quota percentages can read as consumed instead of remaining. `--quota-percent
  used` (on `./install.sh` and `herdr-agent-quota configure --apply`, or
  `$HERDR_AGENT_QUOTA_PERCENT` for a direct CLI run) flips every 5h/7d/30d
  number in the sidebar and the dashboard; `remaining` stays the default. The
  sidebar token keeps its width — no `left`/`used` word rides along — and the
  severity colour is still computed from the remaining quota, so red keeps
  meaning "little runway". The choice is stored in the plugin state directory
  as well as the config directory, because the Claude/Agy statusLine hooks are
  launched by their harness with only `HERDR_PLUGIN_STATE_DIR` set.

### Changed

- The settings popup now opens 78x30 instead of Herdr's default half-size
  popup. The pane draws one row per option and the list is longer than 24
  rows, so the agents section used to open below the fold.

### Fixed

- Agy quota is no longer misread when a bucket reports `remaining_percent`
  rather than `remaining_fraction`. The scale now comes from the key name; the
  previous "below 1.0 means a fraction" heuristic rendered `remaining_percent:
  1.0` — a nearly exhausted pool — as 100% remaining and coloured it green.
- `./install.sh --agent`, `./uninstall.sh --agent`, and
  `--watch-interval-seconds` now reach `configure`. Herdr runs a plugin action
  with a fixed command line in the **server's** environment, so the variables
  these scripts exported were silently dropped: `./uninstall.sh --agent grok`
  removed every agent's configuration instead of Grok's. The selection now
  travels through the plugin config directory, as the sidebar layout and row
  gap already did, and `uninstall.sh` restores the previous value afterwards.
- A single-pane agent event no longer discards the other panes' cached context
  and model. The fetch only enriches the session the event named, and the save
  path dropped every session it had not looked at, so a sibling pane's context
  was cleared and then republished on the next refresh — one avoidable
  metadata write, and one avoidable repaint, per cycle.
- The dashboard pane now lists OpenCode Go, including the 30d window that the
  sidebar has no token for. Both READMEs promised this; nothing rendered it.
- `configure --apply` keeps the user's own key order in
  `~/.claude/settings.json` instead of re-sorting the whole file.

### Added

- Grok plans billed monthly report a 30d window instead of failing to parse.
  The sidebar's long-window slot carries the weekly allowance, or the monthly
  one when a plan has no weekly bucket; the period label travels inside the
  value (`30d 70% 17d8h`), and a weekly window always wins the slot when both
  exist, so a monthly number is never displayed as a weekly one.
- A lapsed prompt cache publishes `quota_cache_state` (`no cached`) instead of
  sharing `quota_error` with real failures. Both render amber, so the two were
  previously indistinguishable even though one is a normal state.
- CI runs `cargo audit`, on pull requests and weekly.

### Removed

- `Severity::Caution` and its `quota_*_caution` sidebar tokens. The variant was
  unreachable, so those rows could never be filled.
- The `omp` and `kimi` harness names. Neither had a collector, a sidebar row,
  or an integration; a detected pane produced no tokens at all, and a status
  event still paid for one pane read to extract a topic that was then dropped.
- `MetadataTokens` fields, and the metadata names behind them, that nothing
  published: `quota_state`, `quota_icon`, `quota_status`, `quota_summary`, and
  the per-window `_label` / `_percent` / `_eta` splits. They were compared on
  every refresh and competed for Herdr's 16-token report budget. Panes still
  carrying them are cleaned up on the next report.

## [1.1.0] - 2026-08-31

### Added

- Sidebar rows can be `packed` (default: join cache/TTL and 5h/7d on one row)
  or `stacked` (provider, model, cache, TTL, context, 5h, and 7d each on their
  own row). Plugin-owned `row_gap` defaults to `1` so adjacent panes are not
  packed flush; `--row-gap 0` packs them together. A user-owned `row_gap` is
  left alone. Herdr only accepts whole rows. `configure --sidebar-layout` /
  `--row-gap`, `./install.sh --sidebar-layout` / `--row-gap`, and the matching
  plugin config-dir prefs select them; the choice is stored so a later repair
  keeps it. Empty tokens still collapse in both layouts. Herdr itself does not
  wrap overflowing tokens.
- Codex now publishes an estimated prompt cache TTL. The rollout JSONL records
  no TTL and no expiry, so the countdown is the documented 30 minute
  `prompt_cache_options.ttl` anchored to the timestamp of the last recorded
  request. The same estimate covers Pi `openai-codex` sessions, anchored to the
  latest assistant message with cache activity. `cache_write_input_tokens` is
  not used as the anchor: ChatGPT-backed sessions report 0 there even while
  cached reads are large.

### Changed

- Sidebar color is two systems: brand answers who, status answers remaining
  quota. Provider uses the brand hue; model uses the dim sibling. Tab names
  are `#eceef2`, prompts `#c8cdd6`, cache/TTL/context `#969eae`. Compact
  `5h 0% 1h18m` / `7d 72% 5d22h` windows (spaces, no middle dots) take the
  remaining-percent color: green at 50%+, amber at 20–49%, red below 20%.
  `no cached` uses that same amber. Herdr joins sibling tokens with ` · `,
  so a window is one token. Selected state must not change provider hue. Selected-card
  fill is left to Herdr: `theme.custom.selection_bg` / `active_row_bg` exist
  from 0.8.2, 0.8.0 rejects them, and the intended fill is `#42474f`.
  Codex is cold white-blue, Claude coral, Agy Gemini blue, Grok silver,
  OpenCode the former Grok purple, Pi mauve.
- Claude cache TTL now comes from statusLine `prompt_cache.expires_at`
  (Claude Code v2.1.251+). Transcript-tail guesses from
  `ephemeral_5m`/`ephemeral_1h` buckets are gone. A cold or missing prefix
  hides the countdown instead of keeping a stale estimate. Pi/Anthropic still
  uses its recorded `cacheWrite1h` split.

### Fixed

- Grok panes no longer hide context/cache when Herdr binds the process's empty
  session-start id. `active_sessions.json` can list that stub and the real
  conversation under one PID; the stub has a model and no `signals.json`, so
  the same-PID sibling supplies the missing numbers. A different PID is never
  used, and a bound session that already has context is left alone.
- Switching Codex ChatGPT accounts no longer keeps the previous login's 5h
  row. The live `account/rateLimits/read` result is the account-level source;
  a local rollout may fill an omitted 5h window only when that file is at
  least as new as `auth.json` and its weekly window still matches. A cache
  from another account, or from before the current credential file, is not
  merged back. Weekly-only accounts now hide 5h immediately instead of after
  the next turn.

### Notes

- Recorded expiries still win where they exist: Claude Code
  `prompt_cache.expires_at` and Pi/Anthropic `cacheWrite1h`. Codex is the one
  place where a documented, model-independent TTL makes an estimate worth
  publishing. Grok, Agy, OpenCode, and other Pi backends have neither an entry
  expiry nor a documented TTL in their local contracts, so they keep cache hit
  rate and leave TTL blank rather than guessing a one-hour countdown.

## [1.0.0] - 2026-08-29

### Added

- `configure --agent` installs and removes one agent at a time. Values are
  `all` (the default), `claude`, `codex`, `grok`, `agy`, `opencode` and `pi`, and can
  be repeated or comma-separated. An agent you do not select gets no sidebar
  row, no statusLine entry and no hook file, so nothing of theirs is created on
  a machine that does not use them. `install.sh --agent` and
  `uninstall.sh --agent` pass the same selection through
  `HERDR_AGENT_QUOTA_AGENTS`, because Herdr plugin actions run a fixed command
  line. Removing one agent leaves the others installed and never touches the
  shared watcher, poll interval or config backup; `--uninstall` with no
  selection still removes everything.
- `configure` now reports when Herdr's own integration for a selected agent is
  missing. Without it Herdr reports no session id for that agent's panes, so
  quota cannot be attributed and the pane stays blank with no other clue. The
  integration belongs to Herdr (`herdr integration install <agent>`); this
  plugin never installs or changes it.
- OpenCode Go quota, fetched once per resolved pane from the official
  `https://opencode.ai/zen/go/v1/usage` endpoint using the key OpenCode already
  stores for its own `opencode-go` backend. The dashboard also renders a monthly
  window; the sidebar stays at 5h/7d because there is no monthly token.
  This provider is best-effort: the repository maintainer has no Go
  subscription, so the response shape comes from CodexBar's implementation and
  tests rather than an observed live response, cited in
  `docs/research/opencode-go-usage.md`. It fails closed on anything unexpected
  and cannot affect the other providers, which keep their exact previous
  behavior. Corrections from anyone with a subscription are welcome.
- OpenCode panes are resolved to a billing target from the exact Herdr session
  id, looked up read-only in `opencode.db` and classified against the
  credential filed for that backend in OpenCode's `auth.json`. Confirmed
  pay-as-you-go clears stale quota once; missing, unreadable or ambiguous
  evidence preserves whatever the pane already shows.
- Exact OpenCode sessions publish their local provider/model and context even
  when the backend has no supported subscription collector. Context follows
  OpenCode's own latest-completed-assistant token formula and bounded local
  model cache; no credential or unrelated session is consulted.

### Changed

- `event` and `focus` act on exactly one named pane from a single agent
  inventory, so a sibling pane on the same subscription receives neither a pane
  read nor a metadata write.
- Plugin-owned sidebar spacing now migrates to Herdr's packed `row_gap = 0`;
  empty metadata rows collapse dynamically, while user-owned spacing remains
  unchanged.
- Cache TTL is published only from a recorded provider bucket. Pi/Anthropic
  sessions now use their `cacheWrite1h` split and request-start timestamp;
  Codex keeps its cache hit rate but no longer guesses a one-hour expiry from a
  rollout event timestamp.
- Missing-integration diagnostics now cover Pi as well as OpenCode and tell the
  user to restart the affected pane after installation.
- The English and Chinese READMEs now lead with installation prerequisites,
  exact route coverage, accuracy gaps, privacy, and concise troubleshooting.

## [0.2.0] - 2026-08-29

### Fixed

- Rebuilding the plugin no longer leaves a stale active-turn watcher that
  keeps publishing the old weekly token. That leftover `$quota_week_normal`
  stacked on `$quota_week_inline_*` and made Grok show `7d` twice.
- Claude/Agy quota windows are remembered per statusLine session so a work
  and a personal login no longer overwrite each other's 5h/7d rows. Grok and
  Codex still publish the account-level windows to every pane of that login;
  a Claude session that has not reported yet shows unavailable instead of
  borrowing another account's numbers. Based on work by @joshfinnie in #30.

### Changed

- Sidebar cards now decide from the 5h token, not the provider name, whether
  weekly quota sits beside context. A present 5h window keeps `5h` and `7d`
  on the limits row so they never share a line with context; an empty 5h
  publishes week on the context row instead, so weekly-only cards still read
  `context · 7d`. Codex can therefore split when OpenAI returns 5h and fold
  after a reset; Grok, Claude, and Agy keep their previous visual shape.
- Agy/Antigravity quota now shows the pool that the active model actually
  draws from instead of the conservative minimum across both pools. Gemini-family
  model names (`gemini`, `flash`, `learnlm`) select the `gemini-*` pool;
  Claude, Sonnet, Haiku, Opus, GPT, and OpenAI o-series models select the
  `3p-*` pool. Unrecognised model names fall back to the previous behaviour
  (minimum across both pools) so the sidebar remains correct after a provider
  adds a new model name without a plugin update.

### Added

- Provider rows now use one compact `$quota_provider_model` identity token
  (`Provider/Model`, with the model omitted when unavailable); provider and
  model share the provider's brand color. Context is the penultimate row,
  while cache diagnostics use a dedicated row: a one-decimal cumulative
  session hit rate and the elapsed time
  since the latest cache-bearing response plus an explicitly approximate TTL
  estimate when the provider exposes a cache bucket or Codex supplies a
  timestamped cache-bearing rollout event. Claude/Agy collectors use local
  statusLine/transcript data only; they do not log in or start model requests.
- Codex now reads the bounded tail of each matching local rollout to publish
  per-session model, current context, and cumulative cache diagnostics. Grok
  supplements its billing snapshot with bounded local `signals.json` and
  `updates.jsonl` session metadata, so both providers expose context/cache when
  the local session files contain those fields; missing data remains hidden.
- Grok session matching no longer requires `signals.json`. Newer CLI sessions
  may only have `summary.json` until the first usage signal; the model still
  comes from `current_model_id`. Context and cache stay hidden until
  `signals.json` / usage updates exist.
- Grok's weekly-only limit is placed beside context in its provider-specific
  row, and Claude/Agy statusLine diagnostics are keyed by session so a fresh
  session cannot inherit another session's cache. All per-session maps are
  bounded to 128 entries.
- A pane without a Herdr session id no longer receives provider-global
  context/cache diagnostics. This prevents a fresh Grok or Agy pane from
  showing another session's cached usage; diagnostics appear once the current
  session can be matched. Weekly labels use the compact `7d` form everywhere.
- Quota rows now show compact `5h`/`7d` window labels with minutes below one
  hour, hours and minutes below one day, and days plus hours for longer windows.
- Cache hit rate and remaining cache TTL now share one short, color-separated
  sidebar row; the verbose last-activity text is no longer shown.
- Cache, TTL, and context diagnostics now share one muted blue-gray style;
  provider/model keep their brand color, while green/amber/red are reserved
  for quota runway health and explicit errors.
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

- Codex and Claude no longer drop a still-current five-hour window when the
  latest payload omits it. The previous 5h value is restored only when its
  reset is still in the future and a sibling window present in both snapshots
  has not itself reset; an empty window list still clears stale quota.
- Codex also reads five-hour limits from `rateLimitsByLimitId`, from
  near-duration token-count headers, and from the latest matching local
  rollout when the app-server omits `secondary`. A newer weekly-only event
  does not restore a stale 5h value. When 5h is genuinely absent, Codex
  elides `$quota_5h` like Grok so the card reads `context · 7d`.
- Codex quota parsing now keeps both provider-reported five-hour and seven-day
  windows. It identifies each window by duration instead of assuming that
  `primary` or `secondary` has a fixed meaning, so the restored five-hour limit
  appears in the sidebar again.
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
- Weekly windows now use the compact `7d ... reset ...` label, including
  weekly-only providers, so narrow sidebars do not truncate the label.
- Grok local-session enrichment now checks only the pane-matched two-level
  session paths, with a bounded newest-session fallback for direct refreshes;
  it no longer recursively scans the entire historical session tree.

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
  sidebar, dashboard, and statusLine fallbacks. Codex now publishes both its
  five-hour and weekly windows when the provider reports them.
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

[Unreleased]: https://github.com/levi-qiao/herdr-agent-quota/compare/v1.3.0...HEAD
[1.3.0]: https://github.com/levi-qiao/herdr-agent-quota/compare/v1.2.0...v1.3.0
[1.2.0]: https://github.com/levi-qiao/herdr-agent-quota/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/levi-qiao/herdr-agent-quota/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/levi-qiao/herdr-agent-quota/compare/v0.2.0...v1.0.0
[0.2.0]: https://github.com/levi-qiao/herdr-agent-quota/releases/tag/v0.2.0
[0.1.0]: https://github.com/levi-qiao/herdr-agent-quota/releases/tag/v0.1.0
