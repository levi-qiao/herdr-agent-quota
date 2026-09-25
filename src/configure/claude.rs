use super::statusline::Adapter;
use crate::cache::{CacheStore, DEFAULT_WATCH_INTERVAL_SECONDS};
use crate::model::Provider;
use crate::presentation::pace_segment;
use crate::providers::claude::parse_statusline;
use crate::providers::statusline::api_generation;
use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const CONFIG: Adapter = Adapter {
    label: "Claude",
    subcommand: "claude-statusline",
    backup_file: "claude-statusline.original.json",
};

fn claude_settings_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CLAUDE_SETTINGS_FILE") {
        return Ok(PathBuf::from(path));
    }
    if let Some(directory) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Ok(PathBuf::from(directory).join("settings.json"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".claude/settings.json"))
}

pub fn check() -> Result<()> {
    let cache = CacheStore::from_env()?;
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    CONFIG.check(&claude_settings_path()?, cache.root(), &executable)
}

pub fn apply() -> Result<()> {
    let cache = CacheStore::from_env()?;
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    apply_at_with_refresh_interval(
        &claude_settings_path()?,
        cache.root(),
        &executable,
        cache.watch_interval_seconds(),
    )
}

pub fn apply_with_refresh_interval(refresh_interval_seconds: u64) -> Result<()> {
    let cache = CacheStore::from_env()?;
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    apply_at_with_refresh_interval(
        &claude_settings_path()?,
        cache.root(),
        &executable,
        refresh_interval_seconds,
    )
}

pub fn uninstall() -> Result<()> {
    let cache = CacheStore::from_env()?;
    uninstall_at(&claude_settings_path()?, cache.root())
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
    let cache = CacheStore::from_env()?;
    let pace_enabled = super::resolved_statusline_pace(None, Some(&cache)).is_on();
    let mut pace = None;
    if let Ok(value) = serde_json::from_slice::<Value>(&input) {
        let now_unix = CacheStore::now_unix();
        if let Ok(snapshot) = parse_statusline(&value, now_unix) {
            if pace_enabled {
                pace = pace_segment(&snapshot.windows, now_unix);
            }
            let generation = api_generation(&value);
            let _ = cache.save_statusline_observation_with_api_generation(
                Provider::Claude,
                snapshot,
                &value,
                generation.as_deref(),
            );
        }
    }
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
