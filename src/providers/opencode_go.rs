//! OpenCode Go subscription usage.
//!
//! Two sources, in order of authority for the panes this runs beside:
//!
//! 1. **Console meters.** OpenCode 2 serves Go inference as the account that
//!    signed in to the console, so the dollars the panes spend live behind
//!    `GET {console}/api/go/status` (`access.meters`), the same numbers the
//!    console page renders. The console login is read from OpenCode's own
//!    credential store.
//! 2. **Key counters.** `GET https://opencode.ai/zen/go/v1/usage` reports what
//!    *this API key* spent. It is per-key, and a key an install stopped using
//!    freezes at its last reading, so it is only a fallback for stores with no
//!    console login. One official REST call, authenticated with the key
//!    OpenCode already stores for its own `opencode-go` backend.
//!
//! No browser cookies, no Keychain, no local spend estimate, and no fallback
//! host. Everything here fails closed: a field that is missing, malformed, or
//! of an unexpected type yields no window rather than a guessed number, and an
//! auth failure never becomes 0% used.

use crate::cache::CacheStore;
use crate::model::{Provider, ProviderSnapshot, ResetAt, UsageWindow, WindowKind};
use crate::opencode::ConsoleCredential;
use crate::providers::ProviderError;
use anyhow::{Context, Result};
use serde_json::Value;
use std::time::Duration;

/// Official host and path. Credentials are only ever sent here; a redirect
/// away from this host drops the request rather than following it.
const USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";

/// Console Go status route, relative to the login's own console server.
const CONSOLE_STATUS_PATH: &str = "/api/go/status";

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
    parse_usage(&value, CacheStore::now_unix())
        .map(|snapshot| snapshot.with_account_id(Some(super::credential_id(key))))
        .map_err(anyhow::Error::from)
}

/// One console call for the account's Go meters.
///
/// The token belongs to the login OpenCode itself uses; it is sent only to the
/// host that login was created on. A redirect is never followed.
pub fn fetch_console(credential: &ConsoleCredential) -> Result<ProviderSnapshot> {
    let Some(server) = console_server(&credential.server) else {
        return Err(
            ProviderError::Unavailable("console host is not opencode.ai".to_string()).into(),
        );
    };
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .timeout_write(Duration::from_secs(10))
        // A credential-bearing request must not be replayed to another host.
        .redirects(0)
        .build();
    let response = agent
        .get(&format!("{server}{CONSOLE_STATUS_PATH}"))
        .set(
            "Authorization",
            &format!("Bearer {}", credential.access.trim()),
        )
        .set("x-org-id", credential.org_id.trim())
        .set("Accept", "application/json")
        .call()
        .map_err(|error| ProviderError::Request(http_error_status(&error)))?;
    let value: Value = response
        .into_json()
        .context("decode OpenCode console response")?;
    parse_console_status(&value, CacheStore::now_unix())
        .map(|snapshot| snapshot.with_account_id(Some(credential.account_id.clone())))
        .map_err(anyhow::Error::from)
}

/// Pin the credential to the console host it was created on.
fn console_server(server: &str) -> Option<String> {
    let server = server.trim().trim_end_matches('/');
    let host = "https://opencode.ai";
    (server == host || server.starts_with(&format!("{host}/"))).then(|| server.to_string())
}

/// Build a snapshot from the console's `access.meters` dollar meters.
///
/// Each meter carries `usedMicroCents`/`limitMicroCents`, where 100,000,000
/// microcents is one dollar. `month` has no reset of its own: it counts down
/// to the paid period's `endsAt`, which is what the console card shows.
pub fn parse_console_status(
    value: &Value,
    now_unix: u64,
) -> Result<ProviderSnapshot, ProviderError> {
    let access = value
        .get("access")
        .and_then(Value::as_object)
        .ok_or_else(|| ProviderError::UnsupportedResponse("missing access".to_string()))?;
    let meters = access
        .get("meters")
        .and_then(Value::as_object)
        .ok_or_else(|| ProviderError::UnsupportedResponse("missing access.meters".to_string()))?;
    let period_reset = access
        .get("endsAt")
        .and_then(Value::as_str)
        .and_then(ResetAt::parse);
    let mut windows = Vec::new();
    for (key, kind) in [
        ("fiveHour", WindowKind::FiveHour),
        ("week", WindowKind::Weekly),
        ("month", WindowKind::Monthly),
    ] {
        if let Some(window) = meters
            .get(key)
            .and_then(|meter| parse_console_meter(meter, kind, period_reset))
        {
            windows.push(window);
        }
    }
    if windows.is_empty() {
        return Err(ProviderError::UnsupportedResponse(
            "console meters are empty".to_string(),
        ));
    }
    Ok(ProviderSnapshot::new(
        Provider::OpenCodeGo,
        windows,
        now_unix,
    ))
}

fn parse_console_meter(
    value: &Value,
    kind: WindowKind,
    period_reset: Option<ResetAt>,
) -> Option<UsageWindow> {
    let limit = micro_cents(value.get("limitMicroCents"))?;
    if limit <= 0.0 {
        return None;
    }
    // A missing or malformed amount drops the window instead of reading as
    // 0% used, which would present as a full allowance.
    let used = micro_cents(value.get("usedMicroCents"))?;
    let used_percent = (used / limit * 100.0).clamp(0.0, 100.0);
    let own_reset = value
        .get("resetsAt")
        .and_then(Value::as_str)
        .and_then(ResetAt::parse);
    let reset = match kind {
        // Only the month borrows the paid period's end; the two shorter
        // windows always carry their own reset.
        WindowKind::Monthly => own_reset.or(period_reset),
        _ => own_reset,
    };
    UsageWindow::new(kind, used_percent, reset).ok()
}

fn micro_cents(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::String(text) => text.trim().parse::<f64>().ok(),
        Value::Number(number) => number.as_f64(),
        _ => None,
    }
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

    // `source` is the collector's cache identity for every other provider;
    // overriding it here made this the one snapshot whose `source` did not
    // match the file it lives in.
    Ok(ProviderSnapshot::new(
        Provider::OpenCodeGo,
        windows,
        now_unix,
    ))
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
        assert_eq!(snapshot.source, Provider::OpenCodeGo.source());
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
    fn every_transport_and_status_failure_maps_to_an_error_not_a_quota_value() {
        // 429 and a timeout must read as failures so the caller keeps the last
        // good snapshot; neither may become a 0% window.
        let rate_limited = http_error_status(&ureq::Error::Status(
            429,
            ureq::Response::new(429, "Too Many Requests", "").unwrap(),
        ));
        assert_eq!(rate_limited, "HTTP 429");

        for code in [401, 403] {
            let status = http_error_status(&ureq::Error::Status(
                code,
                ureq::Response::new(code, "denied", "").unwrap(),
            ));
            assert!(status.contains("invalid credentials"), "{code}: {status}");
        }

        // Every mapped status is a message, never a number a window could be
        // built from. The transport arm (timeout, DNS, TLS) is ureq's own
        // Display text and cannot be constructed here, but it takes the same
        // path: an Err, so the caller keeps its last good snapshot.
        for code in [400, 429, 500, 503] {
            let status = http_error_status(&ureq::Error::Status(
                code,
                ureq::Response::new(code, "x", "").unwrap(),
            ));
            assert!(!status.contains('%'), "{code}: {status}");
        }
    }

    #[test]
    fn the_endpoint_is_the_official_host_only() {
        assert!(USAGE_URL.starts_with("https://opencode.ai/"));
    }

    /// The live console shape with the amounts kept as observed: the
    /// subscription's $12/$30/$60 meters. Strings are how the API sends them.
    fn console_shape() -> Value {
        json!({"access": {
            "startsAt": "2026-09-11T10:09:35.000Z",
            "endsAt": "2026-10-11T10:09:35.000Z",
            "cancelAtPeriodEnd": false,
            "meters": {
                "fiveHour": {
                    "startsAt": "2026-09-24T20:53:33.449Z",
                    "resetsAt": "2026-09-25T01:53:33.449Z",
                    "limitMicroCents": "1200000000",
                    "usedMicroCents": "166484388"
                },
                "week": {
                    "startsAt": "2026-09-21T00:00:00.000Z",
                    "resetsAt": "2026-09-28T00:00:00.000Z",
                    "limitMicroCents": "3000000000",
                    "usedMicroCents": "1792800655"
                },
                "month": {
                    "limitMicroCents": "6000000000",
                    "usedMicroCents": "5322521158"
                }
            }
        }})
    }

    #[test]
    fn console_meters_become_used_percentages_with_the_period_reset_on_month() {
        let snapshot = parse_console_status(&console_shape(), NOW).unwrap();
        assert_eq!(snapshot.provider, Provider::OpenCodeGo);
        let five_hour = window(&snapshot, WindowKind::FiveHour).unwrap();
        assert!((five_hour.used_percent - 13.87).abs() < 0.01);
        assert!((five_hour.remaining_percent - 86.13).abs() < 0.01);
        let weekly = window(&snapshot, WindowKind::Weekly).unwrap();
        assert!((weekly.used_percent - 59.76).abs() < 0.01);
        assert_eq!(weekly.resets_at, ResetAt::parse("2026-09-28T00:00:00.000Z"));
        let monthly = window(&snapshot, WindowKind::Monthly).unwrap();
        assert!((monthly.used_percent - 88.71).abs() < 0.01);
        assert_eq!(
            monthly.resets_at,
            ResetAt::parse("2026-10-11T10:09:35.000Z")
        );
    }

    #[test]
    fn console_numbers_are_accepted_as_numbers_too() {
        let value = json!({"access": {"endsAt": "2026-10-11T10:09:35.000Z", "meters": {
            "fiveHour": {"limitMicroCents": 1200000000, "usedMicroCents": 600000000},
            "month": {"limitMicroCents": 6_000_000_000i64, "usedMicroCents": 0}
        }}});
        let snapshot = parse_console_status(&value, NOW).unwrap();
        assert_eq!(
            window(&snapshot, WindowKind::FiveHour)
                .unwrap()
                .used_percent,
            50.0
        );
        assert_eq!(
            window(&snapshot, WindowKind::Monthly).unwrap().used_percent,
            0.0
        );
        assert!(window(&snapshot, WindowKind::Weekly).is_none());
    }

    #[test]
    fn a_meter_without_a_usable_used_amount_drops_its_window() {
        // A missing or malformed used amount must not silently read as 0% used,
        // which would present as a full allowance.
        for used in [json!(""), json!("nope"), Value::Null] {
            let value = json!({"access": {"meters": {
                "fiveHour": {"limitMicroCents": "1200000000", "usedMicroCents": used}
            }}});
            assert!(
                parse_console_status(&value, NOW).is_err(),
                "accepted {value}"
            );
        }
        let absent = json!({"access": {"meters": {
            "fiveHour": {"limitMicroCents": "1200000000"}
        }}});
        assert!(
            parse_console_status(&absent, NOW).is_err(),
            "accepted {absent}"
        );

        // Only the broken meter drops; a sibling meter still becomes a window.
        let mixed = json!({"access": {"meters": {
            "fiveHour": {"limitMicroCents": "1200000000", "usedMicroCents": "nope"},
            "week": {"limitMicroCents": "3000000000", "usedMicroCents": "600000000"}
        }}});
        let snapshot = parse_console_status(&mixed, NOW).unwrap();
        assert!(window(&snapshot, WindowKind::FiveHour).is_none());
        assert_eq!(
            window(&snapshot, WindowKind::Weekly).unwrap().used_percent,
            20.0
        );
    }

    #[test]
    fn a_console_payload_without_meters_fails_closed() {
        for payload in [
            json!({}),
            json!({"access": {}}),
            json!({"access": {"meters": {}}}),
            json!({"access": {"meters": {"fiveHour": {"limitMicroCents": "0"}}}}),
        ] {
            assert!(
                parse_console_status(&payload, NOW).is_err(),
                "accepted {payload}"
            );
        }
    }

    #[test]
    fn the_console_host_is_pinned_to_opencode_ai() {
        assert!(console_server("https://opencode.ai/console").is_some());
        assert!(console_server("https://opencode.ai").is_some());
        assert!(console_server("https://evil.example/console").is_none());
        assert!(console_server("http://opencode.ai/console").is_none());
        assert!(console_server("https://opencode.ai.evil.example").is_none());
    }
}
