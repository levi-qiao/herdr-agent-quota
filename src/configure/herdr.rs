use crate::cli::{
    AgentSelection, BrandColors, FieldSet, SidebarField, SidebarLayout, SidebarRowGap,
};
use crate::model::Harness;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

const QUOTA_ROW_MARKERS: [&str; 40] = [
    "$quota_badge",
    "$quota_state",
    "$quota_icon",
    "$quota_provider",
    "$quota_model",
    "$quota_provider_model",
    "$quota_status",
    "$quota_summary",
    "$quota_session",
    "$quota_context",
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
];
const ROW_GAP_MARKER: &str = "herdr-agent-quota";

const PROVIDER_STYLE_MARKER: &str = "herdr-agent-quota-provider";
const REFRESH_KEY: &str = "prefix+shift+r";
const REFRESH_ACTION: &str = "herdr-agent-quota.refresh";
const CONFIG_PRESENCE_FILE: &str = "herdr-config.original.present";
// Brand answers "who"; status answers "how much is left". Selected state
// may change background only — never the provider hue. Herdr 0.8.0 rejects
// selection_bg / active_row_bg (0.8.2 added them); intended selected fill
// is #42474f when those keys exist.
const TEXT_COLOR: &str = "#eceef2";
const BODY_COLOR: &str = "#c8cdd6";
const MUTED_COLOR: &str = "#969eae";
const QUOTA_SAFE_COLOR: &str = "#82d978";
const QUOTA_WARNING_COLOR: &str = "#e4b957";
const QUOTA_DANGER_COLOR: &str = "#f16f7e";
const PROVIDER_STYLES: [(Harness, &str, Option<&str>, Option<&str>); 6] = [
    (Harness::Claude, "claude", Some("#e88461"), Some("#f0a080")),
    (Harness::Codex, "codex", Some("#c4d7f5"), Some("#aab9d0")),
    (Harness::Grok, "grok", Some("#d5d5d8"), Some("#acb0b7")),
    (Harness::Agy, "agy", Some("#8ab4f8"), Some("#a7c7fa")),
    (Harness::OpenCode, "opencode", Some("#bba3e8"), None),
    (Harness::Pi, "pi", Some("#d4a0c8"), None),
];
const THEME_SELECTION_KEYS: [&str; 2] = ["selection_bg", "active_row_bg"];

/// Sidebar rows for the selected agents only, so `--agent grok` never writes
/// or removes another agent's row.
fn selected_styles(
    agents: &[Harness],
) -> impl Iterator<Item = (&'static str, Option<&'static str>, Option<&'static str>)> + '_ {
    PROVIDER_STYLES
        .into_iter()
        .filter(move |(harness, _, _, _)| agents.contains(harness))
        .map(|(_, provider, brand, dim)| (provider, brand, dim))
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
    let updated = add_quota_row_with(&original, agents, layout, row_gap, fields, brand)?;
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
    let updated = add_quota_row_with(&original, agents, layout, row_gap, fields, brand)?;
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
/// gap stay brute-forced — there are only four combinations, and neither is
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
        for layout in [SidebarLayout::Packed, SidebarLayout::Stacked] {
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
    let mut document = if input.trim().is_empty() {
        DocumentMut::new()
    } else {
        input
            .parse::<DocumentMut>()
            .context("parse Herdr TOML config")?
    };
    add_refresh_keybinding(&mut document)?;
    let table = ensure_table(&mut document, &["ui", "sidebar", "agents"])?;
    let managed_row_gap = table
        .get("row_gap")
        .and_then(Item::as_value)
        .and_then(|value| value.decor().suffix())
        .and_then(|suffix| suffix.as_str())
        .is_some_and(|suffix| suffix.contains(ROW_GAP_MARKER));
    if !table.contains_key("row_gap") || managed_row_gap {
        let mut gap = Value::from(row_gap.as_i64());
        gap.decor_mut().set_suffix(format!(" # {ROW_GAP_MARKER}"));
        table.insert("row_gap", Item::Value(gap));
    }
    let rows = table["rows"].or_insert(Item::Value(Value::Array(Array::new())));
    let rows = rows
        .as_array_mut()
        .context("Herdr ui.sidebar.agents.rows must be an array")?;
    let mut updated_rows = Array::new();
    for row in rows.iter() {
        let cleaned = normalize_official_row(strip_quota_tokens(row, true));
        if !cleaned.is_empty() {
            updated_rows.push(Value::Array(cleaned));
        }
    }

    // If an older version replaced every row with quota-only rows, restore
    // Herdr's official state/tab row before adding provider, usage, and topic.
    if updated_rows.is_empty() {
        updated_rows.push(Value::Array(default_state_row()));
    }
    append_quota_rows(&mut updated_rows, layout);
    retain_selected_fields(&mut updated_rows, fields);
    *rows = updated_rows;
    let rows = rows.clone();
    if brand.is_on() {
        add_provider_rows(table, &rows, agents)?;
    } else {
        // Without brand hues a per-agent row set would be identical to the
        // shared one, so the plugin removes its own entries rather than
        // writing copies Herdr would have to keep in sync.
        remove_managed_provider_rows(table, agents);
    }
    remove_managed_selection_theme(&mut document);
    Ok(document.to_string())
}

/// Full-installation form, used by callers that remove every agent.
pub fn remove_quota_row(input: &str) -> Result<String> {
    remove_quota_row_for(input, &AgentSelection::SUPPORTED, true)
}

/// `full` means the whole plugin is being removed, so the shared base rows and
/// the refresh keybinding go too. A narrower selection only drops that agent's
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
        remove_refresh_keybinding(&mut document);
    }
    let Some(table) = document
        .get_mut("ui")
        .and_then(Item::as_table_mut)
        .and_then(|ui| ui.get_mut("sidebar"))
        .and_then(Item::as_table_mut)
        .and_then(|sidebar| sidebar.get_mut("agents"))
        .and_then(Item::as_table_mut)
    else {
        return Ok(document.to_string());
    };
    remove_managed_provider_rows(table, agents);
    // The base rows, row gap and keybinding are shared by every agent. Only a
    // full removal may take them; otherwise the agents left installed would
    // lose their sidebar out from under them.
    if !full {
        return Ok(document.to_string());
    }
    if let Some(rows) = table.get_mut("rows").and_then(Item::as_array_mut) {
        let mut retained = Array::new();
        for row in rows.iter() {
            let cleaned = strip_quota_tokens(row, false);
            if cleaned.len() == 1
                && matches!(
                    cleaned.iter().next().and_then(Value::as_str),
                    Some("terminal_title_stripped") | Some("$quota_topic")
                )
            {
                continue;
            }
            if !cleaned.is_empty() {
                retained.push(Value::Array(cleaned));
            }
        }
        table["rows"] = Item::Value(Value::Array(retained));
    }
    let managed_row_gap = table
        .get("row_gap")
        .and_then(Item::as_value)
        .and_then(|value| value.decor().suffix())
        .and_then(|suffix| suffix.as_str())
        .is_some_and(|suffix| suffix.contains(ROW_GAP_MARKER));
    if managed_row_gap {
        table.remove("row_gap");
    }
    remove_managed_selection_theme(&mut document);
    Ok(document.to_string())
}

fn add_refresh_keybinding(document: &mut DocumentMut) -> Result<()> {
    let keys = ensure_table(document, &["keys"])?;
    let commands = keys
        .entry("command")
        .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .context("Herdr keys.command must be an array of tables")?;
    if commands
        .iter()
        .any(|command| command.get("command").and_then(Item::as_str) == Some(REFRESH_ACTION))
        || commands
            .iter()
            .any(|command| command.get("key").and_then(Item::as_str) == Some(REFRESH_KEY))
    {
        return Ok(());
    }

    let mut command = Table::new();
    command.insert("key", Item::Value(Value::from(REFRESH_KEY)));
    command.insert("type", Item::Value(Value::from("plugin_action")));
    command.insert("command", Item::Value(Value::from(REFRESH_ACTION)));
    command.insert(
        "description",
        Item::Value(Value::from("refresh all agent quotas")),
    );
    commands.push(command);
    Ok(())
}

fn remove_refresh_keybinding(document: &mut DocumentMut) {
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
        if command.get("command").and_then(Item::as_str) != Some(REFRESH_ACTION) {
            retained.push(command.clone());
        }
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

fn strip_quota_tokens(row: &Value, keep_provider_model: bool) -> Array {
    let mut cleaned = Array::new();
    if let Some(items) = row.as_array() {
        for item in items {
            let is_quota_token =
                configured_token_name(item).is_some_and(|value| QUOTA_ROW_MARKERS.contains(&value));
            if !is_quota_token
                || (keep_provider_model
                    && configured_token_name(item) == Some("$quota_provider_model"))
            {
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

fn add_provider_rows(table: &mut Table, rows: &Array, agents: &[Harness]) -> Result<()> {
    let rows_by_agent = table
        .entry("rows_by_agent")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .context("Herdr ui.sidebar.agents.rows_by_agent must be a table")?;

    for (provider, brand, dim) in selected_styles(agents) {
        let is_managed = rows_by_agent
            .get(provider)
            .and_then(Item::as_value)
            .is_some_and(has_provider_style_marker);
        if rows_by_agent.contains_key(provider) && !is_managed {
            continue;
        }
        let mut value = Value::Array(provider_rows(rows, brand, dim));
        value
            .decor_mut()
            .set_suffix(format!(" # {PROVIDER_STYLE_MARKER}"));
        rows_by_agent.insert(provider, Item::Value(value));
    }
    Ok(())
}

fn provider_rows(rows: &Array, brand: Option<&str>, dim: Option<&str>) -> Array {
    // Brand on provider, dim on model. 5h vs folded 7d is a publish-time
    // choice from the 5h token, not a per-agent color choice.
    let mut themed = Array::new();
    for row in rows.iter() {
        let Some(items) = row.as_array() else {
            continue;
        };
        let mut themed_row = Array::new();
        append_themed_provider_row(&mut themed_row, items, brand, dim);
        themed.push(Value::Array(themed_row));
    }
    themed
}

fn append_themed_provider_row(
    row: &mut Array,
    items: &Array,
    brand: Option<&str>,
    dim: Option<&str>,
) {
    let model_color = dim.or(brand);
    for item in items {
        match configured_token_name(item) {
            Some(token @ "$quota_provider_model") | Some(token @ "$quota_provider") => {
                row.push(styled_token(token, brand, Some(true), Some(false)));
            }
            Some(token @ "$quota_model") => {
                row.push(styled_token(token, model_color, Some(false), Some(false)));
            }
            _ => row.push(item.clone()),
        }
    }
}

fn remove_managed_provider_rows(table: &mut Table, agents: &[Harness]) {
    let Some(rows_by_agent) = table.get_mut("rows_by_agent").and_then(Item::as_table_mut) else {
        return;
    };
    for (provider, _, _) in selected_styles(agents) {
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
        .is_some_and(|suffix| suffix.contains(PROVIDER_STYLE_MARKER))
}

fn default_state_row() -> Array {
    let mut row = Array::new();
    row.push("state_icon");
    row.push(styled_tab());
    row
}

fn is_tab_token(value: &Value) -> bool {
    value.as_str() == Some("tab") || configured_token_name(value) == Some("tab")
}

fn styled_tab() -> Value {
    styled_token("tab", Some(TEXT_COLOR), Some(true), Some(false))
}

fn normalize_official_row(row: Array) -> Array {
    let has_state_icon = row.iter().any(|item| item.as_str() == Some("state_icon"));
    if !has_state_icon
        || row.iter().any(|item| {
            item.as_str() == Some("agent")
                || configured_token_name(item) == Some("$quota_provider_model")
        })
    {
        return row;
    }
    let mut normalized = Array::new();
    let mut has_tab = false;
    for item in row {
        if is_tab_token(&item) {
            if !has_tab {
                has_tab = true;
                normalized.push(styled_tab());
            }
            continue;
        }
        match item.as_str() {
            Some("workspace") | Some("pane") => {
                if !has_tab {
                    normalized.push(styled_tab());
                    has_tab = true;
                }
            }
            Some("terminal_title_stripped") => {}
            _ => normalized.push(item),
        }
    }
    if !has_tab {
        let insert_at = normalized
            .iter()
            .position(|item| item.as_str() == Some("terminal_title_stripped"))
            .unwrap_or(normalized.len());
        normalized.insert(insert_at, styled_tab());
    }
    normalized
}

fn append_quota_rows(rows: &mut Array, layout: SidebarLayout) {
    // Context can carry the weekly token when 5h is empty. Limits stay on the
    // next row so a present 5h window never shares a line with context. Herdr
    // drops empty tokens and empty rows. Stacked keeps that publish rule and
    // only changes which tokens share a visual line.
    for row in rows.iter_mut() {
        let Some(items) = row.as_array_mut() else {
            continue;
        };
        let has_state_icon = items.iter().any(|item| item.as_str() == Some("state_icon"));
        let mut cleaned = Array::new();
        for item in items.iter() {
            let token_name = configured_token_name(item);
            if token_name.is_some_and(|token| {
                QUOTA_ROW_MARKERS.contains(&token)
                    && !(layout == SidebarLayout::Packed
                        && has_state_icon
                        && token == "$quota_provider_model")
            }) {
                continue;
            }
            match item.as_str() {
                Some("terminal_title_stripped") | Some("$quota_topic") => {}
                Some("agent") if !has_state_icon => {}
                _ => cleaned.push(item.clone()),
            }
        }
        *items = cleaned;
    }

    let mut compacted_rows = Array::new();
    for row in rows.iter() {
        if row.as_array().is_some_and(|items| !items.is_empty()) {
            compacted_rows.push(row.clone());
        }
    }
    *rows = compacted_rows;

    let official_index = rows.iter().position(|row| {
        row.as_array()
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("state_icon")))
    });

    if let Some(index) = official_index {
        if let Some(row) = rows.get_mut(index).and_then(Value::as_array_mut) {
            *row = match layout {
                SidebarLayout::Packed => packed_identity_row(row),
                SidebarLayout::Stacked => stacked_identity_row(row),
            };
        }
    }

    if layout == SidebarLayout::Stacked {
        let insert_at = official_index.map(|index| index + 1).unwrap_or(0);
        rows.insert(
            insert_at,
            Value::Array(styled_row("$quota_provider", None, Some(true), Some(false))),
        );
        rows.insert(
            insert_at + 1,
            Value::Array(styled_row("$quota_model", None, Some(false), Some(false))),
        );
    }

    rows.push(Value::Array(styled_row(
        "$quota_topic",
        Some(BODY_COLOR),
        Some(false),
        Some(false),
    )));

    match layout {
        SidebarLayout::Packed => append_packed_quota_rows(rows),
        SidebarLayout::Stacked => append_stacked_quota_rows(rows),
    }
}

fn packed_identity_row(row: &Array) -> Array {
    let mut compacted = Array::new();
    let mut has_provider_model = false;
    for item in row.iter() {
        if item.as_str() == Some("agent") {
            if !has_provider_model {
                compacted.push(styled_token(
                    "$quota_provider_model",
                    None,
                    Some(true),
                    Some(false),
                ));
                has_provider_model = true;
            }
        } else if configured_token_name(item) == Some("$quota_provider_model") {
            if !has_provider_model {
                compacted.push(item.clone());
                has_provider_model = true;
            }
        } else if is_tab_token(item) {
            compacted.push(styled_tab());
        } else {
            compacted.push(item.clone());
        }
    }
    if !has_provider_model {
        compacted.push(styled_token(
            "$quota_provider_model",
            None,
            Some(true),
            Some(false),
        ));
    }
    compacted
}

fn stacked_identity_row(row: &Array) -> Array {
    let mut compacted = Array::new();
    for item in row.iter() {
        if item.as_str() == Some("agent")
            || matches!(
                configured_token_name(item),
                Some("$quota_provider_model" | "$quota_provider" | "$quota_model")
            )
        {
            continue;
        }
        compacted.push(item.clone());
    }
    // Dropping `agent` would otherwise skip normalize_official_row's tab
    // restore, so a first stacked apply from `["state_icon", "agent"]`
    // would not match the second.
    normalize_official_row(compacted)
}

fn append_packed_quota_rows(rows: &mut Array) {
    append_cache_row(rows);

    let mut context_row = styled_row(
        "$quota_context",
        Some(MUTED_COLOR),
        Some(false),
        Some(false),
    );
    append_window_style_tokens(&mut context_row, "quota_week_inline");
    rows.push(Value::Array(context_row));

    append_window_row(rows);
}

fn append_stacked_quota_rows(rows: &mut Array) {
    rows.push(Value::Array(styled_row(
        "$quota_cache",
        Some(MUTED_COLOR),
        Some(false),
        Some(false),
    )));
    rows.push(Value::Array(styled_row(
        "$quota_cache_ttl",
        Some(MUTED_COLOR),
        Some(false),
        Some(false),
    )));
    rows.push(Value::Array(styled_row(
        "$quota_cache_state",
        Some(QUOTA_WARNING_COLOR),
        Some(false),
        Some(false),
    )));
    rows.push(Value::Array(styled_row(
        "$quota_error",
        Some(QUOTA_WARNING_COLOR),
        Some(false),
        Some(false),
    )));
    rows.push(Value::Array(styled_row(
        "$quota_context",
        Some(MUTED_COLOR),
        Some(false),
        Some(false),
    )));
    let mut five_hour = Array::new();
    append_window_style_tokens(&mut five_hour, "quota_5h");
    rows.push(Value::Array(five_hour));
    // Both week style families live on this row so the existing publish
    // choice (inline when 5h is empty, limits when 5h is present) still
    // renders exactly one 7d line.
    let mut week = Array::new();
    append_window_style_tokens(&mut week, "quota_week_inline");
    append_window_style_tokens(&mut week, "quota_week");
    rows.push(Value::Array(week));
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
            match configured_token_name(item).and_then(field_for_token) {
                Some(field) if !fields.contains(field) => {
                    // `$quota_provider_model` carries the identity as well as
                    // the model, so hiding the model degrades it to the
                    // provider instead of removing the row's only name.
                    if configured_token_name(item) == Some("$quota_provider_model") {
                        retained.push(styled_token(
                            "$quota_provider",
                            None,
                            Some(true),
                            Some(false),
                        ));
                    }
                }
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
/// optional (the provider identity and the error channel).
fn field_for_token(token: &str) -> Option<SidebarField> {
    match token {
        "$quota_topic" => Some(SidebarField::Topic),
        "$quota_model" | "$quota_provider_model" => Some(SidebarField::Model),
        "$quota_cache" | "$quota_cache_state" => Some(SidebarField::Cache),
        "$quota_cache_ttl" => Some(SidebarField::Ttl),
        "$quota_context" => Some(SidebarField::Context),
        _ if token.starts_with("$quota_5h") => Some(SidebarField::FiveHour),
        _ if token.starts_with("$quota_week") => Some(SidebarField::Week),
        _ => None,
    }
}

fn append_cache_row(rows: &mut Array) {
    rows.push(Value::Array(Array::from_iter([
        styled_token("$quota_cache", Some(MUTED_COLOR), Some(false), Some(false)),
        styled_token(
            "$quota_cache_ttl",
            Some(MUTED_COLOR),
            Some(false),
            Some(false),
        ),
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

fn append_window_style_tokens(row: &mut Array, base: &str) {
    // One compact token per window (`5h 0% 1h18m`). Herdr joins sibling
    // tokens with ` · `, so splitting label/percent/eta cannot stay compact.
    // Exactly the bands `Severity::for_window` can produce. There is no
    // "caution" row: that variant was unreachable, so the token could never
    // be filled and only ever consumed a slot.
    for (suffix, color) in [
        ("normal", QUOTA_SAFE_COLOR),
        ("warning", QUOTA_WARNING_COLOR),
        ("danger", QUOTA_DANGER_COLOR),
        ("unknown", MUTED_COLOR),
    ] {
        row.push(styled_token(
            &format!("${base}_{suffix}"),
            Some(color),
            Some(false),
            Some(false),
        ));
    }
}

fn append_window_row(rows: &mut Array) {
    let mut row = Array::new();
    for base in ["quota_5h", "quota_week"] {
        append_window_style_tokens(&mut row, base);
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
            .is_some_and(|suffix| suffix.contains(ROW_GAP_MARKER));
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

fn print_diff_hint(layout: SidebarLayout, fields: FieldSet, brand: BrandColors) {
    println!("  keep Herdr's official state icon and plane tab");
    match layout {
        SidebarLayout::Packed => {
            println!("  show the user prompt, context, and one compact severity-colored 5h/7d row");
        }
        SidebarLayout::Stacked => {
            println!(
                "  show provider, model, the user prompt, then cache, TTL, context, 5h, and 7d on their own rows"
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
    if !brand.is_on() {
        println!("  write no brand colors; severity colors stay");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_quota_rows_without_replacing_official_rows() {
        let original = r#"[ui.sidebar.agents]
rows = [["state_icon", "agent"]]
"#;
        let updated = add_quota_row(original).unwrap();
        assert!(updated.contains("$quota_5h"));
        assert!(updated.contains("$quota_week"));
        assert!(updated.contains("state_icon"));
        assert!(updated.contains("agent"));
        assert!(updated.contains("$quota_topic"));
        assert!(updated.contains("$quota_5h_warning"));
        assert!(updated.contains("$quota_week_danger"));
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
                row.as_array().is_some_and(|items| {
                    items.iter().any(|item| item.as_str() == Some("state_icon"))
                })
            })
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
        assert_eq!(identity_index + 1, provider_index);
        assert_eq!(provider_index + 1, model_index);
        assert_eq!(model_index + 1, topic_index);
        assert!(!rows
            .iter()
            .any(|row| row_contains_token(row, "$quota_provider_model")));
        assert!(row_is_only_token(rows, "$quota_provider"));
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
    fn switching_between_packed_and_stacked_is_idempotent() {
        let original = "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"tab\", \"agent\"]]\n";
        let packed = add_quota_row(original).unwrap();
        let stacked =
            add_quota_row_for(&packed, &AgentSelection::SUPPORTED, SidebarLayout::Stacked).unwrap();
        let stacked_document = stacked.parse::<DocumentMut>().unwrap();
        assert!(row_is_only_token(
            stacked_document["ui"]["sidebar"]["agents"]["rows"]
                .as_array()
                .unwrap(),
            "$quota_cache"
        ));
        let packed_again =
            add_quota_row_for(&stacked, &AgentSelection::SUPPORTED, SidebarLayout::Packed).unwrap();
        assert_eq!(packed_again, packed);
    }

    fn row_contains_token(row: &Value, token: &str) -> bool {
        row.as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| configured_token_name(item) == Some(token))
        })
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
    fn tab_labels_use_primary_text_instead_of_herdr_dim_gray() {
        let updated =
            add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"tab\", \"agent\"]]\n")
                .unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows = document["ui"]["sidebar"]["agents"]["rows"]
            .as_array()
            .unwrap();
        let identity = rows
            .iter()
            .find(|row| {
                row.as_array().is_some_and(|items| {
                    items.iter().any(|item| item.as_str() == Some("state_icon"))
                })
            })
            .and_then(Value::as_array)
            .unwrap();
        let tab = identity
            .iter()
            .find(|item| configured_token_name(item) == Some("tab"))
            .and_then(Value::as_inline_table)
            .unwrap();
        assert_eq!(tab.get("fg").and_then(Value::as_str), Some(TEXT_COLOR));
        assert_eq!(tab.get("bold").and_then(Value::as_bool), Some(true));
        assert_eq!(tab.get("dim").and_then(Value::as_bool), Some(false));
    }

    #[test]
    fn stacked_model_uses_brand_dim_and_leaves_provider_hue_alone() {
        let updated = add_quota_row_for(
            "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n",
            &[Harness::Claude, Harness::Codex],
            SidebarLayout::Stacked,
        )
        .unwrap();
        let document = updated.parse::<DocumentMut>().unwrap();
        let rows_by_agent = document["ui"]["sidebar"]["agents"]["rows_by_agent"]
            .as_table()
            .unwrap();
        for (provider, brand, dim) in [
            ("claude", "#e88461", "#f0a080"),
            ("codex", "#c4d7f5", "#aab9d0"),
        ] {
            let rows = rows_by_agent[provider].as_array().unwrap();
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
            assert_eq!(provider_fg, Some(brand), "{provider} brand");
            assert_eq!(model_fg, Some(dim), "{provider} dim");
        }
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
    fn migrates_old_quota_only_rows_and_restores_herdr_state_row() {
        let original = r#"[ui.sidebar.agents]
rows = [["$quota_provider", "$quota_status"], ["$quota_summary"]]
"#;
        let updated = add_quota_row(original).unwrap();
        assert!(updated.contains("state_icon"));
        assert!(updated.contains("$quota_provider"));
        assert!(updated.contains("$quota_5h"));
        assert!(updated.contains("$quota_week"));
        assert_eq!(add_quota_row(&updated).unwrap(), updated);
    }

    #[test]
    fn preserves_user_owned_provider_rows() {
        let original = r#"[ui.sidebar.agents]
rows = [["state_icon", "agent"]]

[ui.sidebar.agents.rows_by_agent]
claude = [["state_icon", "agent"]]
"#;
        let updated = add_quota_row(original).unwrap();
        assert!(updated.contains("claude = [[\"state_icon\", \"agent\"]]"));
        assert!(updated.contains("codex ="));
        assert!(updated.contains("opencode ="));
        let removed = remove_quota_row(&updated).unwrap();
        assert!(removed.contains("claude = [[\"state_icon\", \"agent\"]]"));
        assert!(!removed.contains("codex ="));
        assert!(!removed.contains("opencode ="));
    }

    #[test]
    fn applying_one_agent_writes_only_that_agents_row() {
        let updated = add_quota_row_for(
            "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n",
            &[Harness::Grok],
            SidebarLayout::Packed,
        )
        .unwrap();
        assert!(updated.contains("grok ="));
        for other in ["claude =", "codex =", "agy =", "opencode ="] {
            assert!(!updated.contains(other), "{other} was written: {updated}");
        }
    }

    #[test]
    fn removing_one_agent_leaves_the_others_installed() {
        let full =
            add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n").unwrap();
        let removed = remove_quota_row_for(&full, &[Harness::Grok], false).unwrap();
        assert!(!removed.contains("grok ="), "grok survived: {removed}");
        for kept in ["claude =", "codex =", "agy =", "opencode ="] {
            assert!(removed.contains(kept), "{kept} was lost: {removed}");
        }
        // The shared parts belong to the installation, not to grok.
        assert!(removed.contains("rows = "));
        assert!(removed.contains("row_gap"));
        assert!(removed.contains(REFRESH_ACTION));
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
        assert!(!complete.contains(REFRESH_ACTION));
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
        assert!(!updated
            .contains("opencode = [[\"state_icon\", \"agent\"]] # herdr-agent-quota-provider"));
        let removed = remove_quota_row(&updated).unwrap();
        assert!(removed.contains("opencode = [[\"state_icon\", \"agent\"]]"));
    }

    #[test]
    fn fresh_tree_gains_managed_opencode_rows() {
        let updated =
            add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n").unwrap();
        assert!(updated.contains("opencode ="));
        assert!(updated.contains("herdr-agent-quota-provider"));
        let removed = remove_quota_row(&updated).unwrap();
        assert!(!removed.contains("opencode ="));
    }

    #[test]
    fn empty_sidebar_configuration_round_trips_to_empty() {
        let updated = add_quota_row("").unwrap();
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
        assert!(flushed.contains("row_gap = 0 # herdr-agent-quota"));
        assert_eq!(remove_quota_row(&flushed).unwrap(), "");
    }

    #[test]
    fn plugin_owned_row_gap_follows_the_requested_spacing() {
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
        assert!(flushed.contains("row_gap = 0 # herdr-agent-quota"));
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
        assert!(separated.contains("row_gap = 1 # herdr-agent-quota"));
        assert!(!separated.contains("row_gap = 0"));
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

    #[test]
    fn hiding_every_optional_field_keeps_the_official_row_and_the_provider() {
        let bare = applied(FieldSet::parse("none").unwrap(), BrandColors::On);
        assert!(bare.contains("state_icon"), "{bare}");
        assert!(bare.contains("$quota_provider\""), "{bare}");
        for token in ["$quota_topic", "$quota_context", "$quota_5h", "$quota_week"] {
            assert!(!bare.contains(token), "{token} survived:\n{}", rows(&bare));
        }
    }

    /// Without brand hues the per-agent rows would duplicate the shared ones,
    /// so the plugin writes none at all.
    #[test]
    fn brand_colours_off_writes_no_per_agent_rows() {
        let plain = applied(FieldSet::all(), BrandColors::Off);
        assert!(!plain.contains("rows_by_agent"), "{plain}");
        // Severity colours are information, not decoration: they stay.
        assert!(plain.contains(QUOTA_DANGER_COLOR), "{plain}");
        let branded = applied(FieldSet::all(), BrandColors::On);
        assert!(branded.contains("rows_by_agent"), "{branded}");
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
