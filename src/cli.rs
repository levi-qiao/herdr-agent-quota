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
    /// Herdr startup hook: restore plugin-owned Herdr state, then refresh.
    ///
    /// Herdr's Agent view is dropped when the server exits, and startup hooks
    /// run again after a restart or a live handoff, so this is where a
    /// configured agent order is put back. Invoked by the plugin's startup
    /// hook; a manual `refresh` is still the way to just fetch quota.
    Startup {
        /// Providers to refresh once the restored state is in place.
        #[arg(long, default_value = "all")]
        provider: ProviderSelection,
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
        /// Agents to configure: all, claude, codex, grok, agy, opencode, pi,
        /// omp, devin. Repeat or comma-separate to pick several. Defaults to
        /// every supported agent (or $HERDR_AGENT_QUOTA_AGENTS when set), so
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
        /// Whether quota percentages read as remaining (default) or used.
        /// Herdr plugin actions run a fixed command line, so install.sh
        /// passes this through $HERDR_AGENT_QUOTA_PERCENT.
        #[arg(long, value_enum)]
        quota_percent: Option<PercentStyle>,
        /// Quota fields the sidebar shows: all (default), none, or a
        /// comma-separated list of topic, model, cache, ttl, context, 5h, 7d.
        /// Provider and the error token are always shown.
        #[arg(long, value_parser = parse_field_set)]
        fields: Option<FieldSet>,
        /// Whether provider and model carry each agent's brand hue. Severity
        /// colours are unaffected.
        #[arg(long, value_enum)]
        brand_colors: Option<BrandColors>,
        /// Blank rows between agent panes. `1` (default) separates them;
        /// `0` packs them flush. Herdr only accepts whole rows. install.sh
        /// writes this to the plugin config directory because plugin actions
        /// run a fixed command line.
        #[arg(long, value_parser = parse_row_gap)]
        row_gap: Option<SidebarRowGap>,
        /// How Herdr's Agent panel is ordered: default (Herdr's own policy)
        /// or quota (least quota left first). `quota` installs a Herdr agent
        /// view owned by this plugin and replaces the user's panel sort until
        /// it is set back to default.
        #[arg(long, value_enum)]
        agent_order: Option<AgentOrder>,
        /// Notify once when a provider's remaining quota falls to this
        /// percentage or below. `off` (default) never notifies.
        #[arg(long, value_parser = parse_low_quota_alert)]
        low_quota_alert: Option<LowQuotaAlert>,
    },
    /// Render the settings pane shown in the Herdr popup pane.
    Settings,
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
    Devin,
}

impl ProviderSelection {
    pub fn providers(self) -> Vec<Provider> {
        match self {
            Self::All => Provider::ALL.to_vec(),
            Self::Codex => vec![Provider::Codex],
            Self::Grok => vec![Provider::Grok],
            Self::Claude => vec![Provider::Claude],
            Self::Agy => vec![Provider::Agy],
            Self::Devin => vec![Provider::Devin],
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
    Omp,
    Devin,
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

/// A quota field the sidebar can be told to leave out.
///
/// Provider is not here: it is the identity of the row, and a row that cannot
/// say which subscription it belongs to is worse than no row. `$quota_error`
/// is not here either — it is how the plugin reports that it could not speak
/// for a pane at all, and hiding it would hide the failure, not the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarField {
    Topic,
    Model,
    Cache,
    Ttl,
    Context,
    FiveHour,
    Week,
}

impl SidebarField {
    pub const ALL: [Self; 7] = [
        Self::Topic,
        Self::Model,
        Self::Cache,
        Self::Ttl,
        Self::Context,
        Self::FiveHour,
        Self::Week,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Topic => "topic",
            Self::Model => "model",
            Self::Cache => "cache",
            Self::Ttl => "ttl",
            Self::Context => "context",
            Self::FiveHour => "5h",
            Self::Week => "7d",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|field| field.name() == name)
            .or(match name.as_str() {
                "week" => Some(Self::Week),
                "5h_limit" | "five_hour" => Some(Self::FiveHour),
                _ => None,
            })
    }

    fn bit(self) -> u8 {
        1 << Self::ALL
            .iter()
            .position(|field| *field == self)
            .unwrap_or(0)
    }
}

/// Which quota fields the sidebar shows. Every field is on by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSet(u8);

impl FieldSet {
    pub const ENV: &'static str = "HERDR_AGENT_QUOTA_FIELDS";

    pub fn all() -> Self {
        Self(
            SidebarField::ALL
                .iter()
                .fold(0, |bits, field| bits | field.bit()),
        )
    }

    pub fn contains(self, field: SidebarField) -> bool {
        self.0 & field.bit() != 0
    }

    pub fn toggled(self, field: SidebarField) -> Self {
        Self(self.0 ^ field.bit())
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// A comma-separated list of the fields that are on, in `ALL` order.
    ///
    /// The empty selection is written as `none` rather than an empty string,
    /// which every preference reader treats as "not set".
    pub fn as_list(self) -> String {
        if self.is_empty() {
            return "none".to_string();
        }
        SidebarField::ALL
            .into_iter()
            .filter(|field| self.contains(*field))
            .map(SidebarField::name)
            .collect::<Vec<_>>()
            .join(",")
    }

    /// `None` when nothing in the list is a field name, so an unparsable
    /// preference falls through to the next source rather than hiding
    /// everything.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.eq_ignore_ascii_case("all") {
            return Some(Self::all());
        }
        if raw.eq_ignore_ascii_case("none") {
            return Some(Self(0));
        }
        let bits = raw
            .split(',')
            .filter_map(SidebarField::parse)
            .fold(0, |bits, field| bits | field.bit());
        (bits != 0).then_some(Self(bits))
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

impl Default for FieldSet {
    fn default() -> Self {
        Self::all()
    }
}

fn parse_field_set(value: &str) -> Result<FieldSet, String> {
    FieldSet::parse(value).ok_or_else(|| {
        format!(
            "fields must be all, none, or a comma-separated list of: {}",
            SidebarField::ALL
                .into_iter()
                .map(SidebarField::name)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// Whether provider and model carry each agent's brand hue.
///
/// Herdr owns the sidebar theme; this is the only colour the plugin writes of
/// its own, so it is the only colour it can offer to turn off. Severity
/// colours stay in both settings: they are information, not decoration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum BrandColors {
    #[default]
    On,
    Off,
}

impl BrandColors {
    pub const ENV: &'static str = "HERDR_AGENT_QUOTA_BRAND_COLORS";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }

    pub fn is_on(self) -> bool {
        self == Self::On
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "on" | "brand" | "true" => Some(Self::On),
            "off" | "plain" | "false" => Some(Self::Off),
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

/// Every option `configure` accepts, from any of its channels.
///
/// They travel together because they are resolved together: a flag wins, then
/// the environment, then the stored preference, then the last applied value.
/// Grouping them keeps that resolution in one place as options are added.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConfigureOptions {
    pub watch_interval_seconds: Option<u64>,
    pub sidebar_layout: Option<SidebarLayout>,
    pub quota_percent: Option<PercentStyle>,
    pub row_gap: Option<SidebarRowGap>,
    pub fields: Option<FieldSet>,
    pub brand_colors: Option<BrandColors>,
    pub agent_order: Option<AgentOrder>,
    pub low_quota_alert: Option<LowQuotaAlert>,
}

/// Which side of a quota window a percentage reports.
///
/// The severity colour is always computed from the remaining quota, so a red
/// token means "little left" in both styles; only the number flips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum PercentStyle {
    /// `5h 42%` — how much of the window is still available.
    #[default]
    Remaining,
    /// `5h 58%` — how much of the window has been consumed.
    Used,
}

impl PercentStyle {
    pub const ENV: &'static str = "HERDR_AGENT_QUOTA_PERCENT";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Remaining => "remaining",
            Self::Used => "used",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "remaining" | "left" => Some(Self::Remaining),
            "used" => Some(Self::Used),
            _ => None,
        }
    }

    /// The percentage this style shows for a window that is `used` percent
    /// consumed. Every window carries both numbers, so this is a choice of
    /// field, not a second calculation that could drift.
    pub fn percent_of(self, window: &crate::model::UsageWindow) -> f64 {
        match self {
            Self::Remaining => window.remaining_percent,
            Self::Used => window.used_percent,
        }
    }

    /// The word the dashboard puts after the number. The sidebar omits it:
    /// a narrow sidebar truncates, and the style is the user's own choice.
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Remaining => "left",
            Self::Used => "used",
        }
    }

    /// Flag wins; otherwise the installer environment; otherwise unset, and
    /// `configure` falls back to the stored preference.
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

/// How Herdr's Agent panel is ordered.
///
/// `quota` hands Herdr a declarative Agent view sorted by this plugin's
/// `quota_headroom` token, so the agent closest to its limit sits at the top.
/// Herdr keeps exactly one such view, and an active one replaces the user's
/// own `ui.agent_panel_sort` policy until it is cleared. That is why the
/// default is `default`: the panel belongs to the user, not to this plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum AgentOrder {
    /// Leave Herdr's own ordering alone.
    #[default]
    Default,
    /// Least quota left first.
    Quota,
}

impl AgentOrder {
    pub const ENV: &'static str = "HERDR_AGENT_QUOTA_AGENT_ORDER";
    /// Herdr's label for the view, shown where it names the active sort.
    pub const LABEL: &'static str = "Quota headroom";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Quota => "quota",
        }
    }

    pub fn is_quota(self) -> bool {
        self == Self::Quota
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "default" | "herdr" | "off" => Some(Self::Default),
            "quota" | "headroom" | "on" => Some(Self::Quota),
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

/// The remaining-quota percentage at or below which a provider gets one
/// desktop notification. `0` disables the alert entirely.
///
/// A threshold rather than a boolean because the useful warning point differs
/// per plan: 20% of a weekly window is hours of work, 5% is minutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LowQuotaAlert(u8);

impl LowQuotaAlert {
    pub const ENV: &'static str = "HERDR_AGENT_QUOTA_LOW_ALERT";
    pub const OFF: Self = Self(0);
    /// The thresholds the settings pane cycles through, `OFF` first.
    pub const CHOICES: [Self; 4] = [Self(0), Self(20), Self(10), Self(5)];

    pub fn as_u8(self) -> u8 {
        self.0
    }

    pub fn is_off(self) -> bool {
        self.0 == 0
    }

    /// Does `remaining` percent sit at or below the alert threshold?
    pub fn triggers(self, remaining: u8) -> bool {
        !self.is_off() && remaining <= self.0
    }

    pub fn parse(name: &str) -> Option<Self> {
        let trimmed = name.trim().trim_end_matches('%');
        if matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "off" | "none" | "false"
        ) {
            return Some(Self::OFF);
        }
        let value: u8 = trimmed.parse().ok()?;
        (value <= 100).then_some(Self(value))
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

impl Default for LowQuotaAlert {
    fn default() -> Self {
        Self::OFF
    }
}

impl std::fmt::Display for LowQuotaAlert {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_off() {
            return write!(formatter, "off");
        }
        write!(formatter, "{}%", self.0)
    }
}

fn parse_low_quota_alert(value: &str) -> Result<LowQuotaAlert, String> {
    LowQuotaAlert::parse(value)
        .ok_or_else(|| "low-quota-alert must be off or a percentage from 1 to 100".to_string())
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
    pub const SUPPORTED: [Harness; 8] = [
        Harness::Claude,
        Harness::Codex,
        Harness::Grok,
        Harness::Agy,
        Harness::OpenCode,
        Harness::Pi,
        Harness::Omp,
        Harness::Devin,
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
            Self::Omp => Some(Harness::Omp),
            Self::Devin => Some(Harness::Devin),
        }
    }

    /// Selection for a `configure` run.
    ///
    /// A Herdr plugin action runs a fixed command line in the *server's*
    /// environment, so a variable exported around `herdr plugin action invoke`
    /// never reaches it. The plugin config directory is the channel that does
    /// work, and it is what `install.sh` writes; the environment is still
    /// honoured first for a direct CLI run.
    ///
    /// Anything unparsable falls through to the next source and finally to
    /// every supported agent, so `--uninstall` alone still removes everything.
    pub fn from_args_or_env(values: &[Self]) -> Vec<Harness> {
        if !values.is_empty() {
            return Self::resolve(values);
        }
        [
            std::env::var("HERDR_AGENT_QUOTA_AGENTS").ok(),
            crate::prefs::read(crate::prefs::AGENTS),
        ]
        .into_iter()
        .flatten()
        .find_map(|raw| Self::parse_list(&raw))
        .unwrap_or_else(|| Self::SUPPORTED.to_vec())
    }

    /// A comma-separated selection, or `None` when it names nothing valid.
    fn parse_list(raw: &str) -> Option<Vec<Harness>> {
        let parsed: Vec<Self> = raw
            .split(',')
            .filter_map(|name| Self::parse(name.trim()))
            .collect();
        (!parsed.is_empty()).then(|| Self::resolve(&parsed))
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
            "omp" => Some(Self::Omp),
            "devin" => Some(Self::Devin),
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

    #[test]
    fn an_agent_order_round_trips_through_its_stored_form() {
        for order in [AgentOrder::Default, AgentOrder::Quota] {
            assert_eq!(AgentOrder::parse(order.as_str()), Some(order));
        }
        assert_eq!(AgentOrder::parse(" QUOTA "), Some(AgentOrder::Quota));
        assert_eq!(AgentOrder::parse("sideways"), None);
        assert_eq!(AgentOrder::default(), AgentOrder::Default);
    }

    #[test]
    fn a_low_quota_alert_round_trips_through_its_stored_form() {
        for alert in LowQuotaAlert::CHOICES {
            assert_eq!(LowQuotaAlert::parse(&alert.to_string()), Some(alert));
        }
        assert_eq!(LowQuotaAlert::parse("off"), Some(LowQuotaAlert::OFF));
        assert_eq!(LowQuotaAlert::parse("10%"), LowQuotaAlert::parse("10"));
        assert_eq!(LowQuotaAlert::parse("101"), None);
        assert_eq!(LowQuotaAlert::parse("later"), None);
        assert!(LowQuotaAlert::default().is_off());
    }

    /// Off is a threshold like any other in the type, and has to stay silent
    /// even at zero remaining.
    #[test]
    fn an_alert_that_is_off_never_triggers() {
        assert!(!LowQuotaAlert::OFF.triggers(0));
        let ten = LowQuotaAlert::parse("10").unwrap();
        assert!(ten.triggers(0));
        assert!(ten.triggers(10));
        assert!(!ten.triggers(11));
    }
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

    /// The environment cannot reach a Herdr plugin action, so the config-dir
    /// preference is the channel `install.sh` / `uninstall.sh` actually use.
    /// A selection that fails to arrive means `--uninstall --agent grok`
    /// removes every agent, so this path is load-bearing.
    #[test]
    fn a_config_directory_preference_narrows_the_selection() {
        let directory = tempfile::tempdir().unwrap();
        crate::prefs::testing::with_config_dir(directory.path(), || {
            crate::prefs::write(crate::prefs::AGENTS, "grok,claude").unwrap();
            assert_eq!(
                AgentSelection::from_args_or_env(&[]),
                vec![Harness::Claude, Harness::Grok]
            );

            // An explicit flag still wins over the stored preference.
            assert_eq!(
                AgentSelection::from_args_or_env(&[AgentSelection::Agy]),
                vec![Harness::Agy]
            );

            // Junk falls through to every agent rather than to none, so a
            // corrupt file can never silently skip an uninstall.
            crate::prefs::write(crate::prefs::AGENTS, "nonsense").unwrap();
            assert_eq!(
                AgentSelection::from_args_or_env(&[]),
                AgentSelection::SUPPORTED.to_vec()
            );
        });
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
    fn percent_style_parses_both_names_and_defaults_to_remaining() {
        assert_eq!(PercentStyle::parse("used"), Some(PercentStyle::Used));
        assert_eq!(
            PercentStyle::parse("Remaining"),
            Some(PercentStyle::Remaining)
        );
        // `left` is the word the dashboard prints, so accept it as an alias.
        assert_eq!(PercentStyle::parse("left"), Some(PercentStyle::Remaining));
        assert_eq!(PercentStyle::parse("nonsense"), None);
        assert_eq!(PercentStyle::default(), PercentStyle::Remaining);
        assert_eq!(
            PercentStyle::from_arg_or_env(Some(PercentStyle::Used)),
            Some(PercentStyle::Used)
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
