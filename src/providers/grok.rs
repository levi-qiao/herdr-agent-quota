use crate::cache::CacheStore;
use crate::model::{
    CacheTotals, CacheUsage, ContextUsage, Provider, ProviderSnapshot, ResetAt, UsageWindow,
    WindowKind,
};
use crate::providers::ProviderError;
use anyhow::{Context, Result};
use serde_json::Value;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const GROK_SESSION_TAIL_BYTES: u64 = 128 * 1024;
const MAX_LOCAL_SESSIONS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrokCredentials {
    pub key: String,
    pub user_id: Option<String>,
}

/// Fetch Grok billing and enrich only the visible pane sessions when Herdr
/// provides their ids. Direct CLI refreshes pass an empty slice and use the
/// bounded newest-session fallback instead.
pub fn fetch_for_sessions(session_ids: &[String]) -> Result<ProviderSnapshot> {
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
    let mut snapshot =
        parse_billing_response(&value, CacheStore::now_unix()).map_err(anyhow::Error::from)?;
    enrich_local_sessions(&mut snapshot, session_ids);
    Ok(snapshot.with_account_id(credentials.user_id.clone()))
}

pub fn auth_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("GROK_AUTH_FILE") {
        return Ok(PathBuf::from(path));
    }
    Ok(grok_home()?.join("auth.json"))
}

fn grok_home() -> Result<PathBuf> {
    let home = crate::home_dir()?;
    Ok(std::env::var_os("GROK_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".grok")))
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
    // The billing period names the window; it is never assumed. A monthly pool
    // is reported as 30d rather than discarded, but it must never be labelled
    // 7d — that would understate how long the credits have to last.
    let kind = if period_type.contains("WEEKLY") {
        WindowKind::Weekly
    } else if period_type.contains("MONTHLY") {
        WindowKind::Monthly
    } else {
        return Err(ProviderError::UnsupportedResponse(format!(
            "unsupported credit period: {period_type}"
        )));
    };
    let reset = period
        .get("end")
        .and_then(Value::as_str)
        .and_then(ResetAt::parse_rfc3339);
    let window = UsageWindow::new(kind, usage, reset)
        .map_err(|error| ProviderError::UnsupportedResponse(error.to_string()))?;
    Ok(ProviderSnapshot::new(
        Provider::Grok,
        vec![window],
        fetched_at_unix,
    ))
}

/// Read the small, provider-owned session metadata files that Grok writes
/// locally. Billing remains the quota source; these files only supplement the
/// pane diagnostics that billing does not contain. The scan is bounded to the
/// newest session directories and never reads Herdr panes or prompt text.
fn enrich_local_sessions(snapshot: &mut ProviderSnapshot, session_ids: &[String]) {
    let Some(home) = grok_home().ok() else {
        return;
    };
    enrich_local_sessions_at(snapshot, &home, session_ids);
}

fn enrich_local_sessions_at(snapshot: &mut ProviderSnapshot, home: &Path, session_ids: &[String]) {
    let sessions_dir = home.join("sessions");
    let active = read_active_sessions(home);
    let lookup_ids = expand_session_ids_with_active_siblings(&active, session_ids);
    let sessions = if lookup_ids.is_empty() {
        collect_recent_session_dirs(&sessions_dir)
    } else {
        collect_matching_session_dirs(&sessions_dir, &lookup_ids)
    };

    let mut newest: Option<(u64, Option<String>, ContextUsage)> = None;
    for (modified, session_dir) in sessions {
        let Some(session_id) = session_dir
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let Some(observation) = observe_session_dir(&session_dir, session_id) else {
            continue;
        };
        if let Some(model) = observation.model.clone() {
            snapshot
                .session_models
                .insert(session_id.to_string(), model);
        }
        let Some(context) = observation.context else {
            continue;
        };
        snapshot
            .session_contexts
            .insert(session_id.to_string(), context.clone());
        if newest
            .as_ref()
            .is_none_or(|(current, _, _)| modified >= *current)
        {
            newest = Some((modified, observation.model, context));
        }
    }
    alias_missing_diagnostics_from_active_siblings(snapshot, &active, session_ids);
    if let Some((_, model, context)) = newest {
        if model.is_some() {
            snapshot.model = model;
        }
        snapshot.context = Some(context);
    }
}

/// Grok stores sessions at `sessions/<encoded-cwd>/<session-id>/`. Context and
/// cache live in `signals.json` when present; newer sessions may only have
/// `summary.json` (`current_model_id`) until the first usage signal is written.
fn collect_recent_session_dirs(directory: &Path) -> Vec<(u64, PathBuf)> {
    let Ok(cwd_entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    // Keep only the newest bounded set while traversing. A user's Grok
    // history can contain tens of thousands of sessions; collecting every
    // path before sorting makes memory grow with that historical count even
    // though only the newest 128 can affect the sidebar.
    let mut newest = BinaryHeap::with_capacity(MAX_LOCAL_SESSIONS);
    for cwd_entry in cwd_entries.flatten() {
        let Ok(cwd_type) = cwd_entry.file_type() else {
            continue;
        };
        if !cwd_type.is_dir() {
            continue;
        }
        let Ok(session_entries) = fs::read_dir(cwd_entry.path()) else {
            continue;
        };
        for session_entry in session_entries.flatten() {
            let Ok(session_type) = session_entry.file_type() else {
                continue;
            };
            if !session_type.is_dir() {
                continue;
            }
            let session_dir = session_entry.path();
            let Some(modified) = session_dir_mtime(&session_dir) else {
                continue;
            };
            newest.push(Reverse((modified, session_dir)));
            if newest.len() > MAX_LOCAL_SESSIONS {
                newest.pop();
            }
        }
    }
    let mut output = newest
        .into_iter()
        .map(|Reverse(candidate)| candidate)
        .collect::<Vec<_>>();
    output.sort_by_key(|candidate| Reverse(candidate.0));
    output
}

/// Grok can register two ids for one process in `active_sessions.json`: a
/// session-start stub that Herdr often binds, and the conversation that
/// actually writes `signals.json`. Same PID is the only join we trust.
struct ActiveSessionRecord {
    session_id: String,
    pid: u64,
}

fn read_active_sessions(home: &Path) -> Vec<ActiveSessionRecord> {
    let Ok(value) = read_json(&home.join("active_sessions.json")) else {
        return Vec::new();
    };
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let session_id = entry
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())?
                .to_string();
            let pid = entry.get("pid").and_then(Value::as_u64)?;
            Some(ActiveSessionRecord { session_id, pid })
        })
        .collect()
}

fn expand_session_ids_with_active_siblings(
    active: &[ActiveSessionRecord],
    session_ids: &[String],
) -> Vec<String> {
    if session_ids.is_empty() {
        return Vec::new();
    }
    let mut output = session_ids.to_vec();
    for requested in session_ids {
        let Some(pid) = active
            .iter()
            .find(|session| session.session_id == *requested)
            .map(|session| session.pid)
        else {
            continue;
        };
        for sibling in active.iter().filter(|session| session.pid == pid) {
            if !output.iter().any(|id| id == &sibling.session_id) {
                output.push(sibling.session_id.clone());
            }
        }
    }
    output
}

fn alias_missing_diagnostics_from_active_siblings(
    snapshot: &mut ProviderSnapshot,
    active: &[ActiveSessionRecord],
    session_ids: &[String],
) {
    for requested in session_ids {
        let needs_context = !snapshot.session_contexts.contains_key(requested);
        let needs_model = !snapshot.session_models.contains_key(requested);
        if !needs_context && !needs_model {
            continue;
        }
        let Some(pid) = active
            .iter()
            .find(|session| session.session_id == *requested)
            .map(|session| session.pid)
        else {
            continue;
        };
        let donors: Vec<&str> = active
            .iter()
            .filter(|session| session.pid == pid && session.session_id != *requested)
            .map(|session| session.session_id.as_str())
            .collect();
        if needs_context {
            if let Some(context) = donors
                .iter()
                .find_map(|id| snapshot.session_contexts.get(*id))
                .cloned()
            {
                snapshot.session_contexts.insert(requested.clone(), context);
            }
        }
        if needs_model {
            if let Some(model) = donors
                .iter()
                .find_map(|id| snapshot.session_models.get(*id))
                .cloned()
            {
                snapshot.session_models.insert(requested.clone(), model);
            }
        }
    }
}

/// With pane ids available, check only the expected session paths. This is
/// the hot path used by the active-turn watcher and avoids scanning unrelated
/// historical sessions altogether. A matching directory is enough: Grok may
/// not have written `signals.json` yet.
fn collect_matching_session_dirs(directory: &Path, session_ids: &[String]) -> Vec<(u64, PathBuf)> {
    let Ok(cwd_entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut output = Vec::new();
    for cwd_entry in cwd_entries.flatten() {
        let Ok(cwd_type) = cwd_entry.file_type() else {
            continue;
        };
        if !cwd_type.is_dir() {
            continue;
        }
        for session_id in session_ids {
            let session_dir = cwd_entry.path().join(session_id);
            if !session_dir.is_dir() {
                continue;
            }
            let Some(modified) = session_dir_mtime(&session_dir) else {
                continue;
            };
            output.push((modified, session_dir));
        }
    }
    output
}

fn session_dir_mtime(session_dir: &Path) -> Option<u64> {
    CacheStore::file_mtime_unix(&session_dir.join("signals.json"))
        .or_else(|| CacheStore::file_mtime_unix(&session_dir.join("summary.json")))
        .or_else(|| CacheStore::file_mtime_unix(session_dir))
}

struct LocalSessionObservation {
    model: Option<String>,
    context: Option<ContextUsage>,
}

fn observe_session_dir(session_dir: &Path, session_id: &str) -> Option<LocalSessionObservation> {
    let updates = read_jsonl_tail(&session_dir.join("updates.jsonl"));
    let mut observation = read_json(&session_dir.join("signals.json"))
        .ok()
        .and_then(|signals| parse_local_session(&signals, updates.as_deref(), session_id));
    if observation
        .as_ref()
        .is_none_or(|observation| observation.model.is_none())
    {
        if let Some(model) = read_json(&session_dir.join("summary.json"))
            .ok()
            .as_ref()
            .and_then(parse_summary_model)
        {
            match &mut observation {
                Some(observation) => observation.model = Some(model),
                None => {
                    observation = Some(LocalSessionObservation {
                        model: Some(model),
                        context: None,
                    });
                }
            }
        }
    }
    observation.filter(|observation| observation.model.is_some() || observation.context.is_some())
}

fn parse_summary_model(summary: &Value) -> Option<String> {
    summary
        .get("current_model_id")
        .or_else(|| summary.get("currentModelId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
}

fn parse_local_session(
    signals: &Value,
    updates: Option<&str>,
    session_id: &str,
) -> Option<LocalSessionObservation> {
    let model = signals
        .get("primaryModelId")
        .or_else(|| signals.get("primary_model_id"))
        .and_then(Value::as_str)
        .or_else(|| {
            signals
                .get("modelsUsed")
                .or_else(|| signals.get("models_used"))
                .and_then(Value::as_array)
                .and_then(|models| models.iter().rev().find_map(Value::as_str))
        })
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string);

    let used = signals
        .get("contextWindowUsage")
        .or_else(|| signals.get("context_window_usage"))
        .and_then(Value::as_f64)
        .or_else(|| {
            let used = signals
                .get("contextTokensUsed")
                .or_else(|| signals.get("context_tokens_used"))
                .and_then(Value::as_f64)?;
            let capacity = signals
                .get("contextWindowTokens")
                .or_else(|| signals.get("context_window_tokens"))
                .and_then(Value::as_f64)?;
            (capacity > 0.0).then_some(used / capacity * 100.0)
        });
    let cache =
        parse_update_cache(updates, session_id).or_else(|| parse_signal_cache(signals, session_id));
    let context = used
        .and_then(|used| ContextUsage::new(used.clamp(0.0, 100.0)).ok())
        .map(|context| context.with_cache(cache));
    (model.is_some() || context.is_some()).then_some(LocalSessionObservation { model, context })
}

fn parse_signal_cache(signals: &Value, session_id: &str) -> Option<CacheUsage> {
    let object = signals.as_object()?;
    let input = token_count(object, "inputTokens", "input_tokens");
    let read = token_count(object, "cachedReadTokens", "cached_read_tokens");
    let creation = token_count(object, "cacheCreationTokens", "cache_creation_tokens");
    cache_usage(input, read, creation, session_id)
}

fn parse_update_cache(updates: Option<&str>, session_id: &str) -> Option<CacheUsage> {
    let mut latest = None;
    for line in updates?.lines().rev() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(usage) = value
            .pointer("/params/update/usage")
            .or_else(|| value.pointer("/update/usage"))
            .or_else(|| value.pointer("/usage"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        let input = token_count(usage, "inputTokens", "input_tokens");
        let read = token_count(usage, "cachedReadTokens", "cached_read_tokens");
        let creation = token_count(usage, "cacheCreationTokens", "cache_creation_tokens");
        if let Some(cache) = cache_usage(input, read, creation, session_id) {
            latest = Some(cache);
            break;
        }
    }
    latest
}

fn cache_usage(input: u64, read: u64, creation: u64, session_id: &str) -> Option<CacheUsage> {
    let cache = CacheUsage::from_token_counts(input.saturating_sub(read), read, creation)?;
    let totals = CacheTotals::from_token_counts(
        cache.fresh_input_tokens,
        cache.read_tokens,
        cache.creation_tokens,
    );
    Some(cache.with_session_totals(totals, session_id, 0))
}

fn token_count(object: &serde_json::Map<String, Value>, camel: &str, snake: &str) -> u64 {
    object
        .get(camel)
        .or_else(|| object.get(snake))
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

fn read_json(path: &Path) -> Result<Value> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

fn read_jsonl_tail(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(GROK_SESSION_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::with_capacity((length - start) as usize);
    file.read_to_end(&mut bytes).ok()?;
    let tail = String::from_utf8_lossy(&bytes);
    if start == 0 {
        return Some(tail.into_owned());
    }
    tail.split_once('\n').map(|(_, lines)| lines.to_string())
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
    fn reads_a_monthly_period_as_thirty_days_never_as_a_week() {
        let value = json!({
            "config": {
                "creditUsagePercent": 42.5,
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_MONTHLY",
                    "end": "2026-09-15T00:00:00Z"
                }
            }
        });
        let snapshot = parse_billing_response(&value, 1).unwrap();
        assert!(snapshot.window(WindowKind::Weekly).is_none());
        let monthly = snapshot.window(WindowKind::Monthly).unwrap();
        assert_eq!(monthly.remaining_percent, 57.5);
        assert_eq!(
            monthly.resets_at,
            Some(ResetAt::from_unix_seconds(1_789_430_400))
        );
    }

    #[test]
    fn rejects_a_period_the_contract_does_not_name() {
        for period_type in ["USAGE_PERIOD_TYPE_DAILY", "UNSPECIFIED", ""] {
            let value = json!({
                "config": {
                    "creditUsagePercent": 42.5,
                    "currentPeriod": {"type": period_type}
                }
            });
            assert!(
                parse_billing_response(&value, 1).is_err(),
                "accepted {period_type}"
            );
        }
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

    #[test]
    fn parses_local_session_context_model_and_cache() {
        let signals = json!({
            "contextWindowUsage": 16,
            "contextTokensUsed": 80_000,
            "contextWindowTokens": 500_000,
            "primaryModelId": "grok-4.6"
        });
        let updates = r#"{"method":"_x.ai/session/update","params":{"update":{"usage":{"inputTokens":1000,"cachedReadTokens":800,"cacheCreationTokens":100}}}}
"#;
        let observation = parse_local_session(&signals, Some(updates), "session-1").unwrap();
        assert_eq!(observation.model.as_deref(), Some("grok-4.6"));
        assert_eq!(observation.context.as_ref().unwrap().used_percent, 16.0);
        let cache = observation.context.unwrap().cache.unwrap();
        assert_eq!(cache.fresh_input_tokens, 200);
        assert_eq!(cache.read_tokens, 800);
        assert_eq!(cache.creation_tokens, 100);
        assert_eq!(cache.session_id.as_deref(), Some("session-1"));
    }

    #[test]
    fn scans_session_files_without_broadcasting_unknown_data() {
        let directory = tempfile::tempdir().unwrap();
        let session_dir = directory.path().join("sessions/cwd/session-1");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            session_dir.join("signals.json"),
            r#"{"contextWindowUsage":25,"primaryModelId":"grok-4.5"}"#,
        )
        .unwrap();
        fs::write(
            session_dir.join("updates.jsonl"),
            r#"{"params":{"update":{"usage":{"inputTokens":10,"cachedReadTokens":5}}}}"#,
        )
        .unwrap();
        let mut snapshot = ProviderSnapshot::new(Provider::Grok, vec![], 1);
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["session-1".to_string()]);
        assert_eq!(snapshot.session_models["session-1"], "grok-4.5");
        assert!(snapshot.context_for_session(Some("session-1")).is_some());
        assert!(snapshot.context_for_session(Some("session-2")).is_none());
    }

    #[test]
    fn matching_session_scan_checks_only_the_expected_two_level_layout() {
        let directory = tempfile::tempdir().unwrap();
        let matching = directory.path().join("sessions/cwd/session-1");
        let unrelated_nested = directory.path().join("sessions/old/nested/session-1");
        fs::create_dir_all(&matching).unwrap();
        fs::create_dir_all(&unrelated_nested).unwrap();
        fs::write(matching.join("signals.json"), "{}").unwrap();
        fs::write(unrelated_nested.join("signals.json"), "{}").unwrap();

        let files = collect_matching_session_dirs(
            &directory.path().join("sessions"),
            &["session-1".to_string()],
        );
        assert_eq!(files.len(), 1);
        assert!(files[0].1.ends_with("sessions/cwd/session-1"));
    }

    #[test]
    fn matching_session_without_signals_still_reads_summary_model() {
        let directory = tempfile::tempdir().unwrap();
        let session_dir = directory.path().join("sessions/cwd/session-1");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            session_dir.join("summary.json"),
            r#"{"current_model_id":"grok-4.6"}"#,
        )
        .unwrap();
        let mut snapshot = ProviderSnapshot::new(Provider::Grok, vec![], 1);
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["session-1".to_string()]);
        assert_eq!(snapshot.session_models["session-1"], "grok-4.6");
        assert!(snapshot.context_for_session(Some("session-1")).is_none());
    }

    #[test]
    fn same_pid_active_sibling_fills_missing_context_for_the_bound_stub() {
        let directory = tempfile::tempdir().unwrap();
        let stub = directory.path().join("sessions/cwd/stub");
        let real = directory.path().join("sessions/cwd/real");
        fs::create_dir_all(&stub).unwrap();
        fs::create_dir_all(&real).unwrap();
        fs::write(
            stub.join("summary.json"),
            r#"{"current_model_id":"grok-4.6"}"#,
        )
        .unwrap();
        fs::write(
            real.join("signals.json"),
            r#"{"contextWindowUsage":79,"primaryModelId":"grok-4.6"}"#,
        )
        .unwrap();
        fs::write(
            real.join("updates.jsonl"),
            r#"{"params":{"update":{"usage":{"inputTokens":1000,"cachedReadTokens":800,"cacheCreationTokens":100}}}}"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("active_sessions.json"),
            r#"[{"session_id":"stub","pid":76526},{"session_id":"real","pid":76526}]"#,
        )
        .unwrap();

        let mut snapshot = ProviderSnapshot::new(Provider::Grok, vec![], 1);
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["stub".to_string()]);
        let context = snapshot.context_for_session(Some("stub")).unwrap();
        assert_eq!(context.used_percent, 79.0);
        let cache = context.cache.as_ref().unwrap();
        assert_eq!(cache.read_tokens, 800);
        assert_eq!(snapshot.session_models["stub"], "grok-4.6");
    }

    #[test]
    fn different_pid_active_session_does_not_fill_context() {
        let directory = tempfile::tempdir().unwrap();
        let stub = directory.path().join("sessions/cwd/stub");
        let other = directory.path().join("sessions/cwd/other");
        fs::create_dir_all(&stub).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::write(
            stub.join("summary.json"),
            r#"{"current_model_id":"grok-4.6"}"#,
        )
        .unwrap();
        fs::write(
            other.join("signals.json"),
            r#"{"contextWindowUsage":79,"primaryModelId":"grok-4.6"}"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("active_sessions.json"),
            r#"[{"session_id":"stub","pid":1},{"session_id":"other","pid":2}]"#,
        )
        .unwrap();

        let mut snapshot = ProviderSnapshot::new(Provider::Grok, vec![], 1);
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["stub".to_string()]);
        assert!(snapshot.context_for_session(Some("stub")).is_none());
        assert_eq!(snapshot.session_models["stub"], "grok-4.6");
    }

    #[test]
    fn own_context_is_not_replaced_by_a_same_pid_sibling() {
        let directory = tempfile::tempdir().unwrap();
        let bound = directory.path().join("sessions/cwd/bound");
        let sibling = directory.path().join("sessions/cwd/sibling");
        fs::create_dir_all(&bound).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        fs::write(
            bound.join("signals.json"),
            r#"{"contextWindowUsage":12,"primaryModelId":"grok-4.6"}"#,
        )
        .unwrap();
        fs::write(
            sibling.join("signals.json"),
            r#"{"contextWindowUsage":79,"primaryModelId":"grok-4.5"}"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("active_sessions.json"),
            r#"[{"session_id":"bound","pid":9},{"session_id":"sibling","pid":9}]"#,
        )
        .unwrap();

        let mut snapshot = ProviderSnapshot::new(Provider::Grok, vec![], 1);
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["bound".to_string()]);
        assert_eq!(
            snapshot
                .context_for_session(Some("bound"))
                .unwrap()
                .used_percent,
            12.0
        );
        assert_eq!(snapshot.session_models["bound"], "grok-4.6");
    }

    #[test]
    fn recent_session_scan_keeps_only_a_bounded_newest_set() {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..(MAX_LOCAL_SESSIONS + 8) {
            let session_dir = directory
                .path()
                .join("sessions/cwd")
                .join(format!("session-{index}"));
            fs::create_dir_all(&session_dir).unwrap();
            fs::write(session_dir.join("signals.json"), "{}").unwrap();
        }

        let files = collect_recent_session_dirs(&directory.path().join("sessions"));
        assert_eq!(files.len(), MAX_LOCAL_SESSIONS);
    }
}
