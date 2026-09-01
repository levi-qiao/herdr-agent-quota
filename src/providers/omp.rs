//! omp subscription usage, read from omp's own usage layer.
//!
//! `omp usage --json` reports every authenticated account's normalized limits.
//! It answers from the durable usage cache omp keeps in `agent.db` (five
//! minutes, plus a last-good fallback), so calling it is not the same as
//! calling the provider: omp added that cache precisely because Anthropic and
//! OpenAI rate-limit their usage endpoints per IP.
//!
//! Everything here fails closed. A missing, malformed, or unrecognized field
//! yields no window rather than a guessed number, and a provider with no
//! report is "unknown", never "0% used".

use crate::model::{BillingTarget, ProviderSnapshot, ResetAt, UsageWindow, WindowKind};
use crate::omp::OmpPaths;
use crate::providers::ProviderError;
use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// Upper bound on the JSON one call may return, so a pathological credential
/// pool cannot be read into memory unbounded.
const MAX_USAGE_BYTES: usize = 4 * 1024 * 1024;

/// One account's quota, as omp reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountUsage {
    /// `credentialPinHash()` of this account, so a transcript's pin selects it.
    pub pin: Option<String>,
    pub windows: Vec<UsageWindow>,
    pub fetched_at_unix: u64,
}

/// What omp knows about one provider's credentials.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProviderUsage {
    pub accounts: Vec<AccountUsage>,
    /// An API key is stored for this provider. Alone, that means the pane is
    /// paying per token and has no subscription window to show.
    pub has_api_key: bool,
    /// OAuth logins omp could identify but could not fetch quota for.
    ///
    /// The optional pin preserves the same account gate as successful reports:
    /// a failed peer account must not make this pane look unavailable.
    pub oauth_without_usage_pins: Vec<Option<String>>,
}

/// Run `omp usage --json` for one provider against one agent directory.
pub fn fetch(paths: &OmpPaths, provider_id: &str, now_unix: u64) -> Result<ProviderUsage> {
    let executable = std::env::var_os("HERDR_AGENT_QUOTA_OMP_BIN").unwrap_or_else(|| "omp".into());
    let mut command = Command::new(executable);
    command.args(["usage", "--json", "--provider", provider_id]);
    if let Some(config_dir) = config_dir_override(&paths.agent_dir) {
        // A plugin action runs in Herdr's environment, so the pane's own
        // `PI_CONFIG_DIR` or `--profile` never reaches us. Point omp back at
        // the directory the transcript came from.
        command.env("PI_CONFIG_DIR", config_dir);
    }
    let output = command
        .output()
        .map_err(|error| ProviderError::Request(error.to_string()))?;
    if !output.status.success() {
        return Err(ProviderError::Unavailable("omp usage failed".to_string()).into());
    }
    if output.stdout.len() > MAX_USAGE_BYTES {
        return Err(
            ProviderError::UnsupportedResponse("usage report too large".to_string()).into(),
        );
    }
    let value: Value = serde_json::from_slice(&output.stdout).context("decode omp usage report")?;
    Ok(parse_usage(&value, provider_id, now_unix))
}

/// `PI_CONFIG_DIR` for an agent directory, when one is needed.
///
/// omp resolves the variable relative to `$HOME` and appends `agent`, so only
/// an agent directory of that shape can be addressed. Anything else (an XDG
/// relocation, a directory outside home) returns `None`: omp's own default
/// resolution is then the best available guess, and a wrong `PI_CONFIG_DIR`
/// would be worse than none.
fn config_dir_override(agent_dir: &Path) -> Option<OsString> {
    let config_root = agent_dir
        .file_name()
        .filter(|name| *name == "agent")
        .and(agent_dir.parent())?;
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    let relative = config_root.strip_prefix(&home).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    (relative != Path::new(".omp")).then(|| relative.as_os_str().to_os_string())
}

/// Read one provider's accounts out of an `omp usage --json` payload.
pub fn parse_usage(value: &Value, provider_id: &str, now_unix: u64) -> ProviderUsage {
    let capacity_windows = capacity_windows(value, provider_id);
    let accounts = value
        .get("reports")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|report| report.get("provider").and_then(Value::as_str) == Some(provider_id))
        .filter_map(|report| account_usage(report, provider_id, now_unix, &capacity_windows))
        .collect();
    let has_api_key = value
        .get("accountsWithoutUsage")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .any(|account| {
            account.get("provider").and_then(Value::as_str) == Some(provider_id)
                && account.get("type").and_then(Value::as_str) == Some("api_key")
        });
    let oauth_without_usage_pins = value
        .get("accountsWithoutUsage")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|account| {
            account.get("provider").and_then(Value::as_str) == Some(provider_id)
                && account.get("type").and_then(Value::as_str) == Some("oauth")
        })
        .map(|account| identity_pin(account, provider_id))
        .collect();
    ProviderUsage {
        accounts,
        has_api_key,
        oauth_without_usage_pins,
    }
}

#[derive(Debug)]
struct CapacityWindow {
    label: String,
    duration_ms: Option<u64>,
}

fn capacity_windows(value: &Value, provider_id: &str) -> Vec<CapacityWindow> {
    value
        .get("capacity")
        .and_then(|capacity| capacity.get(provider_id))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|window| {
            Some(CapacityWindow {
                label: window.get("window")?.as_str()?.to_string(),
                duration_ms: window.get("durationMs").and_then(Value::as_u64),
            })
        })
        .collect()
}

fn account_usage(
    report: &Value,
    provider_id: &str,
    now_unix: u64,
    capacity_windows: &[CapacityWindow],
) -> Option<AccountUsage> {
    let limits = report.get("limits").and_then(Value::as_array)?;
    let mut windows: Vec<UsageWindow> = Vec::new();
    for kind in [
        WindowKind::FiveHour,
        WindowKind::Weekly,
        WindowKind::Monthly,
    ] {
        if let Some((limit, used)) = best_limit(limits, kind) {
            let duration_ms = limit.pointer("/window/durationMs").and_then(Value::as_u64);
            let label = source_window_label(limit, duration_ms, capacity_windows);
            let window = UsageWindow::new(kind, used, resets_at(limit))
                .ok()?
                .with_source_window(label, duration_ms.map(|milliseconds| milliseconds / 1_000));
            windows.push(window);
        }
    }
    if windows.is_empty() {
        return None;
    }
    Some(AccountUsage {
        pin: account_pin(report, provider_id),
        windows,
        fetched_at_unix: report
            .get("fetchedAt")
            .and_then(Value::as_u64)
            .map(|milliseconds| milliseconds / 1_000)
            .unwrap_or(now_unix),
    })
}

/// The limit that speaks for a window.
///
/// A provider can report several limits over the same duration — Anthropic
/// publishes a plain 7-day pool plus per-model ones. The plain pool is the one
/// the sidebar can explain, so the fewest-qualifiers id wins; equal ids fall
/// back to the tightest, which is the one that will actually stop the user.
fn best_limit(limits: &[Value], kind: WindowKind) -> Option<(&Value, f64)> {
    limits
        .iter()
        .filter(|limit| window_kind(limit) == Some(kind))
        .filter_map(|limit| {
            let used = used_percent(limit)?;
            let qualifiers = limit
                .get("id")
                .and_then(Value::as_str)
                .map(|id| id.matches(':').count())
                .unwrap_or(usize::MAX);
            Some((qualifiers, used, limit))
        })
        .min_by(|left, right| left.0.cmp(&right.0).then(right.1.total_cmp(&left.1)))
        .map(|(_, used, limit)| (limit, used))
}

/// Fit omp's normalized windows into the sidebar's existing short/long/monthly
/// slots. The rendered label still comes from omp; this classification only
/// chooses a row and never renames `1d` to a provider-specific guess.
fn window_kind(limit: &Value) -> Option<WindowKind> {
    if let Some(duration_ms) = limit.pointer("/window/durationMs").and_then(Value::as_u64) {
        let seconds = duration_ms / 1_000;
        return Some(if seconds <= 24 * 60 * 60 {
            WindowKind::FiveHour
        } else if seconds <= 14 * 24 * 60 * 60 {
            WindowKind::Weekly
        } else {
            WindowKind::Monthly
        });
    }

    let descriptor = [
        limit.pointer("/window/id").and_then(Value::as_str),
        limit.pointer("/window/label").and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
    .to_ascii_lowercase();
    if descriptor.contains("month") || descriptor.contains("30d") {
        Some(WindowKind::Monthly)
    } else if descriptor.contains("week") || descriptor.contains("7d") || descriptor.contains("1w")
    {
        Some(WindowKind::Weekly)
    } else if !descriptor.is_empty() {
        Some(WindowKind::FiveHour)
    } else {
        None
    }
}

fn source_window_label(
    limit: &Value,
    duration_ms: Option<u64>,
    capacity_windows: &[CapacityWindow],
) -> String {
    capacity_windows
        .iter()
        .find(|window| duration_ms.is_some() && window.duration_ms == duration_ms)
        .or_else(|| (capacity_windows.len() == 1).then(|| &capacity_windows[0]))
        .map(|window| window.label.clone())
        .or_else(|| {
            limit
                .pointer("/window/id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            limit
                .pointer("/window/label")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "usage".to_string())
}

/// Used percent, in omp's own precedence order (`resolveUsedFraction`).
fn used_percent(limit: &Value) -> Option<f64> {
    let amount = limit.get("amount")?;
    let number = |key: &str| amount.get(key).and_then(Value::as_f64);
    let fraction = if let Some(used_fraction) = number("usedFraction") {
        used_fraction
    } else if let (Some(used), Some(limit)) = (number("used"), number("limit")) {
        if limit <= 0.0 {
            return None;
        }
        used / limit
    } else if amount.get("unit").and_then(Value::as_str) == Some("percent") {
        number("used")? / 100.0
    } else if let Some(remaining) = number("remainingFraction") {
        1.0 - remaining
    } else {
        return None;
    };
    fraction
        .is_finite()
        .then(|| (fraction * 100.0).clamp(0.0, 100.0))
}

fn resets_at(limit: &Value) -> Option<ResetAt> {
    limit
        .pointer("/window/resetsAt")
        .and_then(Value::as_u64)
        .map(|milliseconds| ResetAt::from_unix_seconds(milliseconds / 1_000))
}

/// omp's `credentialPinHash()`, recomputed from the identity the report
/// carries: `sha256(provider\0accountId\0email\0orgId\0projectId)`.
///
/// The digest input is omp's persisted contract for `credential_pin` entries,
/// so a transcript written by any omp version that records pins matches.
fn account_pin(report: &Value, provider_id: &str) -> Option<String> {
    let metadata = report.get("metadata")?;
    identity_pin(metadata, provider_id)
}

fn identity_pin(identity: &Value, provider_id: &str) -> Option<String> {
    let field = |key: &str| {
        identity
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
    };
    let account_id = field("accountId");
    let email = field("email");
    if account_id.is_empty() && email.is_empty() {
        return None;
    }
    Some(credential_pin(
        provider_id,
        account_id,
        email,
        field("orgId"),
        field("projectId"),
    ))
}

fn credential_pin(
    provider_id: &str,
    account_id: &str,
    email: &str,
    org_id: &str,
    project_id: &str,
) -> String {
    let mut hasher = Sha256::new();
    for (index, part) in [provider_id, account_id, email, org_id, project_id]
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            hasher.update([0u8]);
        }
        hasher.update(part.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// The account a pane's transcript pins, or the only one there is.
///
/// With no pin and several accounts on the provider there is no evidence for
/// which one is paying, so nothing is published — a plausible-looking number
/// from the wrong account is worse than an empty row.
pub fn select_account<'a>(usage: &'a ProviderUsage, pin: Option<&str>) -> Option<&'a AccountUsage> {
    if let Some(pin) = pin {
        if let Some(account) = usage
            .accounts
            .iter()
            .find(|account| account.pin.as_deref() == Some(pin))
        {
            return Some(account);
        }
    }
    match usage.accounts.as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

/// Whether omp explicitly listed this pane's OAuth login without a report.
/// Multiple accounts without a transcript pin remain ambiguous.
pub fn oauth_without_usage_matches(usage: &ProviderUsage, pin: Option<&str>) -> bool {
    if let Some(pin) = pin {
        return usage
            .oauth_without_usage_pins
            .iter()
            .any(|candidate| candidate.as_deref() == Some(pin));
    }
    usage.oauth_without_usage_pins.len() == 1
}

pub fn snapshot(target: &BillingTarget, account: &AccountUsage) -> ProviderSnapshot {
    let mut snapshot = ProviderSnapshot::new(
        target.billing,
        account.windows.clone(),
        account.fetched_at_unix,
    );
    // The pin is the account gate: a second omp account on the same provider
    // must not read the first one's cached windows.
    snapshot.account_id = account.pin.clone();
    snapshot.source = format!("omp.{}", target.billing.source());
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn anthropic_report() -> Value {
        json!({
            "reports": [{
                "provider": "anthropic",
                "fetchedAt": 1_788_220_000_000u64,
                "metadata": {"email": "user@example.com", "accountId": "acct-1"},
                "limits": [
                    {
                        "id": "anthropic:5h",
                        "window": {"id": "5h", "durationMs": 18_000_000u64, "resetsAt": 1_788_230_000_000u64},
                        "amount": {"usedFraction": 0.25, "unit": "percent"}
                    },
                    {
                        "id": "anthropic:7d",
                        "window": {"id": "7d", "durationMs": 604_800_000u64},
                        "amount": {"used": 30.0, "limit": 100.0, "unit": "percent"}
                    },
                    {
                        "id": "anthropic:7d:opus",
                        "window": {"id": "7d", "durationMs": 604_800_000u64},
                        "amount": {"usedFraction": 0.9, "unit": "percent"}
                    }
                ]
            }],
            "accountsWithoutUsage": [],
            "disabledCredentials": []
        })
    }

    #[test]
    fn the_five_hour_and_seven_day_windows_are_read_from_their_durations() {
        let usage = parse_usage(&anthropic_report(), "anthropic", 0);
        let account = usage.accounts.first().expect("account");
        let five_hour = account
            .windows
            .iter()
            .find(|window| window.kind == WindowKind::FiveHour)
            .expect("5h");
        assert_eq!(five_hour.used_percent, 25.0);
        assert_eq!(
            five_hour.resets_at.map(|reset| reset.unix_seconds()),
            Some(1_788_230_000)
        );
        assert_eq!(account.fetched_at_unix, 1_788_220_000);
    }

    /// A per-model 7-day pool must not displace the plain one the sidebar can
    /// explain, even when it is the tighter of the two.
    #[test]
    fn a_qualified_limit_never_replaces_the_plain_window() {
        let usage = parse_usage(&anthropic_report(), "anthropic", 0);
        let weekly = usage.accounts[0]
            .windows
            .iter()
            .find(|window| window.kind == WindowKind::Weekly)
            .expect("7d");
        assert_eq!(weekly.used_percent, 30.0);
    }

    #[test]
    fn another_providers_report_is_ignored() {
        let usage = parse_usage(&anthropic_report(), "xai", 0);
        assert!(usage.accounts.is_empty());
    }

    #[test]
    fn a_limit_without_a_readable_amount_yields_no_window() {
        let value = json!({
            "reports": [{
                "provider": "xai",
                "limits": [{
                    "id": "1w",
                    "window": {"durationMs": 604_800_000u64},
                    "amount": {"unit": "credits"}
                }]
            }]
        });
        assert!(parse_usage(&value, "xai", 0).accounts.is_empty());
    }

    #[test]
    fn a_stored_api_key_is_reported_for_the_matching_provider_only() {
        let value = json!({
            "reports": [],
            "accountsWithoutUsage": [
                {"provider": "anthropic", "type": "api_key"},
                {"provider": "xai", "type": "oauth"}
            ]
        });
        assert!(parse_usage(&value, "anthropic", 0).has_api_key);
        assert!(!parse_usage(&value, "xai", 0).has_api_key);
    }

    #[test]
    fn a_logged_in_oauth_account_without_usage_keeps_its_pin() {
        let value = json!({
            "reports": [],
            "accountsWithoutUsage": [{
                "provider": "anthropic",
                "type": "oauth",
                "accountId": "acct-1",
                "email": "user@example.com",
                "orgId": "org-1"
            }]
        });
        let usage = parse_usage(&value, "anthropic", 0);
        let expected = credential_pin("anthropic", "acct-1", "user@example.com", "org-1", "");
        assert_eq!(usage.oauth_without_usage_pins, vec![Some(expected.clone())]);
        assert!(oauth_without_usage_matches(&usage, Some(&expected)));
        assert!(!oauth_without_usage_matches(
            &usage,
            Some("another-account")
        ));
    }

    /// The digest is omp's persisted contract; a change here silently orphans
    /// every recorded pin, so it is pinned to a known vector.
    #[test]
    fn the_account_pin_matches_omps_digest_input() {
        let report = json!({
            "metadata": {"email": "user@example.com", "accountId": "acct-1"}
        });
        let mut expected = Sha256::new();
        expected.update(b"anthropic\0acct-1\0user@example.com\0\0");
        assert_eq!(
            account_pin(&report, "anthropic"),
            Some(format!("{:x}", expected.finalize()))
        );
    }

    #[test]
    fn an_account_without_an_identity_has_no_pin() {
        assert_eq!(account_pin(&json!({"metadata": {}}), "anthropic"), None);
    }

    #[test]
    fn a_pinned_account_is_selected_over_its_peers() {
        let usage = ProviderUsage {
            accounts: vec![
                AccountUsage {
                    pin: Some("first".to_string()),
                    windows: vec![],
                    fetched_at_unix: 0,
                },
                AccountUsage {
                    pin: Some("second".to_string()),
                    windows: vec![],
                    fetched_at_unix: 0,
                },
            ],
            has_api_key: false,
            oauth_without_usage_pins: vec![],
        };
        assert_eq!(
            select_account(&usage, Some("second")).and_then(|account| account.pin.clone()),
            Some("second".to_string())
        );
        // Two accounts and no pin is not a coin flip.
        assert_eq!(select_account(&usage, None), None);
        assert_eq!(select_account(&usage, Some("third")), None);
    }

    /// The whole subprocess path, against a stub that records how it was
    /// called: the provider filter has to reach omp, and the report has to come
    /// back parsed.
    #[test]
    fn the_cli_is_called_for_one_provider_and_its_report_is_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("omp-stub");
        let arguments = dir.path().join("arguments");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" > {}\ncat <<'JSON'\n{}\nJSON\n",
                arguments.display(),
                serde_json::to_string(&anthropic_report()).unwrap()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("HERDR_AGENT_QUOTA_OMP_BIN", &stub);
        let paths = OmpPaths {
            agent_dir: dir.path().join(".omp/agent"),
            sessions: dir.path().join(".omp/agent/sessions"),
        };
        let usage = fetch(&paths, "anthropic", 0).expect("usage");
        std::env::remove_var("HERDR_AGENT_QUOTA_OMP_BIN");
        assert_eq!(usage.accounts.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&arguments).unwrap().trim(),
            "usage --json --provider anthropic"
        );
    }

    #[test]
    fn a_default_agent_directory_needs_no_config_override() {
        let home = directories::BaseDirs::new()
            .expect("home")
            .home_dir()
            .to_path_buf();
        assert_eq!(config_dir_override(&home.join(".omp/agent")), None);
        assert_eq!(
            config_dir_override(&home.join(".omp/profiles/work/agent")),
            Some(OsString::from(".omp/profiles/work"))
        );
        assert_eq!(config_dir_override(Path::new("/srv/omp/agent")), None);
    }
}
