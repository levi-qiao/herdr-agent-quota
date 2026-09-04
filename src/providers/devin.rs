//! Devin CLI subscription quota, read from Cognition's Connect RPC HTTP API.
//!
//! Devin CLI stores credentials at `~/.local/share/devin/credentials.toml`
//! (or `$XDG_DATA_HOME/devin/credentials.toml`). The quota endpoint is a
//! Connect RPC call: `POST {api_server_url}/exa.seat_management_pb
//! .SeatManagementService/GetUserStatus` with a JSON body carrying the
//! `windsurf_api_key` in a metadata block.
//!
//! Everything here fails closed. A missing, malformed, or unrecognized field
//! yields no window rather than a guessed number, and a provider with no
//! report is "unknown", never "0% used". The API key is never logged, printed,
//! or included in error messages or pane metadata.

use crate::cache::CacheStore;
use crate::model::{Provider, ProviderSnapshot, ResetAt, UsageWindow, WindowKind};
use crate::providers::ProviderError;
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_API_SERVER_URL: &str = "https://server.codeium.com";
const GET_USER_STATUS_PATH: &str = "/exa.seat_management_pb.SeatManagementService/GetUserStatus";

/// Build the full GetUserStatus URL from an optional `api_server_url`
/// override. Falls back to `DEFAULT_API_SERVER_URL` when the override is
/// absent or empty. Rejects non-`https://` schemes so the API key is never
/// sent in cleartext over the wire.
fn build_url(api_server_url: Option<&str>) -> Result<String> {
    let base = api_server_url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .unwrap_or(DEFAULT_API_SERVER_URL);
    if !base.starts_with("https://") {
        anyhow::bail!("Devin api_server_url must use https://, got: {base}");
    }
    Ok(format!("{base}{GET_USER_STATUS_PATH}"))
}

/// Credentials read from Devin CLI's `credentials.toml`.
#[derive(Debug, Clone, Deserialize)]
struct DevinCredentials {
    #[serde(rename = "windsurf_api_key")]
    windsurf_api_key: String,
    #[serde(rename = "api_server_url", default)]
    api_server_url: Option<String>,
}

/// Fetch Devin CLI's quota and return a snapshot. `session_ids` is accepted
/// for parity with the other provider fetchers but is not used yet — session
/// enrichment is deferred, matching Grok's initial implementation.
pub fn fetch_for_sessions(_session_ids: &[String]) -> Result<ProviderSnapshot> {
    let path = auth_path().context("resolve Devin credentials path")?;
    let credentials = read_credentials(&path).map_err(anyhow::Error::from)?;
    let active_model = active_model_from_config();
    let url = build_url(credentials.api_server_url.as_deref())?;
    let body = serde_json::json!({
        "metadata": {
            "apiKey": credentials.windsurf_api_key,
            "ideName": "devin",
            "ideVersion": "0.0.0",
            "extensionName": "devin",
            "extensionVersion": "0.0.0",
            "locale": "en"
        }
    });
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .timeout_write(Duration::from_secs(10))
        .build();
    let response = agent
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(|error| map_request_error(&error))?;
    let value: Value = response
        .into_json()
        .context("decode Devin GetUserStatus response")?;
    parse_user_status(&value, active_model.as_deref(), CacheStore::now_unix())
        .map_err(anyhow::Error::from)
}

/// Resolve the credentials file path.
///
/// `DEVIN_CREDENTIALS_FILE` overrides everything. Otherwise the path is
/// `$XDG_DATA_HOME/devin/credentials.toml` when `XDG_DATA_HOME` is set, or
/// `~/.local/share/devin/credentials.toml` otherwise.
pub fn auth_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("DEVIN_CREDENTIALS_FILE") {
        return Ok(PathBuf::from(path));
    }
    Ok(devin_data_dir()?.join("credentials.toml"))
}

fn devin_data_dir() -> Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        let xdg = PathBuf::from(xdg);
        if !xdg.as_os_str().is_empty() {
            return Ok(xdg.join("devin"));
        }
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/share/devin"))
}

fn devin_config_dir() -> Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let xdg = PathBuf::from(xdg);
        if !xdg.as_os_str().is_empty() {
            return Ok(xdg.join("devin"));
        }
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/devin"))
}

/// Devin CLI's `config.json`.
#[derive(Debug, Clone, Deserialize)]
struct DevinConfig {
    agent: AgentConfig,
}

#[derive(Debug, Clone, Deserialize)]
struct AgentConfig {
    model: String,
}

/// Read the active model from `~/.config/devin/config.json` (or
/// `$XDG_CONFIG_HOME/devin/config.json`). Returns `None` if the file is
/// missing or malformed so that the quota API's `planName` is used as a
/// fallback.
fn active_model_from_config() -> Option<String> {
    let path = devin_config_dir().ok()?.join("config.json");
    let text = fs::read_to_string(&path).ok()?;
    let config: DevinConfig = serde_json::from_str(&text).ok()?;
    let model = config.agent.model.trim();
    if model.is_empty() {
        return None;
    }
    Some(model.to_string())
}

fn read_credentials(path: &Path) -> std::result::Result<DevinCredentials, ProviderError> {
    let text = fs::read_to_string(path).map_err(|_| ProviderError::MissingCredentials)?;
    let credentials: DevinCredentials = toml::from_str(&text).map_err(|_| {
        ProviderError::Unavailable("Devin credentials file is not valid TOML".to_string())
    })?;
    if credentials.windsurf_api_key.trim().is_empty() {
        return Err(ProviderError::MissingCredentials);
    }
    Ok(credentials)
}

/// Map a `ureq` error to the appropriate `ProviderError`.
///
/// 401/403 means the key is invalid → `MissingCredentials`. Anything else is
/// a transport or unexpected HTTP error. The API key is never included.
fn map_request_error(error: &ureq::Error) -> ProviderError {
    match error {
        ureq::Error::Status(401, _) | ureq::Error::Status(403, _) => {
            ProviderError::MissingCredentials
        }
        ureq::Error::Status(code, _) => ProviderError::Request(format!("HTTP {code}")),
        ureq::Error::Transport(error) => ProviderError::Request(error.to_string()),
    }
}

/// Parse the `GetUserStatus` response into a snapshot.
///
/// The response carries remaining percentages for daily and weekly windows.
/// Those are flipped to used: `used = 100 - remaining`. Reset timestamps are
/// strings of Unix seconds. Missing or malformed fields yield no window
/// rather than a guessed number. `active_model` is preferred for the model
/// field; if it is `None`, the API's `planInfo.planName` is used instead.
pub fn parse_user_status(
    value: &Value,
    active_model: Option<&str>,
    now: u64,
) -> std::result::Result<ProviderSnapshot, ProviderError> {
    let plan_status = value
        .get("userStatus")
        .and_then(|status| status.get("planStatus"))
        .ok_or_else(|| {
            ProviderError::UnsupportedResponse("missing userStatus.planStatus".to_string())
        })?;

    let mut windows = Vec::new();

    if let Some(window) = parse_daily_window(plan_status)? {
        windows.push(window);
    }
    if let Some(window) = parse_weekly_window(plan_status)? {
        windows.push(window);
    }

    if windows.is_empty() {
        return Err(ProviderError::UnsupportedResponse(
            "no readable quota windows in Devin response".to_string(),
        ));
    }

    let mut snapshot = ProviderSnapshot::new(Provider::Devin, windows, now);
    snapshot.model = active_model.map(str::to_string).or_else(|| {
        plan_status
            .get("planInfo")
            .and_then(|info| info.get("planName"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    Ok(snapshot)
}

/// Daily window: remaining percent → used, string timestamp → Unix seconds.
fn parse_daily_window(
    plan_status: &Value,
) -> std::result::Result<Option<UsageWindow>, ProviderError> {
    let Some(remaining) = plan_status
        .get("dailyQuotaRemainingPercent")
        .and_then(Value::as_i64)
    else {
        return Ok(None);
    };
    let used = remaining_to_used(remaining)?;
    let reset = parse_unix_string(plan_status.get("dailyQuotaResetAtUnix"));
    let window = UsageWindow::new(WindowKind::FiveHour, used, reset)
        .map_err(|error| ProviderError::UnsupportedResponse(error.to_string()))?
        .with_source_window("1d", Some(86_400));
    Ok(Some(window))
}

/// Weekly window: remaining percent → used, string timestamp → Unix seconds.
fn parse_weekly_window(
    plan_status: &Value,
) -> std::result::Result<Option<UsageWindow>, ProviderError> {
    let Some(remaining) = plan_status
        .get("weeklyQuotaRemainingPercent")
        .and_then(Value::as_i64)
    else {
        return Ok(None);
    };
    let used = remaining_to_used(remaining)?;
    let reset = parse_unix_string(plan_status.get("weeklyQuotaResetAtUnix"));
    let window = UsageWindow::new(WindowKind::Weekly, used, reset)
        .map_err(|error| ProviderError::UnsupportedResponse(error.to_string()))?;
    Ok(Some(window))
}

/// Flip a remaining percentage to used: `used = 100 - remaining`.
/// Clamped to [0, 100] so an out-of-range upstream value is never published.
fn remaining_to_used(remaining: i64) -> std::result::Result<f64, ProviderError> {
    if !(0..=100).contains(&remaining) {
        return Err(ProviderError::UnsupportedResponse(format!(
            "Devin quota remaining percent out of range: {remaining}"
        )));
    }
    Ok((100 - remaining) as f64)
}

/// Parse a string Unix timestamp into a `ResetAt`.
fn parse_unix_string(value: Option<&Value>) -> Option<ResetAt> {
    value
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<u64>().ok())
        .map(ResetAt::from_unix_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::tempdir;

    fn pro_fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/devin/getuserstatus-pro.json"
        ))
        .expect("fixture is valid JSON")
    }

    #[test]
    fn pro_fixture_flips_remaining_to_used() {
        let snapshot = parse_user_status(&pro_fixture(), None, 1).expect("snapshot");
        assert_eq!(snapshot.provider, Provider::Devin);
        assert_eq!(snapshot.model.as_deref(), Some("Pro"));

        let daily = snapshot.window(WindowKind::FiveHour).expect("daily window");
        // 99 remaining → 1 used
        assert_eq!(daily.used_percent, 1.0);
        assert_eq!(daily.remaining_percent, 99.0);
        assert_eq!(daily.display_label(), "1d");
        assert_eq!(daily.duration_seconds, Some(86_400));
        assert_eq!(
            daily.resets_at.map(|reset| reset.unix_seconds()),
            Some(1_788_508_800)
        );

        let weekly = snapshot.window(WindowKind::Weekly).expect("weekly window");
        // 34 remaining → 66 used
        assert_eq!(weekly.used_percent, 66.0);
        assert_eq!(weekly.remaining_percent, 34.0);
        assert_eq!(
            weekly.resets_at.map(|reset| reset.unix_seconds()),
            Some(1_788_681_600)
        );
    }

    #[test]
    fn missing_daily_quota_yields_only_weekly() {
        let value = json!({
            "userStatus": {
                "planStatus": {
                    "weeklyQuotaRemainingPercent": 50,
                    "weeklyQuotaResetAtUnix": "1788681600"
                }
            }
        });
        let snapshot = parse_user_status(&value, None, 1).expect("snapshot");
        assert!(snapshot.window(WindowKind::FiveHour).is_none());
        let weekly = snapshot.window(WindowKind::Weekly).expect("weekly");
        assert_eq!(weekly.used_percent, 50.0);
    }

    #[test]
    fn active_model_overrides_plan_name() {
        let snapshot =
            parse_user_status(&pro_fixture(), Some("swe-1-7-medium"), 1).expect("snapshot");
        assert_eq!(snapshot.model.as_deref(), Some("swe-1-7-medium"));
    }

    #[test]
    fn plan_name_is_used_when_active_model_is_absent() {
        let snapshot = parse_user_status(&pro_fixture(), None, 1).expect("snapshot");
        assert_eq!(snapshot.model.as_deref(), Some("Pro"));
    }

    #[test]
    fn missing_all_windows_is_an_error() {
        let value = json!({"userStatus": {"planStatus": {}}});
        assert!(parse_user_status(&value, None, 1).is_err());
    }

    #[test]
    fn missing_plan_status_is_an_error() {
        let value = json!({"userStatus": {}});
        assert!(parse_user_status(&value, None, 1).is_err());
    }

    #[test]
    fn out_of_range_remaining_is_an_error() {
        let value = json!({
            "userStatus": {
                "planStatus": {
                    "dailyQuotaRemainingPercent": 150,
                    "dailyQuotaResetAtUnix": "1788508800"
                }
            }
        });
        assert!(parse_user_status(&value, None, 1).is_err());
    }

    #[test]
    fn credentials_path_resolves_via_env_override() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("creds.toml");
        fs::write(&file, "windsurf_api_key = \"test\"\n").unwrap();
        std::env::set_var("DEVIN_CREDENTIALS_FILE", &file);
        let resolved = auth_path().expect("path");
        std::env::remove_var("DEVIN_CREDENTIALS_FILE");
        assert_eq!(resolved, file);
    }

    #[test]
    fn credentials_path_falls_back_to_xdg_data_home() {
        let dir = tempdir().unwrap();
        std::env::set_var("XDG_DATA_HOME", dir.path());
        std::env::remove_var("DEVIN_CREDENTIALS_FILE");
        let resolved = auth_path().expect("path");
        std::env::remove_var("XDG_DATA_HOME");
        assert_eq!(resolved, dir.path().join("devin/credentials.toml"));
    }

    #[test]
    fn credentials_path_falls_back_to_home() {
        std::env::remove_var("XDG_DATA_HOME");
        std::env::remove_var("DEVIN_CREDENTIALS_FILE");
        let home = std::env::var_os("HOME").expect("HOME");
        let resolved = auth_path().expect("path");
        assert_eq!(
            resolved,
            PathBuf::from(home).join(".local/share/devin/credentials.toml")
        );
    }

    #[test]
    fn empty_api_key_is_missing_credentials() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("creds.toml");
        fs::write(&file, "windsurf_api_key = \"\"\n").unwrap();
        assert!(matches!(
            read_credentials(&file),
            Err(ProviderError::MissingCredentials)
        ));
    }

    #[test]
    fn missing_credentials_file_is_missing_credentials() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("nonexistent.toml");
        assert!(matches!(
            read_credentials(&file),
            Err(ProviderError::MissingCredentials)
        ));
    }

    #[test]
    fn invalid_toml_is_unavailable() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("creds.toml");
        fs::write(&file, "this is not toml {{{\n").unwrap();
        assert!(matches!(
            read_credentials(&file),
            Err(ProviderError::Unavailable(_))
        ));
    }

    #[test]
    fn missing_windsurf_api_key_field_is_unavailable() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("creds.toml");
        fs::write(&file, "api_server_url = \"https://example.com\"\n").unwrap();
        assert!(matches!(
            read_credentials(&file),
            Err(ProviderError::Unavailable(_))
        ));
    }

    #[test]
    fn negative_remaining_is_an_error() {
        let value = json!({
            "userStatus": {
                "planStatus": {
                    "dailyQuotaRemainingPercent": -1,
                    "dailyQuotaResetAtUnix": "1788508800"
                }
            }
        });
        assert!(parse_user_status(&value, None, 1).is_err());
    }

    #[test]
    fn build_url_uses_default_when_override_is_absent() {
        let url = build_url(None).expect("default url");
        assert!(url.starts_with("https://server.codeium.com"));
        assert!(url.ends_with(GET_USER_STATUS_PATH));
    }

    #[test]
    fn build_url_uses_override_when_provided() {
        let url = build_url(Some("https://enterprise.example.com")).expect("override url");
        assert!(url.starts_with("https://enterprise.example.com"));
        assert!(url.ends_with(GET_USER_STATUS_PATH));
    }

    #[test]
    fn build_url_ignores_empty_override() {
        let url = build_url(Some("  ")).expect("falls back to default");
        assert!(url.starts_with("https://server.codeium.com"));
    }

    #[test]
    fn build_url_rejects_non_https_scheme() {
        assert!(build_url(Some("http://insecure.example.com")).is_err());
        assert!(build_url(Some("ftp://example.com")).is_err());
    }
}
