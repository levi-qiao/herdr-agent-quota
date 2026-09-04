use crate::cli::PercentStyle;
use crate::model::{
    format_percent, long_window, window_in, Provider, ProviderSnapshot, ResetAt, Severity,
    UsageWindow, WindowKind,
};

/// Exactly the values a pane can be given.
///
/// Every field here is published by [`crate::herdr::desired_tokens`]. Nothing
/// is rendered "in case the sidebar wants it later": an unpublished field
/// still costs a name in Herdr's 16-token report budget, because that budget
/// is spent clearing names the plugin no longer sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataTokens {
    pub quota_provider: String,
    pub quota_model: String,
    pub quota_provider_model: String,
    /// One compact token per window (`5h 42% 4h07m`), severity chooses the hue.
    pub quota_5h: String,
    pub quota_5h_severity: Option<Severity>,
    pub quota_week: String,
    pub quota_week_severity: Option<Severity>,
    pub quota_context: String,
    pub quota_cache: String,
    pub quota_cache_ttl: String,
    /// A lapsed prompt cache (`no cached`). Normal, unlike `quota_error`.
    pub quota_cache_state: String,
    /// The plugin could not speak for this pane at all.
    pub quota_error: Option<String>,
    /// Remaining quota in the tightest window this pane knows about, as a
    /// whole percent. `None` when no window reported one.
    ///
    /// The tightest window rather than the 5h one: whichever limit the user
    /// will hit first is the one worth sorting and warning on, and for a
    /// weekly plan that is often the 7d window.
    pub quota_headroom: Option<u8>,
}

impl MetadataTokens {
    pub fn from_snapshot(snapshot: &ProviderSnapshot, now_unix: u64) -> Self {
        Self::from_snapshot_for_session(snapshot, now_unix, None, PercentStyle::default())
    }

    pub fn from_snapshot_for_session(
        snapshot: &ProviderSnapshot,
        now_unix: u64,
        session_id: Option<&str>,
        style: PercentStyle,
    ) -> Self {
        Self::from_snapshot_parts(
            snapshot,
            now_unix,
            snapshot.model_for_session(session_id),
            snapshot.context_for_session(session_id),
            snapshot.windows_for_session(session_id),
            style,
        )
    }

    /// Render a pane's tokens without broadcasting provider-local diagnostics
    /// when Herdr cannot identify that pane's session. A provider-level model
    /// is still useful for the identity row, but context/cache values are
    /// session data and must stay blank until their session id is known.
    ///
    /// Quota windows follow the same session lookup as model/context, except
    /// they fall back to the account-level snapshot when no session has
    /// reported windows (Grok/Codex, or a legacy StatusLine cache).
    pub fn from_snapshot_for_pane(
        snapshot: &ProviderSnapshot,
        now_unix: u64,
        session_id: Option<&str>,
        style: PercentStyle,
    ) -> Self {
        let quota_model = match session_id {
            Some(session_id) => snapshot.model_for_session(Some(session_id)),
            None => snapshot.model.as_deref(),
        };
        let context =
            session_id.and_then(|session_id| snapshot.context_for_session(Some(session_id)));
        let windows = snapshot.windows_for_session(session_id);
        Self::from_snapshot_parts(snapshot, now_unix, quota_model, context, windows, style)
    }

    fn from_snapshot_parts(
        snapshot: &ProviderSnapshot,
        now_unix: u64,
        model: Option<&str>,
        context: Option<&crate::model::ContextUsage>,
        windows: &[UsageWindow],
        style: PercentStyle,
    ) -> Self {
        let quota_provider = snapshot.provider.display_name().to_string();
        let quota_model = model.unwrap_or_default().to_string();
        let omp_windows = snapshot.source.starts_with("omp.");
        let short_window = if omp_windows {
            window_in(windows, WindowKind::FiveHour)
        } else {
            None
        };
        let long = long_window(windows);
        let quota_5h = if omp_windows {
            short_window
                .map(|window| compact_window_parts(window, now_unix, style).rendered())
                .unwrap_or_default()
        } else {
            five_hour_slot(windows, snapshot.provider, now_unix, style)
        };
        Self {
            quota_provider_model: provider_model_label(&quota_provider, &quota_model),
            quota_provider,
            quota_model,
            quota_5h_severity: short_window
                .map(|window| Severity::for_window(window, now_unix))
                .or_else(|| window_severity(windows, WindowKind::FiveHour, now_unix))
                .or_else(|| {
                    (!omp_windows)
                        .then(|| missing_five_hour_severity(snapshot.provider, &quota_5h))
                        .flatten()
                }),
            quota_5h,
            quota_week: long
                .map(|window| compact_window_parts(window, now_unix, style).rendered())
                .unwrap_or_default(),
            quota_week_severity: long.map(|window| Severity::for_window(window, now_unix)),
            quota_context: sidebar_context(context),
            quota_cache: sidebar_cache(context),
            quota_cache_ttl: sidebar_cache_ttl(context, now_unix),
            quota_cache_state: sidebar_cache_state(context, now_unix),
            quota_error: None,
            quota_headroom: headroom(windows),
        }
    }

    /// The plugin has a snapshot it must not show — currently only a snapshot
    /// belonging to a login the user has since switched away from. Quota reads
    /// `N/A` rather than a stale number, and `quota_error` says why.
    pub fn unavailable(provider: Provider, reason: impl Into<String>) -> Self {
        let quota_provider = provider.display_name().to_string();
        Self {
            quota_provider_model: quota_provider.clone(),
            quota_provider,
            quota_model: String::new(),
            quota_5h: missing_five_hour_label(provider)
                .unwrap_or_default()
                .to_string(),
            quota_5h_severity: missing_five_hour_label(provider).map(|_| Severity::Unknown),
            quota_week: "7d N/A".to_string(),
            quota_week_severity: Some(Severity::Unknown),
            quota_context: String::new(),
            quota_cache: String::new(),
            quota_cache_ttl: String::new(),
            quota_cache_state: String::new(),
            quota_error: Some(reason.into().chars().take(80).collect()),
            quota_headroom: None,
        }
    }
}

/// The least remaining quota across the two windows the sidebar shows.
///
/// Deliberately the same pair as the rendered tokens — the 5h window and
/// whichever long window `long_window` picks — so a sort or an alert can
/// always be explained by a number the user can see. A monthly window that
/// the sidebar has no token for never drives either one.
///
/// Rounded down, so a window one point above a threshold is never rounded
/// onto the wrong side of it.
fn headroom(windows: &[UsageWindow]) -> Option<u8> {
    window_in(windows, WindowKind::FiveHour)
        .into_iter()
        .chain(long_window(windows))
        .map(|window| window.remaining_percent.clamp(0.0, 100.0).floor() as u8)
        .min()
}

fn provider_model_label(provider: &str, model: &str) -> String {
    if model.is_empty() {
        provider.to_string()
    } else {
        format!("{provider}/{model}")
    }
}

fn window_severity(windows: &[UsageWindow], kind: WindowKind, now_unix: u64) -> Option<Severity> {
    window_in(windows, kind).map(|window| Severity::for_window(window, now_unix))
}

/// The dashboard has room for every window, including a monthly one. The
/// sidebar deliberately stays at 5h/7d: there is no monthly metadata token,
/// and a 30d value must never be folded into a weekly one.
pub fn dashboard_summary(
    snapshot: &ProviderSnapshot,
    now_unix: u64,
    style: PercentStyle,
) -> String {
    windows_summary(
        &snapshot.windows,
        &[
            WindowKind::FiveHour,
            WindowKind::Weekly,
            WindowKind::Monthly,
        ],
        now_unix,
        true,
        style,
    )
}

fn windows_summary(
    windows: &[UsageWindow],
    kinds: &[WindowKind],
    now_unix: u64,
    include_suffix: bool,
    style: PercentStyle,
) -> String {
    kinds
        .iter()
        .filter_map(|kind| window_in(windows, *kind))
        .map(|window| format_window(window, now_unix, include_suffix, style))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The 5h slot: the window when the provider reported one, otherwise the
/// provider's placeholder (Claude/Agy keep a visible `5h N/A`; the rest omit
/// the row so the long window can fold onto context).
fn five_hour_slot(
    windows: &[UsageWindow],
    provider: Provider,
    now_unix: u64,
    style: PercentStyle,
) -> String {
    match window_in(windows, WindowKind::FiveHour) {
        Some(window) => compact_window_parts(window, now_unix, style).rendered(),
        None => missing_five_hour_label(provider)
            .unwrap_or_default()
            .to_string(),
    }
}

fn missing_five_hour_label(provider: Provider) -> Option<&'static str> {
    // Codex matches Grok: omit the 5h token so week can fold onto context.
    // Claude/Agy keep a visible placeholder on their separate limits row.
    match provider {
        Provider::Claude | Provider::Agy => Some("5h N/A"),
        Provider::Codex
        | Provider::Grok
        | Provider::OpenCodeGo
        | Provider::Omp
        | Provider::Devin => None,
    }
}

fn missing_five_hour_severity(provider: Provider, quota_5h: &str) -> Option<Severity> {
    (quota_5h == "5h N/A" && missing_five_hour_label(provider).is_some())
        .then_some(Severity::Unknown)
}

pub(crate) fn sidebar_context(context: Option<&crate::model::ContextUsage>) -> String {
    let Some(context) = context else {
        return String::new();
    };
    format!("context {}%", format_percent(context.used_percent))
}

pub(crate) fn sidebar_cache(context: Option<&crate::model::ContextUsage>) -> String {
    let Some(cache) = context.and_then(|context| context.cache.as_ref()) else {
        return String::new();
    };
    let hit_percent = cache
        .session_totals
        .as_ref()
        .map(|totals| totals.hit_percent)
        .unwrap_or(cache.hit_percent);
    format!("cache {:.1}%", hit_percent)
}

pub(crate) fn sidebar_cache_ttl(
    context: Option<&crate::model::ContextUsage>,
    now_unix: u64,
) -> String {
    let Some(cache) = context.and_then(|context| context.cache.as_ref()) else {
        return String::new();
    };
    cache
        .remaining_ttl_seconds(now_unix)
        .filter(|remaining| *remaining > 0)
        .map(|remaining| format!("ttl≈{}", format_ttl(remaining)))
        .unwrap_or_default()
}

/// A lapsed prompt cache is a normal state, not a failure.
///
/// It gets its own token so it is never confused with [`MetadataTokens::
/// unavailable`]'s `quota_error`, which reports that the plugin could not
/// speak for this pane at all. Both are amber, so sharing one token made
/// "your prefix went cold" indistinguishable from "quota is broken".
pub(crate) fn sidebar_cache_state(
    context: Option<&crate::model::ContextUsage>,
    now_unix: u64,
) -> String {
    context
        .and_then(|context| context.cache.as_ref())
        .and_then(|cache| cache.remaining_ttl_seconds(now_unix))
        .filter(|remaining| *remaining == 0)
        .map(|_| "no cached".to_string())
        .unwrap_or_default()
}

fn format_window(
    window: &UsageWindow,
    now_unix: u64,
    include_suffix: bool,
    style: PercentStyle,
) -> String {
    let percent = format!("{}%", format_percent(style.percent_of(window)));
    let suffix = if include_suffix {
        format!(" {}", style.suffix())
    } else {
        String::new()
    };
    let label = format!("{} {percent}{suffix}", window.display_label());
    let Some(reset) = window.resets_at else {
        return label;
    };
    let eta = format_reset_eta(reset, now_unix);
    format!("{label} reset {eta}")
}

struct WindowParts {
    label: String,
    percent: String,
    eta: String,
}

impl WindowParts {
    /// One space-separated token, because Herdr joins sibling tokens with
    /// ` · `. The period label leads, so the value is self-describing however
    /// the sidebar arranges it.
    fn rendered(&self) -> String {
        if self.eta.is_empty() {
            return format!("{} {}", self.label, self.percent);
        }
        format!("{} {} {}", self.label, self.percent, self.eta)
    }
}

fn compact_window_parts(window: &UsageWindow, now_unix: u64, style: PercentStyle) -> WindowParts {
    WindowParts {
        label: window.display_label().to_string(),
        percent: format!("{}%", format_percent(style.percent_of(window))),
        eta: window
            .resets_at
            .map(|reset| format_reset_eta(reset, now_unix))
            .unwrap_or_default(),
    }
}

fn format_reset_eta(reset_at: ResetAt, now_unix: u64) -> String {
    let seconds = reset_at.unix_seconds().saturating_sub(now_unix);
    if seconds == 0 {
        return "due".to_string();
    }
    format_duration(seconds)
}

fn format_duration(seconds: u64) -> String {
    let minutes = (seconds / 60).max(1);
    if minutes >= 24 * 60 {
        return format!("{}d{}h", minutes / (24 * 60), (minutes % (24 * 60)) / 60);
    }
    if minutes >= 60 {
        return format!("{}h{:02}m", minutes / 60, minutes % 60);
    }
    format!("{minutes}m")
}

fn format_ttl(seconds: u64) -> String {
    if seconds == 0 {
        return "0m".to_string();
    }
    let minutes = seconds / 60;
    if (60..24 * 60).contains(&minutes) && minutes.is_multiple_of(60) {
        return format!("{}h", minutes / 60);
    }
    format_duration(seconds)
}

#[cfg(test)]
mod tests {

    /// The sort key and the alert both read this, and both have to be
    /// explainable by a token the user can see, so a monthly window the
    /// sidebar has no room for must not decide either.
    #[test]
    fn headroom_is_the_tightest_window_the_sidebar_actually_shows() {
        let snapshot = ProviderSnapshot::new(
            Provider::OpenCodeGo,
            vec![
                window(WindowKind::FiveHour, 40.0, 3_600),
                window(WindowKind::Weekly, 75.0, 183_600),
                window(WindowKind::Monthly, 98.0, 1_500_000),
            ],
            0,
        );
        // 5h has 60 left, 7d has 25, 30d has 2 and is not shown.
        let tokens = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(tokens.quota_headroom, Some(25));
    }

    #[test]
    fn headroom_rounds_down_so_a_window_never_crosses_a_threshold_early() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::FiveHour, 89.5, 3_600)],
            0,
        );
        assert_eq!(
            MetadataTokens::from_snapshot(&snapshot, 0).quota_headroom,
            Some(10)
        );
    }

    #[test]
    fn a_provider_with_no_window_reports_no_headroom() {
        let snapshot = ProviderSnapshot::new(Provider::Grok, vec![], 0);
        assert_eq!(
            MetadataTokens::from_snapshot(&snapshot, 0).quota_headroom,
            None
        );
        assert_eq!(
            MetadataTokens::unavailable(Provider::Grok, "switched account").quota_headroom,
            None
        );
    }

    #[test]
    fn a_monthly_window_reaches_the_dashboard_but_never_the_sidebar() {
        let snapshot = ProviderSnapshot::new(
            Provider::OpenCodeGo,
            vec![
                window(WindowKind::FiveHour, 10.0, 3_600),
                window(WindowKind::Weekly, 20.0, 183_600),
                window(WindowKind::Monthly, 30.0, 1_500_000),
            ],
            0,
        );
        let dashboard = dashboard_summary(&snapshot, 0, PercentStyle::default());
        assert!(dashboard.contains("30d"), "{dashboard}");

        let sidebar = MetadataTokens::from_snapshot(&snapshot, 0);
        assert!(!sidebar.quota_week.contains("30d"), "{sidebar:?}");
        // No monthly token exists, so the value must not ride in on another.
        assert!(sidebar.quota_5h.contains("5h"));
        assert!(sidebar.quota_week.contains("7d"));
    }
    use super::*;
    use crate::model::{ProviderSnapshot, UsageWindow};

    fn window(kind: WindowKind, used: f64, reset: u64) -> UsageWindow {
        UsageWindow::new(kind, used, Some(ResetAt::from_unix_seconds(reset))).unwrap()
    }

    /// A monthly-only plan (Grok billed monthly, a Go plan with no weekly
    /// bucket) still gets a row, and it says `30d` — the label lives inside
    /// the value, so the long-window slot can carry either period truthfully.
    #[test]
    fn a_monthly_only_plan_fills_the_long_window_slot_with_its_own_label() {
        let snapshot = ProviderSnapshot::new(
            Provider::Grok,
            vec![window(WindowKind::Monthly, 30.0, 1_500_000)],
            0,
        );
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_week, "30d 70% 17d8h");
        assert_eq!(values.quota_week_severity, Some(Severity::Normal));
        assert_eq!(values.quota_5h, "");
    }

    /// Fallback only. A weekly window always wins the slot, because it is the
    /// limit that binds first; a 30d number must never displace it.
    #[test]
    fn a_weekly_window_always_wins_the_long_window_slot() {
        let snapshot = ProviderSnapshot::new(
            Provider::OpenCodeGo,
            vec![
                window(WindowKind::Weekly, 20.0, 183_600),
                window(WindowKind::Monthly, 90.0, 1_500_000),
            ],
            0,
        );
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_week, "7d 80% 2d3h");
        assert!(!values.quota_week.contains("30d"));
    }

    #[test]
    fn formats_reset_eta_for_minutes_hours_days_and_due_windows() {
        assert_eq!(
            format_reset_eta(ResetAt::from_unix_seconds(2_700), 0),
            "45m"
        );
        assert_eq!(
            format_reset_eta(ResetAt::from_unix_seconds(14_820), 0),
            "4h07m"
        );
        assert_eq!(
            format_reset_eta(ResetAt::from_unix_seconds(183_600), 0),
            "2d3h"
        );
        assert_eq!(format_reset_eta(ResetAt::from_unix_seconds(99), 100), "due");
        assert_eq!(format_ttl(0), "0m");
        assert_eq!(format_ttl(3_600), "1h");
    }

    #[test]
    fn summary_is_window_driven_and_keeps_five_hour_before_weekly() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                window(WindowKind::Weekly, 27.0, 183_600),
                window(WindowKind::FiveHour, 58.0, 14_820),
            ],
            0,
        );
        assert_eq!(
            dashboard_summary(&snapshot, 0, PercentStyle::default()),
            "5h 42% left reset 4h07m · 7d 73% left reset 2d3h"
        );
    }

    #[test]
    fn sidebar_windows_use_consistent_single_spacing() {
        let five_hour = format_window(
            &window(WindowKind::FiveHour, 57.0, 14_820),
            0,
            false,
            PercentStyle::default(),
        );
        let weekly = format_window(
            &window(WindowKind::Weekly, 75.0, 183_600),
            0,
            false,
            PercentStyle::default(),
        );
        assert_eq!(five_hour, "5h 43% reset 4h07m");
        assert_eq!(weekly, "7d 25% reset 2d3h");
    }

    /// The sidebar token keeps its width in both styles: no `left`/`used`
    /// word rides along, because the sidebar truncates and the style is a
    /// choice the user made for their own sidebar.
    #[test]
    fn the_used_style_flips_the_sidebar_number_but_not_its_width_or_colour() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                window(WindowKind::FiveHour, 58.0, 14_820),
                window(WindowKind::Weekly, 27.0, 183_600),
            ],
            0,
        );
        let remaining =
            MetadataTokens::from_snapshot_for_session(&snapshot, 0, None, PercentStyle::Remaining);
        assert_eq!(remaining.quota_5h, "5h 42% 4h07m");
        assert_eq!(remaining.quota_week, "7d 73% 2d3h");

        let used =
            MetadataTokens::from_snapshot_for_session(&snapshot, 0, None, PercentStyle::Used);
        assert_eq!(used.quota_5h, "5h 58% 4h07m");
        assert_eq!(used.quota_week, "7d 27% 2d3h");
        assert_eq!(used.quota_5h_severity, remaining.quota_5h_severity);
        assert_eq!(used.quota_week_severity, remaining.quota_week_severity);
    }

    #[test]
    fn metadata_error_stays_within_herdr_token_limit() {
        let values = MetadataTokens::unavailable(Provider::Grok, "x".repeat(120));
        assert_eq!(values.quota_error.as_deref().unwrap().len(), 80);
    }

    #[test]
    fn metadata_keeps_severity_per_quota_window() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                window(WindowKind::FiveHour, 70.0, 14_820),
                window(WindowKind::Weekly, 90.0, 183_600),
            ],
            0,
        );
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_5h_severity, Some(Severity::Warning));
        assert_eq!(values.quota_week_severity, Some(Severity::Danger));
    }

    #[test]
    fn metadata_uses_compact_quota_window_labels() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                window(WindowKind::FiveHour, 58.0, 14_820),
                window(WindowKind::Weekly, 27.0, 183_600),
            ],
            0,
        );
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_5h, "5h 42% 4h07m");
        assert_eq!(values.quota_week, "7d 73% 2d3h");
        assert!(!values.quota_5h.contains('·'));
        assert!(!values.quota_week.contains('·'));
    }

    #[test]
    fn metadata_formats_context_usage_when_available() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::Weekly, 10.0, 183_600)],
            0,
        )
        .with_context(Some(crate::model::ContextUsage::new(23.5).unwrap()));
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_context, "context 24%");
    }

    #[test]
    fn metadata_uses_the_model_for_the_pane_session() {
        let mut snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::Weekly, 10.0, 183_600)],
            0,
        )
        .with_model(Some("latest".to_string()));
        snapshot
            .session_models
            .insert("session-1".to_string(), "Sonnet".to_string());

        let session_one = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            Some("session-1"),
            PercentStyle::default(),
        );
        assert_eq!(session_one.quota_model, "Sonnet");
        assert_eq!(session_one.quota_provider_model, "Claude/Sonnet");

        let session_two = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            Some("session-2"),
            PercentStyle::default(),
        );
        assert_eq!(session_two.quota_model, "");
        assert_eq!(session_two.quota_provider_model, "Claude");
    }

    #[test]
    fn devin_pane_uses_configured_default_when_session_model_is_unknown() {
        let snapshot = ProviderSnapshot::new(
            Provider::Devin,
            vec![window(WindowKind::Weekly, 10.0, 183_600)],
            0,
        )
        .with_model(Some("SWE-1.7 Medium".to_string()));
        let pane = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            Some("session-a"),
            PercentStyle::default(),
        );
        assert_eq!(pane.quota_model, "SWE-1.7 Medium");
        assert_eq!(pane.quota_provider_model, "Devin/SWE-1.7 Medium");
    }

    #[test]
    fn metadata_uses_context_and_cache_for_the_pane_session() {
        let mut snapshot = ProviderSnapshot::new(Provider::Codex, vec![], 0);
        snapshot.session_contexts.insert(
            "session-1".to_string(),
            crate::model::ContextUsage::new(43.2)
                .unwrap()
                .with_cache(crate::model::CacheUsage::from_token_counts(200, 800, 100)),
        );
        let values = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            Some("session-1"),
            PercentStyle::default(),
        );
        assert_eq!(values.quota_context, "context 43%");
        assert_eq!(values.quota_cache, "cache 72.7%");
        assert_eq!(
            MetadataTokens::from_snapshot_for_session(
                &snapshot,
                0,
                Some("session-2"),
                PercentStyle::default()
            )
            .quota_context,
            ""
        );
    }

    #[test]
    fn pane_without_session_id_does_not_broadcast_local_diagnostics() {
        let snapshot = ProviderSnapshot::new(Provider::Grok, vec![], 0)
            .with_model(Some("grok-4.6".to_string()))
            .with_context(Some(
                crate::model::ContextUsage::new(43.2)
                    .unwrap()
                    .with_cache(crate::model::CacheUsage::from_token_counts(200, 800, 100)),
            ));
        let values =
            MetadataTokens::from_snapshot_for_pane(&snapshot, 0, None, PercentStyle::default());
        assert_eq!(values.quota_provider_model, "Grok/grok-4.6");
        assert_eq!(values.quota_context, "");
        assert_eq!(values.quota_cache, "");
        assert_eq!(values.quota_cache_ttl, "");
        assert_eq!(values.quota_error, None);
    }

    #[test]
    fn metadata_formats_session_cache_hit_rate_and_approximate_ttl() {
        let cache = crate::model::CacheUsage::from_token_counts(100, 800, 100)
            .unwrap()
            .with_ttl_estimate(60 * 60, 0)
            .with_session_totals(
                crate::model::CacheTotals::from_token_counts(100, 800, 100),
                "session-1",
                1,
            );
        let context = crate::model::ContextUsage::new(23.5)
            .unwrap()
            .with_cache(Some(cache));
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::Weekly, 10.0, 183_600)],
            0,
        )
        .with_context(Some(context));
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_context, "context 24%");
        assert_eq!(values.quota_cache, "cache 80.0%");
        assert_eq!(values.quota_cache_ttl, "ttl≈1h");
        assert_eq!(values.quota_error, None);
    }

    #[test]
    fn metadata_formats_cache_ttl_for_a_matching_pane_session() {
        let cache = crate::model::CacheUsage::from_token_counts(100, 800, 100)
            .unwrap()
            .with_ttl_estimate(60 * 60, 1_000)
            .with_session_totals(
                crate::model::CacheTotals::from_token_counts(100, 800, 100),
                "codex-session",
                0,
            );
        let mut snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 1);
        snapshot.session_contexts.insert(
            "codex-session".to_string(),
            crate::model::ContextUsage::new(23.5)
                .unwrap()
                .with_cache(Some(cache)),
        );

        let values = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            1_000,
            Some("codex-session"),
            PercentStyle::default(),
        );
        assert_eq!(values.quota_cache, "cache 80.0%");
        assert_eq!(values.quota_cache_ttl, "ttl≈1h");
    }

    #[test]
    fn expired_cache_ttl_is_reported_as_no_cached() {
        let cache = crate::model::CacheUsage::from_token_counts(100, 800, 100)
            .unwrap()
            .with_ttl_estimate(60, 0)
            .with_session_totals(
                crate::model::CacheTotals::from_token_counts(100, 800, 100),
                "session-1",
                1,
            );
        let snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 0).with_context(Some(
            crate::model::ContextUsage::new(23.5)
                .unwrap()
                .with_cache(Some(cache)),
        ));
        let values = MetadataTokens::from_snapshot(&snapshot, 61);
        assert_eq!(values.quota_cache_ttl, "");
        assert_eq!(values.quota_cache_state, "no cached");
        // A cold prefix is a normal state, not a plugin failure: it must not
        // land in the token that reports "quota could not be read at all".
        assert_eq!(values.quota_error, None);
    }

    #[test]
    fn weekly_only_sidebar_window_uses_the_compact_reset_eta() {
        let snapshot = ProviderSnapshot::new(
            Provider::Codex,
            vec![window(WindowKind::Weekly, 31.0, 518_400)],
            0,
        );
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_week, "7d 69% 6d0h");
        assert_eq!(values.quota_5h, "");
        assert_eq!(values.quota_5h_severity, None);
    }

    #[test]
    fn claude_keeps_a_five_hour_placeholder_on_the_limits_row() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::Weekly, 31.0, 518_400)],
            0,
        );
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_5h, "5h N/A");
        assert_eq!(values.quota_5h_severity, Some(Severity::Unknown));
    }

    #[test]
    fn grok_does_not_invent_a_five_hour_row() {
        let snapshot = ProviderSnapshot::new(
            Provider::Grok,
            vec![window(WindowKind::Weekly, 31.0, 518_400)],
            0,
        );
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_5h, "");
        assert_eq!(values.quota_5h_severity, None);
    }

    #[test]
    fn grok_and_codex_panes_keep_account_quota_when_the_session_is_known() {
        let grok = ProviderSnapshot::new(
            Provider::Grok,
            vec![window(WindowKind::Weekly, 31.0, 518_400)],
            0,
        );
        let grok_pane = MetadataTokens::from_snapshot_for_pane(
            &grok,
            0,
            Some("session-1"),
            PercentStyle::default(),
        );
        assert_eq!(grok_pane.quota_week, "7d 69% 6d0h");
        assert_eq!(grok_pane.quota_5h, "");

        let codex = ProviderSnapshot::new(
            Provider::Codex,
            vec![
                window(WindowKind::FiveHour, 40.0, 14_820),
                window(WindowKind::Weekly, 31.0, 518_400),
            ],
            0,
        );
        let codex_pane = MetadataTokens::from_snapshot_for_pane(
            &codex,
            0,
            Some("session-1"),
            PercentStyle::default(),
        );
        assert_eq!(codex_pane.quota_5h, "5h 60% 4h07m");
        assert_eq!(codex_pane.quota_week, "7d 69% 6d0h");
    }

    #[test]
    fn claude_pane_quota_follows_the_pane_session() {
        let mut snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::Weekly, 90.0, 518_400)],
            0,
        );
        snapshot.session_windows.insert(
            "work".to_string(),
            vec![
                window(WindowKind::FiveHour, 18.0, 14_820),
                window(WindowKind::Weekly, 10.0, 518_400),
            ],
        );
        snapshot.session_windows.insert(
            "personal".to_string(),
            vec![
                window(WindowKind::FiveHour, 82.0, 14_820),
                window(WindowKind::Weekly, 90.0, 518_400),
            ],
        );

        let work = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            Some("work"),
            PercentStyle::default(),
        );
        assert_eq!(work.quota_5h, "5h 82% 4h07m");
        assert_eq!(work.quota_week, "7d 90% 6d0h");

        let personal = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            Some("personal"),
            PercentStyle::default(),
        );
        assert_eq!(personal.quota_5h, "5h 18% 4h07m");
        assert_eq!(personal.quota_week, "7d 10% 6d0h");

        let unknown = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            Some("other"),
            PercentStyle::default(),
        );
        assert_eq!(unknown.quota_5h, "5h N/A");
        assert_eq!(unknown.quota_week, "");
    }

    #[test]
    fn session_cache_percentage_keeps_one_decimal_instead_of_rounding_to_100() {
        let cache = crate::model::CacheUsage::from_token_counts(2_000, 433_336, 1_655)
            .unwrap()
            .with_session_totals(
                crate::model::CacheTotals::from_token_counts(2_000, 433_336, 1_655),
                "session-1",
                1,
            );
        let snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 0).with_context(Some(
            crate::model::ContextUsage::new(43.0)
                .unwrap()
                .with_cache(Some(cache)),
        ));
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_cache, "cache 99.2%");
    }
}
