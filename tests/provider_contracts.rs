use herdr_agent_quota::model::{ResetAt, WindowKind};
use herdr_agent_quota::providers::{agy, claude, codex, grok};
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
