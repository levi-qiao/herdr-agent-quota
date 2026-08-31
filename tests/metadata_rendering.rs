use herdr_agent_quota::cli::PercentStyle;
use herdr_agent_quota::dashboard::render_provider;
use herdr_agent_quota::model::{Provider, ProviderSnapshot, ResetAt, UsageWindow, WindowKind};

#[test]
fn agent_row_renders_compact_reset_eta_without_absolute_timestamp() {
    let snapshot = ProviderSnapshot::new(
        Provider::Grok,
        vec![UsageWindow::new(
            WindowKind::Weekly,
            79.0,
            Some(ResetAt::from_unix_seconds(183_600)),
        )
        .unwrap()],
        1,
    );
    assert_eq!(
        render_provider(Provider::Grok, Some(&snapshot), 0, PercentStyle::Remaining),
        "Grok WARN\r\n  7d 21% left reset 2d3h"
    );
}

/// The used style flips the number and the dashboard's trailing word; the
/// severity band still comes from what is left, so `WARN` does not move.
#[test]
fn the_used_style_reports_consumed_quota_without_changing_the_severity_band() {
    let snapshot = ProviderSnapshot::new(
        Provider::Grok,
        vec![UsageWindow::new(
            WindowKind::Weekly,
            79.0,
            Some(ResetAt::from_unix_seconds(183_600)),
        )
        .unwrap()],
        1,
    );
    assert_eq!(
        render_provider(Provider::Grok, Some(&snapshot), 0, PercentStyle::Used),
        "Grok WARN\r\n  7d 79% used reset 2d3h"
    );
}
