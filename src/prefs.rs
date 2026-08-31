//! Installer choices persisted in Herdr's plugin config directory.
//!
//! Herdr runs a plugin action with a fixed command line, in the **server's**
//! environment: variables exported around `herdr plugin action invoke` do not
//! reach the action at all (only Herdr's own `HERDR_PLUGIN_*` vars are
//! injected). These small files are therefore the only channel an installer
//! has for passing a choice to `configure`, and the only reason a later
//! "Install / repair" action can keep the options the user installed with.
//!
//! Every reader treats an absent, empty, or unparsable file as "not set" and
//! falls back to its default: a corrupt preference must never stop a repair
//! the user explicitly asked for.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Comma-separated agent selection, matching `configure --agent`.
pub const AGENTS: &str = "agents";
/// Active-turn poll interval, in seconds.
pub const WATCH_INTERVAL_SECONDS: &str = "watch-interval-seconds";
pub const SIDEBAR_LAYOUT: &str = "sidebar-layout";
pub const ROW_GAP: &str = "row-gap";

/// Every preference a full uninstall must forget.
pub const ALL: [&str; 4] = [AGENTS, WATCH_INTERVAL_SECONDS, SIDEBAR_LAYOUT, ROW_GAP];

fn directory() -> Option<PathBuf> {
    std::env::var_os("HERDR_PLUGIN_CONFIG_DIR").map(PathBuf::from)
}

pub fn read(name: &str) -> Option<String> {
    let value = std::fs::read_to_string(directory()?.join(name)).ok()?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Writing is a no-op outside Herdr, so a direct CLI run still works.
pub fn write(name: &str, value: &str) -> Result<()> {
    let Some(directory) = directory() else {
        return Ok(());
    };
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("create plugin config directory {}", directory.display()))?;
    std::fs::write(directory.join(name), value)
        .with_context(|| format!("write plugin preference {name}"))
}

pub fn clear(name: &str) -> Result<()> {
    let Some(directory) = directory() else {
        return Ok(());
    };
    match std::fs::remove_file(directory.join(name)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove plugin preference {name}")),
    }
}

/// Scoped overrides of the process-global config directory.
///
/// `HERDR_PLUGIN_CONFIG_DIR` is read from the environment, and Rust runs unit
/// tests as threads of one process, so every test that touches it has to share
/// a single lock. This is that one lock — do not add another.
#[cfg(test)]
pub(crate) mod testing {
    use std::path::Path;
    use std::sync::{Mutex, MutexGuard};

    static LOCK: Mutex<()> = Mutex::new(());

    fn guard() -> MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Run `body` with the config directory pointed at `path`.
    pub(crate) fn with_config_dir(path: &Path, body: impl FnOnce()) {
        swap(Some(path), body);
    }

    /// Run `body` as a direct CLI invocation, with no Herdr config directory.
    pub(crate) fn without_config_dir(body: impl FnOnce()) {
        swap(None, body);
    }

    fn swap(path: Option<&Path>, body: impl FnOnce()) {
        let _guard = guard();
        let previous = std::env::var_os("HERDR_PLUGIN_CONFIG_DIR");
        // SAFETY: every writer of this variable holds `LOCK`, and the previous
        // value is restored before the guard is released.
        unsafe {
            match path {
                Some(path) => std::env::set_var("HERDR_PLUGIN_CONFIG_DIR", path),
                None => std::env::remove_var("HERDR_PLUGIN_CONFIG_DIR"),
            }
        }
        body();
        unsafe {
            match previous {
                Some(value) => std::env::set_var("HERDR_PLUGIN_CONFIG_DIR", value),
                None => std::env::remove_var("HERDR_PLUGIN_CONFIG_DIR"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_config_directory_makes_every_operation_a_no_op() {
        // Direct CLI runs have no Herdr config directory; they must not fail.
        testing::without_config_dir(|| {
            assert_eq!(read(AGENTS), None);
            assert!(write(AGENTS, "grok").is_ok());
            assert!(clear(AGENTS).is_ok());
        });
    }

    #[test]
    fn a_blank_preference_reads_as_unset_rather_than_an_empty_selection() {
        let directory = tempfile::tempdir().unwrap();
        testing::with_config_dir(directory.path(), || {
            write(AGENTS, "  \n ").unwrap();
            assert_eq!(read(AGENTS), None);
            write(AGENTS, " grok,claude \n").unwrap();
            assert_eq!(read(AGENTS).as_deref(), Some("grok,claude"));
            clear(AGENTS).unwrap();
            assert_eq!(read(AGENTS), None);
        });
    }
}
