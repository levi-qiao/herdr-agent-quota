use crate::cache::CacheStore;
use crate::model::{Provider, ProviderSnapshot, ResetAt, UsageWindow, WindowKind};
use crate::providers::ProviderError;
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

pub fn parse_rate_limits(
    value: &Value,
    fetched_at_unix: u64,
) -> std::result::Result<ProviderSnapshot, ProviderError> {
    let result = value.get("result").unwrap_or(value);
    let limits = result
        .get("rateLimits")
        .or_else(|| result.get("rate_limits"))
        .ok_or_else(|| ProviderError::UnsupportedResponse("missing rateLimits".to_string()))?;
    let objects = [limits.get("primary"), limits.get("secondary")]
        .into_iter()
        .flatten();
    let weekly = objects
        .filter_map(|candidate| {
            let duration = candidate
                .get("windowDurationMins")
                .or_else(|| candidate.get("window_duration_mins"))
                .and_then(Value::as_u64)?;
            if duration != 10_080 {
                return None;
            }
            let used = candidate
                .get("usedPercent")
                .or_else(|| candidate.get("used_percent"))
                .and_then(Value::as_f64)?;
            let reset = candidate
                .get("resetsAt")
                .or_else(|| candidate.get("resets_at"))
                .and_then(Value::as_u64)
                .map(ResetAt::from_unix_seconds);
            Some((used, reset))
        })
        .next()
        .ok_or_else(|| ProviderError::UnsupportedResponse("no seven-day rate limit".to_string()))?;
    let window = UsageWindow::new(WindowKind::Weekly, weekly.0, weekly.1)
        .map_err(|error| ProviderError::UnsupportedResponse(error.to_string()))?;
    Ok(ProviderSnapshot::new(
        Provider::Codex,
        vec![window],
        fetched_at_unix,
    ))
}

pub fn fetch() -> Result<ProviderSnapshot> {
    let executable = std::env::var_os("CODEX_BIN_PATH").unwrap_or_else(|| "codex".into());
    let mut command = Command::new(executable);
    command
        .args(["app-server", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().context("start codex app-server")?;
    let mut input = child.stdin.take().context("open codex app-server stdin")?;
    let stdout = child
        .stdout
        .take()
        .context("open codex app-server stdout")?;
    let mut output = BufReader::new(stdout);

    // The watchdog and this thread share the child, so it can only ever be
    // signalled while it is still unreaped. Signalling a bare pid after
    // `wait` would race with the operating system recycling that pid.
    let child = Arc::new(Mutex::new(Some(child)));
    let watchdog = Arc::clone(&child);
    thread::spawn(move || {
        thread::sleep(REQUEST_TIMEOUT);
        terminate(&watchdog);
    });

    let result = fetch_from_process(&mut input, &mut output);
    terminate(&child);
    result
}

/// Kill the app-server's process group and reap it, at most once.
///
/// Whichever of the request thread and the watchdog gets here first takes the
/// child; the other one finds an empty slot and does nothing.
fn terminate(child: &Mutex<Option<Child>>) {
    let Ok(mut slot) = child.lock() else {
        return;
    };
    let Some(mut child) = slot.take() else {
        return;
    };
    // `pre_exec` put the app-server in its own process group, so this also
    // collects any helper it spawned.
    #[cfg(unix)]
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn fetch_from_process(
    input: &mut ChildStdin,
    output: &mut BufReader<impl std::io::Read>,
) -> Result<ProviderSnapshot> {
    write_rpc(
        input,
        1,
        "initialize",
        serde_json::json!({
            "clientInfo": {"name": "herdr-agent-quota", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {}
        }),
    )?;
    let _ = read_rpc(output, 1)?;
    write_notification(input, "initialized", serde_json::json!({}))?;

    write_rpc(input, 2, "account/read", serde_json::json!({}))?;
    let account = read_rpc(output, 2)?;
    if !account_is_chatgpt(&account) {
        anyhow::bail!(ProviderError::Unavailable(
            "Codex is using API-key auth, not a ChatGPT subscription".to_string()
        ));
    }

    write_rpc(input, 3, "account/rateLimits/read", serde_json::json!({}))?;
    let limits = read_rpc(output, 3)?;
    let mut snapshot =
        parse_rate_limits(&limits, CacheStore::now_unix()).map_err(anyhow::Error::from)?;
    snapshot.account_id = current_account_id().or_else(|| account_id_from_rpc(&account));

    // Session previews come from Codex's local state database. This is one
    // bounded read in the same app-server process as the quota request; it
    // does not resume threads, scan rollout JSONL, or contact the model.
    write_rpc(
        input,
        4,
        "thread/list",
        serde_json::json!({
            "limit": 50,
            "sortKey": "updated_at",
            "useStateDbOnly": true
        }),
    )?;
    if let Ok(threads) = read_rpc(output, 4) {
        snapshot.session_summaries = parse_session_summaries(&threads);
    }
    Ok(snapshot)
}

fn parse_session_summaries(value: &Value) -> BTreeMap<String, String> {
    let result = value.get("result").unwrap_or(value);
    result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|thread| {
            let id = thread.get("id").and_then(Value::as_str)?;
            let preview = thread.get("preview").and_then(Value::as_str)?;
            let summary = preview
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .filter(|line| !line.eq_ignore_ascii_case("ask codex to do anything"))
                .map(truncate_summary)?;
            Some((id.to_string(), summary))
        })
        .collect()
}

fn truncate_summary(value: &str) -> String {
    let characters: Vec<char> = value.chars().collect();
    if characters.len() <= 80 {
        return value.to_string();
    }
    let mut summary: String = characters.into_iter().take(77).collect();
    summary.push('…');
    summary
}

fn write_rpc(input: &mut ChildStdin, id: u64, method: &str, params: Value) -> Result<()> {
    let message = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    });
    writeln!(input, "{}", serde_json::to_string(&message)?)?;
    input.flush()?;
    Ok(())
}

fn write_notification(input: &mut ChildStdin, method: &str, params: Value) -> Result<()> {
    let message = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params
    });
    writeln!(input, "{}", serde_json::to_string(&message)?)?;
    input.flush()?;
    Ok(())
}

fn read_rpc(output: &mut BufReader<impl std::io::Read>, expected_id: u64) -> Result<Value> {
    let mut line = String::new();
    loop {
        line.clear();
        let count = output.read_line(&mut line)?;
        if count == 0 {
            anyhow::bail!("Codex app-server exited before response {expected_id}");
        }
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if value.get("id").and_then(Value::as_u64) != Some(expected_id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            anyhow::bail!("Codex app-server request failed: {error}");
        }
        return Ok(value);
    }
}

pub fn auth_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CODEX_AUTH_FILE") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let home = PathBuf::from(home);
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    Ok(codex_home.join("auth.json"))
}

pub fn current_account_id() -> Option<String> {
    account_id_from_auth(&auth_path().ok()?)
}

pub fn auth_mtime_unix() -> Option<u64> {
    CacheStore::file_mtime_unix(&auth_path().ok()?)
}

pub fn account_id_from_auth(path: &Path) -> Option<String> {
    let value: Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    value
        .pointer("/tokens/account_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn account_id_from_rpc(value: &Value) -> Option<String> {
    let result = value.get("result").unwrap_or(value);
    let account = result.get("account").unwrap_or(result);
    ["accountId", "account_id", "chatgptAccountId", "id"]
        .iter()
        .find_map(|key| account.get(*key).and_then(Value::as_str))
        .filter(|value| !value.is_empty() && *value != "chatgpt")
        .map(str::to_string)
}

pub fn account_is_chatgpt(value: &Value) -> bool {
    let result = value.get("result").unwrap_or(value);
    let account = result.get("account").unwrap_or(result);
    let auth_mode = account
        .get("authMode")
        .or_else(|| account.get("auth_mode"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let account_type = account
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let plan = account
        .get("plan")
        .and_then(Value::as_str)
        .unwrap_or_default();
    [auth_mode, account_type, plan]
        .iter()
        .any(|value| value.to_ascii_lowercase().contains("chatgpt"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn selects_weekly_codex_window_by_duration_not_position() {
        let value = json!({
            "result": {"rateLimits": {
                "primary": {"usedPercent": 20.0, "windowDurationMins": 300, "resetsAt": 1786795200},
                "secondary": {"usedPercent": 61.0, "windowDurationMins": 10080, "resetsAt": 1787400000}
            }}
        });
        let snapshot = parse_rate_limits(&value, 1).unwrap();
        assert_eq!(
            snapshot.window(WindowKind::Weekly).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(1_787_400_000))
        );
    }

    #[test]
    fn rejects_codex_response_without_seven_day_window() {
        let value = json!({"result": {"rateLimits": {
            "primary": {"usedPercent": 20.0, "windowDurationMins": 300}
        }}});
        assert!(parse_rate_limits(&value, 1).is_err());
    }

    #[test]
    fn reads_codex_account_id_from_local_auth_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        fs::write(
            &path,
            r#"{"auth_mode":"chatgpt","tokens":{"account_id":"acc-1","access_token":"secret"}}"#,
        )
        .unwrap();
        assert_eq!(account_id_from_auth(&path).as_deref(), Some("acc-1"));
    }

    #[test]
    fn distinguishes_chatgpt_subscription_from_api_key() {
        assert!(account_is_chatgpt(
            &json!({"result": {"account": {"authMode": "chatgpt"}}})
        ));
        assert!(!account_is_chatgpt(
            &json!({"result": {"account": {"authMode": "api_key"}}})
        ));
    }

    #[test]
    fn extracts_compact_session_summaries_without_default_prompt() {
        let summaries = parse_session_summaries(&json!({
            "result": {"data": [
                {"id": "thread-1", "preview": "A real task\n\nmore detail"},
                {"id": "thread-2", "preview": "Ask Codex to do anything"}
            ]}
        }));
        assert_eq!(
            summaries.get("thread-1").map(String::as_str),
            Some("A real task")
        );
        assert!(!summaries.contains_key("thread-2"));
    }
}
