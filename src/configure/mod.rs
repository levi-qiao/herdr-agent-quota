pub mod agy;
pub mod claude;
pub mod grok;
pub mod herdr;
mod integration;
mod statusline;

use crate::cache::CacheStore;
use crate::cli::{
    AgentOrder, AgentSelection, BrandColors, ConfigureOptions, FieldSet, LowQuotaAlert,
    PercentStyle, SidebarLayout, SidebarRowGap,
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
            // Herdr keeps this view until something clears it, so an uninstall
            // that skipped it would leave the panel sorted by a token this
            // plugin no longer publishes.
            apply_agent_order(AgentOrder::Default);
            cache.clear_watch_interval()?;
            cache.clear_sidebar_layout()?;
            cache.clear_row_gap()?;
            cache.clear_percent_style()?;
            cache.clear_fields()?;
            cache.clear_brand_colors()?;
            cache.clear_agent_order()?;
            cache.clear_low_quota_alert()?;
            for name in prefs::ALL {
                prefs::clear(name)?;
            }
        }
    } else if apply {
        integration::ensure_omp(agents, full)?;
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
        let alert = resolved_low_quota_alert(options.low_quota_alert, Some(&cache));
        // A new threshold has never warned about anything yet. Without this,
        // lowering it would stay silent for a provider already warned about at
        // the old one.
        if cache.low_quota_alert() != Some(alert) {
            cache.set_low_quota_alerted(&[])?;
        }
        cache.set_low_quota_alert(alert)?;
        prefs::write(prefs::LOW_QUOTA_ALERT, &alert.to_string())?;
        herdr::apply(agents, layout, gap, fields, brand)?;
        // Not gated on a full run, unlike the watcher: the Agent panel order
        // is a choice that arrives on this command line, and the settings pane
        // sends it alongside whatever agent selection the user happens to
        // have. Gating it would silently drop the setting for anyone not
        // running every supported agent.
        let order = resolved_agent_order(options.agent_order, Some(&cache));
        cache.set_agent_order(order)?;
        prefs::write(prefs::AGENT_ORDER, order.as_str())?;
        apply_agent_order(order);
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
        let order = resolved_agent_order(options.agent_order, cache.as_ref());
        let alert = resolved_low_quota_alert(options.low_quota_alert, cache.as_ref());
        herdr::check(agents, layout, gap, fields, brand)?;
        println!("Quota percentages show {} quota.", percent.suffix());
        println!("Agent panel order: {}.", order.as_str());
        println!("Low quota alert: {alert}.");
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

pub(crate) fn resolved_agent_order(
    explicit: Option<AgentOrder>,
    cache: Option<&CacheStore>,
) -> AgentOrder {
    AgentOrder::from_arg_or_env(explicit)
        .or_else(|| prefs::read(prefs::AGENT_ORDER).and_then(|value| AgentOrder::parse(&value)))
        .or_else(|| cache.and_then(CacheStore::agent_order))
        .unwrap_or_default()
}

pub(crate) fn resolved_low_quota_alert(
    explicit: Option<LowQuotaAlert>,
    cache: Option<&CacheStore>,
) -> LowQuotaAlert {
    LowQuotaAlert::from_arg_or_env(explicit)
        .or_else(|| {
            prefs::read(prefs::LOW_QUOTA_ALERT).and_then(|value| LowQuotaAlert::parse(&value))
        })
        .or_else(|| cache.and_then(CacheStore::low_quota_alert))
        .unwrap_or_default()
}

/// Hand Herdr the Agent view the user asked for, or give the panel back.
///
/// Never fatal. The rows and collectors are already written by the time this
/// runs, and a panel that kept its old ordering is a cosmetic disagreement —
/// failing the whole `--apply` over it would be worse than reporting it.
pub(crate) fn apply_agent_order(order: AgentOrder) {
    let result = if order.is_quota() {
        crate::herdr::set_quota_agent_view()
    } else {
        crate::herdr::clear_quota_agent_view()
    };
    if let Err(error) = result {
        println!("Could not set the Herdr agent order: {error}");
    }
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
