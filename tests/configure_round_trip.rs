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

    for provider in ["claude", "codex", "grok", "agy"] {
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
    // their own statusLine ticks. Before session-scoped windows, the second
    // account's tick clobbered a single provider-wide snapshot, so both panes
    // showed whichever account reported most recently.
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
    assert!(work_report.contains("quota_5h=5h 82%"));
    assert!(work_report.contains("quota_week=7d 90%"));
    assert!(personal_report.contains("quota_5h=5h 18%"));
    assert!(personal_report.contains("quota_week=7d 10%"));
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
