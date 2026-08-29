use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

/// Original-four quota collector (the subscription billed for a pane).
///
/// Distinct from [`Harness`], the Herdr agent drawing the pane. A Herdr agent
/// name is not itself a collector: parse it as a harness first, then take
/// [`Harness::billing`]. Cache filenames and the `provider` serde tag stay
/// 1:1 with 0.2 snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Codex,
    Grok,
    Claude,
    Agy,
    /// OpenCode's Go subscription. Deliberately absent from [`Provider::ALL`]:
    /// it has no 1:1 harness mapping and is only ever fetched for a pane that
    /// resolved to it, so the original four keep their exact refresh behavior.
    OpenCodeGo,
}

/// Quota collector identity. The original four keep the historical
/// [`Provider`] name so 0.2 cache files and CLI flags stay compatible.
pub type Billing = Provider;

impl Provider {
    /// The collectors a bare `--provider all` refreshes. OpenCode Go is not
    /// here on purpose; see the variant's note.
    pub const ALL: [Self; 4] = [Self::Codex, Self::Grok, Self::Claude, Self::Agy];

    pub fn badge(self) -> &'static str {
        match self {
            Self::Codex => "[C]",
            Self::Grok => "[X]",
            Self::Claude => "[A]",
            Self::Agy => "[G]",
            Self::OpenCodeGo => "[O]",
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
            Self::OpenCodeGo => "◇Go",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Grok => "Grok",
            Self::Claude => "Claude",
            Self::Agy => "Agy",
            Self::OpenCodeGo => "OpenCode Go",
        }
    }

    pub fn source(self) -> &'static str {
        match self {
            Self::Codex => "codex-app-server",
            Self::Grok => "grok-cli-billing",
            Self::Claude => "claude-statusline",
            Self::Agy => "agy-statusline",
            // Scoped to the OpenCode credential store so it can never collide
            // with the original four's 0.2 filenames.
            Self::OpenCodeGo => "opencode-go.opencode-store",
        }
    }

    /// Whether this provider has a five-hour quota window to display.
    ///
    /// Grok's credits contract has no 5h bucket, so the sidebar must not
    /// invent a `5h N/A` row for it. Codex, Claude, and Agy all expose one.
    pub fn exposes_five_hour_quota(self) -> bool {
        !matches!(self, Self::Grok)
    }
}

/// The agent drawing a Herdr pane. Distinct from [`Billing`]: harnesses without
/// a 1:1 collector may still resolve an exact session to a scoped billing
/// target (for example, Pi to canonical Codex after an account-id match).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Harness {
    Codex,
    Grok,
    Claude,
    Agy,
    OpenCode,
    Pi,
    Omp,
    Kimi,
}

impl Harness {
    /// Classify a Herdr `agent` field. Unknown names are `None`, not a
    /// collector fallback.
    pub fn from_agent_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "codex" => Some(Self::Codex),
            "grok" => Some(Self::Grok),
            "claude" | "claude-code" | "anthropic" => Some(Self::Claude),
            "agy" | "antigravity" | "antigravity-cli" => Some(Self::Agy),
            "opencode" => Some(Self::OpenCode),
            "pi" => Some(Self::Pi),
            "omp" => Some(Self::Omp),
            "kimi" => Some(Self::Kimi),
            _ => None,
        }
    }

    /// Original-four 1:1 map. Named harnesses without a collector, and
    /// unknown names, return `None`.
    pub fn billing(self) -> Option<Billing> {
        match self {
            Self::Codex => Some(Provider::Codex),
            Self::Grok => Some(Provider::Grok),
            Self::Claude => Some(Provider::Claude),
            Self::Agy => Some(Provider::Agy),
            Self::OpenCode | Self::Pi | Self::Omp | Self::Kimi => None,
        }
    }

    pub fn billing_for_agent(name: &str) -> Option<Billing> {
        Self::from_agent_name(name).and_then(Self::billing)
    }
}

/// Opaque local identity for a credential store. Not a token, path, or account id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CredentialScope(&'static str);

impl CredentialScope {
    /// Canonical CLI stores for the original four collectors.
    pub const CANONICAL: Self = Self("canonical");
    /// OpenCode default data store (`$XDG_DATA_HOME/opencode` or `~/.local/share/opencode`).
    pub const OPENCODE_STORE: Self = Self("opencode-store");

    pub fn as_str(self) -> &'static str {
        self.0
    }
}

/// Subscription paying for a pane, scoped to one credential store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BillingTarget {
    pub billing: Provider,
    pub credential_scope: CredentialScope,
}

impl BillingTarget {
    pub fn original_four(provider: Provider) -> Self {
        Self {
            billing: provider,
            credential_scope: CredentialScope::CANONICAL,
        }
    }

    pub fn opencode_go() -> Self {
        Self {
            billing: Provider::OpenCodeGo,
            credential_scope: CredentialScope::OPENCODE_STORE,
        }
    }

    /// The billing identity when it is one of the original four collectors.
    ///
    /// Those are refreshed through the provider list; anything else is fetched
    /// only for the pane that resolved to it.
    pub fn original_provider(self) -> Option<Provider> {
        Provider::ALL
            .contains(&self.billing)
            .then_some(self.billing)
    }

    /// Cache, lease, and refresh-marker filename stem.
    ///
    /// One authority for every target: the original four keep their 0.2 source
    /// ids, and a scoped target carries its credential scope in the stem so it
    /// cannot collide with them.
    pub fn cache_identity(self) -> String {
        self.billing.source().to_string()
    }
}

/// Result of attributing a pane to a subscription. Uncertain evidence never
/// guesses from the number of credentials on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Subscription(BillingTarget),
    NoSubscription,
    Indeterminate,
}

impl std::str::FromStr for Provider {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Harness::billing_for_agent(value)
            .ok_or_else(|| ModelError::UnknownProvider(value.trim().to_ascii_lowercase()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    FiveHour,
    Weekly,
    /// Cached and rendered in the dashboard only. The sidebar has no monthly
    /// token, and a 30d value must never be published through a weekly one.
    Monthly,
}

impl WindowKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::FiveHour => "5h",
            Self::Weekly => "7d",
            Self::Monthly => "30d",
        }
    }

    pub fn duration_seconds(self) -> u64 {
        match self {
            Self::FiveHour => 5 * 60 * 60,
            Self::Weekly => 7 * 24 * 60 * 60,
            Self::Monthly => 30 * 24 * 60 * 60,
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
    /// Human-readable name of the model most recently reported by a provider.
    ///
    /// StatusLine providers also keep the per-session value below so panes
    /// running the same provider can be distinguished from one another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub session_summaries: BTreeMap<String, String>,
    #[serde(default)]
    pub session_models: BTreeMap<String, String>,
    /// Context/cache diagnostics keyed by the provider's session id. Keeping
    /// this per session prevents one provider pane from displaying another
    /// pane's local rollout usage.
    #[serde(default)]
    pub session_contexts: BTreeMap<String, ContextUsage>,
    /// Account quota windows keyed by the provider's session id.
    ///
    /// Quota itself is account-level for every provider. Grok and Codex fetch
    /// one login's windows and leave this map empty. StatusLine providers
    /// (Claude, Agy) can run two signed-in accounts into one cache file, and
    /// the top-level `windows` field only holds whichever account ticked last;
    /// those ticks are stored here so a pane can read its own account.
    #[serde(default)]
    pub session_windows: BTreeMap<String, Vec<UsageWindow>>,
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
            model: None,
            session_summaries: BTreeMap::new(),
            session_models: BTreeMap::new(),
            session_contexts: BTreeMap::new(),
            session_windows: BTreeMap::new(),
            account_id: None,
        }
    }

    pub fn with_context(mut self, context: Option<ContextUsage>) -> Self {
        self.context = context;
        self
    }

    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    /// Return the model for a pane's session. A known session never falls back
    /// to provider-level data, because that value may belong to another pane.
    pub fn model_for_session(&self, session_id: Option<&str>) -> Option<&str> {
        let Some(session_id) = session_id else {
            return self.model.as_deref();
        };
        if let Some(model) = self.session_models.get(session_id) {
            return Some(model);
        }
        None
    }

    /// Return context/cache diagnostics for a pane's session. A known session
    /// never falls back to provider-level data, because an older snapshot may
    /// belong to another pane. The global value is used only when the caller
    /// has no session id at all.
    pub fn context_for_session(&self, session_id: Option<&str>) -> Option<&ContextUsage> {
        let Some(session_id) = session_id else {
            return self.context.as_ref();
        };
        if let Some(context) = self.session_contexts.get(session_id) {
            return Some(context);
        }
        None
    }

    /// Return the quota windows for a pane's session.
    ///
    /// Context and model are session-local, so a known session never falls
    /// back to the provider-level value. Quota is account-level: Grok and
    /// Codex share one login's windows across every pane, and this map stays
    /// empty for them. StatusLine providers fill the map; a known session
    /// missing from a non-empty map must not borrow another account's
    /// numbers. The top-level `windows` field is used when Herdr has no
    /// session id, or when no session has reported windows yet (legacy cache).
    pub fn windows_for_session(&self, session_id: Option<&str>) -> &[UsageWindow] {
        let Some(session_id) = session_id else {
            return &self.windows;
        };
        if let Some(windows) = self.session_windows.get(session_id) {
            return windows;
        }
        if self.session_windows.is_empty() {
            return &self.windows;
        }
        &[]
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
        window_in(&self.windows, kind)
    }

    /// Keep a previously observed quota window when the latest payload omits
    /// it. Upstream often drops the short window for a tick (Claude statusLine
    /// without `five_hour`, Codex `secondary: null` after a reset credit).
    ///
    /// This never invents percentages. An omitted window is restored only when
    /// its reset is still in the future and a sibling window present in both
    /// snapshots has not itself reset. An empty `windows` list still means
    /// "rate limits were absent — clear stale quota".
    pub fn merge_omitted_windows(&mut self, previous: &Self) {
        if !same_quota_account(self, previous) {
            return;
        }
        merge_omitted_window_list(&mut self.windows, &previous.windows, self.fetched_at_unix);
    }

    pub fn severity(&self, now_unix: u64) -> Severity {
        Self::severity_for_windows(self.provider, &self.windows, now_unix)
    }

    /// Same runway-health calculation as [`Self::severity`], but over an
    /// explicit window slice so a pane can be scored against its own
    /// session/account windows instead of the provider-wide top-level ones.
    pub fn severity_for_windows(
        provider: Provider,
        windows: &[UsageWindow],
        now_unix: u64,
    ) -> Severity {
        let relevant = match provider {
            Provider::Grok => window_in(windows, WindowKind::Weekly),
            Provider::Codex | Provider::Claude | Provider::Agy | Provider::OpenCodeGo => {
                window_in(windows, WindowKind::FiveHour)
                    .or_else(|| window_in(windows, WindowKind::Weekly))
            }
        };
        relevant
            .map(|window| Severity::for_window(window, now_unix))
            .unwrap_or(Severity::Unknown)
    }
}

pub(crate) fn window_in(windows: &[UsageWindow], kind: WindowKind) -> Option<&UsageWindow> {
    windows.iter().find(|window| window.kind == kind)
}

/// Restore an omitted 5h/weekly window from a previous observation of the
/// *same* account or session. Callers that key windows by session must pass
/// that session's previous list, not another account's top-level snapshot.
pub(crate) fn merge_omitted_window_list(
    windows: &mut Vec<UsageWindow>,
    previous: &[UsageWindow],
    fetched_at_unix: u64,
) {
    if windows.is_empty() {
        return;
    }
    if sibling_quota_reset_in(windows, previous) {
        return;
    }
    for kind in [WindowKind::FiveHour, WindowKind::Weekly] {
        if window_in(windows, kind).is_some() {
            continue;
        }
        let Some(previous_window) = window_in(previous, kind).cloned() else {
            continue;
        };
        let Some(reset) = previous_window.resets_at else {
            continue;
        };
        if reset.unix_seconds() <= fetched_at_unix {
            continue;
        }
        windows.push(previous_window);
    }
}

fn same_quota_account(current: &ProviderSnapshot, previous: &ProviderSnapshot) -> bool {
    match (
        current.account_id.as_deref(),
        previous.account_id.as_deref(),
    ) {
        (Some(current_id), Some(previous_id)) => current_id == previous_id,
        _ => true,
    }
}

fn sibling_quota_reset_in(current: &[UsageWindow], previous: &[UsageWindow]) -> bool {
    const USED_PERCENT_RESET_DROP: f64 = 5.0;
    [WindowKind::FiveHour, WindowKind::Weekly]
        .into_iter()
        .any(|kind| {
            let (Some(current_window), Some(previous_window)) =
                (window_in(current, kind), window_in(previous, kind))
            else {
                return false;
            };
            if let (Some(current_reset), Some(previous_reset)) =
                (current_window.resets_at, previous_window.resets_at)
            {
                if current_reset != previous_reset {
                    return true;
                }
            }
            current_window.used_percent + USED_PERCENT_RESET_DROP < previous_window.used_percent
        })
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
    fn legacy_statusline_context_is_not_reused_for_an_unknown_session() {
        let cache = CacheUsage::from_token_counts(10, 90, 0)
            .unwrap()
            .with_session_totals(CacheTotals::from_token_counts(10, 90, 0), "old-session", 0);
        let snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 0).with_context(Some(
            ContextUsage::new(23.5).unwrap().with_cache(Some(cache)),
        ));
        assert!(snapshot.context_for_session(Some("new-session")).is_none());
    }

    #[test]
    fn legacy_local_context_is_not_reused_for_an_unknown_session() {
        let cache = CacheUsage::from_token_counts(10, 90, 0)
            .unwrap()
            .with_session_totals(CacheTotals::from_token_counts(10, 90, 0), "old-session", 0);
        for provider in [Provider::Codex, Provider::Grok] {
            let snapshot = ProviderSnapshot::new(provider, vec![], 0).with_context(Some(
                ContextUsage::new(23.5)
                    .unwrap()
                    .with_cache(Some(cache.clone())),
            ));
            assert!(
                snapshot.context_for_session(Some("new-session")).is_none(),
                "{provider:?} leaked provider-level context into a new session"
            );
        }
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
    fn codex_severity_prefers_the_five_hour_window_when_available() {
        let now = 1_000_000;
        let snapshot = ProviderSnapshot::new(
            Provider::Codex,
            vec![
                UsageWindow::new(
                    WindowKind::FiveHour,
                    90.0,
                    Some(ResetAt::after(
                        now,
                        WindowKind::FiveHour.duration_seconds() / 2,
                    )),
                )
                .unwrap(),
                UsageWindow::new(
                    WindowKind::Weekly,
                    10.0,
                    Some(ResetAt::after(
                        now,
                        WindowKind::Weekly.duration_seconds() / 2,
                    )),
                )
                .unwrap(),
            ],
            now,
        );
        assert_eq!(snapshot.severity(now), Severity::Danger);
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
        assert!(Provider::Codex.exposes_five_hour_quota());
        assert!(Provider::Claude.exposes_five_hour_quota());
        assert!(!Provider::Grok.exposes_five_hour_quota());
        assert!("opencode".parse::<Provider>().is_err());
        assert!("OpenCode".parse::<Provider>().is_err());
        assert!("pi".parse::<Provider>().is_err());
    }

    #[test]
    fn opencode_go_cache_identity_cannot_borrow_original_four_files() {
        let target = BillingTarget::opencode_go();
        assert_eq!(target.cache_identity(), "opencode-go.opencode-store");
        assert_eq!(target.credential_scope, CredentialScope::OPENCODE_STORE);
        assert!(target.original_provider().is_none());
        for provider in Provider::ALL {
            let original = BillingTarget::original_four(provider);
            assert_eq!(original.cache_identity(), provider.source());
            assert_eq!(original.credential_scope, CredentialScope::CANONICAL);
            assert_ne!(target.cache_identity(), original.cache_identity());
            assert!(!target.cache_identity().contains(provider.source()));
        }
    }

    #[test]
    fn harness_identity_is_not_a_quota_collector() {
        assert_eq!(
            Harness::from_agent_name("OpenCode"),
            Some(Harness::OpenCode)
        );
        assert_eq!(
            Harness::from_agent_name("opencode"),
            Some(Harness::OpenCode)
        );
        assert_eq!(Harness::billing_for_agent("opencode"), None);
        assert_eq!(Harness::billing_for_agent("pi"), None);
        assert_eq!(Harness::billing_for_agent("cursor"), None);
        assert_eq!(
            Harness::billing_for_agent("claude-code"),
            Some(Provider::Claude)
        );
        assert_eq!(
            Harness::billing_for_agent("antigravity"),
            Some(Provider::Agy)
        );
        assert_eq!(Harness::billing_for_agent("codex"), Some(Provider::Codex));
        assert_eq!(Harness::billing_for_agent("grok"), Some(Provider::Grok));
    }

    #[test]
    fn original_four_v0_2_snapshots_deserialize_with_canonical_sources() {
        let cases = [
            (
                r#"{"provider":"codex","source":"codex-app-server","fetched_at_unix":1,"windows":[]}"#,
                Provider::Codex,
                "codex-app-server",
            ),
            (
                r#"{"provider":"grok","source":"grok-cli-billing","fetched_at_unix":1,"windows":[]}"#,
                Provider::Grok,
                "grok-cli-billing",
            ),
            (
                r#"{"provider":"claude","source":"claude-statusline","fetched_at_unix":1,"windows":[]}"#,
                Provider::Claude,
                "claude-statusline",
            ),
            (
                r#"{"provider":"agy","source":"agy-statusline","fetched_at_unix":1,"windows":[]}"#,
                Provider::Agy,
                "agy-statusline",
            ),
        ];
        for (json, provider, source) in cases {
            let snapshot: ProviderSnapshot = serde_json::from_str(json).unwrap();
            assert_eq!(snapshot.provider, provider);
            assert_eq!(snapshot.source, source);
            assert_eq!(provider.source(), source);
        }
    }

    fn quota_window(kind: WindowKind, used: f64, reset: u64) -> UsageWindow {
        UsageWindow::new(kind, used, Some(ResetAt::from_unix_seconds(reset))).unwrap()
    }

    #[test]
    fn omitted_five_hour_window_is_kept_when_weekly_did_not_reset() {
        let previous = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                quota_window(WindowKind::FiveHour, 22.0, 2_000),
                quota_window(WindowKind::Weekly, 65.0, 10_000),
            ],
            1_000,
        );
        let mut current = ProviderSnapshot::new(
            Provider::Claude,
            vec![quota_window(WindowKind::Weekly, 66.0, 10_000)],
            1_100,
        );
        current.merge_omitted_windows(&previous);
        assert_eq!(
            current.window(WindowKind::FiveHour).unwrap().used_percent,
            22.0
        );
        assert_eq!(
            current.window(WindowKind::Weekly).unwrap().used_percent,
            66.0
        );
    }

    #[test]
    fn omitted_five_hour_window_is_dropped_when_weekly_resets() {
        let previous = ProviderSnapshot::new(
            Provider::Codex,
            vec![
                quota_window(WindowKind::FiveHour, 99.0, 8_000),
                quota_window(WindowKind::Weekly, 49.0, 10_000),
            ],
            1_000,
        );
        let mut current = ProviderSnapshot::new(
            Provider::Codex,
            vec![quota_window(WindowKind::Weekly, 0.0, 9_700)],
            1_200,
        );
        current.merge_omitted_windows(&previous);
        assert!(current.window(WindowKind::FiveHour).is_none());
        assert_eq!(
            current.window(WindowKind::Weekly).unwrap().used_percent,
            0.0
        );
    }

    #[test]
    fn empty_windows_still_clear_stale_quota() {
        let previous = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                quota_window(WindowKind::FiveHour, 22.0, 2_000),
                quota_window(WindowKind::Weekly, 65.0, 10_000),
            ],
            1_000,
        );
        let mut current = ProviderSnapshot::new(Provider::Claude, vec![], 1_100);
        current.merge_omitted_windows(&previous);
        assert!(current.windows.is_empty());
    }

    #[test]
    fn expired_five_hour_window_is_not_preserved() {
        let previous = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                quota_window(WindowKind::FiveHour, 22.0, 1_050),
                quota_window(WindowKind::Weekly, 65.0, 10_000),
            ],
            1_000,
        );
        let mut current = ProviderSnapshot::new(
            Provider::Claude,
            vec![quota_window(WindowKind::Weekly, 65.0, 10_000)],
            1_100,
        );
        current.merge_omitted_windows(&previous);
        assert!(current.window(WindowKind::FiveHour).is_none());
    }

    #[test]
    fn account_level_windows_are_shared_when_no_session_has_reported() {
        let snapshot = ProviderSnapshot::new(
            Provider::Grok,
            vec![quota_window(WindowKind::Weekly, 31.0, 10_000)],
            1,
        );
        assert_eq!(
            snapshot
                .windows_for_session(Some("session-1"))
                .first()
                .map(|window| window.used_percent),
            Some(31.0)
        );
        assert_eq!(
            snapshot
                .windows_for_session(None)
                .first()
                .unwrap()
                .used_percent,
            31.0
        );
    }

    #[test]
    fn statusline_windows_do_not_leak_across_sessions() {
        let mut snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![quota_window(WindowKind::Weekly, 90.0, 10_000)],
            1,
        );
        snapshot.session_windows.insert(
            "work".to_string(),
            vec![quota_window(WindowKind::Weekly, 10.0, 10_000)],
        );
        snapshot.session_windows.insert(
            "personal".to_string(),
            vec![quota_window(WindowKind::Weekly, 90.0, 10_000)],
        );

        assert_eq!(
            snapshot
                .windows_for_session(Some("work"))
                .first()
                .unwrap()
                .used_percent,
            10.0
        );
        assert_eq!(
            snapshot
                .windows_for_session(Some("personal"))
                .first()
                .unwrap()
                .used_percent,
            90.0
        );
        assert!(snapshot.windows_for_session(Some("unknown")).is_empty());
        assert_eq!(
            snapshot
                .windows_for_session(None)
                .first()
                .unwrap()
                .used_percent,
            90.0
        );
    }
}
