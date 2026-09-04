#[test]
fn pane_focus_uses_the_quota_only_focus_path() {
    let manifest = include_str!("../herdr-plugin.toml");
    let hook = manifest
        .split("[[events]]")
        .find(|event| event.contains("on = \"pane.focused\""))
        .unwrap();
    assert!(hook.contains(" focus\"]"));
    assert!(!hook.contains(" event\"]"));
}

#[test]
fn plugin_exposes_one_click_configure_and_uninstall_actions() {
    let manifest = include_str!("../herdr-plugin.toml");
    assert!(manifest.contains("id = \"configure\""));
    assert!(manifest.contains("configure --apply"));
    assert!(manifest.contains("id = \"uninstall\""));
    assert!(manifest.contains("configure --uninstall"));
}

#[test]
fn grok_runtime_refresh_does_not_go_through_a_plugin_action() {
    let manifest = include_str!("../herdr-plugin.toml");
    assert!(!manifest.contains("id = \"refresh-grok\""));
}

#[test]
fn exited_panes_do_not_trigger_a_quota_refresh() {
    let manifest = include_str!("../herdr-plugin.toml");
    assert!(!manifest.contains("on = \"pane.exited\""));
}

/// Settings remains a pane so it receives the plugin environment, while a
/// small action lets a keybinding open that pane.
#[test]
fn settings_are_an_action_backed_by_a_plugin_pane() {
    let manifest = include_str!("../herdr-plugin.toml");
    let pane = manifest
        .split("[[panes]]")
        .find(|pane| pane.contains("id = \"settings\""))
        .unwrap();
    assert!(pane.contains(" settings\"]"), "{pane}");
    assert!(pane.contains("placement = \"popup\""), "{pane}");
    let action = manifest
        .split("[[actions]]")
        .find(|action| action.contains("id = \"open-settings\""))
        .expect("settings action");
    assert!(action.contains("plugin pane open"), "{action}");
    assert!(action.contains("--entrypoint settings"), "{action}");
}

/// The pane draws one row per option and cannot fold them, so the popup has to
/// be tall enough for the whole list. A default popup is 24 rows; the list is
/// longer than that, and an option below the fold is an option nobody finds.
#[test]
fn the_settings_popup_is_tall_enough_for_every_option() {
    let manifest = include_str!("../herdr-plugin.toml");
    let pane = manifest
        .split("[[panes]]")
        .find(|pane| pane.contains("id = \"settings\""))
        .unwrap();
    let height: usize = pane
        .lines()
        .find_map(|line| line.strip_prefix("height = "))
        .expect("the settings popup declares a height")
        .trim()
        .parse()
        .unwrap();
    // Three section headers, seven choices, seven fields, eight agents, four
    // lines of TUI chrome, and the two rows consumed by Herdr's pane border.
    assert!(height >= 3 + 7 + 7 + 8 + 4 + 2, "height = {height}");
}

/// Herdr accepts a plugin-owned agent view only from `plugin:<manifest id>`
/// and answers `plugin_not_found` for anything else, so the source the plugin
/// sends and the id it is installed under have to be the same string.
#[test]
fn the_agent_view_source_matches_the_manifest_id() {
    let manifest = include_str!("../herdr-plugin.toml");
    let id = manifest
        .lines()
        .find_map(|line| line.strip_prefix("id = "))
        .expect("the manifest declares an id")
        .trim()
        .trim_matches('"');
    assert_eq!(id, "herdr-agent-quota");
    let source = include_str!("../src/herdr.rs")
        .lines()
        .find_map(|line| line.trim().strip_prefix("const AGENT_VIEW_SOURCE: &str = "))
        .expect("the plugin declares an agent view source")
        .trim()
        .trim_end_matches(';')
        .trim_matches('"');
    assert_eq!(source, format!("plugin:{id}"));
}
