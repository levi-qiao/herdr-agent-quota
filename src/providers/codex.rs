use crate::cache::CacheStore;
use crate::model::{
    CacheTotals, CacheUsage, ContextUsage, Provider, ProviderSnapshot, ResetAt, UsageWindow,
    WindowKind,
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
/// Compressed rollouts cannot be sought to their tail. Only decode this much
/// output from the beginning, where Codex writes session_meta and early
/// model/context records. Do not expand an entire archived history.
const ROLLOUT_COMPRESSED_PREFIX_BYTES: u64 = 256 * 1024;
/// How far back from EOF to look for the latest `turn_context` when the tail
/// has none. Codex writes that event at turn start, then tool calls and
/// `token_count` lines; a long turn can push the model several megabytes
/// behind EOF. The previous 256 KB *head* fallback returned the session-start
/// model instead. Chunked reverse reads stay within this budget so a 40 MB
/// rollout is not scanned on every watch pulse. Observed live threads put the
/// latest model 1–4 MB from EOF.
const ROLLOUT_MODEL_SCAN_BYTES: u64 = 8 * 1024 * 1024;
/// Extra bytes kept from the newer chunk so a `turn_context` line that
/// straddles a 256 KB boundary is complete in the older window. Live
/// `turn_context` records are about 2 KB.
const ROLLOUT_LINE_OVERLAP_BYTES: u64 = 8 * 1024;
const CODEX_CONTEXT_BASELINE_TOKENS: u64 = 12_000;
/// Prompt cache lifetime assumed for a Codex request.
///
/// Codex never records a TTL or an expiry: the rollout JSONL only carries
/// `cached_input_tokens` / `cache_write_input_tokens` and the request
/// timestamp, and the `prompt_cache_options` the Responses API returns is
/// dropped by the Codex SSE parser before it reaches disk. OpenAI documents
/// `30m` as the default and currently only supported `prompt_cache_options.ttl`,
/// so the sidebar anchors that TTL to the last recorded request. This is an
/// estimate, not a server-reported expiry — the sidebar labels it `ttl≈`.
pub(crate) const CODEX_PROMPT_CACHE_TTL_SECONDS: u64 = 30 * 60;

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

/// Fetch quota and supplement it with local diagnostics for the sessions
/// currently visible in Herdr. The empty slice is used by direct CLI calls;
/// the refresh path supplies pane session ids so an older pane is not lost
/// behind the bounded `thread/list` page.
pub fn fetch_for_sessions(session_ids: &[String]) -> Result<ProviderSnapshot> {
    let mut command = codex_command();
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

/// Herdr runs hooks, actions, and the watcher with its server's PATH, which
/// on macOS can be launchd's `/usr/bin:/bin:/usr/sbin:/sbin`. A bare `codex`
/// then never starts, every fetch keeps the cached snapshot, and pane models
/// stop following new sessions. Fall back to the usual install directories,
/// and put the chosen one on the child's PATH so an npm `env node` shim finds
/// the `node` installed beside it.
fn codex_command() -> Command {
    let path = std::env::var_os("PATH");
    let fallbacks = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/bin"))
        .into_iter()
        .chain(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from))
        .collect::<Vec<_>>();
    let (executable, directory) = resolve_codex_executable(
        std::env::var_os("CODEX_BIN_PATH"),
        path.as_deref(),
        &fallbacks,
    );
    let mut command = Command::new(executable);
    if let Some(directory) = directory {
        let paths = std::iter::once(directory)
            .chain(path.iter().flat_map(std::env::split_paths))
            .collect::<Vec<_>>();
        if let Ok(joined) = std::env::join_paths(paths) {
            command.env("PATH", joined);
        }
    }
    command
}

fn resolve_codex_executable(
    configured: Option<std::ffi::OsString>,
    path: Option<&std::ffi::OsStr>,
    fallbacks: &[PathBuf],
) -> (std::ffi::OsString, Option<PathBuf>) {
    if let Some(configured) = configured {
        // An npm-style shim starts with `#!/usr/bin/env node`, so the script
        // resolves `node` through its own PATH. Herdr's server PATH omits
        // Homebrew, and `codex_command` already prepends the install directory
        // in the auto-discovery case. Mirror that here when the override names
        // a file with a parent directory. A bare name like `codex` is resolved
        // against the existing PATH, so there is no directory to prepend.
        let directory = PathBuf::from(&configured)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(PathBuf::from);
        return (configured, directory);
    }
    let on_path = path
        .into_iter()
        .flat_map(std::env::split_paths)
        .any(|directory| directory.join("codex").is_file());
    if !on_path {
        if let Some(directory) = fallbacks
            .iter()
            .find(|directory| directory.join("codex").is_file())
        {
            return (
                directory.join("codex").into_os_string(),
                Some(directory.clone()),
            );
        }
    }
    ("codex".into(), None)
}

const PROCESS_SESSION_START_TOLERANCE_SECONDS: u64 = 90;

/// Bind missing Herdr sessions by a Codex process start time as well as the
/// rollout's exact cwd. Treehouse worktrees can be reused, so cwd alone is
/// deliberately insufficient once more than one rollout names it.
pub fn session_ids_for_panes(panes: &[(String, String, u64)]) -> BTreeMap<String, String> {
    let Some(home) = codex_home().ok() else {
        return BTreeMap::new();
    };
    session_ids_for_panes_at(&home, panes)
}

fn session_ids_for_panes_at(
    home: &Path,
    panes: &[(String, String, u64)],
) -> BTreeMap<String, String> {
    let wanted = panes
        .iter()
        .filter(|(_, cwd, _)| !cwd.is_empty())
        .map(|(_, cwd, _)| cwd.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut by_cwd = BTreeMap::<String, Vec<(String, u64)>>::new();
    for path in rollouts_started_near(home, panes.iter().map(|(_, _, started)| *started)) {
        let Some((session_id, cwd, started_at)) = rollout_session_meta_with_started_at(&path)
        else {
            continue;
        };
        if wanted.contains(cwd.as_str())
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().contains(session_id.as_str()))
        {
            by_cwd
                .entry(cwd)
                .or_default()
                .push((session_id, started_at));
        }
    }
    panes
        .iter()
        .filter_map(|(pane_id, cwd, process_started_at)| {
            let candidates = by_cwd.get(cwd)?;
            let mut matches = candidates.iter().filter(|(_, rollout_started_at)| {
                rollout_started_at.abs_diff(*process_started_at)
                    <= PROCESS_SESSION_START_TOLERANCE_SECONDS
            });
            let (session_id, _) = matches.next()?;
            matches
                .next()
                .is_none()
                .then(|| (pane_id.clone(), session_id.clone()))
        })
        .collect()
}

/// Rollouts whose file date is within a day of a process start. Codex names
/// both the `sessions/YYYY/MM/DD` directory and the file by the local start
/// date, so the UTC day either side covers every offset. Every Herdr
/// inventory read resolves session-less panes, so this must not walk the
/// whole rollout history.
fn rollouts_started_near(home: &Path, starts: impl Iterator<Item = u64>) -> Vec<PathBuf> {
    let days = starts
        .filter_map(|started| i64::try_from(started).ok())
        .filter_map(|started| time::OffsetDateTime::from_unix_timestamp(started).ok())
        .flat_map(|started| {
            [
                started.date().previous_day(),
                Some(started.date()),
                started.date().next_day(),
            ]
        })
        .flatten()
        .collect::<std::collections::BTreeSet<_>>();
    let mut directories = days
        .iter()
        .map(|day| {
            home.join("sessions")
                .join(format!("{:04}", day.year()))
                .join(format!("{:02}", u8::from(day.month())))
                .join(format!("{:02}", day.day()))
        })
        .collect::<Vec<_>>();
    directories.push(home.join("archived_sessions"));
    let prefixes = days
        .iter()
        .map(|day| {
            format!(
                "rollout-{:04}-{:02}-{:02}T",
                day.year(),
                u8::from(day.month()),
                day.day()
            )
        })
        .collect::<Vec<_>>();
    let mut paths = Vec::new();
    for directory in directories {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if is_rollout_file(&name)
                && prefixes.iter().any(|prefix| name.starts_with(prefix))
                && entry.file_type().is_ok_and(|file_type| file_type.is_file())
            {
                paths.push(entry.path());
            }
        }
    }
    paths
}

fn rollout_session_meta_with_started_at(path: &Path) -> Option<(String, String, u64)> {
    let line = if is_compressed_rollout(path) {
        read_compressed_prefix(path)?.lines().next()?.to_string()
    } else {
        let file = fs::File::open(path).ok()?;
        BufReader::new(file).lines().next()?.ok()?
    };
    let entry = serde_json::from_str::<Value>(&line).ok()?;
    (entry.get("type").and_then(Value::as_str) == Some("session_meta")).then_some(())?;
    let payload = entry.get("payload")?;
    let session_id = payload.get("id")?.as_str()?.trim();
    let cwd = payload.get("cwd")?.as_str()?.trim();
    let started_at = parse_rollout_timestamp(&entry)?;
    (!session_id.is_empty() && !cwd.is_empty())
        .then(|| (session_id.to_string(), cwd.to_string(), started_at))
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
            "clientInfo": {"name": crate::identity::PLUGIN_ID, "version": env!("CARGO_PKG_VERSION")},
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
    enrich_local_sessions_at(snapshot, &home, session_ids);
}

fn enrich_local_sessions_at(snapshot: &mut ProviderSnapshot, home: &Path, session_ids: &[String]) {
    if session_ids.is_empty() {
        return;
    }
    let mut newest: Option<(u64, Option<String>, ContextUsage)> = None;
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
    if let Some((_, model, context)) = newest {
        if model.is_some() {
            snapshot.model = model;
        }
        snapshot.context = Some(context);
    }
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
            if !file_type.is_file() || !is_rollout_file(&entry.file_name().to_string_lossy()) {
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
}

fn is_rollout_file(name: &str) -> bool {
    name.ends_with(".jsonl") || name.ends_with(".jsonl.zst")
}

fn is_compressed_rollout(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with(".jsonl.zst"))
}

fn read_compressed_prefix(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let decoder = zstd::stream::read::Decoder::new(file).ok()?;
    let mut bytes = Vec::new();
    decoder
        .take(ROLLOUT_COMPRESSED_PREFIX_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_rollout_observation(path: &Path, session_id: &str) -> Option<RolloutObservation> {
    if is_compressed_rollout(path) {
        return parse_rollout_observation(&read_compressed_prefix(path)?, session_id);
    }
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
        // The tail is live token_count / tool output. The latest model sits
        // further back, at the start of this turn. Never fall back to the
        // file head: that is the first turn's model, not the current one.
        observation.model = read_latest_rollout_model(path);
    }
    Some(observation)
}

/// Walk the rollout newest-first in tail-sized chunks until a `turn_context`
/// model is found, stopping at [`ROLLOUT_MODEL_SCAN_BYTES`]. Adjacent chunks
/// overlap so a `turn_context` that straddles a boundary is still parsed.
fn read_latest_rollout_model(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    if length == 0 {
        return None;
    }
    let floor = length.saturating_sub(ROLLOUT_MODEL_SCAN_BYTES);
    let mut cursor = length;
    while cursor > floor {
        let start = cursor.saturating_sub(ROLLOUT_TAIL_BYTES).max(floor);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(cursor.saturating_sub(start))
            .read_to_end(&mut bytes)
            .ok()?;
        let text = String::from_utf8_lossy(&bytes);
        let mut slice = text.as_ref();
        if start > 0 {
            slice = match slice.split_once('\n') {
                Some((_, rest)) => rest,
                None => {
                    let Some(next) = next_reverse_cursor(start, cursor, floor) else {
                        break;
                    };
                    cursor = next;
                    continue;
                }
            };
        }
        if cursor < length && !slice.is_empty() && !slice.ends_with('\n') {
            slice = slice.rsplit_once('\n').map(|(rest, _)| rest).unwrap_or("");
        }
        if let Some(model) = parse_rollout_model(slice) {
            return Some(model);
        }
        let Some(next) = next_reverse_cursor(start, cursor, floor) else {
            break;
        };
        cursor = next;
    }
    None
}

fn next_reverse_cursor(start: u64, cursor: u64, floor: u64) -> Option<u64> {
    if start <= floor {
        return None;
    }
    let next = start.saturating_add(ROLLOUT_LINE_OVERLAP_BYTES);
    (next < cursor).then_some(next)
}

fn parse_rollout_observation(text: &str, session_id: &str) -> Option<RolloutObservation> {
    let mut model = None;
    let mut context = None;
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
        // Rollouts do not identify the serving account. A still-running old
        // session can write after auth.json changes, so its quota must never
        // supplement the current account's app-server response.
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
            let cache = cache.with_session_totals(totals, session_id, 0);
            // Every request refreshes the prefix cache, so the entry's own
            // timestamp is the anchor. `cache_write_input_tokens` stays 0 on
            // ChatGPT-backed sessions even while reads are large, so it cannot
            // be used to pick the anchor.
            match parse_rollout_timestamp(&entry) {
                Some(requested_at) => {
                    cache.with_ttl_estimate(CODEX_PROMPT_CACHE_TTL_SECONDS, requested_at)
                }
                None => cache,
            }
        });
        let context_value = ContextUsage::new(used.clamp(0.0, 100.0))
            .ok()?
            .with_cache(cache);
        context = Some(context_value);
    }
    Some(RolloutObservation { model, context })
}

fn parse_rollout_timestamp(entry: &Value) -> Option<u64> {
    entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(ResetAt::parse_rfc3339)
        .map(ResetAt::unix_seconds)
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
        assert_eq!(cache.ttl_seconds, Some(CODEX_PROMPT_CACHE_TTL_SECONDS));
        assert_eq!(cache.last_activity_unix, Some(1_787_711_322));
        assert_eq!(cache.expires_at_unix, None);
    }

    #[test]
    fn rollout_cache_without_a_timestamp_has_no_ttl_estimate() {
        let observation = parse_rollout_observation(
            &format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "type": "event_msg",
                    "payload": {
                        "type": "token_count",
                        "info": {
                            "last_token_usage": {"total_tokens": 50_000},
                            "total_token_usage": {
                                "input_tokens": 1_000,
                                "cached_input_tokens": 800
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
        let cache = observation.context.unwrap().cache.unwrap();
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
        );
        assert_eq!(snapshot.model.as_deref(), Some("gpt-5.6"));
        assert!(snapshot.session_contexts.contains_key("session-1"));
        assert!(!snapshot.session_contexts.contains_key("other-session"));
    }

    fn write_session_meta(home: &Path, day: &str, stamp: &str, id: &str, cwd: &str, at: &str) {
        let rollouts = home.join("sessions").join(day);
        fs::create_dir_all(&rollouts).unwrap();
        fs::write(
            rollouts.join(format!("rollout-{stamp}-{id}.jsonl")),
            serde_json::json!({"type":"session_meta", "timestamp": at,
                "payload":{"id":id,"cwd":cwd,"timestamp":at,"originator":"codex-tui"}})
            .to_string()
                + "\n",
        )
        .unwrap();
    }

    #[test]
    fn resolves_a_reused_cwd_only_when_the_process_start_matches_one_rollout() {
        let directory = tempfile::tempdir().unwrap();
        for (id, stamp, at) in [
            ("old", "2026-09-22T07-01-40", "2026-09-22T13:01:40Z"),
            ("live", "2026-09-22T07-16-40", "2026-09-22T13:16:40Z"),
        ] {
            write_session_meta(
                directory.path(),
                "2026/09/22",
                stamp,
                id,
                "/treehouse/reused",
                at,
            );
        }
        let resolved = session_ids_for_panes_at(
            directory.path(),
            // 2026-09-22T13:16:42Z
            &[(
                "w1:p1".to_string(),
                "/treehouse/reused".to_string(),
                1_790_083_002,
            )],
        );
        assert_eq!(resolved.get("w1:p1").map(String::as_str), Some("live"));
    }

    /// Sanitized from two Firstmate workers relaunched into reused treehouse
    /// worktrees (Herdr panes with hooks disabled, so no agent_session). Each
    /// cwd has an older rollout from the previous worker; only the process
    /// start picks the live one. The third pane starts after UTC midnight
    /// while Codex filed it under the previous local day.
    #[test]
    fn relaunched_workers_in_reused_worktrees_bind_their_own_rollouts() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path();
        let hermes = "/treehouse/ralph-hermes-86f4b1/1/ralph-hermes";
        let ember = "/treehouse/eaves-and-ember-717502/1/eaves-and-ember";
        for (stamp, id, cwd, at) in [
            (
                "2026-09-22T10-13-09",
                "01a0c9e4-7d2e-7420-a351-4edbc4bbda48",
                hermes,
                "2026-09-22T16:13:10.017Z",
            ),
            (
                "2026-09-22T11-36-55",
                "01a0ca31-2d7b-7863-8d3c-58bf5cf4c566",
                hermes,
                "2026-09-22T17:36:55.920Z",
            ),
            (
                "2026-09-22T11-10-32",
                "01a0ca19-06ee-7910-bcc4-cde13c5a5312",
                ember,
                "2026-09-22T17:10:33.180Z",
            ),
            (
                "2026-09-22T11-37-08",
                "01a0ca31-5e0a-7040-bdc6-28bfc15690e1",
                ember,
                "2026-09-22T17:37:08.339Z",
            ),
            (
                "2026-09-22T21-30-00",
                "late-local-evening",
                "/treehouse/late",
                "2026-09-23T03:30:00.500Z",
            ),
        ] {
            write_session_meta(home, "2026/09/22", stamp, id, cwd, at);
        }
        let resolved = session_ids_for_panes_at(
            home,
            &[
                // ps start 2026-09-22T17:36:55Z
                ("w28:p2".to_string(), hermes.to_string(), 1_790_098_615),
                // ps start 2026-09-22T17:37:07Z
                ("w29:p2".to_string(), ember.to_string(), 1_790_098_627),
                // ps start 2026-09-23T03:30:00Z
                (
                    "w30:p1".to_string(),
                    "/treehouse/late".to_string(),
                    1_790_134_200,
                ),
            ],
        );
        assert_eq!(
            resolved.get("w28:p2").map(String::as_str),
            Some("01a0ca31-2d7b-7863-8d3c-58bf5cf4c566")
        );
        assert_eq!(
            resolved.get("w29:p2").map(String::as_str),
            Some("01a0ca31-5e0a-7040-bdc6-28bfc15690e1")
        );
        assert_eq!(
            resolved.get("w30:p1").map(String::as_str),
            Some("late-local-evening")
        );
    }

    #[test]
    fn a_herdr_server_path_without_codex_falls_back_to_an_install_directory() {
        let directory = tempfile::tempdir().unwrap();
        let system = directory.path().join("usr-bin");
        let homebrew = directory.path().join("homebrew-bin");
        fs::create_dir_all(&system).unwrap();
        fs::create_dir_all(&homebrew).unwrap();
        fs::write(homebrew.join("codex"), "").unwrap();
        let fallbacks = [directory.path().join("absent"), homebrew.clone()];

        let (executable, prepended) =
            resolve_codex_executable(None, Some(system.as_os_str()), &fallbacks);
        assert_eq!(executable, homebrew.join("codex").into_os_string());
        assert_eq!(prepended, Some(homebrew.clone()));

        // Codex already on PATH, or an explicit override, is used as given.
        let (executable, prepended) =
            resolve_codex_executable(None, Some(homebrew.as_os_str()), &fallbacks);
        assert_eq!(executable, std::ffi::OsString::from("codex"));
        assert_eq!(prepended, None);
        let (executable, prepended) = resolve_codex_executable(
            Some("/custom/codex".into()),
            Some(system.as_os_str()),
            &fallbacks,
        );
        assert_eq!(executable, std::ffi::OsString::from("/custom/codex"));
        assert_eq!(prepended, Some(PathBuf::from("/custom")));
    }

    #[test]
    fn an_explicit_codex_bin_path_prepends_its_directory_to_the_child_path() {
        let directory = tempfile::tempdir().unwrap();
        let shim_dir = directory.path().join("shims");
        fs::create_dir_all(&shim_dir).unwrap();
        fs::write(shim_dir.join("codex"), "").unwrap();
        let configured = shim_dir.join("codex").into_os_string();

        // $CODEX_BIN_PATH pointing at a shim: use the override as-is and
        // prepend its directory so an `env node` shim resolves node under
        // Herdr's minimal server PATH.
        let (executable, prepended) = resolve_codex_executable(
            Some(configured.clone()),
            Some(directory.path().join("absent").as_os_str()),
            &[],
        );
        assert_eq!(executable, configured);
        assert_eq!(prepended, Some(shim_dir.clone()));

        // A bare name without a directory component is resolved against the
        // inherited PATH; there is no directory to prepend.
        let (executable, prepended) = resolve_codex_executable(
            Some(std::ffi::OsString::from("codex")),
            Some(directory.path().join("absent").as_os_str()),
            &[],
        );
        assert_eq!(executable, std::ffi::OsString::from("codex"));
        assert_eq!(prepended, None);

        // Unset: unchanged - no directory is prepended.
        let (executable, prepended) =
            resolve_codex_executable(None, Some(shim_dir.as_os_str()), &[]);
        assert_eq!(executable, std::ffi::OsString::from("codex"));
        assert_eq!(prepended, None);
    }

    #[test]
    fn two_rollouts_near_one_process_start_stay_unresolved() {
        let directory = tempfile::tempdir().unwrap();
        for (id, stamp, at) in [
            ("first", "2026-09-22T11-36-55", "2026-09-22T17:36:55Z"),
            ("second", "2026-09-22T11-37-20", "2026-09-22T17:37:20Z"),
        ] {
            write_session_meta(directory.path(), "2026/09/22", stamp, id, "/shared", at);
        }
        let resolved = session_ids_for_panes_at(
            directory.path(),
            &[("w1:p1".to_string(), "/shared".to_string(), 1_790_098_615)],
        );
        assert!(resolved.is_empty());
    }

    #[test]
    fn unattributed_rollout_cannot_add_a_window_to_the_current_accounts_quota() {
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
        snapshot.account_id = Some("new-account".to_string());
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["session-1".to_string()]);
        // A file written after login may still be an old account's live
        // session. Even an identical reset timestamp does not prove identity.
        assert!(snapshot.window(WindowKind::FiveHour).is_none());
        assert!(snapshot.session_contexts.contains_key("session-1"));
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
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["session-1".to_string()]);
        assert!(snapshot.window(WindowKind::FiveHour).is_none());
        assert_eq!(
            snapshot.window(WindowKind::Weekly).unwrap().used_percent,
            0.0
        );
    }

    #[test]
    fn rollout_five_hour_window_is_not_used_without_account_identity() {
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
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["session-1".to_string()]);
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
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["session-1".to_string()]);
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

    fn token_count_pad_line() -> String {
        serde_json::to_string(&json!({
            "type": "event_msg",
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
            + "\n"
    }

    fn pad_jsonl(body: &mut String, min_len: usize, pad_line: &str) {
        while body.len() < min_len {
            body.push_str(pad_line);
        }
    }

    fn write_padded_rollout(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    /// A long Codex turn writes `turn_context` at the start, then enough
    /// `token_count` lines to push that model out of the 256 KB tail. The
    /// session-start model is still in the first 256 KB. Publishing the head
    /// fallback is the stale-sidebar bug: context/cache keep moving, model does
    /// not.
    #[test]
    fn latest_turn_context_beyond_the_tail_wins_over_the_session_start_model() {
        let directory = tempfile::tempdir().unwrap();
        let pad = token_count_pad_line();
        let mut body = String::new();
        body.push_str("{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-6-astra\"}}\n");
        pad_jsonl(&mut body, ROLLOUT_TAIL_BYTES as usize + pad.len(), &pad);
        body.push_str("{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.6-sol\"}}\n");
        let tail_floor = body.len() + ROLLOUT_TAIL_BYTES as usize + pad.len();
        pad_jsonl(&mut body, tail_floor, &pad);
        write_padded_rollout(
            &directory
                .path()
                .join("sessions/2026/09/15/rollout-session-1.jsonl"),
            &body,
        );

        let mut snapshot = ProviderSnapshot::new(Provider::Codex, vec![], 1);
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["session-1".to_string()]);
        assert_eq!(
            snapshot.session_models.get("session-1").map(String::as_str),
            Some("gpt-5.6-sol")
        );
        assert_eq!(snapshot.model.as_deref(), Some("gpt-5.6-sol"));
        assert!(snapshot.session_contexts.contains_key("session-1"));
    }

    #[test]
    fn session_start_model_is_kept_when_it_is_still_the_latest_turn_context() {
        let directory = tempfile::tempdir().unwrap();
        let pad = token_count_pad_line();
        let mut body = String::new();
        body.push_str("{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-6-astra\"}}\n");
        pad_jsonl(&mut body, ROLLOUT_TAIL_BYTES as usize + pad.len(), &pad);
        write_padded_rollout(
            &directory
                .path()
                .join("sessions/2026/09/15/rollout-session-1.jsonl"),
            &body,
        );

        let mut snapshot = ProviderSnapshot::new(Provider::Codex, vec![], 1);
        enrich_local_sessions_at(&mut snapshot, directory.path(), &["session-1".to_string()]);
        assert_eq!(
            snapshot.session_models.get("session-1").map(String::as_str),
            Some("gpt-6-astra")
        );
    }

    fn compressed_rollout(path: &Path) -> Vec<u8> {
        let bytes = zstd::stream::encode_all(
            include_bytes!("../../tests/fixtures/codex/rollout-prefix.jsonl").as_slice(),
            0,
        )
        .unwrap();
        fs::write(path, &bytes).unwrap();
        bytes
    }

    fn set_modified(path: &Path, seconds: u64) {
        fs::File::open(path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(seconds))
            .unwrap();
    }

    #[test]
    fn compressed_rollout_is_discovered_and_binds_its_session() {
        let directory = tempfile::tempdir().unwrap();
        let day = directory.path().join("sessions/2026/09/22");
        fs::create_dir_all(&day).unwrap();
        let compressed = day.join("rollout-2026-09-22T07-01-40-session-compressed.jsonl.zst");
        compressed_rollout(&compressed);
        fs::write(day.join("rollout-other.jsonl"), b"\n").unwrap();

        assert_eq!(
            session_ids_for_panes_at(
                directory.path(),
                &[("pane-1".into(), "/workspace".into(), 1_790_082_100)],
            )
            .get("pane-1")
            .map(String::as_str),
            Some("session-compressed")
        );
        assert_eq!(
            find_rollout_paths(directory.path(), &["session-compressed".into()])
                .get("session-compressed"),
            Some(&compressed)
        );
    }

    #[test]
    fn newest_rollout_wins_across_plain_and_compressed_files() {
        let directory = tempfile::tempdir().unwrap();
        let day = directory.path().join("sessions/2026/09/22");
        fs::create_dir_all(&day).unwrap();
        let plain = day.join("rollout-session-compressed.jsonl");
        fs::write(
            &plain,
            b"{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.6-terra\"}}\n",
        )
        .unwrap();
        let compressed = day.join("rollout-session-compressed.jsonl.zst");
        compressed_rollout(&compressed);

        set_modified(&plain, 100);
        set_modified(&compressed, 200);
        let mut snapshot = ProviderSnapshot::new(Provider::Codex, vec![], 1);
        enrich_local_sessions_at(
            &mut snapshot,
            directory.path(),
            &["session-compressed".into()],
        );
        assert_eq!(snapshot.model.as_deref(), Some("gpt-6-astra"));
        assert!((snapshot.context.unwrap().used_percent - 43.1818).abs() < 0.001);

        set_modified(&plain, 300);
        assert_eq!(
            find_rollout_paths(directory.path(), &["session-compressed".into()])
                .get("session-compressed"),
            Some(&plain)
        );
    }

    #[test]
    fn damaged_compressed_rollout_falls_back_like_unreadable_plain_rollout() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("rollout-session-compressed.jsonl.zst");
        let bytes = compressed_rollout(&path);
        fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        assert!(read_rollout_observation(&path, "session-compressed").is_none());
        fs::write(&path, b"not a zstd frame").unwrap();
        assert!(read_rollout_observation(&path, "session-compressed").is_none());
        assert!(read_rollout_observation(
            &directory.path().join("unreadable.jsonl"),
            "session-compressed"
        )
        .is_none());
    }

    #[test]
    fn compressed_reader_stops_at_the_decoded_prefix_budget() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("rollout-session-compressed.jsonl.zst");
        let mut source =
            include_str!("../../tests/fixtures/codex/rollout-prefix.jsonl").to_string();
        let filler = "{\"type\":\"event_msg\",\"payload\":{\"type\":\"ignored\"}}\n";
        while source.len() < ROLLOUT_COMPRESSED_PREFIX_BYTES as usize {
            source.push_str(filler);
        }
        source.push_str("{\"type\":\"turn_context\",\"payload\":{\"model\":\"later-model\"}}\n");
        fs::write(
            &path,
            zstd::stream::encode_all(source.as_bytes(), 0).unwrap(),
        )
        .unwrap();

        let prefix = read_compressed_prefix(&path).unwrap();
        assert_eq!(prefix.len(), ROLLOUT_COMPRESSED_PREFIX_BYTES as usize);
        let observation = read_rollout_observation(&path, "session-compressed").unwrap();
        assert_eq!(observation.model.as_deref(), Some("gpt-6-astra"));
        assert!(observation.context.is_some());
    }
}
