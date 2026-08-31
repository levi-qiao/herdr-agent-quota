use anyhow::Result;
use clap::Parser;
use herdr_agent_quota::cli::{Cli, Command};

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Refresh {
            provider,
            force,
            json,
        } => herdr_agent_quota::refresh::run(&provider.providers(), force, json),
        Command::Watch {
            provider,
            interval_seconds,
        } => herdr_agent_quota::refresh::watch(&provider.providers(), interval_seconds),
        Command::Event => herdr_agent_quota::refresh::event(),
        Command::Focus => herdr_agent_quota::refresh::focus(),
        Command::Dashboard => herdr_agent_quota::dashboard::run(),
        Command::Settings => herdr_agent_quota::settings::run(),
        Command::Configure {
            check,
            apply,
            uninstall,
            agent,
            watch_interval_seconds,
            sidebar_layout,
            quota_percent,
            row_gap,
            fields,
            brand_colors,
        } => herdr_agent_quota::configure::run(
            check,
            apply,
            uninstall,
            &herdr_agent_quota::cli::AgentSelection::from_args_or_env(&agent),
            herdr_agent_quota::cli::ConfigureOptions {
                watch_interval_seconds,
                sidebar_layout,
                quota_percent,
                row_gap,
                fields,
                brand_colors,
            },
        ),
        Command::ClaudeStatusline => herdr_agent_quota::configure::claude::run_statusline_hook(),
        Command::AgyStatusline => herdr_agent_quota::configure::agy::run_statusline_hook(),
    }
}
