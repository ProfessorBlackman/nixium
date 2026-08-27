// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Persisted preferences. Task 0.6 (`FND-5`).
//!
//! Three properties the spec demands, each answering a specific Stacer failure:
//!
//! - **Versioned**, so a future format is detected rather than misread.
//! - **Atomically written**, so a crash mid-save cannot truncate the file.
//! - **Keyed by stable machine identifiers, never localised strings.** Stacer persisted its start
//!   page as a *translated* page title, so changing language silently orphaned the preference.
//!
//! A file that fails to load falls back to defaults and reports why, rather than failing startup.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, Cause, ErrorCode, IoContext, Result};
use crate::paths;

/// Format version. Bump when a change is not backward-compatible, and add a migration.
pub const CURRENT_VERSION: u32 = 1;

/// Which view opens on launch. A stable identifier — deliberately not a display name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum StartView {
    #[default]
    Overview,
    Explorer,
    Reclaim,
    Settings,
}

/// Colour scheme preference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Theme {
    /// Follow the desktop's preference.
    #[default]
    System,
    Light,
    Dark,
}

/// Verbosity of the log file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    #[must_use]
    pub const fn as_filter(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

/// Everything nix remembers between runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct Settings {
    /// Format version of the file this was loaded from.
    pub version: u32,
    pub theme: Theme,
    /// Stable identifier, not a display name.
    pub start_view: StartView,
    pub log_level: LogLevel,
    /// Paths the scanner and executor must never touch, in addition to the built-in set.
    pub protected_paths: Vec<PathBuf>,
    /// Show pseudo-filesystems (tmpfs, squashfs, overlay) in the filesystem list.
    pub show_pseudo_filesystems: bool,
    /// Opt-in periodic collection of category totals (`STO-16`). Off by default: it installs a
    /// systemd user timer, which is a change to the user's system.
    pub growth_history_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            theme: Theme::default(),
            start_view: StartView::default(),
            log_level: LogLevel::default(),
            protected_paths: Vec::new(),
            show_pseudo_filesystems: false,
            growth_history_enabled: false,
        }
    }
}

/// Outcome of a load, so the caller can tell "fresh defaults" from "recovered from a bad file".
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub settings: Settings,
    /// Present when defaults were substituted; worth surfacing in the notification centre.
    pub warning: Option<AppError>,
}

/// Reads and writes [`Settings`] at a known path.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// A store at an explicit path. Used by tests, and by anything that needs a non-default
    /// location.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The store at `$XDG_CONFIG_HOME/nix/settings.json`.
    pub fn discover() -> Result<Self> {
        let dir = paths::config_dir().ok_or_else(|| {
            AppError::new(
                ErrorCode::Unsupported,
                "Could not work out where to keep settings.",
            )
            .with_remedy("Set HOME or XDG_CONFIG_HOME and try again.")
        })?;
        Ok(Self::at(dir.join("settings.json")))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load, falling back to defaults with a warning rather than failing.
    ///
    /// A missing file is not a warning — it is the normal first run.
    pub fn load(&self) -> Loaded {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Loaded {
                    settings: Settings::default(),
                    warning: None,
                };
            }
            Err(e) => {
                return Loaded {
                    settings: Settings::default(),
                    warning: Some(
                        AppError::from_io(&e, "read your settings")
                            .with_path(&self.path)
                            .with_remedy(
                                "Defaults are in use. Your saved settings were not changed.",
                            ),
                    ),
                };
            }
        };

        match serde_json::from_str::<Settings>(&raw) {
            Ok(s) if s.version > CURRENT_VERSION => Loaded {
                settings: Settings::default(),
                warning: Some(
                    AppError::new(
                        ErrorCode::VersionMismatch,
                        format!(
                            "Your settings were written by a newer version of nix (format {}, this build understands {CURRENT_VERSION}).",
                            s.version
                        ),
                    )
                    .with_path(&self.path)
                    .with_remedy("Defaults are in use. Update nix, or remove the file to start fresh."),
                ),
            },
            Ok(s) => Loaded {
                settings: s,
                warning: None,
            },
            Err(e) => Loaded {
                settings: Settings::default(),
                warning: Some(
                    AppError::new(ErrorCode::Parse, "Your settings file could not be read.")
                        .with_path(&self.path)
                        .with_cause(Cause::Malformed {
                            source: self.path.display().to_string(),
                            detail: e.to_string(),
                        })
                        .with_remedy("Defaults are in use. Fix or delete the file to clear this."),
                ),
            },
        }
    }

    /// Save atomically: write a sibling temporary file, fsync it, then rename over the target.
    ///
    /// The temporary lives in the *same directory* so the rename cannot cross a filesystem — which
    /// is exactly the mistake Stacer's hosts editor made by staging in `/tmp` and moving to `/etc`,
    /// turning an atomic replace into a copy and opening a symlink race.
    pub fn save(&self, settings: &Settings) -> Result<()> {
        let dir = self.path.parent().ok_or_else(|| {
            AppError::internal("Settings path has no parent directory.").with_path(&self.path)
        })?;
        std::fs::create_dir_all(dir)
            .doing("create the settings directory")
            .map_err(|e| e.with_path(dir))?;

        let mut to_write = settings.clone();
        to_write.version = CURRENT_VERSION;
        let json = serde_json::to_string_pretty(&to_write).map_err(|e| {
            AppError::internal("Could not encode settings.").with_cause(Cause::Other {
                detail: e.to_string(),
            })
        })?;

        let tmp = self
            .path
            .with_extension(format!("json.tmp.{}", std::process::id()));
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)
                .doing("write your settings")
                .map_err(|e| e.with_path(&tmp))?;
            f.write_all(json.as_bytes())
                .doing("write your settings")
                .map_err(|e| e.with_path(&tmp))?;
            f.write_all(b"\n")
                .doing("write your settings")
                .map_err(|e| e.with_path(&tmp))?;
            // Durability before the rename, so a crash cannot leave an empty file in place.
            f.sync_all()
                .doing("write your settings")
                .map_err(|e| e.with_path(&tmp))?;
        }

        std::fs::rename(&tmp, &self.path).map_err(|e| {
            // Leave no debris behind if the rename failed.
            std::fs::remove_file(&tmp).ok();
            AppError::from_io(&e, "save your settings").with_path(&self.path)
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nix-settings-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_file_is_a_normal_first_run() {
        let store = Store::at(tmpdir("missing").join("settings.json"));
        let loaded = store.load();
        assert_eq!(loaded.settings, Settings::default());
        assert!(loaded.warning.is_none(), "a first run must not warn");
    }

    #[test]
    fn round_trips_through_disk() {
        let store = Store::at(tmpdir("round").join("settings.json"));
        let s = Settings {
            theme: Theme::Dark,
            start_view: StartView::Explorer,
            protected_paths: vec![PathBuf::from("/srv/important")],
            growth_history_enabled: true,
            ..Settings::default()
        };
        store.save(&s).unwrap();

        let loaded = store.load();
        assert!(loaded.warning.is_none());
        assert_eq!(loaded.settings, s);
    }

    #[test]
    fn save_creates_the_directory_and_leaves_no_temp_files() {
        let dir = tmpdir("mkdir").join("nested/deeper");
        let store = Store::at(dir.join("settings.json"));
        store.save(&Settings::default()).unwrap();
        assert!(store.path().is_file());

        let strays: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(strays.is_empty(), "temporary files left behind: {strays:?}");
    }

    #[test]
    fn malformed_file_yields_defaults_and_a_warning() {
        let dir = tmpdir("malformed");
        let path = dir.join("settings.json");
        std::fs::write(&path, b"{ this is not json").unwrap();

        let loaded = Store::at(&path).load();
        assert_eq!(loaded.settings, Settings::default());
        let w = loaded.warning.expect("a malformed file must warn");
        assert_eq!(w.code, ErrorCode::Parse);
        assert!(
            w.remedy.is_some(),
            "a warning without a remedy is not actionable"
        );
    }

    #[test]
    fn future_version_is_refused_rather_than_misread() {
        let dir = tmpdir("future");
        let path = dir.join("settings.json");
        std::fs::write(&path, br#"{"version": 9999, "theme": "dark"}"#).unwrap();

        let loaded = Store::at(&path).load();
        assert_eq!(
            loaded.settings,
            Settings::default(),
            "must not adopt values from a newer format"
        );
        let w = loaded.warning.expect("a future format must warn");
        assert_eq!(w.code, ErrorCode::VersionMismatch);
    }

    #[test]
    fn partial_file_fills_in_defaults() {
        let dir = tmpdir("partial");
        let path = dir.join("settings.json");
        std::fs::write(&path, br#"{"version": 1, "theme": "light"}"#).unwrap();

        let loaded = Store::at(&path).load();
        assert!(
            loaded.warning.is_none(),
            "missing optional keys are not an error"
        );
        assert_eq!(loaded.settings.theme, Theme::Light);
        assert_eq!(loaded.settings.start_view, StartView::default());
    }

    #[test]
    fn save_stamps_the_current_version_even_if_told_otherwise() {
        let dir = tmpdir("stamp");
        let store = Store::at(dir.join("settings.json"));
        let s = Settings {
            version: 0,
            ..Settings::default()
        };
        store.save(&s).unwrap();
        assert_eq!(store.load().settings.version, CURRENT_VERSION);
    }

    /// Guards the Stacer bug directly: no persisted key may hold a human-readable display name.
    #[test]
    fn start_view_serialises_as_a_stable_identifier() {
        let json = serde_json::to_string(&StartView::Explorer).unwrap();
        assert_eq!(json, "\"explorer\"");
    }
}
