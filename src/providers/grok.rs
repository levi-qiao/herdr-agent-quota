use crate::cache::CacheStore;
use crate::model::{Provider, ProviderSnapshot, ResetAt, UsageWindow, WindowKind};
use crate::providers::ProviderError;
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrokCredentials {
    pub key: String,
    pub user_id: Option<String>,
}

pub fn fetch() -> Result<ProviderSnapshot> {
    let path = auth_path().context("resolve Grok auth path")?;
    let credentials = read_credentials(&path).map_err(anyhow::Error::from)?;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .timeout_write(Duration::from_secs(10))
        .build();
    let mut request = agent
        .get(BILLING_URL)
        .set("Authorization", &format!("Bearer {}", credentials.key))
        .set("X-XAI-Token-Auth", "xai-grok-cli")
        .set("Accept", "application/json");
    if let Some(user_id) = &credentials.user_id {
        request = request.set("x-userid", user_id);
    }
    let response = request
        .call()
        .map_err(|error| ProviderError::Request(http_error_status(&error)))
        .map_err(anyhow::Error::from)?;
    let value: Value = response
        .into_json()
        .context("decode Grok billing response")?;
    parse_billing_response(&value, CacheStore::now_unix())
        .map(|snapshot| snapshot.with_account_id(credentials.user_id.clone()))
        .map_err(anyhow::Error::from)
}

pub fn auth_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("GROK_AUTH_FILE") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let home = PathBuf::from(home);
    let grok_home = std::env::var_os("GROK_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".grok"));
    Ok(grok_home.join("auth.json"))
}

pub fn read_credentials(path: &Path) -> std::result::Result<GrokCredentials, ProviderError> {
    let bytes = fs::read(path).map_err(|_| ProviderError::MissingCredentials)?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| ProviderError::Unavailable("Grok auth file is not valid JSON".to_string()))?;
    select_credentials(collect_credentials(&value)).ok_or(ProviderError::MissingCredentials)
}

pub fn auth_mtime_unix(path: &Path) -> Option<u64> {
    CacheStore::file_mtime_unix(path)
}

fn collect_credentials(value: &Value) -> Vec<CredentialCandidate> {
    let mut credentials = Vec::new();
    collect_credentials_into(value, &mut credentials);
    credentials
}

fn collect_credentials_into(value: &Value, credentials: &mut Vec<CredentialCandidate>) {
    match value {
        Value::Object(map) => {
            if let Some(key) = map.get("key").and_then(Value::as_str) {
                if !key.trim().is_empty() {
                    credentials.push(CredentialCandidate {
                        credentials: GrokCredentials {
                            key: key.to_string(),
                            user_id: map
                                .get("user_id")
                                .and_then(Value::as_str)
                                .filter(|value| !value.is_empty())
                                .map(str::to_string),
                        },
                        expires_at: map
                            .get("expires_at")
                            .and_then(Value::as_str)
                            .and_then(ResetAt::parse_rfc3339)
                            .map(ResetAt::unix_seconds),
                        create_time: map
                            .get("create_time")
                            .and_then(Value::as_str)
                            .and_then(ResetAt::parse_rfc3339)
                            .map(ResetAt::unix_seconds),
                    });
                    return;
                }
            }
            for child in map.values() {
                collect_credentials_into(child, credentials);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_credentials_into(child, credentials);
            }
        }
        _ => {}
    }
}

fn select_credentials(candidates: Vec<CredentialCandidate>) -> Option<GrokCredentials> {
    if candidates.is_empty() {
        return None;
    }
    let now = CacheStore::now_unix();
    let unexpired = candidates
        .iter()
        .filter(|candidate| {
            candidate
                .expires_at
                .is_none_or(|expires_at| expires_at > now)
        })
        .cloned()
        .collect::<Vec<_>>();
    let pool = if unexpired.is_empty() {
        candidates
    } else {
        unexpired
    };
    pool.into_iter()
        .max_by_key(|candidate| {
            (
                candidate.create_time.unwrap_or(0),
                candidate.expires_at.unwrap_or(0),
            )
        })
        .map(|candidate| candidate.credentials)
}

#[derive(Debug, Clone)]
struct CredentialCandidate {
    credentials: GrokCredentials,
    expires_at: Option<u64>,
    create_time: Option<u64>,
}

pub fn parse_billing_response(
    value: &Value,
    fetched_at_unix: u64,
) -> std::result::Result<ProviderSnapshot, ProviderError> {
    let config = value
        .get("config")
        .ok_or_else(|| ProviderError::UnsupportedResponse("missing config".to_string()))?;
    // proto3 JSON omits zero scalars, so a fresh SuperGrok week with 0% used
    // arrives without `creditUsagePercent`. That is unused, not "unsupported".
    let usage = match config.get("creditUsagePercent") {
        None | Some(Value::Null) => 0.0,
        Some(value) => value.as_f64().ok_or_else(|| {
            ProviderError::UnsupportedResponse(
                "config.creditUsagePercent is not a number".to_string(),
            )
        })?,
    };
    let period = config
        .get("currentPeriod")
        .ok_or_else(|| ProviderError::UnsupportedResponse("missing currentPeriod".to_string()))?;
    let period_type = period
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !period_type.contains("WEEKLY") {
        return Err(ProviderError::UnsupportedResponse(format!(
            "current period is not weekly: {period_type}"
        )));
    }
    let reset = period
        .get("end")
        .and_then(Value::as_str)
        .and_then(ResetAt::parse_rfc3339);
    let window = UsageWindow::new(WindowKind::Weekly, usage, reset)
        .map_err(|error| ProviderError::UnsupportedResponse(error.to_string()))?;
    Ok(ProviderSnapshot::new(
        Provider::Grok,
        vec![window],
        fetched_at_unix,
    ))
}

fn http_error_status(error: &ureq::Error) -> String {
    match error {
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(error) => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn parses_grok_weekly_credit_pool_as_remaining_percentage() {
        let value = json!({
            "config": {
                "creditUsagePercent": 42.5,
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "end": "2026-08-22T00:00:00Z"
                }
            }
        });
        let snapshot = parse_billing_response(&value, 1).unwrap();
        assert_eq!(snapshot.provider, Provider::Grok);
        assert_eq!(
            snapshot
                .window(WindowKind::Weekly)
                .unwrap()
                .remaining_percent,
            57.5
        );
        assert_eq!(
            snapshot.window(WindowKind::Weekly).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(1_787_356_800))
        );
    }

    #[test]
    fn rejects_monthly_period_instead_of_calling_it_weekly() {
        let value = json!({
            "config": {
                "creditUsagePercent": 42.5,
                "currentPeriod": {"type": "USAGE_PERIOD_TYPE_MONTHLY"}
            }
        });
        assert!(parse_billing_response(&value, 1).is_err());
    }

    #[test]
    fn reads_only_login_key_from_nested_auth_shape() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("auth.json");
        fs::write(
            &path,
            r#"{"auth.x.ai":{"oidc":{"key":"login-token","refresh_token":"do-not-read","user_id":"u1"}}}"#,
        )
        .unwrap();
        assert_eq!(
            read_credentials(&path).unwrap(),
            GrokCredentials {
                key: "login-token".to_string(),
                user_id: Some("u1".to_string())
            }
        );
    }

    #[test]
    fn missing_auth_is_unavailable() {
        let directory = tempdir().unwrap();
        assert_eq!(
            read_credentials(&directory.path().join("missing.json"))
                .unwrap_err()
                .to_string(),
            "provider credentials are unavailable"
        );
    }

    #[test]
    fn omitted_credit_usage_percent_is_zero_used_not_a_parse_error() {
        let value = json!({
            "config": {
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "end": "2026-08-31T14:23:46Z"
                }
            }
        });
        let snapshot = parse_billing_response(&value, 1).unwrap();
        let weekly = snapshot.window(WindowKind::Weekly).unwrap();
        assert_eq!(weekly.used_percent, 0.0);
        assert_eq!(weekly.remaining_percent, 100.0);
        assert_eq!(
            weekly.resets_at,
            Some(ResetAt::from_unix_seconds(1_788_186_226))
        );
    }

    #[test]
    fn explicit_zero_credit_usage_percent_is_full_remaining() {
        let value = json!({
            "config": {
                "creditUsagePercent": 0.0,
                "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY"}
            }
        });
        assert_eq!(
            parse_billing_response(&value, 1)
                .unwrap()
                .window(WindowKind::Weekly)
                .unwrap()
                .remaining_percent,
            100.0
        );
    }

    #[test]
    fn prefers_the_newest_unexpired_login_when_auth_json_still_has_another_account() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("auth.json");
        fs::write(
            &path,
            r#"{
                "https://auth.x.ai::old": {
                    "key": "old-token",
                    "user_id": "u-old",
                    "expires_at": "2099-12-01T00:00:00Z",
                    "create_time": "2020-01-01T00:00:00Z"
                },
                "https://auth.x.ai::new": {
                    "key": "new-token",
                    "user_id": "u-new",
                    "expires_at": "2099-06-01T00:00:00Z",
                    "create_time": "2026-08-25T00:56:20Z"
                }
            }"#,
        )
        .unwrap();
        assert_eq!(
            read_credentials(&path).unwrap(),
            GrokCredentials {
                key: "new-token".to_string(),
                user_id: Some("u-new".to_string())
            }
        );
    }
}
