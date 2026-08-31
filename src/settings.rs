//! The settings popup pane.
//!
//! Every option here is one `configure --apply` would accept. The pane does
//! not write configuration itself: it re-invokes this same binary with the
//! chosen flags, exactly as Herdr's "Install / repair" action does. That keeps
//! one writer for the sidebar rows and the statusLine entries, and it keeps
//! `configure`'s own output off a screen that is in raw mode.
//!
//! Agents are shown but not editable. Removing an agent has to *uninstall*
//! that agent's collector rather than narrow a selection, and a mis-key in a
//! popup must never take a statusLine entry with it; `./install.sh --agent`
//! and `./uninstall.sh --agent` stay the way to change that.

use crate::cache::CacheStore;
use crate::cli::{AgentSelection, PercentStyle, SidebarLayout, SidebarRowGap};
use crate::model::Harness;
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use std::io::{self, IsTerminal, Write};
use std::process::Command;

/// Poll intervals the pane offers, in seconds. `configure` accepts anything
/// between 30s and 1h; these are the values worth one keypress. Every entry
/// must stay inside that range.
const INTERVALS: [u64; 7] = [30, 60, 120, 300, 600, 1_800, 3_600];

/// One editable row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Percent,
    Layout,
    RowGap,
    Interval,
}

impl Field {
    const ALL: [Self; 4] = [Self::Percent, Self::Layout, Self::RowGap, Self::Interval];

    fn label(self) -> &'static str {
        match self {
            Self::Percent => "Percentages",
            Self::Layout => "Sidebar layout",
            Self::RowGap => "Row gap",
            Self::Interval => "Watch interval",
        }
    }
}

/// The choices as the user has them on screen, before applying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    percent: PercentStyle,
    layout: SidebarLayout,
    gap: SidebarRowGap,
    interval_seconds: u64,
}

impl Settings {
    /// What a fresh `configure` run would resolve today, so the pane opens on
    /// the installation's real state rather than on defaults.
    fn current(cache: Option<&CacheStore>) -> Self {
        Self {
            percent: crate::configure::resolved_percent_style(None, cache),
            layout: crate::configure::resolved_sidebar_layout(None, cache),
            gap: crate::configure::resolved_row_gap(None, cache),
            interval_seconds: cache
                .map(CacheStore::watch_interval_seconds)
                .unwrap_or(crate::cache::DEFAULT_WATCH_INTERVAL_SECONDS),
        }
    }

    fn value(self, field: Field) -> String {
        match field {
            Field::Percent => self.percent.as_str().to_string(),
            Field::Layout => self.layout.as_str().to_string(),
            Field::RowGap => self.gap.to_string(),
            Field::Interval => format_interval(self.interval_seconds),
        }
    }

    fn hint(self, field: Field) -> &'static str {
        match (field, self.percent, self.layout, self.gap.as_u8()) {
            (Field::Percent, PercentStyle::Remaining, _, _) => "how much quota is left",
            (Field::Percent, PercentStyle::Used, _, _) => "how much quota is spent",
            (Field::Layout, _, SidebarLayout::Packed, _) => "cache·ttl and 5h·7d share a row",
            (Field::Layout, _, SidebarLayout::Stacked, _) => "every field on its own row",
            (Field::RowGap, _, _, 0) => "panes packed flush",
            (Field::RowGap, _, _, _) => "one blank row between panes",
            (Field::Interval, ..) => "polled while an agent is working",
        }
    }

    /// Move one field by `step` (+1 or -1), wrapping at both ends so a two-way
    /// option needs no thought about which arrow to press.
    fn cycle(&mut self, field: Field, step: i8) {
        match field {
            Field::Percent => {
                self.percent = match self.percent {
                    PercentStyle::Remaining => PercentStyle::Used,
                    PercentStyle::Used => PercentStyle::Remaining,
                }
            }
            Field::Layout => {
                self.layout = match self.layout {
                    SidebarLayout::Packed => SidebarLayout::Stacked,
                    SidebarLayout::Stacked => SidebarLayout::Packed,
                }
            }
            Field::RowGap => {
                self.gap = match self.gap.as_u8() {
                    0 => SidebarRowGap::SEPARATED,
                    _ => SidebarRowGap::FLUSH,
                }
            }
            Field::Interval => {
                let current = INTERVALS
                    .iter()
                    .position(|value| *value == self.interval_seconds)
                    .unwrap_or(1);
                let count = INTERVALS.len() as i8;
                let next = (current as i8 + step).rem_euclid(count);
                self.interval_seconds = INTERVALS[next as usize];
            }
        }
    }

    /// The `configure` invocation that makes this installation match the pane.
    ///
    /// Every value is passed explicitly: a flag outranks the environment and
    /// the stored preference, so applying is not sensitive to what a previous
    /// installer left behind. The agent selection is deliberately absent —
    /// `configure` then resolves the stored one and leaves it as it was.
    fn configure_arguments(self) -> Vec<String> {
        vec![
            "configure".to_string(),
            "--apply".to_string(),
            "--quota-percent".to_string(),
            self.percent.as_str().to_string(),
            "--sidebar-layout".to_string(),
            self.layout.as_str().to_string(),
            "--row-gap".to_string(),
            self.gap.to_string(),
            "--watch-interval-seconds".to_string(),
            self.interval_seconds.to_string(),
        ]
    }
}

fn format_interval(seconds: u64) -> String {
    if seconds >= 60 && seconds.is_multiple_of(60) {
        return format!("{}m", seconds / 60);
    }
    format!("{seconds}s")
}

pub fn run() -> Result<()> {
    let cache = CacheStore::from_env().ok();
    let settings = Settings::current(cache.as_ref());
    if !io::stdin().is_terminal() {
        print!(
            "{}",
            render(&settings, Settings::current(cache.as_ref()), 0, None)
        );
        return Ok(());
    }
    enable_raw_mode()?;
    let result = interactive(settings);
    let _ = disable_raw_mode();
    result
}

fn interactive(applied: Settings) -> Result<()> {
    let mut applied = applied;
    let mut draft = applied;
    let mut selected = 0usize;
    let mut status: Option<String> = None;
    let mut painted: Option<String> = None;
    loop {
        let frame = render(&draft, applied, selected, status.as_deref());
        if painted.as_deref() != Some(frame.as_str()) {
            print!(
                "{}{}{frame}",
                crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                crossterm::cursor::MoveTo(0, 0)
            );
            io::stdout().flush()?;
            painted = Some(frame);
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        // Raw mode delivers Ctrl+C as a key event, so the pane has to honour
        // it itself or there is no way out but killing the pane.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(());
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => {
                selected = (selected + Field::ALL.len() - 1) % Field::ALL.len();
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                selected = (selected + 1) % Field::ALL.len();
            }
            KeyCode::Left | KeyCode::Char('h') => {
                draft.cycle(Field::ALL[selected], -1);
                status = None;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                draft.cycle(Field::ALL[selected], 1);
                status = None;
            }
            KeyCode::Char('a') | KeyCode::Enter => {
                if draft == applied {
                    status = Some("Nothing to apply.".to_string());
                    continue;
                }
                status = Some(match apply(draft) {
                    Ok(()) => {
                        applied = draft;
                        "Applied. Restart running agent panes to reload hooks.".to_string()
                    }
                    Err(error) => format!("Failed: {error}"),
                });
            }
            _ => {}
        }
    }
}

/// Write the configuration, then make Herdr re-read it.
///
/// `configure` runs as a child process rather than in-process: it prints a
/// report, and this screen is in raw mode. The child inherits Herdr's plugin
/// environment, which is what lets it write at all.
fn apply(settings: Settings) -> Result<()> {
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    let output = Command::new(executable)
        .args(settings.configure_arguments())
        .output()
        .context("run configure")?;
    if !output.status.success() {
        anyhow::bail!(
            "{}",
            first_line(&String::from_utf8_lossy(&output.stderr))
                .unwrap_or_else(|| "configure failed".to_string())
        );
    }
    let herdr = std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string());
    let reload = Command::new(herdr)
        .args(["server", "reload-config"])
        .output()
        .context("reload Herdr configuration")?;
    if !reload.status.success() {
        anyhow::bail!(
            "{}",
            first_line(&String::from_utf8_lossy(&reload.stderr))
                .unwrap_or_else(|| "herdr server reload-config failed".to_string())
        );
    }
    Ok(())
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(60).collect())
}

/// The whole frame. Lines end in `\r\n` because Herdr's pane is a raw-mode
/// PTY, where a bare `\n` leaves the cursor at the column it was in.
///
/// The popup is 73 columns wide on a default window, so every line here stays
/// well inside 70.
fn render(draft: &Settings, applied: Settings, selected: usize, status: Option<&str>) -> String {
    let mut output = String::from("Agent quota settings\r\n====================\r\n\r\n");
    for (index, field) in Field::ALL.iter().enumerate() {
        let cursor = if index == selected { '>' } else { ' ' };
        let changed = if draft.value(*field) == applied.value(*field) {
            ' '
        } else {
            '*'
        };
        output.push_str(&format!(
            "{cursor} {changed} {:<15} < {:^10} >  {}\r\n",
            field.label(),
            draft.value(*field),
            draft.hint(*field),
        ));
    }
    output.push_str("\r\n");
    output.push_str(&format!("    {:<15} {}\r\n", "Agents", agent_summary()));
    output.push_str("    change with ./install.sh --agent or ./uninstall.sh --agent\r\n\r\n");
    output.push_str("  \u{2191}\u{2193} field   \u{2190}\u{2192} value   a apply   q quit\r\n");
    if let Some(status) = status {
        output.push_str(&format!("  {status}\r\n"));
    }
    output
}

/// The names `--agent` accepts, so the line reads as the command to type.
fn agent_summary() -> String {
    AgentSelection::from_args_or_env(&[])
        .iter()
        .map(|harness| match harness {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Grok => "grok",
            Harness::Agy => "agy",
            Harness::OpenCode => "opencode",
            Harness::Pi => "pi",
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            percent: PercentStyle::Remaining,
            layout: SidebarLayout::Packed,
            gap: SidebarRowGap::SEPARATED,
            interval_seconds: 60,
        }
    }

    #[test]
    fn every_offered_interval_is_one_configure_accepts() {
        for seconds in INTERVALS {
            assert!(
                crate::cache::CacheStore::validate_watch_interval_seconds(seconds).is_ok(),
                "{seconds}s is outside the range configure accepts"
            );
        }
    }

    #[test]
    fn two_way_options_flip_whichever_arrow_is_pressed() {
        let mut draft = settings();
        draft.cycle(Field::Percent, -1);
        assert_eq!(draft.percent, PercentStyle::Used);
        draft.cycle(Field::Percent, 1);
        assert_eq!(draft.percent, PercentStyle::Remaining);

        draft.cycle(Field::Layout, 1);
        assert_eq!(draft.layout, SidebarLayout::Stacked);
        draft.cycle(Field::RowGap, 1);
        assert_eq!(draft.gap, SidebarRowGap::FLUSH);
    }

    #[test]
    fn the_interval_wraps_at_both_ends_of_the_offered_list() {
        let mut draft = settings();
        draft.cycle(Field::Interval, -1);
        assert_eq!(draft.interval_seconds, INTERVALS[0]);
        draft.cycle(Field::Interval, -1);
        assert_eq!(draft.interval_seconds, INTERVALS[INTERVALS.len() - 1]);
        draft.cycle(Field::Interval, 1);
        assert_eq!(draft.interval_seconds, INTERVALS[0]);
    }

    /// An unknown stored interval (`configure` accepts any value in range)
    /// must not trap the list: the first press lands on a known entry.
    #[test]
    fn an_interval_outside_the_offered_list_still_moves() {
        let mut draft = settings();
        draft.interval_seconds = 45;
        draft.cycle(Field::Interval, 1);
        assert_eq!(draft.interval_seconds, INTERVALS[2]);
    }

    /// Applying passes every value explicitly, so it cannot inherit a stale
    /// preference — and it never passes `--agent`, so the stored agent
    /// selection survives.
    #[test]
    fn applying_names_every_value_and_no_agent() {
        let mut draft = settings();
        draft.cycle(Field::Percent, 1);
        assert_eq!(
            draft.configure_arguments(),
            vec![
                "configure",
                "--apply",
                "--quota-percent",
                "used",
                "--sidebar-layout",
                "packed",
                "--row-gap",
                "1",
                "--watch-interval-seconds",
                "60",
            ]
        );
    }

    #[test]
    fn unapplied_edits_are_marked_and_the_frame_returns_to_column_zero() {
        let applied = settings();
        let mut draft = applied;
        draft.cycle(Field::Layout, 1);
        let frame = render(&draft, applied, 1, Some("Nothing to apply."));
        assert!(frame.contains("> * Sidebar layout"), "{frame}");
        assert!(frame.contains("  Percentages"), "{frame}");
        assert!(frame.contains("stacked"), "{frame}");
        for line in frame.split("\r\n") {
            assert!(!line.contains('\n'), "{frame}");
            assert!(line.chars().count() <= 70, "too wide: {line}");
        }
    }

    #[test]
    fn intervals_read_as_minutes_when_they_divide_evenly() {
        assert_eq!(format_interval(30), "30s");
        assert_eq!(format_interval(60), "1m");
        assert_eq!(format_interval(3_600), "60m");
    }
}
