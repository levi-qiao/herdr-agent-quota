use crate::model::{
    format_percent, Provider, ProviderSnapshot, ResetAt, Severity, UsageWindow, WindowKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataTokens {
    pub quota_state: String,
    pub quota_icon: String,
    pub quota_provider: String,
    pub quota_model: String,
    pub quota_provider_model: String,
    pub quota_status: String,
    pub quota_5h: String,
    pub quota_5h_severity: Option<Severity>,
    pub quota_week: String,
    pub quota_week_severity: Option<Severity>,
    pub quota_summary: String,
    pub quota_context: String,
    pub quota_cache: String,
    pub quota_cache_ttl: String,
    pub quota_error: Option<String>,
}

impl MetadataTokens {
    pub fn from_snapshot(snapshot: &ProviderSnapshot, now_unix: u64) -> Self {
        Self::from_snapshot_for_session(snapshot, now_unix, None)
    }

    pub fn from_snapshot_for_session(
        snapshot: &ProviderSnapshot,
        now_unix: u64,
        session_id: Option<&str>,
    ) -> Self {
        Self::from_snapshot_parts(
            snapshot,
            now_unix,
            snapshot.model_for_session(session_id),
            snapshot.context_for_session(session_id),
            snapshot.windows_for_session(session_id),
        )
    }

    /// Render a pane's tokens without broadcasting provider-local diagnostics
    /// when Herdr cannot identify that pane's session. A provider-level model
    /// is still useful for the identity row, but context/cache values are
    /// session data and must stay blank until their session id is known.
    ///
    /// Quota windows are also resolved per session rather than from the
    /// shared top-level snapshot: StatusLine providers (Claude, Agy) can have
    /// more than one signed-in account reporting concurrently, and the
    /// top-level windows only ever hold whichever account reported last.
    pub fn from_snapshot_for_pane(
        snapshot: &ProviderSnapshot,
        now_unix: u64,
        session_id: Option<&str>,
    ) -> Self {
        let quota_model = match session_id {
            Some(session_id) => snapshot.model_for_session(Some(session_id)),
            None => snapshot.model.as_deref(),
        };
        let context =
            session_id.and_then(|session_id| snapshot.context_for_session(Some(session_id)));
        let windows = snapshot.windows_for_session(session_id);
        Self::from_snapshot_parts(snapshot, now_unix, quota_model, context, windows)
    }

    fn from_snapshot_parts(
        snapshot: &ProviderSnapshot,
        now_unix: u64,
        model: Option<&str>,
        context: Option<&crate::model::ContextUsage>,
        windows: &[UsageWindow],
    ) -> Self {
        let quota_provider = snapshot.provider.display_name().to_string();
        let quota_model = model.unwrap_or_default().to_string();
        let quota_5h = sidebar_window(windows, snapshot.provider, WindowKind::FiveHour, now_unix);
        let severity = ProviderSnapshot::severity_for_windows(snapshot.provider, windows, now_unix);
        Self {
            quota_state: severity.symbol().to_string(),
            quota_icon: snapshot.provider.icon().to_string(),
            quota_provider_model: provider_model_label(&quota_provider, &quota_model),
            quota_provider,
            quota_model,
            quota_status: severity.label().to_string(),
            quota_5h_severity: window_severity(windows, WindowKind::FiveHour, now_unix)
                .or_else(|| missing_five_hour_severity(snapshot.provider, &quota_5h)),
            quota_5h,
            quota_week: sidebar_window(windows, snapshot.provider, WindowKind::Weekly, now_unix),
            quota_week_severity: window_severity(windows, WindowKind::Weekly, now_unix),
            quota_summary: summary_from_windows(windows, now_unix, false),
            quota_context: sidebar_context(context),
            quota_cache: sidebar_cache(context),
            quota_cache_ttl: sidebar_cache_ttl(context, now_unix),
            quota_error: sidebar_cache_error(context, now_unix),
        }
    }

    pub fn unavailable(provider: Provider, reason: impl Into<String>) -> Self {
        let quota_provider = provider.display_name().to_string();
        Self {
            quota_state: Severity::Unknown.symbol().to_string(),
            quota_icon: provider.icon().to_string(),
            quota_provider_model: quota_provider.clone(),
            quota_provider,
            quota_model: String::new(),
            quota_status: Severity::Unknown.label().to_string(),
            quota_5h: missing_five_hour_label(provider)
                .unwrap_or_default()
                .to_string(),
            quota_5h_severity: missing_five_hour_label(provider).map(|_| Severity::Unknown),
            quota_week: "7d N/A".to_string(),
            quota_week_severity: Some(Severity::Unknown),
            quota_summary: "unavailable".to_string(),
            quota_context: String::new(),
            quota_cache: String::new(),
            quota_cache_ttl: String::new(),
            quota_error: Some(reason.into().chars().take(80).collect()),
        }
    }
}

fn provider_model_label(provider: &str, model: &str) -> String {
    if model.is_empty() {
        provider.to_string()
    } else {
        format!("{provider}/{model}")
    }
}

fn window_severity(windows: &[UsageWindow], kind: WindowKind, now_unix: u64) -> Option<Severity> {
    find_window(windows, kind).map(|window| Severity::for_window(window, now_unix))
}

fn find_window(windows: &[UsageWindow], kind: WindowKind) -> Option<&UsageWindow> {
    windows.iter().find(|window| window.kind == kind)
}

pub fn sidebar_summary(snapshot: &ProviderSnapshot, now_unix: u64) -> String {
    summary_from_windows(&snapshot.windows, now_unix, false)
}

pub fn dashboard_summary(snapshot: &ProviderSnapshot, now_unix: u64) -> String {
    summary_from_windows(&snapshot.windows, now_unix, true)
}

fn summary_from_windows(windows: &[UsageWindow], now_unix: u64, include_left: bool) -> String {
    [WindowKind::FiveHour, WindowKind::Weekly]
        .into_iter()
        .filter_map(|kind| find_window(windows, kind))
        .map(|window| format_window(window, now_unix, include_left))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn sidebar_window(
    windows: &[UsageWindow],
    provider: Provider,
    kind: WindowKind,
    now_unix: u64,
) -> String {
    if let Some(window) = find_window(windows, kind) {
        return format_compact_window(window, now_unix);
    }
    if kind == WindowKind::FiveHour {
        return missing_five_hour_label(provider)
            .unwrap_or_default()
            .to_string();
    }
    String::new()
}

fn missing_five_hour_label(provider: Provider) -> Option<&'static str> {
    // Codex matches Grok: omit the 5h token so week can fold onto context.
    // Claude/Agy keep a visible placeholder on their separate limits row.
    match provider {
        Provider::Claude | Provider::Agy => Some("5h N/A"),
        Provider::Codex | Provider::Grok => None,
    }
}

fn missing_five_hour_severity(provider: Provider, quota_5h: &str) -> Option<Severity> {
    (quota_5h == "5h N/A" && missing_five_hour_label(provider).is_some())
        .then_some(Severity::Unknown)
}

fn sidebar_context(context: Option<&crate::model::ContextUsage>) -> String {
    let Some(context) = context else {
        return String::new();
    };
    format!("context {}%", format_percent(context.used_percent))
}

fn sidebar_cache(context: Option<&crate::model::ContextUsage>) -> String {
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

fn sidebar_cache_ttl(context: Option<&crate::model::ContextUsage>, now_unix: u64) -> String {
    let Some(cache) = context.and_then(|context| context.cache.as_ref()) else {
        return String::new();
    };
    cache
        .remaining_ttl_seconds(now_unix)
        .filter(|remaining| *remaining > 0)
        .map(|remaining| format!("ttl≈{}", format_ttl(remaining)))
        .unwrap_or_default()
}

fn sidebar_cache_error(
    context: Option<&crate::model::ContextUsage>,
    now_unix: u64,
) -> Option<String> {
    context
        .and_then(|context| context.cache.as_ref())
        .and_then(|cache| cache.remaining_ttl_seconds(now_unix))
        .filter(|remaining| *remaining == 0)
        .map(|_| "no cached".to_string())
}

fn format_window(window: &UsageWindow, now_unix: u64, include_left: bool) -> String {
    let percent = format!("{}%", format_percent(window.remaining_percent));
    let left = if include_left { " left" } else { "" };
    let label = format!("{} {percent}{left}", window.kind.label());
    let Some(reset) = window.resets_at else {
        return label;
    };
    let eta = format_reset_eta(reset, now_unix);
    format!("{label} reset {eta}")
}

fn format_compact_window(window: &UsageWindow, now_unix: u64) -> String {
    let label = match window.kind {
        WindowKind::FiveHour => "5h",
        WindowKind::Weekly => "7d",
    };
    let percent = format!("{}%", format_percent(window.remaining_percent));
    let Some(reset) = window.resets_at else {
        return format!("{label} {percent}");
    };
    format!("{label} {percent} {}", format_reset_eta(reset, now_unix))
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
    use super::*;
    use crate::model::{ProviderSnapshot, UsageWindow};

    fn window(kind: WindowKind, used: f64, reset: u64) -> UsageWindow {
        UsageWindow::new(kind, used, Some(ResetAt::from_unix_seconds(reset))).unwrap()
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
            sidebar_summary(&snapshot, 0),
            "5h 42% reset 4h07m · 7d 73% reset 2d3h"
        );
        assert_eq!(
            dashboard_summary(&snapshot, 0),
            "5h 42% left reset 4h07m · 7d 73% left reset 2d3h"
        );
    }

    #[test]
    fn sidebar_windows_use_consistent_single_spacing() {
        let five_hour = format_window(&window(WindowKind::FiveHour, 57.0, 14_820), 0, false);
        let weekly = format_window(&window(WindowKind::Weekly, 75.0, 183_600), 0, false);
        assert_eq!(five_hour, "5h 43% reset 4h07m");
        assert_eq!(weekly, "7d 25% reset 2d3h");
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

        let session_one =
            MetadataTokens::from_snapshot_for_session(&snapshot, 0, Some("session-1"));
        assert_eq!(session_one.quota_model, "Sonnet");
        assert_eq!(session_one.quota_provider_model, "Claude/Sonnet");

        let session_two =
            MetadataTokens::from_snapshot_for_session(&snapshot, 0, Some("session-2"));
        assert_eq!(session_two.quota_model, "");
        assert_eq!(session_two.quota_provider_model, "Claude");
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
        let values = MetadataTokens::from_snapshot_for_session(&snapshot, 0, Some("session-1"));
        assert_eq!(values.quota_context, "context 43%");
        assert_eq!(values.quota_cache, "cache 72.7%");
        assert_eq!(
            MetadataTokens::from_snapshot_for_session(&snapshot, 0, Some("session-2"))
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
        let values = MetadataTokens::from_snapshot_for_pane(&snapshot, 0, None);
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
    fn metadata_formats_codex_cache_ttl_for_the_pane_session() {
        let cache = crate::model::CacheUsage::from_token_counts(100, 800, 100)
            .unwrap()
            .with_ttl_estimate(60 * 60, 1_000)
            .with_session_totals(
                crate::model::CacheTotals::from_token_counts(100, 800, 100),
                "codex-session",
                0,
            );
        let mut snapshot = ProviderSnapshot::new(Provider::Codex, vec![], 1);
        snapshot.session_contexts.insert(
            "codex-session".to_string(),
            crate::model::ContextUsage::new(23.5)
                .unwrap()
                .with_cache(Some(cache)),
        );

        let values =
            MetadataTokens::from_snapshot_for_session(&snapshot, 1_000, Some("codex-session"));
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
        assert_eq!(values.quota_error.as_deref(), Some("no cached"));
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
