use super::statusline::{settings_path, Adapter};
use crate::cache::{CacheStore, DEFAULT_WATCH_INTERVAL_SECONDS};
use crate::model::Provider;
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
    if let Ok(value) = serde_json::from_slice::<Value>(&input) {
        if let Ok(snapshot) = parse_statusline(&value, CacheStore::now_unix()) {
            if let Ok(cache) = CacheStore::from_env() {
                let _ = cache.save_statusline_observation(Provider::Claude, snapshot, &value);
            }
        }
    }
    let cache = CacheStore::from_env()?;
    let Some(output) = CONFIG.run_previous(cache.root(), &input)? else {
        return Ok(());
    };
    if output.timed_out {
        return Ok(());
    }
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stdout().flush()?;
    if output.exit_code != Some(0) {
        std::process::exit(output.exit_code.unwrap_or(1));
    }
    Ok(())
}
