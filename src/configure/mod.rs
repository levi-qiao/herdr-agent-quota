pub mod agy;
pub mod claude;
pub mod grok;
pub mod herdr;
mod integration;
mod statusline;

use crate::cache::CacheStore;
use crate::cli::AgentSelection;
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
        herdr::apply(agents)?;
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
        herdr::check(agents)?;
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
