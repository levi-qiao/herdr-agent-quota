use crate::model::{Harness, Provider};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "herdr-agent-quota",
    version,
    about = "Show AI agent subscription quota in Herdr"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Fetch each selected provider's quota and publish it to its Herdr panes.
    ///
    /// Never reads pane output. OpenCode Go is not selectable here: it is
    /// fetched only for a pane that resolved to that subscription.
    Refresh {
        /// Providers to refresh.
        #[arg(long, default_value = "all")]
        provider: ProviderSelection,
        /// Bypass the once-per-minute debounce and fetch now.
        #[arg(long)]
        force: bool,
        /// Print the per-provider outcome as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Keep selected working providers' quotas fresh with one global poller.
    /// This is started automatically by the Herdr status event hook.
    Watch {
        /// Providers to keep fresh while their agents are working.
        #[arg(long, default_value = "all")]
        provider: ProviderSelection,
        /// Override the configured poll interval for this run.
        #[arg(long)]
        interval_seconds: Option<u64>,
    },
    /// Handle one Herdr agent event. Invoked by the plugin's event hooks.
    Event,
    /// Handle a Herdr pane-focus event. Invoked by the plugin's focus hook.
    Focus,
    /// Render the quota dashboard shown in the Herdr popup pane.
    Dashboard,
    /// Install, inspect, or remove this plugin's sidebar rows and collectors.
    ///
    /// With no flag this only reports what would change. Use `--agent` to work
    /// on some agents and leave the rest untouched.
    Configure {
        /// Report what would change without writing anything. This is the
        /// default when no other flag is given.
        #[arg(long, conflicts_with_all = ["apply", "uninstall"])]
        check: bool,
        /// Write the sidebar rows and install the selected agents' collectors.
        /// Safe to re-run; it repairs an existing installation in place.
        #[arg(long, conflicts_with_all = ["check", "uninstall"])]
        apply: bool,
        /// Remove what this plugin installed, restoring the previous
        /// configuration. Without `--agent` this removes everything.
        #[arg(long, conflicts_with_all = ["check", "apply"])]
        uninstall: bool,
        /// Agents to configure: all, claude, codex, grok, agy, opencode, pi.
        /// Repeat or comma-separate to pick several. Defaults to every
        /// supported agent (or $HERDR_AGENT_QUOTA_AGENTS when set), so
        /// `--uninstall` alone still removes everything this plugin installed.
        #[arg(long, value_delimiter = ',')]
        agent: Vec<AgentSelection>,
        /// Persist the active-turn poll interval while applying configuration.
        #[arg(long, requires = "apply")]
        watch_interval_seconds: Option<u64>,
        /// Sidebar row layout: packed joins related tokens on one row;
        /// stacked puts provider, model, cache, TTL, context, 5h, and 7d on
        /// their own rows.
        /// Herdr plugin actions run a fixed command line, so install.sh
        /// passes this through $HERDR_AGENT_QUOTA_SIDEBAR_LAYOUT.
        #[arg(long, value_enum)]
        sidebar_layout: Option<SidebarLayout>,
        /// Blank rows between agent panes. `1` (default) separates them;
        /// `0` packs them flush. Herdr only accepts whole rows. install.sh
        /// writes this to the plugin config directory because plugin actions
        /// run a fixed command line.
        #[arg(long, value_parser = parse_row_gap)]
        row_gap: Option<SidebarRowGap>,
    },
    /// Claude statusLine hook. Claude Code invokes this; not for manual use.
    ClaudeStatusline,
    /// Agy statusLine hook. Antigravity invokes this; not for manual use.
    AgyStatusline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ProviderSelection {
    All,
    Codex,
    Grok,
    Claude,
    Agy,
}

impl ProviderSelection {
    pub fn providers(self) -> Vec<Provider> {
        match self {
            Self::All => Provider::ALL.to_vec(),
            Self::Codex => vec![Provider::Codex],
            Self::Grok => vec![Provider::Grok],
            Self::Claude => vec![Provider::Claude],
            Self::Agy => vec![Provider::Agy],
        }
    }
}

/// Agents `configure` knows how to install and remove.
///
/// Only agents this plugin actually writes something for are listed; a harness
/// with no configuration of its own would silently do nothing here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum AgentSelection {
    All,
    Claude,
    Codex,
    Grok,
    Agy,
    Opencode,
    Pi,
}

/// How quota tokens are arranged in Herdr's agent sidebar.
///
/// Packed is the historical layout: cache sits beside TTL, and 5h sits beside
/// 7d. Stacked gives each field its own row so a narrow sidebar does not
/// truncate both values. Empty tokens still collapse in both layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum SidebarLayout {
    /// Join related tokens on one row (`cache · ttl`, `5h · 7d`).
    #[default]
    Packed,
    /// One field per row (provider, model, cache, TTL, context, 5h, 7d).
    Stacked,
}

/// Blank terminal rows between expanded agent sidebar entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidebarRowGap(u8);

impl SidebarRowGap {
    pub const ENV: &'static str = "HERDR_AGENT_QUOTA_ROW_GAP";
    pub const FLUSH: Self = Self(0);
    pub const SEPARATED: Self = Self(1);

    pub fn as_u8(self) -> u8 {
        self.0
    }

    pub fn as_i64(self) -> i64 {
        i64::from(self.0)
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "0" => Some(Self::FLUSH),
            "1" => Some(Self::SEPARATED),
            _ => None,
        }
    }

    pub fn from_arg_or_env(value: Option<Self>) -> Option<Self> {
        if value.is_some() {
            return value;
        }
        std::env::var(Self::ENV)
            .ok()
            .as_deref()
            .and_then(Self::parse)
    }
}

impl Default for SidebarRowGap {
    fn default() -> Self {
        Self::SEPARATED
    }
}

impl std::fmt::Display for SidebarRowGap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

fn parse_row_gap(value: &str) -> Result<SidebarRowGap, String> {
    SidebarRowGap::parse(value).ok_or_else(|| "row-gap must be 0 or 1".to_string())
}

impl SidebarLayout {
    pub const ENV: &'static str = "HERDR_AGENT_QUOTA_SIDEBAR_LAYOUT";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Packed => "packed",
            Self::Stacked => "stacked",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "packed" => Some(Self::Packed),
            "stacked" => Some(Self::Stacked),
            _ => None,
        }
    }

    /// Flag wins; otherwise the installer environment; otherwise packed.
    ///
    /// Persistence is applied by `configure` after this, so a later repair
    /// with no flag still keeps the layout the user installed.
    pub fn from_arg_or_env(value: Option<Self>) -> Option<Self> {
        if value.is_some() {
            return value;
        }
        std::env::var(Self::ENV)
            .ok()
            .as_deref()
            .and_then(Self::parse)
    }
}

impl AgentSelection {
    /// Every agent `configure` supports, in the order they are reported.
    pub const SUPPORTED: [Harness; 6] = [
        Harness::Claude,
        Harness::Codex,
        Harness::Grok,
        Harness::Agy,
        Harness::OpenCode,
        Harness::Pi,
    ];

    fn harness(self) -> Option<Harness> {
        match self {
            Self::All => None,
            Self::Claude => Some(Harness::Claude),
            Self::Codex => Some(Harness::Codex),
            Self::Grok => Some(Harness::Grok),
            Self::Agy => Some(Harness::Agy),
            Self::Opencode => Some(Harness::OpenCode),
            Self::Pi => Some(Harness::Pi),
        }
    }

    /// Selection for a `configure` run.
    ///
    /// Herdr plugin actions run a fixed command line, so the environment is
    /// the only way an installer can narrow them. An explicit `--agent` always
    /// wins; an unparsable variable is ignored rather than failing a config
    /// write that the user asked for.
    pub fn from_args_or_env(values: &[Self]) -> Vec<Harness> {
        if !values.is_empty() {
            return Self::resolve(values);
        }
        let Some(raw) = std::env::var("HERDR_AGENT_QUOTA_AGENTS").ok() else {
            return Self::SUPPORTED.to_vec();
        };
        let parsed: Vec<Self> = raw
            .split(',')
            .filter_map(|name| Self::parse(name.trim()))
            .collect();
        if parsed.is_empty() {
            return Self::SUPPORTED.to_vec();
        }
        Self::resolve(&parsed)
    }

    fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "all" => Some(Self::All),
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "grok" => Some(Self::Grok),
            "agy" => Some(Self::Agy),
            "opencode" => Some(Self::Opencode),
            "pi" => Some(Self::Pi),
            _ => None,
        }
    }

    /// Flatten a `--agent` selection into a deduplicated harness list that
    /// keeps `SUPPORTED` order, so output and file writes stay deterministic.
    pub fn resolve(values: &[Self]) -> Vec<Harness> {
        if values.is_empty() || values.contains(&Self::All) {
            return Self::SUPPORTED.to_vec();
        }
        let chosen: Vec<Harness> = values.iter().filter_map(|value| value.harness()).collect();
        Self::SUPPORTED
            .into_iter()
            .filter(|harness| chosen.contains(harness))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selection_keeps_supported_order_and_drops_duplicates() {
        assert_eq!(
            AgentSelection::resolve(&[AgentSelection::Grok, AgentSelection::Claude]),
            vec![Harness::Claude, Harness::Grok]
        );
        assert_eq!(
            AgentSelection::resolve(&[AgentSelection::Grok, AgentSelection::Grok]),
            vec![Harness::Grok]
        );
    }

    #[test]
    fn an_unusable_environment_selection_falls_back_to_everything() {
        assert_eq!(AgentSelection::parse("Grok"), Some(AgentSelection::Grok));
        assert_eq!(AgentSelection::parse("Pi"), Some(AgentSelection::Pi));
        assert_eq!(AgentSelection::parse("nonsense"), None);
        // An explicit flag must win over the environment, which is checked in
        // `from_args_or_env` before the variable is read at all.
        assert_eq!(
            AgentSelection::from_args_or_env(&[AgentSelection::Grok]),
            vec![Harness::Grok]
        );
    }

    #[test]
    fn all_wins_and_is_the_default_so_uninstall_alone_removes_everything() {
        assert_eq!(
            AgentSelection::resolve(&[AgentSelection::All]),
            AgentSelection::SUPPORTED.to_vec()
        );
        assert_eq!(
            AgentSelection::resolve(&[AgentSelection::Grok, AgentSelection::All]),
            AgentSelection::SUPPORTED.to_vec()
        );
        assert_eq!(
            AgentSelection::resolve(&[]),
            AgentSelection::SUPPORTED.to_vec()
        );
    }

    #[test]
    fn sidebar_layout_parses_packed_and_stacked_and_ignores_junk() {
        assert_eq!(SidebarLayout::parse("packed"), Some(SidebarLayout::Packed));
        assert_eq!(
            SidebarLayout::parse("Stacked"),
            Some(SidebarLayout::Stacked)
        );
        assert_eq!(SidebarLayout::parse("nonsense"), None);
        assert_eq!(
            SidebarLayout::from_arg_or_env(Some(SidebarLayout::Stacked)),
            Some(SidebarLayout::Stacked)
        );
    }

    #[test]
    fn row_gap_accepts_zero_and_one() {
        assert_eq!(SidebarRowGap::parse("0"), Some(SidebarRowGap::FLUSH));
        assert_eq!(SidebarRowGap::parse("1"), Some(SidebarRowGap::SEPARATED));
        assert_eq!(SidebarRowGap::parse("2"), None);
        assert_eq!(SidebarRowGap::parse("0.5"), None);
        assert_eq!(SidebarRowGap::default().as_u8(), 1);
    }
}
