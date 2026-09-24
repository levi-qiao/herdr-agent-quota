use crate::cache::CacheStore;
use crate::cli::{AgentSelection, LowQuotaAlert};
use crate::herdr::{
    current_focused_pane, find_agent_icon_panes, find_agent_pane, focused_pane_in_snapshot,
    list_agent_panes, list_agent_state, plugin_quota_present, publish_icon_tokens,
    publish_pane_tokens, publish_pane_tokens_with_scrolled_icons, publish_status_icons,
    refresh_pane_topic, AgentPane, AgentStatus, PaneQuotaUpdate, PaneTokens,
};
use crate::model::{
    BillingTarget, CredentialScope, Harness, Provider, ProviderSnapshot, Resolution,
};
use crate::omp::OmpEvidence;
use crate::opencode::OpenCodePaths;
use crate::presentation::{MetadataTokens, RowStyle, SidebarShape};
use crate::providers::statusline::enrich_cache_session;
use crate::providers::{codex, cursor, devin, grok, muse, omp as omp_provider, opencode_go};
use crate::route;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const MAX_ACTIVE_TURN_WATCH: Duration = Duration::from_secs(60 * 60);
const TURN_WATCH_LOCK: &str = "turn.lock";
const WATCH_HERDR_ENV: &str = "watch-herdr.json";

/// A detached watcher outlives the server that supplied its environment.
/// Server-owned entry points record only connection fields, never credentials.
#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct WatchHerdrEnvironment {
    binary: Option<std::path::PathBuf>,
    socket: Option<std::path::PathBuf>,
}

impl WatchHerdrEnvironment {
    fn current() -> Self {
        Self {
            binary: std::env::var_os("HERDR_BIN_PATH").map(Into::into),
            socket: std::env::var_os("HERDR_SOCKET_PATH").map(Into::into),
        }
    }

    fn save(&self, cache: &CacheStore) -> Result<()> {
        // A direct invocation without Herdr's environment cannot describe
        // the server and must not replace its recorded connection.
        if self.binary.is_none() || self.socket.is_none() {
            return Ok(());
        }
        if Self::load(cache).as_ref() == Some(self) {
            return Ok(());
        }
        cache.ensure()?;
        let temporary = cache
            .root()
            .join(format!(".{WATCH_HERDR_ENV}.{}.tmp", std::process::id()));
        std::fs::write(&temporary, serde_json::to_vec(self)?)?;
        std::fs::rename(temporary, cache.root().join(WATCH_HERDR_ENV))?;
        Ok(())
    }

    fn load(cache: &CacheStore) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(cache.root().join(WATCH_HERDR_ENV)).ok()?).ok()
    }
}

#[derive(Debug, Serialize)]
pub struct ProviderOutcome {
    pub provider: Provider,
    pub available: bool,
    pub from_cache: bool,
    pub error: Option<String>,
}

pub fn run(providers: &[Provider], force: bool, json: bool) -> Result<()> {
    run_internal(providers, force, json, None)
}

/// Restore the Herdr state this plugin owns, then refresh once.
///
/// Forced refresh restores the Agent view: Herdr drops a plugin-owned view
/// on disable, and enable does not run startup. Event/focus/watch must not
/// spend a socket call on every turn. A `default` order owns no view, so
/// nothing is put back.
pub fn startup(providers: &[Provider]) -> Result<()> {
    // Handoff need not emit another idle -> working event. An existing
    // watcher adopts the saved environment; otherwise this starts one.
    run(providers, true, false)?;
    spawn_watch(false)
}

/// Refresh selected providers until their agents leave the working state.
///
/// This command is normally launched detached by `event` and is deliberately
/// quota-only: it never reads a pane. Claude/Agy statusLine hooks publish
/// observations to the local mailbox, while Codex/Grok use their normal
/// providers' fetchers. One global watcher reads Herdr's agent inventory once
/// per poll and refreshes every selected provider that is working. Each
/// provider has its own non-blocking refresh lease, so slow I/O never stalls a
/// statusLine hook or another provider. The existing provider-level debounce
/// remains the lower bound for network requests, except when a cached window
/// has already expired — that reading is no longer live, so the next pass
/// fetches it even if the pane itself is idle.
pub fn watch(providers: &[Provider], interval_seconds: Option<u64>, defer: bool) -> Result<()> {
    let cache = CacheStore::from_env()?;
    let interval_seconds = interval_seconds
        .map(CacheStore::validate_watch_interval_seconds)
        .transpose()?
        .unwrap_or_else(|| cache.watch_interval_seconds());
    if !cache.root().is_dir() {
        return Ok(());
    }
    let Some(_lock) = cache.try_lock_named(TURN_WATCH_LOCK)? else {
        return Ok(());
    };

    let started = Instant::now();
    let started_at = SystemTime::now();
    let started_millis = CacheStore::now_millis();
    let interval = Duration::from_secs(interval_seconds);
    if defer {
        wait_for_watch_tick(&cache, interval, started_at, started_millis);
    }
    let mut previous_active = event_json()
        .as_ref()
        .and_then(find_pane_id)
        .map(str::to_string)
        .into_iter()
        .collect::<Vec<_>>();
    let mut settling = BTreeMap::new();
    loop {
        if !cache.root().is_dir() || cache.turn_watchers_stopped_after(started_millis)? {
            break;
        }
        let server = WatchHerdrEnvironment::load(&cache);
        let server_changed = server
            .as_ref()
            .is_some_and(|server| *server != WatchHerdrEnvironment::current());
        if server_changed || watch_binary_is_newer(started_at, current_exe_modified()) {
            drop(_lock);
            return reexec_watch(server.as_ref(), interval_seconds);
        }
        // A transient Herdr failure should not terminate a live watcher; the
        // one-hour cap below still prevents an orphaned process. The next
        // poll retries the single inventory call.
        let Ok(mut state) = list_agent_state() else {
            if started.elapsed() >= MAX_ACTIVE_TURN_WATCH {
                break;
            }
            wait_for_watch_tick(&cache, interval, started_at, started_millis);
            continue;
        };
        let enabled = AgentSelection::from_args_or_env(&[]);
        state.panes.retain(|pane| enabled.contains(&pane.harness));
        let active = state
            .working_pane_ids
            .iter()
            .filter(|id| {
                state
                    .panes
                    .iter()
                    .any(|pane| pane.pane_id == **id && pane_in_watch_scope(pane, providers))
            })
            .cloned()
            .collect::<Vec<_>>();
        let now = CacheStore::now_unix();
        let affected = watch_pass_ids(
            &cache,
            &state.panes,
            providers,
            &active,
            &previous_active,
            &mut settling,
            now,
        );
        // Fold plugin unseen-state into pane.status before any publish so a
        // same-tab idle inventory cannot paint white over a completion the
        // user has not focused yet.
        apply_icon_attention(&mut state.panes, &cache, None, true, false)?;
        let icon_dirty = state
            .panes
            .iter()
            .filter(|pane| pane.icon_needs_update())
            .map(|pane| pane.pane_id.clone())
            .collect::<Vec<_>>();
        let _ = refresh_working_panes(&cache, &state.panes, &affected, &icon_dirty);
        let stale_icons = state.panes.iter().any(has_stale_done_icon);
        if active.is_empty()
            && settling.is_empty()
            && !stale_icons
            && cache.icon_attention().unseen.is_empty()
        {
            break;
        }
        previous_active = active;
        if started.elapsed() >= MAX_ACTIVE_TURN_WATCH && cache.icon_attention().unseen.is_empty() {
            break;
        }
        wait_for_watch_tick(&cache, interval, started_at, started_millis);
    }
    Ok(())
}

/// Include a settled pane in one pass after debounce expires, even while a
/// different provider keeps working. Returning the final ids before retiring
/// them is what prevents the completion reading from being dropped.
fn watch_targets(
    active: &[String],
    previous: &[String],
    settling: &mut BTreeMap<String, u64>,
    now: u64,
) -> Vec<String> {
    for id in previous {
        if !active.contains(id) {
            settling.entry(id.clone()).or_insert(now);
        }
    }
    settling.retain(|id, _| !active.contains(id));
    let mut affected = active.to_vec();
    affected.extend(settling.keys().cloned());
    settling.retain(|_, finished| now.saturating_sub(*finished) < 60);
    affected
}

fn pane_in_watch_scope(pane: &AgentPane, providers: &[Provider]) -> bool {
    covers_every_collector(providers)
        || pane
            .harness
            .billing()
            .is_some_and(|provider| providers.contains(&provider))
}

/// Working and settling panes, plus idle panes the cache has left behind.
///
/// An idle pane is only ever republished by a pass it is named in, so a pane
/// that never starts a turn shows whatever it last published. Including those
/// pane ids in an already-running watch pass rewrites the sidebar without
/// waiting for that pane to start a turn, and without starting a second
/// watcher while everything is idle.
fn watch_pass_ids(
    cache: &CacheStore,
    panes: &[AgentPane],
    providers: &[Provider],
    active: &[String],
    previous: &[String],
    settling: &mut BTreeMap<String, u64>,
    now: u64,
) -> Vec<String> {
    let mut affected = watch_targets(active, previous, settling, now);
    // Reading the row style costs the config file, so a pass with no idle
    // candidate at all must not pay for it.
    let mut style = None;
    for pane in panes {
        if !pane_in_watch_scope(pane, providers) || affected.contains(&pane.pane_id) {
            continue;
        }
        let row = *style.get_or_insert_with(|| publish_row(cache));
        if cached_quota_is_stale(cache, pane, now, row) {
            affected.push(pane.pane_id.clone());
        }
    }
    affected
}

/// True when what this pane is showing is no longer what the cache says.
///
/// Two separate readings are stale, and neither implies the other:
///
/// - the window the pane shows has reset, so the number on it is not a live
///   reading whatever the cache holds; and
/// - the cache has moved on while nothing woke this pane to republish it.
///   Claude's statusLine hook only writes the observation mailbox, so an idle
///   pane whose session gained a fresh window has no other way back in.
///
/// A pane whose expired window was already omitted is the case the second
/// reading misses: the rendered rows agree with an empty cache, and only the
/// first pulls it in for the fetch that replaces them.
///
/// This decides membership only. It loads the one snapshot the pass would read
/// anyway and publishes nothing.
fn cached_quota_is_stale(cache: &CacheStore, pane: &AgentPane, now: u64, row: RowStyle) -> bool {
    let Resolution::Subscription(target) = route::resolve(pane) else {
        return false;
    };
    let Some(snapshot) = cache.load_target(&target).ok().flatten() else {
        return false;
    };
    let session_id = pane.session.as_ref().and_then(|session| session.id());
    if snapshot.displayed_quota_has_expired(session_id, now) {
        return true;
    }
    let values = MetadataTokens::from_snapshot_for_pane_with_row(&snapshot, now, session_id, row);
    crate::herdr::quota_rows_have_drifted(&pane.tokens, &values, row.shape)
}

fn wait_for_watch_tick(
    cache: &CacheStore,
    interval: Duration,
    started_at: SystemTime,
    started_millis: u64,
) {
    let deadline = Instant::now() + interval;
    while Instant::now() < deadline {
        if !cache.root().is_dir()
            || cache
                .turn_watchers_stopped_after(started_millis)
                .unwrap_or(false)
            || watch_binary_is_newer(started_at, current_exe_modified())
            || WatchHerdrEnvironment::load(cache)
                .is_some_and(|server| server != WatchHerdrEnvironment::current())
        {
            break;
        }
        poll_focus_once(cache);
        thread::sleep(
            Duration::from_secs(1).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

/// Herdr 0.9 can change TUI focus without delivering a focus hook. Sample the
/// metadata-only snapshot between quota polls, and repaint only on a change.
fn poll_focus_once(cache: &CacheStore) {
    let attention = cache.icon_attention();
    if attention.working.is_empty() && attention.unseen.is_empty() {
        return;
    }
    let Ok(Some(pane_id)) = focused_pane_in_snapshot(None, None) else {
        return;
    };
    if attention.last_focused.as_deref() != Some(pane_id.as_str()) {
        let _ = paint_focus_icons(cache, Some(&pane_id));
    }
}

/// Refresh working, settling, or expired-quota targets, then publish that
/// account reading to its siblings. Local transcript routing does not read
/// terminal output or poll unrelated subscriptions.
fn refresh_working_panes(
    cache: &CacheStore,
    panes: &[AgentPane],
    affected: &[String],
    icon_dirty: &[String],
) -> Result<()> {
    let routes = panes.iter().map(route::resolve).collect::<Vec<_>>();
    let mut targets = Vec::new();
    for (pane, resolution) in panes.iter().zip(&routes) {
        if affected.contains(&pane.pane_id) {
            if let Resolution::Subscription(target) = resolution {
                if !targets.contains(target) {
                    targets.push(*target);
                }
            }
        }
    }
    let mut selected = panes.iter().zip(routes).filter(|(pane, resolution)| {
        affected.contains(&pane.pane_id)
            || icon_dirty.contains(&pane.pane_id)
            || matches!(resolution, Resolution::Subscription(target) if targets.contains(target))
    }).map(|(pane, _)| pane.clone()).collect::<Vec<_>>();
    let mut providers = Vec::new();
    for provider in targets
        .iter()
        .filter_map(|target| target.original_provider())
    {
        if !providers.contains(&provider) {
            providers.push(provider);
        }
    }
    refresh_selected(cache, &providers, false, &selected)?;
    publish_resolved(cache, &mut selected, None, false, true)
}

fn run_internal(
    providers: &[Provider],
    force: bool,
    json: bool,
    topic_pane: Option<&str>,
) -> Result<()> {
    let cache = CacheStore::from_env()?;
    WatchHerdrEnvironment::current().save(&cache)?;
    if force {
        restore_quota_agent_view(&cache);
    }
    // Agent inventory is metadata-only. Reusing it for both the fetch and the
    // publish pass lets local Codex/Grok diagnostics target the exact pane
    // sessions without adding another Herdr call or reading any pane output.
    let enabled = AgentSelection::from_args_or_env(&[]);
    let panes = list_agent_panes().ok().map(|panes| {
        panes
            .into_iter()
            .filter(|pane| enabled.contains(&pane.harness))
            .collect::<Vec<_>>()
    });
    let session_panes = panes.as_deref().unwrap_or_default();
    let enabled_providers = providers.iter().copied().filter(|provider| {
        enabled.iter().any(|harness| harness.billing() == Some(*provider))
            || session_panes.iter().any(|pane| matches!(route::resolve(pane), Resolution::Subscription(target) if target.original_provider() == Some(*provider)))
    }).collect::<Vec<_>>();
    let outcomes = refresh_selected(&cache, &enabled_providers, force, session_panes)?;
    // The all-provider pass (startup and the manual refresh action) is the only
    // one that speaks for every pane, including harnesses with no legacy 1:1
    // collector. A narrower `--provider` selection publishes only its own panes.
    let mut publish_panes = if covers_every_collector(providers) {
        session_panes.to_vec()
    } else {
        panes_for_providers(session_panes, providers)
    };
    publish_resolved(&cache, &mut publish_panes, topic_pane, force, false)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&outcomes)?);
    }
    Ok(())
}

/// Herdr drops this plugin's Agent view (`plugin:<id>`) on disable. Enable
/// does not run startup, so a forced refresh is the repair that also
/// respawns the watcher. Event/focus/watch ticks stay off this path.
fn restore_quota_agent_view(cache: &CacheStore) {
    let order = crate::configure::resolved_agent_order(None, Some(cache));
    if order.is_quota() {
        crate::configure::apply_agent_order(order);
    }
}

pub fn event() -> Result<()> {
    let event = event_json();
    let Some(event) = event.as_ref() else {
        return Ok(());
    };
    let Some(agent) = find_agent(event) else {
        return Ok(());
    };
    let Some(harness) = Harness::from_agent_name(agent) else {
        return Ok(());
    };
    if !AgentSelection::from_args_or_env(&[]).contains(&harness) {
        return Ok(());
    }
    let Some(pane_id) = find_pane_id(event) else {
        return Ok(());
    };

    let cache = CacheStore::from_env()?;
    let Some(mut pane) = named_pane(pane_id, harness)? else {
        return Ok(());
    };

    let status = find_status(event);
    // The event names the transition that just fired. `agent list` can lag a
    // beat behind (still `working` after completion), and trusting the list
    // would republish the yellow icon and miss `done` entirely when no second
    // event follows.
    if let Some(status) = status {
        pane.status = AgentStatus::parse(status);
    }
    // Brand icon colour mirrors the TUI state_icon: working → yellow, unseen
    // completion → teal, later focus event → white. Never call `herdr pane current`
    // here: status hooks set HERDR_PANE_ID to the *event* pane, so pane
    // current returns the finisher and would skip teal for every completion.
    // `handle_named_pane` folds the working set into status before publish.
    // Pi's and omp's exact session files carry the routing evidence, and
    // Muse/Cursor transcripts record the prompt itself. Reading their panes
    // would add a visible repaint without improving attribution or the topic.
    let topic_pane = (!matches!(
        harness,
        Harness::Pi | Harness::Omp | Harness::Muse | Harness::Cursor
    ))
    .then_some(pane_id);
    let result = handle_named_pane(
        &cache,
        pane,
        topic_pane,
        status.is_some_and(is_working_status),
    );
    if status.is_some_and(is_working_status) {
        if let Err(error) = spawn_watch(true) {
            if result.is_ok() {
                return Err(error);
            }
        }
    }
    result
}

pub fn focus() -> Result<()> {
    // Focus is icon-only. Pane events name their target directly; workspace
    // and tab events need the focused pane from that location's layout.
    // Only a direct CLI invocation falls back to `pane current`.
    let event = event_json();
    let force_idle = match event.as_ref() {
        Some(event) => {
            if let Some(pane_id) = find_pane_id(event) {
                Some(pane_id.to_owned())
            } else {
                let workspace_id = find_field(event, &["workspace_id", "workspaceId"]);
                let tab_id = find_field(event, &["tab_id", "tabId"]);
                if workspace_id.is_some() || tab_id.is_some() {
                    focused_pane_in_snapshot(workspace_id, tab_id)?
                } else {
                    None
                }
            }
        }
        None => current_focused_pane().ok().flatten().map(|(id, _)| id),
    };
    let cache = CacheStore::from_env()?;
    paint_focus_icons(&cache, force_idle.as_deref())
}

/// Fold plugin working/unseen state into each pane's status so icon colour
/// matches the TUI ring, not server `agent_status`.
///
/// Herdr's CLI list marks same-tab completions `idle` (server seen). The TUI
/// ring stays teal until that pane's surface is acknowledged. We persist the
/// same unseen set across event/watch/focus so a watch pass cannot paint
/// white over a completion the user has not focused.
fn apply_icon_attention(
    panes: &mut [AgentPane],
    cache: &CacheStore,
    force_seen: Option<&str>,
    prune_to_live: bool,
    working_event: bool,
) -> Result<()> {
    let _lock = cache.lock_icon_attention()?;
    let hydrate = !cache.icon_attention_exists();
    let mut attention = cache.icon_attention();
    let previous_focused = attention.last_focused.clone();
    if prune_to_live {
        let live: BTreeSet<String> = panes.iter().map(|pane| pane.pane_id.clone()).collect();
        attention.working.retain(|id| live.contains(id));
        attention.unseen.retain(|id| live.contains(id));
        attention.seen.retain(|id| live.contains(id));
    }
    if hydrate {
        for pane in panes.iter() {
            if !pane.focused && tokens_show_done_icon(&pane.tokens) {
                attention.unseen.insert(pane.pane_id.clone());
            }
        }
    }
    if force_seen.is_none() && attention.last_focused.is_none() {
        attention.last_focused = panes
            .iter()
            .find(|pane| pane.focused)
            .map(|pane| pane.pane_id.clone());
    }
    for pane in panes.iter_mut() {
        if working_event && pane.focused {
            attention.last_focused = Some(pane.pane_id.clone());
        }
        let acknowledge = force_seen.is_some_and(|target| {
            pane.pane_id == target || previous_focused.as_deref() == Some(pane.pane_id.as_str())
        });
        if acknowledge {
            let green_on_screen =
                attention.unseen.contains(&pane.pane_id) || tokens_show_done_icon(&pane.tokens);
            if force_seen == Some(pane.pane_id.as_str()) {
                pane.focused = true;
            }
            // A focus hook acknowledges the colour the user actually saw.
            // Herdr's inventory can still report `working` after the pane
            // completed; letting that stale status win would recreate an
            // unseen completion on the next idle watch pass.
            if green_on_screen {
                attention.unseen.remove(&pane.pane_id);
                attention.seen.insert(pane.pane_id.clone());
                attention.working.remove(&pane.pane_id);
                pane.status = AgentStatus::Idle;
                continue;
            }
            if !pane.status.is_working() {
                attention.unseen.remove(&pane.pane_id);
                attention.seen.insert(pane.pane_id.clone());
            }
        }
        if pane.status.is_working() {
            // Inventory may continue saying `working` after a completed pane
            // was acknowledged. Only a fresh working status event starts a
            // new turn; polling the stale inventory must keep the icon white.
            if attention.seen.contains(&pane.pane_id) && !working_event {
                pane.status = AgentStatus::Idle;
                continue;
            }
            attention.working.insert(pane.pane_id.clone());
            attention.unseen.remove(&pane.pane_id);
            attention.seen.remove(&pane.pane_id);
            continue;
        }
        if !matches!(pane.status, AgentStatus::Idle | AgentStatus::Done) {
            continue;
        }
        let was_working = attention.working.remove(&pane.pane_id)
            || (!attention.seen.contains(&pane.pane_id) && tokens_show_working_icon(&pane.tokens));
        if was_working {
            attention.seen.remove(&pane.pane_id);
            attention.unseen.insert(pane.pane_id.clone());
        } else if pane.status == AgentStatus::Done && !attention.seen.contains(&pane.pane_id) {
            attention.unseen.insert(pane.pane_id.clone());
        }
        if acknowledge {
            attention.unseen.remove(&pane.pane_id);
            attention.seen.insert(pane.pane_id.clone());
        }
        pane.status = if attention.unseen.contains(&pane.pane_id) {
            AgentStatus::Done
        } else {
            AgentStatus::Idle
        };
    }
    if let Some(pane_id) = force_seen {
        attention.last_focused = Some(pane_id.to_owned());
    }
    cache.set_icon_attention(&attention)
}

fn paint_focus_icons(cache: &CacheStore, force_idle_id: Option<&str>) -> Result<()> {
    let Some(pane_id) = force_idle_id else {
        return Ok(());
    };
    let enabled = AgentSelection::from_args_or_env(&[]);
    let previous = cache.icon_attention().last_focused;
    let mut ids = vec![pane_id];
    if let Some(previous) = previous.as_deref() {
        ids.push(previous);
    }
    let mut panes = find_agent_icon_panes(&ids)?;
    panes.retain(|pane| enabled.contains(&pane.harness));
    apply_icon_attention(&mut panes, cache, Some(pane_id), false, false)?;
    publish_icon_tokens(&panes, CacheStore::now_millis())
}

/// Teal left on a focused pane — needs a clear pass.
fn has_stale_done_icon(pane: &AgentPane) -> bool {
    pane.focused && tokens_show_done_icon(&pane.tokens)
}

fn tokens_show_done_icon(tokens: &BTreeMap<String, String>) -> bool {
    tokens.contains_key("quota_icon_done")
        || tokens
            .get("quota_icon")
            .is_some_and(|value| value.contains(crate::icons::DONE_TAG))
}

fn tokens_show_working_icon(tokens: &BTreeMap<String, String>) -> bool {
    tokens.contains_key("quota_icon_working")
        || tokens
            .get("quota_icon")
            .is_some_and(|value| value.contains(crate::icons::WORKING_TAG))
}

/// The single pane an entry point is allowed to act on.
///
/// It must still be in the one inventory read and still be running the harness
/// that named it; a stale or mismatched pane id yields nothing, so the caller
/// fetches nothing and writes no metadata to any sibling pane.
fn named_pane(pane_id: &str, harness: Harness) -> Result<Option<AgentPane>> {
    Ok(find_agent_pane(pane_id)?.filter(|pane| pane.harness == harness))
}

fn handle_named_pane(
    cache: &CacheStore,
    pane: AgentPane,
    topic_pane: Option<&str>,
    working_event: bool,
) -> Result<()> {
    if !AgentSelection::from_args_or_env(&[]).contains(&pane.harness) {
        return Ok(());
    }
    WatchHerdrEnvironment::current().save(cache)?;
    let mut panes = vec![pane];
    apply_icon_attention(&mut panes, cache, None, false, working_event)?;
    if topic_pane == Some(panes[0].pane_id.as_str()) {
        refresh_pane_topic(&mut panes[0]);
    }
    let resolved = route::resolve_with_identity(&panes[0]);
    // A cross-harness route may reuse an original collector only after its
    // credential scope is proved. Pi's account-id match is the first such
    // route; its path-shaped session is deliberately not passed to Codex as a
    // thread id.
    if let Resolution::Subscription(target) = &resolved.resolution {
        if let Some(provider) = target.original_provider() {
            refresh_selected(cache, &[provider], false, &panes)?;
        }
    }
    let row = publish_row(cache);
    let tokens = resolved_pane_tokens(
        cache,
        &mut panes[0],
        resolved,
        CacheStore::now_unix(),
        row,
        false,
    )?
    .unwrap_or(PaneTokens {
        pane_id: panes[0].pane_id.clone(),
        quota: PaneQuotaUpdate::Preserve,
        identity: None,
        context: None,
        show_account_quota: true,
    });
    let mut tokens = vec![tokens];
    // Event and focus see one pane, not the whole inventory, which is exactly
    // what the alert needs: the entry is keyed by provider, and a provider
    // with no pane in the pass keeps whatever state it had. Warning here is
    // what makes the alert land at the end of the turn that spent the quota
    // rather than at the next poll.
    notify_low_quota(cache, &tokens);
    sync_vendor_row_siblings(&mut tokens, &mut panes);
    // Completion colour must land even if this pane is scrolled: the scroll
    // guard exists to protect reading transcript, not to leave a stale glyph.
    publish_status_icons(&panes, &tokens, CacheStore::now_millis(), row)
}

/// The layout the user chose and the meter size their sidebar affords,
/// resolved once per refresh: the layout from the state-dir cache the publish
/// hooks can see, the width from Herdr's own config.
fn sidebar_shape(cache: &CacheStore) -> SidebarShape {
    SidebarShape::new(
        cache.sidebar_layout().unwrap_or_default(),
        crate::configure::herdr::sidebar_width(),
    )
}

/// The row style every pane in one pass is rendered with.
fn publish_row(cache: &CacheStore) -> RowStyle {
    RowStyle {
        fields: cache.fields().unwrap_or_default(),
        pacing: cache.sidebar_pacing().unwrap_or_default(),
        ..RowStyle::new(
            cache.percent_style().unwrap_or_default(),
            sidebar_shape(cache),
        )
    }
}

/// Muse last prompt and Cursor/Grok generated session titles are the same
/// evidence other harnesses read off the screen, so they are also that pane's
/// topic. Codex keeps a screen topic and stores the thread preview separately.
fn apply_session_summary(pane: &mut AgentPane, summary: &str) {
    pane.session_summary = summary.to_string();
    if matches!(
        pane.harness,
        Harness::Muse | Harness::Cursor | Harness::Grok
    ) {
        pane.topic = summary.to_string();
    }
}

fn resolved_pane_tokens(
    cache: &CacheStore,
    pane: &mut AgentPane,
    resolved: route::ResolvedPane,
    now: u64,
    row: RowStyle,
    force: bool,
) -> Result<Option<PaneTokens>> {
    let route::ResolvedPane {
        resolution,
        identity,
        context,
        omp,
    } = resolved;
    let mut quota = match resolution {
        Resolution::Subscription(target)
            if target.credential_scope == CredentialScope::OMP_STORE =>
        {
            omp_quota(cache, &target, omp.as_ref(), now, row, force)
        }
        Resolution::Subscription(target) => {
            if let Some(provider) = target.original_provider() {
                let snapshot = cache.load(provider)?;
                let (account_id, mtime) = current_account_gate(provider);
                let usable = snapshot
                    .as_ref()
                    .filter(|snapshot| snapshot.usable_for_account(account_id.as_deref(), mtime));
                if let Some(snapshot) = usable {
                    if let Some(session_id) = pane.session.as_ref().and_then(|session| session.id())
                    {
                        if let Some(summary) = snapshot.session_summaries.get(session_id) {
                            apply_session_summary(pane, summary);
                        }
                    }
                }
                tokens_for_loaded_snapshot(
                    provider,
                    snapshot.as_ref(),
                    usable,
                    now,
                    pane.session.as_ref().and_then(|session| session.id()),
                    row,
                )
                .map(|values| PaneQuotaUpdate::Replace(Box::new(values)))
            } else {
                // Not one of the original four, so it is never fetched by the
                // provider list: this pane resolved to it, so this pane pays
                // for at most one debounced request.
                refresh_scoped_target(cache, &target, force);
                let snapshot = cache.load(target.billing)?;
                let usable = load_usable_snapshot(cache, target.billing)?;
                tokens_for_loaded_snapshot(
                    target.billing,
                    snapshot.as_ref(),
                    usable.as_ref(),
                    now,
                    pane.session.as_ref().and_then(|session| session.id()),
                    row,
                )
                .map(|values| PaneQuotaUpdate::Replace(Box::new(values)))
            }
        }
        Resolution::NoSubscription if plugin_quota_present(&pane.tokens) || identity.is_some() => {
            Some(PaneQuotaUpdate::Clear)
        }
        Resolution::NoSubscription => None,
        Resolution::Indeterminate if plugin_quota_present(&pane.tokens) || identity.is_some() => {
            Some(PaneQuotaUpdate::Clear)
        }
        Resolution::Indeterminate => None,
    };
    if quota.is_none() && (identity.is_some() || context.is_some()) {
        quota = Some(PaneQuotaUpdate::Preserve);
    }
    Ok(quota.map(|quota| PaneTokens {
        pane_id: pane.pane_id.clone(),
        quota,
        identity,
        context,
        show_account_quota: true,
    }))
}

/// Quota for an omp pane, from omp's own usage layer.
///
/// One `omp usage --json` per debounce window, for the one provider the pane
/// is actually talking to — never a fan-out over omp's whole credential pool.
/// Without an account to attribute the numbers to, the pane shows unavailable
/// quota rather than retaining numbers from an unconfirmed account.
fn omp_quota(
    cache: &CacheStore,
    target: &BillingTarget,
    evidence: Option<&OmpEvidence>,
    now: u64,
    row: RowStyle,
    force: bool,
) -> Option<PaneQuotaUpdate> {
    let evidence = evidence?;
    omp_quota_with_refresh(cache, target, evidence, now, row, force, refresh_omp_target)
}

fn omp_quota_with_refresh(
    cache: &CacheStore,
    target: &BillingTarget,
    evidence: &OmpEvidence,
    now: u64,
    row: RowStyle,
    force: bool,
    refresh: impl FnOnce(&CacheStore, &BillingTarget, &OmpEvidence, u64) -> OmpUsage,
) -> Option<PaneQuotaUpdate> {
    let pin = evidence.account_pin.as_deref();
    let report = cache.load_omp_usage(target);
    let legacy = cache.load_target(target).ok().flatten();
    let cached = report
        .as_ref()
        .and_then(|usage| omp_provider::select_account(usage, pin))
        .map(|account| omp_provider::snapshot(target, account))
        .or_else(|| {
            if report.is_some() {
                return None;
            }
            legacy
                .as_ref()
                .filter(|snapshot| snapshot.usable_for_account(pin, None))
                .cloned()
        });
    let unavailable = || {
        Some(PaneQuotaUpdate::Replace(Box::new(
            MetadataTokens::unavailable(target.billing, "quota account is not confirmed"),
        )))
    };
    let debounced = cache
        .should_debounce_target(target, now, 60)
        .unwrap_or(false);
    if debounce_reuses_snapshot(force, debounced, cached.as_ref(), pin, None, now) {
        return cached
            .as_ref()
            .and_then(|snapshot| {
                tokens_for_provider(Some(snapshot), now, None, row)
                    .map(|values| PaneQuotaUpdate::Replace(Box::new(values)))
            })
            .or_else(unavailable);
    }
    match refresh(cache, target, evidence, now) {
        OmpUsage::Account(snapshot) => tokens_for_provider(Some(&snapshot), now, None, row)
            .map(|values| PaneQuotaUpdate::Replace(Box::new(values))),
        // omp holds an API key for this provider and no subscription account
        // at all, so any subscription numbers still on the pane belong to a
        // login that is not paying for it.
        OmpUsage::PayAsYouGo => Some(PaneQuotaUpdate::Clear),
        OmpUsage::Unavailable if cached.is_none() => Some(PaneQuotaUpdate::Replace(Box::new(
            MetadataTokens::unavailable(target.billing, "omp reported no quota data"),
        ))),
        OmpUsage::Unavailable | OmpUsage::Unknown => cached
            .as_ref()
            .and_then(|snapshot| {
                tokens_for_provider(Some(snapshot), now, None, row)
                    .map(|values| PaneQuotaUpdate::Replace(Box::new(values)))
            })
            .or_else(unavailable),
    }
}

/// What one `omp usage --json` call established about a pane's provider.
enum OmpUsage {
    Account(Box<ProviderSnapshot>),
    PayAsYouGo,
    Unavailable,
    Unknown,
}

/// Ask omp for one provider's usage, and cache the account this pane pins.
///
/// Process and parse failures remain silent and preserve the last good value.
/// A successful CLI response that explicitly lists this OAuth account under
/// `accountsWithoutUsage` is different: without an older snapshot it publishes
/// `quota_error` and omits window rows so a failed upstream quota fetch is
/// not mistaken for missing support.
fn refresh_omp_target(
    cache: &CacheStore,
    target: &BillingTarget,
    evidence: &OmpEvidence,
    now: u64,
) -> OmpUsage {
    let Ok(Some(_lease)) = cache.try_lock_target_refresh(target) else {
        return OmpUsage::Unknown;
    };
    // Marked before the call so a failing binary cannot be retried on every
    // event; the window applies to attempts, not to successes.
    if cache.mark_refresh_target(target, now).is_err() {
        return OmpUsage::Unknown;
    }
    let Ok(usage) = omp_provider::fetch(&evidence.paths, &evidence.provider_id, now) else {
        return OmpUsage::Unknown;
    };
    if cache.save_omp_usage(target, &usage).is_err() {
        return OmpUsage::Unknown;
    }
    let Some(account) = omp_provider::select_account(&usage, evidence.account_pin.as_deref())
    else {
        if omp_provider::oauth_without_usage_matches(&usage, evidence.account_pin.as_deref()) {
            return OmpUsage::Unavailable;
        }
        // Several accounts and no pin is not a coin flip either: only a
        // provider that has an API key and nothing else is proved to be
        // pay-as-you-go.
        return if usage.accounts.is_empty()
            && usage.oauth_without_usage_pins.is_empty()
            && usage.has_api_key
        {
            OmpUsage::PayAsYouGo
        } else {
            OmpUsage::Unknown
        };
    };
    let snapshot = omp_provider::snapshot(target, account);
    if cache.save_target(target, &snapshot).is_err() {
        return OmpUsage::Unknown;
    }
    OmpUsage::Account(Box::new(snapshot))
}

/// Refresh a billing target that has no 1:1 harness collector.
///
/// Failure is deliberately silent: the pane keeps the last good snapshot for
/// this same target rather than being cleared, and a missing key is a normal
/// state (the user may not have a Go subscription) rather than an error worth
/// surfacing on every event.
fn refresh_scoped_target(cache: &CacheStore, target: &BillingTarget, force: bool) {
    let now = CacheStore::now_unix();
    if should_skip_fetch(cache, target.billing, force, now).unwrap_or(true) {
        return;
    }
    let Ok(Some(_lease)) = cache.try_lock_target_refresh(target) else {
        return;
    };
    let Some(paths) = OpenCodePaths::from_env() else {
        return;
    };
    let Some(key) = crate::opencode::go_key(&paths) else {
        return;
    };
    // Marked before the request so a failing endpoint cannot be retried on
    // every event; the debounce window applies to attempts, not successes.
    if cache
        .mark_refresh_account(
            target.billing,
            now,
            Some(&crate::providers::credential_id(&key)),
        )
        .is_err()
    {
        return;
    }
    if let Ok(snapshot) = opencode_go::fetch(&key) {
        let _ = cache.save(&snapshot);
    }
}

fn covers_every_collector(providers: &[Provider]) -> bool {
    Provider::ALL
        .iter()
        .all(|provider| providers.contains(provider))
}

fn panes_for_providers(panes: &[AgentPane], providers: &[Provider]) -> Vec<AgentPane> {
    panes
        .iter()
        .filter(|pane| {
            pane.harness
                .billing()
                .is_some_and(|billing| providers.contains(&billing))
        })
        .cloned()
        .collect()
}

#[derive(Debug)]
struct FetchedSnapshot {
    snapshot: ProviderSnapshot,
    preserve_context: bool,
    session_id: Option<String>,
}

impl FetchedSnapshot {
    fn direct(snapshot: ProviderSnapshot) -> Self {
        Self {
            snapshot,
            preserve_context: false,
            session_id: None,
        }
    }
}

fn refresh_selected(
    cache: &CacheStore,
    providers: &[Provider],
    force: bool,
    panes: &[AgentPane],
) -> Result<Vec<ProviderOutcome>> {
    providers
        .iter()
        .copied()
        .map(|provider| refresh_provider(cache, provider, force, panes))
        .collect()
}

fn refresh_provider(
    cache: &CacheStore,
    provider: Provider,
    force: bool,
    panes: &[AgentPane],
) -> Result<ProviderOutcome> {
    let now = CacheStore::now_unix();
    if should_skip_fetch(cache, provider, force, now)? {
        return Ok(ProviderOutcome {
            provider,
            available: load_usable_snapshot(cache, provider)?.is_some(),
            from_cache: true,
            error: None,
        });
    }
    let Some(_lease) = cache.try_lock_provider_refresh(provider)? else {
        return Ok(ProviderOutcome {
            provider,
            available: load_usable_snapshot(cache, provider)?.is_some(),
            from_cache: true,
            error: Some("refresh already in progress".to_string()),
        });
    };

    let session_ids = panes
        .iter()
        .filter(|pane| pane.harness.billing() == Some(provider))
        .filter_map(|pane| {
            pane.session
                .as_ref()
                .and_then(|session| session.id())
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    let (account_id, _) = current_account_gate(provider);
    cache.mark_refresh_account(provider, now, account_id.as_deref())?;
    let fetched = match provider {
        Provider::Codex => codex::fetch_for_sessions(&session_ids).map(FetchedSnapshot::direct),
        Provider::Grok => {
            let cwds = panes
                .iter()
                .filter(|pane| pane.harness == Harness::Grok)
                .filter_map(|pane| {
                    let session_id = pane.session.as_ref()?.id()?.to_string();
                    (!pane.cwd.is_empty()).then(|| (session_id, pane.cwd.clone()))
                })
                .collect::<Vec<_>>();
            grok::fetch_for_sessions_with_cwds(&session_ids, &cwds).map(FetchedSnapshot::direct)
        }
        Provider::Devin => devin::fetch_for_sessions(&session_ids).map(FetchedSnapshot::direct),
        Provider::Muse => muse::fetch_for_sessions(&session_ids).map(FetchedSnapshot::direct),
        Provider::Cursor => cursor::fetch_for_sessions(&session_ids).map(FetchedSnapshot::direct),
        Provider::Claude | Provider::Agy => load_statusline_snapshot(cache, provider),
        // OpenCode Go is fetched for a resolved pane, never through the
        // provider list; see `fetch_opencode_go`.
        Provider::OpenCodeGo | Provider::Omp => Err(anyhow::anyhow!(
            "scoped providers are refreshed per resolved pane, not through --provider"
        )),
    };
    match fetched {
        Ok(fetched) => {
            let FetchedSnapshot {
                mut snapshot,
                preserve_context,
                session_id,
            } = fetched;
            if preserve_context {
                cache.save_preserving_context_for_session(snapshot, session_id.as_deref())?;
            } else if matches!(
                provider,
                Provider::Codex
                    | Provider::Grok
                    | Provider::Devin
                    | Provider::Muse
                    | Provider::Cursor
            ) {
                let (_, mtime) = current_account_gate(provider);
                cache.save_preserving_diagnostics_for_sessions(
                    &mut snapshot,
                    &session_ids,
                    mtime,
                )?;
            } else {
                cache.save(&snapshot)?;
            }
            Ok(ProviderOutcome {
                provider,
                available: true,
                from_cache: false,
                error: None,
            })
        }
        Err(error) => Ok(ProviderOutcome {
            provider,
            available: load_usable_snapshot(cache, provider)?.is_some(),
            from_cache: true,
            error: Some(error.to_string()),
        }),
    }
}

fn should_skip_fetch(
    cache: &CacheStore,
    provider: Provider,
    force: bool,
    now_unix: u64,
) -> Result<bool> {
    let (account, mtime) = current_account_gate(provider);
    should_skip_fetch_for_account(cache, provider, force, now_unix, account.as_deref(), mtime)
}

fn should_skip_fetch_for_account(
    cache: &CacheStore,
    provider: Provider,
    force: bool,
    now_unix: u64,
    account: Option<&str>,
    mtime: Option<u64>,
) -> Result<bool> {
    let snapshot = cache.load(provider)?;
    if !debounce_reuses_snapshot(
        force,
        cache.should_debounce(provider, now_unix, 60)?,
        snapshot.as_ref(),
        account,
        mtime,
        now_unix,
    ) {
        return Ok(false);
    }
    if let Some(attempted) = cache.last_refresh_account(provider) {
        return Ok(attempted.as_deref() == account);
    }
    if snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.usable_for_account(account, mtime))
    {
        return Ok(true);
    }
    // No snapshot at all: keep debounce so missing credentials do not hammer
    // the provider. A snapshot for another account must not debounce — fetch
    // the signed-in identity now.
    Ok(snapshot.is_none())
}

/// Debounce may reuse a cached snapshot unless that same account's windows
/// have already reset. Shared by the list collectors and omp so a lapsed 5h
/// or weekly window is one policy, not two.
fn debounce_reuses_snapshot(
    force: bool,
    debounced: bool,
    snapshot: Option<&ProviderSnapshot>,
    account: Option<&str>,
    mtime: Option<u64>,
    now_unix: u64,
) -> bool {
    !force
        && debounced
        && !snapshot.is_some_and(|snapshot| {
            snapshot.usable_for_account(account, mtime) && snapshot.has_expired_quota(now_unix)
        })
}

fn load_usable_snapshot(
    cache: &CacheStore,
    provider: Provider,
) -> Result<Option<ProviderSnapshot>> {
    let Some(snapshot) = cache.load(provider)? else {
        return Ok(None);
    };
    let (account_id, mtime) = current_account_gate(provider);
    Ok(snapshot
        .usable_for_account(account_id.as_deref(), mtime)
        .then_some(snapshot))
}

fn current_account_gate(provider: Provider) -> (Option<String>, Option<u64>) {
    match provider {
        Provider::Grok => {
            let path = grok::auth_path().ok();
            let account_id = path
                .as_ref()
                .and_then(|path| grok::read_credentials(path).ok())
                .map(|credentials| credentials.account_id());
            let mtime = path.as_ref().and_then(|path| grok::auth_mtime_unix(path));
            (account_id, mtime)
        }
        Provider::Codex => (codex::current_account_id(), codex::auth_mtime_unix()),
        Provider::Devin => (devin::current_account_id(), devin::auth_mtime_unix()),
        Provider::Muse => (muse::current_account_id(), muse::auth_mtime_unix()),
        Provider::Cursor => (cursor::current_account_id(), cursor::auth_mtime_unix()),
        Provider::OpenCodeGo => (
            OpenCodePaths::from_env()
                .and_then(|paths| crate::opencode::go_key(&paths))
                .map(|key| crate::providers::credential_id(&key)),
            None,
        ),
        Provider::Claude | Provider::Agy | Provider::Omp => (None, None),
    }
}

fn load_statusline_snapshot(cache: &CacheStore, provider: Provider) -> Result<FetchedSnapshot> {
    let observation = cache
        .load_statusline_observation(provider)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} usage is collected by the statusLine hook",
                provider.source()
            )
        })?;
    let mut snapshot = observation.snapshot;
    let value = observation.payload;
    if !snapshot.session_quota_only {
        // Migrate from the original raw observation, not legacy windows
        // merged across a profile. No credential or session reset is needed.
        snapshot = match provider {
            Provider::Claude => {
                crate::providers::claude::parse_statusline(&value, snapshot.fetched_at_unix)?
            }
            Provider::Agy => {
                crate::providers::agy::parse_statusline(&value, snapshot.fetched_at_unix)?
            }
            _ => snapshot,
        };
    }
    let previous_cache = cache
        .load(provider)
        .ok()
        .flatten()
        .and_then(|snapshot| snapshot.context)
        .and_then(|context| context.cache);
    enrich_cache_session(&mut snapshot, &value, previous_cache.as_ref());
    if provider == Provider::Claude {
        crate::providers::claude::apply_prompt_cache(
            &mut snapshot.context,
            value
                .get("prompt_cache")
                .or_else(|| value.get("promptCache")),
        );
    }
    let session_id = value
        .get("session_id")
        .or_else(|| value.get("sessionId"))
        .or_else(|| value.get("conversation_id"))
        .or_else(|| value.get("conversationId"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    Ok(FetchedSnapshot {
        snapshot,
        preserve_context: true,
        session_id,
    })
}

fn publish_resolved(
    cache: &CacheStore,
    panes: &mut [AgentPane],
    topic_pane: Option<&str>,
    force: bool,
    allow_icon_while_scrolled: bool,
) -> Result<()> {
    apply_icon_attention(panes, cache, None, false, false)?;
    if let Some(pane) =
        topic_pane.and_then(|pane_id| panes.iter_mut().find(|pane| pane.pane_id == pane_id))
    {
        refresh_pane_topic(pane);
    }
    let mut tokens = Vec::new();
    let now = CacheStore::now_unix();
    let row = publish_row(cache);
    let mut refreshed_targets = Vec::new();
    for pane in panes.iter_mut() {
        let resolved = route::resolve_with_identity(pane);
        let force_target = if let Resolution::Subscription(target) = &resolved.resolution {
            let first = !refreshed_targets.contains(target);
            refreshed_targets.push(*target);
            force && first
        } else {
            false
        };
        tokens.push(
            resolved_pane_tokens(cache, pane, resolved, now, row, force_target)?.unwrap_or(
                PaneTokens {
                    pane_id: pane.pane_id.clone(),
                    quota: PaneQuotaUpdate::Preserve,
                    identity: None,
                    context: None,
                    show_account_quota: true,
                },
            ),
        );
    }
    notify_low_quota(cache, &tokens);
    let mut publish_panes = panes.to_vec();
    sync_vendor_row_siblings(&mut tokens, &mut publish_panes);
    if allow_icon_while_scrolled {
        publish_pane_tokens_with_scrolled_icons(
            &publish_panes,
            &tokens,
            CacheStore::now_millis(),
            row,
        )
    } else {
        publish_pane_tokens(&publish_panes, &tokens, CacheStore::now_millis(), row)
    }
}

/// Overlay this pass's fresher focus/status onto the full agent list so an
/// event that names one pane still sees same-space siblings.
fn panes_for_vendor_rows(live: &[AgentPane]) -> Vec<AgentPane> {
    let Ok(mut inventory) = list_agent_panes() else {
        return live.to_vec();
    };
    if inventory.is_empty() {
        return live.to_vec();
    }
    for pane in live {
        if let Some(existing) = inventory
            .iter_mut()
            .find(|existing| existing.pane_id == pane.pane_id)
        {
            *existing = pane.clone();
        } else {
            inventory.push(pane.clone());
        }
    }
    inventory
}

/// Mark the current pass, then republish same-Space extras whose account
/// windows still disagree with the representative. An event that only names
/// one Grok would otherwise leave a sibling showing duplicate 5h/7d/30d.
fn sync_vendor_row_siblings(tokens: &mut Vec<PaneTokens>, panes: &mut Vec<AgentPane>) {
    let inventory = panes_for_vendor_rows(panes);
    mark_one_quota_row_per_vendor(tokens, &inventory);
    for extra in vendor_row_sync_extras(tokens, &inventory) {
        if let Some(pane) = inventory
            .iter()
            .find(|pane| pane.pane_id == extra.pane_id)
            .cloned()
        {
            panes.push(pane);
            tokens.push(extra);
        }
    }
}

fn vendor_row_sync_extras(tokens: &[PaneTokens], inventory: &[AgentPane]) -> Vec<PaneTokens> {
    let published = tokens
        .iter()
        .map(|token| token.pane_id.as_str())
        .collect::<BTreeSet<_>>();
    let live_groups = tokens
        .iter()
        .filter_map(|token| {
            inventory
                .iter()
                .find(|pane| pane.pane_id == token.pane_id)
                .filter(|pane| crate::herdr::shares_login_quota(pane.harness))
                .map(crate::herdr::nest_group_key)
        })
        .collect::<BTreeSet<_>>();
    inventory
        .iter()
        .filter(|pane| {
            !published.contains(pane.pane_id.as_str())
                && crate::herdr::shares_login_quota(pane.harness)
                && live_groups.contains(&crate::herdr::nest_group_key(pane))
        })
        .filter_map(|pane| {
            let should_show = representative_pane_id(pane, inventory) == pane.pane_id;
            pane_needs_vendor_restyle(pane, inventory, should_show).then(|| PaneTokens {
                pane_id: pane.pane_id.clone(),
                quota: PaneQuotaUpdate::Preserve,
                identity: None,
                context: None,
                show_account_quota: should_show,
            })
        })
        .collect()
}

/// Keep account quota on one pane per login-scoped vendor *in each Space*.
/// Two Grok tabs in the same project collapse; a Grok in another Space stays.
fn mark_one_quota_row_per_vendor(tokens: &mut [PaneTokens], panes: &[AgentPane]) {
    for token in tokens.iter_mut() {
        let Some(pane) = panes.iter().find(|pane| pane.pane_id == token.pane_id) else {
            continue;
        };
        if !crate::herdr::shares_login_quota(pane.harness) {
            continue;
        }
        let representative = representative_pane_id(pane, panes);
        if token.pane_id != representative {
            token.show_account_quota = false;
        }
    }
}

fn pane_has_account_windows(pane: &AgentPane) -> bool {
    pane.tokens.keys().any(|name| {
        name.starts_with("quota_5h_")
            || name.starts_with("quota_week_")
            || name.starts_with("quota_month_")
            || name.starts_with("quota_share_")
    })
}

fn pane_has_share_windows(pane: &AgentPane) -> bool {
    pane.tokens
        .keys()
        .any(|name| name.starts_with("quota_share_"))
}

fn pane_has_brand_identity(pane: &AgentPane) -> bool {
    pane.tokens.keys().any(|name| {
        matches!(
            name.as_str(),
            "quota_icon"
                | "quota_icon_working"
                | "quota_icon_done"
                | "quota_provider"
                | "quota_provider_model"
        )
    })
}

fn group_is_nested(pane: &AgentPane, inventory: &[AgentPane]) -> bool {
    let key = crate::herdr::nest_group_key(pane);
    inventory
        .iter()
        .filter(|candidate| crate::herdr::nest_group_key(candidate) == key)
        .count()
        >= 2
}

/// True when this unpublished sibling's tokens would render the wrong vendor
/// role. A second Cursor in a Space must restyle the existing Flat
/// representative into a nested head even if it already has 5h/7d/30d.
fn pane_needs_vendor_restyle(pane: &AgentPane, inventory: &[AgentPane], should_show: bool) -> bool {
    let has_windows = pane_has_account_windows(pane);
    if should_show != has_windows {
        return true;
    }
    let nested = group_is_nested(pane, inventory);
    if nested && should_show && !pane_has_share_windows(pane) {
        return true;
    }
    if nested && !should_show && pane_has_brand_identity(pane) {
        return true;
    }
    if !nested && (pane_has_share_windows(pane) || !pane_has_brand_identity(pane)) {
        return plugin_quota_present(&pane.tokens);
    }
    false
}

fn representative_pane_id(current: &AgentPane, panes: &[AgentPane]) -> String {
    let key = crate::herdr::nest_group_key(current);
    panes
        .iter()
        .filter(|pane| crate::herdr::nest_group_key(pane) == key)
        .map(|pane| pane.pane_id.as_str())
        .min()
        .unwrap_or(current.pane_id.as_str())
        .to_string()
}

/// The lowest headroom each provider is showing in this pass.
///
/// Keyed by the provider's display name because that is both what a pane
/// reports and what a notification has to say. Several panes on one provider
/// collapse to one entry, so three Claude panes are one warning.
fn lowest_headroom_by_provider(tokens: &[PaneTokens]) -> BTreeMap<String, u8> {
    let mut lowest = BTreeMap::new();
    for pane in tokens {
        let PaneQuotaUpdate::Replace(values) = &pane.quota else {
            continue;
        };
        let Some(headroom) = values.quota_headroom else {
            continue;
        };
        lowest
            .entry(values.quota_provider.clone())
            .and_modify(|current: &mut u8| *current = (*current).min(headroom))
            .or_insert(headroom);
    }
    lowest
}

/// Warn once per provider that has fallen to the alert threshold.
///
/// A provider stays quiet for as long as it stays low, and is re-armed only by
/// recovering above the threshold — a quota that resets and is spent again
/// warns again. Providers with no pane in this pass keep whatever state they
/// had, so closing and reopening a pane is not a way to be warned twice.
fn notify_low_quota(cache: &CacheStore, tokens: &[PaneTokens]) {
    let alert = cache.low_quota_alert().unwrap_or_default();
    if alert.is_off() {
        return;
    }
    let lowest = lowest_headroom_by_provider(tokens);
    let previous = cache.low_quota_alerted();
    let (warn, alerted) = low_quota_transitions(alert, &lowest, &previous);
    for provider in &warn {
        let headroom = lowest.get(provider).copied().unwrap_or_default();
        let _ = crate::herdr::notify(
            &format!("{provider} quota is low"),
            &format!("{headroom}% left in the window closest to its limit."),
        );
    }
    // Publishing happens on every event path. Rewriting an unchanged set every
    // time would be disk churn for nothing.
    if alerted != previous {
        let _ = cache.set_low_quota_alerted(&alerted);
    }
}

/// Which providers to warn about now, and the state to remember afterwards.
///
/// Split out from the notification itself so the rule can be tested without a
/// cache or a Herdr: a provider is warned about on the way down and not again
/// until it has been seen above the threshold.
fn low_quota_transitions(
    alert: LowQuotaAlert,
    lowest: &BTreeMap<String, u8>,
    previous: &[String],
) -> (Vec<String>, Vec<String>) {
    // A provider with no pane in this pass keeps the state it had. Otherwise
    // closing a pane would re-arm the warning and reopening it would repeat.
    let mut alerted: Vec<String> = previous
        .iter()
        .filter(|provider| !lowest.contains_key(*provider))
        .cloned()
        .collect();
    let mut warn = Vec::new();
    for (provider, headroom) in lowest {
        if !alert.triggers(*headroom) {
            continue;
        }
        alerted.push(provider.clone());
        if !previous.contains(provider) {
            warn.push(provider.clone());
        }
    }
    alerted.sort();
    alerted.dedup();
    (warn, alerted)
}

fn event_json() -> Option<Value> {
    let input = std::env::var("HERDR_PLUGIN_EVENT_JSON").ok()?;
    serde_json::from_str(&input).ok()
}

fn find_status(value: &Value) -> Option<&str> {
    find_field(value, &["agent_status", "agentStatus", "status", "state"])
}

fn is_working_status(status: &str) -> bool {
    status.eq_ignore_ascii_case("working")
}

fn current_exe_modified() -> Option<SystemTime> {
    std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())
}

fn watch_binary_is_newer(started: SystemTime, modified: Option<SystemTime>) -> bool {
    modified.is_some_and(|mtime| mtime > started)
}

fn reexec_watch(server: Option<&WatchHerdrEnvironment>, interval_seconds: u64) -> Result<()> {
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    let mut command = Command::new(executable);
    command.args([
        "watch",
        "--provider",
        "all",
        "--interval-seconds",
        &interval_seconds.to_string(),
    ]);
    if let Some(server) = server {
        for (name, value) in [
            ("HERDR_BIN_PATH", &server.binary),
            ("HERDR_SOCKET_PATH", &server.socket),
        ] {
            if let Some(value) = value {
                command.env(name, value);
            } else {
                command.env_remove(name);
            }
        }
    }
    #[cfg(unix)]
    {
        let error = command.exec();
        anyhow::bail!("re-exec active-turn quota watcher: {error}");
    }
    #[cfg(not(unix))]
    {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("restart active-turn quota watcher")?;
        Ok(())
    }
}

fn spawn_watch(defer: bool) -> Result<()> {
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    let mut command = Command::new(executable);
    command
        .args(["watch", "--provider", "all"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if defer {
        command.arg("--defer");
    }
    #[cfg(unix)]
    unsafe {
        // A Herdr event process is short-lived. Put the watcher in its own
        // process group so it survives the hook supervisor cleanly.
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().context("start active-turn quota watcher")?;
    Ok(())
}

// Event payloads are nested and their shape differs per event, so look the
// field up anywhere in the tree rather than at a fixed path.
fn find_field<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    match value {
        Value::Object(map) => names
            .iter()
            .find_map(|name| map.get(*name).and_then(Value::as_str))
            .or_else(|| map.values().find_map(|child| find_field(child, names))),
        Value::Array(values) => values.iter().find_map(|child| find_field(child, names)),
        _ => None,
    }
}

fn find_agent(value: &Value) -> Option<&str> {
    find_field(value, &["agent"])
}

fn find_pane_id(value: &Value) -> Option<&str> {
    find_field(value, &["pane_id", "paneId"])
}

fn tokens_for_provider(
    snapshot: Option<&crate::model::ProviderSnapshot>,
    now_unix: u64,
    session_id: Option<&str>,
    row: RowStyle,
) -> Option<MetadataTokens> {
    snapshot.map(|snapshot| {
        MetadataTokens::from_snapshot_for_pane_with_row(snapshot, now_unix, session_id, row)
    })
}

fn tokens_for_loaded_snapshot(
    provider: Provider,
    raw: Option<&ProviderSnapshot>,
    usable: Option<&ProviderSnapshot>,
    now_unix: u64,
    session_id: Option<&str>,
    row: RowStyle,
) -> Option<MetadataTokens> {
    let mut overlaid = None;
    let usable = match (provider, usable) {
        (Provider::Cursor, Some(snapshot)) => {
            let mut snapshot = snapshot.clone();
            cursor::overlay_live_context(&mut snapshot, session_id);
            Some(&*overlaid.insert(snapshot))
        }
        (_, usable) => usable,
    };
    match (usable, raw) {
        (Some(snapshot), _) => tokens_for_provider(Some(snapshot), now_unix, session_id, row),
        (None, Some(raw)) => Some(MetadataTokens::unavailable_for_windows(
            provider,
            "signed-in account changed",
            &raw.windows,
        )),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{PercentStyle, SidebarLayout};
    use crate::model::{
        ProviderSnapshot, ResetAt, SessionQuotaObservation, UsageWindow, WindowKind,
    };
    use tempfile::tempdir;

    fn test_pane(id: &str, harness: Harness) -> AgentPane {
        AgentPane {
            pane_id: id.to_string(),
            workspace_id: "w1".to_string(),
            cwd: String::new(),
            title: String::new(),
            harness,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
            status: AgentStatus::Idle,
            focused: false,
        }
    }

    #[test]
    fn muse_cursor_and_grok_session_summaries_replace_the_topic() {
        let mut muse = test_pane("w1:p1", Harness::Muse);
        muse.topic = "old prompt".to_string();
        apply_session_summary(&mut muse, "new prompt");
        assert_eq!(muse.topic, "new prompt");
        assert_eq!(muse.session_summary, "new prompt");

        let mut cursor = test_pane("w1:p3", Harness::Cursor);
        cursor.topic = "old prompt".to_string();
        apply_session_summary(&mut cursor, "hi");
        assert_eq!(cursor.topic, "hi");

        let mut grok = test_pane("w1:p4", Harness::Grok);
        grok.topic = "old prompt".to_string();
        apply_session_summary(&mut grok, "Chat honesty: unsupported answers");
        assert_eq!(grok.topic, "Chat honesty: unsupported answers");

        let mut codex = test_pane("w1:p2", Harness::Codex);
        codex.topic = "screen topic".to_string();
        apply_session_summary(&mut codex, "thread name");
        assert_eq!(codex.topic, "screen topic");
        assert_eq!(codex.session_summary, "thread name");
    }

    fn test_pane_with_session(id: &str, harness: Harness, session: &str) -> AgentPane {
        let mut pane = test_pane(id, harness);
        pane.session = Some(crate::herdr::AgentSession {
            kind: Some("id".to_string()),
            value: session.to_string(),
        });
        pane
    }

    fn window(kind: WindowKind, used: f64, reset: u64) -> UsageWindow {
        UsageWindow::new(kind, used, Some(ResetAt::from_unix_seconds(reset))).unwrap()
    }

    fn low(pairs: &[(&str, u8)]) -> BTreeMap<String, u8> {
        pairs
            .iter()
            .map(|(provider, headroom)| ((*provider).to_string(), *headroom))
            .collect()
    }

    #[test]
    fn failed_new_login_attempts_are_debounced_without_reusing_old_quota() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        for provider in [
            Provider::Codex,
            Provider::Grok,
            Provider::Devin,
            Provider::Muse,
            Provider::Cursor,
            Provider::OpenCodeGo,
        ] {
            cache
                .save(
                    &ProviderSnapshot::new(provider, vec![], 90)
                        .with_account_id(Some("old".into())),
                )
                .unwrap();
            cache
                .mark_refresh_account(provider, 100, Some("old"))
                .unwrap();
            assert!(!should_skip_fetch_for_account(
                &cache,
                provider,
                false,
                110,
                Some("new"),
                None
            )
            .unwrap());
            cache
                .mark_refresh_account(provider, 110, Some("new"))
                .unwrap();
            assert!(
                should_skip_fetch_for_account(&cache, provider, false, 120, Some("new"), None)
                    .unwrap()
            );
            assert!(!cache
                .load(provider)
                .unwrap()
                .unwrap()
                .usable_for_account(Some("new"), None));
            assert!(!should_skip_fetch_for_account(
                &cache,
                provider,
                false,
                170,
                Some("new"),
                None
            )
            .unwrap());
            assert!(
                !should_skip_fetch_for_account(&cache, provider, true, 120, Some("new"), None)
                    .unwrap()
            );
        }
    }

    #[test]
    fn a_settled_provider_gets_a_pass_after_debounce_while_another_keeps_working() {
        let a = "codex-pane".to_string();
        let b = "omp-pane".to_string();
        let mut settling = BTreeMap::new();
        assert!(watch_targets(
            std::slice::from_ref(&b),
            &[a.clone(), b.clone()],
            &mut settling,
            10
        )
        .contains(&a));
        assert!(watch_targets(
            std::slice::from_ref(&b),
            std::slice::from_ref(&b),
            &mut settling,
            40
        )
        .contains(&a));
        assert!(watch_targets(
            std::slice::from_ref(&b),
            std::slice::from_ref(&b),
            &mut settling,
            70
        )
        .contains(&a));
        assert!(settling.is_empty());
        assert!(!watch_targets(
            std::slice::from_ref(&b),
            std::slice::from_ref(&b),
            &mut settling,
            100
        )
        .contains(&a));
    }

    #[test]
    fn an_idle_pane_with_an_expired_window_joins_a_running_watch_pass() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        cache
            .save(&ProviderSnapshot::new(
                Provider::Codex,
                vec![
                    window(WindowKind::FiveHour, 96.0, 1_000),
                    window(WindowKind::Weekly, 48.0, 10_000),
                ],
                900,
            ))
            .unwrap();
        let panes = [
            test_pane("codex-idle", Harness::Codex),
            test_pane("grok-working", Harness::Grok),
        ];
        let grok = "grok-working".to_string();
        let mut settling = BTreeMap::new();
        let affected = watch_pass_ids(
            &cache,
            &panes,
            &Provider::ALL,
            std::slice::from_ref(&grok),
            std::slice::from_ref(&grok),
            &mut settling,
            1_001,
        );
        assert!(affected.contains(&"codex-idle".to_string()));
        assert!(affected.contains(&grok));
    }

    #[test]
    fn a_live_idle_pane_does_not_join_another_providers_watch_pass() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        cache
            .save(&ProviderSnapshot::new(
                Provider::Codex,
                vec![
                    window(WindowKind::FiveHour, 20.0, 2_000),
                    window(WindowKind::Weekly, 48.0, 10_000),
                ],
                900,
            ))
            .unwrap();
        let panes = [
            test_pane("codex-idle", Harness::Codex),
            test_pane("grok-working", Harness::Grok),
        ];
        let grok = "grok-working".to_string();
        let mut settling = BTreeMap::new();
        let affected = watch_pass_ids(
            &cache,
            &panes,
            &Provider::ALL,
            std::slice::from_ref(&grok),
            std::slice::from_ref(&grok),
            &mut settling,
            1_001,
        );
        assert!(!affected.contains(&"codex-idle".to_string()));
        assert_eq!(affected, vec![grok]);
    }

    #[test]
    fn an_expired_idle_pane_stays_out_of_a_narrower_watch_selection() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        cache
            .save(&ProviderSnapshot::new(
                Provider::Codex,
                vec![window(WindowKind::FiveHour, 96.0, 1_000)],
                900,
            ))
            .unwrap();
        let panes = [
            test_pane("codex-idle", Harness::Codex),
            test_pane("grok-working", Harness::Grok),
        ];
        let grok = "grok-working".to_string();
        let mut settling = BTreeMap::new();
        let affected = watch_pass_ids(
            &cache,
            &panes,
            &[Provider::Grok],
            std::slice::from_ref(&grok),
            std::slice::from_ref(&grok),
            &mut settling,
            1_001,
        );
        assert!(!affected.contains(&"codex-idle".to_string()));
        assert_eq!(affected, vec![grok]);
    }

    #[test]
    fn an_expired_session_does_not_pull_a_live_sibling_into_the_watch_pass() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let mut snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 900).session_local();
        snapshot.session_windows.insert(
            "live".to_string(),
            vec![window(WindowKind::FiveHour, 20.0, 2_000)],
        );
        snapshot.session_windows.insert(
            "dead".to_string(),
            vec![window(WindowKind::FiveHour, 96.0, 1_000)],
        );
        cache.save(&snapshot).unwrap();
        let panes = [
            test_pane_with_session("claude-live", Harness::Claude, "live"),
            test_pane_with_session("claude-dead", Harness::Claude, "dead"),
            test_pane("grok-working", Harness::Grok),
        ];
        let grok = "grok-working".to_string();
        let mut settling = BTreeMap::new();
        let affected = watch_pass_ids(
            &cache,
            &panes,
            &Provider::ALL,
            std::slice::from_ref(&grok),
            std::slice::from_ref(&grok),
            &mut settling,
            1_001,
        );
        assert!(affected.contains(&"claude-dead".to_string()));
        assert!(!affected.contains(&"claude-live".to_string()));
    }

    #[test]
    fn an_idle_pane_follows_its_sessions_new_windows_without_an_agent_event() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let mut snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 900).session_local();
        snapshot.session_windows.insert(
            "sibling".to_string(),
            vec![window(WindowKind::FiveHour, 20.0, 9_000)],
        );
        cache.save(&snapshot).unwrap();

        // Its own session had no stored window, so the pane omitted 5h
        // but still carries identity tokens from a previous publish.
        let mut idle = test_pane_with_session("claude-idle", Harness::Claude, "s1");
        idle.tokens
            .insert("quota_provider".to_string(), "Claude".to_string());
        let panes = [idle, test_pane("grok-working", Harness::Grok)];
        let grok = "grok-working".to_string();
        let mut settling = BTreeMap::new();

        let unchanged = watch_pass_ids(
            &cache,
            &panes,
            &Provider::ALL,
            std::slice::from_ref(&grok),
            std::slice::from_ref(&grok),
            &mut settling,
            1_001,
        );
        assert!(!unchanged.contains(&"claude-idle".to_string()));

        snapshot.session_windows.insert(
            "s1".to_string(),
            vec![
                window(WindowKind::FiveHour, 40.0, 9_000),
                window(WindowKind::Weekly, 20.0, 90_000),
            ],
        );
        cache.save(&snapshot).unwrap();

        let affected = watch_pass_ids(
            &cache,
            &panes,
            &Provider::ALL,
            std::slice::from_ref(&grok),
            std::slice::from_ref(&grok),
            &mut settling,
            1_001,
        );
        assert!(affected.contains(&"claude-idle".to_string()));
    }

    #[test]
    fn an_idle_pane_already_showing_the_cached_windows_stays_out_of_the_pass() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let mut snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 900).session_local();
        snapshot.session_windows.insert(
            "s1".to_string(),
            vec![
                window(WindowKind::FiveHour, 40.0, 9_000),
                window(WindowKind::Weekly, 20.0, 90_000),
            ],
        );
        snapshot.session_quota_observations.insert(
            "s1".to_string(),
            vec![
                SessionQuotaObservation {
                    kind: WindowKind::FiveHour,
                    observed_at_unix: Some(1_000),
                    api_generation: Some("generation-a".to_string()),
                },
                SessionQuotaObservation {
                    kind: WindowKind::Weekly,
                    observed_at_unix: Some(1_000),
                    api_generation: Some("generation-a".to_string()),
                },
            ],
        );
        cache.save(&snapshot).unwrap();

        let row = publish_row(&cache);
        let values = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            1_001,
            Some("s1"),
            row.percent,
            row.shape,
        );
        assert_eq!(
            values.quota_5h_severity,
            Some(crate::model::Severity::Normal)
        );
        assert_eq!(
            values.quota_week_severity,
            Some(crate::model::Severity::Normal)
        );
        let mut idle = test_pane_with_session("claude-idle", Harness::Claude, "s1");
        idle.tokens
            .insert("quota_5h_normal".to_string(), values.quota_5h.clone());
        idle.tokens
            .insert("quota_week_normal".to_string(), values.quota_week.clone());
        idle.tokens.insert(
            "quota_headroom".to_string(),
            format!("{:03}", values.quota_headroom.unwrap()),
        );
        let panes = [idle, test_pane("grok-working", Harness::Grok)];
        let grok = "grok-working".to_string();
        let mut settling = BTreeMap::new();

        let affected = watch_pass_ids(
            &cache,
            &panes,
            &Provider::ALL,
            std::slice::from_ref(&grok),
            std::slice::from_ref(&grok),
            &mut settling,
            1_001,
        );
        assert_eq!(affected, vec![grok]);
    }

    #[test]
    fn new_windows_for_one_session_leave_another_sessions_idle_pane_alone() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let mut snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 900).session_local();
        cache.save(&snapshot).unwrap();

        let mut first = test_pane_with_session("claude-first", Harness::Claude, "s1");
        first
            .tokens
            .insert("quota_provider".to_string(), "Claude".to_string());
        let mut second = test_pane_with_session("claude-second", Harness::Claude, "s2");
        second
            .tokens
            .insert("quota_provider".to_string(), "Claude".to_string());
        let panes = [first, second, test_pane("grok-working", Harness::Grok)];
        let grok = "grok-working".to_string();
        let mut settling = BTreeMap::new();

        snapshot.session_windows.insert(
            "s1".to_string(),
            vec![window(WindowKind::FiveHour, 40.0, 9_000)],
        );
        cache.save(&snapshot).unwrap();

        let affected = watch_pass_ids(
            &cache,
            &panes,
            &Provider::ALL,
            std::slice::from_ref(&grok),
            std::slice::from_ref(&grok),
            &mut settling,
            1_001,
        );
        assert!(affected.contains(&"claude-first".to_string()));
        assert!(!affected.contains(&"claude-second".to_string()));
    }
    #[test]
    fn omp_panes_keep_both_accounts_from_one_debounced_report() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        let target = BillingTarget::omp("anthropic");
        let mut usage = omp_provider::ProviderUsage::default();
        for (pin, used) in [("a", 20.0), ("b", 80.0)] {
            usage.accounts.push(omp_provider::AccountUsage {
                pin: Some(pin.to_string()),
                windows: vec![UsageWindow::new(WindowKind::Weekly, used, None).unwrap()],
                fetched_at_unix: 100,
            });
        }
        cache.save_omp_usage(&target, &usage).unwrap();
        cache.mark_refresh_target(&target, 100).unwrap();
        for (pin, expected) in [
            ("a", "7d 80%"),
            ("b", "7d 20%"),
            ("a", "7d 80%"),
            ("unknown", ""),
        ] {
            let evidence = OmpEvidence {
                paths: crate::omp::OmpPaths {
                    agent_dir: dir.path().into(),
                    sessions: dir.path().join("sessions"),
                },
                provider_id: "anthropic".to_string(),
                account_pin: Some(pin.to_string()),
            };
            let update = omp_quota_with_refresh(
                &cache,
                &target,
                &evidence,
                110,
                RowStyle::default(),
                false,
                |_, _, _, _| panic!("must not spawn once per account"),
            );
            assert!(
                matches!(update, Some(PaneQuotaUpdate::Replace(values)) if values.quota_week == expected)
            );
        }
    }

    #[test]
    fn legacy_statusline_mailbox_is_migrated_from_raw_session_evidence() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        let legacy = serde_json::json!({
            "snapshot": { "provider":"claude", "source":"claude-statusline", "fetched_at_unix":100,
                "windows": [{"kind":"weekly","used_percent":99.0,"remaining_percent":1.0}],
                "session_windows": {"other":[{"kind":"weekly","used_percent":99.0,"remaining_percent":1.0}]}
            },
            "payload": {"session_id":"current", "rate_limits":{"seven_day":{"used_percentage":20.0}}}
        });
        std::fs::write(
            dir.path().join("claude-statusline.observation.json"),
            legacy.to_string(),
        )
        .unwrap();
        let fetched = load_statusline_snapshot(&cache, Provider::Claude).unwrap();
        assert!(fetched.snapshot.session_quota_only);
        assert_eq!(
            fetched
                .snapshot
                .window(WindowKind::Weekly)
                .unwrap()
                .used_percent,
            20.0
        );
        assert!(fetched.snapshot.session_windows.is_empty());
        assert_eq!(fetched.session_id.as_deref(), Some("current"));
    }

    #[test]
    fn a_provider_below_the_threshold_is_warned_about_once_until_it_recovers() {
        let alert = LowQuotaAlert::parse("10").unwrap();
        let (warn, state) = low_quota_transitions(alert, &low(&[("Claude", 8)]), &[]);
        assert_eq!(warn, vec!["Claude".to_string()]);
        assert_eq!(state, vec!["Claude".to_string()]);

        // Still low: remembered, and silent.
        let (warn, state) = low_quota_transitions(alert, &low(&[("Claude", 3)]), &state);
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(state, vec!["Claude".to_string()]);

        // Recovered above the threshold: re-armed.
        let (warn, state) = low_quota_transitions(alert, &low(&[("Claude", 40)]), &state);
        assert!(warn.is_empty(), "{warn:?}");
        assert!(state.is_empty(), "{state:?}");

        let (warn, _) = low_quota_transitions(alert, &low(&[("Claude", 9)]), &state);
        assert_eq!(warn, vec!["Claude".to_string()]);
    }

    /// Closing the last pane of a provider must not re-arm its warning: the
    /// quota did not recover, the window into it just went away.
    #[test]
    fn a_provider_with_no_pane_in_this_pass_keeps_its_state() {
        let alert = LowQuotaAlert::parse("20").unwrap();
        let previous = vec!["Codex".to_string()];
        let (warn, state) = low_quota_transitions(alert, &low(&[("Claude", 90)]), &previous);
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(state, previous);
    }

    #[test]
    fn the_threshold_is_inclusive_and_off_never_warns() {
        let alert = LowQuotaAlert::parse("10").unwrap();
        let (warn, _) = low_quota_transitions(alert, &low(&[("Grok", 10)]), &[]);
        assert_eq!(warn, vec!["Grok".to_string()]);
        let (warn, _) = low_quota_transitions(alert, &low(&[("Grok", 11)]), &[]);
        assert!(warn.is_empty(), "{warn:?}");
        let (warn, _) = low_quota_transitions(LowQuotaAlert::OFF, &low(&[("Grok", 0)]), &[]);
        assert!(warn.is_empty(), "{warn:?}");
    }

    /// Several panes on one provider are one quota, so they are one warning,
    /// reported at the lowest headroom any of them saw.
    #[test]
    fn panes_sharing_a_provider_collapse_to_one_entry() {
        let tokens = |provider: &str, headroom: Option<u8>| {
            let mut values = MetadataTokens::unavailable(Provider::Claude, "test");
            values.quota_provider = provider.to_string();
            values.quota_headroom = headroom;
            PaneTokens {
                pane_id: format!("w1:{provider}{headroom:?}"),
                quota: PaneQuotaUpdate::Replace(Box::new(values)),
                identity: None,
                context: None,
                show_account_quota: true,
            }
        };
        let lowest = lowest_headroom_by_provider(&[
            tokens("Claude", Some(40)),
            tokens("Claude", Some(12)),
            tokens("Codex", None),
        ]);
        assert_eq!(lowest, low(&[("Claude", 12)]));
    }

    fn quota_tokens(pane_id: &str, provider: &str, headroom: Option<u8>) -> PaneTokens {
        let mut values = MetadataTokens::unavailable(Provider::Claude, "test");
        values.quota_provider = provider.to_string();
        values.quota_headroom = headroom;
        if headroom.is_some() {
            values.quota_week = "7d 12%".to_string();
        }
        PaneTokens {
            pane_id: pane_id.to_string(),
            quota: PaneQuotaUpdate::Replace(Box::new(values)),
            identity: None,
            context: None,
            show_account_quota: true,
        }
    }

    #[test]
    fn one_vendor_quota_row_stays_on_the_stable_pane() {
        let mut tokens = vec![
            quota_tokens("w1:p1", "Grok", Some(87)),
            quota_tokens("w1:p2", "Grok", Some(87)),
            quota_tokens("w1:p3", "Claude", Some(40)),
        ];
        let mut panes = vec![
            test_pane("w1:p1", Harness::Grok),
            test_pane("w1:p2", Harness::Grok),
            test_pane("w1:p3", Harness::Claude),
        ];
        panes[1].focused = true;
        mark_one_quota_row_per_vendor(&mut tokens, &panes);
        assert!(tokens[0].show_account_quota);
        assert!(!tokens[1].show_account_quota);
        assert!(tokens[2].show_account_quota);
    }

    #[test]
    fn focusing_another_tab_does_not_move_the_vendor_quota_row() {
        let mut tokens = vec![
            quota_tokens("w1:p1", "Grok", Some(87)),
            quota_tokens("w1:p2", "Grok", Some(87)),
        ];
        let mut panes = vec![
            test_pane("w1:p1", Harness::Grok),
            test_pane("w1:p2", Harness::Grok),
        ];
        panes[1].status = AgentStatus::Working;
        panes[1].focused = true;
        mark_one_quota_row_per_vendor(&mut tokens, &panes);
        assert!(tokens[0].show_account_quota);
        assert!(!tokens[1].show_account_quota);
    }

    #[test]
    fn a_lone_vendor_pane_keeps_its_quota_row() {
        let mut tokens = vec![quota_tokens("w1:p1", "Grok", Some(87))];
        let panes = vec![test_pane("w1:p1", Harness::Grok)];
        mark_one_quota_row_per_vendor(&mut tokens, &panes);
        assert!(tokens[0].show_account_quota);
    }

    #[test]
    fn claude_panes_keep_their_own_quota_rows() {
        let mut tokens = vec![
            quota_tokens("w1:p1", "Claude", Some(40)),
            quota_tokens("w1:p2", "Claude", Some(12)),
        ];
        let panes = vec![
            test_pane("w1:p1", Harness::Claude),
            test_pane("w1:p2", Harness::Claude),
        ];
        mark_one_quota_row_per_vendor(&mut tokens, &panes);
        assert!(tokens[0].show_account_quota);
        assert!(tokens[1].show_account_quota);
    }

    #[test]
    fn opencode_go_and_opencode_share_one_vendor_row() {
        let mut go = quota_tokens("w1:p1", "OpenCode Go", Some(40));
        go.identity = Some(crate::herdr::PaneIdentity {
            provider: "OpenCode Go".to_string(),
            model: "kimi-k2.5".to_string(),
        });
        let mut tokens = vec![go, quota_tokens("w1:p2", "OpenCode", Some(40))];
        let mut panes = vec![
            test_pane("w1:p1", Harness::OpenCode),
            test_pane("w1:p2", Harness::OpenCode),
        ];
        panes[1].focused = true;
        mark_one_quota_row_per_vendor(&mut tokens, &panes);
        assert!(tokens[0].show_account_quota);
        assert!(!tokens[1].show_account_quota);
    }

    #[test]
    fn each_space_keeps_its_own_vendor_row() {
        let mut other = test_pane("w9:p1", Harness::Grok);
        other.workspace_id = "w9".to_string();
        let mut tokens = vec![
            quota_tokens("w1:p1", "Grok", Some(87)),
            quota_tokens("w9:p1", "Grok", Some(87)),
        ];
        let mut panes = vec![test_pane("w1:p1", Harness::Grok), other];
        panes[0].focused = true;
        mark_one_quota_row_per_vendor(&mut tokens, &panes);
        assert!(tokens[0].show_account_quota);
        assert!(tokens[1].show_account_quota);
    }

    #[test]
    fn a_stale_same_space_sibling_is_queued_to_drop_windows() {
        let mut focused = test_pane("w5:pA", Harness::Grok);
        focused.focused = true;
        focused.workspace_id = "w5".to_string();
        let mut extra = test_pane("w5:pD", Harness::Grok);
        extra.workspace_id = "w5".to_string();
        extra
            .tokens
            .insert("quota_week_inline_normal".to_string(), "7d 54%".to_string());
        let tokens = vec![quota_tokens("w5:pA", "Grok", Some(54))];
        let extras = vendor_row_sync_extras(&tokens, &[focused, extra]);
        assert_eq!(extras.len(), 1, "{extras:?}");
        assert_eq!(extras[0].pane_id, "w5:pD");
        assert!(!extras[0].show_account_quota);
    }

    #[test]
    fn a_flat_representative_is_queued_when_a_sibling_joins() {
        let mut head = test_pane("w9:p1", Harness::Cursor);
        head.workspace_id = "w9".to_string();
        head.tokens
            .insert("quota_icon".to_string(), "x".to_string());
        head.tokens.insert(
            "quota_provider_model".to_string(),
            "Cursor/default".to_string(),
        );
        head.tokens
            .insert("quota_week_danger".to_string(), "7d 0%".to_string());
        let mut child = test_pane("w9:p7", Harness::Cursor);
        child.workspace_id = "w9".to_string();
        let tokens = vec![quota_tokens("w9:p7", "Cursor", Some(0))];
        let extras = vendor_row_sync_extras(&tokens, &[head, child]);
        assert_eq!(extras.len(), 1, "{extras:?}");
        assert_eq!(extras[0].pane_id, "w9:p1");
        assert!(extras[0].show_account_quota);
    }

    #[test]
    fn a_nested_head_with_share_windows_is_not_queued_from_a_child_event() {
        let mut head = test_pane("w5:pA", Harness::Grok);
        head.workspace_id = "w5".to_string();
        head.tokens.insert(
            "quota_share_week_inline_normal".to_string(),
            "7d 54%".to_string(),
        );
        let mut child = test_pane("w5:pD", Harness::Grok);
        child.workspace_id = "w5".to_string();
        child.focused = true;
        let tokens = vec![quota_tokens("w5:pD", "Grok", Some(54))];
        let extras = vendor_row_sync_extras(&tokens, &[head, child]);
        assert!(
            extras.iter().all(|extra| extra.pane_id != "w5:pA"),
            "share tokens already count as account windows: {extras:?}"
        );
    }

    #[test]
    fn vendor_sync_does_not_queue_a_different_vendor() {
        let mut grok = test_pane("w5:pA", Harness::Grok);
        grok.workspace_id = "w5".to_string();
        let mut extra_grok = test_pane("w5:pD", Harness::Grok);
        extra_grok.workspace_id = "w5".to_string();
        extra_grok
            .tokens
            .insert("quota_week_inline_normal".to_string(), "7d 54%".to_string());
        let mut codex = test_pane("w5:pB", Harness::Codex);
        codex.workspace_id = "w5".to_string();
        let tokens = vec![quota_tokens("w5:pA", "Grok", Some(54))];
        let extras = vendor_row_sync_extras(&tokens, &[grok, extra_grok, codex]);
        assert_eq!(extras.len(), 1, "{extras:?}");
        assert_eq!(extras[0].pane_id, "w5:pD");
    }

    #[test]
    fn replaced_watch_binary_is_detected() {
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        assert!(watch_binary_is_newer(
            started,
            Some(started + Duration::from_secs(1))
        ));
        assert!(!watch_binary_is_newer(
            started,
            Some(started - Duration::from_secs(1))
        ));
        assert!(!watch_binary_is_newer(started, None));
    }

    #[test]
    fn successful_snapshot_is_kept_when_provider_refresh_fails() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let snapshot = ProviderSnapshot::new(
            Provider::Grok,
            vec![UsageWindow::new(WindowKind::Weekly, 42.5, None).unwrap()],
            1,
        );
        cache.save(&snapshot).unwrap();
        assert_eq!(cache.load(Provider::Grok).unwrap(), Some(snapshot));
    }

    /// Both legs of the shape have to be live: the layout comes from the
    /// state-dir cache the publish hooks can see, the meter size from the
    /// width Herdr is actually rendering.
    #[test]
    fn the_sidebar_shape_carries_both_the_chosen_layout_and_the_rendered_width() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path().join("cache"));
        cache.set_sidebar_layout(SidebarLayout::Gauges).unwrap();
        let state = directory.path().join("state");
        let shell = state.join("herdr/client-shell");
        std::fs::create_dir_all(&shell).unwrap();
        let absent_config = directory.path().join("absent.toml");
        for (width, cells) in [(26, 6), (35, 12)] {
            std::fs::write(
                shell.join("local-82d9e482d8820ee2.json"),
                format!("{{\"sidebar_width\": {width}}}"),
            )
            .unwrap();
            crate::prefs::testing::with_env(
                &[
                    ("HERDR_CONFIG_FILE", Some(absent_config.as_os_str())),
                    ("XDG_STATE_HOME", Some(state.as_os_str())),
                    (
                        "HERDR_SOCKET_PATH",
                        Some(std::ffi::OsStr::new("/test/herdr.sock")),
                    ),
                ],
                || {
                    let shape = sidebar_shape(&cache);
                    assert_eq!(shape.layout, SidebarLayout::Gauges);
                    assert_eq!(shape.meter_cells, Some(cells), "width {width}");
                },
            );
        }
    }

    #[test]
    fn missing_snapshot_does_not_overwrite_sidebar_with_unavailable() {
        let values = tokens_for_provider(None, 1, None, RowStyle::default());
        assert!(values.is_none());
    }

    /// The publish path reads the layout from the state dir. A layout on its
    /// own carries no meter, so a cache that has one recorded still publishes
    /// exactly what an empty cache publishes.
    #[test]
    fn a_recorded_sidebar_layout_does_not_change_a_published_token() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let snapshot = crate::model::ProviderSnapshot::new(
            Provider::Claude,
            vec![crate::model::UsageWindow::new(
                WindowKind::FiveHour,
                58.0,
                Some(crate::model::ResetAt::from_unix_seconds(14_820)),
            )
            .unwrap()],
            0,
        );
        let unset = tokens_for_provider(
            Some(&snapshot),
            0,
            None,
            RowStyle::new(
                PercentStyle::default(),
                cache.sidebar_layout().unwrap_or_default().into(),
            ),
        );
        cache.set_sidebar_layout(SidebarLayout::Stacked).unwrap();
        let stacked = tokens_for_provider(
            Some(&snapshot),
            0,
            None,
            RowStyle::new(
                PercentStyle::default(),
                cache.sidebar_layout().unwrap_or_default().into(),
            ),
        );
        assert_eq!(cache.sidebar_layout(), Some(SidebarLayout::Stacked));
        assert_eq!(unset, stacked);
        assert_eq!(stacked.unwrap().quota_5h, "5h 42% 4h07m");
    }

    #[test]
    fn publish_row_reads_the_persisted_sidebar_pacing_choice() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        cache
            .set_sidebar_pacing(crate::cli::SidebarPacing::On)
            .unwrap();
        assert_eq!(publish_row(&cache).pacing, crate::cli::SidebarPacing::On);
    }

    #[test]
    fn an_omp_oauth_account_without_usage_is_explicit_on_the_first_fetch() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let target = BillingTarget::omp("anthropic");
        let evidence = crate::omp::OmpEvidence {
            paths: crate::omp::OmpPaths {
                agent_dir: directory.path().join(".omp/agent"),
                sessions: directory.path().join(".omp/agent/sessions"),
            },
            provider_id: "anthropic".to_string(),
            account_pin: Some("account-pin".to_string()),
        };
        let update = omp_quota_with_refresh(
            &cache,
            &target,
            &evidence,
            100,
            RowStyle::default(),
            false,
            |_, _, _, _| OmpUsage::Unavailable,
        )
        .expect("explicit unavailable update");
        let PaneQuotaUpdate::Replace(values) = update else {
            panic!("expected replacement");
        };
        assert_eq!(values.quota_week, "");
        assert_eq!(
            values.quota_error.as_deref(),
            Some("omp reported no quota data")
        );
    }

    #[test]
    fn an_omp_failed_first_fetch_is_debounced_without_a_snapshot() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let target = BillingTarget::omp("anthropic");
        cache.mark_refresh_target(&target, 100).unwrap();
        let evidence = crate::omp::OmpEvidence {
            paths: crate::omp::OmpPaths {
                agent_dir: directory.path().join(".omp/agent"),
                sessions: directory.path().join(".omp/agent/sessions"),
            },
            provider_id: "anthropic".to_string(),
            account_pin: Some("account-pin".to_string()),
        };
        let update = omp_quota_with_refresh(
            &cache,
            &target,
            &evidence,
            120,
            RowStyle::default(),
            false,
            |_, _, _, _| panic!("debounced refresh must not run"),
        );
        assert!(
            matches!(update, Some(PaneQuotaUpdate::Replace(values)) if values.quota_error.is_some())
        );
    }

    #[test]
    fn an_expired_omp_window_bypasses_the_fetch_debounce() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let target = BillingTarget::omp("anthropic");
        cache
            .save_target(
                &target,
                &ProviderSnapshot::new(
                    Provider::Claude,
                    vec![window(WindowKind::FiveHour, 96.0, 1_000)],
                    900,
                )
                .with_account_id(Some("account-pin".to_string())),
            )
            .unwrap();
        cache.mark_refresh_target(&target, 980).unwrap();
        let evidence = crate::omp::OmpEvidence {
            paths: crate::omp::OmpPaths {
                agent_dir: directory.path().join(".omp/agent"),
                sessions: directory.path().join(".omp/agent/sessions"),
            },
            provider_id: "anthropic".to_string(),
            account_pin: Some("account-pin".to_string()),
        };
        let update = omp_quota_with_refresh(
            &cache,
            &target,
            &evidence,
            1_001,
            RowStyle::default(),
            false,
            |_, _, _, _| {
                OmpUsage::Account(Box::new(
                    ProviderSnapshot::new(
                        Provider::Claude,
                        vec![window(WindowKind::FiveHour, 0.0, 2_000)],
                        1_001,
                    )
                    .with_account_id(Some("account-pin".to_string())),
                ))
            },
        )
        .expect("refreshed update");
        let PaneQuotaUpdate::Replace(values) = update else {
            panic!("expected replacement");
        };
        assert!(
            values.quota_5h.starts_with("5h 100%"),
            "{}",
            values.quota_5h
        );
    }

    #[test]
    fn an_omp_usage_failure_keeps_the_same_accounts_last_good_snapshot() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let target = BillingTarget::omp("anthropic");
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![UsageWindow::new(WindowKind::Weekly, 42.0, None).unwrap()],
            90,
        )
        .with_account_id(Some("account-pin".to_string()));
        cache.save_target(&target, &snapshot).unwrap();
        let evidence = crate::omp::OmpEvidence {
            paths: crate::omp::OmpPaths {
                agent_dir: directory.path().join(".omp/agent"),
                sessions: directory.path().join(".omp/agent/sessions"),
            },
            provider_id: "anthropic".to_string(),
            account_pin: Some("account-pin".to_string()),
        };
        let update = omp_quota_with_refresh(
            &cache,
            &target,
            &evidence,
            200,
            RowStyle::default(),
            false,
            |_, _, _, _| OmpUsage::Unavailable,
        )
        .expect("last good update");
        let PaneQuotaUpdate::Replace(values) = update else {
            panic!("expected replacement");
        };
        assert_eq!(values.quota_week, "7d 58%");
        assert_eq!(values.quota_error, None);
    }

    #[test]
    fn other_account_snapshot_is_not_shown_as_the_current_quota() {
        let snapshot = ProviderSnapshot::new(
            Provider::Grok,
            vec![UsageWindow::new(WindowKind::Weekly, 100.0, None).unwrap()],
            1,
        )
        .with_account_id(Some("old-account".to_string()));
        let values = tokens_for_loaded_snapshot(
            Provider::Grok,
            Some(&snapshot),
            None,
            1,
            None,
            RowStyle::default(),
        )
        .unwrap();
        assert_eq!(values.quota_week, "");
        assert_eq!(values.quota_week_severity, None);
        assert_eq!(values.quota_month, "");
        assert_eq!(
            values.quota_error.as_deref(),
            Some("signed-in account changed")
        );
        // A failure must not masquerade as a lapsed prompt cache.
        assert_eq!(values.quota_cache_state, "");
    }

    #[test]
    fn a_monthly_snapshot_for_the_wrong_account_omits_window_rows() {
        let snapshot = ProviderSnapshot::new(
            Provider::Cursor,
            vec![UsageWindow::new(WindowKind::Monthly, 7.0, None).unwrap()],
            1,
        )
        .with_account_id(Some("old-account".to_string()));
        let values = tokens_for_loaded_snapshot(
            Provider::Cursor,
            Some(&snapshot),
            None,
            1,
            None,
            RowStyle::default(),
        )
        .unwrap();
        assert_eq!(values.quota_month, "");
        assert_eq!(values.quota_month_severity, None);
        assert_eq!(values.quota_week, "");
        assert_eq!(
            values.quota_error.as_deref(),
            Some("signed-in account changed")
        );
    }

    #[test]
    fn an_expired_cached_window_bypasses_the_fetch_debounce() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        for provider in Provider::ALL
            .into_iter()
            .chain(std::iter::once(Provider::OpenCodeGo))
        {
            cache
                .save(
                    &ProviderSnapshot::new(
                        provider,
                        vec![
                            window(WindowKind::FiveHour, 96.0, 1_000),
                            window(WindowKind::Weekly, 48.0, 10_000),
                        ],
                        900,
                    )
                    .with_account_id(Some("acc".into())),
                )
                .unwrap();
            cache
                .mark_refresh_account(provider, 980, Some("acc"))
                .unwrap();
            assert!(
                !should_skip_fetch_for_account(&cache, provider, false, 1_001, Some("acc"), None)
                    .unwrap(),
                "{}: a window that has already reset must be fetched inside debounce",
                provider.source()
            );
            cache
                .save(
                    &ProviderSnapshot::new(
                        provider,
                        vec![
                            window(WindowKind::FiveHour, 4.0, 2_000),
                            window(WindowKind::Weekly, 48.0, 10_000),
                        ],
                        1_001,
                    )
                    .with_account_id(Some("acc".into())),
                )
                .unwrap();
            cache
                .mark_refresh_account(provider, 1_001, Some("acc"))
                .unwrap();
            assert!(
                should_skip_fetch_for_account(&cache, provider, false, 1_030, Some("acc"), None)
                    .unwrap(),
                "{}: a still-current window must keep the debounce",
                provider.source()
            );
            cache
                .save(
                    &ProviderSnapshot::new(
                        provider,
                        vec![UsageWindow::new(WindowKind::Weekly, 48.0, None).unwrap()],
                        1_001,
                    )
                    .with_account_id(Some("acc".into())),
                )
                .unwrap();
            assert!(
                should_skip_fetch_for_account(&cache, provider, false, 1_030, Some("acc"), None)
                    .unwrap(),
                "{}: a window without a reset time cannot be proved expired",
                provider.source()
            );
        }
    }

    #[test]
    fn debounce_does_not_keep_another_accounts_grok_snapshot() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let snapshot = ProviderSnapshot::new(
            Provider::Grok,
            vec![UsageWindow::new(WindowKind::Weekly, 100.0, None).unwrap()],
            1,
        )
        .with_account_id(Some("old-account".to_string()));
        cache.save(&snapshot).unwrap();
        cache.mark_refresh(Provider::Grok, 100).unwrap();
        assert!(
            !should_skip_fetch(&cache, Provider::Grok, false, 120).unwrap(),
            "a snapshot for another Grok login must be fetched even inside the debounce window"
        );
    }

    // Reading a pane repaints it, which visibly scrolls the agent's terminal.
    // An event must name exactly one pane to read, so the other panes of the
    // same provider are left alone.
    #[test]
    fn unknown_and_opencode_events_select_no_collectors() {
        // `event` reads the agent name straight off the payload, so this is
        // the exact chain that decides whether a watch may start.
        fn collector(payload: &str) -> Option<Provider> {
            let value: Value = serde_json::from_str(payload).unwrap();
            let agent = find_agent(&value)?;
            Harness::from_agent_name(agent)?.billing()
        }

        assert_eq!(
            collector(
                r#"{"event":"pane_agent_status_changed",
                    "data":{"pane_id":"w1:p9","agent":"opencode","status":"working"}}"#
            ),
            None
        );
        assert_eq!(
            collector(r#"{"data":{"agent":"OpenCode","status":"working"}}"#),
            None
        );
        assert_eq!(
            collector(r#"{"data":{"agent":"cursor","status":"working"}}"#),
            Some(Provider::Cursor)
        );
        assert_eq!(
            collector(r#"{"data":{"agent":"amp","status":"working"}}"#),
            None
        );
        assert_eq!(
            collector(r#"{"data":{"agent":"claude-code","pane_id":"w1:p1"}}"#),
            Some(Provider::Claude)
        );
    }

    #[test]
    fn event_payload_names_the_single_pane_whose_topic_may_be_read() {
        let value: Value = serde_json::from_str(
            r#"{"event":"pane_agent_status_changed",
                "data":{"pane_id":"w1:p2","agent":"grok","status":"working"}}"#,
        )
        .unwrap();
        assert_eq!(find_pane_id(&value), Some("w1:p2"));
        assert_eq!(find_agent(&value), Some("grok"));
    }

    #[test]
    fn an_event_without_a_pane_reads_no_pane_at_all() {
        let value: Value =
            serde_json::from_str(r#"{"event":"x","data":{"agent":"claude"}}"#).unwrap();
        assert_eq!(find_pane_id(&value), None);
    }

    #[test]
    fn status_events_start_pulses_only_for_working_turns() {
        let working: Value =
            serde_json::from_str(r#"{"data":{"agent":"codex","agent_status":"working"}}"#).unwrap();
        let idle: Value =
            serde_json::from_str(r#"{"data":{"agent":"codex","status":"idle"}}"#).unwrap();
        assert!(find_status(&working).is_some_and(is_working_status));
        assert_eq!(find_status(&idle), Some("idle"));
    }

    #[test]
    fn workspace_focus_events_name_the_space_without_a_pane() {
        let value: Value = serde_json::from_str(
            r#"{"event":"workspace_focused","data":{"type":"workspace_focused","workspace_id":"w9"}}"#,
        )
        .unwrap();
        assert_eq!(find_pane_id(&value), None);
    }

    #[test]
    fn unfocused_idle_after_working_becomes_unseen_teal() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        let mut pane = test_pane("w1:p2", Harness::Cursor);
        pane.status = AgentStatus::Idle;
        pane.focused = false;
        pane.tokens
            .insert("quota_icon_working".into(), "yellow".into());

        apply_icon_attention(std::slice::from_mut(&mut pane), &cache, None, false, false).unwrap();
        assert_eq!(pane.status, AgentStatus::Done);
        assert_eq!(pane.icon_status(), AgentStatus::Done);
        assert!(cache.icon_attention().unseen.contains("w1:p2"));
        assert!(!cache.icon_attention().working.contains("w1:p2"));
    }

    #[test]
    fn working_set_survives_lost_working_icon() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        let mut working = test_pane("w1:p2", Harness::Cursor);
        working.status = AgentStatus::Working;
        apply_icon_attention(
            std::slice::from_mut(&mut working),
            &cache,
            None,
            false,
            false,
        )
        .unwrap();
        assert!(cache.icon_attention().working.contains("w1:p2"));

        // Production trap: watch painted idle white and cleared the yellow
        // twin before the completion event ran. The working set must still
        // force teal.
        let mut idle = test_pane("w1:p2", Harness::Cursor);
        idle.status = AgentStatus::Idle;
        idle.focused = false;
        idle.tokens.insert("quota_icon".into(), "white".into());
        apply_icon_attention(std::slice::from_mut(&mut idle), &cache, None, false, false).unwrap();
        assert_eq!(idle.status, AgentStatus::Done);
        assert_eq!(idle.icon_status(), AgentStatus::Done);
        assert!(cache.icon_attention().unseen.contains("w1:p2"));
    }

    #[test]
    fn focused_completion_waits_for_focus_event() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        let mut pane = test_pane("w1:p1", Harness::Cursor);
        pane.status = AgentStatus::Idle;
        pane.focused = true;
        pane.tokens
            .insert("quota_icon_working".into(), "yellow".into());

        apply_icon_attention(std::slice::from_mut(&mut pane), &cache, None, false, false).unwrap();
        assert_eq!(pane.status, AgentStatus::Done);
        assert_eq!(pane.icon_status(), AgentStatus::Done);
        assert!(cache.icon_attention().unseen.contains("w1:p1"));
    }

    #[test]
    fn completion_stays_teal_until_a_later_focus_event() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        let mut pane = test_pane("w1:p1", Harness::Cursor);
        pane.status = AgentStatus::Working;
        pane.focused = true;
        apply_icon_attention(std::slice::from_mut(&mut pane), &cache, None, false, false).unwrap();
        assert_eq!(pane.icon_status(), AgentStatus::Working);

        // A turn can finish in the pane that is already focused. Completion
        // is still unseen until a subsequent pane.focused acknowledgement.
        pane.status = AgentStatus::Idle;
        apply_icon_attention(std::slice::from_mut(&mut pane), &cache, None, false, false).unwrap();
        assert_eq!(pane.icon_status(), AgentStatus::Done);

        // Inventory refreshes must preserve that green state.
        pane.status = AgentStatus::Idle;
        apply_icon_attention(std::slice::from_mut(&mut pane), &cache, None, true, false).unwrap();
        assert_eq!(pane.icon_status(), AgentStatus::Done);

        apply_icon_attention(
            std::slice::from_mut(&mut pane),
            &cache,
            Some("w1:p1"),
            false,
            false,
        )
        .unwrap();
        assert_eq!(pane.icon_status(), AgentStatus::Idle);

        // Herdr may briefly return its old done status after the focus hook.
        pane.status = AgentStatus::Done;
        apply_icon_attention(std::slice::from_mut(&mut pane), &cache, None, false, false).unwrap();
        assert_eq!(pane.icon_status(), AgentStatus::Idle);

        pane.status = AgentStatus::Idle;
        pane.tokens
            .insert("quota_icon_working".into(), "stale yellow".into());
        apply_icon_attention(std::slice::from_mut(&mut pane), &cache, None, false, false).unwrap();
        assert_eq!(pane.icon_status(), AgentStatus::Idle);

        // A later, explicit working event is a new turn and restores yellow.
        pane.status = AgentStatus::Working;
        apply_icon_attention(std::slice::from_mut(&mut pane), &cache, None, false, false).unwrap();
        assert_eq!(pane.icon_status(), AgentStatus::Idle);
        pane.status = AgentStatus::Working;
        apply_icon_attention(std::slice::from_mut(&mut pane), &cache, None, false, true).unwrap();
        assert_eq!(pane.icon_status(), AgentStatus::Working);
    }

    #[test]
    fn plain_unfocused_idle_does_not_become_teal() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        let mut pane = test_pane("w1:p3", Harness::Grok);
        pane.status = AgentStatus::Idle;
        pane.focused = false;
        apply_icon_attention(std::slice::from_mut(&mut pane), &cache, None, false, false).unwrap();
        assert_eq!(pane.status, AgentStatus::Idle);
        assert_eq!(pane.icon_status(), AgentStatus::Idle);
        assert!(cache.icon_attention().unseen.is_empty());
    }

    #[test]
    fn single_pane_update_does_not_drop_sibling_unseen() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        let mut other = test_pane("w1:p9", Harness::Grok);
        other.status = AgentStatus::Idle;
        other.focused = false;
        other
            .tokens
            .insert("quota_icon_working".into(), "yellow".into());
        apply_icon_attention(std::slice::from_mut(&mut other), &cache, None, false, false).unwrap();
        assert!(cache.icon_attention().unseen.contains("w1:p9"));

        let mut pane = test_pane("w1:p1", Harness::Cursor);
        pane.status = AgentStatus::Working;
        apply_icon_attention(std::slice::from_mut(&mut pane), &cache, None, false, false).unwrap();
        assert!(
            cache.icon_attention().unseen.contains("w1:p9"),
            "event for another pane must not drop sibling unseen"
        );
        assert!(cache.icon_attention().working.contains("w1:p1"));
    }

    #[test]
    fn force_seen_clears_unseen_even_when_inventory_is_unfocused() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        let mut pane = test_pane("w1:p1", Harness::Cursor);
        pane.status = AgentStatus::Idle;
        pane.focused = false;
        pane.tokens.insert("quota_icon_done".into(), "teal".into());
        let mut attention = cache.icon_attention();
        attention.unseen.insert("w1:p1".into());
        cache.set_icon_attention(&attention).unwrap();

        apply_icon_attention(
            std::slice::from_mut(&mut pane),
            &cache,
            Some("w1:p1"),
            false,
            false,
        )
        .unwrap();
        assert!(pane.focused);
        assert_eq!(pane.status, AgentStatus::Idle);
        assert_eq!(pane.icon_status(), AgentStatus::Idle);
        assert!(!cache.icon_attention().unseen.contains("w1:p1"));
    }

    #[test]
    fn focus_change_uses_last_focused_and_keeps_other_green_panes() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        let mut previous = test_pane("w1:p1", Harness::Cursor);
        previous.focused = true;
        previous.status = AgentStatus::Working;
        apply_icon_attention(
            std::slice::from_mut(&mut previous),
            &cache,
            None,
            false,
            false,
        )
        .unwrap();
        assert_eq!(
            cache.icon_attention().last_focused.as_deref(),
            Some("w1:p1")
        );
        previous.status = AgentStatus::Idle;
        previous
            .tokens
            .insert("quota_icon_working".into(), "yellow".into());
        apply_icon_attention(
            std::slice::from_mut(&mut previous),
            &cache,
            None,
            false,
            false,
        )
        .unwrap();
        assert_eq!(previous.status, AgentStatus::Done);

        previous.focused = false;
        previous.tokens.remove("quota_icon_working");
        previous
            .tokens
            .insert("quota_icon_done".into(), "green".into());
        let mut next = test_pane("w2:p2", Harness::Grok);
        next.focused = true;
        let mut unrelated = test_pane("w1:p3", Harness::Codex);
        unrelated.status = AgentStatus::Done;
        unrelated
            .tokens
            .insert("quota_icon_done".into(), "green".into());
        let mut panes = [previous, next, unrelated];
        apply_icon_attention(&mut panes, &cache, Some("w2:p2"), false, false).unwrap();
        assert_eq!(panes[0].status, AgentStatus::Idle);
        assert_eq!(panes[2].status, AgentStatus::Done);
        assert_eq!(
            cache.icon_attention().last_focused.as_deref(),
            Some("w2:p2")
        );
    }

    #[test]
    fn focused_working_event_records_the_pane_before_its_completion() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        let mut attention = cache.icon_attention();
        attention.last_focused = Some("w1:p1".into());
        cache.set_icon_attention(&attention).unwrap();

        let mut pane = test_pane("w1:p2", Harness::Cursor);
        pane.focused = true;
        pane.status = AgentStatus::Working;
        apply_icon_attention(std::slice::from_mut(&mut pane), &cache, None, false, true).unwrap();
        assert_eq!(
            cache.icon_attention().last_focused.as_deref(),
            Some("w1:p2")
        );

        pane.status = AgentStatus::Idle;
        apply_icon_attention(std::slice::from_mut(&mut pane), &cache, None, false, false).unwrap();
        assert_eq!(pane.status, AgentStatus::Done);
        assert!(cache.icon_attention().unseen.contains("w1:p2"));
        assert_eq!(
            cache.icon_attention().last_focused.as_deref(),
            Some("w1:p2")
        );
    }

    #[test]
    fn unfocused_done_keeps_teal_icon_status() {
        let mut pane = test_pane("w1:p2", Harness::Grok);
        pane.status = AgentStatus::Done;
        pane.focused = false;
        assert_eq!(
            pane.icon_status(),
            AgentStatus::Done,
            "unfocused completion must stay teal until focused"
        );

        pane.focused = true;
        assert_eq!(
            pane.icon_status(),
            AgentStatus::Done,
            "inventory focus alone does not acknowledge a completion"
        );

        pane.focused = false;
        pane.status = AgentStatus::Idle;
        pane.tokens.insert("quota_icon_done".into(), "teal".into());
        assert_eq!(
            pane.icon_status(),
            AgentStatus::Idle,
            "a leftover done token must not restore teal"
        );
    }

    #[test]
    fn hydrate_done_tokens_only_when_attention_file_is_missing() {
        let dir = tempdir().unwrap();
        let cache = CacheStore::new(dir.path());
        let mut pane = test_pane("w1:p2", Harness::Cursor);
        pane.status = AgentStatus::Idle;
        pane.focused = false;
        pane.tokens.insert("quota_icon_done".into(), "teal".into());
        apply_icon_attention(std::slice::from_mut(&mut pane), &cache, None, false, false).unwrap();
        assert_eq!(pane.status, AgentStatus::Done);
        assert!(cache.icon_attention().unseen.contains("w1:p2"));

        pane.status = AgentStatus::Idle;
        apply_icon_attention(
            std::slice::from_mut(&mut pane),
            &cache,
            Some("w1:p2"),
            false,
            false,
        )
        .unwrap();
        assert_eq!(pane.status, AgentStatus::Idle);
        assert!(!cache.icon_attention().unseen.contains("w1:p2"));

        pane.focused = false;
        pane.status = AgentStatus::Idle;
        pane.tokens.insert("quota_icon_done".into(), "stale".into());
        apply_icon_attention(std::slice::from_mut(&mut pane), &cache, None, false, false).unwrap();
        assert_eq!(
            pane.status,
            AgentStatus::Idle,
            "after mark-seen, stale done token must not come back"
        );
        assert!(!cache.icon_attention().unseen.contains("w1:p2"));
    }

    #[test]
    fn stale_done_icon_keeps_the_watcher_alive_until_cleared() {
        let mut pane = test_pane("w1:p1", Harness::Cursor);
        pane.status = AgentStatus::Done;
        pane.focused = false;
        pane.tokens.insert("quota_icon_done".into(), "teal".into());
        assert!(
            !has_stale_done_icon(&pane),
            "unfocused done is intentional teal, not stale"
        );

        pane.focused = true;
        assert!(has_stale_done_icon(&pane), "focused done must be cleared");

        pane.focused = false;
        pane.status = AgentStatus::Idle;
        assert!(
            !has_stale_done_icon(&pane),
            "unfocused idle+done_token stays until focus"
        );
    }
}
