//! OpenCode Go subscription usage.
//!
//! One official REST call, authenticated with the key OpenCode already stores
//! for its own `opencode-go` backend. No browser cookies, no Keychain, no
//! local spend estimate, and no fallback host.
//!
//! **The maintainer of this repository has no OpenCode Go subscription**, so the
//! success shape below could not be observed first hand. It is taken from
//! CodexBar's implementation and its own test fixtures, recorded with citations
//! in `docs/research/opencode-go-usage.md`. Everything here fails closed: a
//! field that is missing, malformed, or of an unexpected type yields no window
//! rather than a guessed number, and an auth failure never becomes 0% used.
//! Corrections from anyone with a live subscription are welcome.

use crate::cache::CacheStore;
use crate::model::{Provider, ProviderSnapshot, ResetAt, UsageWindow, WindowKind};
use crate::providers::ProviderError;
use anyhow::{Context, Result};
use serde_json::Value;
use std::time::Duration;

/// Official host and path. Credentials are only ever sent here; a redirect
/// away from this host drops the request rather than following it.
const USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";
const SOURCE: &str = "opencode-go-usage";

/// `usage.rolling` is the five-hour bucket; the other two are optional.
///
/// Key order matches CodexBar's, which accepts several spellings because the
/// deployed field name has moved before.
const PERCENT_KEYS: [&str; 4] = ["percent", "usagePercent", "usedPercent", "percentUsed"];
const RESET_IN_KEYS: [&str; 4] = [
    "resetInSec",
    "resetInSeconds",
    "resetSeconds",
    "resetsInSec",
];
const RESET_AT_KEYS: [&str; 4] = ["resetsAt", "resetAt", "resets_at", "reset_at"];

pub fn fetch(key: &str) -> Result<ProviderSnapshot> {
    if key.trim().is_empty() {
        return Err(ProviderError::MissingCredentials.into());
    }
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .timeout_write(Duration::from_secs(10))
        // A credential-bearing request must not be replayed to another host.
        .redirects(0)
        .build();
    let response = agent
        .get(USAGE_URL)
        .set("Authorization", &format!("Bearer {}", key.trim()))
        .set("Accept", "application/json")
        .call()
        .map_err(|error| ProviderError::Request(http_error_status(&error)))?;
    let value: Value = response
        .into_json()
        .context("decode OpenCode Go usage response")?;
    parse_usage(&value, CacheStore::now_unix()).map_err(anyhow::Error::from)
}

/// Build a snapshot from the deployed `usage.{rolling,weekly,monthly}` shape.
///
/// `rolling` is required: without it there is no quota to show, and reporting
/// an empty snapshot would clear a pane that still has a good cached value.
pub fn parse_usage(value: &Value, now_unix: u64) -> Result<ProviderSnapshot, ProviderError> {
    let usage = value
        .get("usage")
        .and_then(Value::as_object)
        .ok_or_else(|| ProviderError::UnsupportedResponse("missing usage object".to_string()))?;
    let rolling = usage
        .get("rolling")
        .and_then(|window| parse_window(window, WindowKind::FiveHour, now_unix))
        .ok_or_else(|| {
            ProviderError::UnsupportedResponse("missing usage.rolling percent".to_string())
        })?;

    let mut windows = vec![rolling];
    for (key, kind) in [
        ("weekly", WindowKind::Weekly),
        ("monthly", WindowKind::Monthly),
    ] {
        // An absent optional window means "this plan has no such bucket", not
        // "0% used". Publishing a zero here would read as a full allowance.
        if let Some(window) = usage.get(key).and_then(|w| parse_window(w, kind, now_unix)) {
            windows.push(window);
        }
    }

    let mut snapshot = ProviderSnapshot::new(Provider::OpenCodeGo, windows, now_unix);
    snapshot.source = SOURCE.to_string();
    Ok(snapshot)
}

/// Percent from the API is a **used** percentage already scaled 0..=100.
///
/// This is the one number worth being paranoid about: reading `0.5` as a
/// fraction would report 50% used instead of 0.5%, a 100x error in the
/// direction that hides an exhausted quota. CodexBar's API path passes
/// `directPercentEncoding: .percent` for exactly this reason, with the comment
/// "API fields ... already use 0...100". No fraction rescaling happens here.
fn parse_window(value: &Value, kind: WindowKind, now_unix: u64) -> Option<UsageWindow> {
    let percent = PERCENT_KEYS
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_f64))?;
    if !percent.is_finite() {
        return None;
    }
    let used_percent = percent.clamp(0.0, 100.0);
    UsageWindow::new(kind, used_percent, parse_reset(value, now_unix)).ok()
}

/// Reset is `resetInSec` (seconds from now) in the deployed response; the
/// absolute `resetsAt` spelling is accepted too because both appear upstream.
fn parse_reset(value: &Value, now_unix: u64) -> Option<ResetAt> {
    if let Some(seconds) = RESET_IN_KEYS
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_u64))
    {
        return Some(ResetAt::from_unix_seconds(now_unix.saturating_add(seconds)));
    }
    RESET_AT_KEYS.into_iter().find_map(|key| {
        let value = value.get(key)?;
        match value {
            Value::String(text) => ResetAt::parse(text),
            Value::Number(number) => number.as_u64().map(ResetAt::from_unix_seconds),
            _ => None,
        }
    })
}

/// Never let an auth or transport failure reach the cache as a quota value.
/// The caller keeps the last good snapshot for this same target instead.
fn http_error_status(error: &ureq::Error) -> String {
    match error {
        ureq::Error::Status(401 | 403, _) => "HTTP 401/403 (invalid credentials)".to_string(),
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(error) => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: u64 = 1_787_000_000;

    fn window(snapshot: &ProviderSnapshot, kind: WindowKind) -> Option<&UsageWindow> {
        snapshot.windows.iter().find(|window| window.kind == kind)
    }

    /// The exact payload CodexBar feeds its own parser, with the values it
    /// asserts: 3 / 1 / 0 percent used.
    fn deployed_shape() -> Value {
        json!({"usage": {
            "rolling": {"percent": 3, "resetInSec": 18100},
            "weekly": {"percent": 1, "resetInSec": 266500},
            "monthly": {"percent": 0, "resetInSec": 1539100}
        }})
    }

    #[test]
    fn parses_the_deployed_rolling_weekly_monthly_shape() {
        let snapshot = parse_usage(&deployed_shape(), NOW).unwrap();
        assert_eq!(snapshot.provider, Provider::OpenCodeGo);
        assert_eq!(snapshot.source, SOURCE);
        assert_eq!(
            window(&snapshot, WindowKind::FiveHour)
                .unwrap()
                .used_percent,
            3.0
        );
        assert_eq!(
            window(&snapshot, WindowKind::Weekly).unwrap().used_percent,
            1.0
        );
        assert_eq!(
            window(&snapshot, WindowKind::Monthly).unwrap().used_percent,
            0.0
        );
        assert_eq!(
            window(&snapshot, WindowKind::FiveHour)
                .unwrap()
                .resets_at
                .unwrap(),
            ResetAt::from_unix_seconds(NOW + 18_100)
        );
    }

    #[test]
    fn a_fractional_percent_is_not_rescaled_to_fifty() {
        // The 100x mistake this parser exists to avoid.
        let snapshot = parse_usage(
            &json!({"usage": {"rolling": {"percent": 0.5, "resetInSec": 60}}}),
            NOW,
        )
        .unwrap();
        let rolling = window(&snapshot, WindowKind::FiveHour).unwrap();
        assert_eq!(rolling.used_percent, 0.5);
        assert_eq!(rolling.remaining_percent, 99.5);
    }

    #[test]
    fn an_absent_optional_window_is_omitted_rather_than_reported_as_zero() {
        let snapshot = parse_usage(
            &json!({"usage": {"rolling": {"percent": 42, "resetInSec": 60}}}),
            NOW,
        )
        .unwrap();
        assert!(window(&snapshot, WindowKind::Weekly).is_none());
        assert!(window(&snapshot, WindowKind::Monthly).is_none());
        assert_eq!(snapshot.windows.len(), 1);
    }

    #[test]
    fn an_unknown_shape_fails_closed() {
        for payload in [
            json!({}),
            json!({"usage": {}}),
            json!({"usage": {"rolling": {}}}),
            json!({"usage": {"rolling": {"percent": "lots"}}}),
            json!({"type": "error", "error": {"type": "AuthError"}}),
        ] {
            assert!(parse_usage(&payload, NOW).is_err(), "accepted {payload}");
        }
    }

    #[test]
    fn an_absolute_reset_timestamp_is_also_accepted() {
        let snapshot = parse_usage(
            &json!({"usage": {"rolling": {"percent": 10, "resetsAt": "2026-08-29T12:00:00Z"}}}),
            NOW,
        )
        .unwrap();
        assert!(window(&snapshot, WindowKind::FiveHour)
            .unwrap()
            .resets_at
            .is_some());
    }

    #[test]
    fn an_out_of_range_percent_is_clamped_instead_of_trusted() {
        let snapshot = parse_usage(&json!({"usage": {"rolling": {"percent": 150}}}), NOW).unwrap();
        assert_eq!(
            window(&snapshot, WindowKind::FiveHour)
                .unwrap()
                .used_percent,
            100.0
        );
    }

    #[test]
    fn credentials_never_appear_in_an_error() {
        let error = fetch("   ").unwrap_err().to_string();
        assert!(!error.contains("   "));
        assert!(error.contains("credentials"));
    }

    #[test]
    fn the_endpoint_is_the_official_host_only() {
        assert!(USAGE_URL.starts_with("https://opencode.ai/"));
    }
}
