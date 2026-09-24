use anyhow::Result;
use clap::Parser;
use herdr_agent_quota::cli::{Cli, Command};

fn main() -> Result<()> {
    herdr_agent_quota::identity::adopt_alias_plugin_dirs();
    let cli = Cli::parse();
    match cli.command {
        Command::Refresh {
            provider,
            force,
            json,
            keychain_approve,
        } => {
            if keychain_approve {
                let providers = provider.providers();
                let muse = providers.contains(&herdr_agent_quota::model::Provider::Muse);
                let cursor = providers.contains(&herdr_agent_quota::model::Provider::Cursor);
                if !muse && !cursor {
                    anyhow::bail!(
                        "--keychain-approve only applies to muse or cursor; run `refresh --provider cursor --keychain-approve`"
                    );
                }
                if muse {
                    herdr_agent_quota::providers::muse::set_keychain_approve_attempt();
                }
                if cursor {
                    herdr_agent_quota::providers::cursor::set_keychain_approve_attempt();
                }
                return herdr_agent_quota::refresh::run(&providers, force, json);
            }
            herdr_agent_quota::refresh::run(&provider.providers(), force, json)
        }
        Command::Watch {
            provider,
            interval_seconds,
            defer,
        } => herdr_agent_quota::refresh::watch(&provider.providers(), interval_seconds, defer),
        Command::Startup { provider } => herdr_agent_quota::refresh::startup(&provider.providers()),
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
            sidebar_pacing,
            row_gap,
            fields,
            brand_colors,
            agent_order,
            low_quota_alert,
        } => herdr_agent_quota::configure::run(
            check,
            apply,
            uninstall,
            &herdr_agent_quota::cli::AgentSelection::from_args_or_env(&agent),
            herdr_agent_quota::cli::ConfigureOptions {
                watch_interval_seconds,
                sidebar_layout,
                quota_percent,
                sidebar_pacing,
                row_gap,
                fields,
                brand_colors,
                agent_order,
                low_quota_alert,
            },
        ),
        Command::ClaudeStatusline => herdr_agent_quota::configure::claude::run_statusline_hook(),
        Command::AgyStatusline => herdr_agent_quota::configure::agy::run_statusline_hook(),
        Command::CursorHooks => herdr_agent_quota::configure::cursor::run_hook(),
    }
}
