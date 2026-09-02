use herdr_agent_quota::cache::CacheStore;
use herdr_agent_quota::configure::herdr::{add_quota_row, remove_quota_row};
use herdr_agent_quota::model::{Provider, ProviderSnapshot, UsageWindow, WindowKind};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn install_herdr_stub(state: &Path, agent_list: &str) -> (PathBuf, PathBuf) {
    let log = state.join("herdr.log");
    let executable = state.join("herdr");
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nif [ \"$1 $2\" = \"agent list\" ]; then\n  printf '%s\\n' '{}'\nelif [ \"$1 $2\" = \"pane read\" ]; then\n  printf '%s\\n' \"$*\" >> '{}'\nelif [ \"$1 $2\" = \"pane report-metadata\" ]; then\n  printf '%s\\n' \"$*\" >> '{}'\nelif [ \"$1 $2\" = \"notification show\" ]; then\n  printf '%s\\n' \"$*\" >> '{}'\nfi\n",
            agent_list,
            log.display(),
            log.display(),
            log.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();
    (executable, log)
}

fn run_claude_collector(state: &Path, herdr: &Path, input: &[u8]) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"))
        .arg("claude-statusline")
        .env("HERDR_PLUGIN_STATE_DIR", state)
        .env("HERDR_BIN_PATH", herdr)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    assert!(child.wait_with_output().unwrap().status.success());
}

fn run_claude_collector_with_timeout(state: &Path, input: &[u8], timeout: Duration) -> bool {
    let mut child = Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"))
        .arg("claude-statusline")
        .env("HERDR_PLUGIN_STATE_DIR", state)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.success();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn hold_refresh_lock_in_child(state: &Path) -> std::process::Child {
    let lock_path = state.join("refresh.lock");
    let ready_path = state.join("refresh.lock.ready");
    let locker = Command::new("perl")
        .args([
            "-e",
            r#"use Fcntl qw(:flock); open my $f, '+>', $ARGV[0] or die $!; flock($f, LOCK_EX) or die $!; open my $r, '>', $ARGV[1] or die $!; print $r 'locked'; close $r; sleep 20"#,
            lock_path.to_str().unwrap(),
            ready_path.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !ready_path.exists() {
        assert!(
            Instant::now() < deadline,
            "refresh lock helper did not start"
        );
        thread::sleep(Duration::from_millis(10));
    }
    locker
}

fn run_claude_refresh(state: &Path, herdr: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"))
        .args(["refresh", "--provider", "claude", "--force"])
        .env("HERDR_PLUGIN_STATE_DIR", state)
        .env("HERDR_BIN_PATH", herdr)
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn sidebar_configuration_is_idempotent_and_removes_plugin_rows() {
    let original = "[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n";
    let canonical_without_plugin = "[ui.sidebar.agents]\nrows = [[\"state_icon\"]]\n";
    let applied = add_quota_row(original).unwrap();
    assert!(applied.contains("key = \"prefix+shift+r\""));
    assert!(applied.contains("type = \"plugin_action\""));
    assert!(applied.contains("command = \"herdr-agent-quota.refresh\""));
    assert!(applied.contains("key = \"prefix+shift+q\""));
    assert!(applied.contains("command = \"herdr-agent-quota.open-settings\""));
    assert_eq!(add_quota_row(&applied).unwrap(), applied);
    assert_eq!(
        remove_quota_row(&applied).unwrap(),
        canonical_without_plugin
    );
}

#[test]
fn sidebar_configuration_preserves_a_conflicting_refresh_key() {
    let original = concat!(
        "[[keys.command]]\n",
        "key = \"prefix+shift+r\"\n",
        "type = \"shell\"\n",
        "command = \"echo user-owned\"\n",
        "description = \"user refresh\"\n\n",
        "[ui.sidebar.agents]\n",
        "rows = [[\"state_icon\", \"agent\"]]\n"
    );
    let applied = add_quota_row(original).unwrap();
    assert_eq!(applied.matches("key = \"prefix+shift+r\"").count(), 1);
    assert!(applied.contains("command = \"echo user-owned\""));
    assert!(!applied.contains("command = \"herdr-agent-quota.refresh\""));
    assert_eq!(
        remove_quota_row(&applied).unwrap(),
        "[[keys.command]]\nkey = \"prefix+shift+r\"\ntype = \"shell\"\ncommand = \"echo user-owned\"\ndescription = \"user refresh\"\n\n[ui.sidebar.agents]\nrows = [[\"state_icon\"]]\n"
    );
}

#[test]
fn sidebar_configuration_preserves_a_conflicting_settings_key() {
    let original = concat!(
        "[[keys.command]]\n",
        "key = \"prefix+shift+q\"\n",
        "type = \"shell\"\n",
        "command = \"echo user-owned\"\n",
        "description = \"user settings\"\n\n",
        "[ui.sidebar.agents]\n",
        "rows = [[\"state_icon\", \"agent\"]]\n"
    );
    let applied = add_quota_row(original).unwrap();
    assert_eq!(applied.matches("key = \"prefix+shift+q\"").count(), 1);
    assert!(applied.contains("command = \"echo user-owned\""));
    assert!(!applied.contains("command = \"herdr-agent-quota.open-settings\""));
    assert!(applied.contains("command = \"herdr-agent-quota.refresh\""));
    let removed = remove_quota_row(&applied).unwrap();
    assert!(removed.contains("command = \"echo user-owned\""));
    assert!(!removed.contains("herdr-agent-quota.refresh"));
}

#[test]
fn default_herdr_rows_become_plane_provider_usage_and_topic_lines() {
    let original = concat!(
        "[ui.sidebar.agents]\n",
        "rows = [[\"state_icon\", \"workspace\", \"tab\"], [\"agent\"]]\n"
    );
    let applied = add_quota_row(original).unwrap();
    assert!(applied.contains("$quota_provider_model"));
    assert!(applied.contains("bold = true"));
    for base in ["$quota_5h", "$quota_week", "$quota_week_inline"] {
        for band in ["normal", "warning", "danger", "unknown"] {
            assert!(applied.contains(&format!("{base}_{band}")), "{base}_{band}");
        }
        // `Severity` has no caution band, so no token can ever fill this row.
        assert!(
            !applied.contains(&format!("{base}_caution")),
            "{base}_caution is unreachable and must not be configured"
        );
    }
    assert!(!applied.contains("[\"$quota_summary\"]"));
    assert!(applied.contains("$quota_topic"));
    assert!(applied.contains("$quota_context"));
    assert!(applied.contains("$quota_provider_model"));
    assert!(!applied.contains("fg = \"#969eae\""));
    assert!(applied.find("$quota_provider_model").unwrap() < applied.find("$quota_topic").unwrap());
    assert!(applied.contains("$quota_cache"));
    assert!(applied.contains("$quota_cache_ttl"));
    assert!(applied.contains("$quota_cache_state"));
    assert!(!applied.contains("$quota_5h_label"));
    assert!(!applied.contains("$quota_5h_eta"));
    assert!(!applied.contains("fg = \"#c8cdd6\""));
    assert!(applied.contains("row_gap = 1 # herdr-agent-quota"));
    assert!(applied.find("$quota_topic").unwrap() < applied.find("$quota_5h_normal").unwrap());
    assert!(applied.contains("fg = \"#82d978\""));
    assert!(applied.contains("fg = \"#e4b957\""));
    assert!(!applied.contains("fg = \"#c6d768\""));
    assert!(!applied.contains("fg = \"#e2bd58\""));
    assert!(applied.contains("fg = \"#f16f7e\""));
    assert!(!applied.contains("fg = \"#eceef2\""));
    assert!(!applied.contains("selection_bg"));
    assert!(!applied.contains("active_row_bg"));
    assert!(applied.contains("[ui.sidebar.agents.rows_by_agent]"));
    assert!(applied.contains("fg = \"#e88461\""));
    assert!(applied.contains("fg = \"#c4d7f5\""));
    assert!(applied.contains("fg = \"#d5d5d8\""));
    assert!(applied.contains("fg = \"#8ab4f8\""));
    assert!(applied.contains("fg = \"#bba3e8\""));
    assert!(applied.contains("fg = \"#d4a0c8\""));
}

#[test]
fn non_semantic_text_inherits_the_active_herdr_theme() {
    let applied = add_quota_row(concat!(
        "[theme]\n",
        "name = \"one-light\"\n\n",
        "[ui.sidebar.agents]\n",
        "rows = [[\"state_icon\", \"tab\", \"agent\"]]\n",
    ))
    .unwrap();
    let document = applied.parse::<toml_edit::DocumentMut>().unwrap();
    let rows = document["ui"]["sidebar"]["agents"]["rows"]
        .as_array()
        .unwrap();
    let inherited = [
        "tab",
        "$quota_topic",
        "$quota_cache",
        "$quota_cache_ttl",
        "$quota_context",
        "$quota_5h_unknown",
        "$quota_week_unknown",
        "$quota_week_inline_unknown",
    ];

    for token in inherited {
        let style = rows
            .iter()
            .filter_map(toml_edit::Value::as_array)
            .flat_map(|row| row.iter())
            .find(|item| configured_token(item) == Some(token))
            .and_then(toml_edit::Value::as_inline_table)
            .unwrap_or_else(|| panic!("missing styled token {token}"));
        assert!(
            !style.contains_key("fg"),
            "{token} must inherit Herdr's foreground: {style}"
        );
    }
}

#[test]
fn context_is_the_penultimate_row_and_model_shares_provider_style() {
    let applied =
        add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n").unwrap();
    let document = applied.parse::<toml_edit::DocumentMut>().unwrap();
    let agents = &document["ui"]["sidebar"]["agents"];
    let rows = agents["rows"].as_array().unwrap();
    let context_index = rows
        .iter()
        .position(|row| {
            row.as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item.as_inline_table()
                        .and_then(|table| table.get("token"))
                        .and_then(toml_edit::Value::as_str)
                        .is_some_and(|token| token == "$quota_context")
                })
            })
        })
        .unwrap();
    let limit_index = rows
        .iter()
        .position(|row| {
            row.as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item.as_inline_table()
                        .and_then(|table| table.get("token"))
                        .and_then(toml_edit::Value::as_str)
                        .is_some_and(|token| token == "$quota_5h_normal")
                })
            })
        })
        .unwrap();
    assert_eq!(context_index + 1, limit_index);
    assert_eq!(limit_index + 1, rows.len());

    for (provider, color, dim) in [
        ("claude", Some("#e88461"), Some("#f0a080")),
        ("codex", Some("#c4d7f5"), Some("#aab9d0")),
        ("grok", Some("#d5d5d8"), Some("#acb0b7")),
        ("agy", Some("#8ab4f8"), Some("#a7c7fa")),
        ("opencode", None, None),
        ("pi", Some("#d4a0c8"), None),
        ("omp", Some("#bba3e8"), None),
    ] {
        let provider_rows = agents["rows_by_agent"][provider].as_value().unwrap();
        let rendered = provider_rows.to_string();
        if let Some(color) = color {
            let needle = format!("fg = \"{color}\"");
            assert_eq!(
                rendered.matches(needle.as_str()).count(),
                1,
                "wrong brand color for {provider}: {rendered}"
            );
        } else {
            assert!(
                rendered
                    .starts_with(" [[\"state_icon\", { token = \"$quota_provider_model\", bold"),
                "{provider} should use the neutral identity style: {rendered}"
            );
        }
        if let Some(dim) = dim {
            assert!(
                !rendered.contains(dim),
                "packed {provider} should not use model dim: {rendered}"
            );
        }
    }
}

#[test]
fn provider_model_is_compact_and_every_provider_can_fold_week_without_five_hour() {
    let applied =
        add_quota_row("[ui.sidebar.agents]\nrows = [[\"state_icon\", \"agent\"]]\n").unwrap();
    let document = applied.parse::<toml_edit::DocumentMut>().unwrap();
    let agents = &document["ui"]["sidebar"]["agents"];
    let rows = agents["rows"].as_array().unwrap();
    let identity_row = rows
        .iter()
        .find(|row| row_contains_token(row, "$quota_provider_model"))
        .unwrap();
    let identity_tokens = identity_row.as_array().unwrap();
    assert!(!identity_tokens.iter().any(|item| {
        matches!(
            configured_token(item),
            Some("$quota_provider") | Some("$quota_model")
        )
    }));

    for provider in ["claude", "codex", "grok", "agy", "opencode", "pi"] {
        let provider_rows = agents["rows_by_agent"][provider].as_array().unwrap();
        let context_row = provider_rows
            .iter()
            .find(|row| row_contains_token(row, "$quota_context"))
            .unwrap()
            .as_array()
            .unwrap();
        assert!(
            context_row
                .iter()
                .any(|item| configured_token(item) == Some("$quota_week_inline_normal")),
            "{provider} should be able to fold 7d onto context when 5h is empty"
        );
        assert!(
            context_row
                .iter()
                .all(|item| configured_token(item) != Some("$quota_5h_normal")),
            "{provider} must not put 5h on the context row"
        );
        assert!(
            provider_rows.iter().any(|row| {
                row_contains_token(row, "$quota_week_normal")
                    && row_contains_token(row, "$quota_5h_normal")
                    && !row_contains_token(row, "$quota_context")
            }),
            "{provider} should keep 5h/7d on a dedicated limits row"
        );
    }
}

#[test]
fn existing_provider_and_model_tokens_are_migrated_to_one_identity_token() {
    let applied = add_quota_row(
        r#"[ui.sidebar.agents]
rows = [["state_icon", "tab", { token = "$quota_provider" }, { token = "$quota_model" }]]
"#,
    )
    .unwrap();
    let document = applied.parse::<toml_edit::DocumentMut>().unwrap();
    let rows = document["ui"]["sidebar"]["agents"]["rows"]
        .as_array()
        .unwrap();
    let identity_row = rows
        .iter()
        .find(|row| row_contains_token(row, "$quota_provider_model"))
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(
        identity_row
            .iter()
            .filter(|item| configured_token(item) == Some("$quota_provider_model"))
            .count(),
        1
    );
    assert!(!identity_row.iter().any(|item| {
        matches!(
            configured_token(item),
            Some("$quota_provider") | Some("$quota_model")
        )
    }));
}

fn configured_token(value: &toml_edit::Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.as_inline_table()?.get("token")?.as_str())
}

fn row_contains_token(row: &toml_edit::Value, token: &str) -> bool {
    row.as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| configured_token(item) == Some(token))
    })
}

#[test]
fn configuration_removes_obsolete_session_summary_rows() {
    let original = concat!(
        "[ui.sidebar.agents]\n",
        "rows = [[\"state_icon\", \"agent\"], [\"$quota_topic\"], [\"$quota_session\"]]\n"
    );
    let applied = add_quota_row(original).unwrap();
    assert!(applied.contains("$quota_context"));
    assert!(!applied.contains("$quota_session"));
}

#[test]
fn sidebar_configuration_preserves_an_explicit_row_gap() {
    let original = concat!(
        "[ui.sidebar.agents]\n",
        "row_gap = 2\n",
        "rows = [[\"state_icon\", \"agent\"]]\n"
    );
    let applied = add_quota_row(original).unwrap();
    assert!(applied.contains("row_gap = 2"));
    assert!(!applied.contains("row_gap = 1"));
    assert_eq!(
        remove_quota_row(&applied).unwrap(),
        "[ui.sidebar.agents]\nrow_gap = 2\nrows = [[\"state_icon\"]]\n"
    );
}

#[test]
fn sidebar_configuration_migrates_the_plugin_owned_gap_to_separated_panes() {
    let original = concat!(
        "[ui.sidebar.agents]\n",
        "row_gap = 0 # herdr-agent-quota\n",
        "rows = [[\"state_icon\", \"agent\"]]\n"
    );
    let applied = add_quota_row(original).unwrap();
    assert!(applied.contains("row_gap = 1 # herdr-agent-quota"));
    assert!(!applied.contains("row_gap = 0"));
}

#[test]
fn claude_collector_is_silent_without_a_previous_statusline() {
    let state = tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"))
        .arg("claude-statusline")
        .env("HERDR_PLUGIN_STATE_DIR", state.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(include_bytes!("fixtures/claude/statusline-both.json"))
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn claude_collector_does_not_wait_for_a_refresh_lock() {
    let state = tempdir().unwrap();
    let mut locker = hold_refresh_lock_in_child(state.path());

    assert!(run_claude_collector_with_timeout(
        state.path(),
        include_bytes!("fixtures/claude/statusline-both.json"),
        Duration::from_secs(2),
    ));
    assert!(state
        .path()
        .join("claude-statusline.observation.json")
        .exists());
    let _ = locker.kill();
    let _ = locker.wait();
}

#[test]
fn claude_collector_bounds_a_hanging_previous_statusline() {
    let state = tempdir().unwrap();
    fs::write(
        state.path().join("claude-statusline.original.json"),
        r#"{"type":"command","command":"sleep 20"}"#,
    )
    .unwrap();

    let started = Instant::now();
    assert!(run_claude_collector_with_timeout(
        state.path(),
        include_bytes!("fixtures/claude/statusline-both.json"),
        Duration::from_secs(4),
    ));
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[test]
fn agy_collector_is_silent_without_a_previous_statusline() {
    let state = tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"))
        .arg("agy-statusline")
        .env("HERDR_PLUGIN_STATE_DIR", state.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(include_bytes!("fixtures/agy/statusline-both.json"))
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn claude_cache_is_published_by_refresh_event() {
    let state = tempdir().unwrap();
    let (herdr_stub, herdr_log) = install_herdr_stub(
        state.path(),
        r#"{"result":{"agents":[{"agent":"claude","pane_id":"w1:p1"}]}}"#,
    );
    run_claude_collector(
        state.path(),
        &herdr_stub,
        include_bytes!("fixtures/claude/statusline-both.json"),
    );
    assert!(!herdr_log.exists());

    run_claude_refresh(state.path(), &herdr_stub);
    let report = fs::read_to_string(herdr_log).unwrap();
    assert!(!report.contains("pane read"));
    assert!(report.contains("quota_5h_warning=5h 42%"));
    assert!(report.contains("quota_week_normal=7d 73%"));
    assert!(!report.contains("quota_5h_label="));
    assert!(!report.contains("quota_week_label="));
}

#[test]
fn claude_statusline_without_rate_limits_clears_stale_quota_windows() {
    let state = tempdir().unwrap();
    let (herdr_stub, _herdr_log) = install_herdr_stub(
        state.path(),
        r#"{"result":{"agents":[{"agent":"claude","pane_id":"w1:p1"}]}}"#,
    );
    run_claude_collector(
        state.path(),
        &herdr_stub,
        include_bytes!("fixtures/claude/statusline-both.json"),
    );
    run_claude_collector(
        state.path(),
        &herdr_stub,
        br#"{"context_window":{"used_percentage":43.0}}"#,
    );
    run_claude_refresh(state.path(), &herdr_stub);

    let snapshot: serde_json::Value =
        serde_json::from_slice(&fs::read(state.path().join("claude-statusline.json")).unwrap())
            .unwrap();
    assert_eq!(snapshot["windows"].as_array().unwrap().len(), 0);
    assert_eq!(snapshot["context"]["used_percent"], 43.0);
}

#[test]
fn statusline_without_context_keeps_the_last_context_snapshot() {
    let state = tempdir().unwrap();
    let (herdr_stub, herdr_log) = install_herdr_stub(
        state.path(),
        r#"{"result":{"agents":[{"agent":"claude","pane_id":"w1:p1","agent_session":{"value":"session-1"}}]}}"#,
    );
    run_claude_collector(
        state.path(),
        &herdr_stub,
        br#"{
            "session_id": "session-1",
            "context_window": {"used_percentage": 23.5},
            "rate_limits": {"seven_day": {"used_percentage": 27.0}}
        }"#,
    );
    run_claude_collector(
        state.path(),
        &herdr_stub,
        br#"{"session_id":"session-1","rate_limits":{"seven_day":{"used_percentage":28.0}}}"#,
    );

    run_claude_refresh(state.path(), &herdr_stub);
    let report = fs::read_to_string(herdr_log).unwrap();
    assert!(report.contains("quota_context=context 24%"));
    assert!(report.contains("quota_week_normal=7d 72%"));
    assert!(!report.contains("quota_week_label="));
}

#[test]
fn concurrent_claude_accounts_keep_their_own_quota_windows() {
    // Two Claude panes signed in to different accounts (for example a work
    // and a personal login in separate CLAUDE_CONFIG_DIR checkouts) each send
    // their own statusLine ticks. Sidebar percentages are remaining, not used:
    // work 18%/10% used → 82%/90% remaining; personal 82%/90% used → 18%/10%.
    let state = tempdir().unwrap();
    let (herdr_stub, herdr_log) = install_herdr_stub(
        state.path(),
        r#"{"result":{"agents":[
            {"agent":"claude","pane_id":"w1:p1","agent_session":{"value":"work-session"}},
            {"agent":"claude","pane_id":"w2:p1","agent_session":{"value":"personal-session"}}
        ]}}"#,
    );
    run_claude_collector(
        state.path(),
        &herdr_stub,
        br#"{
            "session_id": "work-session",
            "rate_limits": {
                "five_hour": {"used_percentage": 18.0},
                "seven_day": {"used_percentage": 10.0}
            }
        }"#,
    );
    run_claude_collector(
        state.path(),
        &herdr_stub,
        br#"{
            "session_id": "personal-session",
            "rate_limits": {
                "five_hour": {"used_percentage": 82.0},
                "seven_day": {"used_percentage": 90.0}
            }
        }"#,
    );

    run_claude_refresh(state.path(), &herdr_stub);
    let report = fs::read_to_string(herdr_log).unwrap();
    let work_report = report
        .lines()
        .find(|line| line.contains("w1:p1"))
        .expect("work pane reported");
    let personal_report = report
        .lines()
        .find(|line| line.contains("w2:p1"))
        .expect("personal pane reported");
    assert!(
        work_report.contains("quota_5h_normal=5h 82%"),
        "{work_report}"
    );
    assert!(
        work_report.contains("quota_week_normal=7d 90%"),
        "{work_report}"
    );
    assert!(
        personal_report.contains("quota_5h_danger=5h 18%"),
        "{personal_report}"
    );
    assert!(
        personal_report.contains("quota_week_danger=7d 10%"),
        "{personal_report}"
    );
}

#[test]
fn quota_refresh_does_not_report_metadata_to_a_scrolled_pane() {
    let state = tempdir().unwrap();
    let log = state.path().join("herdr.log");
    let herdr = state.path().join("herdr");
    fs::write(
        &herdr,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1 $2\" = \"agent list\" ]; then\n  printf '%s\\n' '{}'\nelif [ \"$1 $2\" = \"pane get\" ]; then\n  printf '%s\\n' '{}'\nfi\n",
            log.display(),
            r#"{"result":{"agents":[{"agent":"claude","pane_id":"w1:p1"}]}}"#,
            r#"{"result":{"pane":{"scroll":{"offset_from_bottom":12}}}}"#,
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&herdr).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&herdr, permissions).unwrap();

    run_claude_collector(
        state.path(),
        &herdr,
        include_bytes!("fixtures/claude/statusline-both.json"),
    );
    run_claude_refresh(state.path(), &herdr);
    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("pane get w1:p1"));
    assert!(!calls.contains("pane report-metadata"));
}

#[test]
fn focus_refreshes_only_the_selected_provider_without_reading_the_pane() {
    let state = tempdir().unwrap();
    let log = state.path().join("herdr.log");
    let herdr = state.path().join("herdr");
    fs::write(
        &herdr,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1 $2\" = \"pane current\" ]; then\n  printf '%s\\n' '{}'\nelif [ \"$1 $2\" = \"agent list\" ]; then\n  printf '%s\\n' '{}'\nelif [ \"$1 $2\" = \"pane get\" ]; then\n  printf '%s\\n' '{}'\nfi\n",
            log.display(),
            r#"{"result":{"pane":{"agent":"claude","pane_id":"w1:p1"}}}"#,
            r#"{"result":{"agents":[{"agent":"claude","pane_id":"w1:p1"}]}}"#,
            r#"{"result":{"pane":{"scroll":{"offset_from_bottom":0}}}}"#,
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&herdr).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&herdr, permissions).unwrap();
    run_claude_collector(
        state.path(),
        &herdr,
        include_bytes!("fixtures/claude/statusline-both.json"),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"))
        .arg("focus")
        .env("HERDR_PLUGIN_STATE_DIR", state.path())
        .env("HERDR_BIN_PATH", &herdr)
        .output()
        .unwrap();
    assert!(output.status.success());
    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("pane current"));
    assert!(!calls.contains("pane read"));
    assert!(calls.contains("pane report-metadata w1:p1"));
}

#[test]
fn agent_event_refreshes_and_reads_topics_only_for_its_provider() {
    let state = tempdir().unwrap();
    let log = state.path().join("herdr.log");
    let codex_log = state.path().join("codex.log");
    let herdr = state.path().join("herdr");
    let codex = state.path().join("codex");
    fs::write(
        &herdr,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1 $2\" = \"agent list\" ]; then\n  printf '%s\\n' '{}'\nelif [ \"$1 $2\" = \"pane get\" ]; then\n  printf '%s\\n' '{}'\nfi\n",
            log.display(),
            r#"{"result":{"agents":[{"agent":"claude","pane_id":"w1:p1"},{"agent":"grok","pane_id":"w1:p2"}]}}"#,
            r#"{"result":{"pane":{"scroll":{"offset_from_bottom":0}}}}"#,
        ),
    )
    .unwrap();
    fs::write(
        &codex,
        format!("#!/bin/sh\nprintf called > '{}'\n", codex_log.display()),
    )
    .unwrap();
    for executable in [&herdr, &codex] {
        let mut permissions = fs::metadata(executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(executable, permissions).unwrap();
    }
    run_claude_collector(
        state.path(),
        &herdr,
        include_bytes!("fixtures/claude/statusline-both.json"),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"))
        .arg("event")
        .env("HERDR_PLUGIN_STATE_DIR", state.path())
        .env("HERDR_BIN_PATH", &herdr)
        .env("CODEX_BIN_PATH", &codex)
        .env("GROK_HOME", state.path().join("missing-grok-home"))
        .env(
            "HERDR_PLUGIN_EVENT_JSON",
            r#"{"event":{"pane":{"agent":"claude","pane_id":"w1:p1"}}}"#,
        )
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!codex_log.exists());
    let calls = fs::read_to_string(log).unwrap();
    assert!(calls.contains("pane read w1:p1"));
    assert!(!calls.contains("pane read w1:p2"));
}

fn chmod_exec(path: &Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn original_four_inventory_with_working_codex() -> &'static str {
    r#"{"result":{"agents":[{"agent":"claude","pane_id":"w1:p1","agent_status":"idle"},{"agent":"codex","pane_id":"w1:p2","agent_status":"working"},{"agent":"grok","pane_id":"w1:p3","agent_status":"idle"},{"agent":"agy","pane_id":"w1:p4","agent_status":"idle"},{"agent":"opencode","pane_id":"w1:p9","agent_status":"working"}]}}"#
}

fn install_logged_herdr_and_codex(
    state: &Path,
    agent_list: &str,
    pane_current: Option<&str>,
) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let herdr_log = state.join("herdr.log");
    let codex_log = state.join("codex.log");
    let herdr = state.join("herdr");
    let codex = state.join("codex");
    let pane_current_branch = pane_current
        .map(|json| {
            format!("elif [ \"$1 $2\" = \"pane current\" ]; then\n  printf '%s\\n' '{json}'\n")
        })
        .unwrap_or_default();
    fs::write(
        &herdr,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{log}'\nif [ \"$1 $2\" = \"agent list\" ]; then\n  printf '%s\\n' '{agents}'\n{pane_current}elif [ \"$1 $2\" = \"pane get\" ]; then\n  printf '%s\\n' '{scroll}'\nfi\n",
            log = herdr_log.display(),
            agents = agent_list,
            pane_current = pane_current_branch,
            scroll = r#"{"result":{"pane":{"scroll":{"offset_from_bottom":0}}}}"#,
        ),
    )
    .unwrap();
    fs::write(
        &codex,
        format!("#!/bin/sh\nprintf called > '{}'\n", codex_log.display()),
    )
    .unwrap();
    chmod_exec(&herdr);
    chmod_exec(&codex);
    (herdr, herdr_log, codex, codex_log)
}

fn run_event_binary(
    state: &Path,
    herdr: &Path,
    codex: &Path,
    event_json: &str,
) -> std::process::Output {
    run_event_binary_with_xdg(state, herdr, codex, event_json, &state.join("xdg-data"))
}

fn run_event_binary_with_xdg(
    state: &Path,
    herdr: &Path,
    codex: &Path,
    event_json: &str,
    xdg_data_home: &Path,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"))
        .arg("event")
        .env("HERDR_PLUGIN_STATE_DIR", state)
        .env("HERDR_BIN_PATH", herdr)
        .env("CODEX_BIN_PATH", codex)
        .env("GROK_HOME", state.join("missing-grok-home"))
        .env("XDG_DATA_HOME", xdg_data_home)
        .env(
            "XDG_CACHE_HOME",
            xdg_data_home
                .parent()
                .unwrap_or(xdg_data_home)
                .join("xdg-cache"),
        )
        .env_remove("GROK_AUTH_FILE")
        .env_remove("OPENCODE_API_KEY")
        .env("HERDR_PLUGIN_EVENT_JSON", event_json)
        .output()
        .unwrap()
}

fn opencode_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/opencode")
        .join(name)
}

fn install_opencode_store(xdg_data_home: &Path, auth_name: &str, db_name: &str) {
    let dir = xdg_data_home.join("opencode");
    let cache = xdg_data_home
        .parent()
        .unwrap_or(xdg_data_home)
        .join("xdg-cache/opencode");
    fs::create_dir_all(&dir).unwrap();
    fs::create_dir_all(&cache).unwrap();
    fs::copy(opencode_fixture(db_name), dir.join("opencode.db")).unwrap();
    fs::copy(opencode_fixture(auth_name), dir.join("auth.json")).unwrap();
    fs::copy(opencode_fixture("models.json"), cache.join("models.json")).unwrap();
}

fn plugin_quota_tokens() -> &'static str {
    r#"{"quota_provider":"Claude","quota_provider_model":"Claude","quota_5h_label":"5h","quota_5h_danger":"10%","quota_week_label":"7d","quota_week_warning":"20%"}"#
}

fn two_opencode_inventory(named_session: &str, named_tokens: &str) -> String {
    format!(
        r#"{{"result":{{"agents":[{{"agent":"opencode","pane_id":"w1:p9","agent_status":"working","agent_session":{{"agent":"opencode","value":"{named_session}"}},"tokens":{named_tokens}}},{{"agent":"opencode","pane_id":"w1:p10","agent_status":"idle","agent_session":{{"agent":"opencode","value":"{named_session}"}}}},{{"agent":"codex","pane_id":"w1:p2","agent_status":"working"}}]}}}}"#
    )
}

fn opencode_working_event(pane_id: &str) -> String {
    format!(
        r#"{{"event":"pane_agent_status_changed","data":{{"pane_id":"{pane_id}","agent":"opencode","status":"working"}}}}"#
    )
}

fn assert_named_opencode_event(herdr_log: &Path, named: &str, sibling: &str) {
    let calls = fs::read_to_string(herdr_log).unwrap_or_default();
    assert_eq!(
        calls.matches("agent list").count(),
        1,
        "expected one inventory: {calls}"
    );
    assert!(
        calls.contains(&format!("pane read {named}")),
        "named pane was not read: {calls}"
    );
    assert!(
        calls.contains("pane read") && calls.contains("--source visible"),
        "named pane read must use visible: {calls}"
    );
    assert!(
        !calls.contains("recent"),
        "must not use recent pane sources: {calls}"
    );
    assert!(
        !calls.contains(&format!("pane read {sibling}")),
        "sibling pane was read: {calls}"
    );
    assert!(
        !calls.contains(&format!("pane report-metadata {sibling}")),
        "sibling pane was reported: {calls}"
    );
}

fn original_four_untouched(state: &Path, codex_log: &Path) {
    assert!(
        !codex_log.exists(),
        "Codex stub was invoked: {}",
        fs::read_to_string(codex_log).unwrap_or_default()
    );
    for marker in [
        "codex-app-server.refresh",
        "grok-cli-billing.refresh",
        "claude-statusline.refresh",
        "agy-statusline.refresh",
        "codex-app-server.json",
        "grok-cli-billing.json",
        "claude-statusline.json",
        "agy-statusline.json",
        "codex-app-server.refresh.lock",
        "grok-cli-billing.refresh.lock",
        "claude-statusline.refresh.lock",
        "agy-statusline.refresh.lock",
    ] {
        assert!(!state.join(marker).exists(), "{marker} was written");
    }
}

fn assert_no_original_four_collection(state: &Path, herdr_log: &Path, codex_log: &Path) {
    assert!(
        !codex_log.exists(),
        "Codex stub was invoked: {}",
        fs::read_to_string(codex_log).unwrap_or_default()
    );
    let calls = fs::read_to_string(herdr_log).unwrap_or_default();
    assert!(
        !calls.contains("pane read"),
        "unexpected pane read: {calls}"
    );
    assert!(
        !calls.contains("pane report-metadata"),
        "unexpected metadata write: {calls}"
    );
    for marker in [
        "codex-app-server.refresh",
        "grok-cli-billing.refresh",
        "claude-statusline.refresh",
        "agy-statusline.refresh",
    ] {
        assert!(!state.join(marker).exists(), "{marker} was written");
    }
}

#[test]
fn opencode_working_event_publishes_only_the_named_local_identity() {
    let state = tempdir().unwrap();
    let xdg = state.path().join("xdg-data");
    install_opencode_store(&xdg, "auth-go.json", "sessions.db");
    let inventory = two_opencode_inventory("ses_go", "{}");
    let (herdr, herdr_log, codex, codex_log) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);

    let output = run_event_binary_with_xdg(
        state.path(),
        &herdr,
        &codex,
        &opencode_working_event("w1:p9"),
        &xdg,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    thread::sleep(Duration::from_millis(200));
    original_four_untouched(state.path(), &codex_log);
    assert_named_opencode_event(&herdr_log, "w1:p9", "w1:p10");
    let calls = fs::read_to_string(&herdr_log).unwrap_or_default();
    assert!(!calls.contains("pane report-metadata w1:p10"), "{calls}");
    assert!(calls.contains("pane report-metadata w1:p9"), "{calls}");
    assert!(
        calls.contains("--token quota_provider_model=OpenCode Go/kimi-k2.5"),
        "{calls}"
    );
}

#[test]
fn unknown_agent_working_event_does_not_refresh_any_collector() {
    let state = tempdir().unwrap();
    let (herdr, herdr_log, codex, codex_log) = install_logged_herdr_and_codex(
        state.path(),
        original_four_inventory_with_working_codex(),
        None,
    );

    let output = run_event_binary(
        state.path(),
        &herdr,
        &codex,
        r#"{"event":"pane_agent_status_changed","data":{"pane_id":"w1:p8","agent":"cursor","status":"working"}}"#,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    thread::sleep(Duration::from_millis(200));
    assert_no_original_four_collection(state.path(), &herdr_log, &codex_log);
}

#[test]
fn focus_on_an_opencode_pane_does_not_refresh_collectors() {
    let state = tempdir().unwrap();
    let (herdr, herdr_log, codex, codex_log) = install_logged_herdr_and_codex(
        state.path(),
        original_four_inventory_with_working_codex(),
        Some(r#"{"result":{"pane":{"agent":"opencode","pane_id":"w1:p9"}}}"#),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"))
        .arg("focus")
        .env("HERDR_PLUGIN_STATE_DIR", state.path())
        .env("HERDR_BIN_PATH", &herdr)
        .env("CODEX_BIN_PATH", &codex)
        .env("GROK_HOME", state.path().join("missing-grok-home"))
        .env("XDG_DATA_HOME", state.path().join("xdg-data"))
        .env_remove("GROK_AUTH_FILE")
        .env_remove("OPENCODE_API_KEY")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fs::read_to_string(&herdr_log).unwrap_or_default();
    assert!(calls.contains("pane current"), "{calls}");
    assert!(!calls.contains("pane read"), "{calls}");
    assert_no_original_four_collection(state.path(), &herdr_log, &codex_log);
}

/// The whole alert path, end to end: the threshold on disk, a snapshot below
/// it, and the one `herdr notification show` it is allowed to produce.
#[test]
fn a_low_quota_notifies_once_and_re_arms_only_after_recovering() {
    let state = tempdir().unwrap();
    let (herdr_stub, herdr_log) = install_herdr_stub(
        state.path(),
        r#"{"result":{"agents":[{"agent":"claude","pane_id":"w1:p1","tokens":{}}]}}"#,
    );
    fs::create_dir_all(state.path()).unwrap();
    fs::write(state.path().join("low-quota-alert"), "20%").unwrap();

    let notifications = || {
        fs::read_to_string(&herdr_log)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with("notification show"))
            .count()
    };
    let quota = |five_hour: f64, seven_day: f64| {
        format!(
            r#"{{"rate_limits":{{"five_hour":{{"used_percentage":{five_hour}}},"seven_day":{{"used_percentage":{seven_day}}}}}}}"#
        )
    };

    // The statusLine collector only caches; publishing, and so warning, is the
    // refresh that follows it.
    let observe = |used_five_hour: f64, used_seven_day: f64| {
        run_claude_collector(
            state.path(),
            &herdr_stub,
            quota(used_five_hour, used_seven_day).as_bytes(),
        );
        run_claude_refresh(state.path(), &herdr_stub);
    };

    // 88% of the weekly window spent leaves 12, which is under the threshold.
    observe(10.0, 88.0);
    assert_eq!(notifications(), 1, "{:?}", fs::read_to_string(&herdr_log));

    // Still low: nothing new to say.
    observe(20.0, 91.0);
    assert_eq!(notifications(), 1);

    // Back above the threshold, then below it again: one more warning.
    observe(10.0, 30.0);
    assert_eq!(notifications(), 1);
    observe(10.0, 95.0);
    assert_eq!(notifications(), 2);
}

/// The default has to be silence: a plugin that starts notifying after an
/// upgrade is a plugin people turn off.
#[test]
fn no_alert_threshold_means_no_notification_however_low_the_quota_is() {
    let state = tempdir().unwrap();
    let (herdr_stub, herdr_log) = install_herdr_stub(
        state.path(),
        r#"{"result":{"agents":[{"agent":"claude","pane_id":"w1:p1","tokens":{}}]}}"#,
    );
    run_claude_collector(
        state.path(),
        &herdr_stub,
        br#"{"rate_limits":{"five_hour":{"used_percentage":100.0},"seven_day":{"used_percentage":100.0}}}"#,
    );
    run_claude_refresh(state.path(), &herdr_stub);
    let log = fs::read_to_string(&herdr_log).unwrap_or_default();
    assert!(!log.contains("notification show"), "{log}");
}

/// The pane's tokens are exactly what a previous publish left behind, sort key
/// included: this asserts the steady state, where nothing has changed and so
/// nothing may be written.
#[test]
fn claude_collector_does_not_republish_unchanged_quota() {
    let state = tempdir().unwrap();
    let (herdr_stub, herdr_log) = install_herdr_stub(
        state.path(),
        r#"{"result":{"agents":[{"agent":"claude","pane_id":"w1:p1","tokens":{"quota_provider":"Claude","quota_provider_model":"Claude","quota_5h_warning":"5h 42%","quota_week_normal":"7d 73%","quota_headroom":"042"}}]}}"#,
    );

    let input = br#"{
        "rate_limits": {
            "five_hour": {"used_percentage": 58.0},
            "seven_day": {"used_percentage": 27.0}
        }
    }"#;
    run_claude_collector(state.path(), &herdr_stub, input);
    assert!(!herdr_log.exists());

    run_claude_refresh(state.path(), &herdr_stub);
    assert!(!herdr_log.exists());
}

#[test]
fn sidebar_configuration_preserves_user_owned_opencode_rows() {
    let original = concat!(
        "[ui.sidebar.agents]\n",
        "rows = [[\"state_icon\", \"agent\"]]\n\n",
        "[ui.sidebar.agents.rows_by_agent]\n",
        "opencode = [[\"state_icon\", \"agent\"]]\n"
    );
    let applied = add_quota_row(original).unwrap();
    assert!(applied.contains("opencode = [[\"state_icon\", \"agent\"]]"));
    assert!(applied.contains("codex ="));
    let removed = remove_quota_row(&applied).unwrap();
    assert!(removed.contains("opencode = [[\"state_icon\", \"agent\"]]"));
    assert!(!removed.contains("codex ="));
}

#[test]
fn sidebar_configuration_preserves_user_owned_pi_rows() {
    let original = concat!(
        "[ui.sidebar.agents]\n",
        "rows = [[\"state_icon\", \"agent\"]]\n\n",
        "[ui.sidebar.agents.rows_by_agent]\n",
        "pi = [[\"state_icon\", \"agent\"]]\n"
    );
    let applied = add_quota_row(original).unwrap();
    assert!(applied.contains("pi = [[\"state_icon\", \"agent\"]]"));
    assert!(applied.contains("codex ="));
    let removed = remove_quota_row(&applied).unwrap();
    assert!(removed.contains("pi = [[\"state_icon\", \"agent\"]]"));
    assert!(!removed.contains("codex ="));
}

#[test]
fn opencode_go_event_is_named_pane_only_and_repeatable() {
    let state = tempdir().unwrap();
    let xdg = state.path().join("xdg-data");
    install_opencode_store(&xdg, "auth-go.json", "sessions.db");
    let inventory = two_opencode_inventory(
        "ses_go",
        r#"{"quota_provider":"OpenCode Go","quota_model":"kimi-k2.5","quota_provider_model":"OpenCode Go/kimi-k2.5"}"#,
    );
    let (herdr, herdr_log, codex, codex_log) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);
    let event = opencode_working_event("w1:p9");

    for _ in 0..2 {
        fs::write(&herdr_log, "").ok();
        let output = run_event_binary_with_xdg(state.path(), &herdr, &codex, &event, &xdg);
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        thread::sleep(Duration::from_millis(200));
        original_four_untouched(state.path(), &codex_log);
        assert_named_opencode_event(&herdr_log, "w1:p9", "w1:p10");
        let calls = fs::read_to_string(&herdr_log).unwrap_or_default();
        assert!(!calls.contains("opencode.ai"), "{calls}");
        assert!(!calls.contains("pane report-metadata w1:p10"), "{calls}");
        assert!(!calls.contains("pane report-metadata w1:p9"), "{calls}");
    }
}

#[test]
fn opencode_payg_event_clears_plugin_quota_once() {
    let state = tempdir().unwrap();
    let xdg = state.path().join("xdg-data");
    install_opencode_store(&xdg, "auth-payg.json", "sessions.db");
    let inventory = two_opencode_inventory("ses_payg", plugin_quota_tokens());
    let (herdr, herdr_log, codex, codex_log) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);

    let output = run_event_binary_with_xdg(
        state.path(),
        &herdr,
        &codex,
        &opencode_working_event("w1:p9"),
        &xdg,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    thread::sleep(Duration::from_millis(200));
    original_four_untouched(state.path(), &codex_log);
    assert_named_opencode_event(&herdr_log, "w1:p9", "w1:p10");
    let calls = fs::read_to_string(&herdr_log).unwrap();
    assert!(
        calls.contains("pane report-metadata w1:p9"),
        "expected one-time quota clear: {calls}"
    );
    assert!(
        calls.contains("--clear-token") && calls.contains("quota_5h"),
        "expected plugin quota tokens to be cleared: {calls}"
    );
    assert!(!calls.contains("pane report-metadata w1:p10"), "{calls}");
    assert!(!state
        .path()
        .join("opencode-go.opencode-store.refresh.lock")
        .exists());
}

#[test]
fn opencode_indeterminate_event_preserves_plugin_quota() {
    let state = tempdir().unwrap();
    let xdg = state.path().join("xdg-data");
    install_opencode_store(&xdg, "auth-one-key.json", "sessions.db");
    let inventory = two_opencode_inventory("ses_absent", plugin_quota_tokens());
    let (herdr, herdr_log, codex, codex_log) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);

    let output = run_event_binary_with_xdg(
        state.path(),
        &herdr,
        &codex,
        &opencode_working_event("w1:p9"),
        &xdg,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    original_four_untouched(state.path(), &codex_log);
    let calls = fs::read_to_string(&herdr_log).unwrap();
    assert_eq!(calls.matches("agent list").count(), 1, "{calls}");
    assert!(calls.contains("pane read w1:p9"), "{calls}");
    assert!(!calls.contains("pane read w1:p10"), "{calls}");
    assert!(
        !calls.contains("pane report-metadata"),
        "indeterminate must not clear quota: {calls}"
    );
}

#[test]
fn opencode_malformed_local_data_preserves_plugin_quota() {
    let state = tempdir().unwrap();
    let xdg = state.path().join("xdg-data");
    install_opencode_store(&xdg, "auth-malformed.json", "malformed.db");
    let inventory = two_opencode_inventory("ses_go", plugin_quota_tokens());
    let (herdr, herdr_log, codex, codex_log) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);

    let output = run_event_binary_with_xdg(
        state.path(),
        &herdr,
        &codex,
        &opencode_working_event("w1:p9"),
        &xdg,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    original_four_untouched(state.path(), &codex_log);
    let calls = fs::read_to_string(&herdr_log).unwrap();
    assert!(
        !calls.contains("pane report-metadata"),
        "malformed evidence must preserve quota: {calls}"
    );
}

#[test]
fn opencode_mismatched_event_pane_is_a_noop() {
    let state = tempdir().unwrap();
    let xdg = state.path().join("xdg-data");
    install_opencode_store(&xdg, "auth-go.json", "sessions.db");
    let inventory = two_opencode_inventory("ses_go", plugin_quota_tokens());
    let (herdr, herdr_log, codex, codex_log) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);

    let output = run_event_binary_with_xdg(
        state.path(),
        &herdr,
        &codex,
        r#"{"event":"pane_agent_status_changed","data":{"pane_id":"w1:p2","agent":"opencode","status":"working"}}"#,
        &xdg,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    original_four_untouched(state.path(), &codex_log);
    let calls = fs::read_to_string(&herdr_log).unwrap();
    assert_eq!(calls.matches("agent list").count(), 1, "{calls}");
    assert!(!calls.contains("pane read"), "{calls}");
    assert!(!calls.contains("pane report-metadata"), "{calls}");
}

/// Every file the plugin can write for an agent, so an on-demand install can
/// be checked for footprint rather than just for the row it added.
struct AgentHomes {
    state: PathBuf,
    herdr_config: PathBuf,
    claude_settings: PathBuf,
    agy_settings: PathBuf,
    grok_home: PathBuf,
}

impl AgentHomes {
    fn new(root: &Path) -> Self {
        Self {
            state: root.join("state"),
            herdr_config: root.join("herdr/config.toml"),
            claude_settings: root.join("claude/settings.json"),
            agy_settings: root.join("agy/settings.json"),
            grok_home: root.join("grok-home"),
        }
    }

    fn configure(&self, args: &[&str]) -> std::process::Output {
        self.configure_with_env(args, &[])
    }

    fn configure_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        fs::create_dir_all(&self.state).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"));
        for (key, value) in env {
            command.env(key, value);
        }
        command
            .arg("configure")
            .args(args)
            .env("HERDR_PLUGIN_STATE_DIR", &self.state)
            .env("HERDR_CONFIG_FILE", &self.herdr_config)
            .env("CLAUDE_SETTINGS_FILE", &self.claude_settings)
            .env("AGY_SETTINGS_FILE", &self.agy_settings)
            .env("GROK_HOME", &self.grok_home)
            .env("HERDR_BIN_PATH", self.state.join("herdr-absent"))
            .output()
            .unwrap()
    }

    fn sidebar(&self) -> String {
        fs::read_to_string(&self.herdr_config).unwrap_or_default()
    }
}

#[test]
fn installing_one_agent_leaves_every_other_agent_untouched() {
    let root = tempdir().unwrap();
    let homes = AgentHomes::new(root.path());

    let output = homes.configure(&["--apply", "--agent", "claude"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let sidebar = homes.sidebar();
    assert!(sidebar.contains("claude ="), "{sidebar}");
    for other in ["codex =", "grok =", "agy =", "opencode =", "pi =", "omp ="] {
        assert!(!sidebar.contains(other), "{other} was written: {sidebar}");
    }

    // Someone who does not use Agy or Grok must end up with nothing of theirs
    // on disk, so nothing of theirs can start or interfere later.
    assert!(homes.claude_settings.exists(), "Claude was not configured");
    assert!(
        !homes.agy_settings.exists(),
        "an unselected Agy settings file was created"
    );
    assert!(
        !homes.grok_home.exists(),
        "an unselected Grok home was created"
    );
}

#[test]
fn installing_only_pi_adds_only_its_sidebar_style() {
    let root = tempdir().unwrap();
    let homes = AgentHomes::new(root.path());
    let output = homes.configure(&["--apply", "--agent", "pi"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sidebar = homes.sidebar();
    assert!(sidebar.contains("pi ="), "{sidebar}");
    for other in [
        "claude =",
        "codex =",
        "grok =",
        "agy =",
        "opencode =",
        "omp =",
    ] {
        assert!(!sidebar.contains(other), "{other} was written: {sidebar}");
    }
    assert!(!homes.claude_settings.exists());
    assert!(!homes.agy_settings.exists());
    assert!(!homes.grok_home.exists());
}

#[test]
fn uninstalling_one_agent_keeps_the_rest_working() {
    let root = tempdir().unwrap();
    let homes = AgentHomes::new(root.path());
    assert!(homes.configure(&["--apply"]).status.success());
    assert!(homes.claude_settings.exists());

    let output = homes.configure(&["--uninstall", "--agent", "grok"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let sidebar = homes.sidebar();
    assert!(!sidebar.contains("grok ="), "grok survived: {sidebar}");
    for kept in [
        "claude =",
        "codex =",
        "agy =",
        "opencode =",
        "pi =",
        "omp =",
    ] {
        assert!(sidebar.contains(kept), "{kept} was lost: {sidebar}");
    }
    assert!(
        homes.claude_settings.exists(),
        "removing Grok tore out the Claude statusLine"
    );
}

#[test]
fn uninstall_without_an_agent_still_removes_everything() {
    let root = tempdir().unwrap();
    let homes = AgentHomes::new(root.path());
    assert!(homes.configure(&["--apply"]).status.success());
    assert!(!homes.sidebar().is_empty());

    assert!(homes.configure(&["--uninstall"]).status.success());
    let sidebar = homes.sidebar();
    assert!(
        !sidebar.contains("rows_by_agent"),
        "a managed row survived a full uninstall: {sidebar}"
    );
}

#[test]
fn a_partial_uninstall_can_be_repeated_and_then_completed() {
    let root = tempdir().unwrap();
    let homes = AgentHomes::new(root.path());
    assert!(homes.configure(&["--apply"]).status.success());

    for _ in 0..2 {
        assert!(homes
            .configure(&["--uninstall", "--agent", "grok,agy"])
            .status
            .success());
        let sidebar = homes.sidebar();
        assert!(!sidebar.contains("grok ="));
        assert!(!sidebar.contains("agy ="));
        assert!(sidebar.contains("claude ="));
    }

    assert!(homes.configure(&["--uninstall"]).status.success());
    assert!(!homes.sidebar().contains("rows_by_agent"));
}

#[test]
fn an_installer_can_narrow_the_selection_through_the_environment() {
    // Herdr plugin actions run a fixed command line, so install.sh has no way
    // to pass --agent; it sets this variable instead.
    let root = tempdir().unwrap();
    let homes = AgentHomes::new(root.path());
    let output =
        homes.configure_with_env(&["--apply"], &[("HERDR_AGENT_QUOTA_AGENTS", "codex,grok")]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let sidebar = homes.sidebar();
    assert!(sidebar.contains("codex ="), "{sidebar}");
    assert!(sidebar.contains("grok ="), "{sidebar}");
    assert!(!sidebar.contains("claude ="), "{sidebar}");
    assert!(!homes.claude_settings.exists());
}

#[test]
fn an_unusable_environment_selection_still_installs_everything() {
    let root = tempdir().unwrap();
    let homes = AgentHomes::new(root.path());
    assert!(homes
        .configure_with_env(&["--apply"], &[("HERDR_AGENT_QUOTA_AGENTS", "nonsense")])
        .status
        .success());
    let sidebar = homes.sidebar();
    for expected in [
        "claude =",
        "codex =",
        "grok =",
        "agy =",
        "opencode =",
        "pi =",
        "omp =",
    ] {
        assert!(sidebar.contains(expected), "{expected} missing: {sidebar}");
    }
}

#[test]
fn stacked_sidebar_layout_is_persisted_across_a_repair() {
    let root = tempdir().unwrap();
    let homes = AgentHomes::new(root.path());
    let output = homes.configure(&["--apply", "--sidebar-layout", "stacked"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        sidebar_is_stacked(&homes.sidebar()),
        "first apply was not stacked: {}",
        homes.sidebar()
    );

    assert!(homes.configure(&["--apply"]).status.success());
    assert!(
        sidebar_is_stacked(&homes.sidebar()),
        "repair dropped stacked: {}",
        homes.sidebar()
    );

    assert!(homes
        .configure(&["--apply", "--sidebar-layout", "packed"])
        .status
        .success());
    assert!(
        sidebar_is_packed(&homes.sidebar()),
        "explicit packed did not switch: {}",
        homes.sidebar()
    );
}

#[test]
fn an_installer_can_select_stacked_layout_through_the_environment() {
    let root = tempdir().unwrap();
    let homes = AgentHomes::new(root.path());
    let output = homes.configure_with_env(
        &["--apply"],
        &[("HERDR_AGENT_QUOTA_SIDEBAR_LAYOUT", "stacked")],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(sidebar_is_stacked(&homes.sidebar()), "{}", homes.sidebar());
}

#[test]
fn flush_row_gap_is_persisted_across_a_repair() {
    let root = tempdir().unwrap();
    let homes = AgentHomes::new(root.path());
    let output = homes.configure(&["--apply", "--row-gap", "0"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(homes.sidebar().contains("row_gap = 0 # herdr-agent-quota"));

    assert!(homes.configure(&["--apply"]).status.success());
    assert!(
        homes.sidebar().contains("row_gap = 0 # herdr-agent-quota"),
        "repair dropped flush gap: {}",
        homes.sidebar()
    );

    assert!(homes
        .configure(&["--apply", "--row-gap", "1"])
        .status
        .success());
    assert!(homes.sidebar().contains("row_gap = 1 # herdr-agent-quota"));
    assert!(!homes.sidebar().contains("row_gap = 0"));
}

#[test]
fn an_installer_can_select_flush_gap_through_the_plugin_config_dir() {
    let root = tempdir().unwrap();
    let homes = AgentHomes::new(root.path());
    let config_dir = root.path().join("plugin-config");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(config_dir.join("row-gap"), "0\n").unwrap();
    fs::write(config_dir.join("sidebar-layout"), "stacked\n").unwrap();
    let output = homes.configure_with_env(
        &["--apply"],
        &[("HERDR_PLUGIN_CONFIG_DIR", config_dir.to_str().unwrap())],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sidebar = homes.sidebar();
    assert!(sidebar_is_stacked(&sidebar), "{sidebar}");
    assert!(
        sidebar.contains("row_gap = 0 # herdr-agent-quota"),
        "{sidebar}"
    );
}

fn sidebar_is_packed(sidebar: &str) -> bool {
    quota_tokens_share_a_row(sidebar, "$quota_cache", "$quota_cache_ttl")
        && quota_tokens_share_a_row(sidebar, "$quota_5h_normal", "$quota_week_normal")
        && tab_shares_row_with_provider_model(sidebar)
}

fn sidebar_is_stacked(sidebar: &str) -> bool {
    !quota_tokens_share_a_row(sidebar, "$quota_cache", "$quota_cache_ttl")
        && !quota_tokens_share_a_row(sidebar, "$quota_context", "$quota_week_inline_normal")
        && !quota_tokens_share_a_row(sidebar, "$quota_5h_normal", "$quota_week_normal")
        && !quota_tokens_share_a_row(sidebar, "$quota_provider", "$quota_model")
        && !tab_shares_row_with_provider_model(sidebar)
        && sidebar_has_token(sidebar, "$quota_provider")
        && sidebar_has_token(sidebar, "$quota_model")
        && !sidebar_has_token(sidebar, "$quota_provider_model")
        && sidebar.contains("$quota_cache")
        && sidebar.contains("$quota_week_normal")
}

fn sidebar_has_token(sidebar: &str, token: &str) -> bool {
    let document = sidebar.parse::<toml_edit::DocumentMut>().unwrap();
    let Some(rows) = document["ui"]["sidebar"]["agents"]["rows"].as_array() else {
        return false;
    };
    let present = rows.iter().any(|row| row_contains_token(row, token));
    present
}

fn tab_shares_row_with_provider_model(sidebar: &str) -> bool {
    let document = sidebar.parse::<toml_edit::DocumentMut>().unwrap();
    let Some(rows) = document["ui"]["sidebar"]["agents"]["rows"].as_array() else {
        return false;
    };
    let shares = rows.iter().any(|row| {
        let Some(items) = row.as_array() else {
            return false;
        };
        items
            .iter()
            .any(|item| configured_token(item) == Some("tab"))
            && items
                .iter()
                .any(|item| configured_token(item) == Some("$quota_provider_model"))
    });
    shares
}

fn quota_tokens_share_a_row(sidebar: &str, left: &str, right: &str) -> bool {
    let document = sidebar.parse::<toml_edit::DocumentMut>().unwrap();
    let Some(rows) = document["ui"]["sidebar"]["agents"]["rows"].as_array() else {
        return false;
    };
    let shares = rows
        .iter()
        .any(|row| row_contains_token(row, left) && row_contains_token(row, right));
    shares
}

fn pi_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pi")
        .join(name)
}

fn install_pi_store(
    state: &Path,
    auth_fixture: &str,
    session_fixture: &str,
    session_id: &str,
) -> (PathBuf, PathBuf, PathBuf) {
    let agent = state.join("pi-agent");
    let sessions = state.join("pi-sessions");
    let project = sessions.join("project");
    fs::create_dir_all(&agent).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::copy(pi_fixture(auth_fixture), agent.join("auth.json")).unwrap();
    let session = project.join(format!("2026-08-29T00-00-00-000Z_{session_id}.jsonl"));
    fs::copy(pi_fixture(session_fixture), &session).unwrap();
    (agent, sessions, session)
}

fn write_pi_models_store(agent: &Path, provider: &str, model: &str, context_window: u64) {
    fs::write(
        agent.join("models-store.json"),
        serde_json::json!({
            provider: {
                "models": [{"id": model, "contextWindow": context_window}]
            }
        })
        .to_string(),
    )
    .unwrap();
}

fn write_codex_auth(state: &Path, account_id: &str) -> PathBuf {
    let home = state.join("codex-home");
    fs::create_dir_all(&home).unwrap();
    fs::write(
        home.join("auth.json"),
        serde_json::json!({"tokens": {"account_id": account_id}}).to_string(),
    )
    .unwrap();
    home
}

fn pi_inventory(session: &Path, tokens: &str) -> String {
    serde_json::json!({
        "result": {"agents": [
            {
                "agent": "pi",
                "pane_id": "w1:p9",
                "agent_status": "working",
                "agent_session": {
                    "agent": "pi",
                    "kind": "path",
                    "source": "herdr:pi",
                    "value": session
                },
                "tokens": serde_json::from_str::<serde_json::Value>(tokens).unwrap()
            },
            {"agent": "pi", "pane_id": "w1:p10", "agent_status": "idle"}
        ]}
    })
    .to_string()
}

fn pi_working_event() -> &'static str {
    r#"{"event":"pane_agent_status_changed","data":{"pane_id":"w1:p9","agent":"pi","status":"working"}}"#
}

fn run_pi_event(
    state: &Path,
    herdr: &Path,
    codex: &Path,
    codex_home: &Path,
    pi_agent: &Path,
    pi_sessions: &Path,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"))
        .arg("event")
        .env("HERDR_PLUGIN_STATE_DIR", state)
        .env("HERDR_BIN_PATH", herdr)
        .env("CODEX_BIN_PATH", codex)
        .env("CODEX_HOME", codex_home)
        .env_remove("CODEX_AUTH_FILE")
        .env("PI_CODING_AGENT_DIR", pi_agent)
        .env("PI_CODING_AGENT_SESSION_DIR", pi_sessions)
        .env("GROK_HOME", state.join("missing-grok-home"))
        .env_remove("GROK_AUTH_FILE")
        .env_remove("OPENCODE_API_KEY")
        .env("HERDR_PLUGIN_EVENT_JSON", pi_working_event())
        .output()
        .unwrap()
}

#[test]
fn pi_codex_event_uses_only_the_proved_canonical_cache_and_reads_no_pane() {
    let state = tempdir().unwrap();
    let (pi_agent, pi_sessions, session) = install_pi_store(
        state.path(),
        "auth-matching.json",
        "session-codex.jsonl",
        "session-codex",
    );
    let codex_home = write_codex_auth(state.path(), "account-same");
    let snapshot = ProviderSnapshot::new(
        Provider::Codex,
        vec![UsageWindow::new(WindowKind::Weekly, 20.0, None).unwrap()],
        CacheStore::now_unix(),
    )
    .with_account_id(Some("account-same".to_string()));
    CacheStore::new(state.path()).save(&snapshot).unwrap();

    let inventory = pi_inventory(&session, "{}");
    let (herdr, herdr_log, codex, codex_log) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);
    let output = run_pi_event(
        state.path(),
        &herdr,
        &codex,
        &codex_home,
        &pi_agent,
        &pi_sessions,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let calls = fs::read_to_string(&herdr_log).unwrap();
    assert_eq!(calls.matches("agent list").count(), 1, "{calls}");
    assert!(!calls.contains("pane read"), "{calls}");
    assert!(calls.contains("pane report-metadata w1:p9"), "{calls}");
    assert!(!calls.contains("pane report-metadata w1:p10"), "{calls}");
    assert!(
        calls.contains("--token quota_provider_model=Codex/model-b"),
        "{calls}"
    );
    assert!(
        codex_log.exists(),
        "proved Pi route did not invoke Codex collector"
    );
    assert!(state.path().join("codex-app-server.json").exists());
    assert!(!state
        .path()
        .join("opencode-go.opencode-store.json")
        .exists());
    assert!(!state.path().join("pi.json").exists());
}

#[test]
fn pi_codex_event_overlays_exact_session_context_and_cache_without_inventing_ttl() {
    let state = tempdir().unwrap();
    let (pi_agent, pi_sessions, session) = install_pi_store(
        state.path(),
        "auth-matching.json",
        "session-codex-usage.jsonl",
        "session-codex-usage",
    );
    write_pi_models_store(&pi_agent, "openai-codex", "model-b", 200);
    let codex_home = write_codex_auth(state.path(), "account-same");
    let snapshot = ProviderSnapshot::new(
        Provider::Codex,
        vec![UsageWindow::new(WindowKind::Weekly, 20.0, None).unwrap()],
        CacheStore::now_unix(),
    )
    .with_account_id(Some("account-same".to_string()));
    CacheStore::new(state.path()).save(&snapshot).unwrap();

    let inventory = pi_inventory(&session, "{}");
    let (herdr, herdr_log, codex, _) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);
    let output = run_pi_event(
        state.path(),
        &herdr,
        &codex,
        &codex_home,
        &pi_agent,
        &pi_sessions,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let calls = fs::read_to_string(&herdr_log).unwrap();
    assert!(
        calls.contains("--token quota_context=context 55%"),
        "{calls}"
    );
    assert!(calls.contains("--token quota_cache=cache 85.0%"), "{calls}");
    assert!(!calls.contains("quota_cache_ttl"), "{calls}");
    assert!(
        calls.contains("--token quota_week_inline_normal=7d 80%"),
        "{calls}"
    );
}

#[test]
fn pi_anthropic_event_uses_the_recorded_one_hour_cache_bucket() {
    let state = tempdir().unwrap();
    let (pi_agent, pi_sessions, session) = install_pi_store(
        state.path(),
        "auth-unsupported-oauth.json",
        "session-anthropic-usage.jsonl",
        "session-anthropic-usage",
    );
    write_pi_models_store(&pi_agent, "anthropic", "model-a", 200);
    let now_millis = CacheStore::now_unix().saturating_mul(1_000).to_string();
    let session_jsonl = fs::read_to_string(&session)
        .unwrap()
        .replace("4070908801000", &now_millis)
        .replace("4070908861000", &now_millis);
    fs::write(&session, session_jsonl).unwrap();
    let codex_home = write_codex_auth(state.path(), "account-same");
    let inventory = pi_inventory(&session, plugin_quota_tokens());
    let (herdr, herdr_log, codex, codex_log) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);

    assert!(run_pi_event(
        state.path(),
        &herdr,
        &codex,
        &codex_home,
        &pi_agent,
        &pi_sessions,
    )
    .status
    .success());

    let calls = fs::read_to_string(&herdr_log).unwrap();
    assert!(
        calls.contains("--token quota_provider_model=Claude/model-a"),
        "{calls}"
    );
    assert!(
        calls.contains("--token quota_context=context 55%"),
        "{calls}"
    );
    assert!(calls.contains("--token quota_cache_ttl=ttl≈"), "{calls}");
    assert!(!codex_log.exists(), "unsupported OAuth invoked Codex");
}

#[test]
fn pi_payg_event_clears_stale_quota_without_invoking_a_collector() {
    let state = tempdir().unwrap();
    let (pi_agent, pi_sessions, session) = install_pi_store(
        state.path(),
        "auth-payg.json",
        "session-payg-usage.jsonl",
        "session-payg-usage",
    );
    write_pi_models_store(&pi_agent, "openai", "model-payg", 210);
    let codex_home = write_codex_auth(state.path(), "account-same");
    let inventory = pi_inventory(&session, plugin_quota_tokens());
    let (herdr, herdr_log, codex, codex_log) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);
    assert!(run_pi_event(
        state.path(),
        &herdr,
        &codex,
        &codex_home,
        &pi_agent,
        &pi_sessions,
    )
    .status
    .success());

    let calls = fs::read_to_string(&herdr_log).unwrap();
    assert_eq!(calls.matches("agent list").count(), 1, "{calls}");
    assert!(!calls.contains("pane read"), "{calls}");
    assert!(calls.contains("pane report-metadata w1:p9"), "{calls}");
    assert!(
        calls.contains("--clear-token") && calls.contains("quota_5h"),
        "{calls}"
    );
    assert!(
        calls.contains("--token quota_context=context 50%"),
        "{calls}"
    );
    assert!(calls.contains("--token quota_cache=cache 80.0%"), "{calls}");
    assert!(!calls.contains("quota_cache_ttl"), "{calls}");
    assert!(!codex_log.exists(), "PAYG route invoked Codex");
}

#[test]
fn pi_indeterminate_event_preserves_quota_but_replaces_session_diagnostics() {
    let state = tempdir().unwrap();
    let (pi_agent, pi_sessions, session) = install_pi_store(
        state.path(),
        "auth-unsupported-oauth.json",
        "session-xai-usage.jsonl",
        "session-xai-usage",
    );
    write_pi_models_store(&pi_agent, "xai", "model-x", 210);
    let codex_home = write_codex_auth(state.path(), "account-same");
    let stale = r#"{"quota_state":"?","quota_provider":"Claude","quota_provider_model":"Claude/old","quota_context":"context 99%","quota_cache":"cache 1.0%","quota_cache_ttl":"ttl≈1h","quota_5h":"5h 10%","quota_week":"7d 20%","quota_summary":"5h 10% · 7d 20%"}"#;
    let inventory = pi_inventory(&session, stale);
    let (herdr, herdr_log, codex, codex_log) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);
    assert!(run_pi_event(
        state.path(),
        &herdr,
        &codex,
        &codex_home,
        &pi_agent,
        &pi_sessions,
    )
    .status
    .success());

    let calls = fs::read_to_string(&herdr_log).unwrap();
    assert!(
        calls.contains("--token quota_provider_model=Grok/model-x"),
        "{calls}"
    );
    assert!(
        calls.contains("--token quota_context=context 50%"),
        "{calls}"
    );
    assert!(calls.contains("--token quota_cache=cache 80.0%"), "{calls}");
    assert!(calls.contains("--clear-token quota_cache_ttl"), "{calls}");
    assert!(calls.contains("--token quota_5h=5h 10%"), "{calls}");
    assert!(calls.contains("--token quota_week=7d 20%"), "{calls}");
    assert!(!codex_log.exists(), "indeterminate route invoked Codex");
}

#[test]
fn pi_different_account_preserves_stale_quota_and_cannot_borrow_codex_cache() {
    let state = tempdir().unwrap();
    let (pi_agent, pi_sessions, session) = install_pi_store(
        state.path(),
        "auth-matching.json",
        "session-codex.jsonl",
        "session-codex",
    );
    let codex_home = write_codex_auth(state.path(), "different-account");
    let snapshot = ProviderSnapshot::new(
        Provider::Codex,
        vec![UsageWindow::new(WindowKind::Weekly, 20.0, None).unwrap()],
        CacheStore::now_unix(),
    )
    .with_account_id(Some("different-account".to_string()));
    CacheStore::new(state.path()).save(&snapshot).unwrap();

    let inventory = pi_inventory(&session, plugin_quota_tokens());
    let (herdr, herdr_log, codex, codex_log) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);
    assert!(run_pi_event(
        state.path(),
        &herdr,
        &codex,
        &codex_home,
        &pi_agent,
        &pi_sessions,
    )
    .status
    .success());

    let calls = fs::read_to_string(&herdr_log).unwrap();
    assert_eq!(calls.matches("agent list").count(), 1, "{calls}");
    assert!(!calls.contains("pane read"), "{calls}");
    assert!(calls.contains("pane report-metadata w1:p9"), "{calls}");
    assert!(
        calls.contains("--token quota_provider_model=Codex/model-b"),
        "{calls}"
    );
    assert!(calls.contains("--token quota_5h_danger=10%"), "{calls}");
    assert!(calls.contains("--token quota_week_warning=20%"), "{calls}");
    assert!(!calls.contains("--clear-token quota_5h"), "{calls}");
    assert!(!calls.contains("--clear-token quota_week"), "{calls}");
    assert!(!codex_log.exists(), "indeterminate route invoked Codex");
}

#[test]
fn pi_model_switch_updates_identity_but_preserves_indeterminate_quota() {
    let state = tempdir().unwrap();
    let (pi_agent, pi_sessions, session) = install_pi_store(
        state.path(),
        "auth-unsupported-oauth.json",
        "session-switched-xai.jsonl",
        "session-switched-xai",
    );
    let codex_home = write_codex_auth(state.path(), "account-same");
    let inventory = pi_inventory(&session, plugin_quota_tokens());
    let (herdr, herdr_log, codex, codex_log) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);
    assert!(run_pi_event(
        state.path(),
        &herdr,
        &codex,
        &codex_home,
        &pi_agent,
        &pi_sessions,
    )
    .status
    .success());

    let calls = fs::read_to_string(&herdr_log).unwrap_or_default();
    assert_eq!(calls.matches("agent list").count(), 1, "{calls}");
    assert!(!calls.contains("pane read"), "{calls}");
    assert!(calls.contains("pane report-metadata w1:p9"), "{calls}");
    assert!(!calls.contains("pane report-metadata w1:p10"), "{calls}");
    assert!(
        calls.contains("--token quota_provider_model=Grok/grok-4.6"),
        "{calls}"
    );
    assert!(calls.contains("--token quota_5h_danger=10%"), "{calls}");
    assert!(calls.contains("--token quota_week_warning=20%"), "{calls}");
    assert!(!calls.contains("--clear-token quota_5h"), "{calls}");
    assert!(!calls.contains("--clear-token quota_week"), "{calls}");
    assert!(!codex_log.exists(), "switched xAI route invoked Codex");
}

/// Someone with an OpenCode pane but no Go subscription must never cause a
/// request. The stub herdr logs every call, and the plugin has no key to send,
/// so a network attempt would show up as a failure or a hang rather than a
/// clean, silent no-op.
#[test]
fn an_opencode_pane_without_a_go_key_makes_no_request_but_shows_exact_identity() {
    let state = tempdir().unwrap();
    let xdg = state.path().join("xdg-data");
    install_opencode_store(&xdg, "auth-payg.json", "sessions.db");
    let inventory = two_opencode_inventory("ses_go", "{}");
    let (herdr, herdr_log, codex, codex_log) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);

    let output = run_event_binary_with_xdg(
        state.path(),
        &herdr,
        &codex,
        &opencode_working_event("w1:p9"),
        &xdg,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    thread::sleep(Duration::from_millis(200));

    original_four_untouched(state.path(), &codex_log);
    let calls = fs::read_to_string(&herdr_log).unwrap_or_default();
    assert!(!calls.contains("opencode.ai"), "{calls}");
    assert!(
        calls.contains("pane report-metadata w1:p9"),
        "exact OpenCode session stayed blank: {calls}"
    );
    assert!(
        calls.contains("--token quota_provider_model=OpenCode Go/kimi-k2.5"),
        "exact OpenCode identity was not published: {calls}"
    );
    assert!(
        !state
            .path()
            .join("opencode-go.opencode-store.json")
            .exists(),
        "a snapshot was cached without any credential"
    );
}

/// A Go key present but the session on a pay-as-you-go backend must also stay
/// quiet: resolution decides, not the presence of a credential on disk.
#[test]
fn a_payg_session_does_not_fetch_go_usage_even_with_a_key_on_disk() {
    let state = tempdir().unwrap();
    let xdg = state.path().join("xdg-data");
    install_opencode_store(&xdg, "auth-go.json", "sessions.db");
    let inventory = two_opencode_inventory("ses_payg", "{}");
    let (herdr, _herdr_log, codex, codex_log) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);

    assert!(run_event_binary_with_xdg(
        state.path(),
        &herdr,
        &codex,
        &opencode_working_event("w1:p9"),
        &xdg,
    )
    .status
    .success());
    thread::sleep(Duration::from_millis(200));

    original_four_untouched(state.path(), &codex_log);
    assert!(
        !state
            .path()
            .join("opencode-go.opencode-store.json")
            .exists(),
        "a pay-as-you-go session fetched Go usage"
    );
}

/// Reading a pane makes Herdr repaint it, which the user sees as the agent's
/// terminal scrolling. Only the `event` path may ever pay that cost, and only
/// for the one pane the event named. A manual or startup refresh must read
/// nothing at all, no matter how many panes are open.
#[test]
fn a_manual_refresh_reads_no_pane_at_all() {
    let state = tempdir().unwrap();
    let xdg = state.path().join("xdg-data");
    install_opencode_store(&xdg, "auth-go.json", "sessions.db");
    // A mixed inventory: original-four panes plus two OpenCode panes, so every
    // resolution branch runs in the same pass.
    let inventory = two_opencode_inventory("ses_go", "{}");
    let (herdr, herdr_log, codex, _codex_log) =
        install_logged_herdr_and_codex(state.path(), &inventory, None);

    let output = Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"))
        .args(["refresh", "--provider", "all"])
        .env("HERDR_PLUGIN_STATE_DIR", state.path())
        .env("HERDR_BIN_PATH", &herdr)
        .env("CODEX_BIN_PATH", &codex)
        .env("GROK_HOME", state.path().join("missing-grok-home"))
        .env("XDG_DATA_HOME", &xdg)
        .env_remove("GROK_AUTH_FILE")
        .env_remove("OPENCODE_API_KEY")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let calls = fs::read_to_string(&herdr_log).unwrap_or_default();
    assert!(
        !calls.contains("pane read"),
        "manual refresh read a pane: {calls}"
    );
    assert!(
        !calls.contains("recent"),
        "manual refresh used a repainting source: {calls}"
    );
}
