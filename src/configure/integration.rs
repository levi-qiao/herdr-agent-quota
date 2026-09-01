//! Herdr agent integration setup and diagnostics.
//!
//! Quota attribution starts from the session id Herdr reports for a pane, and
//! Herdr only learns that id once its integration for that agent is installed.
//! Without it a pane is detected but carries no session, so this plugin can
//! never attribute it and silently shows nothing. That is the least obvious
//! way for a fresh install to look broken, so `configure` says so out loud.
//!
//! omp is installed automatically when the user enables omp in this plugin;
//! otherwise the pane can be detected but has no session path, which makes
//! every omp feature look broken. Existing integrations are left untouched.

use crate::model::Harness;
use anyhow::{bail, Result};
use std::process::Command;

/// Herdr's integration id for a harness, when it has one.
///
/// Agy reports through its statusLine instead, so it has no integration.
fn integration_id(harness: Harness) -> Option<&'static str> {
    match harness {
        Harness::Claude => Some("claude"),
        Harness::Codex => Some("codex"),
        Harness::Grok => Some("grok"),
        Harness::OpenCode => Some("opencode"),
        Harness::Pi => Some("pi"),
        Harness::Omp => Some("omp"),
        Harness::Agy => None,
    }
}

pub fn report_missing(agents: &[Harness]) {
    let Some(status) = read_status() else {
        return;
    };
    for harness in agents {
        let Some(id) = integration_id(*harness) else {
            continue;
        };
        if !is_missing(&status, id) {
            continue;
        }
        println!(
            "Herdr's {id} integration is not installed, so Herdr reports no session id for {id} panes and their quota cannot be attributed. Install it with `herdr integration install {id}`, then restart that agent pane."
        );
    }
}

/// Install the omp integration when omp is selected and Herdr explicitly says
/// it is absent. A missing `herdr` binary or an unrecognized status format is
/// not guessed at; the existing advisory remains the fallback.
pub fn ensure_omp(agents: &[Harness]) -> Result<()> {
    let Some(status) = read_status() else {
        return Ok(());
    };
    if !needs_omp_install(agents, &status) {
        return Ok(());
    }
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let output = Command::new(executable)
        .args(["integration", "install", "omp"])
        .output()?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        bail!("install Herdr omp integration: {}", detail.trim());
    }
    println!("Installed Herdr's omp integration. Restart already-running omp panes once.");
    Ok(())
}

fn needs_omp_install(agents: &[Harness], status: &str) -> bool {
    agents.contains(&Harness::Omp) && is_missing(status, "omp")
}

fn read_status() -> Option<String> {
    let executable = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let output = Command::new(executable)
        .args(["integration", "status"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Herdr prints one `<id>: <state> (<path>)` line per integration. Only an
/// explicit "not installed" is actionable; an unknown id or a reworded state
/// stays quiet rather than nagging about something that may be fine.
fn is_missing(status: &str, id: &str) -> bool {
    status.lines().any(|line| {
        line.trim()
            .strip_prefix(id)
            .and_then(|rest| rest.strip_prefix(':'))
            .is_some_and(|state| state.trim_start().starts_with("not installed"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "\
claude: current (v7) (/home/u/.claude/hooks/herdr-agent-state.sh)
codex: current (v7) (/home/u/.codex/herdr-agent-state.sh)
opencode: not installed (/home/u/.config/opencode/plugins/herdr-agent-state.js)
omp: not installed (/home/u/.omp/agent/extensions/herdr-agent-state.ts)
grok: outdated (v0) (/home/u/.grok/hooks/herdr-agent-state.sh)
";

    #[test]
    fn only_an_explicit_not_installed_line_is_reported() {
        assert!(is_missing(STATUS, "opencode"));
        assert!(is_missing(STATUS, "omp"));
        assert!(!is_missing(STATUS, "claude"));
        assert!(!is_missing(STATUS, "codex"));
        assert!(!is_missing(STATUS, "grok"));
        assert!(!is_missing(STATUS, "kimi"));
        assert!(!is_missing("", "opencode"));
    }

    #[test]
    fn a_prefix_match_is_not_a_hit() {
        // "open" must not match the "opencode:" line.
        assert!(!is_missing(STATUS, "open"));
    }

    #[test]
    fn session_backed_harnesses_report_their_integration_id() {
        assert_eq!(integration_id(Harness::Agy), None);
        assert_eq!(integration_id(Harness::OpenCode), Some("opencode"));
        assert_eq!(integration_id(Harness::Pi), Some("pi"));
        assert_eq!(integration_id(Harness::Omp), Some("omp"));
    }

    #[test]
    fn only_a_selected_and_explicitly_missing_omp_is_auto_installed() {
        assert!(needs_omp_install(&[Harness::Omp], STATUS));
        assert!(!needs_omp_install(&[Harness::Pi], STATUS));
        assert!(!needs_omp_install(
            &[Harness::Omp],
            "omp: current (v8) (/home/u/.omp/agent/extensions/herdr-agent-state.ts)"
        ));
    }
}
