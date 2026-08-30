use crate::cache::CacheStore;
use crate::model::{
    sibling_quota_reset_in, CacheTotals, CacheUsage, ContextUsage, Provider, ProviderSnapshot,
    ResetAt, UsageWindow, WindowKind,
};
use crate::providers::ProviderError;
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const FIVE_HOUR_WINDOW_MINUTES: u64 = 5 * 60;
const WEEKLY_WINDOW_MINUTES: u64 = 7 * 24 * 60;
const ROLLOUT_TAIL_BYTES: u64 = 256 * 1024;
const ROLLOUT_HEAD_BYTES: u64 = 256 * 1024;
const CODEX_CONTEXT_BASELINE_TOKENS: u64 = 12_000;

pub fn parse_rate_limits(
    value: &Value,
    fetched_at_unix: u64,
) -> std::result::Result<ProviderSnapshot, ProviderError> {
    let result = value.get("result").unwrap_or(value);
    let windows = collect_codex_windows(result);
    if windows.is_empty() {
        return Err(ProviderError::UnsupportedResponse(
            "no supported rate limit windows".to_string(),
        ));
    }
    Ok(ProviderSnapshot::new(
        Provider::Codex,
        windows,
        fetched_at_unix,
    ))
}

fn collect_codex_windows(value: &Value) -> Vec<UsageWindow> {
    let mut windows = Vec::new();
    let mut push_from = |limits: &Value| {
        for candidate in [limits.get("primary"), limits.get("secondary")]
            .into_iter()
            .flatten()
        {
            let Some(window) = parse_codex_window(candidate) else {
                continue;
            };
            if windows
                .iter()
                .any(|existing: &UsageWindow| existing.kind == window.kind)
            {
                continue;
            }
            windows.push(window);
        }
    };
    if let Some(limits) = value.get("rateLimits").or_else(|| value.get("rate_limits")) {
        push_from(limits);
    }
    if let Some(by_id) = value
        .get("rateLimitsByLimitId")
        .or_else(|| value.get("rate_limits_by_limit_id"))
        .and_then(Value::as_object)
    {
        for limits in by_id.values() {
            push_from(limits);
        }
    }
    if value.get("primary").is_some() || value.get("secondary").is_some() {
        push_from(value);
    }
    windows
}

fn parse_codex_window(candidate: &Value) -> Option<UsageWindow> {
    if candidate.is_null() {
        return None;
    }
    let kind = candidate
        .get("windowDurationMins")
        .or_else(|| candidate.get("window_duration_mins"))
        .or_else(|| candidate.get("window_minutes"))
        .and_then(json_u64)
        .and_then(window_kind)?;
    let used = candidate
        .get("usedPercent")
        .or_else(|| candidate.get("used_percent"))
        .and_then(Value::as_f64)?;
    let reset = candidate
        .get("resetsAt")
        .or_else(|| candidate.get("resets_at"))
        .and_then(parse_reset);
    UsageWindow::new(kind, used, reset).ok()
}

fn json_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        let number = value.as_f64()?;
        (number.is_finite() && number >= 0.0).then_some(number.round() as u64)
    })
}

fn window_kind(duration_minutes: u64) -> Option<WindowKind> {
    // Token-count headers sometimes report 299 / 10079 remaining minutes
    // instead of the nominal 300 / 10080 window length.
    if duration_minutes.abs_diff(FIVE_HOUR_WINDOW_MINUTES) <= 60 {
        Some(WindowKind::FiveHour)
    } else if duration_minutes.abs_diff(WEEKLY_WINDOW_MINUTES) <= 180 {
        Some(WindowKind::Weekly)
    } else {
        None
    }
}

fn parse_reset(value: &Value) -> Option<ResetAt> {
    json_u64(value)
        .map(ResetAt::from_unix_seconds)
        .or_else(|| value.as_str().and_then(ResetAt::parse))
}

pub fn fetch() -> Result<ProviderSnapshot> {
    fetch_for_sessions(&[])
}

/// Fetch quota and supplement it with local diagnostics for the sessions
/// currently visible in Herdr. The empty slice is used by direct CLI calls;
/// the refresh path supplies pane session ids so an older pane is not lost
/// behind the bounded `thread/list` page.
pub fn fetch_for_sessions(session_ids: &[String]) -> Result<ProviderSnapshot> {
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

    let result = fetch_from_process(&mut input, &mut output, session_ids);
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
    requested_session_ids: &[String],
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
    let mut session_ids = requested_session_ids.to_vec();
    if let Ok(threads) = read_rpc(output, 4) {
        snapshot.session_summaries = parse_session_summaries(&threads);
        for session_id in parse_thread_ids(&threads) {
            if !session_ids.contains(&session_id) {
                session_ids.push(session_id);
            }
        }
    }
    enrich_local_sessions(&mut snapshot, &session_ids);
    Ok(snapshot)
}

fn parse_thread_ids(value: &Value) -> Vec<String> {
    let result = value.get("result").unwrap_or(value);
    result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|thread| {
            thread
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
        })
        .collect()
}

/// Supplement quota data with bounded, local-only reads from the rollout
/// belonging to each thread returned by `thread/list`. The app-server request
/// above does not expose live token usage, while the rollout tail does. We
/// never resume a thread, read prompt text into memory, or scan every pane's
/// output; only the matching JSONL filenames are opened.
fn enrich_local_sessions(snapshot: &mut ProviderSnapshot, session_ids: &[String]) {
    let Some(home) = codex_home().ok() else {
        return;
    };
    enrich_local_sessions_at(snapshot, &home, session_ids, auth_mtime_unix());
}

fn enrich_local_sessions_at(
    snapshot: &mut ProviderSnapshot,
    home: &Path,
    session_ids: &[String],
    auth_mtime_unix: Option<u64>,
) {
    if session_ids.is_empty() {
        return;
    }
    let mut newest: Option<(u64, Option<String>, ContextUsage)> = None;
    let mut newest_windows: Option<(u64, Vec<UsageWindow>)> = None;
    let rollout_paths = find_rollout_paths(home, session_ids);
    for session_id in session_ids {
        let Some(path) = rollout_paths.get(session_id) else {
            continue;
        };
        let Some(observation) = read_rollout_observation(path, session_id) else {
            continue;
        };
        if let Some(model) = observation.model.clone() {
            snapshot.session_models.insert(session_id.clone(), model);
        }
        let modified = fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_secs());
        if rollout_windows_can_fill_account_quota(
            snapshot,
            &observation.windows,
            modified,
            auth_mtime_unix,
        ) && newest_windows
            .as_ref()
            .is_none_or(|(current, _)| modified >= *current)
        {
            newest_windows = Some((modified, observation.windows));
        }
        let Some(context) = observation.context else {
            continue;
        };
        snapshot
            .session_contexts
            .insert(session_id.clone(), context.clone());

        if newest
            .as_ref()
            .is_none_or(|(current, _, _)| modified >= *current)
        {
            newest = Some((modified, observation.model, context));
        }
    }
    if let Some((_, windows)) = newest_windows {
        for window in windows {
            if snapshot.window(window.kind).is_none() {
                snapshot.windows.push(window);
            }
        }
    }
    if let Some((_, model, context)) = newest {
        if model.is_some() {
            snapshot.model = model;
        }
        snapshot.context = Some(context);
    }
}

/// Account-level 5h/7d comes from `account/rateLimits/read`. A local rollout
/// may fill a window the API omitted this tick, but only when that rollout
/// still belongs to the signed-in account: written at or after the current
/// `auth.json`, and with a weekly window that has not itself reset.
fn rollout_windows_can_fill_account_quota(
    snapshot: &ProviderSnapshot,
    rollout_windows: &[UsageWindow],
    rollout_mtime: u64,
    auth_mtime_unix: Option<u64>,
) -> bool {
    if rollout_windows.is_empty() {
        return false;
    }
    if auth_mtime_unix.is_some_and(|auth| rollout_mtime < auth) {
        return false;
    }
    !sibling_quota_reset_in(&snapshot.windows, rollout_windows)
}

fn find_rollout_paths(home: &Path, session_ids: &[String]) -> BTreeMap<String, PathBuf> {
    let mut directories = vec![home.join("sessions"), home.join("archived_sessions")];
    let mut newest = BTreeMap::<String, (u64, PathBuf)>::new();
    while let Some(directory) = directories.pop() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                directories.push(path);
                continue;
            }
            if !file_type.is_file()
                || path.extension().and_then(|extension| extension.to_str()) != Some("jsonl")
            {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let matching_ids = session_ids
                .iter()
                .filter(|session_id| name.contains(session_id.as_str()));
            let modified = entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_secs());
            for session_id in matching_ids {
                let replace = newest
                    .get(session_id)
                    .is_none_or(|(current, _)| modified >= *current);
                if replace {
                    newest.insert(session_id.clone(), (modified, path.clone()));
                }
            }
        }
    }
    newest
        .into_iter()
        .map(|(session_id, (_, path))| (session_id, path))
        .collect()
}

struct RolloutObservation {
    model: Option<String>,
    context: Option<ContextUsage>,
    windows: Vec<UsageWindow>,
}

fn read_rollout_observation(path: &Path, session_id: &str) -> Option<RolloutObservation> {
    let mut file = fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(ROLLOUT_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::with_capacity((length - start) as usize);
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let text = if start == 0 {
        text.into_owned()
    } else {
        text.split_once('\n')?.1.to_string()
    };
    let mut observation = parse_rollout_observation(&text, session_id)?;
    if observation.model.is_none() {
        observation.model = read_rollout_head_model(path);
    }
    Some(observation)
}

fn read_rollout_head_model(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(ROLLOUT_HEAD_BYTES).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    parse_rollout_model(&text)
}

fn parse_rollout_observation(text: &str, session_id: &str) -> Option<RolloutObservation> {
    let mut model = None;
    let mut context = None;
    let mut windows = Vec::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) == Some("turn_context") {
            let payload = entry.get("payload").unwrap_or(&entry);
            model = parse_model_payload(payload).or(model);
            continue;
        }
        if entry.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }
        let payload = entry.get("payload").unwrap_or(&entry);
        if payload.get("type").and_then(Value::as_str) != Some("token_count") {
            continue;
        }
        if let Some(rate_limits) = payload.get("rate_limits") {
            windows = collect_codex_windows(rate_limits);
        }
        let info = payload.get("info").unwrap_or(payload);
        let Some(last) = info
            .get("last_token_usage")
            .or_else(|| info.get("lastTokenUsage"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        let Some(window) = info
            .get("model_context_window")
            .or_else(|| info.get("modelContextWindow"))
            .and_then(Value::as_u64)
        else {
            continue;
        };
        let total = token_count(last, "total_tokens", "totalTokens");
        if total == 0 || window <= CODEX_CONTEXT_BASELINE_TOKENS {
            continue;
        }
        let used = total.saturating_sub(CODEX_CONTEXT_BASELINE_TOKENS) as f64
            / (window - CODEX_CONTEXT_BASELINE_TOKENS) as f64
            * 100.0;
        let Some(info_object) = info.as_object() else {
            continue;
        };
        let cache = parse_rollout_cache(info_object).map(|cache| {
            let totals = CacheTotals::from_token_counts(
                cache.fresh_input_tokens,
                cache.read_tokens,
                cache.creation_tokens,
            );
            cache.with_session_totals(totals, session_id, 0)
        });
        let context_value = ContextUsage::new(used.clamp(0.0, 100.0))
            .ok()?
            .with_cache(cache);
        context = Some(context_value);
    }
    Some(RolloutObservation {
        model,
        context,
        windows,
    })
}

fn parse_rollout_model(text: &str) -> Option<String> {
    text.lines().rev().find_map(|line| {
        let entry = serde_json::from_str::<Value>(line).ok()?;
        (entry.get("type").and_then(Value::as_str) == Some("turn_context"))
            .then(|| parse_model_payload(entry.get("payload").unwrap_or(&entry)))
            .flatten()
    })
}

fn parse_model_payload(payload: &Value) -> Option<String> {
    payload
        .get("model")
        .or_else(|| payload.get("model_slug"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
}

fn parse_rollout_cache(info: &serde_json::Map<String, Value>) -> Option<CacheUsage> {
    let totals = info
        .get("total_token_usage")
        .or_else(|| info.get("totalTokenUsage"))
        .and_then(Value::as_object)
        .or_else(|| {
            info.get("last_token_usage")
                .or_else(|| info.get("lastTokenUsage"))
                .and_then(Value::as_object)
        })?;
    let input = token_count(totals, "input_tokens", "inputTokens");
    let read = token_count(totals, "cached_input_tokens", "cachedInputTokens");
    let creation =
        token_count(totals, "cache_write_input_tokens", "cacheWriteInputTokens").max(token_count(
            totals,
            "cache_creation_input_tokens",
            "cacheCreationInputTokens",
        ));
    CacheUsage::from_token_counts(input.saturating_sub(read), read, creation)
}

fn token_count(object: &serde_json::Map<String, Value>, snake: &str, camel: &str) -> u64 {
    object
        .get(snake)
        .or_else(|| object.get(camel))
        .and_then(Value::as_u64)
        .unwrap_or_default()
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
    Ok(codex_home()?.join("auth.json"))
}

fn codex_home() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let home = PathBuf::from(home);
    Ok(std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex")))
}

pub fn current_account_id() -> Option<String> {
    account_id_from_auth(&auth_path().ok()?)
}

pub fn auth_mtime_unix() -> Option<u64> {
    CacheStore::file_mtime_unix(&auth_path().ok()?)
}

pub fn account_id_from_auth(path: &Path) -> Option<String> {
    #[derive(Deserialize)]
    struct AuthMetadata {
        tokens: Option<TokenMetadata>,
    }

    #[derive(Deserialize)]
    struct TokenMetadata {
        #[serde(default)]
        account_id: Option<String>,
        #[serde(default, alias = "chatgptAccountId")]
        chatgpt_account_id: Option<String>,
    }

    // Only materialize the stable account id. Token fields are ignored by the
    // streaming deserializer and never enter an owned Rust value.
    let metadata: AuthMetadata =
        serde_json::from_reader(BufReader::new(fs::File::open(path).ok()?)).ok()?;
    let tokens = metadata.tokens?;
    tokens
        .account_id
        .or(tokens.chatgpt_account_id)
        .filter(|value| !value.is_empty())
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
    fn selects_codex_windows_by_duration_not_position() {
        let value = json!({
            "result": {"rateLimits": {
                "primary": {"usedPercent": 20.0, "windowDurationMins": 300, "resetsAt": 1786795200},
                "secondary": {"usedPercent": 61.0, "windowDurationMins": 10080, "resetsAt": 1787400000}
            }}
        });
        let snapshot = parse_rate_limits(&value, 1).unwrap();
        assert_eq!(snapshot.windows.len(), 2);
        assert_eq!(
            snapshot.window(WindowKind::FiveHour).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(1_786_795_200))
        );
        assert_eq!(
            snapshot.window(WindowKind::Weekly).unwrap().resets_at,
            Some(ResetAt::from_unix_seconds(1_787_400_000))
        );
    }

    #[test]
    fn accepts_a_codex_response_with_only_the_five_hour_window() {
        let value = json!({"result": {"rateLimits": {
            "primary": {"usedPercent": 20.0, "windowDurationMins": 300}
        }}});
        let snapshot = parse_rate_limits(&value, 1).unwrap();
        assert_eq!(snapshot.windows.len(), 1);
        assert!(snapshot.window(WindowKind::FiveHour).is_some());
    }

    #[test]
    fn rejects_codex_response_without_supported_windows() {
        let value = json!({"result": {"rateLimits": {
            "primary": {"usedPercent": 20.0, "windowDurationMins": 60}
        }}});
        assert!(parse_rate_limits(&value, 1).is_err());
    }

    #[test]
    fn maps_near_five_hour_header_durations_to_the_five_hour_window() {
        let value = json!({"result": {"rateLimits": {
            "primary": {"usedPercent": 12.0, "windowDurationMins": 299, "resetsAt": 1786795200},
            "secondary": {"usedPercent": 24.0, "windowDurationMins": 10079, "resetsAt": 1787400000}
        }}});
        let snapshot = parse_rate_limits(&value, 1).unwrap();
        assert!(snapshot.window(WindowKind::FiveHour).is_some());
        assert!(snapshot.window(WindowKind::Weekly).is_some());
    }

    #[test]
    fn reads_a_five_hour_window_from_another_limit_id_bucket() {
        let value = json!({
            "result": {
                "rateLimits": {
                    "primary": {"usedPercent": 11.0, "windowDurationMins": 10080, "resetsAt": 1787400000},
                    "secondary": null
                },
                "rateLimitsByLimitId": {
                    "codex": {
                        "primary": {"usedPercent": 11.0, "windowDurationMins": 10080, "resetsAt": 1787400000},
                        "secondary": null
                    },
                    "codex_other": {
                        "primary": {"usedPercent": 40.0, "windowDurationMins": 300, "resetsAt": 1786795200},
                        "secondary": null
                    }
                }
            }
        });
        let snapshot = parse_rate_limits(&value, 1).unwrap();
        assert_eq!(
            snapshot.window(WindowKind::FiveHour).unwrap().used_percent,
            40.0
        );
        assert_eq!(
            snapshot.window(WindowKind::Weekly).unwrap().used_percent,
            11.0
        );
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

    #[test]
    fn parses_rollout_model_context_and_session_cache() {
        let observation = parse_rollout_observation(
            &format!(
                "{}\n{}\n",
                serde_json::to_string(&json!({
                    "type": "turn_context",
                    "payload": {"model": "gpt-5.6-luna"}
                }))
                .unwrap(),
                serde_json::to_string(&json!({
                    "type": "event_msg",
                    "timestamp": "2026-08-26T02:28:42Z",
                    "payload": {
                        "type": "token_count",
                        "info": {
                            "last_token_usage": {
                                "total_tokens": 50_000,
                                "cached_input_tokens": 800,
                                "cache_write_input_tokens": 100
                            },
                            "total_token_usage": {
                                "input_tokens": 1_000,
                                "cached_input_tokens": 800,
                                "cache_write_input_tokens": 100
                            },
                            "model_context_window": 100_000
                        }
                    }
                }))
                .unwrap()
            ),
            "session-1",
        )
        .unwrap();
        assert_eq!(observation.model.as_deref(), Some("gpt-5.6-luna"));
        let context = observation.context.unwrap();
        assert!((context.used_percent - 43.1818).abs() < 0.001);
        let cache = context.cache.unwrap();
        assert_eq!(cache.fresh_input_tokens, 200);
        assert_eq!(cache.read_tokens, 800);
        assert_eq!(cache.creation_tokens, 100);
        assert_eq!(cache.session_id.as_deref(), Some("session-1"));
        assert_eq!(cache.session_totals.unwrap().hit_percent, 72.72727272727273);
        assert_eq!(cache.ttl_seconds, None);
        assert_eq!(cache.last_activity_unix, None);
    }

    #[test]
    fn enriches_only_rollouts_matching_thread_ids() {
        let directory = tempfile::tempdir().unwrap();
        let rollout_dir = directory.path().join("sessions/2026/08/26");
        fs::create_dir_all(&rollout_dir).unwrap();
        fs::write(
            rollout_dir.join("rollout-session-1.jsonl"),
            r#"{"type":"turn_context","payload":{"model":"gpt-5.6"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"total_tokens":25000},"model_context_window":100000}}}
"#,
        )
        .unwrap();
        let mut snapshot = ProviderSnapshot::new(Provider::Codex, vec![], 1);
        enrich_local_sessions_at(
            &mut snapshot,
            directory.path(),
            &["session-1".to_string(), "other-session".to_string()],
            None,
        );
        assert_eq!(snapshot.model.as_deref(), Some("gpt-5.6"));
        assert!(snapshot.session_contexts.contains_key("session-1"));
        assert!(!snapshot.session_contexts.contains_key("other-session"));
    }

    #[test]
    fn latest_rollout_rate_limits_fill_a_missing_five_hour_window() {
        let directory = tempfile::tempdir().unwrap();
        let rollout_dir = directory.path().join("sessions/2026/08/27");
        fs::create_dir_all(&rollout_dir).unwrap();
        fs::write(
            rollout_dir.join("rollout-session-1.jsonl"),
            r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":12.0,"window_minutes":300,"resets_at":1786795200},"secondary":{"used_percent":24.0,"window_minutes":10080,"resets_at":1787400000}},"info":{"last_token_usage":{"total_tokens":25000},"model_context_window":100000}}}
"#,
        )
        .unwrap();
        let mut snapshot = parse_rate_limits(
            &json!({"result":{"rateLimits":{
                "primary":{"usedPercent":24.0,"windowDurationMins":10080,"resetsAt":1787400000},
                "secondary":null
            }}}),
            1,
        )
        .unwrap();
        enrich_local_sessions_at(
            &mut snapshot,
            directory.path(),
            &["session-1".to_string()],
            None,
        );
        assert_eq!(
            snapshot.window(WindowKind::FiveHour).unwrap().used_percent,
            12.0
        );
        assert_eq!(
            snapshot.window(WindowKind::Weekly).unwrap().used_percent,
            24.0
        );
    }

    #[test]
    fn stale_rollout_five_hour_window_is_not_used_after_a_newer_weekly_only_event() {
        let directory = tempfile::tempdir().unwrap();
        let rollout_dir = directory.path().join("sessions/2026/08/27");
        fs::create_dir_all(&rollout_dir).unwrap();
        fs::write(
            rollout_dir.join("rollout-session-1.jsonl"),
            r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":99.0,"window_minutes":300,"resets_at":1786795200},"secondary":{"used_percent":49.0,"window_minutes":10080,"resets_at":1787400000}}}}
{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":0.0,"window_minutes":10080,"resets_at":1787397000},"secondary":null},"info":{"last_token_usage":{"total_tokens":25000},"model_context_window":100000}}}
"#,
        )
        .unwrap();
        let mut snapshot = parse_rate_limits(
            &json!({"result":{"rateLimits":{
                "primary":{"usedPercent":0.0,"windowDurationMins":10080,"resetsAt":1787397000},
                "secondary":null
            }}}),
            1,
        )
        .unwrap();
        enrich_local_sessions_at(
            &mut snapshot,
            directory.path(),
            &["session-1".to_string()],
            None,
        );
        assert!(snapshot.window(WindowKind::FiveHour).is_none());
        assert_eq!(
            snapshot.window(WindowKind::Weekly).unwrap().used_percent,
            0.0
        );
    }

    #[test]
    fn older_rollout_five_hour_window_is_not_used_after_auth_switch() {
        let directory = tempfile::tempdir().unwrap();
        let rollout_dir = directory.path().join("sessions/2026/08/27");
        fs::create_dir_all(&rollout_dir).unwrap();
        fs::write(
            rollout_dir.join("rollout-session-1.jsonl"),
            r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":80.0,"window_minutes":300,"resets_at":1786795200},"secondary":{"used_percent":31.0,"window_minutes":10080,"resets_at":1787400000}},"info":{"last_token_usage":{"total_tokens":25000},"model_context_window":100000}}}
"#,
        )
        .unwrap();
        let mut snapshot = parse_rate_limits(
            &json!({"result":{"rateLimits":{
                "primary":{"usedPercent":12.0,"windowDurationMins":10080,"resetsAt":1787400000},
                "secondary":null
            }}}),
            1,
        )
        .unwrap();
        enrich_local_sessions_at(
            &mut snapshot,
            directory.path(),
            &["session-1".to_string()],
            Some(u64::MAX),
        );
        assert!(snapshot.window(WindowKind::FiveHour).is_none());
        assert_eq!(
            snapshot.window(WindowKind::Weekly).unwrap().used_percent,
            12.0
        );
    }

    #[test]
    fn rollout_five_hour_window_is_not_used_when_weekly_reset_disagrees() {
        let directory = tempfile::tempdir().unwrap();
        let rollout_dir = directory.path().join("sessions/2026/08/27");
        fs::create_dir_all(&rollout_dir).unwrap();
        fs::write(
            rollout_dir.join("rollout-session-1.jsonl"),
            r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":80.0,"window_minutes":300,"resets_at":1786795200},"secondary":{"used_percent":31.0,"window_minutes":10080,"resets_at":1787400000}},"info":{"last_token_usage":{"total_tokens":25000},"model_context_window":100000}}}
"#,
        )
        .unwrap();
        let mut snapshot = parse_rate_limits(
            &json!({"result":{"rateLimits":{
                "primary":{"usedPercent":12.0,"windowDurationMins":10080,"resetsAt":1787397000},
                "secondary":null
            }}}),
            1,
        )
        .unwrap();
        enrich_local_sessions_at(
            &mut snapshot,
            directory.path(),
            &["session-1".to_string()],
            None,
        );
        assert!(snapshot.window(WindowKind::FiveHour).is_none());
        assert_eq!(
            snapshot.window(WindowKind::Weekly).unwrap().used_percent,
            12.0
        );
    }

    #[test]
    fn reads_codex_account_id_from_chatgpt_account_id_when_account_id_is_absent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        fs::write(
            &path,
            r#"{"auth_mode":"chatgpt","tokens":{"chatgpt_account_id":"acc-2","access_token":"secret"}}"#,
        )
        .unwrap();
        assert_eq!(account_id_from_auth(&path).as_deref(), Some("acc-2"));
    }
}
