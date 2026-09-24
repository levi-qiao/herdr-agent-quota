use crate::cli::{FieldSet, PercentStyle, SidebarField, SidebarLayout, SidebarPacing};
use crate::model::{
    format_percent, live_windows, printed_percent, window_in, Provider, ProviderSnapshot, ResetAt,
    Severity, UsageWindow, WindowKind,
};

/// Three-character label, three spaces, `100%`, and a six-character ETA.
const METER_ROW_OVERHEAD: usize = 16;
/// Herdr 0.9 secondary-row indent (3) plus scrollbar (1). The panel divider
/// sits outside the sidebar body. Reserve the scrollbar even when hidden so
/// adding an agent does not clip rows.
const SIDEBAR_CHROME_WIDTH: usize = 4;
/// Past a dozen cells a bar stops being read at a glance and becomes texture.
const MAX_METER_CELLS: usize = 12;
/// Below four cells the bar cannot distinguish enough levels to be worth the
/// columns it costs, so the row keeps its non-gauge shape instead.
const MIN_METER_CELLS: usize = 4;
// `\u{25b0}` and `\u{25b1}` are East-Asian-Width Neutral, so both are one
// column wide in a CJK locale and identical in width to each other.
const METER_FILLED: char = '\u{25b0}';
const METER_EMPTY: char = '\u{25b1}';
/// Align `cx`, `5h`, `7d`, and `30d` without inventing period aliases.
const GAUGE_LABEL_WIDTH: usize = 3;
const GAUGE_CONTEXT_LABEL: &str = "cx";
/// A Claude percentage older than this is no longer allowed to act like
/// current headroom. This is a product safety policy, not statusLine's redraw
/// interval: redraws can happen without another provider response.
const CLAUDE_QUOTA_STALE_AFTER_SECONDS: u64 = 2 * 60;
/// How many meter cells a window row can afford at `sidebar_width` columns,
/// or `None` when the row should render through its existing non-gauge shape
/// rather than lose the number the bar labels to truncation.
pub(crate) fn meter_cells(sidebar_width: usize) -> Option<usize> {
    let cells = sidebar_width
        .saturating_sub(SIDEBAR_CHROME_WIDTH + METER_ROW_OVERHEAD)
        .min(MAX_METER_CELLS);
    (cells >= MIN_METER_CELLS).then_some(cells)
}

/// How the sidebar draws a row: the layout the user chose, and the meter size
/// their configured sidebar width affords. The two always travel together,
/// from the publish sites down to each rendered row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidebarShape {
    pub layout: SidebarLayout,
    pub meter_cells: Option<usize>,
    pub content_width: usize,
}

impl Default for SidebarShape {
    /// Layout-agnostic token identity stays packed. Gauges needs a measured
    /// width before it is gauges; `from_snapshot` without a shape must not
    /// rewrite historical compact values.
    fn default() -> Self {
        Self {
            layout: SidebarLayout::Packed,
            meter_cells: None,
            content_width: 0,
        }
    }
}

impl SidebarShape {
    pub fn new(layout: SidebarLayout, sidebar_width: usize) -> Self {
        Self {
            layout,
            meter_cells: meter_cells(sidebar_width),
            content_width: sidebar_width.saturating_sub(SIDEBAR_CHROME_WIDTH),
        }
    }
}

/// The two rendering knobs a pane's tokens are drawn with: whether a percent
/// reads remaining or used, and the shape of the row it sits in. They are
/// chosen together once per refresh pass and never vary between panes, so
/// they travel as one value rather than as two parallel parameters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RowStyle {
    pub percent: PercentStyle,
    pub shape: SidebarShape,
    pub fields: FieldSet,
    pub pacing: SidebarPacing,
}

impl RowStyle {
    pub fn new(percent: PercentStyle, shape: SidebarShape) -> Self {
        Self {
            percent,
            shape,
            fields: FieldSet::all(),
            pacing: SidebarPacing::default(),
        }
    }
}

impl From<SidebarLayout> for SidebarShape {
    fn from(layout: SidebarLayout) -> Self {
        Self {
            layout,
            meter_cells: None,
            content_width: 0,
        }
    }
}

/// A meter that agrees with the integer printed beside it: only 0
/// draws an empty bar and only 100 draws a full one, so a window with quota
/// left never reads as spent.
fn meter(printed: u32, cells: usize) -> String {
    let filled = (f64::from(printed) * cells as f64 / 100.0).round() as usize;
    let filled = if (1..=99).contains(&printed) {
        filled.clamp(1, cells - 1)
    } else {
        filled.min(cells)
    };
    std::iter::repeat_n(METER_FILLED, filled)
        .chain(std::iter::repeat_n(METER_EMPTY, cells - filled))
        .collect()
}

/// How many meter cells a row labelled `label` draws, or `None` when it keeps
/// its existing non-gauge shape: another layout, a sidebar too narrow for a
/// bar, or a label that would not fit the label column.
fn gauge_cells(shape: SidebarShape, label: &str) -> Option<usize> {
    match shape.layout {
        SidebarLayout::Packed | SidebarLayout::Stacked => None,
        SidebarLayout::Gauges => shape
            .meter_cells
            .filter(|_| label.is_ascii() && label.len() <= GAUGE_LABEL_WIDTH),
    }
}

/// Exactly the values a pane can be given.
///
/// Every field here is published by [`crate::herdr::desired_tokens`]. Nothing
/// is rendered "in case the sidebar wants it later": an unpublished field
/// still costs a name in Herdr's 16-token report budget, because that budget
/// is spent clearing names the plugin no longer sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataTokens {
    pub quota_provider: String,
    pub quota_model: String,
    pub quota_provider_model: String,
    /// One compact token per window (`5h 42% 4h07m`), severity chooses the hue.
    pub quota_5h: String,
    pub quota_5h_severity: Option<Severity>,
    pub quota_week: String,
    pub quota_week_severity: Option<Severity>,
    pub quota_month: String,
    pub quota_month_severity: Option<Severity>,
    pub quota_context: String,
    /// Read from context *left*, on the same bands as the window severities,
    /// whichever side of the ledger the row prints. Only `gauges` renders it.
    pub quota_context_severity: Option<Severity>,
    pub quota_cache: String,
    pub quota_cache_ttl: String,
    /// A lapsed prompt cache (`no cached`). Normal, unlike `quota_error`.
    pub quota_cache_state: String,
    /// The plugin could not speak for this pane at all.
    pub quota_error: Option<String>,
    /// Remaining quota in the tightest window this pane knows about, as a
    /// whole percent. `None` when no window reported one.
    ///
    /// The tightest window rather than the 5h one: whichever limit the user
    /// will hit first is the one worth sorting and warning on, and for a
    /// weekly plan that is often the 7d window.
    pub quota_headroom: Option<u8>,
}

impl MetadataTokens {
    pub fn from_snapshot(snapshot: &ProviderSnapshot, now_unix: u64) -> Self {
        Self::from_snapshot_for_session(
            snapshot,
            now_unix,
            None,
            PercentStyle::default(),
            SidebarShape::default(),
        )
    }

    pub fn from_snapshot_for_session(
        snapshot: &ProviderSnapshot,
        now_unix: u64,
        session_id: Option<&str>,
        style: PercentStyle,
        shape: SidebarShape,
    ) -> Self {
        Self::from_snapshot_parts(
            snapshot,
            now_unix,
            session_id,
            snapshot.model_for_session(session_id),
            snapshot.context_for_session(session_id),
            if session_id.is_none() {
                &snapshot.windows
            } else {
                snapshot.windows_for_session(session_id)
            },
            RowStyle::new(style, shape),
        )
    }

    /// Render a pane's tokens without broadcasting provider-local diagnostics
    /// when Herdr cannot identify that pane's session. A provider-level model
    /// is still useful for the identity row, but context/cache values are
    /// session data and must stay blank until their session id is known.
    ///
    /// Quota windows follow the same session lookup as model/context, except
    /// they fall back to the account-level snapshot when no session has
    /// reported windows (Grok/Codex, or a legacy StatusLine cache).
    pub fn from_snapshot_for_pane(
        snapshot: &ProviderSnapshot,
        now_unix: u64,
        session_id: Option<&str>,
        style: PercentStyle,
        shape: SidebarShape,
    ) -> Self {
        Self::from_snapshot_for_pane_with_fields(
            snapshot,
            now_unix,
            session_id,
            style,
            shape,
            FieldSet::all(),
        )
    }

    pub fn from_snapshot_for_pane_with_fields(
        snapshot: &ProviderSnapshot,
        now_unix: u64,
        session_id: Option<&str>,
        style: PercentStyle,
        shape: SidebarShape,
        fields: FieldSet,
    ) -> Self {
        Self::from_snapshot_for_pane_with_row(
            snapshot,
            now_unix,
            session_id,
            RowStyle {
                percent: style,
                shape,
                fields,
                pacing: SidebarPacing::default(),
            },
        )
    }

    pub fn from_snapshot_for_pane_with_row(
        snapshot: &ProviderSnapshot,
        now_unix: u64,
        session_id: Option<&str>,
        row: RowStyle,
    ) -> Self {
        let quota_model = match session_id {
            Some(session_id) => snapshot.model_for_session(Some(session_id)),
            None => snapshot.model.as_deref(),
        };
        let context =
            session_id.and_then(|session_id| snapshot.context_for_session(Some(session_id)));
        let windows = snapshot.windows_for_session(session_id);
        Self::from_snapshot_parts(
            snapshot,
            now_unix,
            session_id,
            quota_model,
            context,
            windows,
            row,
        )
    }

    fn from_snapshot_parts(
        snapshot: &ProviderSnapshot,
        now_unix: u64,
        session_id: Option<&str>,
        model: Option<&str>,
        context: Option<&crate::model::ContextUsage>,
        windows: &[UsageWindow],
        row: RowStyle,
    ) -> Self {
        let style = row.percent;
        let shape = row.shape;
        let fields = row.fields;
        let pacing = row.pacing;
        let live = live_windows(windows, now_unix);
        let windows = live.as_slice();
        let quota_provider = snapshot.provider.display_name().to_string();
        let quota_model = model.unwrap_or_default().to_string();
        let quota_provider_model =
            provider_model_label(&quota_provider, &quota_model, shape.content_width);
        let narrow_identity =
            shape.content_width > 0 && shape.content_width < NARROW_IDENTITY_CONTENT_WIDTH;
        let five_hour = window_in(windows, WindowKind::FiveHour);
        let weekly = window_in(windows, WindowKind::Weekly);
        let monthly = window_in(windows, WindowKind::Monthly);
        let five_hour_stale = five_hour
            .and_then(|_| stale_quota_age(snapshot, session_id, WindowKind::FiveHour, now_unix));
        let weekly_stale = weekly
            .and_then(|_| stale_quota_age(snapshot, session_id, WindowKind::Weekly, now_unix));
        let monthly_stale = monthly
            .and_then(|_| stale_quota_age(snapshot, session_id, WindowKind::Monthly, now_unix));
        let has_stale_quota = (fields.contains(SidebarField::FiveHour)
            && five_hour_stale.is_some())
            || (fields.contains(SidebarField::Week) && weekly_stale.is_some())
            || (fields.contains(SidebarField::Month) && monthly_stale.is_some());
        Self {
            quota_provider_model,
            quota_provider: if narrow_identity && !quota_model.is_empty() {
                String::new()
            } else {
                quota_provider
            },
            quota_model,
            quota_5h_severity: five_hour.map(|window| {
                if five_hour_stale.is_some() {
                    Severity::Unknown
                } else {
                    Severity::for_window(window, now_unix)
                }
            }),
            quota_5h: five_hour
                .map(|window| {
                    five_hour_stale.map_or_else(
                        || sidebar_window_parts(window, now_unix, style, shape, pacing).rendered(),
                        |age| stale_window(window, age),
                    )
                })
                .unwrap_or_default(),
            quota_week: weekly
                .map(|window| {
                    weekly_stale.map_or_else(
                        || sidebar_window_parts(window, now_unix, style, shape, pacing).rendered(),
                        |age| stale_window(window, age),
                    )
                })
                .unwrap_or_default(),
            quota_week_severity: weekly.map(|window| {
                if weekly_stale.is_some() {
                    Severity::Unknown
                } else {
                    Severity::for_window(window, now_unix)
                }
            }),
            quota_month: monthly
                .map(|window| {
                    monthly_stale.map_or_else(
                        || compact_window_parts(window, now_unix, style, shape).rendered(),
                        |age| stale_window(window, age),
                    )
                })
                .unwrap_or_default(),
            quota_month_severity: monthly.map(|window| {
                if monthly_stale.is_some() {
                    Severity::Unknown
                } else {
                    Severity::for_window(window, now_unix)
                }
            }),
            quota_context: sidebar_context(context, style, shape),
            quota_context_severity: context.map(|context| context_severity(context, style)),
            quota_cache: sidebar_cache(context),
            quota_cache_ttl: sidebar_cache_ttl(context, now_unix),
            quota_cache_state: sidebar_cache_state(context, now_unix),
            quota_error: None,
            // Once any displayed Claude window is stale, there is no current
            // provider headroom to sort or use for alert recovery. Returning
            // None also preserves an already-fired low-quota alert until a
            // genuinely fresh observation can re-arm it.
            quota_headroom: if has_stale_quota {
                None
            } else {
                headroom(windows, fields)
            },
        }
    }

    /// The plugin has a snapshot it must not show — currently only a snapshot
    /// belonging to a login the user has since switched away from. Window rows
    /// stay off rather than reading `N/A`, and `quota_error` says why.
    pub fn unavailable(provider: Provider, reason: impl Into<String>) -> Self {
        let quota_provider = provider.display_name().to_string();
        Self {
            quota_provider_model: quota_provider.clone(),
            quota_provider,
            quota_model: String::new(),
            quota_5h: String::new(),
            quota_5h_severity: None,
            quota_week: String::new(),
            quota_week_severity: None,
            quota_month: String::new(),
            quota_month_severity: None,
            quota_context: String::new(),
            quota_context_severity: None,
            quota_cache: String::new(),
            quota_cache_ttl: String::new(),
            quota_cache_state: String::new(),
            quota_error: Some(reason.into().chars().take(80).collect()),
            quota_headroom: None,
        }
    }

    /// Same as [`Self::unavailable`]. Window rows are omitted rather than
    /// filled with `N/A`, whether or not the unusable snapshot carried them.
    pub fn unavailable_for_windows(
        provider: Provider,
        reason: impl Into<String>,
        _windows: &[UsageWindow],
    ) -> Self {
        Self::unavailable(provider, reason)
    }
}

/// Return the age of a Claude session-local window once it is too old to
/// claim current headroom. `Some(None)` means legacy/unknown age: the row is
/// stale immediately because there is no proof of when the percentage was
/// observed. Other providers keep their existing semantics.
fn stale_quota_age(
    snapshot: &ProviderSnapshot,
    session_id: Option<&str>,
    kind: WindowKind,
    now_unix: u64,
) -> Option<Option<u64>> {
    if snapshot.provider != Provider::Claude || !snapshot.session_quota_only {
        return None;
    }
    let session_id = session_id?;
    let observation = snapshot.quota_observation_for_session(Some(session_id), kind);
    match observation.and_then(|observation| observation.observed_at_unix) {
        Some(observed_at) => {
            let age = now_unix.saturating_sub(observed_at);
            (age >= CLAUDE_QUOTA_STALE_AFTER_SECONDS).then_some(Some(age))
        }
        None => Some(None),
    }
}

fn stale_window(window: &UsageWindow, age_seconds: Option<u64>) -> String {
    let label = window.display_label();
    match age_seconds {
        Some(age_seconds) => format!("{label} stale {}", format_stale_age(age_seconds)),
        None => format!("{label} stale"),
    }
}

fn format_stale_age(seconds: u64) -> String {
    if seconds < 60 * 60 {
        format!("{}m", (seconds / 60).max(1))
    } else if seconds < 24 * 60 * 60 {
        format!("{}h", seconds / (60 * 60))
    } else {
        format!("{}d", seconds / (24 * 60 * 60))
    }
}

/// The least remaining quota across the windows the sidebar shows.
///
/// Deliberately the same set as the rendered tokens — 5h, 7d, and 30d — so a
/// sort or an alert can always be explained by a number the user can see.
///
/// Rounded down, so a window one point above a threshold is never rounded
/// onto the wrong side of it.
fn headroom(windows: &[UsageWindow], fields: FieldSet) -> Option<u8> {
    [
        fields
            .contains(SidebarField::FiveHour)
            .then(|| window_in(windows, WindowKind::FiveHour))
            .flatten(),
        fields
            .contains(SidebarField::Week)
            .then(|| window_in(windows, WindowKind::Weekly))
            .flatten(),
        fields
            .contains(SidebarField::Month)
            .then(|| window_in(windows, WindowKind::Monthly))
            .flatten(),
    ]
    .into_iter()
    .flatten()
    .map(|window| window.remaining_percent.clamp(0.0, 100.0).floor() as u8)
    .min()
}

/// Below this content width the logo already names the vendor, so the
/// identity label keeps only the model (radar-style narrow reading).
const NARROW_IDENTITY_CONTENT_WIDTH: usize = 22;

fn provider_model_label(provider: &str, model: &str, content_width: usize) -> String {
    if model.is_empty() {
        return provider.to_string();
    }
    if content_width > 0 && content_width < NARROW_IDENTITY_CONTENT_WIDTH {
        return model.to_string();
    }
    format!("{provider}/{model}")
}

/// Every window the collector reported, including a monthly one. The sidebar
/// publishes 5h, 7d, and 30d as separate tokens; a 30d value must never be
/// folded into a weekly one.
pub fn dashboard_summary(
    snapshot: &ProviderSnapshot,
    now_unix: u64,
    style: PercentStyle,
) -> String {
    let live = live_windows(&snapshot.windows, now_unix);
    windows_summary(
        &live,
        &[
            WindowKind::FiveHour,
            WindowKind::Weekly,
            WindowKind::Monthly,
        ],
        now_unix,
        true,
        style,
    )
}

fn windows_summary(
    windows: &[UsageWindow],
    kinds: &[WindowKind],
    now_unix: u64,
    include_suffix: bool,
    style: PercentStyle,
) -> String {
    kinds
        .iter()
        .filter_map(|kind| window_in(windows, *kind))
        .map(|window| format_window(window, now_unix, include_suffix, style))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The percentage the `gauges` context row prints under `style`.
///
/// One scale per sidebar: the context row joins a column with the window
/// rows, and a column of aligned numbers reads as one quantity, so it prints
/// the same side of the ledger they do.
fn context_percent(context: &crate::model::ContextUsage, style: PercentStyle) -> f64 {
    match style {
        PercentStyle::Remaining => 100.0 - context.used_percent,
        PercentStyle::Used => context.used_percent,
    }
}

/// The context row's colour, always read from headroom and never from the
/// number printed beside it. The meter still fills to that number, so under
/// `used` a filling bar runs green → amber → red and under `remaining` a
/// draining one does the same: colour means "how much is left" either way.
///
/// Classified on the integer the row prints, like [`Severity::for_window`],
/// so colour and number cannot disagree at a band edge.
pub(crate) fn context_severity(
    context: &crate::model::ContextUsage,
    style: PercentStyle,
) -> Severity {
    let printed = f64::from(printed_percent(context_percent(context, style)));
    let remaining = match style {
        PercentStyle::Remaining => printed,
        PercentStyle::Used => 100.0 - printed,
    };
    Severity::for_context_remaining(remaining)
}

pub(crate) fn sidebar_context(
    context: Option<&crate::model::ContextUsage>,
    style: PercentStyle,
    shape: SidebarShape,
) -> String {
    let Some(context) = context else {
        return String::new();
    };
    match gauge_cells(shape, GAUGE_CONTEXT_LABEL) {
        Some(cells) => {
            let percent = context_percent(context, style);
            format!(
                "{GAUGE_CONTEXT_LABEL:<width$} {} {:>3}%",
                meter(printed_percent(percent), cells),
                format_percent(percent),
                width = GAUGE_LABEL_WIDTH,
            )
        }
        None if shape.layout == SidebarLayout::Gauges => format!(
            "{GAUGE_CONTEXT_LABEL} {}%",
            format_percent(context_percent(context, style))
        ),
        // Historical layouts keep their consumption label.
        None => format!("context {}%", format_percent(context.used_percent)),
    }
}

pub(crate) fn sidebar_cache(context: Option<&crate::model::ContextUsage>) -> String {
    let Some(cache) = context.and_then(|context| context.cache.as_ref()) else {
        return String::new();
    };
    let hit_percent = cache
        .session_totals
        .as_ref()
        .map(|totals| totals.hit_percent)
        .unwrap_or(cache.hit_percent);
    format!("cache {:.1}%", hit_percent)
}

pub(crate) fn sidebar_cache_ttl(
    context: Option<&crate::model::ContextUsage>,
    now_unix: u64,
) -> String {
    let Some(cache) = context.and_then(|context| context.cache.as_ref()) else {
        return String::new();
    };
    cache
        .remaining_ttl_seconds(now_unix)
        .filter(|remaining| *remaining > 0)
        .map(|remaining| format!("ttl≈{}", format_ttl(remaining)))
        .unwrap_or_default()
}

/// A lapsed prompt cache is a normal state, not a failure.
///
/// It gets its own token so it is never confused with [`MetadataTokens::
/// unavailable`]'s `quota_error`, which reports that the plugin could not
/// speak for this pane at all. Both are amber, so sharing one token made
/// "your prefix went cold" indistinguishable from "quota is broken".
pub(crate) fn sidebar_cache_state(
    context: Option<&crate::model::ContextUsage>,
    now_unix: u64,
) -> String {
    context
        .and_then(|context| context.cache.as_ref())
        .and_then(|cache| cache.remaining_ttl_seconds(now_unix))
        .filter(|remaining| *remaining == 0)
        .map(|_| "no cached".to_string())
        .unwrap_or_default()
}

fn format_window(
    window: &UsageWindow,
    now_unix: u64,
    include_suffix: bool,
    style: PercentStyle,
) -> String {
    let percent = format!("{}%", format_percent(style.percent_of(window)));
    let suffix = if include_suffix {
        format!(" {}", style.suffix())
    } else {
        String::new()
    };
    let label = format!("{} {percent}{suffix}", window.display_label());
    let Some(reset) = window.resets_at else {
        return label;
    };
    let eta = format_reset_eta(reset, now_unix);
    format!("{label} reset {eta}")
}

struct WindowParts {
    label: String,
    percent: String,
    eta: String,
    /// Set only when this row draws a meter, so every caller of `rendered`
    /// picks up the gauge shape without deciding anything itself.
    meter: Option<String>,
}

impl WindowParts {
    /// One space-separated token, because Herdr joins sibling tokens with
    /// ` · `. The period label leads, so the value is self-describing however
    /// the sidebar arranges it.
    fn rendered(&self) -> String {
        let head = match &self.meter {
            Some(meter) => format!("{} {meter} {}", self.label, self.percent),
            None => format!("{} {}", self.label, self.percent),
        };
        if self.eta.is_empty() {
            return head;
        }
        format!("{head} {}", self.eta)
    }
}

fn compact_window_parts(
    window: &UsageWindow,
    now_unix: u64,
    style: PercentStyle,
    shape: SidebarShape,
) -> WindowParts {
    let label = window.display_label();
    let percent = style.percent_of(window);
    let eta = window
        .resets_at
        .map(|reset| format_reset_eta(reset, now_unix))
        .unwrap_or_default();
    match gauge_cells(shape, label) {
        // The number is right-aligned so the ETA column lines up across rows.
        Some(cells) => WindowParts {
            label: format!("{label:<width$}", width = GAUGE_LABEL_WIDTH),
            percent: format!("{:>3}%", format_percent(percent)),
            eta,
            meter: Some(meter(printed_percent(percent), cells)),
        },
        None => {
            let percent_text = format!("{}%", format_percent(percent));
            let eta = if shape.layout == SidebarLayout::Gauges && shape.content_width > 0 {
                // Keep the number. Shorten an ETA to its leading unit before
                // dropping it; never shorten a provider's label.
                fit_eta(
                    eta,
                    shape
                        .content_width
                        .saturating_sub(label.chars().count() + 1 + percent_text.len() + 1),
                )
            } else {
                eta
            };
            WindowParts {
                label: label.to_string(),
                percent: percent_text,
                eta,
                meter: None,
            }
        }
    }
}

/// Replace recurring quota text with a signed pace only when the user opted
/// in and the provider supplied enough clock information. Severity, sorting,
/// and alerts continue to use remaining quota; this changes display text only.
fn sidebar_window_parts(
    window: &UsageWindow,
    now_unix: u64,
    style: PercentStyle,
    shape: SidebarShape,
    pacing: SidebarPacing,
) -> WindowParts {
    if pacing.is_on() && matches!(window.kind, WindowKind::FiveHour | WindowKind::Weekly) {
        if let Some(pace) = window_pace(window, now_unix) {
            return WindowParts {
                label: window.display_label().to_string(),
                percent: format_signed_pace(pace.delta),
                eta: format_pace_remaining(pace.remaining_seconds),
                meter: None,
            };
        }
    }
    compact_window_parts(window, now_unix, style, shape)
}

/// `29d23h` → `29d`, `23h59m` → `23h`. A value with no unit suffix is kept
/// or cleared; `due` is never rewritten into `d`.
fn fit_eta(eta: String, available: usize) -> String {
    if eta.chars().count() <= available {
        return eta;
    }
    let shortened = leading_eta_unit(&eta).unwrap_or("");
    if shortened.chars().count() <= available && !shortened.is_empty() {
        shortened.to_string()
    } else {
        String::new()
    }
}

fn leading_eta_unit(eta: &str) -> Option<&str> {
    for unit in ['d', 'h'] {
        if let Some(index) = eta.find(unit) {
            if index > 0 && eta[..index].bytes().all(|byte| byte.is_ascii_digit()) {
                return Some(&eta[..=index]);
            }
        }
    }
    None
}

fn format_reset_eta(reset_at: ResetAt, now_unix: u64) -> String {
    let seconds = reset_at.unix_seconds().saturating_sub(now_unix);
    if seconds == 0 {
        return "due".to_string();
    }
    format_duration(seconds)
}

fn format_duration(seconds: u64) -> String {
    let minutes = (seconds / 60).max(1);
    if minutes >= 24 * 60 {
        return format!("{}d{}h", minutes / (24 * 60), (minutes % (24 * 60)) / 60);
    }
    if minutes >= 60 {
        return format!("{}h{:02}m", minutes / 60, minutes % 60);
    }
    format!("{minutes}m")
}

fn format_ttl(seconds: u64) -> String {
    if seconds == 0 {
        return "0m".to_string();
    }
    let minutes = seconds / 60;
    if (60..24 * 60).contains(&minutes) && minutes.is_multiple_of(60) {
        return format!("{}h", minutes / 60);
    }
    format_duration(seconds)
}

fn format_pace_remaining(seconds: u64) -> String {
    let minutes = (seconds / 60).max(1);
    if minutes >= 24 * 60 {
        let days = minutes / (24 * 60);
        let hours = (minutes % (24 * 60)) / 60;
        return if hours == 0 {
            format!("{days}d")
        } else {
            format!("{days}d{hours}h")
        };
    }
    if minutes >= 60 {
        let hours = minutes / 60;
        let minutes = minutes % 60;
        return if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {minutes} min")
        };
    }
    format!("{minutes} min")
}

struct WindowPace {
    /// Remaining quota minus remaining time, in percentage points.
    delta: f64,
    remaining_seconds: u64,
}

fn window_pace(window: &UsageWindow, now_unix: u64) -> Option<WindowPace> {
    const MIN_ELAPSED_FRACTION: f64 = 0.05;
    let remaining_seconds = window
        .resets_at?
        .unix_seconds()
        .checked_sub(now_unix)
        .filter(|remaining| *remaining > 0)?;
    let duration = window
        .duration_seconds
        .unwrap_or_else(|| window.kind.duration_seconds());
    let elapsed_seconds = duration.checked_sub(remaining_seconds)?;
    if (elapsed_seconds as f64) < MIN_ELAPSED_FRACTION * duration as f64 {
        return None;
    }
    let elapsed_percent = elapsed_seconds as f64 * 100.0 / duration as f64;
    Some(WindowPace {
        delta: elapsed_percent - window.used_percent,
        remaining_seconds,
    })
}

fn format_signed_pace(delta: f64) -> String {
    let rounded = delta.round() as i64;
    if rounded > 0 {
        format!("+{rounded}%")
    } else {
        format!("{rounded}%")
    }
}

/// Spending pace for the Claude Code status line: quota consumed versus how
/// much of the window's clock has run, in percentage points.
///
/// Paces against the binding window — the 5h/7d window with the least
/// remaining quota, the same rule as [`headroom`], because whichever limit
/// runs out first is the one the pace has to respect — and names it
/// (`5h`/`7d`) so a weekly pace is never mistaken for a five-hour one. The
/// binding window is chosen *before* asking whether it can be paced: if it
/// cannot, nothing is rendered rather than pacing the looser window, which
/// would put a confident arrow on the wrong budget. `↓` means slow down
/// (consumption is ahead of the clock), `↑` means there is headroom, `=` is
/// within five points either way, decided on the unrounded difference with an
/// inclusive boundary. Nothing is rendered without a reset time, after the
/// window expires, when its reset lies further away than the window is long,
/// or in the first 5% of it — the ratio is noise right after a reset, and a
/// confident wrong arrow is worse than none.
pub fn pace_segment(windows: &[UsageWindow], now_unix: u64) -> Option<String> {
    const TOLERANCE_POINTS: f64 = 5.0;
    let window = windows
        .iter()
        .filter(|window| matches!(window.kind, WindowKind::FiveHour | WindowKind::Weekly))
        // `min_by` keeps the first of equals, and 5h is parsed first.
        .min_by(|a, b| a.remaining_percent.total_cmp(&b.remaining_percent))?;
    let delta = -window_pace(window, now_unix)?.delta;
    let pace = if delta.abs() <= TOLERANCE_POINTS {
        "=".to_string()
    } else if delta > 0.0 {
        format!("↓{}%", delta.round())
    } else {
        format!("↑{}%", (-delta).round())
    };
    Some(format!("⏱ {} {pace}", window.kind.label()))
}

#[cfg(test)]
mod tests {

    /// The sort key and the alert both read this, and both have to be
    /// explainable by a token the user can see.
    #[test]
    fn headroom_is_the_tightest_window_the_sidebar_actually_shows() {
        let snapshot = ProviderSnapshot::new(
            Provider::OpenCodeGo,
            vec![
                window(WindowKind::FiveHour, 40.0, 3_600),
                window(WindowKind::Weekly, 75.0, 183_600),
                window(WindowKind::Monthly, 98.0, 1_500_000),
            ],
            0,
        );
        // 5h has 60 left, 7d has 25, 30d has 2.
        let tokens = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(tokens.quota_headroom, Some(2));
        let visible = MetadataTokens::from_snapshot_for_pane_with_fields(
            &snapshot,
            0,
            None,
            PercentStyle::default(),
            SidebarShape::default(),
            FieldSet::parse("5h,7d").unwrap(),
        );
        assert_eq!(visible.quota_headroom, Some(25));
        let hidden = MetadataTokens::from_snapshot_for_pane_with_fields(
            &snapshot,
            0,
            None,
            PercentStyle::default(),
            SidebarShape::default(),
            FieldSet::parse("none").unwrap(),
        );
        assert_eq!(hidden.quota_headroom, None);
    }

    #[test]
    fn stale_claude_session_quota_is_visible_but_cannot_claim_headroom() {
        let mut snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 100).session_local();
        snapshot.session_windows.insert(
            "session-a".to_string(),
            vec![window(WindowKind::FiveHour, 92.0, 14_820)],
        );
        snapshot.session_quota_observations.insert(
            "session-a".to_string(),
            vec![SessionQuotaObservation {
                kind: WindowKind::FiveHour,
                observed_at_unix: Some(100),
                api_generation: Some("generation-a".to_string()),
            }],
        );

        let stale = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            400,
            Some("session-a"),
            PercentStyle::Remaining,
            SidebarShape::default(),
        );
        assert_eq!(stale.quota_5h, "5h stale 5m");
        assert_eq!(stale.quota_5h_severity, Some(Severity::Unknown));
        assert_eq!(stale.quota_headroom, None);

        let paced_stale = MetadataTokens::from_snapshot_for_pane_with_row(
            &snapshot,
            400,
            Some("session-a"),
            RowStyle {
                percent: PercentStyle::Remaining,
                shape: SidebarShape::default(),
                fields: FieldSet::all(),
                pacing: SidebarPacing::On,
            },
        );
        assert_eq!(paced_stale.quota_5h, "5h stale 5m");
        assert_eq!(paced_stale.quota_5h_severity, Some(Severity::Unknown));
        assert_eq!(paced_stale.quota_headroom, None);

        let fresh = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            200,
            Some("session-a"),
            PercentStyle::Remaining,
            SidebarShape::default(),
        );
        assert_eq!(fresh.quota_5h, "5h 8% 4h03m");
        assert_eq!(fresh.quota_5h_severity, Some(Severity::Danger));
        assert_eq!(fresh.quota_headroom, Some(8));
    }

    #[test]
    fn a_hidden_stale_claude_window_does_not_suppress_visible_fresh_headroom() {
        let mut snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 100).session_local();
        snapshot.session_windows.insert(
            "session-a".to_string(),
            vec![
                window(WindowKind::FiveHour, 20.0, 14_820),
                window(WindowKind::Weekly, 95.0, 90_000),
            ],
        );
        snapshot.session_quota_observations.insert(
            "session-a".to_string(),
            vec![
                SessionQuotaObservation {
                    kind: WindowKind::FiveHour,
                    observed_at_unix: Some(250),
                    api_generation: Some("generation-fresh".to_string()),
                },
                SessionQuotaObservation {
                    kind: WindowKind::Weekly,
                    observed_at_unix: Some(100),
                    api_generation: Some("generation-old".to_string()),
                },
            ],
        );

        let values = MetadataTokens::from_snapshot_for_pane_with_fields(
            &snapshot,
            300,
            Some("session-a"),
            PercentStyle::Remaining,
            SidebarShape::default(),
            FieldSet::parse("5h").unwrap(),
        );
        assert_eq!(values.quota_5h, "5h 80% 4h02m");
        assert_eq!(values.quota_headroom, Some(80));
    }

    #[test]
    fn legacy_claude_session_quota_has_unknown_age_instead_of_looking_live() {
        let mut snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 100).session_local();
        snapshot.session_windows.insert(
            "session-a".to_string(),
            vec![window(WindowKind::FiveHour, 5.0, 14_820)],
        );

        let values = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            400,
            Some("session-a"),
            PercentStyle::Remaining,
            SidebarShape::default(),
        );
        assert_eq!(values.quota_5h, "5h stale");
        assert_eq!(values.quota_5h_severity, Some(Severity::Unknown));
        assert_eq!(values.quota_headroom, None);
    }

    #[test]
    fn headroom_rounds_down_so_a_window_never_crosses_a_threshold_early() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::FiveHour, 89.5, 3_600)],
            0,
        );
        assert_eq!(
            MetadataTokens::from_snapshot(&snapshot, 0).quota_headroom,
            Some(10)
        );
    }

    #[test]
    fn a_provider_with_no_window_reports_no_headroom() {
        let snapshot = ProviderSnapshot::new(Provider::Grok, vec![], 0);
        assert_eq!(
            MetadataTokens::from_snapshot(&snapshot, 0).quota_headroom,
            None
        );
        assert_eq!(
            MetadataTokens::unavailable(Provider::Grok, "switched account").quota_headroom,
            None
        );
    }

    #[test]
    fn agy_third_party_pool_renders_as_api() {
        let snapshot = ProviderSnapshot::new(
            Provider::Agy,
            vec![
                window(WindowKind::FiveHour, 15.0, 14_820),
                window(WindowKind::Weekly, 37.0, 518_400),
                window(WindowKind::Monthly, 0.0, 518_400).with_source_window("api", None),
            ],
            0,
        );
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert!(values.quota_5h.starts_with("5h "), "{values:?}");
        assert!(values.quota_week.starts_with("7d "), "{values:?}");
        assert!(values.quota_month.starts_with("api "), "{values:?}");
        assert!(!values.quota_month.contains("30d"), "{values:?}");
    }

    #[test]
    fn a_monthly_window_has_its_own_sidebar_token() {
        let snapshot = ProviderSnapshot::new(
            Provider::OpenCodeGo,
            vec![
                window(WindowKind::FiveHour, 10.0, 3_600),
                window(WindowKind::Weekly, 20.0, 183_600),
                window(WindowKind::Monthly, 30.0, 1_500_000),
            ],
            0,
        );
        let dashboard = dashboard_summary(&snapshot, 0, PercentStyle::default());
        assert!(dashboard.contains("30d"), "{dashboard}");

        let sidebar = MetadataTokens::from_snapshot(&snapshot, 0);
        assert!(sidebar.quota_5h.contains("5h"));
        assert!(sidebar.quota_week.contains("7d"));
        assert!(sidebar.quota_month.contains("30d"), "{sidebar:?}");
        assert!(!sidebar.quota_week.contains("30d"), "{sidebar:?}");
    }
    use super::*;
    use crate::model::{ProviderSnapshot, SessionQuotaObservation, UsageWindow};

    fn window(kind: WindowKind, used: f64, reset: u64) -> UsageWindow {
        UsageWindow::new(kind, used, Some(ResetAt::from_unix_seconds(reset))).unwrap()
    }

    /// A monthly-only plan (Grok billed monthly, a Go plan with no weekly
    /// bucket) still gets a row, and it says `30d` — the label lives inside
    /// the value, so the long-window slot can carry either period truthfully.
    #[test]
    fn a_monthly_only_plan_fills_the_long_window_slot_with_its_own_label() {
        let snapshot = ProviderSnapshot::new(
            Provider::Grok,
            vec![window(WindowKind::Monthly, 30.0, 1_500_000)],
            0,
        );
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_month, "30d 70% 17d8h");
        assert_eq!(values.quota_month_severity, Some(Severity::Normal));
        assert_eq!(values.quota_week, "");
        assert_eq!(values.quota_5h, "");
    }

    #[test]
    fn unavailable_omits_window_rows_instead_of_printing_na() {
        let cursor = MetadataTokens::unavailable(Provider::Cursor, "signed-in account changed");
        assert_eq!(cursor.quota_month, "");
        assert_eq!(cursor.quota_month_severity, None);
        assert_eq!(cursor.quota_week, "");
        assert_eq!(cursor.quota_5h, "");
        assert_eq!(
            cursor.quota_error.as_deref(),
            Some("signed-in account changed")
        );

        let claude = MetadataTokens::unavailable(Provider::Claude, "signed-in account changed");
        assert_eq!(claude.quota_5h, "");
        assert_eq!(claude.quota_week, "");
        assert_eq!(claude.quota_month, "");
        assert_eq!(claude.quota_5h_severity, None);
    }

    #[test]
    fn unavailable_for_windows_does_not_invent_na_rows() {
        let values = MetadataTokens::unavailable_for_windows(
            Provider::Grok,
            "signed-in account changed",
            &[window(WindowKind::Monthly, 30.0, 1_500_000)],
        );
        assert_eq!(values.quota_month, "");
        assert_eq!(values.quota_month_severity, None);
        assert_eq!(values.quota_week, "");
        assert_eq!(values.quota_5h, "");
        assert_eq!(
            values.quota_error.as_deref(),
            Some("signed-in account changed")
        );
    }

    /// Fallback only. A weekly window always wins the slot, because it is the
    /// limit that binds first; a 30d number must never displace it.
    #[test]
    fn a_weekly_window_always_wins_the_long_window_slot() {
        let snapshot = ProviderSnapshot::new(
            Provider::OpenCodeGo,
            vec![
                window(WindowKind::Weekly, 20.0, 183_600),
                window(WindowKind::Monthly, 90.0, 1_500_000),
            ],
            0,
        );
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_week, "7d 80% 2d3h");
        assert!(!values.quota_week.contains("30d"));
        assert!(values.quota_month.contains("30d"));
    }

    #[test]
    fn formats_reset_eta_for_minutes_hours_days_and_due_windows() {
        assert_eq!(
            format_reset_eta(ResetAt::from_unix_seconds(2_700), 0),
            "45m"
        );
        assert_eq!(
            format_reset_eta(ResetAt::from_unix_seconds(14_820), 0),
            "4h07m"
        );
        assert_eq!(
            format_reset_eta(ResetAt::from_unix_seconds(183_600), 0),
            "2d3h"
        );
        assert_eq!(format_reset_eta(ResetAt::from_unix_seconds(99), 100), "due");
        assert_eq!(format_ttl(0), "0m");
        assert_eq!(format_ttl(3_600), "1h");
    }

    #[test]
    fn summary_is_window_driven_and_keeps_five_hour_before_weekly() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                window(WindowKind::Weekly, 27.0, 183_600),
                window(WindowKind::FiveHour, 58.0, 14_820),
            ],
            0,
        );
        assert_eq!(
            dashboard_summary(&snapshot, 0, PercentStyle::default()),
            "5h 42% left reset 4h07m · 7d 73% left reset 2d3h"
        );
    }

    #[test]
    fn sidebar_windows_use_consistent_single_spacing() {
        let five_hour = format_window(
            &window(WindowKind::FiveHour, 57.0, 14_820),
            0,
            false,
            PercentStyle::default(),
        );
        let weekly = format_window(
            &window(WindowKind::Weekly, 75.0, 183_600),
            0,
            false,
            PercentStyle::default(),
        );
        assert_eq!(five_hour, "5h 43% reset 4h07m");
        assert_eq!(weekly, "7d 25% reset 2d3h");
    }

    /// The sidebar token keeps its width in both styles: no `left`/`used`
    /// word rides along, because the sidebar truncates and the style is a
    /// choice the user made for their own sidebar.
    #[test]
    fn the_used_style_flips_the_sidebar_number_but_not_its_width_or_colour() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                window(WindowKind::FiveHour, 58.0, 14_820),
                window(WindowKind::Weekly, 27.0, 183_600),
            ],
            0,
        );
        let remaining = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            SidebarShape::default(),
        );
        assert_eq!(remaining.quota_5h, "5h 42% 4h07m");
        assert_eq!(remaining.quota_week, "7d 73% 2d3h");

        let used = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Used,
            SidebarShape::default(),
        );
        assert_eq!(used.quota_5h, "5h 58% 4h07m");
        assert_eq!(used.quota_week, "7d 27% 2d3h");
        assert_eq!(used.quota_5h_severity, remaining.quota_5h_severity);
        assert_eq!(used.quota_week_severity, remaining.quota_week_severity);
    }

    /// U4 gives `gauges` a meter; `packed` and `stacked` must stay identical
    /// to each other and to what they printed before the layout existed.
    #[test]
    fn the_sidebar_layout_does_not_move_any_packed_or_stacked_token() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                window(WindowKind::FiveHour, 19.0, 2_580),
                window(WindowKind::Weekly, 27.0, 183_600),
            ],
            0,
        )
        .with_context(Some(crate::model::ContextUsage::new(23.5).unwrap()));
        let packed = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::default(),
            SidebarLayout::Packed.into(),
        );
        let stacked = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::default(),
            SidebarLayout::Stacked.into(),
        );
        assert_eq!(packed, stacked);
        assert_eq!(packed.quota_5h, "5h 81% 43m");
    }

    #[test]
    fn the_existing_literals_survive_every_layout() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                window(WindowKind::FiveHour, 58.0, 14_820),
                window(WindowKind::Weekly, 27.0, 183_600),
            ],
            0,
        )
        .with_context(Some(crate::model::ContextUsage::new(23.5).unwrap()));
        for layout in [SidebarLayout::Packed, SidebarLayout::Stacked] {
            let values = MetadataTokens::from_snapshot_for_session(
                &snapshot,
                0,
                None,
                PercentStyle::default(),
                layout.into(),
            );
            assert_eq!(values.quota_5h, "5h 42% 4h07m");
            assert_eq!(values.quota_week, "7d 73% 2d3h");
            assert_eq!(values.quota_context, "context 24%");
        }
    }

    /// The dashboard renders through `format_window`, not the sidebar path,
    /// so no layout may ever reach it.
    #[test]
    fn the_dashboard_never_carries_a_meter() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                window(WindowKind::FiveHour, 58.0, 14_820),
                window(WindowKind::Weekly, 27.0, 183_600),
            ],
            0,
        );
        let summary = dashboard_summary(&snapshot, 0, PercentStyle::default());
        assert_eq!(
            summary,
            "5h 42% left reset 4h07m \u{b7} 7d 73% left reset 2d3h"
        );
        assert!(!summary.contains('\u{25b0}'));
        assert!(!summary.contains('\u{25b1}'));
    }

    #[test]
    fn metadata_error_stays_within_herdr_token_limit() {
        let values = MetadataTokens::unavailable(Provider::Grok, "x".repeat(120));
        assert_eq!(values.quota_error.as_deref().unwrap().len(), 80);
    }

    #[test]
    fn metadata_keeps_severity_per_quota_window() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                window(WindowKind::FiveHour, 70.0, 14_820),
                window(WindowKind::Weekly, 90.0, 183_600),
            ],
            0,
        );
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_5h_severity, Some(Severity::Warning));
        assert_eq!(values.quota_week_severity, Some(Severity::Danger));
    }

    #[test]
    fn metadata_uses_compact_quota_window_labels() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                window(WindowKind::FiveHour, 58.0, 14_820),
                window(WindowKind::Weekly, 27.0, 183_600),
            ],
            0,
        );
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_5h, "5h 42% 4h07m");
        assert_eq!(values.quota_week, "7d 73% 2d3h");
        assert!(!values.quota_5h.contains('·'));
        assert!(!values.quota_week.contains('·'));
    }

    #[test]
    fn metadata_formats_context_usage_when_available() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::Weekly, 10.0, 183_600)],
            0,
        )
        .with_context(Some(crate::model::ContextUsage::new(23.5).unwrap()));
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_context, "context 24%");
    }

    #[test]
    fn metadata_uses_the_model_for_the_pane_session() {
        let mut snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::Weekly, 10.0, 183_600)],
            0,
        )
        .with_model(Some("latest".to_string()));
        snapshot
            .session_models
            .insert("session-1".to_string(), "Sonnet".to_string());

        let session_one = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            Some("session-1"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(session_one.quota_model, "Sonnet");
        assert_eq!(session_one.quota_provider_model, "Claude/Sonnet");

        let session_two = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            Some("session-2"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(session_two.quota_model, "");
        assert_eq!(session_two.quota_provider_model, "Claude");
    }

    #[test]
    fn devin_pane_uses_configured_default_when_session_model_is_unknown() {
        let snapshot = ProviderSnapshot::new(
            Provider::Devin,
            vec![window(WindowKind::Weekly, 10.0, 183_600)],
            0,
        )
        .with_model(Some("SWE-1.7 Medium".to_string()));
        let pane = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            Some("session-a"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(pane.quota_model, "SWE-1.7 Medium");
        assert_eq!(pane.quota_provider_model, "Devin/SWE-1.7 Medium");
    }

    #[test]
    fn devin_panes_keep_distinct_session_models() {
        let mut snapshot = ProviderSnapshot::new(
            Provider::Devin,
            vec![window(WindowKind::Weekly, 10.0, 183_600)],
            0,
        )
        .with_model(Some("SWE-1.7 Medium".to_string()));
        snapshot
            .session_models
            .insert("session-a".to_string(), "Opus 4.6".to_string());

        let switched = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            Some("session-a"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(switched.quota_model, "Opus 4.6");
        assert_eq!(switched.quota_provider_model, "Devin/Opus 4.6");

        let untouched = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            Some("session-b"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(untouched.quota_model, "SWE-1.7 Medium");
        assert_eq!(untouched.quota_provider_model, "Devin/SWE-1.7 Medium");
    }

    #[test]
    fn metadata_uses_context_and_cache_for_the_pane_session() {
        let mut snapshot = ProviderSnapshot::new(Provider::Codex, vec![], 0);
        snapshot.session_contexts.insert(
            "session-1".to_string(),
            crate::model::ContextUsage::new(43.2)
                .unwrap()
                .with_cache(crate::model::CacheUsage::from_token_counts(200, 800, 100)),
        );
        let values = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            Some("session-1"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(values.quota_context, "context 43%");
        assert_eq!(values.quota_cache, "cache 72.7%");
        assert_eq!(
            MetadataTokens::from_snapshot_for_session(
                &snapshot,
                0,
                Some("session-2"),
                PercentStyle::default(),
                SidebarShape::default()
            )
            .quota_context,
            ""
        );
    }

    #[test]
    fn pane_without_session_id_does_not_broadcast_local_diagnostics() {
        let snapshot = ProviderSnapshot::new(Provider::Grok, vec![], 0)
            .with_model(Some("grok-4.6".to_string()))
            .with_context(Some(
                crate::model::ContextUsage::new(43.2)
                    .unwrap()
                    .with_cache(crate::model::CacheUsage::from_token_counts(200, 800, 100)),
            ));
        let values = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            None,
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(values.quota_provider_model, "Grok/grok-4.6");
        assert_eq!(values.quota_context, "");
        assert_eq!(values.quota_cache, "");
        assert_eq!(values.quota_cache_ttl, "");
        assert_eq!(values.quota_error, None);
    }

    #[test]
    fn a_narrow_sidebar_keeps_only_the_model_beside_the_logo() {
        let snapshot =
            ProviderSnapshot::new(Provider::Grok, vec![], 0).with_model(Some("grok-4.6".into()));
        let wide = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            None,
            PercentStyle::default(),
            SidebarShape::new(SidebarLayout::Packed, 30),
        );
        assert_eq!(wide.quota_provider_model, "Grok/grok-4.6");
        assert_eq!(wide.quota_provider, "Grok");
        let narrow = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            None,
            PercentStyle::default(),
            SidebarShape::new(SidebarLayout::Packed, 22),
        );
        // content_width = 22 - 4 = 18 < 22 → model only
        assert_eq!(narrow.quota_provider_model, "grok-4.6");
        assert!(narrow.quota_provider.is_empty());
        assert_eq!(narrow.quota_model, "grok-4.6");
    }

    #[test]
    fn metadata_formats_session_cache_hit_rate_and_approximate_ttl() {
        let cache = crate::model::CacheUsage::from_token_counts(100, 800, 100)
            .unwrap()
            .with_ttl_estimate(60 * 60, 0)
            .with_session_totals(
                crate::model::CacheTotals::from_token_counts(100, 800, 100),
                "session-1",
                1,
            );
        let context = crate::model::ContextUsage::new(23.5)
            .unwrap()
            .with_cache(Some(cache));
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::Weekly, 10.0, 183_600)],
            0,
        )
        .with_context(Some(context));
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_context, "context 24%");
        assert_eq!(values.quota_cache, "cache 80.0%");
        assert_eq!(values.quota_cache_ttl, "ttl≈1h");
        assert_eq!(values.quota_error, None);
    }

    #[test]
    fn metadata_formats_cache_ttl_for_a_matching_pane_session() {
        let cache = crate::model::CacheUsage::from_token_counts(100, 800, 100)
            .unwrap()
            .with_ttl_estimate(60 * 60, 1_000)
            .with_session_totals(
                crate::model::CacheTotals::from_token_counts(100, 800, 100),
                "codex-session",
                0,
            );
        let mut snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 1);
        snapshot.session_contexts.insert(
            "codex-session".to_string(),
            crate::model::ContextUsage::new(23.5)
                .unwrap()
                .with_cache(Some(cache)),
        );

        let values = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            1_000,
            Some("codex-session"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(values.quota_cache, "cache 80.0%");
        assert_eq!(values.quota_cache_ttl, "ttl≈1h");
    }

    #[test]
    fn expired_cache_ttl_is_reported_as_no_cached() {
        let cache = crate::model::CacheUsage::from_token_counts(100, 800, 100)
            .unwrap()
            .with_ttl_estimate(60, 0)
            .with_session_totals(
                crate::model::CacheTotals::from_token_counts(100, 800, 100),
                "session-1",
                1,
            );
        let snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 0).with_context(Some(
            crate::model::ContextUsage::new(23.5)
                .unwrap()
                .with_cache(Some(cache)),
        ));
        let values = MetadataTokens::from_snapshot(&snapshot, 61);
        assert_eq!(values.quota_cache_ttl, "");
        assert_eq!(values.quota_cache_state, "no cached");
        // A cold prefix is a normal state, not a plugin failure: it must not
        // land in the token that reports "quota could not be read at all".
        assert_eq!(values.quota_error, None);
    }

    #[test]
    fn weekly_only_sidebar_window_uses_the_compact_reset_eta() {
        let snapshot = ProviderSnapshot::new(
            Provider::Codex,
            vec![window(WindowKind::Weekly, 31.0, 518_400)],
            0,
        );
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_week, "7d 69% 6d0h");
        assert_eq!(values.quota_5h, "");
        assert_eq!(values.quota_5h_severity, None);
    }

    #[test]
    fn a_missing_five_hour_window_omits_the_row() {
        for provider in [
            Provider::Claude,
            Provider::Agy,
            Provider::Codex,
            Provider::Grok,
        ] {
            let snapshot =
                ProviderSnapshot::new(provider, vec![window(WindowKind::Weekly, 31.0, 518_400)], 0);
            let values = MetadataTokens::from_snapshot(&snapshot, 0);
            assert_eq!(values.quota_5h, "", "{provider:?}");
            assert_eq!(values.quota_5h_severity, None, "{provider:?}");
            assert_eq!(values.quota_week, "7d 69% 6d0h", "{provider:?}");
        }
    }

    #[test]
    fn grok_does_not_invent_a_five_hour_row() {
        let snapshot = ProviderSnapshot::new(
            Provider::Grok,
            vec![window(WindowKind::Weekly, 31.0, 518_400)],
            0,
        );
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_5h, "");
        assert_eq!(values.quota_5h_severity, None);
    }

    #[test]
    fn grok_and_codex_panes_keep_account_quota_when_the_session_is_known() {
        let grok = ProviderSnapshot::new(
            Provider::Grok,
            vec![window(WindowKind::Weekly, 31.0, 518_400)],
            0,
        );
        let grok_pane = MetadataTokens::from_snapshot_for_pane(
            &grok,
            0,
            Some("session-1"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(grok_pane.quota_week, "7d 69% 6d0h");
        assert_eq!(grok_pane.quota_5h, "");

        let codex = ProviderSnapshot::new(
            Provider::Codex,
            vec![
                window(WindowKind::FiveHour, 40.0, 14_820),
                window(WindowKind::Weekly, 31.0, 518_400),
            ],
            0,
        );
        let codex_pane = MetadataTokens::from_snapshot_for_pane(
            &codex,
            0,
            Some("session-1"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(codex_pane.quota_5h, "5h 60% 4h07m");
        assert_eq!(codex_pane.quota_week, "7d 69% 6d0h");
    }

    #[test]
    fn claude_pane_quota_follows_the_pane_session() {
        let mut snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::Weekly, 90.0, 518_400)],
            0,
        );
        snapshot.session_windows.insert(
            "work".to_string(),
            vec![
                window(WindowKind::FiveHour, 18.0, 14_820),
                window(WindowKind::Weekly, 10.0, 518_400),
            ],
        );
        snapshot.session_windows.insert(
            "personal".to_string(),
            vec![
                window(WindowKind::FiveHour, 82.0, 14_820),
                window(WindowKind::Weekly, 90.0, 518_400),
            ],
        );

        let work = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            Some("work"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(work.quota_5h, "5h 82% 4h07m");
        assert_eq!(work.quota_week, "7d 90% 6d0h");

        let personal = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            Some("personal"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(personal.quota_5h, "5h 18% 4h07m");
        assert_eq!(personal.quota_week, "7d 10% 6d0h");

        let unknown = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            Some("other"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(unknown.quota_5h, "");
        assert_eq!(unknown.quota_week, "");
    }

    #[test]
    fn agy_sidebar_keeps_account_quota_when_herdr_session_is_a_subagent() {
        let mut snapshot = ProviderSnapshot::new(
            Provider::Agy,
            vec![
                window(WindowKind::FiveHour, 1.0, 14_820),
                window(WindowKind::Weekly, 2.0, 518_400),
            ],
            0,
        )
        .session_local()
        .with_model(Some("Gemini 3.8 Flash (High)".to_string()));
        snapshot.session_windows.insert(
            "ed02b39b-7ea3-46c9-9855-1672f4ef7e91".to_string(),
            snapshot.windows.clone(),
        );
        snapshot.session_models.insert(
            "ed02b39b-7ea3-46c9-9855-1672f4ef7e91".to_string(),
            "Gemini 3.8 Flash (High)".to_string(),
        );
        snapshot.context = Some(crate::model::ContextUsage::new(3.427886962890625).unwrap());
        snapshot.session_contexts.insert(
            "ed02b39b-7ea3-46c9-9855-1672f4ef7e91".to_string(),
            crate::model::ContextUsage::new(3.427886962890625).unwrap(),
        );

        let values = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            Some("6a4d6f77-88be-4704-adcc-a51401ad7c03"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(values.quota_5h, "5h 99% 4h07m");
        assert_eq!(values.quota_week, "7d 98% 6d0h");
        assert_eq!(values.quota_provider_model, "Agy/Gemini 3.8 Flash (High)");
        assert_eq!(values.quota_context, "context 3%");
        assert_eq!(values.quota_cache, "");
    }

    #[test]
    fn agy_sidebar_does_not_show_another_panes_model_or_context() {
        let mut snapshot = ProviderSnapshot::new(
            Provider::Agy,
            vec![window(WindowKind::FiveHour, 80.0, 14_820)],
            0,
        )
        .session_local()
        .with_model(Some("Claude Sonnet".to_string()));
        snapshot.session_windows.insert(
            "w1:p1".to_string(),
            vec![window(WindowKind::FiveHour, 10.0, 14_820)],
        );
        snapshot.session_windows.insert(
            "w1:p2".to_string(),
            vec![window(WindowKind::FiveHour, 80.0, 14_820)],
        );
        snapshot
            .session_models
            .insert("w1:p2".to_string(), "Claude Sonnet".to_string());
        snapshot.context = Some(crate::model::ContextUsage::new(70.0).unwrap());
        snapshot.session_contexts.insert(
            "w1:p2".to_string(),
            crate::model::ContextUsage::new(70.0).unwrap(),
        );

        let pane_a = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            Some("w1:p1"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(pane_a.quota_5h, "5h 90% 4h07m");
        assert_eq!(pane_a.quota_provider_model, "Agy");
        assert_eq!(pane_a.quota_context, "");
        assert_eq!(pane_a.quota_cache, "");

        let pane_b = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            Some("w1:p2"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(pane_b.quota_provider_model, "Agy/Claude Sonnet");
        assert_eq!(pane_b.quota_context, "context 70%");
    }

    #[test]
    fn claude_panes_on_the_same_profile_share_the_newest_quota() {
        let mut snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::FiveHour, 92.0, 14_820)],
            0,
        );
        snapshot
            .session_quota_scopes
            .insert("session-c".to_string(), "scope-w".to_string());
        snapshot
            .session_quota_scopes
            .insert("session-a".to_string(), "scope-w".to_string());
        snapshot.session_windows.insert(
            "session-c".to_string(),
            vec![window(WindowKind::FiveHour, 5.0, 14_820)],
        );
        snapshot.session_windows.insert(
            "session-a".to_string(),
            vec![window(WindowKind::FiveHour, 92.0, 14_820)],
        );
        snapshot.quota_scope_windows.insert(
            "scope-w".to_string(),
            vec![window(WindowKind::FiveHour, 92.0, 14_820)],
        );

        let idle = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            Some("session-c"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        let live = MetadataTokens::from_snapshot_for_pane(
            &snapshot,
            0,
            Some("session-a"),
            PercentStyle::default(),
            SidebarShape::default(),
        );
        assert_eq!(idle.quota_5h, "5h 8% 4h07m");
        assert_eq!(live.quota_5h, "5h 8% 4h07m");
        assert_eq!(idle.quota_headroom, Some(8));
        assert_eq!(live.quota_headroom, Some(8));
    }

    #[test]
    fn an_expired_window_is_not_shown_as_live_quota() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::FiveHour, 20.0, 1_000)],
            0,
        );
        let tokens = MetadataTokens::from_snapshot(&snapshot, 1_001);
        assert_eq!(tokens.quota_5h, "");
        assert_eq!(tokens.quota_5h_severity, None);
        assert_eq!(tokens.quota_headroom, None);
        assert!(!tokens.quota_5h.contains("80%"), "{tokens:?}");
        assert_eq!(
            dashboard_summary(&snapshot, 1_001, PercentStyle::default()),
            ""
        );
        assert_eq!(
            ProviderSnapshot::severity_for_windows(Provider::Claude, &snapshot.windows, 1_001),
            Severity::Unknown
        );
    }

    #[test]
    fn an_expired_five_hour_window_does_not_drive_headroom_or_severity() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                window(WindowKind::FiveHour, 95.0, 1_000),
                window(WindowKind::Weekly, 10.0, 10_000),
            ],
            0,
        );
        let tokens = MetadataTokens::from_snapshot(&snapshot, 1_001);
        assert_eq!(tokens.quota_5h, "");
        assert_eq!(tokens.quota_5h_severity, None);
        assert!(tokens.quota_week.starts_with("7d 90%"), "{tokens:?}");
        assert_eq!(tokens.quota_headroom, Some(90));
        assert_eq!(tokens.quota_week_severity, Some(Severity::Normal));
    }

    #[test]
    fn session_cache_percentage_keeps_one_decimal_instead_of_rounding_to_100() {
        let cache = crate::model::CacheUsage::from_token_counts(2_000, 433_336, 1_655)
            .unwrap()
            .with_session_totals(
                crate::model::CacheTotals::from_token_counts(2_000, 433_336, 1_655),
                "session-1",
                1,
            );
        let snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 0).with_context(Some(
            crate::model::ContextUsage::new(43.0)
                .unwrap()
                .with_cache(Some(cache)),
        ));
        let values = MetadataTokens::from_snapshot(&snapshot, 0);
        assert_eq!(values.quota_cache, "cache 99.2%");
    }

    /// The bar shortens with the sidebar and disappears rather
    /// than truncating the number it labels. Counts are after Herdr's
    /// secondary-row indent and scrollbar (`SIDEBAR_CHROME_WIDTH`).
    #[test]
    fn the_meter_cell_count_follows_the_configured_sidebar_width() {
        assert_eq!(meter_cells(18), None);
        assert_eq!(meter_cells(23), None);
        assert_eq!(meter_cells(24), Some(4));
        assert_eq!(meter_cells(26), Some(6));
        assert_eq!(meter_cells(30), Some(10));
        assert_eq!(meter_cells(32), Some(12));
        assert_eq!(meter_cells(36), Some(12));
        assert_eq!(
            SidebarShape::new(SidebarLayout::Gauges, 26).content_width,
            22
        );
    }

    #[test]
    fn a_sidebar_width_outside_the_documented_range_is_sized_as_its_nearest_bound() {
        assert_eq!(meter_cells(0), meter_cells(18));
        assert_eq!(meter_cells(12), meter_cells(18));
        assert_eq!(meter_cells(48), meter_cells(36));
    }

    /// At the narrowest meter the clamp does all the work: one spent cell
    /// cannot read as empty, and one unspent cell cannot read as full.
    #[test]
    fn the_narrowest_meter_still_separates_almost_empty_from_almost_full() {
        assert_eq!(
            meter(1, MIN_METER_CELLS),
            "\u{25b0}\u{25b1}\u{25b1}\u{25b1}"
        );
        assert_eq!(
            meter(99, MIN_METER_CELLS),
            "\u{25b0}\u{25b0}\u{25b0}\u{25b1}"
        );
        assert_eq!(
            meter(0, MIN_METER_CELLS),
            "\u{25b1}\u{25b1}\u{25b1}\u{25b1}"
        );
        assert_eq!(
            meter(100, MIN_METER_CELLS),
            "\u{25b0}\u{25b0}\u{25b0}\u{25b0}"
        );
    }

    /// `METER_ROW_OVERHEAD` budgets six columns for the ETA, so every reset a
    /// provider can plausibly quote has to print within six. Past 99 days it
    /// does not, and the meter would then be one column too long.
    #[test]
    fn a_reset_eta_prints_within_the_six_columns_the_row_overhead_budgets() {
        for seconds in [
            0,
            1,
            59,
            60,
            59 * 60 + 59,
            60 * 60,
            23 * 60 * 60 + 59 * 60,
            24 * 60 * 60,
            29 * 24 * 60 * 60 + 23 * 60 * 60,
            99 * 24 * 60 * 60 + 23 * 60 * 60,
        ] {
            let printed = format_duration(seconds);
            assert!(
                printed.chars().count() <= 6,
                "{seconds}s printed as {printed}"
            );
        }
        assert_eq!(format_duration(1_000 * 24 * 60 * 60), "1000d0h");
    }

    #[test]
    fn a_sidebar_too_narrow_for_four_cells_asks_for_no_meter_at_all() {
        for width in 18..24 {
            assert_eq!(meter_cells(width), None, "width {width}");
        }
        assert_eq!(meter_cells(24), Some(4));
    }

    /// The shape a `gauges` sidebar of `width` columns resolves to. Every
    /// exact-string test below names the cell count it assumes, so a width
    /// change moves one fixture rather than the suite.
    fn gauges(width: usize) -> SidebarShape {
        SidebarShape::new(SidebarLayout::Gauges, width)
    }

    fn paced(snapshot: &ProviderSnapshot, now_unix: u64) -> MetadataTokens {
        MetadataTokens::from_snapshot_for_pane_with_row(
            snapshot,
            now_unix,
            None,
            RowStyle {
                percent: PercentStyle::Remaining,
                shape: gauges(26),
                fields: FieldSet::all(),
                pacing: SidebarPacing::On,
            },
        )
    }

    #[test]
    fn sidebar_pacing_is_opt_in_and_reports_signed_delta_with_time_left() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                window(WindowKind::FiveHour, 91.0, 45 * 60),
                window(WindowKind::Weekly, 20.0, 6 * 24 * 60 * 60),
            ],
            0,
        );
        let quota = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            gauges(26),
        );
        assert!(quota.quota_5h.contains("9%"), "{}", quota.quota_5h);

        let pace = paced(&snapshot, 0);
        assert_eq!(pace.quota_5h, "5h -6% 45 min");
        assert_eq!(pace.quota_week, "7d -6% 6d");
        assert_eq!(pace.quota_5h_severity, quota.quota_5h_severity);
    }

    #[test]
    fn sidebar_pacing_falls_back_to_quota_without_a_usable_clock() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![UsageWindow::new(WindowKind::FiveHour, 40.0, None).unwrap()],
            0,
        );
        let pace = paced(&snapshot, 0);
        assert!(pace.quota_5h.contains("60%"), "{}", pace.quota_5h);
        assert!(!pace.quota_5h.contains('+'));
    }

    #[test]
    fn sidebar_pacing_uses_a_provider_normalized_duration_and_compact_days() {
        let snapshot = ProviderSnapshot::new(
            Provider::Omp,
            vec![
                window(WindowKind::FiveHour, 20.0, 3 * 24 * 60 * 60 + 2 * 60 * 60)
                    .with_source_window("4d", Some(4 * 24 * 60 * 60)),
            ],
            0,
        );
        assert_eq!(paced(&snapshot, 0).quota_5h, "4d +3% 3d2h");
    }

    /// Six cells, the default 26-column sidebar after Herdr chrome. The bar
    /// fills to the number beside it: 81% remaining draws a bar 81% full.
    #[test]
    fn the_gauges_meter_fills_to_the_remaining_number_it_prints() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::FiveHour, 19.0, 2_580)],
            0,
        );
        let values = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            gauges(26),
        );
        assert_eq!(
            values.quota_5h,
            "5h  \u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b1}  81% 43m"
        );
        assert_eq!(values.quota_5h_severity, Some(Severity::Normal));
    }

    /// Six cells. The `used` style flips both the number and the bar, and
    /// neither touches severity, which still reads remaining.
    #[test]
    fn the_used_style_shortens_the_gauges_meter_without_changing_its_colour() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::FiveHour, 19.0, 2_580)],
            0,
        );
        let remaining = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            gauges(26),
        );
        let used = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Used,
            gauges(26),
        );
        assert_eq!(
            used.quota_5h,
            "5h  \u{25b0}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}  19% 43m"
        );
        assert_eq!(used.quota_5h_severity, remaining.quota_5h_severity);
    }

    /// One scale per sidebar: under `gauges` the context row prints through
    /// the same percent style as the window rows, so the column of numbers
    /// reads as one quantity. Twelve cells, a 36-column sidebar.
    #[test]
    fn the_gauges_context_row_prints_through_the_chosen_percent_style() {
        let snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 0)
            .with_context(Some(crate::model::ContextUsage::new(43.0).unwrap()));
        let remaining = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            gauges(36),
        );
        assert_eq!(
            remaining.quota_context,
            "cx  \u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}  57%"
        );
        let used = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Used,
            gauges(36),
        );
        assert_eq!(
            used.quota_context,
            "cx  \u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}  43%"
        );
    }

    /// The style must not leak into the layouts that never joined the column:
    /// `packed` and `stacked` keep printing consumption as `context N%`.
    #[test]
    fn packed_and_stacked_print_context_used_under_either_percent_style() {
        let snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 0)
            .with_context(Some(crate::model::ContextUsage::new(43.0).unwrap()));
        for layout in [SidebarLayout::Packed, SidebarLayout::Stacked] {
            for style in [PercentStyle::Remaining, PercentStyle::Used] {
                let plain = MetadataTokens::from_snapshot_for_session(
                    &snapshot,
                    0,
                    None,
                    style,
                    layout.into(),
                );
                assert_eq!(plain.quota_context, "context 43%", "{layout:?} {style:?}");
            }
        }
    }

    /// Dropping the bar must not flip remaining into used. The number stays
    /// the quantity `quota-percent` selected; only the meter goes away.
    #[test]
    fn a_gauges_sidebar_too_narrow_for_a_meter_still_prints_the_chosen_percent() {
        let snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 0)
            .with_context(Some(crate::model::ContextUsage::new(43.0).unwrap()));
        let remaining = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            gauges(18),
        );
        assert_eq!(remaining.quota_context, "cx 57%");
        let used = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Used,
            gauges(18),
        );
        assert_eq!(used.quota_context, "cx 43%");
    }

    /// The whole point of the fix: cx, 5h and 7d print the same quantity,
    /// and all three move together when the style flips.
    #[test]
    fn every_gauges_row_prints_the_same_quantity_when_the_style_flips() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                window(WindowKind::FiveHour, 31.0, 2_400),
                window(WindowKind::Weekly, 23.0, 388_200),
            ],
            0,
        )
        .with_context(Some(crate::model::ContextUsage::new(43.0).unwrap()));
        let used = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Used,
            gauges(30),
        );
        for (token, expected) in [
            (&used.quota_context, "43%"),
            (&used.quota_5h, "31%"),
            (&used.quota_week, "23%"),
        ] {
            assert!(token.contains(expected), "{token} lacks {expected}");
        }
        let remaining = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            gauges(30),
        );
        for (token, expected) in [
            (&remaining.quota_context, "57%"),
            (&remaining.quota_5h, "69%"),
            (&remaining.quota_week, "77%"),
        ] {
            assert!(token.contains(expected), "{token} lacks {expected}");
        }
    }

    /// Colour reads headroom on every row, so the context row bands on
    /// remaining context and not on the number it happens to print.
    #[test]
    fn the_context_row_is_coloured_by_remaining_context_under_either_style() {
        for (used, expected) in [
            (0.0, Severity::Normal),
            (31.0, Severity::Normal),
            (49.0, Severity::Normal),
            (50.0, Severity::Normal),
            (51.0, Severity::Warning),
            (53.0, Severity::Warning),
            (79.0, Severity::Warning),
            (80.0, Severity::Warning),
            (81.0, Severity::Danger),
            (85.0, Severity::Danger),
            (100.0, Severity::Danger),
        ] {
            let snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 0)
                .with_context(Some(crate::model::ContextUsage::new(used).unwrap()));
            for style in [PercentStyle::Remaining, PercentStyle::Used] {
                let values = MetadataTokens::from_snapshot_for_session(
                    &snapshot,
                    0,
                    None,
                    style,
                    gauges(30),
                );
                assert_eq!(
                    values.quota_context_severity,
                    Some(expected),
                    "{used} used, {style:?}"
                );
            }
        }
    }

    /// Six cells, the default 26-column sidebar. The meter fills to the
    /// number beside it, and `cx` exists only under `gauges`.
    #[test]
    fn the_context_meter_fills_to_its_number_and_is_labelled_cx_only_under_gauges() {
        let snapshot = ProviderSnapshot::new(Provider::Claude, vec![], 0)
            .with_context(Some(crate::model::ContextUsage::new(31.0).unwrap()));
        let gauged = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Used,
            gauges(26),
        );
        assert_eq!(
            gauged.quota_context,
            "cx  \u{25b0}\u{25b0}\u{25b1}\u{25b1}\u{25b1}\u{25b1}  31%"
        );
        for layout in [SidebarLayout::Packed, SidebarLayout::Stacked] {
            let plain = MetadataTokens::from_snapshot_for_session(
                &snapshot,
                0,
                None,
                PercentStyle::Used,
                layout.into(),
            );
            assert_eq!(plain.quota_context, "context 31%", "{layout:?}");
        }
    }

    /// Six cells. The weekly slot renders through `from_snapshot_parts`, so
    /// it needs its own pin.
    #[test]
    fn the_weekly_window_carries_a_meter_of_its_own() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::Weekly, 17.0, 424_800)],
            0,
        );
        let values = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            gauges(26),
        );
        assert_eq!(
            values.quota_week,
            "7d  \u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b1}  83% 4d22h"
        );
        assert_eq!(values.quota_week.chars().count(), 21);
    }

    /// Eight cells. Only 0 may draw nothing and only 100 may draw everything,
    /// so a window with quota left never looks spent.
    #[test]
    fn only_zero_draws_an_empty_meter_and_only_a_hundred_draws_a_full_one() {
        assert_eq!(
            meter(0, 8),
            "\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}"
        );
        assert_eq!(
            meter(100, 8),
            "\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}"
        );
        for printed in [1, 6] {
            assert_eq!(
                meter(printed, 8).matches('\u{25b0}').count(),
                1,
                "printed {printed}"
            );
        }
        assert_eq!(meter(99, 8).matches('\u{25b0}').count(), 7);
        assert_eq!(meter(50, 8).matches('\u{25b0}').count(), 4);
        assert_eq!(meter(56, 8).matches('\u{25b0}').count(), 4);
        assert_eq!(meter(57, 8).matches('\u{25b0}').count(), 5);
    }

    /// Eight cells, at 28 columns. `{:.0}` rounds half to even and
    /// `f64::round` rounds half away from zero; at exactly 18.5 they
    /// disagree, and a bar that read the float directly would show two cells
    /// beside `18%`.
    #[test]
    fn the_meter_and_the_number_round_the_same_way_at_a_half_percent() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::FiveHour, 81.5, 2_580)],
            0,
        );
        let values = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            gauges(28),
        );
        assert_eq!(
            values.quota_5h,
            "5h  \u{25b0}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}  18% 43m"
        );
    }

    /// Twelve cells at 36 columns; none at 18, where the row must be exactly
    /// what `stacked` publishes rather than a truncated meter.
    #[test]
    fn a_wider_sidebar_lengthens_the_meter_and_a_narrow_one_drops_it() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::Weekly, 17.0, 424_800)],
            0,
        );
        let wide = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            gauges(36),
        );
        assert_eq!(
            wide.quota_week,
            "7d  \u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b1}\u{25b1}  83% 4d22h"
        );

        let narrow = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            gauges(18),
        );
        let stacked = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            SidebarLayout::Stacked.into(),
        );
        assert_eq!(narrow.quota_week, stacked.quota_week);
        assert!(!narrow.quota_week.contains('\u{2026}'));
    }

    #[test]
    fn a_missing_five_hour_window_stays_omitted_under_every_layout() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::Weekly, 17.0, 424_800)],
            0,
        );
        for shape in [
            SidebarLayout::Packed.into(),
            SidebarLayout::Stacked.into(),
            gauges(26),
        ] {
            let values = MetadataTokens::from_snapshot_for_session(
                &snapshot,
                0,
                None,
                PercentStyle::Remaining,
                shape,
            );
            assert_eq!(values.quota_5h, "", "{shape:?}");
            assert_eq!(values.quota_5h_severity, None, "{shape:?}");
        }
    }

    /// A label wider than the three-character column keeps the whole row on
    /// the non-gauge shape, because truncating a label is worse than
    /// dropping a bar.
    #[test]
    fn a_label_too_long_for_the_gauges_column_keeps_the_non_gauge_shape() {
        let mut snapshot = ProviderSnapshot::new(
            Provider::Omp,
            vec![window(WindowKind::FiveHour, 58.0, 11_400).with_source_window("usage", None)],
            0,
        );
        snapshot.source = "omp.acme".to_string();
        let values = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            gauges(26),
        );
        assert_eq!(values.quota_5h, "usage 42% 3h10m");
    }

    /// Six cells. `week_style_base` folds week beside context when 5h is
    /// empty; the meter has to ride along into that token too.
    #[test]
    fn a_week_row_folded_beside_context_still_carries_its_meter() {
        let snapshot = ProviderSnapshot::new(
            Provider::Grok,
            vec![window(WindowKind::Weekly, 17.0, 424_800)],
            0,
        );
        let values = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            gauges(26),
        );
        assert_eq!(values.quota_5h, "");
        assert_eq!(
            values.quota_week,
            "7d  \u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b1}  83% 4d22h"
        );
    }

    /// Herdr joins sibling tokens with ` \u{b7} `, so a value carrying one
    /// reads as two tokens; and no value may approach the token budget.
    #[test]
    fn no_gauges_token_carries_a_separator_or_approaches_the_token_budget() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![
                window(WindowKind::FiveHour, 0.0, 86_340),
                window(WindowKind::Weekly, 17.0, 424_800),
            ],
            0,
        )
        .with_context(Some(crate::model::ContextUsage::new(31.0).unwrap()));
        let values = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            gauges(26),
        );
        // 100% with the longest ETA `format_duration` prints is the widest
        // row the default width has to hold.
        assert_eq!(
            values.quota_5h,
            "5h  \u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0} 100% 23h59m"
        );
        for value in [&values.quota_5h, &values.quota_week, &values.quota_context] {
            assert!(!value.contains('\u{b7}'), "{value}");
            assert!(value.chars().count() < 80, "{value}");
        }
        assert_eq!(values.quota_5h.chars().count(), 22);
        assert!(values.quota_5h.chars().count() <= gauges(26).content_width);
    }

    /// Six cells. The label column is three characters wide, so `cx` and `5h`
    /// pad to `30d` and the meters line up.
    #[test]
    fn the_gauges_label_column_is_three_characters_wide() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::FiveHour, 19.0, 2_580)],
            0,
        )
        .with_context(Some(crate::model::ContextUsage::new(31.0).unwrap()));
        let values = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Used,
            gauges(26),
        );
        assert!(
            values.quota_5h.starts_with("5h  \u{25b0}"),
            "{}",
            values.quota_5h
        );
        assert!(
            values.quota_context.starts_with("cx  \u{25b0}"),
            "{}",
            values.quota_context
        );
    }

    /// A monthly allowance rides the long-window slot when a plan has no
    /// weekly bucket, so losing its meter would leave that user without one
    /// on their only recurring row. Three characters fit `30d` without an
    /// alias; `packed` keeps the same label.
    #[test]
    fn a_monthly_window_keeps_its_meter_under_a_three_character_label() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::Monthly, 17.0, 424_800)],
            0,
        );
        let gauged = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            gauges(26),
        );
        assert_eq!(
            gauged.quota_month,
            "30d \u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b1}  83% 4d22h"
        );

        let packed = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            SidebarLayout::Packed.into(),
        );
        assert_eq!(packed.quota_month, "30d 83% 4d22h");
    }

    /// `30d 100% 29d23h` is one column past an 18-wide content area. The
    /// number stays; the ETA drops to its leading unit.
    #[test]
    fn a_narrow_gauges_row_shortens_eta_before_dropping_the_percent() {
        let snapshot = ProviderSnapshot::new(
            Provider::Claude,
            vec![window(WindowKind::Monthly, 100.0, 29 * 86400 + 23 * 3600)],
            0,
        );
        let values = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Used,
            gauges(18),
        );
        assert_eq!(values.quota_month, "30d 100% 29d");
        assert!(values.quota_month.chars().count() <= gauges(18).content_width);
    }

    /// A provider-supplied label is never rewritten, so one too long for the
    /// column keeps the plain row rather than being abbreviated by guesswork.
    #[test]
    fn a_provider_supplied_long_label_still_keeps_the_plain_row() {
        let long = UsageWindow::new(
            WindowKind::Monthly,
            17.0,
            Some(ResetAt::from_unix_seconds(424_800)),
        )
        .unwrap()
        .with_source_window("Monthly", None);
        let snapshot = ProviderSnapshot::new(Provider::Omp, vec![long], 0);
        let values = MetadataTokens::from_snapshot_for_session(
            &snapshot,
            0,
            None,
            PercentStyle::Remaining,
            gauges(26),
        );
        assert_eq!(values.quota_month, "Monthly 83% 4d22h");
    }

    // --- Claude status line spending pace ---------------------------------

    const HOUR: u64 = 3_600;
    const DAY: u64 = 24 * HOUR;

    /// 58% spent with 40% of the 5h window gone: 18 points ahead of the
    /// clock, so slow down.
    #[test]
    fn pace_ahead_of_the_clock_says_slow_down() {
        let windows = vec![window(WindowKind::FiveHour, 58.0, 3 * HOUR)];
        assert_eq!(pace_segment(&windows, 0).as_deref(), Some("⏱ 5h ↓18%"));
    }

    /// 10% spent with 80% of the window gone: 70 points of headroom.
    #[test]
    fn pace_behind_the_clock_says_speed_up() {
        let windows = vec![window(WindowKind::FiveHour, 10.0, HOUR)];
        assert_eq!(pace_segment(&windows, 0).as_deref(), Some("⏱ 5h ↑70%"));
    }

    /// Within five points of the clock is on pace; no arrow, no number.
    #[test]
    fn pace_within_tolerance_is_on_pace() {
        let windows = vec![window(WindowKind::FiveHour, 42.0, 3 * HOUR)];
        assert_eq!(pace_segment(&windows, 0).as_deref(), Some("⏱ 5h ="));
        let windows = vec![window(WindowKind::FiveHour, 36.0, 3 * HOUR)];
        assert_eq!(pace_segment(&windows, 0).as_deref(), Some("⏱ 5h ="));
    }

    #[test]
    fn pace_needs_a_reset_time() {
        let windows = vec![UsageWindow::new(WindowKind::FiveHour, 58.0, None).unwrap()];
        assert_eq!(pace_segment(&windows, 0), None);
        assert_eq!(pace_segment(&[], 0), None);
    }

    /// Right after a reset the elapsed fraction is noise; say nothing until
    /// 5% of the window (15 minutes of a 5h one) has passed.
    #[test]
    fn pace_is_silent_just_after_a_reset() {
        let windows = vec![window(WindowKind::FiveHour, 3.0, 5 * HOUR - 2 * 60)];
        assert_eq!(pace_segment(&windows, 0), None);
        let windows = vec![window(WindowKind::FiveHour, 3.0, 5 * HOUR - 16 * 60)];
        assert_eq!(pace_segment(&windows, 0).as_deref(), Some("⏱ 5h ="));
    }

    /// An expired window or a reset further away than the window is long
    /// (clock skew, provider quirk) is not a pace either.
    #[test]
    fn pace_is_silent_for_expired_or_implausible_windows() {
        let expired = vec![window(WindowKind::FiveHour, 58.0, 100)];
        assert_eq!(pace_segment(&expired, 200), None);
        let too_far = vec![window(WindowKind::FiveHour, 58.0, 6 * HOUR)];
        assert_eq!(pace_segment(&too_far, 0), None);
    }

    /// Both windows present: pace against the one with the least remaining
    /// quota, the same rule the sidebar's headroom uses, and say which.
    #[test]
    fn pace_follows_the_tightest_window() {
        let five_hour_binds = vec![
            window(WindowKind::FiveHour, 58.0, 3 * HOUR),
            window(WindowKind::Weekly, 27.0, 2 * DAY),
        ];
        assert_eq!(
            pace_segment(&five_hour_binds, 0).as_deref(),
            Some("⏱ 5h ↓18%")
        );
        // 90% of the week spent with 5 of 7 days gone (71% elapsed).
        let weekly_binds = vec![
            window(WindowKind::FiveHour, 20.0, 3 * HOUR),
            window(WindowKind::Weekly, 90.0, 2 * DAY),
        ];
        assert_eq!(pace_segment(&weekly_binds, 0).as_deref(), Some("⏱ 7d ↓19%"));
    }

    /// The binding window is chosen before asking whether it can be paced.
    /// When it cannot, nothing is shown: pacing the looser window instead
    /// would put a confident arrow on the wrong budget.
    #[test]
    fn a_degraded_binding_window_silences_the_pace_instead_of_falling_back() {
        // Weekly is tighter (40 left vs 42) and just reset.
        let tighter_just_reset = vec![
            window(WindowKind::FiveHour, 58.0, 3 * HOUR),
            window(WindowKind::Weekly, 60.0, 7 * DAY - HOUR),
        ];
        assert_eq!(pace_segment(&tighter_just_reset, 0), None);
        // 5h is tighter and has no reset time.
        let tighter_no_reset = vec![
            UsageWindow::new(WindowKind::FiveHour, 70.0, None).unwrap(),
            window(WindowKind::Weekly, 27.0, 2 * DAY),
        ];
        assert_eq!(pace_segment(&tighter_no_reset, 0), None);
        // 5h is tighter and expired.
        let tighter_expired = vec![
            window(WindowKind::FiveHour, 70.0, 100),
            window(WindowKind::Weekly, 27.0, 200 + 2 * DAY),
        ];
        assert_eq!(pace_segment(&tighter_expired, 200), None);
    }

    /// The band is decided on the unrounded difference and is inclusive:
    /// 4.4, 4.6 and 5.0 points are on pace, 5.4 is not.
    #[test]
    fn pace_band_boundary_is_inclusive_and_unrounded() {
        // 3h left of 5h: 40.0% elapsed exactly.
        let at = |used: f64| pace_segment(&[window(WindowKind::FiveHour, used, 3 * HOUR)], 0);
        assert_eq!(at(44.4).as_deref(), Some("⏱ 5h ="));
        assert_eq!(at(44.6).as_deref(), Some("⏱ 5h ="));
        assert_eq!(at(45.0).as_deref(), Some("⏱ 5h ="));
        assert_eq!(at(45.4).as_deref(), Some("⏱ 5h ↓5%"));
        assert_eq!(at(35.6).as_deref(), Some("⏱ 5h ="));
        assert_eq!(at(35.0).as_deref(), Some("⏱ 5h ="));
        assert_eq!(at(34.6).as_deref(), Some("⏱ 5h ↑5%"));
    }
}
