use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Codex,
    Grok,
    Claude,
    Agy,
}

impl Provider {
    pub const ALL: [Self; 4] = [Self::Codex, Self::Grok, Self::Claude, Self::Agy];

    pub fn badge(self) -> &'static str {
        match self {
            Self::Codex => "[C]",
            Self::Grok => "[X]",
            Self::Claude => "[A]",
            Self::Agy => "[G]",
        }
    }

    /// Compact text marker for a narrow Herdr sidebar. Plugin v1 accepts text
    /// tokens rather than provider SVGs, so the letters keep it recognizable.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Codex => "◈C",
            Self::Grok => "✕G",
            Self::Claude => "✦Cl",
            Self::Agy => "△Ag",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Grok => "Grok",
            Self::Claude => "Claude",
            Self::Agy => "Agy",
        }
    }

    pub fn source(self) -> &'static str {
        match self {
            Self::Codex => "codex-app-server",
            Self::Grok => "grok-cli-billing",
            Self::Claude => "claude-statusline",
            Self::Agy => "agy-statusline",
        }
    }
}

impl std::str::FromStr for Provider {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "codex" => Ok(Self::Codex),
            "grok" => Ok(Self::Grok),
            "claude" | "claude-code" | "anthropic" => Ok(Self::Claude),
            "agy" | "antigravity" | "antigravity-cli" => Ok(Self::Agy),
            other => Err(ModelError::UnknownProvider(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    FiveHour,
    Weekly,
}

impl WindowKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::FiveHour => "5h",
            Self::Weekly => "week",
        }
    }

    pub fn duration_seconds(self) -> u64 {
        match self {
            Self::FiveHour => 5 * 60 * 60,
            Self::Weekly => 7 * 24 * 60 * 60,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ResetAt(u64);

impl ResetAt {
    pub fn from_unix_seconds(seconds: u64) -> Self {
        Self(seconds)
    }

    pub fn parse_rfc3339(value: &str) -> Option<Self> {
        let timestamp = OffsetDateTime::parse(value, &Rfc3339)
            .ok()?
            .unix_timestamp();
        u64::try_from(timestamp).ok().map(Self)
    }

    pub fn parse(value: &str) -> Option<Self> {
        value
            .parse::<u64>()
            .ok()
            .map(Self)
            .or_else(|| Self::parse_rfc3339(value))
    }

    pub fn after(base_unix: u64, seconds: u64) -> Self {
        Self(base_unix.saturating_add(seconds))
    }

    pub fn unix_seconds(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for ResetAt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Unix(u64),
            Text(String),
        }

        match Repr::deserialize(deserializer)? {
            Repr::Unix(value) => Ok(Self(value)),
            Repr::Text(value) => Self::parse(&value).ok_or_else(|| {
                serde::de::Error::custom("reset time is not Unix seconds or RFC 3339")
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    pub kind: WindowKind,
    pub used_percent: f64,
    pub remaining_percent: f64,
    pub resets_at: Option<ResetAt>,
}

impl UsageWindow {
    pub fn new(
        kind: WindowKind,
        used_percent: f64,
        resets_at: Option<ResetAt>,
    ) -> Result<Self, ModelError> {
        if !used_percent.is_finite() || !(0.0..=100.0).contains(&used_percent) {
            return Err(ModelError::InvalidPercentage(used_percent));
        }
        Ok(Self {
            kind,
            used_percent,
            remaining_percent: (100.0 - used_percent).clamp(0.0, 100.0),
            resets_at,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextUsage {
    pub used_percent: f64,
    #[serde(default)]
    pub cache: Option<CacheUsage>,
}

impl ContextUsage {
    pub fn new(used_percent: f64) -> Result<Self, ModelError> {
        if !used_percent.is_finite() || !(0.0..=100.0).contains(&used_percent) {
            return Err(ModelError::InvalidPercentage(used_percent));
        }
        Ok(Self {
            used_percent,
            cache: None,
        })
    }

    pub fn with_cache(mut self, cache: Option<CacheUsage>) -> Self {
        self.cache = cache;
        self
    }
}

/// Cache counters reported for the latest provider request.
///
/// The provider statusLine payloads expose uncached input, cache creation, and
/// cache reads. Keeping the raw counters alongside the derived percentage
/// makes the displayed ratio auditable and leaves room for richer diagnostics
/// without another provider request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheUsage {
    pub fresh_input_tokens: u64,
    pub read_tokens: u64,
    pub creation_tokens: u64,
    pub hit_percent: f64,
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
    #[serde(default)]
    pub last_activity_unix: Option<u64>,
    /// Cumulative cache counters for the current provider session.
    ///
    /// `current_usage` is a latest-request view for Claude/Agy, so the
    /// sidebar uses this optional aggregate when a local transcript gives us
    /// a trustworthy session boundary and offset.
    #[serde(default)]
    pub session_totals: Option<CacheTotals>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub transcript_offset: u64,
}

/// Cache counters accumulated across all completed requests in one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheTotals {
    pub fresh_input_tokens: u64,
    pub read_tokens: u64,
    pub creation_tokens: u64,
    pub hit_percent: f64,
}

impl CacheTotals {
    pub fn from_token_counts(
        fresh_input_tokens: u64,
        read_tokens: u64,
        creation_tokens: u64,
    ) -> Option<Self> {
        let total = fresh_input_tokens
            .saturating_add(read_tokens)
            .saturating_add(creation_tokens);
        if total == 0 {
            return None;
        }
        Some(Self {
            fresh_input_tokens,
            read_tokens,
            creation_tokens,
            hit_percent: read_tokens as f64 / total as f64 * 100.0,
        })
    }

    pub fn add_token_counts(
        &mut self,
        fresh_input_tokens: u64,
        read_tokens: u64,
        creation_tokens: u64,
    ) {
        self.fresh_input_tokens = self.fresh_input_tokens.saturating_add(fresh_input_tokens);
        self.read_tokens = self.read_tokens.saturating_add(read_tokens);
        self.creation_tokens = self.creation_tokens.saturating_add(creation_tokens);
        let total = self
            .fresh_input_tokens
            .saturating_add(self.read_tokens)
            .saturating_add(self.creation_tokens);
        self.hit_percent = if total == 0 {
            0.0
        } else {
            self.read_tokens as f64 / total as f64 * 100.0
        };
    }
}

impl CacheUsage {
    pub fn from_token_counts(
        fresh_input_tokens: u64,
        read_tokens: u64,
        creation_tokens: u64,
    ) -> Option<Self> {
        let total = fresh_input_tokens
            .saturating_add(read_tokens)
            .saturating_add(creation_tokens);
        if total == 0 {
            return None;
        }
        Some(Self {
            fresh_input_tokens,
            read_tokens,
            creation_tokens,
            hit_percent: read_tokens as f64 / total as f64 * 100.0,
            ttl_seconds: None,
            last_activity_unix: None,
            session_totals: None,
            session_id: None,
            transcript_offset: 0,
        })
    }

    pub fn with_ttl_estimate(mut self, ttl_seconds: u64, last_activity_unix: u64) -> Self {
        self.ttl_seconds = Some(ttl_seconds);
        self.last_activity_unix = Some(last_activity_unix);
        self
    }

    pub fn remaining_ttl_seconds(&self, now_unix: u64) -> Option<u64> {
        let ttl = self.ttl_seconds?;
        let last_activity = self.last_activity_unix?;
        Some(last_activity.saturating_add(ttl).saturating_sub(now_unix))
    }

    pub fn with_session_totals(
        mut self,
        totals: Option<CacheTotals>,
        session_id: impl Into<String>,
        transcript_offset: u64,
    ) -> Self {
        self.session_totals = totals;
        self.session_id = Some(session_id.into());
        self.transcript_offset = transcript_offset;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSnapshot {
    pub provider: Provider,
    pub source: String,
    pub fetched_at_unix: u64,
    pub windows: Vec<UsageWindow>,
    #[serde(default)]
    pub context: Option<ContextUsage>,
    #[serde(default)]
    pub session_summaries: BTreeMap<String, String>,
    /// Login identity the snapshot was fetched for (Grok `user_id`, Codex
    /// `tokens.account_id`). Used to drop another account's cached quota after
    /// `grok login` / Codex account switch. Absent on snapshots written before
    /// this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

impl ProviderSnapshot {
    pub fn new(provider: Provider, windows: Vec<UsageWindow>, fetched_at_unix: u64) -> Self {
        Self {
            provider,
            source: provider.source().to_string(),
            fetched_at_unix,
            windows,
            context: None,
            session_summaries: BTreeMap::new(),
            account_id: None,
        }
    }

    pub fn with_context(mut self, context: Option<ContextUsage>) -> Self {
        self.context = context;
        self
    }

    pub fn with_account_id(mut self, account_id: Option<String>) -> Self {
        self.account_id = account_id;
        self
    }

    /// Whether this cached snapshot still belongs to the signed-in account.
    ///
    /// A failed refresh must keep the last good value for the *current*
    /// account, not a previous login. After an account switch:
    /// - snapshots stamped with another `account_id` are unusable;
    /// - legacy snapshots (no stamp) are unusable when the credential file is
    ///   newer than `fetched_at_unix`, which is what `grok login` does.
    pub fn usable_for_account(
        &self,
        current_account_id: Option<&str>,
        credentials_mtime_unix: Option<u64>,
    ) -> bool {
        match (self.account_id.as_deref(), current_account_id) {
            (Some(saved), Some(current)) => saved == current,
            (Some(_), None) => false,
            (None, Some(_)) => {
                credentials_mtime_unix.is_none_or(|mtime| mtime <= self.fetched_at_unix)
            }
            (None, None) => true,
        }
    }

    pub fn window(&self, kind: WindowKind) -> Option<&UsageWindow> {
        self.windows.iter().find(|window| window.kind == kind)
    }

    pub fn severity(&self, now_unix: u64) -> Severity {
        let relevant = match self.provider {
            Provider::Codex | Provider::Grok => self.window(WindowKind::Weekly),
            Provider::Claude | Provider::Agy => self
                .window(WindowKind::FiveHour)
                .or_else(|| self.window(WindowKind::Weekly)),
        };
        relevant
            .map(|window| Severity::for_window(window, now_unix))
            .unwrap_or(Severity::Unknown)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Normal,
    Warning,
    Danger,
    Unknown,
}

impl Severity {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Normal => "●",
            Self::Warning => "▲",
            Self::Danger => "!",
            Self::Unknown => "?",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "OK",
            Self::Warning => "WARN",
            Self::Danger => "LOW",
            Self::Unknown => "N/A",
        }
    }

    pub fn for_window(window: &UsageWindow, now_unix: u64) -> Self {
        let Some(reset_at) = window.resets_at else {
            return Self::Unknown;
        };
        let remaining_seconds = reset_at.unix_seconds().saturating_sub(now_unix);
        if remaining_seconds == 0 {
            return Self::Unknown;
        }

        let remaining_time_percent = remaining_seconds.min(window.kind.duration_seconds()) as f64
            / window.kind.duration_seconds() as f64
            * 100.0;
        if window.remaining_percent >= remaining_time_percent {
            Self::Normal
        } else if window.remaining_percent < 20.0 {
            Self::Danger
        } else {
            Self::Warning
        }
    }
}

pub fn format_percent(value: f64) -> String {
    format!("{value:.0}")
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("unknown provider: {0}")]
    UnknownProvider(String),
    #[error("percentage must be finite and between 0 and 100, got {0}")]
    InvalidPercentage(f64),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(kind: WindowKind, used: f64) -> UsageWindow {
        UsageWindow::new(kind, used, None).expect("fixture percentage is valid")
    }

    #[test]
    fn remaining_percentage_is_derived_from_used_percentage() {
        let value = window(WindowKind::Weekly, 42.5);
        assert_eq!(value.remaining_percent, 57.5);
        assert_eq!(format_percent(value.remaining_percent), "58");
    }

    #[test]
    fn cache_hit_ratio_uses_fresh_creation_and_read_tokens() {
        let cache = CacheUsage::from_token_counts(100, 800, 100).unwrap();
        assert_eq!(cache.hit_percent, 80.0);
        assert_eq!(CacheUsage::from_token_counts(0, 0, 0), None);
        assert_eq!(
            CacheUsage::from_token_counts(100, 0, 0)
                .unwrap()
                .hit_percent,
            0.0
        );
    }

    #[test]
    fn session_cache_totals_accumulate_and_recompute_hit_ratio() {
        let mut totals = CacheTotals::from_token_counts(100, 800, 100).unwrap();
        totals.add_token_counts(100, 0, 0);
        assert_eq!(totals.fresh_input_tokens, 200);
        assert_eq!(totals.read_tokens, 800);
        assert_eq!(totals.creation_tokens, 100);
        assert_eq!(totals.hit_percent, 72.72727272727273);
    }

    #[test]
    fn old_context_snapshots_deserialize_without_cache_fields() {
        let context: ContextUsage = serde_json::from_str(r#"{"used_percent":23.5}"#).unwrap();
        assert_eq!(context.used_percent, 23.5);
        assert!(context.cache.is_none());
    }

    #[test]
    fn cached_snapshot_from_another_account_is_not_usable() {
        let snapshot = ProviderSnapshot::new(Provider::Grok, vec![], 100)
            .with_account_id(Some("account-a".to_string()));
        assert!(!snapshot.usable_for_account(Some("account-b"), Some(50)));
        assert!(snapshot.usable_for_account(Some("account-a"), Some(200)));
    }

    #[test]
    fn legacy_snapshot_is_dropped_when_credentials_are_newer_than_the_fetch() {
        let snapshot = ProviderSnapshot::new(Provider::Grok, vec![], 100);
        assert!(!snapshot.usable_for_account(Some("account-b"), Some(150)));
        assert!(snapshot.usable_for_account(Some("account-b"), Some(100)));
        assert!(snapshot.usable_for_account(Some("account-b"), Some(50)));
    }

    #[test]
    fn old_snapshots_deserialize_without_account_id() {
        let snapshot: ProviderSnapshot = serde_json::from_str(
            r#"{"provider":"grok","source":"grok-cli-billing","fetched_at_unix":1,"windows":[]}"#,
        )
        .unwrap();
        assert_eq!(snapshot.account_id, None);
    }

    #[test]
    fn approximate_cache_ttl_saturates_after_expiry() {
        let cache = CacheUsage::from_token_counts(1, 1, 0)
            .unwrap()
            .with_ttl_estimate(300, 1_000);
        assert_eq!(cache.remaining_ttl_seconds(1_100), Some(200));
        assert_eq!(cache.remaining_ttl_seconds(1_301), Some(0));
    }

    #[test]
    fn reset_time_deserializes_new_unix_and_legacy_rfc3339_cache_values() {
        let unix: ResetAt = serde_json::from_str("1787400000").unwrap();
        let legacy: ResetAt = serde_json::from_str("\"2026-08-22T12:00:00Z\"").unwrap();
        assert_eq!(unix, ResetAt::from_unix_seconds(1_787_400_000));
        assert_eq!(legacy, unix);
        assert_eq!(serde_json::to_string(&unix).unwrap(), "1787400000");
    }

    #[test]
    fn severity_compares_quota_runway_with_time_remaining() {
        let now = 1_000_000;
        let reset = ResetAt::after(now, WindowKind::Weekly.duration_seconds() / 2);

        let healthy = UsageWindow::new(WindowKind::Weekly, 40.0, Some(reset)).unwrap();
        let behind = UsageWindow::new(WindowKind::Weekly, 60.0, Some(reset)).unwrap();
        let danger = UsageWindow::new(WindowKind::Weekly, 85.0, Some(reset)).unwrap();

        assert_eq!(Severity::for_window(&healthy, now), Severity::Normal);
        assert_eq!(Severity::for_window(&behind, now), Severity::Warning);
        assert_eq!(Severity::for_window(&danger, now), Severity::Danger);
    }

    #[test]
    fn low_quota_is_safe_when_reset_is_close() {
        let now = 1_000_000;
        let reset = ResetAt::after(now, WindowKind::Weekly.duration_seconds() / 10);
        let window = UsageWindow::new(WindowKind::Weekly, 85.0, Some(reset)).unwrap();

        assert_eq!(Severity::for_window(&window, now), Severity::Normal);
    }

    #[test]
    fn severity_is_unknown_without_a_current_reset_time() {
        let window = UsageWindow::new(WindowKind::Weekly, 85.0, None).unwrap();
        assert_eq!(Severity::for_window(&window, 1_000_000), Severity::Unknown);

        let expired = UsageWindow::new(
            WindowKind::Weekly,
            85.0,
            Some(ResetAt::from_unix_seconds(999_999)),
        )
        .unwrap();
        assert_eq!(Severity::for_window(&expired, 1_000_000), Severity::Unknown);
    }

    #[test]
    fn provider_aliases_are_explicit() {
        assert_eq!("claude-code".parse::<Provider>().unwrap(), Provider::Claude);
        assert_eq!("antigravity".parse::<Provider>().unwrap(), Provider::Agy);
        assert_eq!(Provider::Grok.badge(), "[X]");
        assert_eq!(Provider::Codex.icon(), "◈C");
        assert_eq!(Provider::Claude.icon(), "✦Cl");
    }
}
