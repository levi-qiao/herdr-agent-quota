use crate::cache::CacheStore;
use crate::model::{Provider, ProviderSnapshot, ResetAt, UsageWindow, WindowKind};
use crate::providers::statusline::{parse_context, parse_model};
use crate::providers::ProviderError;
use serde_json::Value;

/// Fallback key lists used when the active pool cannot be determined.
const FIVE_HOUR_KEYS: [&str; 2] = ["gemini-5h", "3p-5h"];
const WEEKLY_KEYS: [&str; 2] = ["gemini-weekly", "3p-weekly"];

/// Spellings whose value is already a `0..=1` fraction.
const FRACTION_KEYS: [&str; 2] = ["remaining_fraction", "remainingFraction"];
/// Spellings whose value is a `0..=100` percentage.
const PERCENT_KEYS: [&str; 2] = ["remaining_percent", "remainingPercentage"];

const GEMINI_FIVE_HOUR_KEYS: [&str; 1] = ["gemini-5h"];
const GEMINI_WEEKLY_KEYS: [&str; 1] = ["gemini-weekly"];
const THIRD_PARTY_FIVE_HOUR_KEYS: [&str; 1] = ["3p-5h"];
const THIRD_PARTY_WEEKLY_KEYS: [&str; 1] = ["3p-weekly"];

/// The quota pool that the active model draws from.
#[derive(Debug, Clone, Copy)]
enum Pool {
    Gemini,
    ThirdParty,
}

impl Pool {
    fn five_hour_keys(self) -> &'static [&'static str] {
        match self {
            Self::Gemini => &GEMINI_FIVE_HOUR_KEYS,
            Self::ThirdParty => &THIRD_PARTY_FIVE_HOUR_KEYS,
        }
    }

    fn weekly_keys(self) -> &'static [&'static str] {
        match self {
            Self::Gemini => &GEMINI_WEEKLY_KEYS,
            Self::ThirdParty => &THIRD_PARTY_WEEKLY_KEYS,
        }
    }
}

/// Detect which quota pool the active model draws from.
///
/// Gemini-family model names contain `gemini`, `flash`, or `learnlm`.
/// Third-party model names include Claude variants (`claude`, `sonnet`,
/// `haiku`, `opus`), GPT models, and OpenAI reasoning series (`o1`, `o3`,
/// `o4`). Returns `None` for unrecognised names so the caller falls back to
/// the conservative minimum across both pools.
fn active_pool(model: Option<&str>) -> Option<Pool> {
    let lower = model?.to_ascii_lowercase();
    if lower.contains("gemini") || lower.contains("flash") || lower.contains("learnlm") {
        Some(Pool::Gemini)
    } else if lower.contains("claude")
        || lower.contains("sonnet")
        || lower.contains("haiku")
        || lower.contains("opus")
        || lower.contains("gpt")
        || lower.starts_with("o1")
        || lower.starts_with("o3")
        || lower.starts_with("o4")
    {
        Some(Pool::ThirdParty)
    } else {
        None
    }
}

/// Parse the quota object emitted by Agy/Antigravity's statusLine JSON.
///
/// Agy reports separate Gemini and third-party (Claude/GPT) pools. When the
/// active model can be identified, only its pool's quota is shown so the
/// sidebar reflects the limit that actually applies to the current session.
/// For unrecognised model names the sidebar falls back to the conservative
/// minimum across both pools.
pub fn parse_statusline(
    value: &Value,
    fetched_at_unix: u64,
) -> std::result::Result<ProviderSnapshot, ProviderError> {
    let quota = value
        .get("quota")
        .and_then(Value::as_object)
        .ok_or_else(|| ProviderError::UnsupportedResponse("missing quota".to_string()))?;
    let model = parse_model(value);
    let pool = active_pool(model.as_deref());
    let five_hour_keys: &[&str] = pool.map_or(&FIVE_HOUR_KEYS, Pool::five_hour_keys);
    let weekly_keys: &[&str] = pool.map_or(&WEEKLY_KEYS, Pool::weekly_keys);
    let mut windows = Vec::new();
    for (kind, keys) in [
        (WindowKind::FiveHour, five_hour_keys),
        (WindowKind::Weekly, weekly_keys),
    ] {
        if let Some(window) = parse_window(quota, kind, keys, fetched_at_unix)? {
            windows.push(window);
        }
    }
    if windows.is_empty() {
        return Err(ProviderError::UnsupportedResponse(
            "quota has no supported windows".to_string(),
        ));
    }
    Ok(
        ProviderSnapshot::new(Provider::Agy, windows, fetched_at_unix)
            .with_model(model)
            .with_context(
                parse_context(
                    value
                        .get("context_window")
                        .or_else(|| value.get("contextWindow")),
                )
                .unwrap_or(None),
            ),
    )
}

fn parse_window(
    quota: &serde_json::Map<String, Value>,
    kind: WindowKind,
    keys: &[&str],
    fetched_at_unix: u64,
) -> std::result::Result<Option<UsageWindow>, ProviderError> {
    let mut lowest_remaining: Option<f64> = None;
    let mut reset = None;
    for key in keys {
        let Some(bucket) = quota.get(*key) else {
            continue;
        };
        let Some(remaining) = parse_remaining(bucket) else {
            continue;
        };
        if lowest_remaining.is_none_or(|current| remaining < current) {
            lowest_remaining = Some(remaining);
            reset = parse_reset(bucket, fetched_at_unix);
        }
    }
    let Some(remaining) = lowest_remaining else {
        return Ok(None);
    };
    let used = (100.0 - remaining * 100.0).clamp(0.0, 100.0);
    UsageWindow::new(kind, used, reset)
        .map(Some)
        .map_err(|error| ProviderError::UnsupportedResponse(error.to_string()))
}

fn parse_reset(value: &Value, fetched_at_unix: u64) -> Option<ResetAt> {
    value
        .get("reset_time")
        .or_else(|| value.get("resetTime"))
        .and_then(Value::as_str)
        .and_then(ResetAt::parse_rfc3339)
        .or_else(|| {
            value
                .get("reset_in_seconds")
                .or_else(|| value.get("resetInSeconds"))
                .and_then(Value::as_u64)
                .map(|seconds| ResetAt::after(fetched_at_unix, seconds))
        })
}

/// Remaining allowance as a `0..=1` fraction.
///
/// The scale comes from the key, never from the value. Inferring it from the
/// magnitude reads a `remaining_percent` of `1.0` as a full pool instead of a
/// nearly exhausted one — the same 100x error, in the same dangerous
/// direction, that [`crate::providers::opencode_go`] exists to avoid.
fn parse_remaining(value: &Value) -> Option<f64> {
    let object = value.as_object()?;
    let (raw, per_unit) = FRACTION_KEYS
        .iter()
        .find_map(|key| {
            object
                .get(*key)
                .and_then(Value::as_f64)
                .map(|raw| (raw, 1.0))
        })
        .or_else(|| {
            PERCENT_KEYS.iter().find_map(|key| {
                object
                    .get(*key)
                    .and_then(Value::as_f64)
                    .map(|raw| (raw, 100.0))
            })
        })?;
    raw.is_finite().then(|| (raw / per_unit).clamp(0.0, 1.0))
}

pub fn run_statusline(input: &[u8]) -> std::result::Result<ProviderSnapshot, ProviderError> {
    let value: Value = serde_json::from_slice(input).map_err(|_| {
        ProviderError::UnsupportedResponse("statusLine input is not JSON".to_string())
    })?;
    parse_statusline(&value, CacheStore::now_unix())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_both_agy_windows_from_official_quota_keys() {
        let value = json!({
            "quota": {
                "gemini-5h": {"remaining_fraction": 0.9969, "reset_time": "2026-08-15T12:00:00Z"},
                "gemini-weekly": {"remaining_fraction": 0.8, "reset_time": "2026-08-22T12:00:00Z"},
                "3p-5h": {"remaining_fraction": 0.72, "reset_time": "2026-08-15T13:00:00Z"},
                "3p-weekly": {"remaining_fraction": 0.91}
            }
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        assert_eq!(snapshot.provider, Provider::Agy);
        assert_eq!(
            snapshot.window(WindowKind::FiveHour).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(1_786_798_800))
        );
    }

    #[test]
    fn parses_optional_context_window_usage() {
        let value = json!({
            "context_window": {
                "used_percentage": 41.0,
                "current_usage": {
                    "input_tokens": 50,
                    "cache_read_input_tokens": 150,
                    "cache_creation_input_tokens": 0
                }
            },
            "quota": {
                "gemini-weekly": {"remaining_fraction": 0.8}
            }
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        assert_eq!(
            snapshot
                .context
                .as_ref()
                .map(|context| context.used_percent),
            Some(41.0)
        );
        assert_eq!(
            snapshot
                .context
                .as_ref()
                .unwrap()
                .cache
                .as_ref()
                .unwrap()
                .hit_percent,
            75.0
        );
    }

    #[test]
    fn parses_the_human_readable_active_model_name() {
        let value = json!({
            "model": {"id": "gemini-3.5-flash", "display_name": "Gemini Flash"},
            "quota": {"gemini-weekly": {"remaining_fraction": 0.8}}
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        assert_eq!(snapshot.model.as_deref(), Some("Gemini Flash"));
    }

    #[test]
    fn ignores_missing_pool_without_marking_the_window_unavailable() {
        let value = json!({
            "quota": {"gemini-weekly": {"remaining_fraction": 0.61}}
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        assert!(snapshot.window(WindowKind::FiveHour).is_none());
    }

    #[test]
    fn derives_absolute_agy_reset_from_relative_seconds() {
        let value = json!({
            "quota": {"gemini-5h": {
                "remaining_fraction": 0.5,
                "reset_in_seconds": 900
            }}
        });
        let snapshot = parse_statusline(&value, 1_000).unwrap();
        assert_eq!(
            snapshot.window(WindowKind::FiveHour).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(1_900))
        );
    }

    #[test]
    fn rejects_payload_without_quota_windows() {
        assert!(parse_statusline(&json!({"quota": {}}), 1).is_err());
    }

    #[test]
    fn routes_claude_model_to_third_party_pool() {
        // gemini-5h is exhausted (0 %), but the active model is Claude so the
        // sidebar should show the 3p-5h value (52 %) not the minimum (0 %).
        let value = json!({
            "model": {"display_name": "Claude Sonnet 4.5"},
            "quota": {
                "gemini-5h": {"remaining_fraction": 0.0, "reset_in_seconds": 1000},
                "gemini-weekly": {"remaining_fraction": 0.8, "reset_in_seconds": 7200},
                "3p-5h": {"remaining_fraction": 0.52, "reset_in_seconds": 5000},
                "3p-weekly": {"remaining_fraction": 0.84, "reset_in_seconds": 90000}
            }
        });
        let snapshot = parse_statusline(&value, 0).unwrap();
        // 3p-5h reset_in_seconds=5000 from base 0 → resets_at 5000
        assert_eq!(
            snapshot.window(WindowKind::FiveHour).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(5000))
        );
        assert_remaining_pct(&snapshot, WindowKind::FiveHour, 52.0);
        assert_remaining_pct(&snapshot, WindowKind::Weekly, 84.0);
    }

    #[test]
    fn routes_gemini_model_to_gemini_pool() {
        // 3p-5h is low (10 %), but the active model is Gemini so the sidebar
        // should show the gemini-5h value (75 %) not the minimum (10 %).
        let value = json!({
            "model": {"display_name": "Gemini Flash"},
            "quota": {
                "gemini-5h": {"remaining_fraction": 0.75, "reset_in_seconds": 1000},
                "gemini-weekly": {"remaining_fraction": 0.9, "reset_in_seconds": 7200},
                "3p-5h": {"remaining_fraction": 0.1, "reset_in_seconds": 5000},
                "3p-weekly": {"remaining_fraction": 0.2, "reset_in_seconds": 90000}
            }
        });
        let snapshot = parse_statusline(&value, 0).unwrap();
        // gemini-5h reset_in_seconds=1000 from base 0 → resets_at 1000
        assert_eq!(
            snapshot.window(WindowKind::FiveHour).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(1000))
        );
        assert_remaining_pct(&snapshot, WindowKind::FiveHour, 75.0);
        assert_remaining_pct(&snapshot, WindowKind::Weekly, 90.0);
    }

    #[test]
    fn falls_back_to_conservative_min_for_unknown_model() {
        // Unrecognised model → min(gemini-5h=90 %, 3p-5h=30 %) = 30 % with
        // the 3p-5h reset timestamp.
        let value = json!({
            "model": {"display_name": "Future Model XYZ"},
            "quota": {
                "gemini-5h": {"remaining_fraction": 0.9, "reset_in_seconds": 1000},
                "3p-5h": {"remaining_fraction": 0.3, "reset_in_seconds": 5000}
            }
        });
        let snapshot = parse_statusline(&value, 0).unwrap();
        assert_eq!(
            snapshot.window(WindowKind::FiveHour).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(5000))
        );
        assert_remaining_pct(&snapshot, WindowKind::FiveHour, 30.0);
    }

    #[test]
    fn routes_gpt_oss_to_third_party_pool() {
        let value = json!({
            "model": {"display_name": "GPT-OSS 120B (Medium)"},
            "quota": {
                "gemini-5h": {"remaining_fraction": 0.0, "reset_in_seconds": 1000},
                "3p-5h": {"remaining_fraction": 0.52, "reset_in_seconds": 5000},
                "3p-weekly": {"remaining_fraction": 0.84, "reset_in_seconds": 90000}
            }
        });
        let snapshot = parse_statusline(&value, 0).unwrap();
        assert_eq!(
            snapshot.window(WindowKind::FiveHour).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(5000))
        );
        assert_remaining_pct(&snapshot, WindowKind::FiveHour, 52.0);
        assert_remaining_pct(&snapshot, WindowKind::Weekly, 84.0);
    }

    /// A percentage below 1 must not be mistaken for a fraction. Reading
    /// `remaining_percent: 1.0` as "full" paints a nearly exhausted pool green.
    #[test]
    fn a_small_remaining_percent_is_not_rescaled_to_a_full_pool() {
        for (key, raw, expected_remaining) in [
            ("remaining_percent", 1.0, 1.0),
            ("remaining_percent", 0.5, 0.5),
            ("remainingPercentage", 5.0, 5.0),
            ("remaining_fraction", 1.0, 100.0),
            ("remainingFraction", 0.5, 50.0),
        ] {
            let value = json!({"quota": {"gemini-5h": {key: raw}}});
            let snapshot = parse_statusline(&value, 0).unwrap();
            assert_remaining_pct(&snapshot, WindowKind::FiveHour, expected_remaining);
        }
    }

    #[test]
    fn a_fraction_key_wins_over_a_percent_key_in_the_same_bucket() {
        let value = json!({"quota": {"gemini-5h": {
            "remaining_fraction": 0.25,
            "remaining_percent": 25.0
        }}});
        let snapshot = parse_statusline(&value, 0).unwrap();
        assert_remaining_pct(&snapshot, WindowKind::FiveHour, 25.0);
    }

    #[test]
    fn out_of_range_and_non_finite_remaining_values_fail_closed() {
        let clamped = parse_statusline(
            &json!({"quota": {"gemini-5h": {"remaining_percent": 150.0}}}),
            0,
        )
        .unwrap();
        assert_remaining_pct(&clamped, WindowKind::FiveHour, 100.0);

        let negative = parse_statusline(
            &json!({"quota": {"gemini-5h": {"remaining_fraction": -1.0}}}),
            0,
        )
        .unwrap();
        assert_remaining_pct(&negative, WindowKind::FiveHour, 0.0);

        assert!(parse_statusline(
            &json!({"quota": {"gemini-5h": {"remaining_fraction": "lots"}}}),
            0
        )
        .is_err());
    }

    fn assert_remaining_pct(snapshot: &ProviderSnapshot, kind: WindowKind, expected: f64) {
        let actual = snapshot.window(kind).unwrap().remaining_percent;
        assert!(
            (actual - expected).abs() < 1e-9,
            "{kind:?} remaining {actual} != {expected}"
        );
    }
}
