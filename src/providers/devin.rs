//! Devin CLI subscription quota, read from Cognition's Connect RPC HTTP API.
//!
//! Devin CLI stores credentials at `~/.local/share/devin/credentials.toml`
//! (or `$XDG_DATA_HOME/devin/credentials.toml`). The quota endpoint is the
//! same Connect RPC call the CLI uses: `POST {api_server_url}/exa.seat_management_pb
//! .SeatManagementService/GetUserStatus` with a JSON body carrying the
//! `windsurf_api_key` in a metadata block. That is the Grok/OpenCode Go
//! pattern — local credential plus the official CLI contract — not a private
//! web scrape.
//!
//! Everything here fails closed. A missing, malformed, or unrecognized field
//! yields no window rather than a guessed number, and a provider with no
//! report is "unknown", never "0% used". The API key is never logged, printed,
//! or included in error messages or pane metadata. Cache identity is
//! `sha256("devin\0" || key)` so a credential swap cannot keep the previous
//! account's last-good snapshot.
//!
//! Model comes from Devin's own files, not from the quota API:
//!
//! - `~/.config/devin/config.json` `agent.model` is the CLI default / current
//!   configured model (Issue #53). New sessions that never run `/model` use
//!   this value. It is published as `snapshot.model` and shown on every Devin
//!   pane.
//! - `devin-models.json` maps that id to a display label. Missing or malformed
//!   catalog leaves the raw id; quota fetch is unaffected.
//! - `planInfo.planName` is the subscription plan, not a model.

use crate::cache::CacheStore;
use crate::model::{Provider, ProviderSnapshot, ResetAt, UsageWindow, WindowKind};
use crate::providers::ProviderError;
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_API_SERVER_URL: &str = "https://server.codeium.com";
const GET_USER_STATUS_PATH: &str = "/exa.seat_management_pb.SeatManagementService/GetUserStatus";
/// Same bound OpenCode uses for its local models catalog. A huge file is
/// treated as absent so a corrupt dump cannot stall a quota refresh.
const MAX_MODELS_BYTES: u64 = 8 * 1024 * 1024;

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
#[derive(Clone, Deserialize)]
struct DevinCredentials {
    #[serde(rename = "windsurf_api_key")]
    windsurf_api_key: String,
    #[serde(rename = "api_server_url", default)]
    api_server_url: Option<String>,
}

impl std::fmt::Debug for DevinCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevinCredentials")
            .field("windsurf_api_key", &"[redacted]")
            .field("api_server_url", &self.api_server_url)
            .finish()
    }
}

/// Fetch Devin CLI's quota. The model is the CLI `config.json` default, shared
/// across panes, so it is published as `snapshot.model`.
pub fn fetch_for_sessions(_session_ids: &[String]) -> Result<ProviderSnapshot> {
    let path = auth_path().context("resolve Devin credentials path")?;
    let credentials = read_credentials(&path).map_err(anyhow::Error::from)?;
    let account_id = account_pin(&credentials.windsurf_api_key);
    let configured_model = configured_model_from_config();
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
    let mut snapshot =
        parse_user_status(&value, CacheStore::now_unix()).map_err(anyhow::Error::from)?;
    apply_configured_model(
        &mut snapshot,
        configured_model.as_deref(),
        load_models_catalog().as_ref(),
    );
    Ok(snapshot.with_account_id(Some(account_id)))
}

/// Issue #53: `config.json` `agent.model` is Devin's model. Map it through the
/// local catalog when possible, then store it on `snapshot.model`.
fn apply_configured_model(
    snapshot: &mut ProviderSnapshot,
    configured_model: Option<&str>,
    catalog: Option<&Value>,
) {
    let Some(model_id) = configured_model.map(str::trim).filter(|id| !id.is_empty()) else {
        return;
    };
    snapshot.model = Some(display_name_for_model(model_id, catalog));
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

/// Stable cache identity for the signed-in Devin key. The raw key never
/// enters the snapshot; a different key is a different account.
pub fn current_account_id() -> Option<String> {
    let path = auth_path().ok()?;
    let credentials = read_credentials(&path).ok()?;
    Some(account_pin(&credentials.windsurf_api_key))
}

pub fn auth_mtime_unix() -> Option<u64> {
    CacheStore::file_mtime_unix(&auth_path().ok()?)
}

/// `sha256("devin\0" || trimmed key)`. Pinned in tests so a hash change is a
/// test failure rather than a silent last-good leak across accounts.
fn account_pin(api_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"devin\0");
    hasher.update(api_key.trim().as_bytes());
    format!("{:x}", hasher.finalize())
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

/// Read `agent.model` from `~/.config/devin/config.json` (or `$XDG_CONFIG_HOME`).
/// Missing, commented-unparseable, or empty values yield `None`.
fn configured_model_from_config() -> Option<String> {
    configured_model_from_path(&devin_config_dir().ok()?.join("config.json"))
}

fn configured_model_from_path(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    configured_model_from_text(&text)
}

fn configured_model_from_text(text: &str) -> Option<String> {
    let value = parse_jsonc_value(text)?;
    let model = value
        .get("agent")
        .and_then(|agent| agent.get("model"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())?;
    Some(model.to_string())
}

/// Local `devin-models.json` catalog from Issue #53. First readable file wins:
/// `DEVIN_MODELS_FILE`, then the Devin config dir, data dir, and `data/cli`.
fn load_models_catalog() -> Option<Value> {
    for path in models_catalog_candidates() {
        if let Some(catalog) = load_models_catalog_from_path(&path) {
            return Some(catalog);
        }
    }
    None
}

fn models_catalog_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = std::env::var_os("DEVIN_MODELS_FILE") {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            paths.push(path);
        }
    }
    if let Ok(dir) = devin_config_dir() {
        paths.push(dir.join("devin-models.json"));
    }
    if let Ok(dir) = devin_data_dir() {
        paths.push(dir.join("devin-models.json"));
        paths.push(dir.join("cli").join("devin-models.json"));
    }
    paths
}

fn load_models_catalog_from_path(path: &Path) -> Option<Value> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_MODELS_BYTES {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let value = parse_jsonc_value(&text)?;
    value.get("families")?.as_array()?;
    Some(value)
}

fn display_name_for_model(model_id: &str, catalog: Option<&Value>) -> String {
    let trimmed = model_id.trim();
    catalog
        .and_then(|catalog| lookup_model_label(catalog, trimmed))
        .unwrap_or_else(|| trimmed.to_string())
}

fn lookup_model_label(catalog: &Value, model_id: &str) -> Option<String> {
    let families = catalog.get("families")?.as_array()?;
    variant_label(families, model_id, true)
        .or_else(|| variant_label(families, model_id, false))
        .or_else(|| family_label(families, model_id, true))
        .or_else(|| family_label(families, model_id, false))
}

fn json_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn variant_label(families: &[Value], model_id: &str, exact: bool) -> Option<String> {
    for family in families {
        let Some(variants) = family.get("variants").and_then(Value::as_array) else {
            continue;
        };
        for variant in variants {
            let Some(label) = json_str(variant.get("label")) else {
                continue;
            };
            if eq_model_key(json_str(variant.get("model_uid")), model_id, exact)
                || eq_model_key(Some(label), model_id, exact)
            {
                return Some(label.to_string());
            }
        }
    }
    None
}

fn family_label(families: &[Value], model_id: &str, exact: bool) -> Option<String> {
    for family in families {
        let mut aliases = family
            .get("aliases")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|value| json_str(Some(value)));
        let hit = eq_model_key(json_str(family.get("slug")), model_id, exact)
            || eq_model_key(json_str(family.get("family_uid")), model_id, exact)
            || aliases.any(|alias| eq_model_key(Some(alias), model_id, exact));
        if hit {
            if let Some(label) = json_str(family.get("family_label")) {
                return Some(label.to_string());
            }
        }
    }
    None
}

fn eq_model_key(candidate: Option<&str>, model_id: &str, exact: bool) -> bool {
    let Some(candidate) = candidate.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    if exact {
        candidate == model_id
    } else {
        candidate.eq_ignore_ascii_case(model_id)
    }
}

/// Devin config files are JSON with `//` and `/* */` comments. Try strict
/// JSON first; strip comments only when that fails. Strings are left intact.
fn parse_jsonc_value(text: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str(text) {
        return Some(value);
    }
    serde_json::from_str(&strip_json_comments(text)).ok()
}

fn strip_json_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }
        if c == '/' {
            match chars.peek() {
                Some('/') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                }
                Some('*') => {
                    chars.next();
                    let mut prev = '\0';
                    for next in chars.by_ref() {
                        if prev == '*' && next == '/' {
                            break;
                        }
                        prev = next;
                    }
                }
                _ => out.push(c),
            }
            continue;
        }
        out.push(c);
    }
    out
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
/// rather than a guessed number. `planInfo.planName` is the subscription
/// plan, not a model, and is ignored here.
pub fn parse_user_status(
    value: &Value,
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

    Ok(ProviderSnapshot::new(Provider::Devin, windows, now))
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

    fn catalog_fixture() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/devin/devin-models.json"))
            .expect("catalog fixture is valid JSON")
    }

    #[test]
    fn pro_fixture_flips_remaining_to_used() {
        let snapshot = parse_user_status(&pro_fixture(), 1).expect("snapshot");
        assert_eq!(snapshot.provider, Provider::Devin);
        assert_eq!(snapshot.model, None);

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
        let snapshot = parse_user_status(&value, 1).expect("snapshot");
        assert!(snapshot.window(WindowKind::FiveHour).is_none());
        let weekly = snapshot.window(WindowKind::Weekly).expect("weekly");
        assert_eq!(weekly.used_percent, 50.0);
    }

    #[test]
    fn plan_name_is_not_used_as_model() {
        let snapshot = parse_user_status(&pro_fixture(), 1).expect("snapshot");
        assert_eq!(snapshot.model, None);
        assert_ne!(snapshot.model.as_deref(), Some("Pro"));
    }

    #[test]
    fn valid_config_model_is_the_configured_default() {
        assert_eq!(
            configured_model_from_text(r#"{"agent":{"model":"swe-1-7-medium"}}"#).as_deref(),
            Some("swe-1-7-medium")
        );
    }

    #[test]
    fn missing_config_file_yields_no_model() {
        let dir = tempdir().unwrap();
        assert_eq!(
            configured_model_from_path(&dir.path().join("config.json")),
            None
        );
    }

    #[test]
    fn malformed_config_yields_no_model() {
        assert_eq!(configured_model_from_text("{not json"), None);
        assert_eq!(configured_model_from_text("[]"), None);
        assert_eq!(configured_model_from_text(r#"{"agent":"nope"}"#), None);
        assert_eq!(configured_model_from_text(r#"{"agent":{"model":1}}"#), None);
    }

    #[test]
    fn empty_config_model_yields_no_model() {
        assert_eq!(
            configured_model_from_text(r#"{"agent":{"model":""}}"#),
            None
        );
        assert_eq!(
            configured_model_from_text(r#"{"agent":{"model":"   "}}"#),
            None
        );
        assert_eq!(configured_model_from_text(r#"{"agent":{}}"#), None);
        assert_eq!(configured_model_from_text("{}"), None);
    }

    #[test]
    fn commented_config_json_still_reads_the_default_model() {
        let text = r#"
        {
          // user-wide default, not the session /model
          "agent": {
            "model": "swe-1-7-medium" // Default AI model
          }
        }
        "#;
        assert_eq!(
            configured_model_from_text(text).as_deref(),
            Some("swe-1-7-medium")
        );
    }

    #[test]
    fn catalog_maps_model_uid_to_variant_label() {
        let catalog = catalog_fixture();
        assert_eq!(
            display_name_for_model("swe-1-7-medium", Some(&catalog)),
            "SWE-1.7 Medium"
        );
    }

    #[test]
    fn catalog_maps_family_slug_to_family_label_not_a_variant() {
        let catalog = catalog_fixture();
        assert_eq!(display_name_for_model("swe", Some(&catalog)), "SWE-1.7");
        assert_eq!(
            display_name_for_model("opus", Some(&catalog)),
            "Claude Opus"
        );
    }

    #[test]
    fn missing_catalog_keeps_the_raw_id() {
        assert_eq!(
            display_name_for_model("swe-1-7-medium", None),
            "swe-1-7-medium"
        );
    }

    #[test]
    fn malformed_catalog_file_is_skipped() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("devin-models.json");
        fs::write(&path, "{not json").unwrap();
        assert!(load_models_catalog_from_path(&path).is_none());
        fs::write(&path, r#"{"not":"families"}"#).unwrap();
        assert!(load_models_catalog_from_path(&path).is_none());
    }

    #[test]
    fn unknown_model_id_falls_back_to_raw_id() {
        let catalog = catalog_fixture();
        assert_eq!(
            display_name_for_model("not-a-real-model", Some(&catalog)),
            "not-a-real-model"
        );
    }

    #[test]
    fn configured_model_is_not_written_as_session_model() {
        let mut snapshot = parse_user_status(&pro_fixture(), 1).expect("snapshot");
        apply_configured_model(
            &mut snapshot,
            Some("swe-1-7-medium"),
            Some(&catalog_fixture()),
        );
        assert_eq!(snapshot.model.as_deref(), Some("SWE-1.7 Medium"));
        assert!(snapshot.session_models.is_empty());
    }

    #[test]
    fn configured_model_without_catalog_stays_raw() {
        let mut snapshot = parse_user_status(&pro_fixture(), 1).expect("snapshot");
        apply_configured_model(&mut snapshot, Some("swe-1-7-medium"), None);
        assert_eq!(snapshot.model.as_deref(), Some("swe-1-7-medium"));
        assert!(snapshot.session_models.is_empty());
    }

    #[test]
    fn applying_no_configured_model_leaves_plan_name_out() {
        let mut snapshot = parse_user_status(&pro_fixture(), 1).expect("snapshot");
        apply_configured_model(&mut snapshot, None, None);
        assert_eq!(snapshot.model, None);
        assert!(snapshot.session_models.is_empty());
    }

    #[test]
    fn comment_markers_inside_strings_are_kept() {
        assert_eq!(
            configured_model_from_text(r#"{"agent":{"model":"foo//bar"}}"#).as_deref(),
            Some("foo//bar")
        );
    }

    #[test]
    fn oversized_catalog_file_is_skipped() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("devin-models.json");
        let mut bytes = br#"{"families":[]}"#.to_vec();
        bytes.resize(MAX_MODELS_BYTES as usize + 1, b' ');
        fs::write(&path, bytes).unwrap();
        assert!(load_models_catalog_from_path(&path).is_none());
    }

    #[test]
    fn catalog_file_override_loads_from_devin_models_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("devin-models.json");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/devin/devin-models.json"),
            &path,
        )
        .unwrap();
        std::env::set_var("DEVIN_MODELS_FILE", &path);
        let catalog = load_models_catalog();
        std::env::remove_var("DEVIN_MODELS_FILE");
        assert_eq!(
            display_name_for_model("swe-1-7-medium", catalog.as_ref()),
            "SWE-1.7 Medium"
        );
    }

    #[test]
    fn missing_all_windows_is_an_error() {
        let value = json!({"userStatus": {"planStatus": {}}});
        assert!(parse_user_status(&value, 1).is_err());
    }

    #[test]
    fn missing_plan_status_is_an_error() {
        let value = json!({"userStatus": {}});
        assert!(parse_user_status(&value, 1).is_err());
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
        assert!(parse_user_status(&value, 1).is_err());
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
        assert!(parse_user_status(&value, 1).is_err());
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

    /// The digest is the cache contract. Changing it orphans every last-good
    /// Devin snapshot on the next credential read, so it is pinned here.
    #[test]
    fn account_pin_matches_the_devin_key_digest() {
        let pin = account_pin("secret-key");
        let mut expected = Sha256::new();
        expected.update(b"devin\0secret-key");
        assert_eq!(pin, format!("{:x}", expected.finalize()));
        assert!(!pin.contains("secret"));
        assert_ne!(pin, account_pin("other-key"));
        assert_eq!(account_pin(" secret-key "), pin);
    }

    #[test]
    fn credentials_debug_redacts_the_api_key() {
        let credentials = DevinCredentials {
            windsurf_api_key: "secret-key".to_string(),
            api_server_url: None,
        };
        let rendered = format!("{credentials:?}");
        assert!(rendered.contains("[redacted]"));
        assert!(!rendered.contains("secret-key"));
    }
}
