//! The settings popup pane.
//!
//! Every option here is one `configure` already accepts. The pane does not
//! write configuration itself: it re-invokes this same binary, exactly as
//! Herdr's "Install / repair" action does. That keeps one writer for the
//! sidebar rows and the statusLine entries, and it keeps `configure`'s report
//! off a screen that is in raw mode.
//!
//! Applying finishes the job: it reloads Herdr's configuration so new rows
//! take effect, then forces one refresh so the tokens in those rows are
//! republished. Without that last step a changed percentage style would sit
//! invisible until the next agent event.

use crate::cache::CacheStore;
use crate::cli::{
    AgentOrder, AgentSelection, BrandColors, FieldSet, LowQuotaAlert, PercentStyle, SidebarField,
    SidebarLayout, SidebarRowGap,
};
use crate::model::Harness;
use crate::prefs;
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use std::io::{self, IsTerminal, Write};
use std::process::Command;

/// Poll intervals the pane offers, in seconds. `configure` accepts anything
/// between 30s and 1h; these are the values worth one keypress.
const INTERVALS: [u64; 7] = [30, 60, 120, 300, 600, 1_800, 3_600];

/// Rows the pane draws, in order. Headers are drawn but never selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Header(&'static str),
    Choice(Choice),
    Field(SidebarField),
    Agent(Harness),
}

/// A row whose value cycles through a small list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    Percent,
    Layout,
    RowGap,
    Interval,
    Brand,
    Order,
    Alert,
}

impl Choice {
    fn label(self) -> &'static str {
        match self {
            Self::Percent => "Percentages",
            Self::Layout => "Sidebar layout",
            Self::RowGap => "Row gap",
            Self::Interval => "Watch interval",
            Self::Brand => "Brand colors",
            Self::Order => "Agent order",
            Self::Alert => "Low quota alert",
        }
    }
}

fn rows() -> Vec<Row> {
    let mut rows = vec![
        Row::Header("Display"),
        Row::Choice(Choice::Percent),
        Row::Choice(Choice::Layout),
        Row::Choice(Choice::RowGap),
        Row::Choice(Choice::Interval),
        Row::Choice(Choice::Brand),
        Row::Choice(Choice::Order),
        Row::Choice(Choice::Alert),
        Row::Header("Fields"),
    ];
    rows.extend(SidebarField::ALL.into_iter().map(Row::Field));
    rows.push(Row::Header("Agents"));
    rows.extend(AgentSelection::SUPPORTED.into_iter().map(Row::Agent));
    rows
}

/// The choices as the user has them on screen, before applying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    percent: PercentStyle,
    layout: SidebarLayout,
    gap: SidebarRowGap,
    interval_seconds: u64,
    brand: BrandColors,
    order: AgentOrder,
    alert: LowQuotaAlert,
    fields: FieldSet,
    /// Indexed by [`AgentSelection::SUPPORTED`], so the whole struct stays
    /// `Copy` and comparing a draft to what is applied is one `==`.
    agents: [bool; AgentSelection::SUPPORTED.len()],
}

impl Settings {
    /// What a fresh `configure` run would resolve today, so the pane opens on
    /// the installation's real state rather than on defaults.
    fn current(cache: Option<&CacheStore>) -> Self {
        let installed = AgentSelection::from_args_or_env(&[]);
        let mut agents = [false; AgentSelection::SUPPORTED.len()];
        for (slot, harness) in agents.iter_mut().zip(AgentSelection::SUPPORTED) {
            *slot = installed.contains(&harness);
        }
        Self {
            percent: crate::configure::resolved_percent_style(None, cache),
            layout: crate::configure::resolved_sidebar_layout(None, cache),
            gap: crate::configure::resolved_row_gap(None, cache),
            interval_seconds: cache
                .map(CacheStore::watch_interval_seconds)
                .unwrap_or(crate::cache::DEFAULT_WATCH_INTERVAL_SECONDS),
            brand: crate::configure::resolved_brand_colors(None, cache),
            order: crate::configure::resolved_agent_order(None, cache),
            alert: crate::configure::resolved_low_quota_alert(None, cache),
            fields: crate::configure::resolved_fields(None, cache),
            agents,
        }
    }

    fn choice_value(self, choice: Choice) -> String {
        match choice {
            Choice::Percent => self.percent.as_str().to_string(),
            Choice::Layout => self.layout.as_str().to_string(),
            Choice::RowGap => self.gap.to_string(),
            Choice::Interval => format_interval(self.interval_seconds),
            Choice::Brand => self.brand.as_str().to_string(),
            Choice::Order => self.order.as_str().to_string(),
            Choice::Alert => self.alert.to_string(),
        }
    }

    fn choice_hint(self, choice: Choice) -> &'static str {
        match choice {
            Choice::Percent => match self.percent {
                PercentStyle::Remaining => "how much quota is left",
                PercentStyle::Used => "how much quota is spent",
            },
            Choice::Layout => match self.layout {
                SidebarLayout::Packed => "cache·ttl and 5h·7d share a row",
                SidebarLayout::Stacked => "every field on its own row",
            },
            Choice::RowGap => match self.gap.as_u8() {
                0 => "panes packed flush",
                _ => "one blank row between panes",
            },
            Choice::Interval => "polled while an agent is working",
            Choice::Brand => match self.brand {
                BrandColors::On => "provider and model in agent hues",
                BrandColors::Off => "severity colors only",
            },
            Choice::Order => match self.order {
                AgentOrder::Default => "Herdr sorts the agent panel",
                AgentOrder::Quota => "least quota left at the top",
            },
            Choice::Alert => match self.alert.is_off() {
                true => "no notification",
                false => "notify once per provider on the way down",
            },
        }
    }

    fn agents(self) -> Vec<Harness> {
        AgentSelection::SUPPORTED
            .into_iter()
            .zip(self.agents)
            .filter_map(|(harness, on)| on.then_some(harness))
            .collect()
    }

    fn has_agent(self, harness: Harness) -> bool {
        self.agents().contains(&harness)
    }

    /// Agents this draft would remove from an installation that has `applied`.
    fn removed_agents(self, applied: Settings) -> Vec<Harness> {
        AgentSelection::SUPPORTED
            .into_iter()
            .filter(|harness| applied.has_agent(*harness) && !self.has_agent(*harness))
            .collect()
    }

    /// Move one row's value by `step` (+1 or -1). Two-way options flip on
    /// either arrow, so no one has to remember which direction they live in.
    fn cycle(&mut self, row: Row, step: i8) {
        match row {
            Row::Header(_) => {}
            Row::Choice(Choice::Percent) => {
                self.percent = match self.percent {
                    PercentStyle::Remaining => PercentStyle::Used,
                    PercentStyle::Used => PercentStyle::Remaining,
                }
            }
            Row::Choice(Choice::Layout) => {
                self.layout = match self.layout {
                    SidebarLayout::Packed => SidebarLayout::Stacked,
                    SidebarLayout::Stacked => SidebarLayout::Packed,
                }
            }
            Row::Choice(Choice::RowGap) => {
                self.gap = match self.gap.as_u8() {
                    0 => SidebarRowGap::SEPARATED,
                    _ => SidebarRowGap::FLUSH,
                }
            }
            Row::Choice(Choice::Brand) => {
                self.brand = match self.brand {
                    BrandColors::On => BrandColors::Off,
                    BrandColors::Off => BrandColors::On,
                }
            }
            Row::Choice(Choice::Order) => {
                self.order = match self.order {
                    AgentOrder::Default => AgentOrder::Quota,
                    AgentOrder::Quota => AgentOrder::Default,
                }
            }
            Row::Choice(Choice::Alert) => {
                let current = LowQuotaAlert::CHOICES
                    .iter()
                    .position(|value| *value == self.alert)
                    .unwrap_or(0);
                let count = LowQuotaAlert::CHOICES.len() as i8;
                let next = (current as i8 + step).rem_euclid(count);
                self.alert = LowQuotaAlert::CHOICES[next as usize];
            }
            Row::Choice(Choice::Interval) => {
                let current = INTERVALS
                    .iter()
                    .position(|value| *value == self.interval_seconds)
                    .unwrap_or(1);
                let count = INTERVALS.len() as i8;
                let next = (current as i8 + step).rem_euclid(count);
                self.interval_seconds = INTERVALS[next as usize];
            }
            Row::Field(field) => self.fields = self.fields.toggled(field),
            Row::Agent(harness) => {
                if let Some(index) = AgentSelection::SUPPORTED
                    .iter()
                    .position(|supported| *supported == harness)
                {
                    self.agents[index] = !self.agents[index];
                }
            }
        }
    }

    /// The `configure --apply` invocation that makes this installation match
    /// the pane.
    ///
    /// Every value is passed explicitly, so applying cannot inherit a stale
    /// preference from an earlier installer run.
    fn apply_arguments(self) -> Vec<String> {
        let mut arguments = vec![
            "configure".to_string(),
            "--apply".to_string(),
            "--agent".to_string(),
            agent_list(&self.agents()),
            "--quota-percent".to_string(),
            self.percent.as_str().to_string(),
            "--sidebar-layout".to_string(),
            self.layout.as_str().to_string(),
            "--row-gap".to_string(),
            self.gap.to_string(),
            "--brand-colors".to_string(),
            self.brand.as_str().to_string(),
            "--fields".to_string(),
            self.fields.as_list(),
            "--agent-order".to_string(),
            self.order.as_str().to_string(),
            "--low-quota-alert".to_string(),
            self.alert.to_string(),
        ];
        arguments.push("--watch-interval-seconds".to_string());
        arguments.push(self.interval_seconds.to_string());
        arguments
    }

    /// Removing an agent is an uninstall of that agent's collector, not a
    /// narrower install: its statusLine entry and hook file have to be given
    /// back before the remaining agents are re-applied.
    fn uninstall_arguments(removed: &[Harness]) -> Vec<String> {
        vec![
            "configure".to_string(),
            "--uninstall".to_string(),
            "--agent".to_string(),
            agent_list(removed),
        ]
    }
}

fn agent_name(harness: Harness) -> &'static str {
    match harness {
        Harness::Claude => "claude",
        Harness::Codex => "codex",
        Harness::Grok => "grok",
        Harness::Agy => "agy",
        Harness::OpenCode => "opencode",
        Harness::Pi => "pi",
        Harness::Omp => "omp",
    }
}

fn agent_list(agents: &[Harness]) -> String {
    agents
        .iter()
        .copied()
        .map(agent_name)
        .collect::<Vec<_>>()
        .join(",")
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
        print!("{}", render(&settings, settings, 0, 24, None));
        return Ok(());
    }
    enable_raw_mode()?;
    let result = interactive(settings);
    let _ = disable_raw_mode();
    result
}

fn interactive(applied: Settings) -> Result<()> {
    let rows = rows();
    let mut applied = applied;
    let mut draft = applied;
    let mut selected = first_selectable(&rows);
    let mut status: Option<String> = None;
    let mut confirming = false;
    let mut painted: Option<String> = None;
    loop {
        let height = crossterm::terminal::size()
            .map(|(_, rows)| rows)
            .unwrap_or(24);
        let frame = render(&draft, applied, selected, height, status.as_deref());
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
            KeyCode::Up | KeyCode::Char('k') => selected = step_selection(&rows, selected, -1),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                selected = step_selection(&rows, selected, 1)
            }
            KeyCode::Left | KeyCode::Char('h') => {
                draft.cycle(rows[selected], -1);
                status = None;
                confirming = false;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                draft.cycle(rows[selected], 1);
                status = None;
                confirming = false;
            }
            KeyCode::Char('a') | KeyCode::Enter => {
                let (next_status, confirmed) = attempt_apply(draft, &mut applied, confirming);
                status = Some(next_status);
                confirming = confirmed;
            }
            _ => {}
        }
    }
}

/// One `a` press. Returns the line to show and whether the next press is a
/// confirmation of an agent removal.
fn attempt_apply(draft: Settings, applied: &mut Settings, confirming: bool) -> (String, bool) {
    if draft == *applied {
        return ("Nothing to apply.".to_string(), false);
    }
    if draft.agents().is_empty() {
        return ("Keep at least one agent.".to_string(), false);
    }
    let removed = draft.removed_agents(*applied);
    if !removed.is_empty() && !confirming {
        return (
            format!(
                "Removing {} restores its own config. Press a again.",
                agent_list(&removed)
            ),
            true,
        );
    }
    match apply(draft, &removed) {
        Ok(()) => {
            *applied = draft;
            (
                "Applied. Restart running agent panes to reload hooks.".to_string(),
                false,
            )
        }
        Err(error) => (format!("Failed: {error}"), false),
    }
}

/// Write the configuration, make Herdr re-read it, then republish the tokens.
///
/// `configure` runs as a child process rather than in-process: it prints a
/// report, and this screen is in raw mode. The child inherits Herdr's plugin
/// environment, which is what lets it write at all.
fn apply(settings: Settings, removed: &[Harness]) -> Result<()> {
    let executable = std::env::current_exe().context("resolve plugin executable")?;
    if !removed.is_empty() {
        run_self(&executable, &Settings::uninstall_arguments(removed))?;
    }
    // A Herdr plugin action runs a fixed command line, so the agent selection
    // has to be stored where a later "Install / repair" will find it.
    prefs::write(prefs::AGENTS, &agent_list(&settings.agents()))?;
    run_self(&executable, &settings.apply_arguments())?;

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
    // Reloading redraws the rows; the tokens inside them are only republished
    // by a refresh, so a changed percentage style would otherwise wait for the
    // next agent event.
    run_self(
        &executable,
        &[
            "refresh".to_string(),
            "--provider".to_string(),
            "all".to_string(),
            "--force".to_string(),
        ],
    )
}

fn run_self(executable: &std::path::Path, arguments: &[String]) -> Result<()> {
    let output = Command::new(executable)
        .args(arguments)
        .output()
        .with_context(|| {
            format!(
                "run {}",
                arguments.first().map_or("configure", String::as_str)
            )
        })?;
    if !output.status.success() {
        anyhow::bail!(
            "{}",
            first_line(&String::from_utf8_lossy(&output.stderr))
                .unwrap_or_else(|| "configure failed".to_string())
        );
    }
    Ok(())
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(56).collect())
}

fn first_selectable(rows: &[Row]) -> usize {
    rows.iter()
        .position(|row| !matches!(row, Row::Header(_)))
        .unwrap_or(0)
}

/// Move the cursor by one selectable row, skipping headers and wrapping.
fn step_selection(rows: &[Row], selected: usize, step: isize) -> usize {
    let count = rows.len() as isize;
    let mut index = selected as isize;
    for _ in 0..rows.len() {
        index = (index + step).rem_euclid(count);
        if !matches!(rows[index as usize], Row::Header(_)) {
            return index as usize;
        }
    }
    selected
}

/// The whole frame. Lines end in `\r\n` because Herdr's pane is a raw-mode
/// PTY, where a bare `\n` leaves the cursor at the column it was in.
///
/// More rows than fit scroll: the popup is 24 lines tall by default and the
/// list is longer than that, so the visible window follows the selection.
fn render(
    draft: &Settings,
    applied: Settings,
    selected: usize,
    height: u16,
    status: Option<&str>,
) -> String {
    let rows = rows();
    // The Herdr-managed pane border already carries the title. Reserve only
    // the two scroll markers, help line, and status line here, whether or not
    // each is drawn: the window must fit in the worst case.
    let chrome = 4;
    let visible = (height as usize).saturating_sub(chrome).max(6);
    let start = scroll_start(selected, rows.len(), visible);
    let end = (start + visible).min(rows.len());

    let mut output = String::new();
    if start > 0 {
        output.push_str("  \u{2191} more\r\n");
    }
    for (index, row) in rows.iter().enumerate().take(end).skip(start) {
        output.push_str(&render_row(draft, applied, *row, index == selected));
    }
    if end < rows.len() {
        output.push_str("  \u{2193} more\r\n");
    }
    output.push_str("  \u{2191}\u{2193} row  \u{2190}\u{2192}/space value  a apply  q quit\r\n");
    if let Some(status) = status {
        output.push_str(&format!("  {status}\r\n"));
    }
    output
}

/// Keep the selection inside the window, scrolling only when it leaves.
fn scroll_start(selected: usize, count: usize, visible: usize) -> usize {
    if count <= visible {
        return 0;
    }
    let half = visible / 2;
    selected.saturating_sub(half).min(count - visible)
}

fn render_row(draft: &Settings, applied: Settings, row: Row, selected: bool) -> String {
    let cursor = if selected { '>' } else { ' ' };
    match row {
        // Headers cost exactly one line, like every other row: the visible
        // window is counted in rows, and a header that also drew a blank line
        // would push the last agent off a 24-line pane.
        Row::Header(title) => format!("  \u{2500} {title} \u{2500}\u{2500}\u{2500}\r\n"),
        Row::Choice(choice) => {
            let changed = marker(draft.choice_value(choice) == applied.choice_value(choice));
            format!(
                "{cursor} {changed} {:<15} < {:^9} > {}\r\n",
                choice.label(),
                draft.choice_value(choice),
                draft.choice_hint(choice)
            )
        }
        Row::Field(field) => {
            let changed = marker(draft.fields.contains(field) == applied.fields.contains(field));
            format!(
                "{cursor} {changed} {} {}\r\n",
                checkbox(draft.fields.contains(field)),
                field.name()
            )
        }
        Row::Agent(harness) => {
            let changed = marker(draft.has_agent(harness) == applied.has_agent(harness));
            format!(
                "{cursor} {changed} {} {}\r\n",
                checkbox(draft.has_agent(harness)),
                agent_name(harness)
            )
        }
    }
}

fn marker(unchanged: bool) -> char {
    if unchanged {
        ' '
    } else {
        '*'
    }
}

fn checkbox(on: bool) -> &'static str {
    if on {
        "[x]"
    } else {
        "[ ]"
    }
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
            brand: BrandColors::On,
            order: AgentOrder::Default,
            alert: LowQuotaAlert::OFF,
            fields: FieldSet::all(),
            agents: [true; AgentSelection::SUPPORTED.len()],
        }
    }

    #[test]
    fn every_offered_interval_is_one_configure_accepts() {
        for seconds in INTERVALS {
            assert!(
                CacheStore::validate_watch_interval_seconds(seconds).is_ok(),
                "{seconds}s is outside the range configure accepts"
            );
        }
    }

    #[test]
    fn two_way_options_flip_whichever_arrow_is_pressed() {
        let mut draft = settings();
        draft.cycle(Row::Choice(Choice::Percent), -1);
        assert_eq!(draft.percent, PercentStyle::Used);
        draft.cycle(Row::Choice(Choice::Percent), 1);
        assert_eq!(draft.percent, PercentStyle::Remaining);

        draft.cycle(Row::Choice(Choice::Layout), 1);
        assert_eq!(draft.layout, SidebarLayout::Stacked);
        draft.cycle(Row::Choice(Choice::RowGap), 1);
        assert_eq!(draft.gap, SidebarRowGap::FLUSH);
        draft.cycle(Row::Choice(Choice::Brand), 1);
        assert_eq!(draft.brand, BrandColors::Off);
    }

    #[test]
    fn the_interval_wraps_at_both_ends_of_the_offered_list() {
        let mut draft = settings();
        draft.cycle(Row::Choice(Choice::Interval), -1);
        assert_eq!(draft.interval_seconds, INTERVALS[0]);
        draft.cycle(Row::Choice(Choice::Interval), -1);
        assert_eq!(draft.interval_seconds, INTERVALS[INTERVALS.len() - 1]);
        draft.cycle(Row::Choice(Choice::Interval), 1);
        assert_eq!(draft.interval_seconds, INTERVALS[0]);
    }

    /// An unknown stored interval (`configure` accepts any value in range)
    /// must not trap the list: the first press lands on a known entry.
    #[test]
    fn an_interval_outside_the_offered_list_still_moves() {
        let mut draft = settings();
        draft.interval_seconds = 45;
        draft.cycle(Row::Choice(Choice::Interval), 1);
        assert_eq!(draft.interval_seconds, INTERVALS[2]);
    }

    #[test]
    fn toggling_a_field_and_an_agent_changes_only_that_one() {
        let mut draft = settings();
        draft.cycle(Row::Field(SidebarField::Cache), 1);
        assert!(!draft.fields.contains(SidebarField::Cache));
        assert!(draft.fields.contains(SidebarField::Ttl));

        draft.cycle(Row::Agent(Harness::Grok), 1);
        assert!(!draft.has_agent(Harness::Grok));
        assert!(draft.has_agent(Harness::Claude));
    }

    /// Applying names every value, so it cannot inherit a stale preference,
    /// and it names the agents so a narrowed selection is what gets installed.
    #[test]
    fn applying_names_every_value_including_the_agent_selection() {
        let mut draft = settings();
        draft.cycle(Row::Choice(Choice::Percent), 1);
        draft.cycle(Row::Field(SidebarField::Topic), 1);
        draft.cycle(Row::Agent(Harness::Pi), 1);
        assert_eq!(
            draft.apply_arguments(),
            vec![
                "configure",
                "--apply",
                "--agent",
                "claude,codex,grok,agy,opencode,omp",
                "--quota-percent",
                "used",
                "--sidebar-layout",
                "packed",
                "--row-gap",
                "1",
                "--brand-colors",
                "on",
                "--fields",
                "model,cache,ttl,context,5h,7d",
                "--agent-order",
                "default",
                "--low-quota-alert",
                "off",
                "--watch-interval-seconds",
                "60",
            ]
        );
    }

    /// Turning every field off is a real choice, and `configure` accepts the
    /// word for it — an empty `--fields` value would read as "not set".
    #[test]
    fn an_empty_field_selection_is_passed_as_none() {
        let mut draft = settings();
        for field in SidebarField::ALL {
            draft.cycle(Row::Field(field), 1);
        }
        assert!(draft.apply_arguments().contains(&"none".to_string()));
    }

    /// Removing an agent gives its statusLine back before the rest are
    /// re-applied, and it takes a second keypress to get there.
    #[test]
    fn removing_an_agent_needs_confirmation_and_uninstalls_that_agent() {
        let applied = settings();
        let mut draft = applied;
        draft.cycle(Row::Agent(Harness::Claude), 1);
        assert_eq!(draft.removed_agents(applied), vec![Harness::Claude]);
        assert_eq!(
            Settings::uninstall_arguments(&draft.removed_agents(applied)),
            vec!["configure", "--uninstall", "--agent", "claude"]
        );

        let mut current = applied;
        let (message, confirming) = attempt_apply(draft, &mut current, false);
        assert!(message.contains("Press a again"), "{message}");
        assert!(confirming);
        // Nothing was applied while the question was open.
        assert_eq!(current, applied);
    }

    /// An empty agent list would uninstall everything through a path meant for
    /// narrowing, so the pane refuses it before `configure` ever runs.
    #[test]
    fn applying_with_no_agent_selected_is_refused() {
        let applied = settings();
        let mut draft = applied;
        for harness in AgentSelection::SUPPORTED {
            draft.cycle(Row::Agent(harness), 1);
        }
        let mut current = applied;
        let (message, confirming) = attempt_apply(draft, &mut current, true);
        assert_eq!(message, "Keep at least one agent.");
        assert!(!confirming);
        assert_eq!(current, applied);
    }

    #[test]
    fn navigation_skips_section_headers_and_wraps() {
        let rows = rows();
        let first = first_selectable(&rows);
        assert!(matches!(rows[first], Row::Choice(Choice::Percent)));
        assert!(!matches!(
            rows[step_selection(&rows, first, -1)],
            Row::Header(_)
        ));
        let mut index = first;
        for _ in 0..rows.len() * 2 {
            index = step_selection(&rows, index, 1);
            assert!(!matches!(rows[index], Row::Header(_)));
        }
    }

    #[test]
    fn unapplied_edits_are_marked_and_the_frame_fits_the_pane() {
        let applied = settings();
        let mut draft = applied;
        draft.cycle(Row::Choice(Choice::Layout), 1);
        let frame = render(&draft, applied, 2, 24, Some("Nothing to apply."));
        assert!(frame.contains("> * Sidebar layout"), "{frame}");
        assert!(frame.contains("stacked"), "{frame}");
        assert!(!frame.contains("Agent quota settings"), "{frame}");
        // The frame ends in a line break, so the split leaves a trailing "".
        let lines: Vec<&str> = frame.trim_end_matches("\r\n").split("\r\n").collect();
        assert!(lines.len() <= 24, "{} lines:\n{frame}", lines.len());
        for line in lines {
            assert!(!line.contains('\n'), "{frame}");
            assert!(line.chars().count() <= 70, "too wide: {line}");
        }
    }

    /// A selection below the fold has to bring its own row into view.
    #[test]
    fn the_list_scrolls_to_keep_the_selection_visible() {
        let rows = rows();
        let last = rows.len() - 1;
        let frame = render(&settings(), settings(), last, 24, None);
        assert!(frame.contains("pi"), "{frame}");
        assert!(frame.contains("\u{2191} more"), "{frame}");
        let top = render(&settings(), settings(), first_selectable(&rows), 24, None);
        assert!(top.contains("\u{2193} more"), "{top}");
    }

    #[test]
    fn intervals_read_as_minutes_when_they_divide_evenly() {
        assert_eq!(format_interval(30), "30s");
        assert_eq!(format_interval(60), "1m");
        assert_eq!(format_interval(3_600), "60m");
    }
}
