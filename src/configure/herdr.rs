use crate::cli::{
    AgentSelection, BrandColors, FieldSet, SidebarField, SidebarLayout, SidebarRowGap,
};
use crate::identity::{self, PLUGIN_ID};
use crate::model::Harness;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

const QUOTA_ROW_MARKERS: [&str; 68] = [
    "$quota_badge",
    "$quota_state",
    "$quota_icon",
    "$quota_icon_working",
    "$quota_icon_done",
    "$quota_group",
    "$quota_pad",
    "$quota_nest_gap",
    "$quota_provider",
    "$quota_model",
    "$quota_provider_model",
    "$quota_status",
    "$quota_summary",
    "$quota_session",
    "$quota_context",
    "$quota_context_normal",
    "$quota_context_warning",
    "$quota_context_danger",
    "$quota_cache",
    "$quota_cache_ttl",
    "$quota_cache_state",
    "$quota_error",
    "$quota_topic",
    "$quota_5h",
    "$quota_5h_percent",
    "$quota_week",
    "$quota_header",
    "$quota_5h_label",
    "$quota_5h_eta",
    "$quota_5h_normal",
    "$quota_5h_caution",
    "$quota_5h_warning",
    "$quota_5h_danger",
    "$quota_5h_unknown",
    "$quota_week_label",
    "$quota_week_eta",
    "$quota_week_normal",
    "$quota_week_caution",
    "$quota_week_warning",
    "$quota_week_danger",
    "$quota_week_unknown",
    "$quota_week_inline_label",
    "$quota_week_inline_eta",
    "$quota_week_inline_normal",
    "$quota_week_inline_caution",
    "$quota_week_inline_warning",
    "$quota_week_inline_danger",
    "$quota_week_inline_unknown",
    "$quota_month_normal",
    "$quota_month_warning",
    "$quota_month_danger",
    "$quota_month_unknown",
    "$quota_share_5h_normal",
    "$quota_share_5h_warning",
    "$quota_share_5h_danger",
    "$quota_share_5h_unknown",
    "$quota_share_week_normal",
    "$quota_share_week_warning",
    "$quota_share_week_danger",
    "$quota_share_week_unknown",
    "$quota_share_week_inline_normal",
    "$quota_share_week_inline_warning",
    "$quota_share_week_inline_danger",
    "$quota_share_week_inline_unknown",
    "$quota_share_month_normal",
    "$quota_share_month_warning",
    "$quota_share_month_danger",
    "$quota_share_month_unknown",
];
const ROW_GAP_MARKER: &str = PLUGIN_ID;
const REFRESH_KEY: &str = "prefix+shift+r";
const SETTINGS_KEY: &str = "prefix+shift+q";
const CONFIG_PRESENCE_FILE: &str = "herdr-config.original.present";
// Brand answers "who"; status answers "how much is left". All other text
// inherits Herdr's active theme. Selected state may change background only —
// never the provider hue. Herdr 0.8.0 rejects selection_bg / active_row_bg
// (0.8.2 added them); intended selected fill is #42474f when those keys exist.
const QUOTA_SAFE_COLOR: &str = "#82d978";
const QUOTA_WARNING_COLOR: &str = "#e4b957";
/// Idle logo ink. Working / done / blocked colours match Herdr's default dark
/// `state_icon` palette so the brand glyph replaces the ring without a second
/// circle on the row.
const IDLE_ICON_COLOR: &str = "#e9e9f0";
const WORKING_ICON_COLOR: &str = "#f9e2af";
const DONE_ICON_COLOR: &str = "#94e2d5";
const BLOCKED_ICON_COLOR: &str = "#f38ba8";
const QUOTA_DANGER_COLOR: &str = "#f16f7e";
// The same three bands, muted, for the meter rows only. `packed` and
// `stacked` tint one short token, where a full-strength hue is legible; a
// `gauges` row repeats it across a dozen bar glyphs, where it reads as alarm
// rather than as a reading. Both sets exist because Herdr fixes `fg` per token
// name, so a layout cannot restyle a token it shares with another layout.
const GAUGE_QUOTA_SAFE_COLOR: &str = "#98b17d";
const GAUGE_QUOTA_WARNING_COLOR: &str = "#dec27f";
const GAUGE_QUOTA_DANGER_COLOR: &str = "#df919b";
const SEVERITY_PALETTE: [&str; 3] = [QUOTA_SAFE_COLOR, QUOTA_WARNING_COLOR, QUOTA_DANGER_COLOR];
const GAUGE_SEVERITY_PALETTE: [&str; 3] = [
    GAUGE_QUOTA_SAFE_COLOR,
    GAUGE_QUOTA_WARNING_COLOR,
    GAUGE_QUOTA_DANGER_COLOR,
];

/// The normal/warning/danger hues a layout paints its quota rows with.
fn severity_palette(layout: SidebarLayout) -> [&'static str; 3] {
    match layout {
        SidebarLayout::Gauges => GAUGE_SEVERITY_PALETTE,
        SidebarLayout::Packed | SidebarLayout::Stacked => SEVERITY_PALETTE,
    }
}
/// Provider row keys only. Provider and model text inherit the sidebar theme;
/// status colour lives on the vendor icon's three mutually exclusive tokens.
const PROVIDER_STYLES: [(Harness, &str); 10] = [
    (Harness::Claude, "claude"),
    (Harness::Codex, "codex"),
    (Harness::Grok, "grok"),
    (Harness::Agy, "agy"),
    (Harness::OpenCode, "opencode"),
    (Harness::Pi, "pi"),
    (Harness::Omp, "omp"),
    (Harness::Devin, "devin"),
    (Harness::Muse, "muse"),
    (Harness::Cursor, "cursor"),
];
const THEME_SELECTION_KEYS: [&str; 2] = ["selection_bg", "active_row_bg"];
const OFFICIAL_IDENTITY_TOKENS: [&str; 4] = ["state_icon", "machine", "workspace", "tab"];

/// Sidebar rows for the selected agents only, so `--agent grok` never writes
/// or removes another agent's row.
fn selected_providers(agents: &[Harness]) -> impl Iterator<Item = &'static str> + '_ {
    PROVIDER_STYLES
        .into_iter()
        .filter(move |(harness, _)| agents.contains(harness))
        .map(|(_, provider)| provider)
}

pub fn check(
    agents: &[Harness],
    layout: SidebarLayout,
    row_gap: SidebarRowGap,
    fields: FieldSet,
    brand: BrandColors,
) -> Result<()> {
    let path = config_path()?;
    let original = fs::read_to_string(&path).unwrap_or_default();
    let (updated, skipped) =
        rewrite_quota_sidebar(&original, agents, layout, row_gap, fields, brand)?;
    if updated == original {
        println!(
            "Herdr sidebar already contains quota tokens: {}",
            path.display()
        );
    } else {
        println!(
            "Herdr sidebar preview ({}) for {}:",
            layout.as_str(),
            path.display()
        );
        print_diff_hint(layout, fields, brand);
    }
    if let Some(line) = skipped_provider_notice(&skipped) {
        println!("{line}");
    }
    Ok(())
}

pub fn apply(
    agents: &[Harness],
    layout: SidebarLayout,
    row_gap: SidebarRowGap,
    fields: FieldSet,
    brand: BrandColors,
) -> Result<()> {
    let path = config_path()?;
    let existed = path.exists();
    let original = fs::read_to_string(&path).unwrap_or_default();
    let (updated, skipped) =
        rewrite_quota_sidebar(&original, agents, layout, row_gap, fields, brand)?;
    if let Some(line) = skipped_provider_notice(&skipped) {
        println!("{line}");
    }
    if updated == original {
        return Ok(());
    }
    if let Some(backup) = backup_path()? {
        write_backup(&backup, &original, existed)?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create Herdr config directory")?;
    }
    fs::write(&path, updated).context("write Herdr config")?;
    println!("Added quota sidebar row to {}", path.display());
    Ok(())
}

/// `full` removes the whole sidebar installation, including the backup that
/// makes it reversible. A narrower selection only drops those agents' rows and
/// deliberately keeps the backup, because the rest is still installed.
pub fn uninstall(
    agents: &[Harness],
    full: bool,
    fields: FieldSet,
    brand: BrandColors,
) -> Result<()> {
    let path = config_path()?;
    if !full {
        if path.exists() {
            let original = fs::read_to_string(&path).context("read Herdr config")?;
            let updated = remove_quota_row_for(&original, agents, false)?;
            if updated != original {
                fs::write(&path, updated).context("remove quota sidebar rows")?;
                println!(
                    "Removed selected quota sidebar rows from {}",
                    path.display()
                );
            }
        }
        return Ok(());
    }
    if path.exists() {
        let original = fs::read_to_string(&path).context("read Herdr config")?;
        let updated =
            reversible_backup(&original, fields, brand)?.unwrap_or(remove_quota_row(&original)?);
        let originally_absent = backup_presence_path()?
            .and_then(|path| fs::read_to_string(path).ok())
            .is_some_and(|value| value.trim() == "absent");
        if originally_absent && updated.is_empty() {
            fs::remove_file(&path).context("remove empty Herdr config")?;
            println!(
                "Removed quota sidebar configuration from {}",
                path.display()
            );
        } else if updated != original {
            fs::write(&path, updated).context("remove quota sidebar row")?;
            println!("Removed quota sidebar row from {}", path.display());
        }
    }
    if let Some(backup) = backup_path()? {
        if backup.exists() {
            fs::remove_file(backup).context("remove Herdr config backup")?;
        }
        if let Some(presence) = backup_presence_path()? {
            if presence.exists() {
                fs::remove_file(presence).context("remove Herdr config backup marker")?;
            }
        }
    }
    Ok(())
}

pub fn config_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("HERDR_CONFIG_FILE") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/herdr/config.toml"))
}

/// Herdr's own documented defaults for the sidebar width keys, used whenever
/// its config does not say (or says something unusable).
pub(crate) const DEFAULT_SIDEBAR_WIDTH: usize = 26;
pub(crate) const DEFAULT_SIDEBAR_MIN_WIDTH: usize = 18;
pub(crate) const DEFAULT_SIDEBAR_MAX_WIDTH: usize = 36;
/// No terminal is this wide, so a larger value is a typo rather than a choice.
const MAX_PLAUSIBLE_SIDEBAR_WIDTH: i64 = 1_000;

/// The connected endpoint's saved manual width, then its configured width.
/// Herdr persists chrome preferences per socket, not per rendering client;
/// metadata shared by several clients cannot be sized independently for each.
///
/// Read live on every refresh pass rather than cached: the watcher is
/// long-lived and each hook is its own process, so a cached width would let
/// the two publish different meter widths for the same pane.
pub fn sidebar_width() -> usize {
    let config = config_path()
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .unwrap_or_default();
    let (configured, minimum, maximum) = sidebar_width_settings(&config);
    client_shell_sidebar_width()
        .or(configured)
        .unwrap_or(DEFAULT_SIDEBAR_WIDTH)
        .clamp(minimum, maximum)
}

/// Read only the connected endpoint's chrome preferences. Without a socket
/// identity or a usable saved width, use config rather than another endpoint.
fn client_shell_sidebar_width() -> Option<usize> {
    let state = client_shell_state_dir()?;
    let socket = std::env::var_os("HERDR_SOCKET_PATH").filter(|socket| !socket.is_empty())?;
    let file = state.join(client_shell_file_name(Path::new(&socket)));
    let contents = fs::read_to_string(file).ok()?;
    client_shell_width_for_config(&contents)
}

fn client_shell_state_dir() -> Option<PathBuf> {
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        if !state.is_empty() {
            return Some(PathBuf::from(state).join("herdr/client-shell"));
        }
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/state/herdr/client-shell"))
}

/// Herdr 0.9 `client/shell/preferences.rs::path_for_local_endpoint`: FNV-1a
/// over the socket path. Keep the persisted filename contract pinned by a test.
fn client_shell_file_name(socket: &Path) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in socket.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("local-{hash:016x}.json")
}

fn client_shell_width_for_config(state: &str) -> Option<usize> {
    let value: serde_json::Value = serde_json::from_str(state).ok()?;
    let width = value.get("sidebar_width")?.as_i64()?;
    (1..=MAX_PLAUSIBLE_SIDEBAR_WIDTH)
        .contains(&width)
        .then(|| usize::try_from(width).ok())
        .flatten()
}

/// Strictly read-only: a refresh must never rewrite or reformat the user's
/// config, and an unreadable or malformed one must not abort the refresh.
///
/// The configured width if the config states a usable one, and the bounds any
/// width — wherever it came from — is clamped into.
fn sidebar_width_settings(config: &str) -> (Option<usize>, usize, usize) {
    let Ok(document) = config.parse::<DocumentMut>() else {
        return (None, DEFAULT_SIDEBAR_MIN_WIDTH, DEFAULT_SIDEBAR_MAX_WIDTH);
    };
    let ui = document.get("ui").and_then(Item::as_table_like);
    let width = |key: &str| {
        ui.and_then(|table| table.get(key))
            .and_then(Item::as_integer)
            .filter(|value| (1..=MAX_PLAUSIBLE_SIDEBAR_WIDTH).contains(value))
            .and_then(|value| usize::try_from(value).ok())
    };
    let minimum = width("sidebar_min_width").unwrap_or(DEFAULT_SIDEBAR_MIN_WIDTH);
    let maximum = width("sidebar_max_width")
        .unwrap_or(DEFAULT_SIDEBAR_MAX_WIDTH)
        .max(minimum);
    (width("sidebar_width"), minimum, maximum)
}

fn backup_path() -> Result<Option<PathBuf>> {
    let state = std::env::var_os("HERDR_PLUGIN_STATE_DIR");
    Ok(state.map(|directory| PathBuf::from(directory).join("herdr-config.original.toml")))
}

fn backup_presence_path() -> Result<Option<PathBuf>> {
    Ok(std::env::var_os("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .map(|directory| directory.join(CONFIG_PRESENCE_FILE)))
}

fn write_backup(path: &Path, original: &str, existed: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create plugin state directory")?;
    }
    if !path.exists() {
        fs::write(path, original).context("write Herdr config backup")?;
        if let Some(marker) = backup_presence_path()? {
            fs::write(marker, if existed { "present" } else { "absent" })
                .context("write Herdr config backup marker")?;
        }
    }
    Ok(())
}

fn reversible_backup(
    current: &str,
    fields: FieldSet,
    brand: BrandColors,
) -> Result<Option<String>> {
    let Some(path) = backup_path()? else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    let original = fs::read_to_string(&path).context("read Herdr config backup")?;
    if matches_installed_quota_rows(&original, current, fields, brand)? {
        return Ok(Some(original));
    }
    Ok(None)
}

/// Is `current` exactly what this plugin would have written from `original`?
///
/// The stored field set and brand choice come first: they are the ones that
/// produced the rows on disk. The full defaults follow, so a configuration
/// written before those settings existed is still recognised. Layout and row
/// gap stay brute-forced — there are only six combinations, and neither is
/// recoverable from a config this function is deciding whether to trust.
fn matches_installed_quota_rows(
    original: &str,
    current: &str,
    fields: FieldSet,
    brand: BrandColors,
) -> Result<bool> {
    let mut variants = vec![(fields, brand)];
    if !variants.contains(&(FieldSet::all(), BrandColors::On)) {
        variants.push((FieldSet::all(), BrandColors::On));
    }
    for (fields, brand) in variants {
        for layout in SidebarLayout::CHOICES {
            for gap in [SidebarRowGap::FLUSH, SidebarRowGap::SEPARATED] {
                if add_quota_row_with(
                    original,
                    &AgentSelection::SUPPORTED,
                    layout,
                    gap,
                    fields,
                    brand,
                )? == current
                {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// Full-installation form, used by callers that configure every agent.
pub fn add_quota_row(input: &str) -> Result<String> {
    add_quota_row_with(
        input,
        &AgentSelection::SUPPORTED,
        SidebarLayout::Packed,
        SidebarRowGap::default(),
        FieldSet::all(),
        BrandColors::On,
    )
}

pub fn add_quota_row_for(input: &str, agents: &[Harness], layout: SidebarLayout) -> Result<String> {
    add_quota_row_with(
        input,
        agents,
        layout,
        SidebarRowGap::default(),
        FieldSet::all(),
        BrandColors::On,
    )
}

pub fn add_quota_row_with(
    input: &str,
    agents: &[Harness],
    layout: SidebarLayout,
    row_gap: SidebarRowGap,
    fields: FieldSet,
    brand: BrandColors,
) -> Result<String> {
    Ok(rewrite_quota_sidebar(input, agents, layout, row_gap, fields, brand)?.0)
}

fn rewrite_quota_sidebar(
    input: &str,
    agents: &[Harness],
    layout: SidebarLayout,
    row_gap: SidebarRowGap,
    fields: FieldSet,
    _brand: BrandColors,
) -> Result<(String, Vec<&'static str>)> {
    let mut document = if input.trim().is_empty() {
        DocumentMut::new()
    } else {
        input
            .parse::<DocumentMut>()
            .context("parse Herdr TOML config")?
    };
    add_plugin_keybinding(
        &mut document,
        REFRESH_KEY,
        &identity::refresh_action(),
        "refresh all agent quotas",
    )?;
    add_plugin_keybinding(
        &mut document,
        SETTINGS_KEY,
        &identity::settings_action(),
        "open agent quota settings",
    )?;
    ensure_spaces_panel_sort(&mut document)?;
    let table = ensure_table(&mut document, &["ui", "sidebar", "agents"])?;
    let managed_row_gap = table
        .get("row_gap")
        .and_then(Item::as_value)
        .and_then(|value| value.decor().suffix())
        .and_then(|suffix| suffix.as_str())
        .is_some_and(identity::comment_owns);
    if !table.contains_key("row_gap") || managed_row_gap {
        // Herdr's row_gap also splits nested vendor children. Keep panes
        // packed and paint the user's 1-line gap with `$quota_nest_gap`.
        let _ = row_gap;
        let mut gap = Value::from(SidebarRowGap::FLUSH.as_i64());
        gap.decor_mut().set_suffix(format!(" # {ROW_GAP_MARKER}"));
        table.insert("row_gap", Item::Value(gap));
    }
    let original_rows = table.get("rows").and_then(Item::as_array);
    let rows_managed = table
        .get("rows")
        .and_then(Item::as_value)
        .is_some_and(has_rows_marker);
    let rows_safe = rows_managed || original_rows.is_none_or(is_safe_to_take_over);
    let managed_rows = build_managed_rows(
        original_rows,
        layout,
        fields,
        if rows_safe {
            RowRewrite::Takeover
        } else {
            RowRewrite::Preserve
        },
    )?;
    // Identity is ink-white for every harness; status colour is the exclusive
    // `$quota_icon` / `_working` / `_done` twin — never a second `state_icon`.
    let skipped = if !rows_safe {
        add_provider_rows(table, &managed_rows, agents)?
    } else {
        remove_managed_provider_rows(table, agents);
        Vec::new()
    };
    if rows_safe {
        let mut rows_value = Value::Array(managed_rows);
        rows_value
            .decor_mut()
            .set_suffix(format!(" # {}", identity::row_marker()));
        table.insert("rows", Item::Value(rows_value));
    }
    remove_managed_selection_theme(&mut document);
    Ok((document.to_string(), skipped))
}

#[derive(Clone, Copy)]
enum RowRewrite {
    Takeover,
    Preserve,
}

fn build_managed_rows(
    original: Option<&Array>,
    layout: SidebarLayout,
    fields: FieldSet,
    rewrite: RowRewrite,
) -> Result<Array> {
    let preserved = match rewrite {
        // Group headers replace the per-pane workspace/tab identity row:
        // starting from Herdr's stock machine/workspace/tab list would print
        // the Space name twice — once natively, once as `$quota_group`.
        RowRewrite::Takeover if original.is_none_or(|rows| is_default_layout(rows, true)) => {
            Array::new()
        }
        _ => {
            let mut rows = Array::new();
            if let Some(original) = original {
                for row in original {
                    let cleaned = strip_quota_tokens(row);
                    if cleaned.is_empty() {
                        continue;
                    }
                    // Drop the stock identity row when we are about to publish
                    // `$quota_group`; keeping both is the double-header the
                    // screenshot showed. Styled machine/workspace/tab counts
                    // too — a hue on the Space name is still a Space name.
                    if is_official_identity_row(&cleaned) || is_navigation_identity_row(&cleaned) {
                        continue;
                    }
                    rows.push(Value::Array(cleaned));
                }
            }
            rows
        }
    };
    let user_count = preserved.len();
    let mut updated_rows = preserved;
    append_quota_rows(&mut updated_rows, layout);
    // Keep `$quota_group` first so Space aggregation stays the head row;
    // user-owned extras (pane/git/other plugins) sit directly under it.
    if user_count > 0 {
        let mut reordered = Array::new();
        reordered.push(
            updated_rows
                .get(user_count)
                .expect("group header follows preserved rows")
                .clone(),
        );
        for index in 0..user_count {
            reordered.push(updated_rows.get(index).expect("preserved row").clone());
        }
        for index in (user_count + 1)..updated_rows.len() {
            reordered.push(updated_rows.get(index).expect("quota row").clone());
        }
        updated_rows = reordered;
    }
    retain_selected_fields(&mut updated_rows, fields);
    Ok(updated_rows)
}

// Recognize only the tab styling written by older plugin versions. A managed
// marker does not make later user-added Git, directory, or styled rows ours.
fn is_legacy_identity_row(row: &Array) -> bool {
    if row.len() != 2 || row.get(0).and_then(Value::as_str) != Some("state_icon") {
        return false;
    }
    let Some(tab) = row.get(1).and_then(Value::as_inline_table) else {
        return false;
    };
    tab.get("token").and_then(Value::as_str) == Some("tab")
        && tab.get("bold").and_then(Value::as_bool) == Some(true)
        && (tab.len() == 2
            || (tab.len() == 3 && tab.get("dim").and_then(Value::as_bool) == Some(false)))
}

fn has_rows_marker(value: &Value) -> bool {
    value
        .decor()
        .suffix()
        .and_then(|suffix| suffix.as_str())
        .is_some_and(identity::owns_row_comment)
}

fn is_safe_to_take_over(rows: &Array) -> bool {
    is_default_layout(rows, false)
}

fn is_default_layout(rows: &Array, allow_legacy: bool) -> bool {
    let native: Vec<_> = rows
        .iter()
        .map(strip_quota_tokens)
        .filter(|row| !row.is_empty())
        .collect();
    match native.as_slice() {
        [] => true,
        [identity] => {
            is_default_state_equivalent(identity)
                || (allow_legacy && is_legacy_identity_row(identity))
        }
        [identity, agent] => {
            is_default_state_equivalent(identity)
                && !identity.iter().any(|item| item.as_str() == Some("agent"))
                && is_standalone_agent_row(agent)
        }
        _ => false,
    }
}

fn is_default_state_equivalent(row: &Array) -> bool {
    let mut has_state_icon = false;
    for item in row.iter() {
        match item.as_str() {
            Some("state_icon") => has_state_icon = true,
            Some("agent" | "tab" | "machine" | "workspace") => {}
            _ => return false,
        }
    }
    has_state_icon
}

/// Stock navigation identity, including user-styled machine/workspace/tab.
/// Dropped when publishing `$quota_group` so a restyled Space name cannot
/// reprint under the group header.
fn is_navigation_identity_row(row: &Array) -> bool {
    let mut has_state_icon = false;
    for item in row.iter() {
        match configured_token_name(item) {
            Some("state_icon") => has_state_icon = true,
            Some("agent" | "tab" | "machine" | "workspace") => {}
            _ => return false,
        }
    }
    has_state_icon
}

/// Full-installation form, used by callers that remove every agent.
pub fn remove_quota_row(input: &str) -> Result<String> {
    remove_quota_row_for(input, &AgentSelection::SUPPORTED, true)
}

/// `full` means the whole plugin is being removed, so the shared base rows and
/// the plugin keybindings go too. A narrower selection only drops that agent's
/// own `rows_by_agent` entry and leaves the rest of the sidebar intact.
pub fn remove_quota_row_for(input: &str, agents: &[Harness], full: bool) -> Result<String> {
    if input.trim().is_empty() {
        return Ok(input.to_string());
    }
    // No settings are passed in here: the caller is removing rows, not
    // rewriting them. The stored preferences are what produced the rows on
    // disk, and the defaults are tried after them.
    if full
        && matches_installed_quota_rows(
            "",
            input,
            super::resolved_fields(None, None),
            super::resolved_brand_colors(None, None),
        )?
    {
        return Ok(String::new());
    }
    let mut document = input
        .parse::<DocumentMut>()
        .context("parse Herdr TOML config")?;
    if full {
        remove_plugin_keybindings(&mut document);
    }
    let Some(table) = document
        .get_mut("ui")
        .and_then(Item::as_table_mut)
        .and_then(|ui| ui.get_mut("sidebar"))
        .and_then(Item::as_table_mut)
        .and_then(|sidebar| sidebar.get_mut("agents"))
        .and_then(Item::as_table_mut)
    else {
        if full {
            remove_managed_panel_sort(&mut document);
        }
        return Ok(document.to_string());
    };
    remove_managed_provider_rows(table, agents);
    // The base rows, row gap and keybindings are shared by every agent. Only a
    // full removal may take them; otherwise the agents left installed would
    // lose their sidebar out from under them.
    if !full {
        return Ok(document.to_string());
    }
    if let Some(rows) = table.get_mut("rows").and_then(Item::as_array_mut) {
        let mut retained = Array::new();
        for row in rows.iter() {
            let cleaned = strip_quota_tokens(row);
            if !cleaned.is_empty() {
                retained.push(Value::Array(cleaned));
            }
        }
        // Managed installs drop machine/workspace/tab in favour of
        // `$quota_group`. After stripping quota tokens only `state_icon`
        // remains — put Herdr's stock identity + `agent` rows back.
        if retained.is_empty()
            || retained.iter().all(|row| {
                row.as_array().is_some_and(|items| {
                    is_official_identity_row(items)
                        || (items.len() == 1
                            && items.get(0).and_then(Value::as_str) == Some("state_icon"))
                })
            })
        {
            let mut restored = official_agent_rows();
            restored.push(herdr_native_agent_row());
            table["rows"] = Item::Value(Value::Array(restored));
        } else {
            if retained.len() == 1
                && retained
                    .get(0)
                    .and_then(Value::as_array)
                    .is_some_and(is_official_identity_row)
            {
                retained.push(herdr_native_agent_row());
            }
            table["rows"] = Item::Value(Value::Array(retained));
        }
    }
    let managed_row_gap = table
        .get("row_gap")
        .and_then(Item::as_value)
        .and_then(|value| value.decor().suffix())
        .and_then(|suffix| suffix.as_str())
        .is_some_and(identity::comment_owns);
    if managed_row_gap {
        table.remove("row_gap");
    }
    remove_managed_selection_theme(&mut document);
    remove_managed_panel_sort(&mut document);
    Ok(document.to_string())
}

fn add_plugin_keybinding(
    document: &mut DocumentMut,
    key: &str,
    action: &str,
    description: &str,
) -> Result<()> {
    let keys = ensure_table(document, &["keys"])?;
    let commands = keys
        .entry("command")
        .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .context("Herdr keys.command must be an array of tables")?;
    let wanted_kind = action.rsplit('.').next().unwrap_or(action);
    let mut rewritten = false;
    for command in commands.iter_mut() {
        let Some(existing) = command.get("command").and_then(Item::as_str) else {
            continue;
        };
        if !identity::is_managed_keybinding(existing) {
            continue;
        }
        if !existing.ends_with(&format!(".{wanted_kind}")) {
            continue;
        }
        if existing != action {
            command.insert("command", Item::Value(Value::from(action)));
        }
        rewritten = true;
    }
    if rewritten {
        return Ok(());
    }
    if commands
        .iter()
        .any(|command| command.get("command").and_then(Item::as_str) == Some(action))
        || commands
            .iter()
            .any(|command| command.get("key").and_then(Item::as_str) == Some(key))
    {
        return Ok(());
    }

    let mut command = Table::new();
    command.insert("key", Item::Value(Value::from(key)));
    command.insert("type", Item::Value(Value::from("plugin_action")));
    command.insert("command", Item::Value(Value::from(action)));
    command.insert("description", Item::Value(Value::from(description)));
    commands.push(command);
    Ok(())
}

fn remove_plugin_keybindings(document: &mut DocumentMut) {
    let Some(keys) = document.get_mut("keys").and_then(Item::as_table_mut) else {
        return;
    };
    let Some(commands) = keys
        .get_mut("command")
        .and_then(Item::as_array_of_tables_mut)
    else {
        return;
    };
    let mut retained = ArrayOfTables::new();
    for command in commands.iter() {
        if command
            .get("command")
            .and_then(Item::as_str)
            .is_some_and(identity::is_managed_keybinding)
        {
            continue;
        }
        retained.push(command.clone());
    }
    if retained.is_empty() {
        keys.remove("command");
    } else {
        keys["command"] = Item::ArrayOfTables(retained);
    }
    if keys.is_empty() {
        document.remove("keys");
    }
}

fn ensure_table<'a>(document: &'a mut DocumentMut, path: &[&str]) -> Result<&'a mut Table> {
    let mut item: &mut Item = document.as_item_mut();
    for key in path {
        let table = item
            .as_table_mut()
            .context("Herdr config section is not a table")?;
        item = table.entry(key).or_insert(Item::Table(Table::new()));
    }
    item.as_table_mut()
        .context("Herdr config section is not a table")
}

fn strip_quota_tokens(row: &Value) -> Array {
    let mut cleaned = Array::new();
    if let Some(items) = row.as_array() {
        for item in items {
            let is_quota_token =
                configured_token_name(item).is_some_and(|value| QUOTA_ROW_MARKERS.contains(&value));
            if !is_quota_token {
                cleaned.push(item.clone());
            }
        }
    }
    cleaned
}

fn configured_token_name(value: &Value) -> Option<&str> {
    value.as_str().or_else(|| {
        value
            .as_inline_table()
            .and_then(|table| table.get("token"))
            .and_then(Value::as_str)
    })
}

fn add_provider_rows(
    table: &mut Table,
    rows: &Array,
    agents: &[Harness],
) -> Result<Vec<&'static str>> {
    let rows_by_agent = table
        .entry("rows_by_agent")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .context("Herdr ui.sidebar.agents.rows_by_agent must be a table")?;

    let mut skipped = Vec::new();
    for provider in selected_providers(agents) {
        let is_managed = rows_by_agent
            .get(provider)
            .and_then(Item::as_value)
            .is_some_and(has_provider_style_marker);
        if rows_by_agent.contains_key(provider) && !is_managed {
            skipped.push(provider);
            continue;
        }
        let mut value = Value::Array(rows.clone());
        value
            .decor_mut()
            .set_suffix(format!(" # {}", identity::provider_marker()));
        rows_by_agent.insert(provider, Item::Value(value));
    }
    Ok(skipped)
}

fn remove_managed_provider_rows(table: &mut Table, agents: &[Harness]) {
    let Some(rows_by_agent) = table.get_mut("rows_by_agent").and_then(Item::as_table_mut) else {
        return;
    };
    for provider in selected_providers(agents) {
        let is_managed = rows_by_agent
            .get(provider)
            .and_then(Item::as_value)
            .is_some_and(has_provider_style_marker);
        if is_managed {
            rows_by_agent.remove(provider);
        }
    }
    if rows_by_agent.is_empty() {
        table.remove("rows_by_agent");
    }
}

fn has_provider_style_marker(value: &Value) -> bool {
    value
        .decor()
        .suffix()
        .and_then(|suffix| suffix.as_str())
        .is_some_and(identity::owns_provider_comment)
}

/// Herdr 0.9's native navigation row. Plugin fields are appended below it so
/// empty metadata never removes workspace/tab identity.
///
/// The native `agent` row is omitted on purpose: `$quota_provider_model`
/// already names the harness, and keeping both shows `grok` above
/// `Grok/grok-4.6`. Uninstall puts `agent` back.
fn official_agent_rows() -> Array {
    let mut rows = Array::new();
    rows.push(Value::Array(OFFICIAL_IDENTITY_TOKENS.into_iter().collect()));
    rows
}

fn is_official_identity_row(row: &Array) -> bool {
    row.len() == OFFICIAL_IDENTITY_TOKENS.len()
        && OFFICIAL_IDENTITY_TOKENS
            .into_iter()
            .enumerate()
            .all(|(index, token)| row.get(index).and_then(Value::as_str) == Some(token))
}

fn herdr_native_agent_row() -> Value {
    Value::Array(["agent"].into_iter().collect())
}

fn is_standalone_agent_row(row: &Array) -> bool {
    row.len() == 1 && row.get(0).and_then(Value::as_str) == Some("agent")
}

fn append_quota_rows(rows: &mut Array, layout: SidebarLayout) {
    // Group header first: empty on non-head panes so Herdr collapses the row.
    // The stock machine/workspace/tab identity is omitted on purpose — the
    // group header is the Space name, and repeating it on every pane is what
    // made the list look ungrouped.
    rows.push(Value::Array(styled_row(
        "$quota_group",
        None,
        Some(true),
        Some(false),
    )));
    match layout {
        SidebarLayout::Gauges => {
            append_identity_row(rows);
            // Filled only on a nested vendor head so a single Cursor/Grok pane
            // keeps topic and context above its own 5h/7d/30d.
            append_share_window_rows(rows, layout);
            rows.push(Value::Array(styled_row(
                "$quota_model",
                Some(IDLE_ICON_COLOR),
                Some(false),
                Some(false),
            )));
            rows.push(Value::Array(styled_row(
                "$quota_topic",
                None,
                Some(false),
                Some(false),
            )));
            append_cache_error_and_context_rows(rows, layout);
            append_window_rows(rows, layout);
            append_pack_gap_row(rows);
        }
        SidebarLayout::Packed => {
            append_identity_row(rows);
            rows.push(Value::Array(styled_row(
                "$quota_topic",
                None,
                Some(false),
                Some(false),
            )));
            append_packed_quota_rows(rows);
            append_pack_gap_row(rows);
        }
        SidebarLayout::Stacked => {
            rows.push(Value::Array(identity_cells("$quota_provider", Some(true))));
            rows.push(Value::Array(styled_row(
                "$quota_model",
                Some(IDLE_ICON_COLOR),
                Some(false),
                Some(false),
            )));
            rows.push(Value::Array(styled_row(
                "$quota_topic",
                None,
                Some(false),
                Some(false),
            )));
            append_stacked_quota_rows(rows, layout);
            append_pack_gap_row(rows);
        }
    }
}

fn append_pack_gap_row(rows: &mut Array) {
    rows.push(Value::Array(styled_row(
        "$quota_nest_gap",
        None,
        Some(false),
        Some(false),
    )));
}

fn append_identity_row(rows: &mut Array) {
    rows.push(Value::Array(identity_cells(
        "$quota_provider_model",
        Some(true),
    )));
}

/// Brand icon, then the name.
///
/// Colour lives on `$quota_icon` via `rules` so the glyph stays the first
/// identity token. A later `$quota_icon_done` twin on a Space-head row
/// hang-indents one cell to the right. No `state_icon`: two circles on one
/// row is what this replaces.
fn identity_cells(name: &str, name_bold: Option<bool>) -> Array {
    let mut row = Array::new();
    row.push(identity_icon_token());
    row.push(styled_token(
        name,
        Some(IDLE_ICON_COLOR),
        name_bold,
        Some(false),
    ));
    row
}

fn identity_icon_token() -> Value {
    let mut value = InlineTable::new();
    value.insert("token", Value::from("$quota_icon"));
    value.insert("fg", Value::from(IDLE_ICON_COLOR));
    value.insert("bold", Value::from(false));
    value.insert("dim", Value::from(false));
    let mut rules = Array::new();
    rules.push(contains_fg_rule(crate::icons::DONE_TAG, DONE_ICON_COLOR));
    rules.push(contains_fg_rule(
        crate::icons::WORKING_TAG,
        WORKING_ICON_COLOR,
    ));
    rules.push(contains_fg_rule(
        crate::icons::BLOCKED_TAG,
        BLOCKED_ICON_COLOR,
    ));
    value.insert("rules", Value::Array(rules));
    Value::InlineTable(value)
}

fn contains_fg_rule(contains: &str, fg: &str) -> Value {
    let mut rule = InlineTable::new();
    rule.insert("contains", Value::from(contains));
    rule.insert("fg", Value::from(fg));
    Value::InlineTable(rule)
}

fn append_packed_quota_rows(rows: &mut Array) {
    let palette = severity_palette(SidebarLayout::Packed);
    append_cache_row(rows);

    let mut context_row = styled_row("$quota_context", None, Some(false), Some(false));
    append_window_style_tokens(&mut context_row, "quota_week_inline", palette);
    rows.push(Value::Array(context_row));

    append_window_row(rows, palette);
}

fn append_share_window_rows(rows: &mut Array, layout: SidebarLayout) {
    let palette = severity_palette(layout);
    let mut five_hour = Array::new();
    append_window_style_tokens(&mut five_hour, "quota_share_5h", palette);
    rows.push(Value::Array(five_hour));
    let mut week = Array::new();
    append_window_style_tokens(&mut week, "quota_share_week_inline", palette);
    append_window_style_tokens(&mut week, "quota_share_week", palette);
    rows.push(Value::Array(week));
    let mut month = Array::new();
    append_window_style_tokens(&mut month, "quota_share_month", palette);
    rows.push(Value::Array(month));
}

fn append_window_rows(rows: &mut Array, layout: SidebarLayout) {
    let palette = severity_palette(layout);
    let mut five_hour = Array::new();
    append_window_style_tokens(&mut five_hour, "quota_5h", palette);
    rows.push(Value::Array(five_hour));
    // Both week style families live on this row so the existing publish
    // choice (inline when 5h is empty, limits when 5h is present) still
    // renders exactly one 7d line.
    let mut week = Array::new();
    append_window_style_tokens(&mut week, "quota_week_inline", palette);
    append_window_style_tokens(&mut week, "quota_week", palette);
    rows.push(Value::Array(week));
    let mut month = Array::new();
    append_window_style_tokens(&mut month, "quota_month", palette);
    rows.push(Value::Array(month));
}

fn append_cache_error_and_context_rows(rows: &mut Array, layout: SidebarLayout) {
    let palette = severity_palette(layout);
    match layout {
        // Herdr colours each token, not each row. Folding `no cached` into
        // `$quota_cache` would share the line and paint the warning grey.
        SidebarLayout::Gauges => {
            rows.push(Value::Array(Array::from_iter([
                styled_token("$quota_cache", None, Some(false), Some(false)),
                styled_token(
                    "$quota_cache_state",
                    Some(QUOTA_WARNING_COLOR),
                    Some(false),
                    Some(false),
                ),
            ])));
            rows.push(Value::Array(styled_row(
                "$quota_cache_ttl",
                None,
                Some(false),
                Some(false),
            )));
        }
        _ => {
            rows.push(Value::Array(styled_row(
                "$quota_cache",
                None,
                Some(false),
                Some(false),
            )));
            rows.push(Value::Array(styled_row(
                "$quota_cache_ttl",
                None,
                Some(false),
                Some(false),
            )));
            rows.push(Value::Array(styled_row(
                "$quota_cache_state",
                Some(QUOTA_WARNING_COLOR),
                Some(false),
                Some(false),
            )));
        }
    }
    rows.push(Value::Array(styled_row(
        "$quota_error",
        Some(QUOTA_WARNING_COLOR),
        Some(false),
        Some(false),
    )));
    match layout {
        SidebarLayout::Gauges => {
            let mut context_row = Array::new();
            append_context_style_tokens(&mut context_row, palette);
            rows.push(Value::Array(context_row));
        }
        _ => rows.push(Value::Array(styled_row(
            "$quota_context",
            None,
            Some(false),
            Some(false),
        ))),
    }
}

fn append_stacked_quota_rows(rows: &mut Array, layout: SidebarLayout) {
    append_cache_error_and_context_rows(rows, layout);
    append_window_rows(rows, layout);
}

/// Drop the tokens of every field the user turned off, then drop the rows
/// that are left empty.
///
/// This runs over the finished rows rather than inside each builder: the
/// layouts differ in which tokens share a line, but not in which token means
/// which field, so one pass keeps packed and stacked honest at once.
fn retain_selected_fields(rows: &mut Array, fields: FieldSet) {
    let mut kept = Array::new();
    for row in rows.iter() {
        let Some(items) = row.as_array() else {
            kept.push(row.clone());
            continue;
        };
        let mut retained = Array::new();
        for item in items.iter() {
            // `$quota_provider_model` carries two fields on one token, so
            // `field_for_token` cannot decide it: hiding the model degrades it
            // to the provider, hiding the provider degrades it to the model,
            // and hiding both drops the identity line.
            if configured_token_name(item) == Some("$quota_provider_model") {
                match (
                    fields.contains(SidebarField::Provider),
                    fields.contains(SidebarField::Model),
                ) {
                    (true, true) => retained.push(item.clone()),
                    (true, false) => retained.push(styled_token(
                        "$quota_provider",
                        None,
                        Some(true),
                        Some(false),
                    )),
                    (false, true) => {
                        retained.push(styled_token("$quota_model", None, Some(false), Some(false)))
                    }
                    (false, false) => {}
                }
                continue;
            }
            match configured_token_name(item).and_then(field_for_token) {
                Some(field) if !fields.contains(field) => {}
                _ => retained.push(item.clone()),
            }
        }
        if !retained.is_empty() {
            kept.push(Value::Array(retained));
        }
    }
    *rows = kept;
}

/// The field a published token belongs to, or `None` for a token that is not
/// optional (the `$quota_error` channel). `$quota_provider_model` is not here:
/// it names two fields at once and is decided in `retain_selected_fields`.
fn field_for_token(token: &str) -> Option<SidebarField> {
    match token {
        "$quota_provider" => Some(SidebarField::Provider),
        "$quota_topic" => Some(SidebarField::Topic),
        "$quota_model" => Some(SidebarField::Model),
        "$quota_cache" | "$quota_cache_state" => Some(SidebarField::Cache),
        "$quota_cache_ttl" => Some(SidebarField::Ttl),
        "$quota_context"
        | "$quota_context_normal"
        | "$quota_context_warning"
        | "$quota_context_danger" => Some(SidebarField::Context),
        _ if token.starts_with("$quota_5h") || token.starts_with("$quota_share_5h") => {
            Some(SidebarField::FiveHour)
        }
        _ if token.starts_with("$quota_week") || token.starts_with("$quota_share_week") => {
            Some(SidebarField::Week)
        }
        _ if token.starts_with("$quota_month") || token.starts_with("$quota_share_month") => {
            Some(SidebarField::Month)
        }
        _ => None,
    }
}

fn append_cache_row(rows: &mut Array) {
    rows.push(Value::Array(Array::from_iter([
        styled_token("$quota_cache", None, Some(false), Some(false)),
        styled_token("$quota_cache_ttl", None, Some(false), Some(false)),
        styled_token(
            "$quota_cache_state",
            Some(QUOTA_WARNING_COLOR),
            Some(false),
            Some(false),
        ),
        styled_token(
            "$quota_error",
            Some(QUOTA_WARNING_COLOR),
            Some(false),
            Some(false),
        ),
    ])));
}

fn append_window_style_tokens(row: &mut Array, base: &str, palette: [&'static str; 3]) {
    // One compact token per window (`5h 0% 1h18m`). Herdr joins sibling
    // tokens with ` · `, so splitting label/percent/eta cannot stay compact.
    // Exactly the bands `Severity::for_window` can produce. There is no
    // "caution" row: that variant was unreachable, so the token could never
    // be filled and only ever consumed a slot.
    for (suffix, color) in [
        ("normal", Some(palette[0])),
        ("warning", Some(palette[1])),
        ("danger", Some(palette[2])),
        ("unknown", None),
    ] {
        row.push(styled_token(
            &format!("${base}_{suffix}"),
            color,
            Some(false),
            Some(false),
        ));
    }
}

/// The context row's own severity family. Context severity is read from the
/// context *left*, and `Severity::for_context_remaining` always lands on one
/// of these three, so there is no `unknown` variant to fill.
fn append_context_style_tokens(row: &mut Array, palette: [&'static str; 3]) {
    for (suffix, color) in ["normal", "warning", "danger"].into_iter().zip(palette) {
        row.push(styled_token(
            &format!("$quota_context_{suffix}"),
            Some(color),
            Some(false),
            Some(false),
        ));
    }
}

fn append_window_row(rows: &mut Array, palette: [&'static str; 3]) {
    let mut row = Array::new();
    for base in ["quota_5h", "quota_week", "quota_month"] {
        append_window_style_tokens(&mut row, base, palette);
    }
    rows.push(Value::Array(row));
}

fn remove_managed_selection_theme(document: &mut DocumentMut) {
    let Some(theme) = document.get_mut("theme").and_then(Item::as_table_mut) else {
        return;
    };
    let Some(custom) = theme.get_mut("custom").and_then(Item::as_table_mut) else {
        return;
    };
    for key in THEME_SELECTION_KEYS {
        let managed = custom
            .get(key)
            .and_then(Item::as_value)
            .and_then(|value| value.decor().suffix())
            .and_then(|suffix| suffix.as_str())
            .is_some_and(identity::comment_owns);
        if managed {
            custom.remove(key);
        }
    }
    if custom.is_empty() {
        theme.remove("custom");
    }
    if theme.is_empty() {
        document.remove("theme");
    }
}

/// Keep Herdr's Agent panel grouped by Space unless the user already set a
/// different `agent_panel_sort` themselves. Quota order additionally ranks by
/// headroom inside each space via the Agent view; this config key is what
/// remains when that view is cleared.
fn ensure_spaces_panel_sort(document: &mut DocumentMut) -> Result<()> {
    let ui = ensure_table(document, &["ui"])?;
    let managed = ui
        .get("agent_panel_sort")
        .and_then(Item::as_value)
        .and_then(|value| value.decor().suffix())
        .and_then(|suffix| suffix.as_str())
        .is_some_and(identity::comment_owns);
    if !ui.contains_key("agent_panel_sort") || managed {
        let mut sort = Value::from("spaces");
        sort.decor_mut().set_suffix(format!(" # {PLUGIN_ID}"));
        ui.insert("agent_panel_sort", Item::Value(sort));
    }
    Ok(())
}

fn remove_managed_panel_sort(document: &mut DocumentMut) {
    let Some(ui) = document.get_mut("ui").and_then(Item::as_table_mut) else {
        return;
    };
    let managed = ui
        .get("agent_panel_sort")
        .and_then(Item::as_value)
        .and_then(|value| value.decor().suffix())
        .and_then(|suffix| suffix.as_str())
        .is_some_and(identity::comment_owns);
    if managed {
        ui.remove("agent_panel_sort");
    }
    if ui.is_empty() {
        document.remove("ui");
    }
}

fn styled_row(token: &str, fg: Option<&str>, bold: Option<bool>, dim: Option<bool>) -> Array {
    let mut row = Array::new();
    row.push(styled_token(token, fg, bold, dim));
    row
}

fn styled_token(token: &str, fg: Option<&str>, bold: Option<bool>, dim: Option<bool>) -> Value {
    let mut value = InlineTable::new();
    value.insert("token", Value::from(token));
    if let Some(fg) = fg {
        value.insert("fg", Value::from(fg));
    }
    if let Some(bold) = bold {
        value.insert("bold", Value::from(bold));
    }
    if let Some(dim) = dim {
        value.insert("dim", Value::from(dim));
    }
    Value::InlineTable(value)
}

fn skipped_provider_notice(providers: &[&str]) -> Option<String> {
    if providers.is_empty() {
        return None;
    }
    let keys = providers
        .iter()
        .map(|name| format!("rows_by_agent.{name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let labels = providers
        .iter()
        .map(|name| skipped_provider_label(name))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "Preserved user-owned {keys}; quota rows were not installed for {labels}."
    ))
}

fn skipped_provider_label(provider: &str) -> &str {
    match provider {
        "claude" => "Claude",
        "codex" => "Codex",
        "grok" => "Grok",
        "agy" => "Agy",
        "opencode" => "OpenCode",
        "pi" => "Pi",
        "omp" => "OMP",
        "devin" => "Devin",
        "muse" => "Muse",
        "cursor" => "Cursor",
        other => other,
    }
}

fn print_diff_hint(layout: SidebarLayout, fields: FieldSet, _brand: BrandColors) {
    println!("  use the Space group header instead of repeating machine, workspace, and tab rows");
    match layout {
        SidebarLayout::Packed => {
            println!("  show the user prompt, context, and one compact severity-colored 5h/7d row");
        }
        SidebarLayout::Stacked => {
            println!(
                "  show provider, model, the user prompt, then cache, TTL, context, 5h, and 7d on their own rows"
            );
        }
        SidebarLayout::Gauges if crate::presentation::meter_cells(sidebar_width()).is_some() => {
            println!(
                "  show the user prompt, then cache, TTL, context, 5h, and 7d on their own rows, each with a meter beside the number"
            );
        }
        SidebarLayout::Gauges => {
            println!(
                "  show the user prompt, then cache, TTL, context, 5h, and 7d on their own rows"
            );
            println!(
                "  draw no meter: this sidebar is too narrow for one, so the rows render as they do under stacked"
            );
        }
    }
    let hidden: Vec<&str> = SidebarField::ALL
        .into_iter()
        .filter(|field| !fields.contains(*field))
        .map(SidebarField::name)
        .collect();
    if !hidden.is_empty() {
        println!("  leave out {}", hidden.join(", "));
    }
    println!("  paint brand icon idle/working/done (no state_icon ring)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_harness_has_a_sidebar_style_in_supported_order() {
        let styles: Vec<Harness> = PROVIDER_STYLES
            .iter()
            .map(|(harness, _)| *harness)
            .collect();
        assert_eq!(styles.as_slice(), AgentSelection::SUPPORTED.as_slice());
    }

    #[test]
    fn rewrites_alias_owned_markers_and_keybindings_to_the_current_id() {
        let original = concat!(
            "[[keys.command]]\n",
            "key = \"prefix+shift+r\"\n",
            "type = \"plugin_action\"\n",
            "command = \"herdr-agent-quota.refresh\"\n",
            "description = \"refresh all agent quotas\"\n\n",
            "[[keys.command]]\n",
            "key = \"prefix+shift+q\"\n",
            "type = \"plugin_action\"\n",
            "command = \"herdr-agent-quota.open-settings\"\n",
            "description = \"open agent quota settings\"\n\n",
            "[ui]\n",
            "agent_panel_sort = \"spaces\" # herdr-agent-quota\n\n",
            "[ui.sidebar.agents]\n",
            "row_gap = 0 # herdr-agent-quota\n",
            "rows = [[\"state_icon\", \"agent\"]] # herdr-agent-quota-row\n",
        );
        let updated = add_quota_row(original).unwrap();
        assert!(updated.contains(identity::refresh_action().as_str()));
        assert!(updated.contains(identity::settings_action().as_str()));
        assert!(updated.contains(&format!("agent_panel_sort = \"spaces\" # {PLUGIN_ID}")));
        assert!(updated.contains(&format!("row_gap = 0 # {PLUGIN_ID}")));
        assert!(updated.contains(identity::row_marker().as_str()));
        assert!(!updated.contains("herdr-agent-quota.refresh"));
        assert!(!updated.contains("herdr-agent-quota-row"));
        assert_eq!(add_quota_row(&updated).unwrap(), updated);
        let removed = remove_quota_row(&updated).unwrap();
        assert!(!removed.contains(identity::refresh_action().as_str()));
        assert!(!removed.contains("row_gap"));
    }

    #[test]
    fn extra_agent_rows_and_unmarked_legacy_styles_are_preserved() {
        // Two standalone agent rows on a marked layout: stock identity is
        // dropped for grouping, but the extra agent row stays.
        let marked = "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"machine\", \"workspace\", \"tab\"], [\"agent\"], [\"agent\"]] # herdr-agent-quota-row\n";
        let applied = add_quota_row(marked).unwrap();
        let parsed = applied.parse::<DocumentMut>().unwrap();
        let rows = parsed["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        assert_eq!(
            rows.iter()
                .filter(|row| row.as_array().is_some_and(is_standalone_agent_row))
                .count(),
            2,
            "both agent rows should survive: {applied}"
        );
        assert!(row_is_only_token(rows, "$quota_group"));
        assert_eq!(add_quota_row(&applied).unwrap(), applied);

        // Unmarked legacy tab styling is user-owned and kept above plugin rows.
        let legacy = "[ui.sidebar.agents]\nrows = [[\"state_icon\", { token = \"tab\", bold = true, dim = false }]]\n";
        let applied = add_quota_row(legacy).unwrap();
        let parsed = applied.parse::<DocumentMut>().unwrap();
        let rows = parsed["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        let first = rows.get(0).unwrap().as_array().unwrap();
        assert_eq!(first.get(0).and_then(Value::as_str), Some("state_icon"));
        assert_eq!(
            first
                .get(1)
                .and_then(Value::as_inline_table)
                .and_then(|table| table.get("token"))
                .and_then(Value::as_str),
            Some("tab")
        );
        assert_eq!(add_quota_row(&applied).unwrap(), applied);
    }

    #[test]
    fn repair_preserves_native_rows_added_after_installation() {
        let installed = add_quota_row(concat!(
            "[ui]\nagent_panel_sort = \"spaces\"\n",
            "[ui.sidebar.spaces]\n",
            "rows = [[\"state_icon\", \"workspace\"], [\"branch\", \"git_status\"]]\n",
        ))
        .unwrap();
        let mut document = installed.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array_mut()
            .unwrap();
        let mut custom = Array::new();
        custom.push("pane");
        custom.push(styled_token("workspace", Some("#123456"), None, None));
        custom.push("$git_branch");
        rows.insert(1, Value::Array(custom.clone()));
        let customized = document.to_string();

        for layout in SidebarLayout::CHOICES {
            for brand in [BrandColors::On, BrandColors::Off] {
                let apply = |input: &str| {
                    add_quota_row_with(
                        input,
                        &AgentSelection::SUPPORTED,
                        layout,
                        SidebarRowGap::default(),
                        FieldSet::all(),
                        brand,
                    )
                    .unwrap()
                };
                let repaired = apply(&customized);
                let parsed = repaired.parse::<DocumentMut>().unwrap();
                assert_eq!(parsed["ui"]["agent_panel_sort"].as_str(), Some("spaces"));
                assert_eq!(
                    parsed["ui"]["sidebar"]["spaces"].to_string(),
                    document["ui"]["sidebar"]["spaces"].to_string()
                );
                let agents = &parsed["ui"]["sidebar"]["agents"];
                let shared = agents["rows"].as_array().unwrap();
                assert_eq!(
                    shared.get(1).unwrap().to_string().trim(),
                    custom.to_string()
                );
                // Takeover installs shared rows only; brand-on no longer
                // mirrors them into rows_by_agent.
                assert!(agents.get("rows_by_agent").is_none(), "{repaired}");
                assert_eq!(apply(&repaired), repaired);
                let removed = remove_quota_row(&repaired).unwrap();
                assert!(removed.contains("pane"));
                assert!(removed.contains("$git_branch"));
                assert!(removed.contains("#123456"));
                assert!(!removed.contains("$quota_"));
            }
        }
    }

    #[test]
    fn group_header_replaces_per_pane_workspace_identity() {
        for original in [
            "",
            "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"machine\", \"workspace\", \"tab\"], [\"agent\"]]\n",
            "[ui.sidebar.agents]\nrows = [[\"state_icon\", { token = \"tab\", bold = true }, \"$quota_provider_model\"], [\"$quota_topic\"]] # herdr-agent-quota-row\n",
        ] {
            for layout in SidebarLayout::CHOICES {
                let updated = add_quota_row_for(original, &[Harness::Claude], layout).unwrap();
                let document = updated.parse::<DocumentMut>().unwrap();
                let rows = document["ui"]["sidebar"]["agents"]["rows"]
                    .as_array()
                    .unwrap();
                assert!(
                    document["ui"]["sidebar"]["agents"]
                        .get("rows_by_agent")
                        .is_none(),
                    "takeover must not write per-agent brand rows:\n{updated}"
                );
                assert!(row_is_only_token(rows, "$quota_group"), "{updated}");
                assert!(
                    !rows.iter().any(|row| {
                        row.as_array().is_some_and(|items| {
                            items.iter().any(|item| item.as_str() == Some("workspace"))
                                || items.iter().any(|item| item.as_str() == Some("machine"))
                                || items.iter().any(|item| item.as_str() == Some("tab"))
                        })
                    }),
                    "per-pane workspace identity survived grouping:\n{updated}"
                );
                assert!(
                    !has_standalone_agent_row(rows),
                    "native agent row duplicates provider/model:\n{updated}"
                );
                assert!(rows.iter().any(|row| row_contains_token(row, "$quota_icon")));
                assert!(
                    !rows
                        .iter()
                        .any(|row| row_contains_token(row, "$quota_icon_working")),
                    "working colour is a rule on $quota_icon, not a later twin"
                );
                assert!(
                    !rows
                        .iter()
                        .any(|row| row_contains_token(row, "$quota_icon_done")),
                    "done colour is a rule on $quota_icon, not a later twin"
                );
                assert!(
                    !rows.iter().any(|row| {
                        row.as_array().is_some_and(|items| {
                            items.iter().any(|item| item.as_str() == Some("state_icon"))
                                && (row_contains_token(row, "$quota_icon")
                                    || row_contains_token(row, "$quota_topic"))
                        })
                    }),
                    "managed identity must not host state_icon beside the brand icon:\n{updated}"
                );
                assert!(rows.iter().any(|row| row_contains_token(row, "$quota_topic")));
                assert_eq!(
                    add_quota_row_for(&updated, &[Harness::Claude], layout).unwrap(),
                    updated
                );
            }
        }
    }

    #[test]
    fn uninstall_puts_the_native_agent_row_back_on_a_default_layout() {
        let original = "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"machine\", \"workspace\", \"tab\"], [\"agent\"]]\n";
        let installed = add_quota_row(original).unwrap();
        let document = installed.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        assert!(!has_standalone_agent_row(rows), "{installed}");
        assert!(rows
            .iter()
            .any(|row| row_contains_token(row, "$quota_provider_model")));
        assert_eq!(
            remove_quota_row(&installed).unwrap(),
            "[ui]\n[ui.sidebar.agents]\nrows = [[\"state_icon\", \"machine\", \"workspace\", \"tab\"], [\"agent\"]]\n"
        );
    }

    #[test]
    fn custom_styles_on_native_tokens_survive_repair() {
        // Styled stock identity is not a plain default, so shared rows stay
        // user-owned. Provider copies still drop that navigation row for
        // `$quota_group`, or the Space name would print twice under brand.
        let original = "[ui.sidebar.agents]\nrows = [[\"state_icon\", { token = \"workspace\", fg = \"#123456\" }, \"tab\"], [\"agent\"]]\n";
        let updated = add_quota_row(original).unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let shared = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        let first = shared.get(0).unwrap().as_array().unwrap();
        assert_eq!(first.len(), 3);
        assert_eq!(
            first
                .get(1)
                .unwrap()
                .as_inline_table()
                .unwrap()
                .get("fg")
                .and_then(Value::as_str),
            Some("#123456")
        );
        assert_eq!(token_names(shared.get(1).unwrap()), ["agent"]);

        let provider = document["ui"]["sidebar"]["agents"]["rows_by_agent"]["claude"]
            .as_array()
            .unwrap();
        assert!(row_is_only_token(provider, "$quota_group"));
        assert_eq!(token_names(provider.get(1).unwrap()), ["agent"]);
        assert_eq!(add_quota_row(&updated).unwrap(), updated);
    }

    #[test]
    fn adds_quota_rows_with_group_header_and_vendor_icon() {
        let original = r#"[ui.sidebar.agents]
rows = [["state_icon", "agent"]]
"#;
        let updated = add_quota_row(original).unwrap();
        assert!(updated.contains("$quota_5h"));
        assert!(updated.contains("$quota_week"));
        assert!(updated.contains("$quota_group"));
        assert!(updated.contains("$quota_icon"));
        assert!(
            !updated.contains("$quota_icon_working"),
            "working colour is a rule, not a twin token:\n{updated}"
        );
        assert!(
            !updated.contains("$quota_icon_done"),
            "done colour is a rule, not a twin token:\n{updated}"
        );
        assert!(!updated.contains("state_icon") || updated.contains("machine")); // stock may remain elsewhere only if preserve
        assert!(!updated.contains("$quota_pad"));
        assert!(!updated.contains("\"workspace\""));
        assert!(!updated.contains("\"machine\""));
        assert!(updated.contains("$quota_topic"));
        assert!(updated.contains("$quota_5h_warning"));
        assert!(updated.contains("$quota_week_danger"));
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        assert!(!has_standalone_agent_row(rows), "{updated}");
        // Takeover drops state_icon from the managed identity.
        assert!(!rows.iter().any(|row| {
            row.as_array()
                .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("state_icon")))
        }));
        assert_eq!(add_quota_row(&updated).unwrap(), updated);
    }

    #[test]
    fn context_row_can_fold_weekly_without_placing_five_hour_beside_it() {
        let updated =
            add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n").unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        let context_row = rows
            .iter()
            .find(|row| {
                row.as_array().is_some_and(|items| {
                    items
                        .iter()
                        .any(|item| configured_token_name(item) == Some("$quota_context"))
                })
            })
            .and_then(Value::as_array)
            .unwrap();
        assert!(context_row
            .iter()
            .any(|item| configured_token_name(item) == Some("$quota_week_inline_normal")));
        assert!(context_row
            .iter()
            .all(|item| configured_token_name(item) != Some("$quota_5h_normal")));
        assert!(rows.iter().any(|row| {
            let items = row.as_array().unwrap();
            items
                .iter()
                .any(|item| configured_token_name(item) == Some("$quota_5h_normal"))
                && items
                    .iter()
                    .any(|item| configured_token_name(item) == Some("$quota_week_normal"))
                && items
                    .iter()
                    .all(|item| configured_token_name(item) != Some("$quota_context"))
        }));
    }

    #[test]
    fn puts_both_quota_windows_on_one_color_preserving_row() {
        let updated =
            add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n").unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        assert!(rows.iter().any(|row| {
            let items = row.as_array().unwrap();
            items
                .iter()
                .any(|item| configured_token_name(item) == Some("$quota_5h_normal"))
                && items
                    .iter()
                    .any(|item| configured_token_name(item) == Some("$quota_week_normal"))
        }));
    }

    #[test]
    fn puts_cache_rate_and_remaining_ttl_on_one_row() {
        let updated =
            add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n").unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        assert!(rows.iter().any(|row| {
            let items = row.as_array().unwrap();
            items
                .iter()
                .any(|item| configured_token_name(item) == Some("$quota_cache"))
                && items
                    .iter()
                    .any(|item| configured_token_name(item) == Some("$quota_cache_ttl"))
        }));
    }

    #[test]
    fn stacked_layout_puts_each_quota_field_on_its_own_row() {
        let original = "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n";
        let updated =
            add_quota_row_for(original, &AgentSelection::SUPPORTED, SidebarLayout::Stacked)
                .unwrap();
        assert_eq!(
            add_quota_row_for(&updated, &AgentSelection::SUPPORTED, SidebarLayout::Stacked)
                .unwrap(),
            updated
        );
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        let identity_index = rows
            .iter()
            .position(|row| {
                row_contains_token(row, "$quota_icon") && row_contains_token(row, "$quota_provider")
            })
            .unwrap();
        let group_index = rows
            .iter()
            .position(|row| row_contains_token(row, "$quota_group"))
            .unwrap();
        let provider_index = rows
            .iter()
            .position(|row| row_contains_token(row, "$quota_provider"))
            .unwrap();
        let model_index = rows
            .iter()
            .position(|row| row_contains_token(row, "$quota_model"))
            .unwrap();
        let topic_index = rows
            .iter()
            .position(|row| row_contains_token(row, "$quota_topic"))
            .unwrap();
        assert_eq!(group_index + 1, identity_index);
        assert_eq!(identity_index, provider_index);
        assert_eq!(provider_index + 1, model_index);
        assert_eq!(model_index + 1, topic_index);
        assert!(!rows
            .iter()
            .any(|row| row_contains_token(row, "$quota_provider_model")));
        assert!(rows.iter().any(|row| {
            row_contains_token(row, "$quota_icon") && row_contains_token(row, "$quota_provider")
        }));
        assert!(row_is_only_token(rows, "$quota_group"));
        assert!(row_is_only_token(rows, "$quota_model"));
        assert!(row_is_only_token(rows, "$quota_cache"));
        assert!(row_is_only_token(rows, "$quota_cache_ttl"));
        assert!(row_is_only_token(rows, "$quota_error"));
        assert!(row_is_only_token(rows, "$quota_context"));
        assert!(rows.iter().any(|row| {
            row_contains_token(row, "$quota_5h_normal")
                && !row_contains_token(row, "$quota_week_normal")
                && !row_contains_token(row, "$quota_5h_label")
                && !row_contains_token(row, "$quota_5h_eta")
        }));
        assert!(rows.iter().any(|row| {
            row_contains_token(row, "$quota_week_normal")
                && row_contains_token(row, "$quota_week_inline_normal")
                && !row_contains_token(row, "$quota_5h_normal")
                && !row_contains_token(row, "$quota_context")
        }));
        assert!(rows.iter().any(|row| {
            row_contains_token(row, "$quota_month_normal")
                && !row_contains_token(row, "$quota_week_normal")
                && !row_contains_token(row, "$quota_5h_normal")
        }));
        assert!(!rows.iter().any(|row| {
            row_contains_token(row, "$quota_cache") && row_contains_token(row, "$quota_cache_ttl")
        }));
        assert_eq!(
            remove_quota_row(
                &add_quota_row_for("", &AgentSelection::SUPPORTED, SidebarLayout::Stacked).unwrap()
            )
            .unwrap(),
            ""
        );
    }

    #[test]
    fn gauges_layout_keeps_a_packed_identity_row_above_a_stacked_body() {
        let original = "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n";
        let updated =
            add_quota_row_for(original, &AgentSelection::SUPPORTED, SidebarLayout::Gauges).unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        assert!(
            document["ui"]["sidebar"]["agents"]
                .get("rows_by_agent")
                .is_none(),
            "{updated}"
        );
        assert!(rows.iter().any(|row| {
            row_contains_token(row, "$quota_icon")
                && row_contains_token(row, "$quota_provider_model")
        }));
        assert!(row_is_only_token(rows, "$quota_group"));
        assert!(!rows
            .iter()
            .any(|row| row_contains_token(row, "$quota_provider")));
        assert!(row_is_only_token(rows, "$quota_model"));
        assert!(rows.iter().any(|row| {
            row_contains_token(row, "$quota_cache")
                && row_contains_token(row, "$quota_cache_state")
                && !row_contains_token(row, "$quota_cache_ttl")
        }));
        for token in ["$quota_cache_ttl", "$quota_error"] {
            assert!(row_is_only_token(rows, token), "{token} shares a row");
        }
        // The context row is the severity family here, not the plain
        // token, so it is a row of three names rather than one.
        assert!(rows.iter().any(|row| {
            row_contains_token(row, "$quota_context_normal")
                && !row_contains_token(row, "$quota_5h_normal")
                && !row_contains_token(row, "$quota_week_normal")
        }));
        assert!(rows.iter().any(|row| {
            row_contains_token(row, "$quota_5h_normal")
                && !row_contains_token(row, "$quota_week_normal")
        }));
        assert!(rows.iter().any(|row| {
            row_contains_token(row, "$quota_week_normal")
                && row_contains_token(row, "$quota_week_inline_normal")
                && !row_contains_token(row, "$quota_5h_normal")
        }));
        assert!(rows.iter().any(|row| {
            row_contains_token(row, "$quota_share_week_normal")
                && row_contains_token(row, "$quota_share_week_inline_normal")
        }));
        assert_eq!(
            remove_quota_row(
                &add_quota_row_for("", &AgentSelection::SUPPORTED, SidebarLayout::Gauges).unwrap()
            )
            .unwrap(),
            ""
        );
    }

    /// Herdr fixes `fg` per token name, so a coloured context row is a
    /// severity-suffixed family exactly like the windows have.
    #[test]
    fn gauges_gives_the_context_row_its_own_severity_colours() {
        let updated =
            add_quota_row_for("", &AgentSelection::SUPPORTED, SidebarLayout::Gauges).unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        let context_row = rows
            .iter()
            .find(|row| row_contains_token(row, "$quota_context_normal"))
            .and_then(Value::as_array)
            .expect("gauges context row");
        let styled = context_row
            .iter()
            .map(|item| {
                let table = item.as_inline_table().expect("styled token");
                (
                    table.get("token").and_then(Value::as_str).unwrap_or(""),
                    table.get("fg").and_then(Value::as_str).unwrap_or(""),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            styled,
            vec![
                ("$quota_context_normal", GAUGE_QUOTA_SAFE_COLOR),
                ("$quota_context_warning", GAUGE_QUOTA_WARNING_COLOR),
                ("$quota_context_danger", GAUGE_QUOTA_DANGER_COLOR),
            ]
        );
        assert!(!rows
            .iter()
            .any(|row| row_contains_token(row, "$quota_context")));
    }

    /// `packed` and `stacked` keep the plain uncoloured context token.
    #[test]
    fn packed_and_stacked_leave_the_context_row_uncoloured() {
        for layout in [SidebarLayout::Packed, SidebarLayout::Stacked] {
            let updated = add_quota_row_for("", &AgentSelection::SUPPORTED, layout).unwrap();
            let document = updated.parse::<DocumentMut>().unwrap();
            let rows = document["ui"]["sidebar"]["agents"]["rows"]
                .as_array()
                .unwrap();
            assert!(
                rows.iter()
                    .any(|row| row_contains_token(row, "$quota_context")),
                "{layout:?}"
            );
            for suffix in ["normal", "warning", "danger"] {
                let token = format!("$quota_context_{suffix}");
                assert!(
                    !rows.iter().any(|row| row_contains_token(row, &token)),
                    "{layout:?} wrote {token}"
                );
            }
        }
    }

    /// Uninstall strips by token name, so a name missing from the strip list
    /// leaves an orphaned row behind.
    #[test]
    fn uninstall_strips_every_context_token_from_a_gauges_install() {
        let installed = add_quota_row_for(
            "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n",
            &AgentSelection::SUPPORTED,
            SidebarLayout::Gauges,
        )
        .unwrap();
        assert!(installed.contains("$quota_context_normal"), "{installed}");
        let removed = remove_quota_row(&installed).unwrap();
        assert!(!removed.contains("quota_context"), "{removed}");
        assert!(!removed.contains("$quota_"), "{removed}");
    }

    /// `--fields` without `context` has to drop the row whichever family the
    /// layout publishes it into.
    #[test]
    fn hiding_context_drops_the_row_in_every_layout() {
        for layout in SidebarLayout::CHOICES {
            let updated = add_quota_row_with(
                "",
                &AgentSelection::SUPPORTED,
                layout,
                SidebarRowGap::default(),
                FieldSet::all().toggled(SidebarField::Context),
                BrandColors::On,
            )
            .unwrap();
            assert!(
                !updated.contains("$quota_context"),
                "{layout:?}:\n{updated}"
            );
        }
    }

    #[test]
    fn switching_between_any_two_layouts_is_idempotent() {
        let original = "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"tab\", \"agent\"]]\n";
        let apply = |input: &str, layout| {
            add_quota_row_for(input, &AgentSelection::SUPPORTED, layout).unwrap()
        };
        for first in SidebarLayout::CHOICES {
            for second in SidebarLayout::CHOICES {
                let switched = apply(&apply(original, first), second);
                assert_eq!(
                    apply(&switched, second),
                    switched,
                    "{first:?} then {second:?} is not a fixed point"
                );
                assert_eq!(
                    switched,
                    apply(original, second),
                    "{first:?} then {second:?} differs from a fresh {second:?} install"
                );
                assert_severity_palette(&switched, second);
            }
        }
    }

    /// The bytes `packed` and `stacked` write for the agents that existed
    /// before Muse. A 30d row was added after gauges; these digests track that
    /// template. An agent added since only appends its own row style.
    #[test]
    fn packed_and_stacked_write_the_same_bytes_as_before_gauges() {
        use sha2::{Digest, Sha256};

        let agents_before_gauges = &AgentSelection::SUPPORTED[..8];
        assert!(!agents_before_gauges.contains(&Harness::Muse));

        for (original, expected) in [
            (
                "",
                [
                    "968a9be02158fe4b5833005047630bb02b3307a54d4a25f70025b95acdb33ddf",
                    "99ca8c9d94dc74d118912dad635dcbe8192e44dec2915615dd72e1b3d0f79491",
                ],
            ),
            (
                "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"machine\", \"workspace\", \"tab\"], [\"agent\"]]\n",
                [
                    "c789fc538136af25fbd022bccb1187e033b8d007e518f4004d5a98f8f95a1259",
                    "cd148107f981ace3825d5949ca85b1e8a54ce477302bc6bbb1132a9fc6e511e7",
                ],
            ),
            (
                "[ui.sidebar.agents]\nrows = [[\"state_icon\", { token = \"tab\", bold = true }, \"$quota_provider_model\"], [\"$quota_topic\"]] # herdr-agent-quota-row\n",
                [
                    "c789fc538136af25fbd022bccb1187e033b8d007e518f4004d5a98f8f95a1259",
                    "cd148107f981ace3825d5949ca85b1e8a54ce477302bc6bbb1132a9fc6e511e7",
                ],
            ),
        ] {
            for (layout, digest) in [SidebarLayout::Packed, SidebarLayout::Stacked]
                .into_iter()
                .zip(expected)
            {
                let updated = add_quota_row_with(
                    original,
                    agents_before_gauges,
                    layout,
                    SidebarRowGap::default(),
                    FieldSet::all(),
                    BrandColors::On,
                )
                .unwrap();
                let blocked_rule = format!(
                    ", {{ contains = \"{}\", fg = \"{}\" }}",
                    crate::icons::BLOCKED_TAG,
                    BLOCKED_ICON_COLOR
                );
                assert!(
                    updated.contains(&blocked_rule),
                    "{layout:?} is missing the blocked icon rule:\n{updated}"
                );
                let stable = updated.replace(&blocked_rule, "");
                assert_eq!(
                    format!("{:x}", Sha256::digest(stable.as_bytes())),
                    digest,
                    "{layout:?} output changed outside the blocked icon rule:\n{updated}"
                );
            }
        }
    }

    /// The severity hexes a layout is expected to publish on its meter rows.
    /// `gauges` gets the muted set; the other two keep the saturated one.
    fn assert_severity_palette(sidebar: &str, layout: SidebarLayout) {
        let document = sidebar.parse::<DocumentMut>().unwrap();
        let expected = severity_palette(layout);
        for base in ["quota_5h", "quota_week", "quota_week_inline"] {
            for (suffix, hex) in ["normal", "warning", "danger"].into_iter().zip(expected) {
                let token = format!("${base}_{suffix}");
                assert_eq!(
                    token_fg(&document, &token).as_deref(),
                    Some(hex),
                    "{layout:?} wrote the wrong fg on {token}"
                );
            }
        }
        for (suffix, hex) in ["normal", "warning", "danger"].into_iter().zip(expected) {
            let token = format!("$quota_context_{suffix}");
            let fg = token_fg(&document, &token);
            match layout {
                SidebarLayout::Gauges => assert_eq!(
                    fg.as_deref(),
                    Some(hex),
                    "{layout:?} wrote the wrong fg on {token}"
                ),
                _ => assert_eq!(fg, None, "{layout:?} wrote {token}"),
            }
        }
    }

    /// The `fg` a named token carries in `rows`, or `None` when the layout
    /// never publishes it.
    fn token_fg(document: &DocumentMut, token: &str) -> Option<String> {
        document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()?
            .iter()
            .filter_map(Value::as_array)
            .flat_map(Array::iter)
            .find(|item| configured_token_name(item) == Some(token))
            .and_then(Value::as_inline_table)
            .and_then(|table| table.get("fg"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// A whole row of bar glyphs in a full-strength hue reads as alarm, so
    /// `gauges` publishes its own muted set. The other two layouts colour one
    /// short token and must keep the saturated hexes byte for byte.
    #[test]
    fn only_gauges_publishes_the_muted_severity_palette() {
        for layout in SidebarLayout::CHOICES {
            let updated = add_quota_row_for("", &AgentSelection::SUPPORTED, layout).unwrap();
            assert_severity_palette(&updated, layout);
        }
        let gauged =
            add_quota_row_for("", &AgentSelection::SUPPORTED, SidebarLayout::Gauges).unwrap();
        for hex in [
            GAUGE_QUOTA_SAFE_COLOR,
            GAUGE_QUOTA_WARNING_COLOR,
            GAUGE_QUOTA_DANGER_COLOR,
        ] {
            assert!(gauged.contains(hex), "gauges lacks {hex}:\n{gauged}");
        }
        for hex in [QUOTA_SAFE_COLOR, QUOTA_DANGER_COLOR] {
            assert!(
                !gauged.contains(hex),
                "gauges still writes {hex}:\n{gauged}"
            );
        }
        for layout in [SidebarLayout::Packed, SidebarLayout::Stacked] {
            let plain = add_quota_row_for("", &AgentSelection::SUPPORTED, layout).unwrap();
            for hex in [
                GAUGE_QUOTA_SAFE_COLOR,
                GAUGE_QUOTA_WARNING_COLOR,
                GAUGE_QUOTA_DANGER_COLOR,
            ] {
                assert!(!plain.contains(hex), "{layout:?} wrote {hex}:\n{plain}");
            }
        }
    }

    /// Shared rows carry the gauges palette; takeover no longer mirrors them
    /// into per-provider copies.
    #[test]
    fn the_gauges_palette_reaches_the_shared_rows() {
        let updated =
            add_quota_row_for("", &AgentSelection::SUPPORTED, SidebarLayout::Gauges).unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        for token in ["$quota_5h_normal", "$quota_context_normal"] {
            let fg = rows
                .iter()
                .filter_map(Value::as_array)
                .flat_map(Array::iter)
                .find(|item| configured_token_name(item) == Some(token))
                .and_then(Value::as_inline_table)
                .and_then(|table| table.get("fg"))
                .and_then(Value::as_str);
            assert_eq!(fg, Some(GAUGE_QUOTA_SAFE_COLOR), "{token}");
        }
    }

    fn row_contains_token(row: &Value, token: &str) -> bool {
        row.as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| configured_token_name(item) == Some(token))
        })
    }

    fn has_standalone_agent_row(rows: &Array) -> bool {
        rows.iter()
            .any(|row| row.as_array().is_some_and(is_standalone_agent_row))
    }

    fn row_is_only_token(rows: &Array, token: &str) -> bool {
        rows.iter().any(|row| {
            let items = row.as_array().unwrap();
            items.len() == 1
                && items
                    .iter()
                    .next()
                    .is_some_and(|item| configured_token_name(item) == Some(token))
        })
    }

    #[test]
    fn agent_identity_uses_status_coloured_brand_icons_without_state_icon() {
        let updated =
            add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"tab\", \"agent\"]]\n")
                .unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        let identity = rows
            .iter()
            .find(|row| row_contains_token(row, "$quota_icon"))
            .and_then(Value::as_array)
            .unwrap();
        assert!(!identity
            .iter()
            .any(|item| item.as_str() == Some("state_icon")));
        assert_eq!(
            configured_token_name(identity.get(0).unwrap()),
            Some("$quota_icon")
        );
        assert_eq!(
            configured_token_name(identity.get(1).unwrap()),
            Some("$quota_provider_model")
        );
        assert!(identity
            .iter()
            .any(|item| configured_token_name(item) == Some("$quota_provider_model")));
        let topic = rows
            .iter()
            .find(|row| row_contains_token(row, "$quota_topic"))
            .and_then(Value::as_array)
            .unwrap();
        assert!(
            !topic.iter().any(|item| item.as_str() == Some("state_icon")),
            "topic row does not host a status ring"
        );
        let idle = identity
            .iter()
            .find(|item| configured_token_name(item) == Some("$quota_icon"))
            .and_then(Value::as_inline_table)
            .unwrap();
        assert_eq!(
            idle.get("fg").and_then(Value::as_str),
            Some(IDLE_ICON_COLOR)
        );
        let rules = idle.get("rules").and_then(Value::as_array).unwrap();
        assert_eq!(rules.len(), 3);
        let done = rules.get(0).unwrap().as_inline_table().unwrap();
        assert_eq!(
            done.get("contains").and_then(Value::as_str),
            Some(crate::icons::DONE_TAG)
        );
        assert_eq!(
            done.get("fg").and_then(Value::as_str),
            Some(DONE_ICON_COLOR)
        );
        let working = rules.get(1).unwrap().as_inline_table().unwrap();
        assert_eq!(
            working.get("contains").and_then(Value::as_str),
            Some(crate::icons::WORKING_TAG)
        );
        assert_eq!(
            working.get("fg").and_then(Value::as_str),
            Some(WORKING_ICON_COLOR)
        );
        let blocked = rules.get(2).unwrap().as_inline_table().unwrap();
        assert_eq!(
            blocked.get("contains").and_then(Value::as_str),
            Some(crate::icons::BLOCKED_TAG)
        );
        assert_eq!(
            blocked.get("fg").and_then(Value::as_str),
            Some(BLOCKED_ICON_COLOR)
        );
    }

    #[test]
    fn idle_identity_uses_ink_white_on_the_shared_logo() {
        let updated = add_quota_row_for(
            "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n",
            &[Harness::Claude],
            SidebarLayout::Packed,
        )
        .unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        assert!(
            document["ui"]["sidebar"]["agents"]
                .get("rows_by_agent")
                .is_none(),
            "{updated}"
        );
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        let identity = rows
            .iter()
            .find(|row| row_contains_token(row, "$quota_icon"))
            .and_then(Value::as_array)
            .unwrap();
        let icon_fg = identity
            .iter()
            .find(|item| configured_token_name(item) == Some("$quota_icon"))
            .and_then(Value::as_inline_table)
            .and_then(|table| table.get("fg"))
            .and_then(Value::as_str);
        let name_fg = identity
            .iter()
            .find(|item| configured_token_name(item) == Some("$quota_provider_model"))
            .and_then(Value::as_inline_table)
            .and_then(|table| table.get("fg"))
            .and_then(Value::as_str);
        assert_eq!(icon_fg, Some(IDLE_ICON_COLOR));
        assert_eq!(name_fg, Some(IDLE_ICON_COLOR));
    }

    #[test]
    fn stacked_model_and_provider_share_idle_ink_white() {
        let updated = add_quota_row_for(
            "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n",
            &[Harness::Claude, Harness::Codex],
            SidebarLayout::Stacked,
        )
        .unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        assert!(
            document["ui"]["sidebar"]["agents"]
                .get("rows_by_agent")
                .is_none(),
            "{updated}"
        );
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        let provider_row = rows
            .iter()
            .find(|row| row_contains_token(row, "$quota_provider"))
            .and_then(Value::as_array)
            .unwrap();
        let model_row = rows
            .iter()
            .find(|row| row_contains_token(row, "$quota_model"))
            .and_then(Value::as_array)
            .unwrap();
        let provider_fg = provider_row
            .iter()
            .find(|item| configured_token_name(item) == Some("$quota_provider"))
            .and_then(Value::as_inline_table)
            .and_then(|table| table.get("fg"))
            .and_then(Value::as_str);
        let model_fg = model_row
            .iter()
            .find(|item| configured_token_name(item) == Some("$quota_model"))
            .and_then(Value::as_inline_table)
            .and_then(|table| table.get("fg"))
            .and_then(Value::as_str);
        assert_eq!(provider_fg, Some(IDLE_ICON_COLOR));
        assert_eq!(model_fg, Some(IDLE_ICON_COLOR));
    }

    #[test]
    fn gives_no_cached_a_red_token_without_spending_an_extra_metadata_slot() {
        let updated =
            add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n").unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        assert!(rows.iter().any(|row| {
            let items = row.as_array().unwrap();
            items
                .iter()
                .any(|item| configured_token_name(item) == Some("$quota_error"))
        }));
        assert!(updated.contains("fg = \"#e4b957\""));
    }

    #[test]
    fn removes_plugin_tokens_but_keeps_the_official_agent_row() {
        let original = r#"[ui.sidebar.agents]
rows = [["state_icon", "pane", "terminal_title_stripped"], ["agent", "$quota_icon", "$quota_5h"], ["$quota_week"]]
"#;
        let updated = remove_quota_row(original).unwrap();
        assert!(updated.contains("state_icon"));
        assert!(updated.contains("agent"));
        assert!(!updated.contains("$quota_summary"));
        assert!(!updated.contains("$quota_icon"));
        assert!(updated.contains("terminal_title_stripped"));
    }

    #[test]
    fn migrates_old_quota_only_rows_onto_the_group_layout() {
        let original = r#"[ui.sidebar.agents]
rows = [["$quota_provider", "$quota_status"], ["$quota_summary"]]
"#;
        let updated = add_quota_row(original).unwrap();
        assert!(updated.contains("$quota_group"));
        assert!(updated.contains("$quota_icon"));
        assert!(updated.contains("$quota_provider"));
        assert!(updated.contains("$quota_5h"));
        assert!(updated.contains("$quota_week"));
        assert_eq!(add_quota_row(&updated).unwrap(), updated);
    }

    #[test]
    fn preserves_user_owned_provider_rows() {
        // Unsafe shared rows force add_provider_rows; an unmarked claude
        // entry must stay user-owned while managed copies land for the rest.
        let original = r#"[ui.sidebar.agents]
rows = [["state_icon", "pane", "terminal_title_stripped"]]

[ui.sidebar.agents.rows_by_agent]
claude = [["state_icon", "agent"]]
"#;
        let updated = add_quota_row(original).unwrap();
        assert!(updated.contains("claude = [[\"state_icon\", \"agent\"]]"));
        assert!(updated.contains("codex ="));
        assert!(updated.contains("opencode ="));
        assert!(updated.contains(identity::provider_marker().as_str()));
        let skipped = rewrite_quota_sidebar(
            original,
            &AgentSelection::SUPPORTED,
            SidebarLayout::Packed,
            SidebarRowGap::default(),
            FieldSet::all(),
            BrandColors::On,
        )
        .unwrap()
        .1;
        assert_eq!(skipped, ["claude"]);
        let removed = remove_quota_row(&updated).unwrap();
        assert!(removed.contains("claude = [[\"state_icon\", \"agent\"]]"));
        assert!(!removed.contains("codex ="));
        assert!(!removed.contains("opencode ="));
    }

    #[test]
    fn applying_one_agent_installs_shared_rows_without_per_agent_brand() {
        let updated = add_quota_row_for(
            "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n",
            &[Harness::Grok],
            SidebarLayout::Packed,
        )
        .unwrap();
        assert!(updated.contains("$quota_group"));
        assert!(updated.contains("$quota_icon"));
        assert!(updated.contains(identity::row_marker().as_str()));
        assert!(!updated.contains("rows_by_agent"), "{updated}");
        for agent_key in ["grok =", "claude =", "codex =", "agy =", "opencode ="] {
            assert!(
                !updated.contains(agent_key),
                "{agent_key} was written: {updated}"
            );
        }
    }

    #[test]
    fn removing_one_agent_leaves_shared_sidebar_installed() {
        let full =
            add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n").unwrap();
        assert!(!full.contains("rows_by_agent"), "{full}");
        let removed = remove_quota_row_for(&full, &[Harness::Grok], false).unwrap();
        // Takeover installs no per-agent brand rows, so a partial uninstall
        // has nothing agent-specific to drop — shared quota stays.
        assert!(removed.contains("rows = "));
        assert!(removed.contains("$quota_icon"));
        assert!(removed.contains("row_gap"));
        assert!(removed.contains(identity::refresh_action().as_str()));
        assert!(!removed.contains("rows_by_agent"), "{removed}");
    }

    #[test]
    fn removing_every_agent_one_at_a_time_matches_a_full_uninstall() {
        let original = "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n";
        let full = add_quota_row(original).unwrap();
        let mut piecemeal = full.clone();
        for harness in AgentSelection::SUPPORTED {
            piecemeal = remove_quota_row_for(&piecemeal, &[harness], false).unwrap();
        }
        assert!(!piecemeal.contains("rows_by_agent"));
        // The last agent leaving does not take the shared rows with it; that
        // is what the full uninstall is for.
        let complete = remove_quota_row(&full).unwrap();
        assert!(!complete.contains("rows_by_agent"));
        assert!(!complete.contains(identity::refresh_action().as_str()));
    }

    #[test]
    fn a_partial_uninstall_never_touches_a_user_owned_row() {
        let original = r#"[ui.sidebar.agents]
rows = [["state_icon", "agent"]]

[ui.sidebar.agents.rows_by_agent]
grok = [["state_icon", "agent"]]
"#;
        let applied = add_quota_row(original).unwrap();
        let removed = remove_quota_row_for(&applied, &[Harness::Grok], false).unwrap();
        assert!(removed.contains("grok = [[\"state_icon\", \"agent\"]]"));
    }

    #[test]
    fn preserves_user_owned_opencode_rows() {
        let original = r#"[ui.sidebar.agents]
rows = [["state_icon", "agent"]]

[ui.sidebar.agents.rows_by_agent]
opencode = [["state_icon", "agent"]]
"#;
        let updated = add_quota_row(original).unwrap();
        assert!(updated.contains("opencode = [[\"state_icon\", \"agent\"]]"));
        assert!(!updated.contains(&format!(
            "opencode = [[\"state_icon\", \"agent\"]] # {}",
            identity::provider_marker()
        )));
        let removed = remove_quota_row(&updated).unwrap();
        assert!(removed.contains("opencode = [[\"state_icon\", \"agent\"]]"));
    }

    #[test]
    fn fresh_tree_installs_shared_rows_without_managed_opencode() {
        let updated =
            add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n").unwrap();
        assert!(updated.contains("$quota_group"));
        assert!(updated.contains(identity::row_marker().as_str()));
        assert!(!updated.contains("rows_by_agent"), "{updated}");
        assert!(!updated.contains("opencode ="), "{updated}");
        assert!(
            !updated.contains(identity::provider_marker().as_str()),
            "{updated}"
        );
        let removed = remove_quota_row(&updated).unwrap();
        assert!(!removed.contains("opencode ="));
        assert!(!removed.contains("$quota_"));
    }

    #[test]
    fn empty_sidebar_configuration_round_trips_to_empty() {
        let updated = add_quota_row("").unwrap();
        assert!(updated.contains(&format!("agent_panel_sort = \"spaces\" # {PLUGIN_ID}")));
        assert_eq!(remove_quota_row(&updated).unwrap(), "");
        let stacked =
            add_quota_row_for("", &AgentSelection::SUPPORTED, SidebarLayout::Stacked).unwrap();
        assert_eq!(remove_quota_row(&stacked).unwrap(), "");
        let flushed = add_quota_row_with(
            "",
            &AgentSelection::SUPPORTED,
            SidebarLayout::Stacked,
            SidebarRowGap::FLUSH,
            FieldSet::all(),
            BrandColors::On,
        )
        .unwrap();
        assert!(flushed.contains(&format!("row_gap = 0 # {PLUGIN_ID}")));
        assert_eq!(remove_quota_row(&flushed).unwrap(), "");
    }

    #[test]
    fn install_writes_spaces_panel_sort_unless_the_user_already_chose() {
        let fresh = add_quota_row("").unwrap();
        assert!(fresh.contains(&format!("agent_panel_sort = \"spaces\" # {PLUGIN_ID}")));
        assert_eq!(add_quota_row(&fresh).unwrap(), fresh);

        let user_priority = "[ui]\nagent_panel_sort = \"priority\"\n";
        let kept = add_quota_row(user_priority).unwrap();
        assert!(kept.contains("agent_panel_sort = \"priority\""));
        assert!(!kept.contains("agent_panel_sort = \"spaces\""));

        let removed = remove_quota_row(&fresh).unwrap();
        assert!(!removed.contains("agent_panel_sort"));
    }

    #[test]
    fn plugin_owned_row_gap_stays_packed_so_nested_children_can_flush() {
        let original = "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n";
        let flushed = add_quota_row_with(
            original,
            &AgentSelection::SUPPORTED,
            SidebarLayout::Packed,
            SidebarRowGap::FLUSH,
            FieldSet::all(),
            BrandColors::On,
        )
        .unwrap();
        assert!(flushed.contains(&format!("row_gap = 0 # {PLUGIN_ID}")));
        assert!(!flushed.contains("row_gap = 1"));
        let separated = add_quota_row_with(
            &flushed,
            &AgentSelection::SUPPORTED,
            SidebarLayout::Packed,
            SidebarRowGap::SEPARATED,
            FieldSet::all(),
            BrandColors::On,
        )
        .unwrap();
        assert!(separated.contains(&format!("row_gap = 0 # {PLUGIN_ID}")));
        assert!(!separated.contains("row_gap = 1"));
    }

    #[test]
    fn does_not_write_selection_keys_that_herdr_08_rejects() {
        let updated =
            add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n").unwrap();
        assert!(!updated.contains("selection_bg"));
        assert!(!updated.contains("active_row_bg"));
        assert!(!updated.contains("[theme.custom]"));
    }

    #[test]
    fn clears_plugin_owned_selection_keys_rejected_by_herdr_08() {
        let original = concat!(
            "[theme.custom]\n",
            "selection_bg = \"#393f48\" # herdr-agent-quota\n",
            "active_row_bg = \"#393f48\" # herdr-agent-quota\n\n",
            "[ui.sidebar.agents]\n",
            "rows = [[\"state_icon\", \"agent\"]]\n"
        );
        let applied = add_quota_row(original).unwrap();
        assert!(!applied.contains("selection_bg"));
        assert!(!applied.contains("active_row_bg"));
        assert!(!applied.contains("[theme.custom]"));
    }

    #[test]
    fn preserves_user_owned_selection_background() {
        let original = concat!(
            "[theme.custom]\n",
            "selection_bg = \"#111111\"\n",
            "active_row_bg = \"#222222\"\n\n",
            "[ui.sidebar.agents]\n",
            "rows = [[\"state_icon\", \"agent\"]]\n"
        );
        let applied = add_quota_row(original).unwrap();
        assert!(applied.contains("selection_bg = \"#111111\""));
        assert!(applied.contains("active_row_bg = \"#222222\""));
        let removed = remove_quota_row(&applied).unwrap();
        assert!(removed.contains("selection_bg = \"#111111\""));
        assert!(removed.contains("active_row_bg = \"#222222\""));
    }

    #[test]
    fn uninstall_keeps_unrelated_theme_overrides() {
        let original = concat!(
            "[theme]\n",
            "name = \"terminal\"\n\n",
            "[ui.sidebar.agents]\n",
            "rows = [[\"state_icon\", \"agent\"]]\n"
        );
        let applied = add_quota_row(original).unwrap();
        assert!(applied.contains("name = \"terminal\""));
        assert!(!applied.contains("[theme.custom]"));
        let removed = remove_quota_row(&applied).unwrap();
        assert!(removed.contains("name = \"terminal\""));
        assert!(!removed.contains("[theme.custom]"));
    }

    #[test]
    fn preserves_custom_user_sidebar_rows() {
        let original = r#"[ui.sidebar.agents]
rows = [["state_icon", "pane", "terminal_title_stripped"]]
"#;
        let updated = add_quota_row(original).unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 1);
        let items = rows.iter().next().unwrap().as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert!(items.iter().any(|item| item.as_str() == Some("state_icon")));
        assert!(items.iter().any(|item| item.as_str() == Some("pane")));
        assert!(items
            .iter()
            .any(|item| item.as_str() == Some("terminal_title_stripped")));
        assert!(updated.contains("claude =") || updated.contains("codex ="));
        assert!(updated.contains(identity::refresh_action().as_str()));
        assert!(updated.contains("row_gap"));
    }

    #[test]
    fn preserves_sidebar_rows_from_another_plugin() {
        let original = r#"[ui.sidebar.agents]
rows = [["lantern_status"], ["state_icon", "my_plugin_token"]]
"#;
        let updated = add_quota_row(original).unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(updated.contains("lantern_status"));
        assert!(updated.contains("my_plugin_token"));
        assert!(updated.contains("claude =") || updated.contains("grok ="));
        assert!(updated.contains(identity::refresh_action().as_str()));
    }

    fn custom_shared_rows() -> &'static str {
        "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"pane\", \"terminal_title_stripped\"]]\n"
    }

    fn shared_row_names(toml: &str) -> Vec<String> {
        let document = toml.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 1, "{toml}");
        token_names(rows.get(0).unwrap())
    }

    fn token_names(row: &Value) -> Vec<String> {
        row.as_array()
            .unwrap()
            .iter()
            .filter_map(configured_token_name)
            .map(str::to_string)
            .collect()
    }

    fn provider_token_names(toml: &str, provider: &str) -> Vec<String> {
        let document = toml.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows_by_agent"][provider]
            .as_array()
            .expect(provider);
        rows.iter().flat_map(token_names).collect()
    }

    fn provider_fg_colors(toml: &str, provider: &str) -> Vec<String> {
        let document = toml.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows_by_agent"][provider]
            .as_array()
            .expect(provider);
        rows.iter()
            .filter_map(Value::as_array)
            .flat_map(|row| row.iter())
            .filter_map(|item| {
                item.as_inline_table()
                    .and_then(|table| table.get("fg"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    }

    #[test]
    fn custom_rows_with_brand_on_preserve_all_user_tokens_in_provider_rows() {
        let updated = add_quota_row(custom_shared_rows()).unwrap();
        assert_eq!(
            shared_row_names(&updated),
            ["state_icon", "pane", "terminal_title_stripped"]
        );
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows_value = document["ui"]["sidebar"]["agents"]["rows"]
            .as_value()
            .unwrap();
        assert!(!has_rows_marker(rows_value), "{updated}");
        let claude = provider_token_names(&updated, "claude");
        for token in ["state_icon", "pane", "terminal_title_stripped"] {
            assert!(
                claude.iter().any(|name| name == token),
                "{token} missing: {claude:?}\n{updated}"
            );
        }
        assert!(
            claude.iter().any(|name| name.starts_with("$quota_")),
            "quota missing: {claude:?}\n{updated}"
        );
    }

    #[test]
    fn custom_rows_with_brand_off_still_render_quota_in_provider_rows() {
        let updated = add_quota_row_with(
            custom_shared_rows(),
            &AgentSelection::SUPPORTED,
            SidebarLayout::Packed,
            SidebarRowGap::default(),
            FieldSet::all(),
            BrandColors::Off,
        )
        .unwrap();
        assert_eq!(
            shared_row_names(&updated),
            ["state_icon", "pane", "terminal_title_stripped"]
        );
        let claude = provider_token_names(&updated, "claude");
        for token in ["state_icon", "pane", "terminal_title_stripped"] {
            assert!(
                claude.iter().any(|name| name == token),
                "{token} missing: {claude:?}\n{updated}"
            );
        }
        assert!(
            claude.iter().any(|name| name.starts_with("$quota_")),
            "quota missing: {claude:?}\n{updated}"
        );
        assert!(
            updated.contains("claude ="),
            "provider rows missing:\n{updated}"
        );
        let claude_fgs = provider_fg_colors(&updated, "claude");
        assert!(
            !claude_fgs.iter().any(|fg| fg == "#d97757"),
            "brand hue survived brand-off: {claude_fgs:?}\n{updated}"
        );
    }

    #[test]
    fn custom_rows_reapply_is_idempotent() {
        let once = add_quota_row(custom_shared_rows()).unwrap();
        let twice = add_quota_row(&once).unwrap();
        assert_eq!(once, twice);
        let off = add_quota_row_with(
            custom_shared_rows(),
            &AgentSelection::SUPPORTED,
            SidebarLayout::Packed,
            SidebarRowGap::default(),
            FieldSet::all(),
            BrandColors::Off,
        )
        .unwrap();
        let off_again = add_quota_row_with(
            &off,
            &AgentSelection::SUPPORTED,
            SidebarLayout::Packed,
            SidebarRowGap::default(),
            FieldSet::all(),
            BrandColors::Off,
        )
        .unwrap();
        assert_eq!(off, off_again);
    }

    #[test]
    fn skipped_provider_notice_names_each_preserved_row() {
        assert_eq!(
            skipped_provider_notice(&["claude"]),
            Some(
                "Preserved user-owned rows_by_agent.claude; quota rows were not installed for Claude."
                    .to_string()
            )
        );
        assert_eq!(
            skipped_provider_notice(&["claude", "codex"]),
            Some(
                "Preserved user-owned rows_by_agent.claude, rows_by_agent.codex; quota rows were not installed for Claude, Codex."
                    .to_string()
            )
        );
        assert_eq!(skipped_provider_notice(&[]), None);
    }

    #[test]
    fn idempotent_reapply_does_not_grow_rows() {
        let original = "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n";
        let once = add_quota_row(original).unwrap();
        let document = once.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        let first_count = rows.len();
        let twice = add_quota_row(&once).unwrap();
        let document = twice.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), first_count);
        assert_eq!(once, twice);
    }

    #[test]
    fn uninstall_leaves_unmanaged_user_rows_intact() {
        let original = r#"[ui.sidebar.agents]
rows = [["state_icon", "pane", "terminal_title_stripped"], ["agent", "$quota_icon", "$quota_5h"], ["$quota_week"]]
"#;
        let updated = remove_quota_row(original).unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| {
            row.as_array().is_some_and(|items| {
                items.len() == 3
                    && items.iter().any(|item| item.as_str() == Some("state_icon"))
                    && items.iter().any(|item| item.as_str() == Some("pane"))
                    && items
                        .iter()
                        .any(|item| item.as_str() == Some("terminal_title_stripped"))
            })
        }));
        assert!(rows.iter().any(|row| {
            row.as_array().is_some_and(|items| {
                items.len() == 1 && items.iter().any(|item| item.as_str() == Some("agent"))
            })
        }));
        assert!(!updated.contains("$quota_"));
    }
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn rows(input: &str) -> String {
        input
            .lines()
            .filter(|line| line.contains("token"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn applied(fields: FieldSet, brand: BrandColors) -> String {
        add_quota_row_with(
            "",
            &AgentSelection::SUPPORTED,
            SidebarLayout::Packed,
            SidebarRowGap::default(),
            fields,
            brand,
        )
        .unwrap()
    }

    /// A field the user turned off leaves no token behind, and the row it was
    /// alone on disappears with it.
    #[test]
    fn a_hidden_field_writes_none_of_its_tokens() {
        let without_cache = applied(
            FieldSet::all()
                .toggled(SidebarField::Cache)
                .toggled(SidebarField::Ttl),
            BrandColors::On,
        );
        assert!(!without_cache.contains("$quota_cache"), "{without_cache}");
        assert!(without_cache.contains("$quota_context"), "{without_cache}");
        // The error token is not optional: it is how a broken pane is reported.
        assert!(without_cache.contains("$quota_error"), "{without_cache}");
    }

    /// Hiding the model must not take the row's identity with it.
    #[test]
    fn hiding_the_model_leaves_the_provider_on_the_identity_row() {
        let packed = applied(
            FieldSet::all().toggled(SidebarField::Model),
            BrandColors::On,
        );
        assert!(packed.contains("$quota_provider\""), "{packed}");
        assert!(!packed.contains("$quota_provider_model"), "{packed}");
        assert!(!packed.contains("$quota_model"), "{packed}");
    }

    /// Hiding the provider must not take the row's model with it, and the
    /// degraded token must still be a token the theming pass knows.
    #[test]
    fn hiding_the_provider_leaves_the_model_on_the_identity_row() {
        let packed = applied(
            FieldSet::all().toggled(SidebarField::Provider),
            BrandColors::On,
        );
        assert!(packed.contains("$quota_model\""), "{packed}");
        assert!(!packed.contains("$quota_provider_model"), "{packed}");
        assert!(!packed.contains("$quota_provider\""), "{packed}");
    }

    /// The user's own selection (`fields = 5h`) must leave no identity row at
    /// all, and applying it twice must land on the same config.
    #[test]
    fn a_single_field_selection_writes_no_identity_row() {
        let fields = FieldSet::parse("5h").unwrap();
        let once = applied(fields, BrandColors::On);
        assert!(!once.contains("$quota_provider"), "{once}");
        assert!(once.contains("$quota_5h"), "{once}");
        let twice = add_quota_row_with(
            &once,
            &AgentSelection::SUPPORTED,
            SidebarLayout::Packed,
            SidebarRowGap::default(),
            fields,
            BrandColors::On,
        )
        .unwrap();
        assert_eq!(once, twice);
    }

    /// `fields = all` saved before the provider was a field still means every
    /// field, so the identity row keeps the provider after an upgrade.
    #[test]
    fn a_pre_provider_full_selection_still_renders_the_provider() {
        let legacy = FieldSet::parse("topic,model,cache,ttl,context,5h,7d").unwrap();
        assert_eq!(
            applied(legacy, BrandColors::On),
            applied(FieldSet::all(), BrandColors::On)
        );
    }

    /// Hiding only the provider is a selection the settings pane makes, so it
    /// has to survive its stored form: reading it back as the pre-provider full
    /// list would turn the provider on again.
    #[test]
    fn hiding_only_the_provider_survives_its_stored_form() {
        let providerless = FieldSet::all().toggled(SidebarField::Provider);
        let stored = FieldSet::parse(&providerless.as_list()).unwrap();
        let rendered = applied(stored, BrandColors::On);
        assert!(rendered.contains("$quota_model\""), "{rendered}");
        assert!(!rendered.contains("$quota_provider\""), "{rendered}");
    }

    /// In `stacked` the provider and the model sit on rows of their own, so a
    /// hidden provider has to take its row and leave the model's alone.
    #[test]
    fn a_stacked_layout_drops_the_provider_row_with_the_field() {
        let stacked = add_quota_row_with(
            "",
            &AgentSelection::SUPPORTED,
            SidebarLayout::Stacked,
            SidebarRowGap::default(),
            FieldSet::all().toggled(SidebarField::Provider),
            BrandColors::On,
        )
        .unwrap();
        assert!(!stacked.contains("$quota_provider\""), "{stacked}");
        assert!(stacked.contains("$quota_model\""), "{stacked}");
    }

    #[test]
    fn hiding_every_field_keeps_group_icon_and_error_token() {
        let bare = applied(FieldSet::parse("none").unwrap(), BrandColors::On);
        // Group furniture and the vendor mark are not optional: without them
        // Space aggregation and logos disappear. The error token stays too —
        // it is how a broken pane is reported.
        assert!(bare.contains("$quota_group"), "{bare}");
        assert!(bare.contains("$quota_icon"), "{bare}");
        assert!(
            !bare.contains("$quota_icon_working"),
            "working colour is a rule on $quota_icon:\n{bare}"
        );
        assert!(
            !bare.contains("$quota_icon_done"),
            "done colour is a rule on $quota_icon:\n{bare}"
        );
        assert!(!bare.contains("state_icon"), "{bare}");
        assert!(!bare.contains("$quota_pad"), "{bare}");
        assert!(bare.contains("$quota_error"), "{bare}");
        assert!(!bare.contains("$quota_provider"), "{bare}");
        for token in ["$quota_topic", "$quota_context", "$quota_5h", "$quota_week"] {
            assert!(!bare.contains(token), "{token} survived:\n{}", rows(&bare));
        }
    }

    /// Without brand hues, plugin-managed shared rows already carry quota, so
    /// per-agent copies would be identical and are omitted — brand-on and
    /// brand-off both write shared rows only.
    #[test]
    fn brand_colours_off_writes_no_per_agent_rows() {
        let plain = applied(FieldSet::all(), BrandColors::Off);
        assert!(!plain.contains("rows_by_agent"), "{plain}");
        // Severity colours are information, not decoration: they stay.
        assert!(plain.contains(QUOTA_DANGER_COLOR), "{plain}");
        let branded = applied(FieldSet::all(), BrandColors::On);
        assert!(!branded.contains("rows_by_agent"), "{branded}");
        assert_eq!(plain, branded);
    }

    #[test]
    fn idle_identity_uses_ink_white_on_shared_rows() {
        let installed = applied(FieldSet::all(), BrandColors::On);
        assert!(!installed.contains("rows_by_agent"), "{installed}");
        let document = installed.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        for token in ["$quota_icon", "$quota_provider_model"] {
            let fg = rows
                .iter()
                .filter_map(Value::as_array)
                .flat_map(Array::iter)
                .find(|item| configured_token_name(item) == Some(token))
                .and_then(Value::as_inline_table)
                .and_then(|table| table.get("fg"))
                .and_then(Value::as_str);
            assert_eq!(fg, Some(IDLE_ICON_COLOR), "{token}");
        }
    }

    /// Switching brand colours off and back on must land exactly where it
    /// started, or a repair would keep rewriting the config.
    #[test]
    fn brand_colours_round_trip_through_off_and_back() {
        let branded = applied(FieldSet::all(), BrandColors::On);
        let plain = add_quota_row_with(
            &branded,
            &AgentSelection::SUPPORTED,
            SidebarLayout::Packed,
            SidebarRowGap::default(),
            FieldSet::all(),
            BrandColors::Off,
        )
        .unwrap();
        assert!(!plain.contains("rows_by_agent"), "{plain}");
        let rebranded = add_quota_row_with(
            &plain,
            &AgentSelection::SUPPORTED,
            SidebarLayout::Packed,
            SidebarRowGap::default(),
            FieldSet::all(),
            BrandColors::On,
        )
        .unwrap();
        assert_eq!(rebranded, branded);
    }

    /// A second apply with the same settings must be a no-op, including after
    /// fields were hidden — otherwise every repair rewrites Herdr's config.
    #[test]
    fn applying_a_field_selection_twice_changes_nothing_the_second_time() {
        let fields = FieldSet::all()
            .toggled(SidebarField::Topic)
            .toggled(SidebarField::Ttl);
        let once = applied(fields, BrandColors::On);
        let twice = add_quota_row_with(
            &once,
            &AgentSelection::SUPPORTED,
            SidebarLayout::Packed,
            SidebarRowGap::default(),
            fields,
            BrandColors::On,
        )
        .unwrap();
        assert_eq!(once, twice);
    }

    /// The `gauges` body is stacked's, so a hidden field has to take its whole
    /// row with it there too — and the identity row still has to survive
    /// hiding the model.
    #[test]
    fn a_hidden_field_leaves_no_row_behind_in_gauges() {
        let gauges = |fields| {
            add_quota_row_with(
                "",
                &AgentSelection::SUPPORTED,
                SidebarLayout::Gauges,
                SidebarRowGap::default(),
                fields,
                BrandColors::On,
            )
            .unwrap()
        };
        let windowless = gauges(
            FieldSet::all()
                .toggled(SidebarField::FiveHour)
                .toggled(SidebarField::Week),
        );
        assert!(!windowless.contains("$quota_5h"), "{windowless}");
        assert!(!windowless.contains("$quota_week"), "{windowless}");
        assert!(!windowless.contains("$quota_share_5h"), "{windowless}");
        assert!(!windowless.contains("$quota_share_week"), "{windowless}");
        assert!(windowless.contains("$quota_context"), "{windowless}");
        assert!(windowless.contains("$quota_provider_model"), "{windowless}");

        let modelless = gauges(FieldSet::all().toggled(SidebarField::Model));
        assert!(modelless.contains("$quota_provider\""), "{modelless}");
        assert!(!modelless.contains("$quota_provider_model"), "{modelless}");
        assert!(!modelless.contains("$quota_model"), "{modelless}");

        let providerless = gauges(FieldSet::all().toggled(SidebarField::Provider));
        assert!(providerless.contains("$quota_model\""), "{providerless}");
        assert!(!providerless.contains("$quota_provider"), "{providerless}");
    }

    /// Uninstall has to recognise rows written with a non-default selection,
    /// or it falls back to token-stripping and leaves the file behind.
    #[test]
    fn uninstall_recognises_rows_written_with_hidden_fields() {
        let fields = FieldSet::all().toggled(SidebarField::Context);
        let installed = applied(fields, BrandColors::Off);
        assert!(
            matches_installed_quota_rows("", &installed, fields, BrandColors::Off).unwrap(),
            "{installed}"
        );
    }
}

#[cfg(test)]
mod sidebar_width_tests {
    use super::*;

    const SHELL_FILE: &str = "local-82d9e482d8820ee2.json";

    #[test]
    fn shell_filename_matches_herdrs_persisted_socket_hash() {
        assert_eq!(
            client_shell_file_name(Path::new("/test/herdr.sock")),
            SHELL_FILE
        );
    }

    #[test]
    fn a_config_without_a_sidebar_width_reads_as_herdrs_documented_default() {
        assert_eq!(width_for_config(""), DEFAULT_SIDEBAR_WIDTH);
        assert_eq!(width_for_config("[ui]\ntheme = \"dark\"\n"), 26);
    }

    #[test]
    fn a_configured_sidebar_width_is_read_as_written() {
        assert_eq!(width_for_config("[ui]\nsidebar_width = 30\n"), 30);
        assert_eq!(width_for_config("ui = { sidebar_width = 22 }\n"), 22);
    }

    /// A user's typo must cost them the default bar, never the refresh.
    #[test]
    fn an_unusable_sidebar_width_degrades_to_the_documented_default() {
        for config in [
            "[ui]\nsidebar_width = \"wide\"\n",
            "[ui]\nsidebar_width = 26.5\n",
            "[ui]\nsidebar_width = -8\n",
            "[ui]\nsidebar_width = 0\n",
            "[ui]\nsidebar_width = 99999999\n",
            "[ui]\nsidebar_width = true\n",
            "[ui\nsidebar_width = 30\n",
            "sidebar_width = 30\n",
        ] {
            assert_eq!(width_for_config(config), DEFAULT_SIDEBAR_WIDTH, "{config}");
        }
    }

    #[test]
    fn a_sidebar_width_outside_the_configured_bounds_is_clamped_into_them() {
        assert_eq!(width_for_config("[ui]\nsidebar_width = 48\n"), 36);
        assert_eq!(width_for_config("[ui]\nsidebar_width = 12\n"), 18);
        assert_eq!(
            width_for_config("[ui]\nsidebar_width = 48\nsidebar_max_width = 40\n"),
            40
        );
        assert_eq!(
            width_for_config("[ui]\nsidebar_width = 12\nsidebar_min_width = 10\n"),
            12
        );
    }

    /// The width sources in precedence order: what Herdr rendered, then what
    /// the config asked for, then the documented default.
    #[test]
    fn client_shell_state_outranks_the_config_which_outranks_the_default() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.toml");
        let state = directory.path().join("state");
        let shell = state.join("herdr/client-shell");
        fs::create_dir_all(&shell).unwrap();
        fs::write(&config, "[ui]\nsidebar_width = 30\n").unwrap();

        with_width_sources(&config, &state, || assert_eq!(sidebar_width(), 30));

        fs::write(shell.join(SHELL_FILE), "{\"sidebar_width\": 22}").unwrap();
        with_width_sources(&config, &state, || assert_eq!(sidebar_width(), 22));

        fs::remove_file(&config).unwrap();
        with_width_sources(&config, &state, || assert_eq!(sidebar_width(), 22));

        fs::remove_file(shell.join(SHELL_FILE)).unwrap();
        with_width_sources(&config, &state, || {
            assert_eq!(sidebar_width(), DEFAULT_SIDEBAR_WIDTH)
        });
    }

    /// Whatever source the width came from, it lands inside the bounds the
    /// config sets.
    #[test]
    fn a_client_shell_width_outside_the_configured_bounds_is_clamped_into_them() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.toml");
        let state = directory.path().join("state");
        let shell = state.join("herdr/client-shell");
        fs::create_dir_all(&shell).unwrap();
        fs::write(&config, "[ui]\nsidebar_max_width = 32\n").unwrap();
        fs::write(shell.join(SHELL_FILE), "{\"sidebar_width\": 48}").unwrap();
        with_width_sources(&config, &state, || assert_eq!(sidebar_width(), 32));
    }

    /// A state file the plugin cannot make sense of costs the meter nothing:
    /// the next source answers instead.
    #[test]
    fn an_unusable_client_shell_state_falls_through_to_the_config() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.toml");
        let state = directory.path().join("state");
        let shell = state.join("herdr/client-shell");
        fs::create_dir_all(&shell).unwrap();
        fs::write(&config, "[ui]\nsidebar_width = 30\n").unwrap();
        for contents in [
            "",
            "not json at all",
            "{\"agent_panel_sort\": \"priority\"}",
            "{\"sidebar_width\": \"35\"}",
            "{\"sidebar_width\": 35.5}",
            "{\"sidebar_width\": 0}",
            "{\"sidebar_width\": -8}",
            "{\"sidebar_width\": 99999999}",
            "[35]",
        ] {
            fs::write(shell.join(SHELL_FILE), contents).unwrap();
            with_width_sources(&config, &state, || {
                assert_eq!(sidebar_width(), 30, "{contents}")
            });
        }
    }

    /// Another endpoint can write its preferences later without changing the
    /// connected endpoint's meter size.
    #[test]
    fn another_endpoints_newer_state_file_cannot_change_the_width() {
        let directory = tempfile::tempdir().unwrap();
        let shell = directory.path().join("herdr/client-shell");
        fs::create_dir_all(&shell).unwrap();
        fs::write(shell.join(SHELL_FILE), "{\"sidebar_width\": 22}").unwrap();
        fs::write(shell.join("notes.txt"), "{\"sidebar_width\": 30}").unwrap();
        fs::write(shell.join("local-new.json"), "{\"sidebar_width\": 35}").unwrap();
        let absent = directory.path().join("absent.toml");
        with_width_sources(&absent, directory.path(), || {
            assert_eq!(sidebar_width(), 22)
        });
        fs::remove_file(shell.join(SHELL_FILE)).unwrap();
        with_width_sources(&absent, directory.path(), || {
            assert_eq!(sidebar_width(), DEFAULT_SIDEBAR_WIDTH)
        });
    }

    #[test]
    fn a_direct_invocation_without_socket_identity_uses_config() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.toml");
        let shell = directory.path().join("herdr/client-shell");
        fs::create_dir_all(&shell).unwrap();
        fs::write(shell.join(SHELL_FILE), "{\"sidebar_width\": 35}").unwrap();
        fs::write(&config, "[ui]\nsidebar_width = 22\n").unwrap();
        crate::prefs::testing::with_env(
            &[
                ("HERDR_CONFIG_FILE", Some(config.as_os_str())),
                ("XDG_STATE_HOME", Some(directory.path().as_os_str())),
                ("HERDR_SOCKET_PATH", None),
            ],
            || assert_eq!(sidebar_width(), 22),
        );
    }

    #[test]
    fn an_absent_config_and_state_directory_read_as_the_documented_default() {
        let directory = tempfile::tempdir().unwrap();
        with_width_sources(
            &directory.path().join("absent.toml"),
            &directory.path().join("absent-state"),
            || assert_eq!(sidebar_width(), DEFAULT_SIDEBAR_WIDTH),
        );
    }

    #[test]
    fn reading_the_sidebar_width_leaves_the_config_file_byte_identical() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let original = "# my herdr config\n[ui]\nsidebar_width   =    30\ntheme='dark'\n";
        fs::write(&path, original).unwrap();
        with_width_sources(&path, &directory.path().join("absent-state"), || {
            assert_eq!(sidebar_width(), 30)
        });
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    /// The width a config alone resolves to, with no client-shell state for
    /// it to lose to.
    fn width_for_config(config: &str) -> usize {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, config).unwrap();
        let mut width = 0;
        with_width_sources(&path, &directory.path().join("absent-state"), || {
            width = sidebar_width()
        });
        width
    }

    /// Both width sources are process-global environment lookups, so they
    /// move under the one env lock.
    fn with_width_sources(config: &Path, state: &Path, body: impl FnOnce()) {
        crate::prefs::testing::with_env(
            &[
                ("HERDR_CONFIG_FILE", Some(config.as_os_str())),
                ("XDG_STATE_HOME", Some(state.as_os_str())),
                (
                    "HERDR_SOCKET_PATH",
                    Some(std::ffi::OsStr::new("/test/herdr.sock")),
                ),
            ],
            body,
        );
    }
}
