#![cfg(unix)]

use herdr_agent_quota::cli::AgentSelection;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn watcher_inherits_current_server_environment_after_handoff() {
    assert_watcher_adopts_environment("startup");
}

#[test]
fn refresh_repairs_a_watcher_when_plugin_enable_does_not_run_startup() {
    assert_watcher_adopts_environment("refresh");
}

fn assert_watcher_adopts_environment(entrypoint: &str) {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("herdr-old");
    let current = dir.path().join("herdr-current");
    fs::write(
        &old,
        "#!/bin/sh\nprintf '%s\\n' 'protocol_mismatch' >&2\nexit 1\n",
    )
    .unwrap();
    fs::write(
        &current,
        r#"#!/bin/sh
[ "$HERDR_SOCKET_PATH" = "$TEST_CURRENT_SOCKET" ] || exit 2
printf '%s\n' "$*" >> "$TEST_LOG"
printf '%s\n' '{"result":{"agents":[]}}'
"#,
    )
    .unwrap();
    for path in [&old, &current] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let log = dir.path().join("calls");
    let socket = dir.path().join("current.sock");
    let command = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"));
        cmd.env("HERDR_PLUGIN_STATE_DIR", dir.path())
            .env("HERDR_PLUGIN_CONFIG_DIR", dir.path())
            .env("HERDR_AGENT_QUOTA_AGENT_ORDER", "default")
            .env("TEST_CURRENT_SOCKET", &socket)
            .env("TEST_LOG", &log)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd
    };
    assert!(command()
        .args([entrypoint, "--provider", "agy"])
        .env("HERDR_BIN_PATH", &current)
        .env("HERDR_SOCKET_PATH", &socket)
        .status()
        .unwrap()
        .success());
    // Startup may launch its own short-lived watcher; wait for its lease.
    let cache = herdr_agent_quota::cache::CacheStore::new(dir.path());
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let expected = if entrypoint == "startup" { 2 } else { 1 };
        if fs::read_to_string(&log).unwrap().lines().count() >= expected
            && cache.try_lock_named("turn.lock").unwrap().is_some()
        {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    let before = fs::read_to_string(&log).unwrap().lines().count();
    let mut child = command()
        .args(["watch", "--provider", "agy"])
        .env("HERDR_BIN_PATH", &old)
        .env("HERDR_SOCKET_PATH", dir.path().join("old.sock"))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(status.is_some_and(|status| status.success()),
        "watcher kept polling the old incompatible Herdr client instead of adopting startup's environment");
    assert!(fs::read_to_string(log).unwrap().lines().count() > before);
}

#[test]
fn every_supported_working_harness_keeps_the_watcher_alive() {
    for harness in AgentSelection::SUPPORTED {
        let harness = AgentSelection::harness_name(harness);
        let dir = tempfile::tempdir().unwrap();
        let herdr = dir.path().join("herdr");
        fs::write(&herdr, format!("#!/bin/sh\ntouch \"$TEST_INVENTORY\"\nprintf '%s\\n' '{{\"result\":{{\"agents\":[{{\"pane_id\":\"w1:p1\",\"agent\":\"{harness}\",\"agent_status\":\"working\"}}]}}}}'\n")).unwrap();
        fs::set_permissions(&herdr, fs::Permissions::from_mode(0o755)).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"))
            .args(["watch", "--provider", "all"])
            .env("HERDR_PLUGIN_STATE_DIR", dir.path())
            .env("HERDR_BIN_PATH", herdr)
            .env("TEST_INVENTORY", dir.path().join("inventory"))
            .env("CODEX_BIN_PATH", dir.path().join("absent"))
            .env("GROK_AUTH_FILE", dir.path().join("absent"))
            .env("DEVIN_CREDENTIALS_FILE", dir.path().join("absent"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dir.path().join("inventory").exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(200));
        let premature = child.try_wait().unwrap();
        if premature.is_none() {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        assert!(
            premature.is_none(),
            "watcher exited while {harness} was working"
        );
    }
}

#[test]
fn partial_install_startup_and_events_ignore_unselected_working_agents() {
    let dir = tempfile::tempdir().unwrap();
    let herdr = dir.path().join("herdr");
    let log = dir.path().join("calls");
    fs::write(dir.path().join("agents"), "agy\n").unwrap();
    fs::write(
        &herdr,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$TEST_LOG"
printf '%s\n' '{"result":{"agents":[{"pane_id":"w1:p1","agent":"codex","agent_status":"working"}]}}'
"#,
    )
    .unwrap();
    fs::set_permissions(&herdr, fs::Permissions::from_mode(0o755)).unwrap();
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_herdr-agent-quota"));
        command
            .env("HERDR_PLUGIN_STATE_DIR", dir.path())
            .env("HERDR_PLUGIN_CONFIG_DIR", dir.path())
            .env_remove("HERDR_AGENT_QUOTA_AGENTS")
            .env("HERDR_AGENT_QUOTA_AGENT_ORDER", "default")
            .env("HERDR_BIN_PATH", &herdr)
            .env("CODEX_BIN_PATH", dir.path().join("absent"))
            .env("TEST_LOG", &log)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    };
    assert!(command()
        .args(["startup", "--provider", "all"])
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read_to_string(&log).unwrap().lines().count() < 2 {
        assert!(
            Instant::now() < deadline,
            "startup watcher did not inspect inventory"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(command()
        .arg("event")
        .env(
            "HERDR_PLUGIN_EVENT_JSON",
            r#"{"pane_id":"w1:p1","agent":"codex","status":"working"}"#
        )
        .status()
        .unwrap()
        .success());
    assert!(fs::read_to_string(&log)
        .unwrap()
        .lines()
        .all(|line| line == "agent list"));
    let cache = herdr_agent_quota::cache::CacheStore::new(dir.path());
    assert!(!cache
        .should_debounce(
            herdr_agent_quota::model::Provider::Codex,
            herdr_agent_quota::cache::CacheStore::now_unix(),
            60
        )
        .unwrap());
}
