use crate::cache::CacheStore;
use crate::model::{ContextUsage, Provider, ProviderSnapshot, ResetAt, UsageWindow, WindowKind};
use crate::providers::statusline::{parse_context, parse_model};
use crate::providers::ProviderError;
use serde_json::Value;

pub fn parse_statusline(
    value: &Value,
    fetched_at_unix: u64,
) -> std::result::Result<ProviderSnapshot, ProviderError> {
    let mut context = parse_context(
        value
            .get("context_window")
            .or_else(|| value.get("contextWindow")),
    )
    .unwrap_or(None);
    apply_prompt_cache(
        &mut context,
        value
            .get("prompt_cache")
            .or_else(|| value.get("promptCache")),
    );
    let model = parse_model(value);
    let Some(limits) = value.get("rate_limits") else {
        return Ok(
            ProviderSnapshot::new(Provider::Claude, vec![], fetched_at_unix)
                .with_model(model)
                .with_context(context),
        );
    };
    let mut windows = Vec::new();
    if let Some(window) = parse_window(limits.get("five_hour"), WindowKind::FiveHour)? {
        windows.push(window);
    }
    if let Some(window) = parse_window(limits.get("seven_day"), WindowKind::Weekly)? {
        windows.push(window);
    }
    if windows.is_empty() {
        return Ok(
            ProviderSnapshot::new(Provider::Claude, vec![], fetched_at_unix)
                .with_model(model)
                .with_context(context),
        );
    }
    Ok(
        ProviderSnapshot::new(Provider::Claude, windows, fetched_at_unix)
            .with_model(model)
            .with_context(context),
    )
}

fn parse_window(
    value: Option<&Value>,
    kind: WindowKind,
) -> std::result::Result<Option<UsageWindow>, ProviderError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let used = value
        .get("used_percentage")
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            ProviderError::UnsupportedResponse(format!("missing {} usage", kind.label()))
        })?;
    let reset = value.get("resets_at").and_then(|value| {
        value
            .as_u64()
            .map(ResetAt::from_unix_seconds)
            .or_else(|| value.as_str().and_then(ResetAt::parse))
    });
    UsageWindow::new(kind, used, reset)
        .map(Some)
        .map_err(|error| ProviderError::UnsupportedResponse(error.to_string()))
}

/// Claude Code v2.1.251+ reports the live prefix expiry on statusLine stdin.
/// Cold or missing expiry clears any previous countdown instead of guessing
/// from a transcript bucket.
pub(crate) fn apply_prompt_cache(context: &mut Option<ContextUsage>, prompt_cache: Option<&Value>) {
    let Some(prompt_cache) = prompt_cache.filter(|value| !value.is_null()) else {
        return;
    };
    let Some(object) = prompt_cache.as_object() else {
        return;
    };
    let Some(cache) = context.as_mut().and_then(|context| context.cache.as_mut()) else {
        return;
    };
    let warm = object.get("warm").and_then(Value::as_bool) != Some(false);
    let expires_at = object.get("expires_at").and_then(parse_expires_at);
    match (warm, expires_at) {
        (true, Some(expires_at)) => {
            cache.expires_at_unix = Some(expires_at);
            cache.ttl_seconds = object
                .get("ttl")
                .and_then(Value::as_str)
                .and_then(parse_prompt_cache_ttl);
            if let Some(ttl_seconds) = cache.ttl_seconds {
                cache.last_activity_unix = Some(expires_at.saturating_sub(ttl_seconds));
            }
        }
        _ => {
            cache.expires_at_unix = Some(0);
            cache.ttl_seconds = None;
            cache.last_activity_unix = None;
        }
    }
}

fn parse_prompt_cache_ttl(value: &str) -> Option<u64> {
    match value.trim() {
        "5m" => Some(5 * 60),
        "1h" => Some(60 * 60),
        _ => None,
    }
}

fn parse_expires_at(value: &Value) -> Option<u64> {
    if value.is_null() {
        return None;
    }
    value
        .as_u64()
        .or_else(|| {
            let number = value.as_f64()?;
            (number.is_finite() && number >= 0.0).then_some(number.round() as u64)
        })
        .or_else(|| {
            value
                .as_str()
                .and_then(ResetAt::parse)
                .map(ResetAt::unix_seconds)
        })
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
    fn parses_claude_five_hour_and_weekly_limits() {
        let value = json!({
            "rate_limits": {
                "five_hour": {"used_percentage": 58.0, "resets_at": 1786795200},
                "seven_day": {"used_percentage": 27.0, "resets_at": 1787400000}
            }
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        assert_eq!(
            snapshot.window(WindowKind::FiveHour).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(1_786_795_200))
        );
    }

    #[test]
    fn parses_optional_context_window_usage() {
        let value = json!({
            "context_window": {
                "used_percentage": 23.5,
                "remaining_percentage": 76.5,
                "current_usage": {
                    "input_tokens": 100,
                    "cache_read_input_tokens": 800,
                    "cache_creation_input_tokens": 100
                }
            },
            "rate_limits": {
                "five_hour": {"used_percentage": 58.0}
            }
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        assert_eq!(
            snapshot
                .context
                .as_ref()
                .map(|context| context.used_percent),
            Some(23.5)
        );
        let cache = snapshot.context.as_ref().unwrap().cache.as_ref().unwrap();
        assert_eq!(cache.read_tokens, 800);
        assert_eq!(cache.creation_tokens, 100);
        assert_eq!(cache.hit_percent, 80.0);
    }

    #[test]
    fn parses_the_human_readable_active_model_name() {
        let value = json!({
            "model": {"id": "claude-sonnet-4-20250514", "display_name": "Sonnet"},
            "rate_limits": {"five_hour": {"used_percentage": 1.0}}
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        assert_eq!(snapshot.model.as_deref(), Some("Sonnet"));
    }

    #[test]
    fn reads_prompt_cache_expiry_from_statusline() {
        let value = json!({
            "context_window": {
                "used_percentage": 23.5,
                "current_usage": {
                    "input_tokens": 10,
                    "cache_read_input_tokens": 80,
                    "cache_creation_input_tokens": 10
                }
            },
            "prompt_cache": {
                "warm": true,
                "ttl": "1h",
                "expires_at": 1_787_396_400
            },
            "rate_limits": {
                "five_hour": {"used_percentage": 58.0}
            }
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        let cache = snapshot.context.unwrap().cache.unwrap();
        assert_eq!(cache.expires_at_unix, Some(1_787_396_400));
        assert_eq!(cache.ttl_seconds, Some(60 * 60));
        assert_eq!(cache.last_activity_unix, Some(1_787_392_800));
        assert_eq!(cache.remaining_ttl_seconds(1_787_392_800), Some(3_600));
    }

    #[test]
    fn cold_prompt_cache_clears_ttl() {
        let value = json!({
            "context_window": {
                "used_percentage": 23.5,
                "current_usage": {
                    "input_tokens": 10,
                    "cache_read_input_tokens": 80,
                    "cache_creation_input_tokens": 0
                }
            },
            "prompt_cache": {
                "warm": false,
                "ttl": "1h",
                "expires_at": null
            },
            "rate_limits": {
                "five_hour": {"used_percentage": 58.0}
            }
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        let cache = snapshot.context.unwrap().cache.unwrap();
        assert_eq!(cache.expires_at_unix, Some(0));
        assert!(cache.ttl_seconds.is_none());
        assert_eq!(cache.remaining_ttl_seconds(1_787_392_800), Some(0));
    }

    #[test]
    fn ignores_transcript_buckets_when_prompt_cache_is_absent() {
        let value = json!({
            "transcript_path": "/tmp/unused.jsonl",
            "context_window": {
                "used_percentage": 23.5,
                "current_usage": {
                    "input_tokens": 10,
                    "cache_read_input_tokens": 80,
                    "cache_creation_input_tokens": 10
                }
            },
            "rate_limits": {
                "five_hour": {"used_percentage": 58.0}
            }
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        let cache = snapshot.context.unwrap().cache.unwrap();
        assert!(cache.expires_at_unix.is_none());
        assert!(cache.ttl_seconds.is_none());
        assert!(cache.last_activity_unix.is_none());
    }

    #[test]
    fn parses_rfc3339_reset_emitted_by_claude_statusline() {
        let value = json!({
            "rate_limits": {
                "five_hour": {
                    "used_percentage": 57.0,
                    "resets_at": "2026-08-15T12:00:00Z"
                }
            }
        });
        let snapshot = parse_statusline(&value, 1).unwrap();
        assert_eq!(
            snapshot.window(WindowKind::FiveHour).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(1_786_795_200))
        );
    }

    #[test]
    fn allows_a_missing_claude_window() {
        let value = json!({"rate_limits": {"five_hour": null}});
        assert!(parse_statusline(&value, 1).unwrap().windows.is_empty());
        let value = json!({
            "rate_limits": {"seven_day": {"used_percentage": 25.0}}
        });
        assert_eq!(parse_statusline(&value, 1).unwrap().windows.len(), 1);
    }

    #[test]
    fn accepts_a_payload_without_rate_limits_to_clear_a_stale_quota() {
        let value = json!({"context_window": {"used_percentage": 43.0}});
        let snapshot = parse_statusline(&value, 1).unwrap();
        assert!(snapshot.windows.is_empty());
        assert_eq!(
            snapshot
                .context
                .as_ref()
                .map(|context| context.used_percent),
            Some(43.0)
        );
    }

    #[test]
    fn rejects_non_json_statusline_input() {
        assert!(run_statusline(b"not-json").is_err());
    }
}
