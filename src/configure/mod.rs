pub mod agy;
pub mod claude;
pub mod grok;
pub mod herdr;
mod integration;
mod statusline;

use crate::cache::CacheStore;
use crate::cli::{AgentSelection, SidebarLayout, SidebarRowGap};
use crate::model::Harness;
use anyhow::{Context, Result};

/// Is this selection every agent `configure` supports?
///
/// Only a full run may touch shared state — the watcher, the poll interval,
/// and the config backups all belong to the installation as a whole, not to
/// any one agent. A narrower `--agent` run leaves them alone so removing one
/// agent never disturbs the others.
fn is_full(agents: &[Harness]) -> bool {
    AgentSelection::SUPPORTED
        .iter()
        .all(|harness| agents.contains(harness))
}

/// `--check` is also the no-flag default, so it needs no branch of its own.
pub fn run(
    _check: bool,
    apply: bool,
    uninstall: bool,
    agents: &[Harness],
    watch_interval_seconds: Option<u64>,
    sidebar_layout: Option<SidebarLayout>,
    row_gap: Option<SidebarRowGap>,
) -> Result<()> {
    if apply || uninstall {
        std::env::var_os("HERDR_PLUGIN_STATE_DIR").context(
            "configuration writes must run through Herdr so every collector uses the same cache; invoke herdr-agent-quota.configure or herdr-agent-quota.uninstall",
        )?;
    }
    if agents.is_empty() {
        println!("No supported agent selected; nothing to do.");
        return Ok(());
    }
    let full = is_full(agents);

    if uninstall {
        let cache = CacheStore::from_env()?;
        if full {
            cache.stop_turn_watchers()?;
        }
        if agents.contains(&Harness::Grok) {
            grok::uninstall()?;
        }
        if agents.contains(&Harness::Agy) {
            agy::uninstall()?;
        }
        if agents.contains(&Harness::Claude) {
            claude::uninstall()?;
        }
        herdr::uninstall(agents, full)?;
        if full {
            cache.clear_watch_interval()?;
            cache.clear_sidebar_layout()?;
            cache.clear_row_gap()?;
            clear_plugin_pref("sidebar-layout")?;
            clear_plugin_pref("row-gap")?;
        }
    } else if apply {
        let cache = CacheStore::from_env()?;
        cache.clear_turn_watcher_stop()?;
        let interval = watch_interval_seconds.or_else(|| {
            std::env::var("HERDR_AGENT_QUOTA_WATCH_INTERVAL_SECONDS")
                .ok()
                .and_then(|value| value.parse().ok())
        });
        let interval = if let Some(interval) = interval {
            cache.set_watch_interval_seconds(interval)?;
            interval
        } else {
            cache.watch_interval_seconds()
        };
        let layout = resolve_sidebar_layout(sidebar_layout, Some(&cache));
        cache.set_sidebar_layout(layout)?;
        write_plugin_pref("sidebar-layout", layout.as_str())?;
        let gap = resolve_row_gap(row_gap, Some(&cache));
        cache.set_row_gap(gap)?;
        write_plugin_pref("row-gap", &gap.to_string())?;
        herdr::apply(agents, layout, gap)?;
        if agents.contains(&Harness::Claude) {
            claude::apply_with_refresh_interval(interval)?;
        }
        if agents.contains(&Harness::Agy) {
            agy::apply()?;
        }
        if agents.contains(&Harness::Grok) {
            grok::apply()?;
        }
        integration::report_missing(agents);
    } else {
        let cache = CacheStore::from_env().ok();
        let layout = resolve_sidebar_layout(sidebar_layout, cache.as_ref());
        let gap = resolve_row_gap(row_gap, cache.as_ref());
        herdr::check(agents, layout, gap)?;
        if agents.contains(&Harness::Claude) {
            claude::check()?;
        }
        if agents.contains(&Harness::Agy) {
            agy::check()?;
        }
        if agents.contains(&Harness::Grok) {
            grok::check()?;
        }
        integration::report_missing(agents);
    }
    Ok(())
}

fn resolve_sidebar_layout(
    explicit: Option<SidebarLayout>,
    cache: Option<&CacheStore>,
) -> SidebarLayout {
    SidebarLayout::from_arg_or_env(explicit)
        .or_else(|| plugin_pref("sidebar-layout").and_then(|value| SidebarLayout::parse(&value)))
        .or_else(|| cache.and_then(CacheStore::sidebar_layout))
        .unwrap_or_default()
}

fn resolve_row_gap(explicit: Option<SidebarRowGap>, cache: Option<&CacheStore>) -> SidebarRowGap {
    SidebarRowGap::from_arg_or_env(explicit)
        .or_else(|| plugin_pref("row-gap").and_then(|value| SidebarRowGap::parse(&value)))
        .or_else(|| cache.and_then(CacheStore::row_gap))
        .unwrap_or_default()
}

fn plugin_pref(name: &str) -> Option<String> {
    let directory = std::env::var_os("HERDR_PLUGIN_CONFIG_DIR")?;
    let value = std::fs::read_to_string(std::path::PathBuf::from(directory).join(name)).ok()?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn write_plugin_pref(name: &str, value: &str) -> Result<()> {
    let Some(directory) = std::env::var_os("HERDR_PLUGIN_CONFIG_DIR") else {
        return Ok(());
    };
    let path = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&path)
        .with_context(|| format!("create plugin config directory {}", path.display()))?;
    std::fs::write(path.join(name), value).with_context(|| format!("write plugin pref {name}"))
}

fn clear_plugin_pref(name: &str) -> Result<()> {
    let Some(directory) = std::env::var_os("HERDR_PLUGIN_CONFIG_DIR") else {
        return Ok(());
    };
    match std::fs::remove_file(std::path::PathBuf::from(directory).join(name)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove plugin pref {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_complete_selection_may_touch_shared_state() {
        assert!(is_full(&AgentSelection::SUPPORTED));
        assert!(!is_full(&[Harness::Grok]));
        assert!(!is_full(&[
            Harness::Claude,
            Harness::Codex,
            Harness::Grok,
            Harness::Agy
        ]));
        assert!(!is_full(&[]));
    }
}
