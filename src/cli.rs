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
    Refresh {
        #[arg(long, default_value = "all")]
        provider: ProviderSelection,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Keep selected working providers' quotas fresh with one global poller.
    /// This is started automatically by the Herdr status event hook.
    Watch {
        #[arg(long, default_value = "all")]
        provider: ProviderSelection,
        /// Override the configured poll interval for this run.
        #[arg(long)]
        interval_seconds: Option<u64>,
    },
    Event,
    Focus,
    Dashboard,
    Configure {
        #[arg(long, conflicts_with_all = ["apply", "uninstall"])]
        check: bool,
        #[arg(long, conflicts_with_all = ["check", "uninstall"])]
        apply: bool,
        #[arg(long, conflicts_with_all = ["check", "apply"])]
        uninstall: bool,
        /// Agents to configure: all, claude, codex, grok, agy, opencode.
        /// Repeat or comma-separate to pick several. Defaults to every
        /// supported agent (or $HERDR_AGENT_QUOTA_AGENTS when set), so
        /// `--uninstall` alone still removes everything this plugin installed.
        #[arg(long, value_delimiter = ',')]
        agent: Vec<AgentSelection>,
        /// Persist the active-turn poll interval while applying configuration.
        #[arg(long, requires = "apply")]
        watch_interval_seconds: Option<u64>,
    },
    ClaudeStatusline,
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
}

impl AgentSelection {
    /// Every agent `configure` supports, in the order they are reported.
    pub const SUPPORTED: [Harness; 5] = [
        Harness::Claude,
        Harness::Codex,
        Harness::Grok,
        Harness::Agy,
        Harness::OpenCode,
    ];

    fn harness(self) -> Option<Harness> {
        match self {
            Self::All => None,
            Self::Claude => Some(Harness::Claude),
            Self::Codex => Some(Harness::Codex),
            Self::Grok => Some(Harness::Grok),
            Self::Agy => Some(Harness::Agy),
            Self::Opencode => Some(Harness::OpenCode),
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
}
