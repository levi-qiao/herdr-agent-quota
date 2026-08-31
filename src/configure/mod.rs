pub mod agy;
pub mod claude;
pub mod grok;
pub mod herdr;
mod integration;
mod statusline;

use crate::cache::CacheStore;
use crate::cli::{
    AgentSelection, BrandColors, ConfigureOptions, FieldSet, PercentStyle, SidebarLayout,
    SidebarRowGap,
};
use crate::model::Harness;
use crate::prefs;
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
    options: ConfigureOptions,
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
        // The rows on disk were written from these settings, so uninstall
        // needs them to recognise its own work and restore the backup.
        let fields = resolved_fields(None, Some(&cache));
        let brand = resolved_brand_colors(None, Some(&cache));
        herdr::uninstall(agents, full, fields, brand)?;
        if full {
            cache.clear_watch_interval()?;
            cache.clear_sidebar_layout()?;
            cache.clear_row_gap()?;
            cache.clear_percent_style()?;
            cache.clear_fields()?;
            cache.clear_brand_colors()?;
            for name in prefs::ALL {
                prefs::clear(name)?;
            }
        }
    } else if apply {
        let cache = CacheStore::from_env()?;
        cache.clear_turn_watcher_stop()?;
        let interval = options
            .watch_interval_seconds
            .or_else(|| {
                std::env::var("HERDR_AGENT_QUOTA_WATCH_INTERVAL_SECONDS")
                    .ok()
                    .and_then(|value| value.parse().ok())
            })
            .or_else(|| {
                prefs::read(prefs::WATCH_INTERVAL_SECONDS).and_then(|value| value.parse().ok())
            });
        let interval = if let Some(interval) = interval {
            cache.set_watch_interval_seconds(interval)?;
            interval
        } else {
            cache.watch_interval_seconds()
        };
        let layout = resolved_sidebar_layout(options.sidebar_layout, Some(&cache));
        cache.set_sidebar_layout(layout)?;
        prefs::write(prefs::SIDEBAR_LAYOUT, layout.as_str())?;
        let gap = resolved_row_gap(options.row_gap, Some(&cache));
        cache.set_row_gap(gap)?;
        prefs::write(prefs::ROW_GAP, &gap.to_string())?;
        // Rendering reads this at publish time, not install time, so the row
        // layout is untouched: only the number inside a quota token changes.
        let percent = resolved_percent_style(options.quota_percent, Some(&cache));
        cache.set_percent_style(percent)?;
        prefs::write(prefs::QUOTA_PERCENT, percent.as_str())?;
        println!("Quota percentages show {} quota.", percent.suffix());
        let fields = resolved_fields(options.fields, Some(&cache));
        cache.set_fields(fields)?;
        prefs::write(prefs::FIELDS, &fields.as_list())?;
        let brand = resolved_brand_colors(options.brand_colors, Some(&cache));
        cache.set_brand_colors(brand)?;
        prefs::write(prefs::BRAND_COLORS, brand.as_str())?;
        herdr::apply(agents, layout, gap, fields, brand)?;
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
        let layout = resolved_sidebar_layout(options.sidebar_layout, cache.as_ref());
        let gap = resolved_row_gap(options.row_gap, cache.as_ref());
        let percent = resolved_percent_style(options.quota_percent, cache.as_ref());
        let fields = resolved_fields(options.fields, cache.as_ref());
        let brand = resolved_brand_colors(options.brand_colors, cache.as_ref());
        herdr::check(agents, layout, gap, fields, brand)?;
        println!("Quota percentages show {} quota.", percent.suffix());
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

pub(crate) fn resolved_sidebar_layout(
    explicit: Option<SidebarLayout>,
    cache: Option<&CacheStore>,
) -> SidebarLayout {
    SidebarLayout::from_arg_or_env(explicit)
        .or_else(|| {
            prefs::read(prefs::SIDEBAR_LAYOUT).and_then(|value| SidebarLayout::parse(&value))
        })
        .or_else(|| cache.and_then(CacheStore::sidebar_layout))
        .unwrap_or_default()
}

pub(crate) fn resolved_percent_style(
    explicit: Option<PercentStyle>,
    cache: Option<&CacheStore>,
) -> PercentStyle {
    PercentStyle::from_arg_or_env(explicit)
        .or_else(|| prefs::read(prefs::QUOTA_PERCENT).and_then(|value| PercentStyle::parse(&value)))
        .or_else(|| cache.and_then(CacheStore::percent_style))
        .unwrap_or_default()
}

pub(crate) fn resolved_fields(explicit: Option<FieldSet>, cache: Option<&CacheStore>) -> FieldSet {
    FieldSet::from_arg_or_env(explicit)
        .or_else(|| prefs::read(prefs::FIELDS).and_then(|value| FieldSet::parse(&value)))
        .or_else(|| cache.and_then(CacheStore::fields))
        .unwrap_or_default()
}

pub(crate) fn resolved_brand_colors(
    explicit: Option<BrandColors>,
    cache: Option<&CacheStore>,
) -> BrandColors {
    BrandColors::from_arg_or_env(explicit)
        .or_else(|| prefs::read(prefs::BRAND_COLORS).and_then(|value| BrandColors::parse(&value)))
        .or_else(|| cache.and_then(CacheStore::brand_colors))
        .unwrap_or_default()
}

pub(crate) fn resolved_row_gap(
    explicit: Option<SidebarRowGap>,
    cache: Option<&CacheStore>,
) -> SidebarRowGap {
    SidebarRowGap::from_arg_or_env(explicit)
        .or_else(|| prefs::read(prefs::ROW_GAP).and_then(|value| SidebarRowGap::parse(&value)))
        .or_else(|| cache.and_then(CacheStore::row_gap))
        .unwrap_or_default()
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
