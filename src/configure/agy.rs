use super::statusline::{settings_path, Adapter};
use crate::cache::CacheStore;
use crate::model::Provider;
use crate::providers::agy::parse_statusline;
use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::Path;

const CONFIG: Adapter = Adapter {
    label: "Agy",
    subcommand: "agy-statusline",
    backup_file: "agy-statusline.original.json",
};

pub fn check() -> Result<()> {
    CONFIG.check(&settings_path(
        "AGY_SETTINGS_FILE",
        ".gemini/antigravity-cli/settings.json",
    )?)
}

pub fn apply() -> Result<()> {
    let cache = CacheStore::from_env()?;
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    apply_at(
        &settings_path("AGY_SETTINGS_FILE", ".gemini/antigravity-cli/settings.json")?,
        cache.root(),
        &executable,
    )
}

pub fn uninstall() -> Result<()> {
    let cache = CacheStore::from_env()?;
    uninstall_at(
        &settings_path("AGY_SETTINGS_FILE", ".gemini/antigravity-cli/settings.json")?,
        cache.root(),
    )
}

pub fn apply_at(settings: &Path, state: &Path, executable: &Path) -> Result<()> {
    CONFIG.apply(settings, state, executable)
}

pub fn uninstall_at(settings: &Path, state: &Path) -> Result<()> {
    CONFIG.uninstall(settings, state)
}

/// Consume one Agy statusLine payload, cache quota silently, then preserve a
/// user-owned statusLine command when one existed before installation.
pub fn run_statusline_hook() -> Result<()> {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    if let Ok(value) = serde_json::from_slice::<Value>(&input) {
        if let Ok(snapshot) = parse_statusline(&value, CacheStore::now_unix()) {
            if let Ok(cache) = CacheStore::from_env() {
                let _ = cache.save_statusline_observation(Provider::Agy, snapshot, &value);
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
