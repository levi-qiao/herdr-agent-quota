use crate::model::{ContextUsage, Provider, ProviderSnapshot};
use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_WATCH_INTERVAL_SECONDS: u64 = 60;
pub const MIN_WATCH_INTERVAL_SECONDS: u64 = 30;
pub const MAX_WATCH_INTERVAL_SECONDS: u64 = 60 * 60;
const WATCH_INTERVAL_ENV: &str = "HERDR_AGENT_QUOTA_WATCH_INTERVAL_SECONDS";
const WATCH_INTERVAL_FILE: &str = "watch-interval-seconds";

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
        let session_id = observation
            .get("session_id")
            .or_else(|| observation.get("sessionId"))
            .and_then(Value::as_str);
        if let Some(session_id) = session_id {
            if let Some(cache) = snapshot
                .context
                .as_mut()
                .and_then(|context| context.cache.as_mut())
            {
                cache.session_id = Some(session_id.to_string());
            }
        }
        let previous = self
            .load_statusline_observation(provider)
            .ok()
            .flatten()
            .and_then(|observation| observation.snapshot.context);
        merge_preserved_context(&mut snapshot, previous, session_id);
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
    /// response and immediately after compaction). Keep the last known value
    /// while still replacing the quota windows with the newest snapshot.
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
        let previous = self
            .load(snapshot.provider)
            .ok()
            .flatten()
            .and_then(|snapshot| snapshot.context);
        merge_preserved_context(&mut snapshot, previous, session_id);
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
        self.try_lock_named(&format!("{}.refresh.lock", provider.source()))
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

fn strip_session_cache_state(cache: &mut crate::model::CacheUsage, clear_ttl: bool) {
    cache.session_totals = None;
    cache.session_id = None;
    cache.transcript_offset = 0;
    if clear_ttl {
        cache.ttl_seconds = None;
        cache.last_activity_unix = None;
    }
}

fn merge_preserved_context(
    snapshot: &mut ProviderSnapshot,
    previous: Option<ContextUsage>,
    session_id: Option<&str>,
) {
    let Some(previous_context) = previous else {
        return;
    };
    match (&mut snapshot.context, previous_context) {
        (None, mut previous_context) => {
            if let Some(cache) = previous_context.cache.as_mut() {
                let same_session = session_id
                    .is_some_and(|session_id| cache.session_id.as_deref() == Some(session_id));
                if !same_session {
                    let clear_ttl = cache.session_id.is_some() || session_id.is_some();
                    strip_session_cache_state(cache, clear_ttl);
                }
            }
            snapshot.context = Some(previous_context);
        }
        (Some(current), previous_context) if current.cache.is_none() => {
            let mut previous_cache = previous_context.cache.clone();
            let same_session = session_id.is_some_and(|session_id| {
                previous_cache
                    .as_ref()
                    .and_then(|cache| cache.session_id.as_deref())
                    == Some(session_id)
            });
            if !same_session {
                if let Some(cache) = previous_cache.as_mut() {
                    let clear_ttl = cache.session_id.is_some() || session_id.is_some();
                    strip_session_cache_state(cache, clear_ttl);
                }
            }
            current.cache = previous_cache;
        }
        (Some(current), previous_context) => {
            let Some(current_cache) = current.cache.as_mut() else {
                return;
            };
            let Some(previous_cache) = previous_context.cache.as_ref() else {
                return;
            };
            let same_session = current_cache.session_id.is_some()
                && current_cache.session_id == previous_cache.session_id
                && session_id.is_none_or(|session_id| {
                    current_cache.session_id.as_deref() == Some(session_id)
                });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CacheUsage, ContextUsage, Provider, UsageWindow, WindowKind};
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
    fn missing_cache_is_not_an_error() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        assert_eq!(cache.load(Provider::Claude).unwrap(), None);
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
        cache.stop_turn_watchers().unwrap();
        assert!(cache
            .turn_watchers_stopped_after(CacheStore::now_millis().saturating_sub(1))
            .unwrap());
        cache.clear_turn_watcher_stop().unwrap();
        assert!(!cache
            .turn_watchers_stopped_after(CacheStore::now_millis().saturating_sub(1))
            .unwrap());
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
