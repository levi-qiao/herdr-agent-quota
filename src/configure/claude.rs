use super::statusline::{settings_path, Adapter};
use crate::cache::{CacheStore, DEFAULT_WATCH_INTERVAL_SECONDS};
use crate::model::Provider;
use crate::presentation::pace_segment;
use crate::providers::claude::parse_statusline;
use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::Path;

const CONFIG: Adapter = Adapter {
    label: "Claude",
    subcommand: "claude-statusline",
    backup_file: "claude-statusline.original.json",
};

pub fn check() -> Result<()> {
    CONFIG.check(&settings_path(
        "CLAUDE_SETTINGS_FILE",
        ".claude/settings.json",
    )?)
}

pub fn apply() -> Result<()> {
    let cache = CacheStore::from_env()?;
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    apply_at_with_refresh_interval(
        &settings_path("CLAUDE_SETTINGS_FILE", ".claude/settings.json")?,
        cache.root(),
        &executable,
        cache.watch_interval_seconds(),
    )
}

pub fn apply_with_refresh_interval(refresh_interval_seconds: u64) -> Result<()> {
    let cache = CacheStore::from_env()?;
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    apply_at_with_refresh_interval(
        &settings_path("CLAUDE_SETTINGS_FILE", ".claude/settings.json")?,
        cache.root(),
        &executable,
        refresh_interval_seconds,
    )
}

pub fn uninstall() -> Result<()> {
    let cache = CacheStore::from_env()?;
    uninstall_at(
        &settings_path("CLAUDE_SETTINGS_FILE", ".claude/settings.json")?,
        cache.root(),
    )
}

pub fn apply_at(settings: &Path, state: &Path, executable: &Path) -> Result<()> {
    apply_at_with_refresh_interval(settings, state, executable, DEFAULT_WATCH_INTERVAL_SECONDS)
}

pub fn apply_at_with_refresh_interval(
    settings: &Path,
    state: &Path,
    executable: &Path,
    refresh_interval_seconds: u64,
) -> Result<()> {
    CONFIG.apply_with_refresh_interval(settings, state, executable, Some(refresh_interval_seconds))
}

pub fn uninstall_at(settings: &Path, state: &Path) -> Result<()> {
    CONFIG.uninstall(settings, state)
}

pub fn run_statusline_hook() -> Result<()> {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let mut pace = None;
    if let Ok(value) = serde_json::from_slice::<Value>(&input) {
        let now_unix = CacheStore::now_unix();
        if let Ok(snapshot) = parse_statusline(&value, now_unix) {
            pace = pace_segment(&snapshot.windows, now_unix);
            if let Ok(cache) = CacheStore::from_env() {
                let _ = cache.save_statusline_observation(Provider::Claude, snapshot, &value);
                // Herdr's own SessionStart integration may be absent, outdated,
                // or unwired into `settings.json`, leaving `agent_session`
                // unset for this pane. Self-report the pane -> session mapping
                // from the same env var Herdr's own hook reads, so the pane can
                // resolve its session without depending on that integration.
                if let (Some(pane_id), Some(session_id)) = (
                    std::env::var("HERDR_PANE_ID").ok(),
                    value.get("session_id").and_then(Value::as_str),
                ) {
                    let _ = cache.save_pane_session(Provider::Claude, &pane_id, session_id);
                }
            }
        }
    }
    let cache = CacheStore::from_env()?;
    let Some(output) = CONFIG.run_previous(cache.root(), &input)? else {
        if let Some(pace) = pace {
            println!("{pace}");
        }
        return Ok(());
    };
    if output.timed_out {
        return Ok(());
    }
    let stdout = if output.exit_code == Some(0) {
        append_pace(output.stdout, pace.as_deref())
    } else {
        output.stdout
    };
    std::io::stdout().write_all(&stdout)?;
    std::io::stdout().flush()?;
    if output.exit_code != Some(0) {
        std::process::exit(output.exit_code.unwrap_or(1));
    }
    Ok(())
}

/// Add the pace to the end of the wrapped command's last line so the status
/// line keeps whatever layout the user's own script produced.
fn append_pace(mut stdout: Vec<u8>, pace: Option<&str>) -> Vec<u8> {
    let Some(pace) = pace else {
        return stdout;
    };
    let newline = stdout.ends_with(b"\n");
    while stdout.last() == Some(&b'\n') {
        stdout.pop();
    }
    if !stdout.is_empty() {
        stdout.push(b' ');
    }
    stdout.extend_from_slice(pace.as_bytes());
    if newline {
        stdout.push(b'\n');
    }
    stdout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pace_joins_the_last_status_line_and_keeps_the_trailing_newline() {
        assert_eq!(
            append_pace(b"a\nb\n".to_vec(), Some("⏱ 5h =")),
            "a\nb ⏱ 5h =\n".as_bytes()
        );
        assert_eq!(append_pace(b"a".to_vec(), Some("x")), b"a x");
        assert_eq!(append_pace(b"".to_vec(), Some("x")), b"x");
        assert_eq!(append_pace(b"a\n".to_vec(), None), b"a\n");
    }
}
