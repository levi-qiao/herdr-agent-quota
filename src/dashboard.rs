use crate::cache::CacheStore;
use crate::model::{Provider, ProviderSnapshot};
use crate::presentation::dashboard_summary;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use std::io::{self, IsTerminal, Write};
use std::time::Duration;

pub fn run() -> Result<()> {
    let cache = CacheStore::from_env()?;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        print_snapshot(&cache)?;
        return Ok(());
    }
    enable_raw_mode()?;
    let result = interactive(&cache);
    let _ = disable_raw_mode();
    result
}

/// Idle wait between frames. `poll` returns as soon as a key arrives, so this
/// bounds only how long an unattended popup sleeps, never how fast it reacts.
const IDLE_POLL: Duration = Duration::from_secs(1);

/// Repaint only when the rendered frame actually changed.
///
/// The popup stays open for as long as the user leaves it there. Clearing and
/// redrawing the whole screen several times a second flickers and re-reads
/// every cached snapshot for nothing: the numbers only move when a refresh
/// lands or a reset countdown ticks over a minute boundary.
fn interactive(cache: &CacheStore) -> Result<()> {
    let mut painted: Option<String> = None;
    loop {
        let frame = format!("{}\r\nr refresh  q quit\r\n", render_snapshot(cache)?);
        if painted.as_deref() != Some(frame.as_str()) {
            print!(
                "{}{}{frame}",
                crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                crossterm::cursor::MoveTo(0, 0)
            );
            io::stdout().flush()?;
            painted = Some(frame);
        }
        if event::poll(IDLE_POLL)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('r') => {
                        crate::refresh::run(&Provider::ALL, true, false)?;
                        // A forced refresh scrolls provider output over the
                        // popup, so the next frame must repaint even if every
                        // number came back identical.
                        painted = None;
                    }
                    _ => {}
                }
            }
        }
    }
}

fn print_snapshot(cache: &CacheStore) -> Result<()> {
    print!("{}", render_snapshot(cache)?);
    Ok(())
}

fn render_snapshot(cache: &CacheStore) -> Result<String> {
    let mut output = String::from("Herdr Agent Quota\r\n=================\r\n");
    let now = CacheStore::now_unix();
    for provider in Provider::ALL {
        let snapshot = cache.load(provider)?;
        output.push_str(&render_provider(provider, snapshot.as_ref(), now));
        output.push_str("\r\n");
    }
    // The dashboard is the only surface with room for a scoped collector's
    // full window set, including the 30d bucket the sidebar has no token for.
    for provider in Provider::SCOPED {
        let Some(snapshot) = cache.load(provider)? else {
            continue;
        };
        output.push_str(&render_provider(provider, Some(&snapshot), now));
        output.push_str("\r\n");
    }
    Ok(output)
}

pub fn render_provider(
    provider: Provider,
    snapshot: Option<&ProviderSnapshot>,
    now_unix: u64,
) -> String {
    match snapshot {
        Some(snapshot) => format!(
            "{} {}\r\n  {}",
            provider.display_name(),
            snapshot.severity(now_unix).label(),
            dashboard_summary(snapshot, now_unix)
        ),
        None => format!("{} N/A\r\n  unavailable", provider.display_name()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ResetAt, UsageWindow, WindowKind};
    use tempfile::tempdir;

    #[test]
    fn renders_compact_remaining_values_with_reset_eta() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                UsageWindow::new(
                    WindowKind::FiveHour,
                    58.0,
                    Some(ResetAt::from_unix_seconds(14_820)),
                )
                .unwrap(),
                UsageWindow::new(
                    WindowKind::Weekly,
                    27.0,
                    Some(ResetAt::from_unix_seconds(183_600)),
                )
                .unwrap(),
            ],
            1,
        );
        let rendered = render_provider(Provider::Claude, Some(&snapshot), 0);
        assert_eq!(
            rendered,
            "Claude WARN\r\n  5h 42% left reset 4h07m · 7d 73% left reset 2d3h"
        );
    }

    /// The sidebar has no monthly token, so the dashboard is where a Go plan's
    /// 30d bucket has to surface. It appears only once something is cached.
    #[test]
    fn a_scoped_collector_appears_with_its_monthly_window_once_cached() {
        let directory = tempdir().unwrap();
        let cache = CacheStore::new(directory.path());
        assert!(!render_snapshot(&cache).unwrap().contains("OpenCode Go"));

        cache
            .save(&ProviderSnapshot::new(
                Provider::OpenCodeGo,
                vec![
                    UsageWindow::new(
                        WindowKind::FiveHour,
                        10.0,
                        Some(ResetAt::from_unix_seconds(3_600)),
                    )
                    .unwrap(),
                    UsageWindow::new(
                        WindowKind::Monthly,
                        30.0,
                        Some(ResetAt::from_unix_seconds(1_500_000)),
                    )
                    .unwrap(),
                ],
                0,
            ))
            .unwrap();
        let rendered = render_snapshot(&cache).unwrap();
        assert!(rendered.contains("OpenCode Go"), "{rendered}");
        assert!(rendered.contains("30d 70% left"), "{rendered}");
    }

    #[test]
    fn snapshot_lines_return_to_column_zero_in_herdr_pty() {
        let directory = tempdir().unwrap();
        let rendered = render_snapshot(&CacheStore::new(directory.path())).unwrap();
        assert!(rendered.contains("Quota\r\n=================\r\nCodex"));
        assert!(!rendered.contains("Quota\n================="));
    }
}
