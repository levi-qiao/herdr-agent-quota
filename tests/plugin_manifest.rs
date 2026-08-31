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

/// The settings pane writes configuration by re-invoking `configure`, so it
/// needs the plugin environment Herdr injects into a pane. It must therefore
/// stay a pane entry rather than becoming an action with a fixed command.
#[test]
fn settings_are_edited_in_a_pane_that_inherits_the_plugin_environment() {
    let manifest = include_str!("../herdr-plugin.toml");
    let pane = manifest
        .split("[[panes]]")
        .find(|pane| pane.contains("id = \"settings\""))
        .unwrap();
    assert!(pane.contains(" settings\"]"), "{pane}");
    assert!(pane.contains("placement = \"popup\""), "{pane}");
}
