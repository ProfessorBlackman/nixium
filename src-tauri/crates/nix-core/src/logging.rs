// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Structured logging and the diagnostics bundle. Task 0.8 (`FND-8`).
//!
//! Stacer wrote a complete file logger and then never called `qInstallMessageHandler`, so every
//! `qCritical()` in that codebase went to a stderr nobody read and `stacer.log` was never created.
//! It therefore shipped with no error reporting at all. [`is_initialised`] exists so a test can
//! assert we have not repeated that.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::caps;
use crate::error::Result;
use crate::paths;
use crate::settings::LogLevel;

static INITIALISED: AtomicBool = AtomicBool::new(false);

/// Whether logging has been installed in this process.
///
/// Guards against the Stacer failure of writing a logger and never wiring it up.
#[must_use]
pub fn is_initialised() -> bool {
    INITIALISED.load(Ordering::Relaxed)
}

/// Where the log file lives, if a state directory is resolvable.
#[must_use]
pub fn log_dir() -> Option<PathBuf> {
    paths::state_dir().map(|d| d.join("logs"))
}

/// Handle that keeps the non-blocking writer alive. Dropping it stops file logging, so the caller
/// must hold it for the life of the process.
pub struct Guard {
    _appender: Option<tracing_appender::non_blocking::WorkerGuard>,
}

/// Install logging: a rolling daily file under the state directory, plus stderr.
///
/// Idempotent. Never fails the caller — if the log directory cannot be created we still install a
/// stderr subscriber and report the problem through the returned error, because a missing log file
/// must not prevent the app from starting.
pub fn init(level: LogLevel) -> (Guard, Option<crate::error::AppError>) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, fmt};

    if INITIALISED.swap(true, Ordering::SeqCst) {
        return (Guard { _appender: None }, None);
    }

    // `NIX_LOG` overrides the persisted preference, for debugging a build in place.
    let filter = EnvFilter::try_from_env("NIX_LOG").unwrap_or_else(|_| {
        EnvFilter::new(format!(
            "nix_core={0},nix_app={0},nix_helper={0}",
            level.as_filter()
        ))
    });

    let mut problem = None;
    let mut file_layer = None;
    let mut guard = None;

    if let Some(dir) = log_dir() {
        match std::fs::create_dir_all(&dir) {
            Ok(()) => {
                let appender = tracing_appender::rolling::daily(&dir, "nix.log");
                let (writer, g) = tracing_appender::non_blocking(appender);
                guard = Some(g);
                file_layer = Some(
                    fmt::layer()
                        .with_writer(writer)
                        .with_ansi(false)
                        .with_target(true),
                );
            }
            Err(e) => {
                problem = Some(
                    crate::error::AppError::from_io(&e, "create the log directory")
                        .with_path(&dir)
                        .with_remedy("nix will keep running; logs go to standard error only."),
                );
            }
        }
    }

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stderr).with_target(true))
        .with(file_layer)
        .init();

    tracing::info!(
        version = crate::VERSION,
        log_dir = ?log_dir(),
        "logging initialised"
    );

    (Guard { _appender: guard }, problem)
}

/// Everything useful in a bug report, collected in one place.
///
/// Deliberately contains no file contents and no paths outside nix's own directories, so it is safe
/// to copy into a public issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Diagnostics {
    pub core_version: String,
    pub kernel: Option<String>,
    pub capabilities: caps::Capabilities,
    pub log_dir: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub logging_initialised: bool,
}

/// Collect the diagnostics bundle.
pub fn diagnostics() -> Result<Diagnostics> {
    Ok(Diagnostics {
        core_version: crate::VERSION.to_string(),
        kernel: std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .map(|s| s.trim().to_string()),
        capabilities: caps::registry().snapshot(),
        log_dir: log_dir(),
        config_dir: paths::config_dir(),
        state_dir: paths::state_dir(),
        logging_initialised: is_initialised(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_are_collectable_without_logging_installed() {
        let d = diagnostics().unwrap();
        assert!(!d.core_version.is_empty());
        // Present on Linux; absent elsewhere, and absence must not fail collection.
        if cfg!(target_os = "linux") {
            assert!(
                d.kernel.is_some(),
                "kernel release should be readable on Linux"
            );
        }
    }

    #[test]
    fn diagnostics_round_trip() {
        let d = diagnostics().unwrap();
        let json = serde_json::to_string(&d).unwrap();
        let back: Diagnostics = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn log_levels_map_to_filter_strings() {
        assert_eq!(LogLevel::Trace.as_filter(), "trace");
        assert_eq!(LogLevel::Error.as_filter(), "error");
    }
}
