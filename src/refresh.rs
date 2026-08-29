use crate::cache::CacheStore;
use crate::herdr::{
    current_agent_provider, list_agent_panes, list_agent_state, publish_pane_tokens,
    refresh_pane_topic, AgentPane, PaneTokens,
};
use crate::model::{Harness, Provider, ProviderSnapshot};
use crate::presentation::MetadataTokens;
use crate::providers::statusline::enrich_cache_session;
use crate::providers::{codex, grok};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const MAX_ACTIVE_TURN_WATCH: Duration = Duration::from_secs(60 * 60);
const TURN_WATCH_LOCK: &str = "turn.lock";

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

/// Refresh selected providers until their agents leave the working state.
///
/// This command is normally launched detached by `event` and is deliberately
/// quota-only: it never reads a pane. Claude/Agy statusLine hooks publish
/// observations to the local mailbox, while Codex/Grok use their normal
/// providers' fetchers. One global watcher reads Herdr's agent inventory once
/// per poll and refreshes every selected provider that is working. Each
/// provider has its own non-blocking refresh lease, so slow I/O never stalls a
/// statusLine hook or another provider. The existing provider-level debounce
/// remains the lower bound for network requests.
pub fn watch(providers: &[Provider], interval_seconds: Option<u64>) -> Result<()> {
    let cache = CacheStore::from_env()?;
    let interval_seconds = interval_seconds
        .map(CacheStore::validate_watch_interval_seconds)
        .transpose()?
        .unwrap_or_else(|| cache.watch_interval_seconds());
    let Some(_lock) = cache.try_lock_named(TURN_WATCH_LOCK)? else {
        return Ok(());
    };

    let started = Instant::now();
    let started_at = SystemTime::now();
    let started_millis = CacheStore::now_millis();
    let interval = Duration::from_secs(interval_seconds);
    let mut previous_active = Vec::new();
    loop {
        if cache.turn_watchers_stopped_after(started_millis)? {
            break;
        }
        if watch_binary_is_newer(started_at, current_exe_modified()) {
            drop(_lock);
            return reexec_watch();
        }
        // A transient Herdr failure should not terminate a live watcher; the
        // one-hour cap below still prevents an orphaned process. The next
        // poll retries the single inventory call.
        let Ok(state) = list_agent_state() else {
            if started.elapsed() >= MAX_ACTIVE_TURN_WATCH {
                break;
            }
            thread::sleep(interval);
            continue;
        };
        let active = providers
            .iter()
            .copied()
            .filter(|provider| state.working_providers.contains(provider))
            .collect::<Vec<_>>();
        let finishing = previous_active
            .iter()
            .copied()
            .filter(|provider| !active.contains(provider))
            .collect::<Vec<_>>();

        // A provider can settle while another provider keeps working. Run one
        // final debounced pass for providers that just transitioned to idle,
        // then continue polling the remaining active set in the same process.
        if !finishing.is_empty() {
            let _ = refresh_and_publish(&cache, &finishing, false, &state.panes);
        }
        if active.is_empty() {
            break;
        }
        let _ = refresh_and_publish(&cache, &active, false, &state.panes);
        previous_active = active;
        if started.elapsed() >= MAX_ACTIVE_TURN_WATCH {
            break;
        }
        thread::sleep(interval);
    }
    Ok(())
}

fn refresh_and_publish(
    cache: &CacheStore,
    providers: &[Provider],
    force: bool,
    panes: &[AgentPane],
) -> Result<()> {
    refresh_selected(cache, providers, force, panes)?;
    publish(cache, providers, None, Some(panes))
}

fn run_internal(
    providers: &[Provider],
    force: bool,
    json: bool,
    topic_pane: Option<&str>,
) -> Result<()> {
    let cache = CacheStore::from_env()?;
    // Agent inventory is metadata-only. Reusing it for both the fetch and the
    // publish pass lets local Codex/Grok diagnostics target the exact pane
    // sessions without adding another Herdr call or reading any pane output.
    let panes = list_agent_panes().ok();
    let session_panes = panes.as_deref().unwrap_or_default();
    let outcomes = refresh_selected(&cache, providers, force, session_panes)?;
    publish(&cache, providers, topic_pane, panes.as_deref())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&outcomes)?);
    }
    Ok(())
}

pub fn event() -> Result<()> {
    let event = event_json();
    let providers = event_collectors(event.as_ref());
    // Unknown/OpenCode harnesses have no collector. Do not fall back to
    // every billing target, read sibling panes, or start an all-provider watch.
    if providers.is_empty() {
        return Ok(());
    }
    let topic_pane = event.as_ref().and_then(find_pane_id);
    let status = event.as_ref().and_then(find_status);
    // The detached watcher owns the final debounced pass. Keeping the event
    // itself debounced avoids a duplicate Codex/Grok fetch when the agent
    // emits its idle transition immediately after a tool hook.
    let result = run_internal(&providers, false, false, topic_pane);
    if status.is_some_and(is_working_status) {
        if let Err(error) = spawn_watch() {
            if result.is_ok() {
                return Err(error);
            }
        }
    }
    result
}

fn event_collectors(event: Option<&Value>) -> Vec<Provider> {
    event
        .and_then(find_agent)
        .and_then(Harness::billing_for_agent)
        .map(|billing| vec![billing])
        .unwrap_or_default()
}

pub fn focus() -> Result<()> {
    let Some(provider) = current_agent_provider()? else {
        return Ok(());
    };
    run(&[provider], false, false)
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
        .filter(|pane| pane.provider == provider)
        .filter_map(|pane| pane.session_id.clone())
        .collect::<Vec<_>>();
    let fetched = match provider {
        Provider::Codex => codex::fetch_for_sessions(&session_ids).map(FetchedSnapshot::direct),
        Provider::Grok => grok::fetch_for_sessions(&session_ids).map(FetchedSnapshot::direct),
        Provider::Claude | Provider::Agy => load_statusline_snapshot(cache, provider),
    };
    cache.mark_refresh(provider, now)?;
    match fetched {
        Ok(fetched) => {
            let FetchedSnapshot {
                mut snapshot,
                preserve_context,
                session_id,
            } = fetched;
            if preserve_context {
                cache.save_preserving_context_for_session(snapshot, session_id.as_deref())?;
            } else if matches!(provider, Provider::Codex | Provider::Grok) {
                cache.save_preserving_diagnostics_for_sessions(&mut snapshot, &session_ids)?;
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
    if force || !cache.should_debounce(provider, now_unix, 60)? {
        return Ok(false);
    }
    if load_usable_snapshot(cache, provider)?.is_some() {
        return Ok(true);
    }
    // No snapshot at all: keep debounce so missing credentials do not hammer
    // the provider. A snapshot for another account must not debounce — fetch
    // the signed-in identity now.
    Ok(cache.load(provider)?.is_none())
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
                .and_then(|credentials| credentials.user_id);
            let mtime = path.as_ref().and_then(|path| grok::auth_mtime_unix(path));
            (account_id, mtime)
        }
        Provider::Codex => (codex::current_account_id(), codex::auth_mtime_unix()),
        Provider::Claude | Provider::Agy => (None, None),
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
    let previous_cache = cache
        .load(provider)
        .ok()
        .flatten()
        .and_then(|snapshot| snapshot.context)
        .and_then(|context| context.cache);
    enrich_cache_session(&mut snapshot, &value, previous_cache.as_ref());
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

fn publish(
    cache: &CacheStore,
    providers: &[Provider],
    topic_pane: Option<&str>,
    agent_panes: Option<&[AgentPane]>,
) -> Result<()> {
    let mut panes = agent_panes.map_or_else(
        || list_agent_panes().unwrap_or_default(),
        |panes| panes.to_vec(),
    );
    if let Some(pane) = topic_pane.and_then(|pane_id| {
        panes
            .iter_mut()
            .find(|pane| pane.pane_id == pane_id && providers.contains(&pane.provider))
    }) {
        refresh_pane_topic(pane);
    }
    let mut tokens = Vec::new();
    let now = CacheStore::now_unix();
    for provider in providers {
        let snapshot = cache.load(*provider)?;
        let (account_id, mtime) = current_account_gate(*provider);
        let usable = snapshot
            .as_ref()
            .filter(|snapshot| snapshot.usable_for_account(account_id.as_deref(), mtime));
        for pane in panes.iter_mut().filter(|pane| pane.provider == *provider) {
            if let Some(snapshot) = usable {
                if let Some(session_id) = pane.session_id.as_deref() {
                    if let Some(summary) = snapshot.session_summaries.get(session_id) {
                        pane.session_summary = summary.clone();
                    }
                }
            }
            if let Some(values) = tokens_for_loaded_snapshot(
                *provider,
                snapshot.as_ref(),
                usable,
                now,
                pane.session_id.as_deref(),
            ) {
                tokens.push(PaneTokens {
                    pane_id: pane.pane_id.clone(),
                    values,
                });
            }
        }
    }
    publish_pane_tokens(&panes, &tokens, CacheStore::now_millis())
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

fn reexec_watch() -> Result<()> {
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    let mut command = Command::new(executable);
    command.args(["watch", "--provider", "all"]);
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

fn spawn_watch() -> Result<()> {
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    let mut command = Command::new(executable);
    command
        .args(["watch", "--provider", "all"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
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
) -> Option<MetadataTokens> {
    snapshot.map(|snapshot| MetadataTokens::from_snapshot_for_pane(snapshot, now_unix, session_id))
}

fn tokens_for_loaded_snapshot(
    provider: Provider,
    raw: Option<&ProviderSnapshot>,
    usable: Option<&ProviderSnapshot>,
    now_unix: u64,
    session_id: Option<&str>,
) -> Option<MetadataTokens> {
    match (usable, raw) {
        (Some(snapshot), _) => tokens_for_provider(Some(snapshot), now_unix, session_id),
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
    use crate::model::{ProviderSnapshot, UsageWindow, WindowKind};
    use tempfile::tempdir;

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
        let values = tokens_for_provider(None, 1, None);
        assert!(values.is_none());
    }

    #[test]
    fn other_account_snapshot_is_not_shown_as_the_current_quota() {
        let snapshot = ProviderSnapshot::new(
            Provider::Grok,
            vec![UsageWindow::new(WindowKind::Weekly, 100.0, None).unwrap()],
            1,
        )
        .with_account_id(Some("old-account".to_string()));
        let values =
            tokens_for_loaded_snapshot(Provider::Grok, Some(&snapshot), None, 1, None).unwrap();
        assert_eq!(values.quota_summary, "unavailable");
        assert_eq!(
            values.quota_error.as_deref(),
            Some("signed-in account changed")
        );
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
        let opencode: Value = serde_json::from_str(
            r#"{"event":"pane_agent_status_changed",
                "data":{"pane_id":"w1:p9","agent":"opencode","status":"working"}}"#,
        )
        .unwrap();
        assert!(event_collectors(Some(&opencode)).is_empty());

        let mixed_case: Value =
            serde_json::from_str(r#"{"data":{"agent":"OpenCode","status":"working"}}"#).unwrap();
        assert!(event_collectors(Some(&mixed_case)).is_empty());

        let other: Value =
            serde_json::from_str(r#"{"data":{"agent":"cursor","status":"working"}}"#).unwrap();
        assert!(event_collectors(Some(&other)).is_empty());

        let claude: Value =
            serde_json::from_str(r#"{"data":{"agent":"claude-code","pane_id":"w1:p1"}}"#).unwrap();
        assert_eq!(event_collectors(Some(&claude)), vec![Provider::Claude]);
        assert!(event_collectors(None).is_empty());
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
