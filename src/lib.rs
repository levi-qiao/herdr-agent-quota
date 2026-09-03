pub mod cache;
pub mod cli;
pub mod model;
pub mod prefs;
pub mod presentation;
pub mod process;
pub mod process_group;

pub mod configure;
pub mod dashboard;
pub mod herdr;
pub mod omp;
pub mod opencode;
pub mod pi;
pub mod providers;
pub mod refresh;
pub mod route;
pub mod settings;

use anyhow::{Context, Result};
use directories::BaseDirs;
use std::path::PathBuf;

/// Get the user's home directory in a cross-platform way
///
/// Works on Windows (USERPROFILE), macOS, and Linux (HOME)
pub fn home_dir() -> Result<PathBuf> {
    BaseDirs::new()
        .context("Cannot determine home directory")?
        .home_dir()
        .to_path_buf()
        .pipe(Ok)
}

trait Pipe {
    fn pipe<F, R>(self, f: F) -> R
    where
        F: FnOnce(Self) -> R,
        Self: Sized,
    {
        f(self)
    }
}

impl<T> Pipe for T {}
