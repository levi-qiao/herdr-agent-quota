use crate::model::{
    merge_omitted_window_list, BillingTarget, ContextUsage, Provider, ProviderSnapshot,
};
use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_WATCH_INTERVAL_SECONDS: u64 = 60;
pub const MIN_WATCH_INTERVAL_SECONDS: u64 = 30;
pub const MAX_WATCH_INTERVAL_SECONDS: u64 = 60 * 60;
const WATCH_INTERVAL_ENV: &str = "HERDR_AGENT_QUOTA_WATCH_INTERVAL_SECONDS";
const WATCH_INTERVAL_FILE: &str = "watch-interval-seconds";
const MAX_STATUSLINE_SESSIONS: usize = 128;

#[derive(Debug, Clone)]
pub struct CacheStore {
    root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatuslineObservation {
    pub snapshot: ProviderSnapshot,
    pub payload: Value,
}

impl CacheStore {
    pub fn from_env() -> Result<Self> {
        let root = std::env::var_os("HERDR_PLUGIN_STATE_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                ProjectDirs::from("dev", "herdr", "herdr-agent-quota")
                    .map(|dirs| dirs.data_local_dir().to_path_buf())
            })
            .context("cannot determine plugin state directory")?;
        Ok(Self { root })
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("create cache directory {}", self.root.display()))
    }

    pub fn load(&self, provider: Provider) -> Result<Option<ProviderSnapshot>> {
        let path = self.snapshot_path(provider);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let snapshot = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse cached {} snapshot", provider.source()))?;
        Ok(Some(snapshot))
    }

    pub fn save(&self, snapshot: &ProviderSnapshot) -> Result<()> {
        self.ensure()?;
        let destination = self.snapshot_path(snapshot.provider);
        let temporary = self.root.join(format!(
            ".{}.{}.tmp",
            snapshot.provider.source(),
            std::process::id()
        ));
        let bytes = serde_json::to_vec_pretty(snapshot).context("serialize quota snapshot")?;
        Self::atomic_replace(&destination, &temporary, bytes)
    }

    /// Keep provider-local diagnostics when a successful quota refresh cannot
    /// read one of the session files for a moment. Quota windows from the
    /// latest fetch replace the cache; an omitted 5h/weekly window is restored
    /// from the previous snapshot only when that window is still current.
    /// Diagnostics are merged only for the same signed in account so a login
    /// switch can never inherit another user's data.
    pub fn save_preserving_diagnostics(&self, mut snapshot: ProviderSnapshot) -> Result<()> {
        self.save_preserving_diagnostics_for_sessions(&mut snapshot, &[])
    }

    /// Variant used by the refresh path, which knows the session ids currently
    /// visible in Herdr. It preserves only those ids, keeping a bounded local
    /// snapshot instead of growing it forever as old sessions age out.
    pub fn save_preserving_diagnostics_for_sessions(
        &self,
        snapshot: &mut ProviderSnapshot,
        session_ids: &[String],
    ) -> Result<()> {
        if let Some(previous) = self.load(snapshot.provider).ok().flatten() {
            let same_account = snapshot.account_id == previous.account_id;
            if same_account {
                snapshot.merge_omitted_windows(&previous);
                if session_ids.is_empty() {
                    if snapshot.context.is_none() {
                        snapshot.context = previous.context.clone();
                    }
                    if snapshot.model.is_none() {
                        snapshot.model = previous.model.clone();
                    }
                    if snapshot.session_contexts.is_empty() {
                        for (session_id, context) in previous.session_contexts {
                            snapshot
                                .session_contexts
                                .entry(session_id)
                                .or_insert(context);
                        }
                    }
                    if snapshot.session_models.is_empty() {
                        for (session_id, model) in previous.session_models {
                            snapshot.session_models.entry(session_id).or_insert(model);
                        }
                    }
                } else {
                    for session_id in session_ids {
                        if let Some(context) = previous.session_contexts.get(session_id) {
                            snapshot
                                .session_contexts
                                .entry(session_id.clone())
                                .or_insert_with(|| context.clone());
                        }
                        if let Some(model) = previous.session_models.get(session_id) {
                            snapshot
                                .session_models
                                .entry(session_id.clone())
                                .or_insert_with(|| model.clone());
                        }
                    }
                }
            }
        }
        prune_session_diagnostics(snapshot, session_ids);
        self.save(snapshot)
    }

    /// Store the latest statusLine observation without coordinating with a
    /// refresh. The statusLine hook is a latency-sensitive producer; its only
    /// shared-state operation is an atomic last-observation replacement.
    pub fn save_statusline_observation(
        &self,
        provider: Provider,
        mut snapshot: ProviderSnapshot,
        observation: &Value,
    ) -> Result<()> {
        self.ensure()?;
        let session_id = statusline_session_id(observation);
        if let Some(session_id) = session_id {
            if let Some(cache) = snapshot
                .context
                .as_mut()
                .and_then(|context| context.cache.as_mut())
            {
                cache.session_id = Some(session_id.to_string());
            }
        }
        let previous = self.load_statusline_observation(provider).ok().flatten();
        let previous_snapshot = previous.as_ref().map(|observation| &observation.snapshot);
        let previous_session_id = previous
            .as_ref()
            .and_then(|observation| statusline_session_id(&observation.payload));
        merge_preserved_context(
            &mut snapshot,
            previous_snapshot.and_then(|snapshot| snapshot.context.clone()),
            previous_session_id,
            session_id,
        );
        merge_session_models(
            &mut snapshot,
            previous_snapshot,
            previous_session_id,
            session_id,
        );
        if let Some(previous_snapshot) = previous_snapshot {
            for (session_id, context) in &previous_snapshot.session_contexts {
                snapshot
                    .session_contexts
                    .entry(session_id.clone())
                    .or_insert_with(|| context.clone());
            }
        }
        if let Some(session_id) = session_id {
            if let Some(context) = snapshot.context.clone() {
                snapshot
                    .session_contexts
                    .insert(session_id.to_string(), context);
            }
        }
        merge_session_windows(&mut snapshot, previous_snapshot, session_id);
        let current_session_ids = session_id
            .map(|session_id| vec![session_id.to_string()])
            .unwrap_or_default();
        prune_session_diagnostics(&mut snapshot, &current_session_ids);
        let saved = StatuslineObservation {
            snapshot,
            payload: observation.clone(),
        };
        let destination = self.statusline_observation_path(provider);
        let temporary = self.root.join(format!(
            ".{}.observation.{}.tmp",
            provider.source(),
            std::process::id()
        ));
        let bytes = serde_json::to_vec(&saved).context("serialize statusLine observation")?;
        Self::atomic_replace(&destination, &temporary, bytes)
    }

    pub fn load_statusline_observation(
        &self,
        provider: Provider,
    ) -> Result<Option<StatuslineObservation>> {
        let path = self.statusline_observation_path(provider);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let observation = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse {} observation", provider.source()))?;
        Ok(Some(observation))
    }

    /// StatusLine payloads may temporarily omit context (before the first
    /// response and immediately after compaction). Keep the last known value.
    /// Quota windows still come from the newest snapshot, except an omitted
    /// 5h/weekly window is restored when it is still current.
    pub fn save_preserving_context(&self, snapshot: ProviderSnapshot) -> Result<()> {
        self.save_preserving_context_for_session(snapshot, None)
    }

    /// Save a statusLine snapshot while matching preserved diagnostics to the
    /// session id from the same stdin payload. This keeps a compacted Claude
    /// session's aggregate offset without carrying it into a new session.
    pub fn save_preserving_context_for_session(
        &self,
        mut snapshot: ProviderSnapshot,
        session_id: Option<&str>,
    ) -> Result<()> {
        if let Some(session_id) = session_id {
            if let Some(cache) = snapshot
                .context
                .as_mut()
                .and_then(|context| context.cache.as_mut())
            {
                cache.session_id = Some(session_id.to_string());
            }
        }
        // A malformed/temporarily unreadable old snapshot must not prevent a
        // fresh statusLine value from replacing it.
        let previous = self.load(snapshot.provider).ok().flatten();
        let previous_session_id = previous
            .as_ref()
            .and_then(|snapshot| snapshot.context.as_ref())
            .and_then(|context| context.cache.as_ref())
            .and_then(|cache| cache.session_id.as_deref());
        merge_preserved_context(
            &mut snapshot,
            previous
                .as_ref()
                .and_then(|snapshot| snapshot.context.clone()),
            previous_session_id,
            session_id,
        );
        merge_session_models(
            &mut snapshot,
            previous.as_ref(),
            previous_session_id,
            session_id,
        );
        if let Some(previous) = previous.as_ref() {
            for (session_id, context) in &previous.session_contexts {
                snapshot
                    .session_contexts
                    .entry(session_id.clone())
                    .or_insert_with(|| context.clone());
            }
        }
        if let Some(session_id) = session_id {
            if let Some(context) = snapshot.context.clone() {
                snapshot
                    .session_contexts
                    .insert(session_id.to_string(), context);
            }
        }
        merge_session_windows(&mut snapshot, previous.as_ref(), session_id);
        let current_session_ids = session_id
            .map(|session_id| vec![session_id.to_string()])
            .unwrap_or_default();
        prune_session_diagnostics(&mut snapshot, &current_session_ids);
        self.save(&snapshot)
    }

    /// Try to claim a named long-running coordination lock.
    ///
    /// Active-turn refreshers are started by two Herdr events at the same
    /// boundary (and there may be several working providers). A non-blocking
    /// OS lock lets the first global watcher own the poll loop while later
    /// starts exit immediately instead of creating duplicate pollers.
    pub fn try_lock_named(&self, name: &str) -> Result<Option<File>> {
        self.ensure()?;
        let path = self.root.join(name);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(error) => Err(error).with_context(|| format!("lock {}", path.display())),
        }
    }

    /// Claim a provider refresh lease without making a statusLine or event
    /// caller wait behind another provider's slow I/O.
    pub fn try_lock_provider_refresh(&self, provider: Provider) -> Result<Option<File>> {
        self.try_lock_target_refresh(&BillingTarget::original_four(provider))
    }

    /// Refresh lease for a billing target. Original-four names stay the 0.2
    /// `*.refresh.lock` files; OpenCode Go is scoped to the OpenCode store.
    pub fn try_lock_target_refresh(&self, target: &BillingTarget) -> Result<Option<File>> {
        self.try_lock_named(&format!("{}.refresh.lock", target.cache_identity()))
    }

    pub fn stop_turn_watchers(&self) -> Result<()> {
        self.ensure()?;
        fs::write(
            self.root.join("turn-watch.stop"),
            Self::now_millis().to_string(),
        )
        .context("stop active-turn quota watchers")
    }

    pub fn clear_turn_watcher_stop(&self) -> Result<()> {
        let path = self.root.join("turn-watch.stop");
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("clear active-turn watcher stop marker"),
        }
    }

    pub fn turn_watchers_stopped_after(&self, started_millis: u64) -> Result<bool> {
        let path = self.root.join("turn-watch.stop");
        let Ok(value) = fs::read_to_string(path) else {
            return Ok(false);
        };
        Ok(value
            .trim()
            .parse::<u64>()
            .is_ok_and(|stopped| stopped >= started_millis))
    }

    /// Return the configured active-turn polling interval.
    ///
    /// An environment override is useful for one-off runs and installation
    /// scripts; the state file is the persistent user setting. Invalid or
    /// out-of-range values deliberately fall back to the safe default.
    pub fn watch_interval_seconds(&self) -> u64 {
        std::env::var(WATCH_INTERVAL_ENV)
            .ok()
            .and_then(|value| value.parse().ok())
            .and_then(Self::valid_watch_interval)
            .or_else(|| {
                fs::read_to_string(self.watch_interval_path())
                    .ok()
                    .and_then(|value| value.trim().parse().ok())
                    .and_then(Self::valid_watch_interval)
            })
            .unwrap_or(DEFAULT_WATCH_INTERVAL_SECONDS)
    }

    pub fn set_watch_interval_seconds(&self, seconds: u64) -> Result<()> {
        Self::valid_watch_interval(seconds).with_context(|| {
            format!(
                "watch interval must be between {MIN_WATCH_INTERVAL_SECONDS} and {MAX_WATCH_INTERVAL_SECONDS} seconds"
            )
        })?;
        self.ensure()?;
        fs::write(self.watch_interval_path(), seconds.to_string())
            .context("write active-turn watch interval")
    }

    pub fn clear_watch_interval(&self) -> Result<()> {
        match fs::remove_file(self.watch_interval_path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("remove active-turn watch interval"),
        }
    }

    pub fn validate_watch_interval_seconds(seconds: u64) -> Result<u64> {
        Self::valid_watch_interval(seconds).with_context(|| {
            format!(
                "watch interval must be between {MIN_WATCH_INTERVAL_SECONDS} and {MAX_WATCH_INTERVAL_SECONDS} seconds"
            )
        })
    }

    pub fn should_debounce(
        &self,
        provider: Provider,
        now_unix: u64,
        interval_seconds: u64,
    ) -> Result<bool> {
        let Ok(contents) = fs::read_to_string(self.refresh_marker_path(provider)) else {
            return Ok(false);
        };
        let Ok(last) = contents.trim().parse::<u64>() else {
            return Ok(false);
        };
        Ok(now_unix.saturating_sub(last) < interval_seconds)
    }

    pub fn mark_refresh(&self, provider: Provider, now_unix: u64) -> Result<()> {
        self.ensure()?;
        fs::write(self.refresh_marker_path(provider), now_unix.to_string())
            .context("write refresh marker")
    }

    pub fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default()
    }

    pub fn file_mtime_unix(path: &Path) -> Option<u64> {
        fs::metadata(path)
            .ok()?
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_secs())
    }

    pub fn now_millis() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or_default()
    }

    fn snapshot_path(&self, provider: Provider) -> PathBuf {
        self.root.join(format!("{}.json", provider.source()))
    }

    fn statusline_observation_path(&self, provider: Provider) -> PathBuf {
        self.root
            .join(format!("{}.observation.json", provider.source()))
    }

    fn atomic_replace(destination: &Path, temporary: &Path, bytes: Vec<u8>) -> Result<()> {
        fs::write(temporary, bytes).with_context(|| format!("write {}", temporary.display()))?;
        if let Err(error) = fs::rename(temporary, destination) {
            // Otherwise a failed rename leaves the scratch file behind, and
            // every later refresh adds another one.
            let _ = fs::remove_file(temporary);
            return Err(error).with_context(|| {
                format!(
                    "atomically replace {} with {}",
                    destination.display(),
                    temporary.display()
                )
            });
        }
        Ok(())
    }

    fn refresh_marker_path(&self, provider: Provider) -> PathBuf {
        self.root.join(format!("{}.refresh", provider.source()))
    }

    fn watch_interval_path(&self) -> PathBuf {
        self.root.join(WATCH_INTERVAL_FILE)
    }

    fn valid_watch_interval(seconds: u64) -> Option<u64> {
        (MIN_WATCH_INTERVAL_SECONDS..=MAX_WATCH_INTERVAL_SECONDS)
            .contains(&seconds)
            .then_some(seconds)
    }
}

fn merge_session_models(
    snapshot: &mut ProviderSnapshot,
    previous: Option<&ProviderSnapshot>,
    previous_session_id: Option<&str>,
    session_id: Option<&str>,
) {
    if let Some(previous) = previous {
        for (session_id, model) in &previous.session_models {
            snapshot
                .session_models
                .entry(session_id.clone())
                .or_insert_with(|| model.clone());
        }
    }
    let Some(session_id) = session_id else {
        return;
    };
    if let Some(model) = snapshot.model.as_ref() {
        snapshot
            .session_models
            .insert(session_id.to_string(), model.clone());
    } else if previous_session_id == Some(session_id) {
        if let Some(model) = previous.and_then(|previous| previous.model.as_ref()) {
            snapshot
                .session_models
                .entry(session_id.to_string())
                .or_insert_with(|| model.clone());
        }
    }
}

fn merge_session_windows(
    snapshot: &mut ProviderSnapshot,
    previous: Option<&ProviderSnapshot>,
    session_id: Option<&str>,
) {
    if let Some(previous) = previous {
        for (session_id, windows) in &previous.session_windows {
            snapshot
                .session_windows
                .entry(session_id.clone())
                .or_insert_with(|| windows.clone());
        }
        match session_id {
            Some(session_id) => {
                let previous_windows = previous
                    .session_windows
                    .get(session_id)
                    .map(Vec::as_slice)
                    .or_else(|| {
                        previous
                            .session_windows
                            .is_empty()
                            .then_some(previous.windows.as_slice())
                    });
                if let Some(previous_windows) = previous_windows {
                    merge_omitted_window_list(
                        &mut snapshot.windows,
                        previous_windows,
                        snapshot.fetched_at_unix,
                    );
                }
            }
            None => snapshot.merge_omitted_windows(previous),
        }
    }
    if let Some(session_id) = session_id {
        snapshot
            .session_windows
            .insert(session_id.to_string(), snapshot.windows.clone());
    }
}

fn prune_session_diagnostics(snapshot: &mut ProviderSnapshot, current_session_ids: &[String]) {
    prune_session_map(&mut snapshot.session_models, current_session_ids);
    prune_session_map(&mut snapshot.session_contexts, current_session_ids);
    prune_session_map(&mut snapshot.session_windows, current_session_ids);
}

fn prune_session_map<T>(map: &mut BTreeMap<String, T>, current_session_ids: &[String]) {
    while map.len() > MAX_STATUSLINE_SESSIONS {
        let Some(session_id) = map
            .keys()
            .find(|session_id| {
                !current_session_ids
                    .iter()
                    .any(|current| current == *session_id)
            })
            .cloned()
            .or_else(|| map.keys().next().cloned())
        else {
            break;
        };
        map.remove(&session_id);
    }
}

fn statusline_session_id(observation: &Value) -> Option<&str> {
    observation
        .get("session_id")
        .or_else(|| observation.get("sessionId"))
        .or_else(|| observation.get("conversation_id"))
        .or_else(|| observation.get("conversationId"))
        .and_then(Value::as_str)
}

fn merge_preserved_context(
    snapshot: &mut ProviderSnapshot,
    previous: Option<ContextUsage>,
    previous_session_id: Option<&str>,
    session_id: Option<&str>,
) {
    let Some(previous_context) = previous else {
        return;
    };
    let same_session = sessions_match(previous_session_id, session_id);
    match (&mut snapshot.context, previous_context) {
        (None, previous_context) if same_session => {
            snapshot.context = Some(previous_context);
        }
        (None, _) => {}
        (Some(current), previous_context) if current.cache.is_none() && same_session => {
            current.cache = previous_context.cache;
        }
        (Some(current), previous_context) => {
            let Some(current_cache) = current.cache.as_mut() else {
                return;
            };
            let Some(previous_cache) = previous_context.cache.as_ref() else {
                return;
            };
            if same_session {
                if current_cache.session_totals.is_none() {
                    current_cache.session_totals = previous_cache.session_totals.clone();
                }
                if current_cache.transcript_offset == 0 {
                    current_cache.transcript_offset = previous_cache.transcript_offset;
                }
                if current_cache.ttl_seconds.is_none() {
                    current_cache.ttl_seconds = previous_cache.ttl_seconds;
                }
                if current_cache.last_activity_unix.is_none() {
                    current_cache.last_activity_unix = previous_cache.last_activity_unix;
                }
            }
        }
    }
}

fn sessions_match(previous_session_id: Option<&str>, session_id: Option<&str>) -> bool {
    match (previous_session_id, session_id) {
        (Some(previous), Some(current)) => previous == current,
        (None, None) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        BillingTarget, CacheUsage, ContextUsage, Provider, ResetAt, UsageWindow, WindowKind,
    };
    use serde_json::json;
    use tempfile::tempdir;

    fn snapshot() -> ProviderSnapshot {
        ProviderSnapshot::new(
            Provider::Grok,
            vec![UsageWindow::new(WindowKind::Weekly, 42.5, None).unwrap()],
            123,
        )
    }

    #[test]
    fn successful_snapshot_round_trips_through_atomic_cache() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        cache.save(&snapshot()).unwrap();
        assert_eq!(cache.load(Provider::Grok).unwrap(), Some(snapshot()));
    }

    #[test]
    fn opencode_go_lease_does_not_touch_original_four_cache_files() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let target = BillingTarget::opencode_go();
        let lease = cache.try_lock_target_refresh(&target).unwrap();
        assert!(lease.is_some());
        assert!(directory
            .path()
            .join("opencode-go.opencode-store.refresh.lock")
            .exists());
        for filename in [
            "codex-app-server.json",
            "grok-cli-billing.json",
            "claude-statusline.json",
            "agy-statusline.json",
            "codex-app-server.refresh.lock",
            "grok-cli-billing.refresh.lock",
            "claude-statusline.refresh.lock",
            "agy-statusline.refresh.lock",
            "codex-app-server.refresh",
            "grok-cli-billing.refresh",
            "claude-statusline.refresh",
            "agy-statusline.refresh",
        ] {
            assert!(
                !directory.path().join(filename).exists(),
                "OpenCode lease created {filename}"
            );
        }
    }

    #[test]
    fn original_four_snapshots_use_canonical_0_2_cache_filenames() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        for (provider, filename) in [
            (Provider::Codex, "codex-app-server.json"),
            (Provider::Grok, "grok-cli-billing.json"),
            (Provider::Claude, "claude-statusline.json"),
            (Provider::Agy, "agy-statusline.json"),
        ] {
            cache
                .save(&ProviderSnapshot::new(provider, vec![], 1))
                .unwrap();
            let path = directory.path().join(filename);
            assert!(path.exists(), "missing {filename}");
            let loaded: ProviderSnapshot =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(loaded.provider, provider);
            assert_eq!(loaded.source, provider.source());
            assert_eq!(cache.load(provider).unwrap().unwrap().provider, provider);
        }
    }

    #[test]
    fn statusline_observation_preserves_context_when_the_next_payload_omits_it() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let previous = ProviderSnapshot::new(
            Provider::Claude,
            vec![UsageWindow::new(WindowKind::Weekly, 27.0, None).unwrap()],
            1,
        )
        .with_context(Some(ContextUsage::new(23.5).unwrap()));
        cache
            .save_statusline_observation(
                Provider::Claude,
                previous,
                &json!({"session_id": "session-1"}),
            )
            .unwrap();

        let latest = ProviderSnapshot::new(Provider::Claude, vec![], 2);
        cache
            .save_statusline_observation(
                Provider::Claude,
                latest,
                &json!({"session_id": "session-1"}),
            )
            .unwrap();

        let saved = cache
            .load_statusline_observation(Provider::Claude)
            .unwrap()
            .unwrap();
        assert_eq!(saved.snapshot.windows.len(), 0);
        assert_eq!(saved.snapshot.context.as_ref().unwrap().used_percent, 23.5);
    }

    #[test]
    fn statusline_observations_keep_models_for_multiple_sessions() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        cache
            .save_statusline_observation(
                Provider::Claude,
                ProviderSnapshot::new(Provider::Claude, vec![], 1)
                    .with_model(Some("Sonnet".to_string())),
                &json!({"session_id": "session-1"}),
            )
            .unwrap();
        cache
            .save_statusline_observation(
                Provider::Claude,
                ProviderSnapshot::new(Provider::Claude, vec![], 2)
                    .with_model(Some("Opus".to_string())),
                &json!({"conversation_id": "session-2"}),
            )
            .unwrap();
        cache
            .save_statusline_observation(
                Provider::Claude,
                ProviderSnapshot::new(Provider::Claude, vec![], 3),
                &json!({"session_id": "session-2"}),
            )
            .unwrap();

        let saved = cache
            .load_statusline_observation(Provider::Claude)
            .unwrap()
            .unwrap()
            .snapshot;
        assert_eq!(saved.session_models["session-1"], "Sonnet");
        assert_eq!(saved.session_models["session-2"], "Opus");
    }

    #[test]
    fn statusline_observations_keep_quota_windows_for_multiple_sessions() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        cache
            .save_statusline_observation(
                Provider::Claude,
                ProviderSnapshot::new(
                    Provider::Claude,
                    vec![UsageWindow::new(WindowKind::Weekly, 10.0, None).unwrap()],
                    1,
                ),
                &json!({"session_id": "work"}),
            )
            .unwrap();
        cache
            .save_statusline_observation(
                Provider::Claude,
                ProviderSnapshot::new(
                    Provider::Claude,
                    vec![UsageWindow::new(WindowKind::Weekly, 90.0, None).unwrap()],
                    2,
                ),
                &json!({"session_id": "personal"}),
            )
            .unwrap();

        let saved = cache
            .load_statusline_observation(Provider::Claude)
            .unwrap()
            .unwrap()
            .snapshot;
        assert_eq!(
            saved.windows_for_session(Some("work"))[0].used_percent,
            10.0
        );
        assert_eq!(
            saved.windows_for_session(Some("personal"))[0].used_percent,
            90.0
        );
        assert!(saved.windows_for_session(Some("unknown")).is_empty());
    }

    #[test]
    fn statusline_omitted_five_hour_window_stays_on_the_same_session() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        cache
            .save_statusline_observation(
                Provider::Claude,
                ProviderSnapshot::new(
                    Provider::Claude,
                    vec![
                        UsageWindow::new(
                            WindowKind::FiveHour,
                            22.0,
                            Some(ResetAt::from_unix_seconds(2_000)),
                        )
                        .unwrap(),
                        UsageWindow::new(
                            WindowKind::Weekly,
                            65.0,
                            Some(ResetAt::from_unix_seconds(10_000)),
                        )
                        .unwrap(),
                    ],
                    1_000,
                ),
                &json!({"session_id": "work"}),
            )
            .unwrap();
        cache
            .save_statusline_observation(
                Provider::Claude,
                ProviderSnapshot::new(
                    Provider::Claude,
                    vec![UsageWindow::new(
                        WindowKind::Weekly,
                        90.0,
                        Some(ResetAt::from_unix_seconds(10_000)),
                    )
                    .unwrap()],
                    1_100,
                ),
                &json!({"session_id": "personal"}),
            )
            .unwrap();
        cache
            .save_statusline_observation(
                Provider::Claude,
                ProviderSnapshot::new(
                    Provider::Claude,
                    vec![UsageWindow::new(
                        WindowKind::Weekly,
                        66.0,
                        Some(ResetAt::from_unix_seconds(10_000)),
                    )
                    .unwrap()],
                    1_200,
                ),
                &json!({"session_id": "work"}),
            )
            .unwrap();

        let saved = cache
            .load_statusline_observation(Provider::Claude)
            .unwrap()
            .unwrap()
            .snapshot;
        assert_eq!(
            saved
                .windows_for_session(Some("work"))
                .iter()
                .find(|window| window.kind == WindowKind::FiveHour)
                .unwrap()
                .used_percent,
            22.0
        );
        assert!(saved
            .windows_for_session(Some("personal"))
            .iter()
            .all(|window| window.kind != WindowKind::FiveHour));
    }

    #[test]
    fn statusline_refresh_preserves_previous_cache_diagnostics_when_current_usage_is_missing() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let context = ContextUsage::new(23.5).unwrap().with_cache(Some(
            CacheUsage::from_token_counts(10, 90, 0)
                .unwrap()
                .with_ttl_estimate(300, 1_000),
        ));
        cache
            .save(&snapshot().with_context(Some(context.clone())))
            .unwrap();

        let latest = snapshot().with_context(Some(ContextUsage::new(24.0).unwrap()));
        cache.save_preserving_context(latest).unwrap();
        let saved_context = cache
            .load(Provider::Grok)
            .unwrap()
            .unwrap()
            .context
            .unwrap();
        assert_eq!(saved_context.used_percent, 24.0);
        assert_eq!(saved_context.cache, context.cache);
    }

    #[test]
    fn statusline_refresh_records_context_for_the_current_session() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let current = snapshot().with_context(Some(ContextUsage::new(12.0).unwrap()));
        cache
            .save_preserving_context_for_session(current, Some("session-1"))
            .unwrap();
        let saved = cache.load(Provider::Grok).unwrap().unwrap();
        assert_eq!(
            saved
                .context_for_session(Some("session-1"))
                .map(|context| context.used_percent),
            Some(12.0)
        );
        assert!(saved.session_contexts.contains_key("session-1"));
    }

    #[test]
    fn statusline_refresh_preserves_model_for_the_same_session() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let previous = snapshot()
            .with_model(Some("Sonnet".to_string()))
            .with_context(Some(
                ContextUsage::new(10.0).unwrap().with_cache(Some(
                    CacheUsage::from_token_counts(1, 1, 0)
                        .unwrap()
                        .with_session_totals(None, "session-1", 0),
                )),
            ));
        cache.save(&previous).unwrap();

        let current = snapshot().with_context(Some(ContextUsage::new(12.0).unwrap()));
        cache
            .save_preserving_context_for_session(current, Some("session-1"))
            .unwrap();
        let saved = cache.load(Provider::Grok).unwrap().unwrap();
        assert_eq!(saved.session_models["session-1"], "Sonnet");
    }

    #[test]
    fn direct_provider_refresh_preserves_missing_local_session_diagnostics() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let mut previous = snapshot().with_account_id(Some("account-1".to_string()));
        previous.model = Some("grok-4.6".to_string());
        previous
            .session_models
            .insert("session-1".to_string(), "grok-4.6".to_string());
        previous
            .session_contexts
            .insert("session-1".to_string(), ContextUsage::new(24.0).unwrap());
        cache.save(&previous).unwrap();

        let latest = snapshot().with_account_id(Some("account-1".to_string()));
        cache.save_preserving_diagnostics(latest).unwrap();
        let saved = cache.load(Provider::Grok).unwrap().unwrap();
        assert_eq!(saved.model.as_deref(), Some("grok-4.6"));
        assert_eq!(saved.session_models["session-1"], "grok-4.6");
        assert!(saved.session_contexts.contains_key("session-1"));
    }

    #[test]
    fn direct_provider_refresh_does_not_leak_global_context_to_a_new_session() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let previous = snapshot().with_context(Some(
            ContextUsage::new(24.0).unwrap().with_cache(Some(
                CacheUsage::from_token_counts(10, 90, 0)
                    .unwrap()
                    .with_session_totals(None, "old-session", 0),
            )),
        ));
        cache.save(&previous).unwrap();

        let mut latest = snapshot();
        cache
            .save_preserving_diagnostics_for_sessions(&mut latest, &["new-session".to_string()])
            .unwrap();
        let saved = cache.load(Provider::Grok).unwrap().unwrap();
        assert!(saved.context_for_session(Some("new-session")).is_none());
    }

    #[test]
    fn direct_provider_diagnostics_remain_bounded() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let mut previous = snapshot();
        for index in 0..(MAX_STATUSLINE_SESSIONS + 8) {
            let session_id = format!("session-{index}");
            previous
                .session_models
                .insert(session_id.clone(), format!("model-{index}"));
            previous
                .session_contexts
                .insert(session_id, ContextUsage::new((index % 100) as f64).unwrap());
        }
        cache.save(&previous).unwrap();

        cache.save_preserving_diagnostics(snapshot()).unwrap();
        let saved = cache.load(Provider::Grok).unwrap().unwrap();
        assert_eq!(saved.session_models.len(), MAX_STATUSLINE_SESSIONS);
        assert_eq!(saved.session_contexts.len(), MAX_STATUSLINE_SESSIONS);
    }

    #[test]
    fn statusline_refresh_preserves_session_totals_only_for_the_same_session() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let previous_cache = CacheUsage::from_token_counts(10, 90, 0)
            .unwrap()
            .with_ttl_estimate(300, 1_000)
            .with_session_totals(
                crate::model::CacheTotals::from_token_counts(10, 90, 0),
                "session-1",
                512,
            );
        cache
            .save(
                &snapshot().with_context(Some(
                    ContextUsage::new(23.5)
                        .unwrap()
                        .with_cache(Some(previous_cache.clone())),
                )),
            )
            .unwrap();

        let same_session = snapshot().with_context(Some(
            ContextUsage::new(24.0).unwrap().with_cache(Some(
                CacheUsage::from_token_counts(1, 2, 3)
                    .unwrap()
                    .with_session_totals(None, "session-1", 0),
            )),
        ));
        cache
            .save_preserving_context_for_session(same_session, Some("session-1"))
            .unwrap();
        let saved = cache.load(Provider::Grok).unwrap().unwrap();
        let saved_cache = saved.context.unwrap().cache.unwrap();
        assert_eq!(saved_cache.session_totals, previous_cache.session_totals);
        assert_eq!(saved_cache.transcript_offset, 512);
        assert_eq!(saved_cache.ttl_seconds, Some(300));

        let new_session = snapshot().with_context(Some(
            ContextUsage::new(25.0).unwrap().with_cache(Some(
                CacheUsage::from_token_counts(1, 2, 3)
                    .unwrap()
                    .with_session_totals(None, "session-2", 0),
            )),
        ));
        cache
            .save_preserving_context_for_session(new_session, Some("session-2"))
            .unwrap();
        let saved_cache = cache
            .load(Provider::Grok)
            .unwrap()
            .unwrap()
            .context
            .unwrap()
            .cache
            .unwrap();
        assert!(saved_cache.session_totals.is_none());
        assert!(saved_cache.ttl_seconds.is_none());
    }

    #[test]
    fn statusline_new_session_does_not_inherit_previous_cache_diagnostics() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let previous = ProviderSnapshot::new(Provider::Claude, vec![], 1).with_context(Some(
            ContextUsage::new(23.5).unwrap().with_cache(Some(
                CacheUsage::from_token_counts(10, 90, 0)
                    .unwrap()
                    .with_session_totals(
                        crate::model::CacheTotals::from_token_counts(10, 90, 0),
                        "session-1",
                        512,
                    ),
            )),
        ));
        cache
            .save_statusline_observation(
                Provider::Claude,
                previous,
                &json!({"session_id": "session-1"}),
            )
            .unwrap();

        let latest = ProviderSnapshot::new(Provider::Claude, vec![], 2)
            .with_context(Some(ContextUsage::new(0.0).unwrap()));
        cache
            .save_statusline_observation(
                Provider::Claude,
                latest,
                &json!({"session_id": "session-2"}),
            )
            .unwrap();

        let saved = cache
            .load_statusline_observation(Provider::Claude)
            .unwrap()
            .unwrap()
            .snapshot;
        assert!(saved
            .context_for_session(Some("session-2"))
            .unwrap()
            .cache
            .is_none());
        assert!(saved.session_contexts.contains_key("session-2"));
    }

    #[test]
    fn statusline_session_diagnostics_remain_bounded() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        for index in 0..(MAX_STATUSLINE_SESSIONS + 8) {
            let session_id = format!("session-{index}");
            cache
                .save_statusline_observation(
                    Provider::Claude,
                    ProviderSnapshot::new(Provider::Claude, vec![], index as u64)
                        .with_model(Some(format!("model-{index}")))
                        .with_context(Some(ContextUsage::new(0.0).unwrap())),
                    &json!({"session_id": session_id}),
                )
                .unwrap();
        }

        let saved = cache
            .load_statusline_observation(Provider::Claude)
            .unwrap()
            .unwrap()
            .snapshot;
        assert_eq!(saved.session_models.len(), MAX_STATUSLINE_SESSIONS);
        assert_eq!(saved.session_contexts.len(), MAX_STATUSLINE_SESSIONS);
        assert_eq!(saved.session_windows.len(), MAX_STATUSLINE_SESSIONS);
        assert!(saved
            .session_models
            .contains_key(&format!("session-{}", MAX_STATUSLINE_SESSIONS + 7)));
    }

    #[test]
    fn missing_cache_is_not_an_error() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        assert_eq!(cache.load(Provider::Claude).unwrap(), None);
    }

    #[test]
    fn statusline_refresh_preserves_an_omitted_five_hour_window() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let previous = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                UsageWindow::new(
                    WindowKind::FiveHour,
                    22.0,
                    Some(ResetAt::from_unix_seconds(2_000)),
                )
                .unwrap(),
                UsageWindow::new(
                    WindowKind::Weekly,
                    65.0,
                    Some(ResetAt::from_unix_seconds(10_000)),
                )
                .unwrap(),
            ],
            1_000,
        );
        cache.save(&previous).unwrap();

        let current = ProviderSnapshot::new(
            Provider::Claude,
            vec![UsageWindow::new(
                WindowKind::Weekly,
                66.0,
                Some(ResetAt::from_unix_seconds(10_000)),
            )
            .unwrap()],
            1_100,
        );
        cache
            .save_preserving_context_for_session(current, Some("session-1"))
            .unwrap();
        let saved = cache.load(Provider::Claude).unwrap().unwrap();
        assert_eq!(
            saved.window(WindowKind::FiveHour).unwrap().used_percent,
            22.0
        );
        assert_eq!(saved.window(WindowKind::Weekly).unwrap().used_percent, 66.0);
        assert_eq!(
            saved
                .windows_for_session(Some("session-1"))
                .iter()
                .find(|window| window.kind == WindowKind::FiveHour)
                .unwrap()
                .used_percent,
            22.0
        );
    }

    #[test]
    fn refresh_marker_debounces_only_within_interval() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        cache.mark_refresh(Provider::Codex, 100).unwrap();
        assert!(cache.should_debounce(Provider::Codex, 120, 60).unwrap());
        assert!(!cache.should_debounce(Provider::Codex, 161, 60).unwrap());
    }

    #[test]
    fn named_turn_lock_is_non_blocking_and_exclusive() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let first = cache.try_lock_named("codex.turn.lock").unwrap();
        assert!(first.is_some());
        let second = cache.try_lock_named("codex.turn.lock").unwrap();
        assert!(second.is_none());
        drop(first);
        assert!(cache.try_lock_named("codex.turn.lock").unwrap().is_some());
    }

    #[test]
    fn provider_refresh_lease_is_non_blocking_and_scoped_to_one_provider() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let first = cache.try_lock_provider_refresh(Provider::Claude).unwrap();
        assert!(first.is_some());
        assert!(cache
            .try_lock_provider_refresh(Provider::Claude)
            .unwrap()
            .is_none());
        assert!(cache
            .try_lock_provider_refresh(Provider::Agy)
            .unwrap()
            .is_some());
    }

    #[test]
    fn watcher_stop_marker_is_reversible_for_reinstall() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        let started_millis = CacheStore::now_millis();
        cache.stop_turn_watchers().unwrap();
        assert!(cache.turn_watchers_stopped_after(started_millis).unwrap());
        cache.clear_turn_watcher_stop().unwrap();
        assert!(!cache.turn_watchers_stopped_after(started_millis).unwrap());
    }

    #[test]
    fn watch_interval_defaults_and_persists_a_safe_custom_value() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        assert_eq!(
            cache.watch_interval_seconds(),
            DEFAULT_WATCH_INTERVAL_SECONDS
        );
        cache.set_watch_interval_seconds(300).unwrap();
        assert_eq!(cache.watch_interval_seconds(), 300);
        cache.clear_watch_interval().unwrap();
        assert_eq!(
            cache.watch_interval_seconds(),
            DEFAULT_WATCH_INTERVAL_SECONDS
        );
    }

    #[test]
    fn watch_interval_rejects_values_that_are_too_short_or_long() {
        assert!(
            CacheStore::validate_watch_interval_seconds(MIN_WATCH_INTERVAL_SECONDS - 1).is_err()
        );
        assert!(
            CacheStore::validate_watch_interval_seconds(MAX_WATCH_INTERVAL_SECONDS + 1).is_err()
        );
        assert!(CacheStore::validate_watch_interval_seconds(MIN_WATCH_INTERVAL_SECONDS).is_ok());
        assert!(CacheStore::validate_watch_interval_seconds(MAX_WATCH_INTERVAL_SECONDS).is_ok());
    }
}
