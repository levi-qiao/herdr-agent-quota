use crate::process::{run_shell_with_deadline, CommandOutput, STATUSLINE_COMMAND_BUDGET};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) struct Adapter {
    pub label: &'static str,
    pub subcommand: &'static str,
    pub backup_file: &'static str,
}

impl Adapter {
    pub fn check(&self, path: &Path) -> Result<()> {
        let settings = read_settings(path, self.label)?;
        if self.is_installed(settings.get("statusLine")) {
            println!(
                "{} statusLine collector is installed: {}",
                self.label,
                path.display()
            );
        } else {
            println!(
                "{} statusLine preview for {}: install a reversible, silent quota collector",
                self.label,
                path.display()
            );
        }
        Ok(())
    }

    pub fn apply(&self, path: &Path, state: &Path, executable: &Path) -> Result<()> {
        self.apply_with_refresh_interval(path, state, executable, None)
    }

    pub fn apply_with_refresh_interval(
        &self,
        path: &Path,
        state: &Path,
        executable: &Path,
        refresh_interval_seconds: Option<u64>,
    ) -> Result<()> {
        let mut settings = read_settings(path, self.label)?;
        let installed = self.is_installed(settings.get("statusLine"));
        if !installed && !can_chain_statusline(settings.get("statusLine")) {
            anyhow::bail!(
                "existing {} statusLine has no safely chainable command; refusing to replace it",
                self.label
            );
        }
        fs::create_dir_all(state).context("create plugin state directory")?;
        let backup = state.join(self.backup_file);
        if !backup.exists() {
            let original = if installed {
                self.previous_backup_from_wrapper(settings.get("statusLine"))?
                    .unwrap_or(Value::Null)
            } else {
                settings.get("statusLine").cloned().unwrap_or(Value::Null)
            };
            fs::write(&backup, serde_json::to_vec_pretty(&original)?)
                .with_context(|| format!("write {} statusLine backup", self.label))?;
        }
        let wrapper_command = format!(
            "HERDR_PLUGIN_STATE_DIR={} {} {}",
            shell_quote(state),
            shell_quote(executable),
            self.subcommand
        );
        let status_line = settings
            .get_mut("statusLine")
            .and_then(Value::as_object_mut)
            .map(|object| {
                object.insert("type".to_string(), Value::String("command".to_string()));
                object.insert(
                    "command".to_string(),
                    Value::String(wrapper_command.clone()),
                );
                if let Some(seconds) = refresh_interval_seconds {
                    if installed || !object.contains_key("refreshInterval") {
                        object.insert("refreshInterval".to_string(), Value::from(seconds));
                    }
                }
                Value::Object(object.clone())
            })
            .unwrap_or_else(|| {
                let mut value = json!({"type": "command", "command": wrapper_command});
                if let Some(seconds) = refresh_interval_seconds {
                    value["refreshInterval"] = Value::from(seconds);
                }
                value
            });
        settings["statusLine"] = status_line;
        write_settings(path, &settings, self.label)
    }

    pub fn uninstall(&self, path: &Path, state: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let mut settings = read_settings(path, self.label)?;
        if !self.is_installed(settings.get("statusLine")) {
            return Ok(());
        }
        let backup = state.join(self.backup_file);
        let original: Value = if backup.exists() {
            serde_json::from_slice(&fs::read(&backup)?)?
        } else {
            Value::Null
        };
        if original.is_null() {
            settings
                .as_object_mut()
                .with_context(|| format!("{} settings must be an object", self.label))?
                .remove("statusLine");
        } else {
            settings["statusLine"] = original;
        }
        write_settings(path, &settings, self.label)?;
        if backup.exists() {
            fs::remove_file(backup)
                .with_context(|| format!("remove {} statusLine backup", self.label))?;
        }
        Ok(())
    }

    pub fn previous_command(&self, state: &Path) -> Result<Option<String>> {
        let backup = state.join(self.backup_file);
        if !backup.exists() {
            return Ok(None);
        }
        let value: Value = serde_json::from_slice(&fs::read(backup)?)?;
        Ok(match value {
            Value::String(command) => Some(command),
            Value::Object(map) => map
                .get("command")
                .and_then(Value::as_str)
                .map(str::to_string),
            _ => None,
        })
    }

    pub(crate) fn run_previous(&self, state: &Path, input: &[u8]) -> Result<Option<CommandOutput>> {
        let Some(command) = self.previous_command(state)? else {
            return Ok(None);
        };
        run_shell_with_deadline(&command, input, STATUSLINE_COMMAND_BUDGET).map(Some)
    }

    fn is_installed(&self, status_line: Option<&Value>) -> bool {
        status_line
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str)
            .is_some_and(|command| {
                command.contains(self.subcommand)
                    && (command.contains("herdr-agent-quota")
                        || command.contains("agy-statusline.sh"))
            })
    }

    fn previous_backup_from_wrapper(&self, status_line: Option<&Value>) -> Result<Option<Value>> {
        let Some(command) = status_line
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str)
        else {
            return Ok(None);
        };
        let Some(rest) = command.strip_prefix("HERDR_PLUGIN_STATE_DIR='") else {
            return Ok(None);
        };
        let Some((old_state, _)) = rest.split_once("' ") else {
            return Ok(None);
        };
        let backup = Path::new(old_state).join(self.backup_file);
        if !backup.exists() {
            return Ok(None);
        }
        let value = serde_json::from_slice(&fs::read(backup)?)?;
        Ok(Some(value))
    }
}

fn can_chain_statusline(status_line: Option<&Value>) -> bool {
    match status_line {
        None | Some(Value::Null) | Some(Value::String(_)) => true,
        Some(Value::Object(map)) => map
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| !command.trim().is_empty()),
        Some(_) => false,
    }
}

pub(crate) fn settings_path(environment: &str, relative: &str) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(environment) {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(relative))
}

fn read_settings(path: &Path, label: &str) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let value: Value =
        serde_json::from_slice(&fs::read(path).with_context(|| format!("read {label} settings"))?)
            .with_context(|| format!("parse {label} settings"))?;
    if !value.is_object() {
        anyhow::bail!("{label} settings must be a JSON object")
    }
    Ok(value)
}

fn write_settings(path: &Path, settings: &Value, label: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {label} settings directory"))?;
    }
    let temporary = path.with_extension("json.herdr-agent-quota.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(settings)?)?;
    fs::rename(temporary, path).with_context(|| format!("replace {label} settings"))
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}
