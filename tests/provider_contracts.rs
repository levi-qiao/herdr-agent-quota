use herdr_agent_quota::model::{BillingTarget, ResetAt, WindowKind};
use herdr_agent_quota::presentation::MetadataTokens;
use herdr_agent_quota::providers::{agy, claude, codex, grok, omp};
use serde_json::Value;

fn fixture(value: &str) -> Value {
    serde_json::from_str(value).expect("fixture is valid JSON")
}

#[test]
fn codex_fixture_exposes_the_five_hour_and_weekly_contracts() {
    let value = fixture(include_str!("fixtures/codex/rate-limits-weekly.json"));
    let snapshot = codex::parse_rate_limits(&value, 1).unwrap();
    assert_eq!(snapshot.windows.len(), 2);
    assert_eq!(
        snapshot
            .window(WindowKind::FiveHour)
            .unwrap()
            .remaining_percent,
        80.0
    );
    assert_eq!(
        snapshot
            .window(WindowKind::Weekly)
            .unwrap()
            .remaining_percent,
        39.0
    );
    assert_eq!(
        snapshot.window(WindowKind::Weekly).unwrap().resets_at,
        Some(ResetAt::from_unix_seconds(1_787_400_000))
    );
}

#[test]
fn grok_fixture_requires_explicit_weekly_period() {
    let weekly = fixture(include_str!("fixtures/grok/credits-weekly.json"));
    assert_eq!(
        grok::parse_billing_response(&weekly, 1)
            .unwrap()
            .window(WindowKind::Weekly)
            .unwrap()
            .remaining_percent,
        57.5
    );
    // A monthly pool is shown as 30d. The one thing it must never do is
    // occupy the weekly window, which would understate the credits' lifetime.
    let monthly = fixture(include_str!("fixtures/grok/credits-monthly.json"));
    let monthly = grok::parse_billing_response(&monthly, 1).unwrap();
    assert!(monthly.window(WindowKind::Weekly).is_none());
    assert_eq!(
        monthly
            .window(WindowKind::Monthly)
            .unwrap()
            .remaining_percent,
        57.5
    );
    let omitted = fixture(include_str!("fixtures/grok/credits-omitted-percent.json"));
    assert_eq!(
        grok::parse_billing_response(&omitted, 1)
            .unwrap()
            .window(WindowKind::Weekly)
            .unwrap()
            .remaining_percent,
        100.0
    );
}

#[test]
fn claude_fixture_contains_both_subscription_windows() {
    let value = fixture(include_str!("fixtures/claude/statusline-both.json"));
    let snapshot = claude::parse_statusline(&value, 1).unwrap();
    assert_eq!(snapshot.windows.len(), 2);
    assert_eq!(
        snapshot
            .window(WindowKind::FiveHour)
            .unwrap()
            .remaining_percent,
        42.0
    );
    assert_eq!(
        snapshot.window(WindowKind::Weekly).unwrap().resets_at,
        Some(ResetAt::from_unix_seconds(1_787_400_000))
    );
}

#[test]
fn agy_fixture_aggregates_gemini_and_third_party_windows() {
    let value = fixture(include_str!("fixtures/agy/statusline-both.json"));
    let snapshot = agy::parse_statusline(&value, 1).unwrap();
    assert_eq!(snapshot.windows.len(), 2);
    assert!(
        (snapshot
            .window(WindowKind::Weekly)
            .unwrap()
            .remaining_percent
            - 99.69)
            .abs()
            < 1e-9
    );
}

/// Recorded from a live `omp usage --json --redact` (omp 18.0.11) against a
/// real credential pool: a SuperGrok login, a ChatGPT login, a Cursor plan,
/// and Antigravity. It is the contract the omp collector reads.
fn omp_usage() -> Value {
    fixture(include_str!("fixtures/omp/usage-redacted.json"))
}

#[test]
fn omp_reports_the_supergrok_weekly_pool_and_not_its_per_product_twin() {
    let usage = omp::parse_usage(&omp_usage(), "xai-oauth", 1);
    let account = &usage.accounts[0];
    assert_eq!(account.windows.len(), 1);
    let weekly = &account.windows[0];
    assert_eq!(weekly.kind, WindowKind::Weekly);
    assert_eq!(weekly.remaining_percent, 41.0);
    assert_eq!(
        weekly.resets_at,
        Some(ResetAt::from_unix_seconds(1_788_701_555))
    );
    // Both the pool and `grokbuild` report the same duration; the unqualified
    // id is the one a sidebar row can be explained by.
    assert!(account.pin.is_some());
}

#[test]
fn omp_reports_both_codex_windows() {
    let usage = omp::parse_usage(&omp_usage(), "openai-codex", 1);
    let windows = &usage.accounts[0].windows;
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].kind, WindowKind::FiveHour);
    assert_eq!(windows[0].remaining_percent, 100.0);
    assert_eq!(windows[1].kind, WindowKind::Weekly);
    assert_eq!(windows[1].remaining_percent, 86.0);
}

/// omp owns its normalization contract. The plugin keeps the labels from its
/// capacity report instead of maintaining provider-specific period guesses.
#[test]
fn omp_windows_keep_omps_normalized_labels() {
    let antigravity = omp::parse_usage(&omp_usage(), "google-antigravity", 1);
    let daily = &antigravity.accounts[0].windows[0];
    assert_eq!(daily.kind, WindowKind::FiveHour);
    assert_eq!(daily.display_label(), "1d");
    assert_eq!(daily.duration_seconds, Some(86_400));

    let cursor = omp::parse_usage(&omp_usage(), "cursor", 1);
    let monthly = &cursor.accounts[0].windows[0];
    assert_eq!(monthly.kind, WindowKind::Monthly);
    assert_eq!(monthly.display_label(), "Monthly");
    assert_eq!(monthly.remaining_percent, 0.0);
}

#[test]
fn omp_daily_is_rendered_as_1d_instead_of_being_dropped_or_renamed() {
    let usage = omp::parse_usage(&omp_usage(), "google-antigravity", 1);
    let snapshot = omp::snapshot(
        &BillingTarget::omp("google-antigravity"),
        &usage.accounts[0],
    );
    let tokens = MetadataTokens::from_snapshot(&snapshot, 1_788_220_000);
    assert!(tokens.quota_5h.starts_with("1d 100%"), "{tokens:?}");
    assert_eq!(tokens.quota_week, "");
}

/// A provider nobody is signed in to reads as unknown, never as empty quota.
#[test]
fn omp_reports_nothing_for_an_absent_provider() {
    let usage = omp::parse_usage(&omp_usage(), "anthropic", 1);
    assert!(usage.accounts.is_empty());
    assert!(!usage.has_api_key);
}
