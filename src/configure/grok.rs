use crate::cache::CacheStore;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const HOOK_FILE: &str = "herdr-agent-quota.json";
const LEGACY_REFRESH_ACTION: &str = "herdr-agent-quota.refresh-grok";
const MANAGED_BY: &str = "herdr-agent-quota";

pub fn check() -> Result<()> {
    let path = hook_path()?;
    if is_managed_hook(&path) {
        println!(
            "Legacy Grok quota hook will be removed; the unified active-turn watcher handles refreshes: {}",
            path.display()
        );
    } else {
        println!(
            "No Grok response hook is needed; the unified active-turn watcher handles {}",
            path.display()
        );
    }
    Ok(())
}

pub fn apply() -> Result<()> {
    let cache = CacheStore::from_env()?;
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    apply_at(&hook_path()?, cache.root(), &executable)
}

pub fn uninstall() -> Result<()> {
    uninstall_at(&hook_path()?)
}

pub fn apply_at(path: &Path, _state: &Path, _executable: &Path) -> Result<()> {
    // The unified watcher replaces the old per-tool Grok hook. Only remove a
    // file that this plugin owns; a user's unrelated hook is never touched.
    if is_managed_hook(path) {
        fs::remove_file(path).context("remove legacy Grok quota hook")?;
        println!("Removed legacy Grok quota hook from {}", path.display());
    }
    Ok(())
}

pub fn uninstall_at(path: &Path) -> Result<()> {
    if is_managed_hook(path) {
        fs::remove_file(path).context("remove Grok quota hook")?;
        println!("Removed Grok quota hook from {}", path.display());
    }
    Ok(())
}

fn hook_path() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("GROK_HOME") {
        return Ok(PathBuf::from(home).join("hooks").join(HOOK_FILE));
    }
    let home = crate::home_dir()?;
    Ok(home.join(".grok/hooks").join(HOOK_FILE))
}

fn is_managed_hook(path: &Path) -> bool {
    fs::read_to_string(path).is_ok_and(|contents| {
        contents.contains(LEGACY_REFRESH_ACTION)
            || (contents.contains(MANAGED_BY) && contents.contains("refresh --provider grok"))
    })
}
