use herdr_agent_quota::configure::herdr::{add_quota_row, remove_quota_row};
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
            "#!/bin/sh\nif [ \"$1 $2\" = \"agent list\" ]; then\n  printf '%s\\n' '{}'\nelif [ \"$1 $2\" = \"pane read\" ]; then\n  printf '%s\\n' \"$*\" >> '{}'\nelif [ \"$1 $2\" = \"pane report-metadata\" ]; then\n  printf '%s\\n' \"$*\" >> '{}'\nfi\n",
            agent_list,
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
fn default_herdr_rows_become_plane_provider_usage_and_topic_lines() {
    let original = concat!(
        "[ui.sidebar.agents]\n",
        "rows = [[\"state_icon\", \"workspace\", \"tab\"], [\"agent\"]]\n"
    );
    let applied = add_quota_row(original).unwrap();
    assert!(applied.contains("$quota_provider_model"));
    assert!(applied.contains("bold = true"));
    assert!(applied.contains("$quota_5h_normal"));
    assert!(applied.contains("$quota_5h_warning"));
    assert!(applied.contains("$quota_5h_danger"));
    assert!(applied.contains("$quota_week_normal"));
    assert!(applied.contains("$quota_week_warning"));
    assert!(applied.contains("$quota_week_danger"));
    assert!(applied.contains("$quota_week_inline_normal"));
    assert!(applied.contains("$quota_week_inline_warning"));
    assert!(applied.contains("$quota_week_inline_danger"));
    assert!(!applied.contains("[\"$quota_summary\"]"));
    assert!(applied.contains("$quota_topic"));
    assert!(applied.contains("$quota_context"));
    assert!(applied.contains("$quota_provider_model"));
    assert!(applied.contains("fg = \"#9aa7b8\""));
    assert!(applied.find("$quota_provider_model").unwrap() < applied.find("$quota_topic").unwrap());
    assert!(applied.contains("$quota_cache"));
    assert!(applied.contains("$quota_cache_ttl"));
    assert!(applied.contains("fg = \"#9aa7b8\""));
    assert!(applied.contains("row_gap = 1 # herdr-agent-quota"));
    assert!(applied.find("$quota_topic").unwrap() < applied.find("$quota_5h_normal").unwrap());
    assert!(applied.contains("fg = \"#84b084\""));
    assert!(applied.contains("fg = \"#cdaa65\""));
    assert!(applied.contains("fg = \"#ca6470\""));
    assert!(applied.contains("[ui.sidebar.agents.rows_by_agent]"));
    assert!(applied.contains("fg = \"#c47f6a\""));
    assert!(applied.contains("fg = \"#7998b7\""));
    assert!(applied.contains("fg = \"#acb4c3\""));
    assert!(applied.contains("fg = \"#84b0af\""));
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

    let claude_rows = agents["rows_by_agent"]["claude"].as_value().unwrap();
    let rendered = claude_rows.to_string();
    assert_eq!(rendered.matches("fg = \"#c47f6a\"").count(), 1);
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

    for provider in ["claude", "codex", "grok", "agy", "opencode"] {
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
    assert!(report.contains("quota_5h=5h 42%"));
    assert!(report.contains("quota_week=7d 73%"));
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
    assert!(report.contains("quota_week=7d 72%"));
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
    assert!(work_report.contains("quota_5h=5h 82%"), "{work_report}");
    assert!(work_report.contains("quota_week=7d 90%"), "{work_report}");
    assert!(
        personal_report.contains("quota_5h=5h 18%"),
        "{personal_report}"
    );
    assert!(
        personal_report.contains("quota_week=7d 10%"),
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
    fs::create_dir_all(&dir).unwrap();
    fs::copy(opencode_fixture(db_name), dir.join("opencode.db")).unwrap();
    fs::copy(opencode_fixture(auth_name), dir.join("auth.json")).unwrap();
}

fn plugin_quota_tokens() -> &'static str {
    r#"{"quota_state":"?","quota_provider":"Claude","quota_provider_model":"Claude","quota_5h":"5h 10%","quota_week":"7d 20%","quota_summary":"5h 10% · 7d 20%"}"#
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
fn opencode_working_event_does_not_refresh_any_collector() {
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
    // Resolved as OpenCode Go, but no collector exists yet, so the pane keeps
    // whatever metadata it already had instead of taking a write.
    assert!(!calls.contains("pane report-metadata w1:p9"), "{calls}");
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

#[test]
fn claude_collector_does_not_republish_unchanged_quota() {
    let state = tempdir().unwrap();
    let (herdr_stub, herdr_log) = install_herdr_stub(
        state.path(),
        r#"{"result":{"agents":[{"agent":"claude","pane_id":"w1:p1","tokens":{"quota_state":"?","quota_provider":"Claude","quota_provider_model":"Claude","quota_5h":"5h 42%","quota_5h_warning":"5h 42%","quota_week":"7d 73%","quota_week_warning":"7d 73%","quota_summary":"5h 42% · 7d 73%"}}]}}"#,
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
fn opencode_go_event_is_named_pane_only_and_repeatable() {
    let state = tempdir().unwrap();
    let xdg = state.path().join("xdg-data");
    install_opencode_store(&xdg, "auth-go.json", "sessions.db");
    let inventory = two_opencode_inventory("ses_go", "{}");
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
    for other in ["codex =", "grok =", "agy =", "opencode ="] {
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
    for kept in ["claude =", "codex =", "agy =", "opencode ="] {
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
    for expected in ["claude =", "codex =", "grok =", "agy =", "opencode ="] {
        assert!(sidebar.contains(expected), "{expected} missing: {sidebar}");
    }
}

/// Someone with an OpenCode pane but no Go subscription must never cause a
/// request. The stub herdr logs every call, and the plugin has no key to send,
/// so a network attempt would show up as a failure or a hang rather than a
/// clean, silent no-op.
#[test]
fn an_opencode_pane_without_a_go_key_makes_no_request_and_no_metadata_write() {
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
