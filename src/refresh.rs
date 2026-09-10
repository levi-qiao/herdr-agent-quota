use crate::cache::CacheStore;
use crate::cli::{AgentSelection, LowQuotaAlert, PercentStyle};
use crate::herdr::{
    current_focused_pane, list_agent_panes, list_agent_state, plugin_quota_present,
    publish_pane_tokens, refresh_pane_topic, AgentPane, PaneQuotaUpdate, PaneTokens,
};
use crate::model::{
    BillingTarget, CredentialScope, Harness, Provider, ProviderSnapshot, Resolution,
};
use crate::omp::OmpEvidence;
use crate::opencode::OpenCodePaths;
use crate::presentation::MetadataTokens;
use crate::providers::statusline::enrich_cache_session;
use crate::providers::{codex, devin, grok, omp as omp_provider, opencode_go};
use crate::route;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
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
/// Only the agent order is restored, and only when it is this plugin's to
/// restore: a `default` order owns no Herdr view, so startup has nothing to
/// put back and must not spend a socket call saying so.
pub fn startup(providers: &[Provider]) -> Result<()> {
    if let Ok(cache) = CacheStore::from_env() {
        let order = crate::configure::resolved_agent_order(None, Some(&cache));
        if order.is_quota() {
            crate::configure::apply_agent_order(order);
        }
    }
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
        let _ = refresh_working_panes(&cache, &state.panes, &affected);
        if active.is_empty() && settling.is_empty() {
            break;
        }
        previous_active = active;
        if started.elapsed() >= MAX_ACTIVE_TURN_WATCH {
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

/// Working and settling panes, plus idle panes whose displayed quota has lapsed.
///
/// An expired window is no longer a live reading. Including those pane ids in
/// an already-running watch pass rewrites the sidebar after a reset without
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
    for pane in panes {
        if !pane_in_watch_scope(pane, providers) || affected.contains(&pane.pane_id) {
            continue;
        }
        if cached_quota_has_expired(cache, pane, now) {
            affected.push(pane.pane_id.clone());
        }
    }
    affected
}

fn cached_quota_has_expired(cache: &CacheStore, pane: &AgentPane, now: u64) -> bool {
    let Resolution::Subscription(target) = route::resolve(pane) else {
        return false;
    };
    cache
        .load_target(&target)
        .ok()
        .flatten()
        .is_some_and(|snapshot| {
            snapshot.displayed_quota_has_expired(
                pane.session.as_ref().and_then(|session| session.id()),
                now,
            )
        })
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
        thread::sleep(
            Duration::from_secs(1).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

/// Refresh working, settling, or expired-quota targets, then publish that
/// account reading to its siblings. Local transcript routing does not read
/// terminal output or poll unrelated subscriptions.
fn refresh_working_panes(
    cache: &CacheStore,
    panes: &[AgentPane],
    affected: &[String],
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
        affected.contains(&pane.pane_id) || matches!(resolution, Resolution::Subscription(target) if targets.contains(target))
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
    publish_resolved(cache, &mut selected, None, false)
}

fn run_internal(
    providers: &[Provider],
    force: bool,
    json: bool,
    topic_pane: Option<&str>,
) -> Result<()> {
    let cache = CacheStore::from_env()?;
    WatchHerdrEnvironment::current().save(&cache)?;
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
    publish_resolved(&cache, &mut publish_panes, topic_pane, force)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&outcomes)?);
    }
    Ok(())
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
    let Some(pane) = named_pane(pane_id, harness)? else {
        return Ok(());
    };

    let status = find_status(event);
    // Pi's and omp's exact session files carry the routing evidence. Reading
    // their panes would add a visible repaint without improving attribution.
    let topic_pane = (!matches!(harness, Harness::Pi | Harness::Omp)).then_some(pane_id);
    let result = handle_named_pane(&cache, pane, topic_pane);
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
    let pane = if let Some(event) = event_json() {
        let Some(pane_id) = find_pane_id(&event) else {
            return Ok(());
        };
        // Focus may have moved again, or belong to another client. The event
        // names our target; the inventory supplies its current harness.
        list_agent_state()?
            .panes
            .into_iter()
            .find(|pane| pane.pane_id == pane_id)
    } else {
        let Some((pane_id, harness)) = current_focused_pane()? else {
            return Ok(());
        };
        named_pane(&pane_id, harness)?
    };
    let Some(pane) = pane else {
        return Ok(());
    };
    let cache = CacheStore::from_env()?;
    handle_named_pane(&cache, pane, None)
}

/// The single pane an entry point is allowed to act on.
///
/// It must still be in the one inventory read and still be running the harness
/// that named it; a stale or mismatched pane id yields nothing, so the caller
/// fetches nothing and writes no metadata to any sibling pane.
fn named_pane(pane_id: &str, harness: Harness) -> Result<Option<AgentPane>> {
    Ok(list_agent_state()?
        .panes
        .into_iter()
        .find(|pane| pane.pane_id == pane_id && pane.harness == harness))
}

fn handle_named_pane(cache: &CacheStore, pane: AgentPane, topic_pane: Option<&str>) -> Result<()> {
    if !AgentSelection::from_args_or_env(&[]).contains(&pane.harness) {
        return Ok(());
    }
    WatchHerdrEnvironment::current().save(cache)?;
    let mut panes = [pane];
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
    let style = cache.percent_style().unwrap_or_default();
    let tokens = resolved_pane_tokens(
        cache,
        &mut panes[0],
        resolved,
        CacheStore::now_unix(),
        style,
        false,
    )?
    .into_iter()
    .collect::<Vec<_>>();
    // Event and focus see one pane, not the whole inventory, which is exactly
    // what the alert needs: the entry is keyed by provider, and a provider
    // with no pane in the pass keeps whatever state it had. Warning here is
    // what makes the alert land at the end of the turn that spent the quota
    // rather than at the next poll.
    notify_low_quota(cache, &tokens);
    publish_pane_tokens(&panes, &tokens, CacheStore::now_millis())
}

fn resolved_pane_tokens(
    cache: &CacheStore,
    pane: &mut AgentPane,
    resolved: route::ResolvedPane,
    now: u64,
    style: PercentStyle,
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
            omp_quota(cache, &target, omp.as_ref(), now, style, force)
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
                            pane.session_summary = summary.clone();
                        }
                    }
                }
                tokens_for_loaded_snapshot(
                    provider,
                    snapshot.as_ref(),
                    usable,
                    now,
                    pane.session.as_ref().and_then(|session| session.id()),
                    style,
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
                    style,
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
    style: PercentStyle,
    force: bool,
) -> Option<PaneQuotaUpdate> {
    let evidence = evidence?;
    omp_quota_with_refresh(
        cache,
        target,
        evidence,
        now,
        style,
        force,
        refresh_omp_target,
    )
}

fn omp_quota_with_refresh(
    cache: &CacheStore,
    target: &BillingTarget,
    evidence: &OmpEvidence,
    now: u64,
    style: PercentStyle,
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
                tokens_for_provider(Some(snapshot), now, None, style)
                    .map(|values| PaneQuotaUpdate::Replace(Box::new(values)))
            })
            .or_else(unavailable);
    }
    match refresh(cache, target, evidence, now) {
        OmpUsage::Account(snapshot) => tokens_for_provider(Some(&snapshot), now, None, style)
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
                tokens_for_provider(Some(snapshot), now, None, style)
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
/// `accountsWithoutUsage` is different: without an older snapshot it renders
/// N/A so a failed upstream quota fetch is not mistaken for missing support.
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
        Provider::Grok => grok::fetch_for_sessions(&session_ids).map(FetchedSnapshot::direct),
        Provider::Devin => devin::fetch_for_sessions(&session_ids).map(FetchedSnapshot::direct),
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
            } else if matches!(provider, Provider::Codex | Provider::Grok | Provider::Devin) {
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
) -> Result<()> {
    if let Some(pane) =
        topic_pane.and_then(|pane_id| panes.iter_mut().find(|pane| pane.pane_id == pane_id))
    {
        refresh_pane_topic(pane);
    }
    let mut tokens = Vec::new();
    let now = CacheStore::now_unix();
    let style = cache.percent_style().unwrap_or_default();
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
        if let Some(pane_tokens) =
            resolved_pane_tokens(cache, pane, resolved, now, style, force_target)?
        {
            tokens.push(pane_tokens);
        }
    }
    notify_low_quota(cache, &tokens);
    publish_pane_tokens(panes, &tokens, CacheStore::now_millis())
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
    style: PercentStyle,
) -> Option<MetadataTokens> {
    snapshot.map(|snapshot| {
        MetadataTokens::from_snapshot_for_pane(snapshot, now_unix, session_id, style)
    })
}

fn tokens_for_loaded_snapshot(
    provider: Provider,
    raw: Option<&ProviderSnapshot>,
    usable: Option<&ProviderSnapshot>,
    now_unix: u64,
    session_id: Option<&str>,
    style: PercentStyle,
) -> Option<MetadataTokens> {
    match (usable, raw) {
        (Some(snapshot), _) => tokens_for_provider(Some(snapshot), now_unix, session_id, style),
        (None, Some(_)) => Some(MetadataTokens::unavailable(
            provider,
            "signed-in account changed",
        )),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProviderSnapshot, ResetAt, UsageWindow, WindowKind};
    use tempfile::tempdir;

    fn test_pane(id: &str, harness: Harness) -> AgentPane {
        AgentPane {
            pane_id: id.to_string(),
            harness,
            session: None,
            session_summary: String::new(),
            topic: String::new(),
            tokens: BTreeMap::new(),
        }
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
            ("unknown", "7d N/A"),
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
                PercentStyle::default(),
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
            }
        };
        let lowest = lowest_headroom_by_provider(&[
            tokens("Claude", Some(40)),
            tokens("Claude", Some(12)),
            tokens("Codex", None),
        ]);
        assert_eq!(lowest, low(&[("Claude", 12)]));
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

    #[test]
    fn missing_snapshot_does_not_overwrite_sidebar_with_unavailable() {
        let values = tokens_for_provider(None, 1, None, PercentStyle::default());
        assert!(values.is_none());
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
            PercentStyle::default(),
            false,
            |_, _, _, _| OmpUsage::Unavailable,
        )
        .expect("explicit unavailable update");
        let PaneQuotaUpdate::Replace(values) = update else {
            panic!("expected replacement");
        };
        assert_eq!(values.quota_week, "7d N/A");
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
            PercentStyle::default(),
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
            PercentStyle::default(),
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
            PercentStyle::default(),
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
            PercentStyle::default(),
        )
        .unwrap();
        assert_eq!(values.quota_week, "7d N/A");
        assert_eq!(
            values.quota_week_severity,
            Some(crate::model::Severity::Unknown)
        );
        assert_eq!(
            values.quota_error.as_deref(),
            Some("signed-in account changed")
        );
        // A failure must not masquerade as a lapsed prompt cache.
        assert_eq!(values.quota_cache_state, "");
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
}
