#![cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn normal_upgrade_preserves_preferences_and_runs_recovery_in_the_server_environment() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let config = dir.path().join("config");
    let plugin = dir.path().join("plugin/target/release");
    for path in [&bin, &config, &plugin] {
        fs::create_dir_all(path).unwrap();
    }
    fs::write(config.join("agents"), "codex,omp\n").unwrap();
    fs::write(config.join("sidebar-layout"), "stacked\n").unwrap();
    fs::write(config.join("watch-interval-seconds"), "300\n").unwrap();
    let log = dir.path().join("calls");
    let manifest: toml::Value = toml::from_str(include_str!("../herdr-plugin.toml")).unwrap();
    let configure = manifest["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["id"].as_str() == Some("configure"))
        .unwrap()["command"][2]
        .as_str()
        .unwrap();
    for (path, script) in [
        (bin.join("cargo"), "#!/bin/sh\nexit 0\n"),
        (
            plugin.join("herdr-agent-quota"),
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TEST_LOG\"\n",
        ),
        (
            bin.join("herdr"),
            r#"#!/bin/sh
case "$1 $2" in
  'plugin link') exit 0 ;;
  'plugin config-dir') printf '%s\n' "$TEST_CONFIG" ;;
  'plugin action')
    HERDR_PLUGIN_ROOT="$TEST_PLUGIN" sh -c "$TEST_ACTION" || exit 1
    printf '%s\n' '{"log_id":"upgrade-test","status":"succeeded"}' ;;
  'plugin log') printf '%s\n' '{"log_id":"upgrade-test","status":"succeeded"}' ;;
  'server reload-config') printf '%s\n' reload-config >> "$TEST_LOG" ;;
  *) exit 2 ;;
esac
"#,
        ),
    ] {
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = Command::new("bash")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/install.sh"))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("TEST_CONFIG", &config)
        .env("TEST_PLUGIN", dir.path().join("plugin"))
        .env("TEST_ACTION", configure)
        .env("TEST_LOG", &log)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(log).unwrap(),
        "configure --apply\nreload-config\nstartup --provider all\n"
    );
    assert_eq!(
        fs::read_to_string(config.join("agents")).unwrap(),
        "codex,omp\n"
    );
    assert_eq!(
        fs::read_to_string(config.join("sidebar-layout")).unwrap(),
        "stacked\n"
    );
    assert_eq!(
        fs::read_to_string(config.join("watch-interval-seconds")).unwrap(),
        "300\n"
    );
}

#[test]
fn an_explicit_install_agent_list_is_stored_as_a_subset() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let config = dir.path().join("config");
    let plugin = dir.path().join("plugin/target/release");
    for path in [&bin, &config, &plugin] {
        fs::create_dir_all(path).unwrap();
    }
    let log = dir.path().join("calls");
    let manifest: toml::Value = toml::from_str(include_str!("../herdr-plugin.toml")).unwrap();
    let configure = manifest["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["id"].as_str() == Some("configure"))
        .unwrap()["command"][2]
        .as_str()
        .unwrap();
    for (path, script) in [
        (bin.join("cargo"), "#!/bin/sh\nexit 0\n"),
        (
            plugin.join("herdr-agent-quota"),
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TEST_LOG\"\n",
        ),
        (
            bin.join("herdr"),
            r#"#!/bin/sh
case "$1 $2" in
  'plugin link') exit 0 ;;
  'plugin config-dir') printf '%s\n' "$TEST_CONFIG" ;;
  'plugin action')
    HERDR_PLUGIN_ROOT="$TEST_PLUGIN" sh -c "$TEST_ACTION" || exit 1
    printf '%s\n' '{"log_id":"upgrade-test","status":"succeeded"}' ;;
  'plugin log') printf '%s\n' '{"log_id":"upgrade-test","status":"succeeded"}' ;;
  'server reload-config') printf '%s\n' reload-config >> "$TEST_LOG" ;;
  *) exit 2 ;;
esac
"#,
        ),
    ] {
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = Command::new("bash")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/install.sh"))
        .args(["--agent", "claude,codex,grok,agy,opencode,pi,omp,devin"])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("TEST_CONFIG", &config)
        .env("TEST_PLUGIN", dir.path().join("plugin"))
        .env("TEST_ACTION", configure)
        .env("TEST_LOG", &log)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(config.join("agents")).unwrap(),
        "only,claude,codex,grok,agy,opencode,pi,omp,devin\n"
    );
}
