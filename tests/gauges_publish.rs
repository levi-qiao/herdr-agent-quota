#![cfg(unix)]

use herdr_agent_quota::cache::CacheStore;
use herdr_agent_quota::cli::{FieldSet, PercentStyle, SidebarLayout};
use herdr_agent_quota::model::{Provider, ProviderSnapshot, UsageWindow, WindowKind};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

struct Sidebar {
    directory: tempfile::TempDir,
    tokens: BTreeMap<String, String>,
}

impl Sidebar {
    fn new(style: PercentStyle) -> Self {
        Self::with_prompt_cache(
            style,
            json!({"warm":true,"expires_at":CacheStore::now_unix() + 29 * 60 + 30}),
        )
    }

    fn expired(style: PercentStyle) -> Self {
        Self::with_prompt_cache(style, json!({"warm":false,"expires_at":null}))
    }

    fn with_prompt_cache(style: PercentStyle, prompt_cache: serde_json::Value) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let cache = CacheStore::new(root);
        cache.set_sidebar_layout(SidebarLayout::Gauges).unwrap();
        cache.set_percent_style(style).unwrap();
        cache.set_fields(FieldSet::all()).unwrap();
        let payload = json!({
            "session_id":"sample-session",
            "context_window":{"used_percentage":43.0,"current_usage":{
                "input_tokens":48,"cache_read_input_tokens":952,"cache_creation_input_tokens":0
            }},
            "prompt_cache": prompt_cache,
            "rate_limits":{"five_hour":{"used_percentage":20.0},"seven_day":{"used_percentage":35.0}}
        });
        let snapshot = herdr_agent_quota::providers::claude::parse_statusline(
            &payload,
            CacheStore::now_unix(),
        )
        .unwrap();
        cache
            .save_statusline_observation(Provider::Claude, snapshot, &payload)
            .unwrap();
        fs::write(
            root.join("herdr"),
            r#"#!/bin/sh
printf '%s %s\n' "$1" "$2" >> "$TEST_CALLS"
case "$1 $2" in
  'agent list') cat "$TEST_INVENTORY" ;;
  'pane get') printf '%s\n' '{"result":{"pane":{"scroll":{"offset_from_bottom":0}}}}' ;;
  'pane report-metadata') printf '%s\n' "$@" > "$TEST_REPORT" ;;
  *) exit 1 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(root.join("herdr"), fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            directory,
            tokens: BTreeMap::new(),
        }
    }

    fn refresh(&mut self, width: usize) -> Vec<String> {
        let root = self.directory.path();
        fs::write(
            root.join("config.toml"),
            format!("[ui]\nsidebar_width = {width}\n"),
        )
        .unwrap();
        fs::write(
            root.join("inventory.json"),
            serde_json::to_vec(&json!({"result":{"agents":[{
                "agent":"claude", "pane_id":"w1:p1", "agent_status":"idle",
                "agent_session":{"value":"sample-session"}, "tokens":self.tokens
            }]}}))
            .unwrap(),
        )
        .unwrap();
        fs::write(root.join("calls"), "").unwrap();
        fs::write(root.join("report"), "").unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"))
            .args(["refresh", "--provider", "claude"])
            .env("HERDR_PLUGIN_STATE_DIR", root)
            .env("HERDR_PLUGIN_CONFIG_DIR", root)
            .env("HERDR_BIN_PATH", root.join("herdr"))
            .env("HERDR_CONFIG_FILE", root.join("config.toml"))
            .env("HERDR_SOCKET_PATH", root.join("herdr.sock"))
            .env("XDG_STATE_HOME", root.join("xdg-state"))
            .env("CLAUDE_CONFIG_DIR", root.join("claude"))
            .env("HERDR_AGENT_QUOTA_AGENTS", "claude")
            .env("TEST_INVENTORY", root.join("inventory.json"))
            .env("TEST_CALLS", root.join("calls"))
            .env("TEST_REPORT", root.join("report"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = fs::read_to_string(root.join("report")).unwrap();
        let args: Vec<_> = report.lines().collect();
        assert!(
            args.iter()
                .filter(|arg| matches!(**arg, "--token" | "--clear-token"))
                .count()
                <= 16
        );
        for pair in args.windows(2) {
            match pair[0] {
                "--token" => {
                    let (name, value) = pair[1].split_once('=').unwrap();
                    self.tokens.insert(name.into(), value.into());
                }
                "--clear-token" => {
                    self.tokens.remove(pair[1]);
                }
                _ => {}
            }
        }
        let calls: Vec<_> = fs::read_to_string(root.join("calls"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(calls.iter().filter(|call| *call == "agent list").count(), 1);
        assert!(!calls.iter().any(|call| call == "pane read"));
        assert!(
            calls
                .iter()
                .filter(|call| *call == "pane report-metadata")
                .count()
                <= 1
        );
        calls
    }

    fn context(&self) -> &str {
        self.tokens
            .iter()
            .find(|(name, _)| name.starts_with("quota_context"))
            .unwrap()
            .1
    }
}

#[test]
fn gauges_refresh_keeps_the_selected_context_quantity_when_the_sidebar_narrows() {
    for (style, expected) in [
        (PercentStyle::Remaining, "57%"),
        (PercentStyle::Used, "43%"),
    ] {
        let mut sidebar = Sidebar::new(style);
        for width in [36, 18, 26, 36] {
            sidebar.refresh(width);
            assert!(
                sidebar.context().ends_with(expected),
                "{style:?} width={width}: {}",
                sidebar.context()
            );
            assert!(
                sidebar.context().starts_with("cx "),
                "{}",
                sidebar.context()
            );
        }
    }
}

#[test]
fn gauges_refresh_folds_cache_and_ttl_only_when_they_fit_and_suppresses_noops() {
    let mut sidebar = Sidebar::new(PercentStyle::Remaining);
    sidebar.refresh(26);
    assert!(sidebar.tokens["quota_cache"].starts_with("cache 95.2% · ttl≈"));
    sidebar.refresh(36);
    assert!(sidebar.tokens["quota_cache"].starts_with("cache 95.2% · ttl≈"));
    assert!(!sidebar.tokens.contains_key("quota_cache_ttl"));
    assert!(!sidebar
        .refresh(36)
        .iter()
        .any(|call| call == "pane report-metadata"));
    sidebar.refresh(18);
    assert_eq!(sidebar.tokens["quota_cache"], "cache 95.2%");
    assert!(sidebar.tokens["quota_cache_ttl"].starts_with("ttl≈"));
    sidebar.refresh(36);
    assert!(!sidebar.tokens.contains_key("quota_cache_ttl"));
}

#[test]
fn gauges_refresh_keeps_no_cached_on_its_amber_token() {
    let mut sidebar = Sidebar::expired(PercentStyle::Remaining);
    sidebar.refresh(36);
    assert_eq!(sidebar.tokens["quota_cache"], "cache 95.2%");
    assert_eq!(sidebar.tokens["quota_cache_state"], "no cached");
    assert!(!sidebar.tokens.contains_key("quota_cache_ttl"));
    sidebar.refresh(18);
    assert_eq!(sidebar.tokens["quota_cache"], "cache 95.2%");
    assert_eq!(sidebar.tokens["quota_cache_state"], "no cached");
}

#[test]
fn gauges_folding_respects_hidden_fields_and_other_layouts() {
    let mut sidebar = Sidebar::new(PercentStyle::Remaining);
    let cache = CacheStore::new(sidebar.directory.path());
    for fields in ["cache", "ttl", "none"] {
        cache.set_fields(FieldSet::parse(fields).unwrap()).unwrap();
        sidebar.refresh(36);
        assert_eq!(
            sidebar.tokens["quota_cache"], "cache 95.2%",
            "fields={fields}"
        );
        assert!(
            sidebar.tokens.contains_key("quota_cache_ttl"),
            "fields={fields}"
        );
    }
    cache.set_fields(FieldSet::all()).unwrap();
    for layout in [SidebarLayout::Packed, SidebarLayout::Stacked] {
        cache.set_sidebar_layout(layout).unwrap();
        sidebar.refresh(36);
        assert_eq!(sidebar.tokens["quota_cache"], "cache 95.2%");
        assert!(sidebar.tokens.contains_key("quota_cache_ttl"));
        assert_eq!(sidebar.tokens["quota_context"], "context 43%");
    }
}

#[test]
fn gauges_window_rows_fit_herdr_content_width_in_both_percentage_modes() {
    use herdr_agent_quota::model::ResetAt;
    use herdr_agent_quota::presentation::{MetadataTokens, SidebarShape};
    for style in [PercentStyle::Remaining, PercentStyle::Used] {
        for width in 18..=40 {
            let snapshot = ProviderSnapshot::new(
                Provider::Claude,
                vec![
                    UsageWindow::new(
                        WindowKind::FiveHour,
                        0.0,
                        Some(ResetAt::from_unix_seconds(86340)),
                    )
                    .unwrap(),
                    UsageWindow::new(
                        WindowKind::Monthly,
                        100.0,
                        Some(ResetAt::from_unix_seconds(29 * 86400 + 23 * 3600)),
                    )
                    .unwrap(),
                ],
                0,
            );
            let shape = SidebarShape::new(SidebarLayout::Gauges, width);
            let tokens =
                MetadataTokens::from_snapshot_for_session(&snapshot, 0, None, style, shape);
            for value in [&tokens.quota_5h, &tokens.quota_week] {
                assert!(
                    value.chars().count() <= shape.content_width,
                    "{style:?} width={width}: {value}"
                );
            }
        }
    }
}
