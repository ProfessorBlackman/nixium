// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Persistence for measured package sizes. `PKG-1`.
//!
//! # Why measurements are cached and inventory is not
//!
//! Listing every installed package costs 71 ms — cheap enough to do on every visit, so it is never
//! stale. Measuring one costs a walk of its file list, which for a theme package is thirty thousand
//! `stat` calls, and the answer does not change until the package does. So the inventory is always
//! live and only the measurements are kept.
//!
//! # Keyed by version, which is what makes staleness impossible rather than unlikely
//!
//! The key is manager, identity **and version**. An upgrade changes the version, so the old entry is
//! simply never looked up again — there is no invalidation step that could be forgotten, and no
//! window in which a cached figure describes a package that has since been replaced. Compare the scan
//! cache, which serves deliberately stale data under a rule that forbids acting on it (D6); this one
//! cannot be stale at all, so it needs no such rule.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::Measured;
use crate::error::{AppError, Cause, ErrorCode, IoContext, Result};
use crate::paths;
use crate::space::Manager;

/// Bumped when the stored shape changes; a mismatch discards rather than misreads.
const STORE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    version: u32,
    #[serde(default)]
    entries: HashMap<String, Measured>,
}

/// Measured sizes, keyed so that an upgrade can never return an old figure.
#[derive(Debug, Clone)]
pub struct MeasureStore {
    path: PathBuf,
    entries: HashMap<String, Measured>,
}

impl MeasureStore {
    /// A store backed by an explicit file, loading it if it is there.
    ///
    /// An unreadable or unparsable file yields an empty store rather than an error: a lost cache
    /// costs a re-measurement, and failing the user's action over it would be a worse trade.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let entries = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Stored>(&bytes).ok())
            .filter(|stored| stored.version == STORE_VERSION)
            .map(|stored| stored.entries)
            .unwrap_or_default();
        Self { path, entries }
    }

    /// The store under `$XDG_CACHE_HOME/nix`.
    pub fn discover() -> Result<Self> {
        let dir = paths::cache_dir().ok_or_else(|| {
            AppError::new(
                ErrorCode::Unsupported,
                "Could not work out where to keep measured package sizes.",
            )
            .with_remedy("Set HOME or XDG_CACHE_HOME and try again.")
        })?;
        Ok(Self::at(dir.join("measured-packages.json")))
    }

    fn key(manager: Manager, id: &str, version: &str) -> String {
        format!("{manager:?}/{id}@{version}")
    }

    /// The measurement for exactly this package at exactly this version.
    #[must_use]
    pub fn get(&self, manager: Manager, id: &str, version: &str) -> Option<Measured> {
        self.entries.get(&Self::key(manager, id, version)).copied()
    }

    /// Record a measurement. Overwrites, since a fresh walk is more current than a stored one.
    pub fn put(&mut self, manager: Manager, id: &str, version: &str, measured: Measured) {
        self.entries
            .insert(Self::key(manager, id, version), measured);
    }

    /// How many measurements are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop every entry for a manager whose packages are no longer installed at those versions.
    ///
    /// Called with the current inventory, so the store does not grow without bound across upgrades:
    /// a package measured at ten successive versions would otherwise keep all ten.
    pub fn retain_current(&mut self, manager: Manager, current: &[(String, String)]) {
        let live: std::collections::HashSet<String> = current
            .iter()
            .map(|(id, version)| Self::key(manager, id, version))
            .collect();
        let prefix = format!("{manager:?}/");
        self.entries
            .retain(|key, _| !key.starts_with(&prefix) || live.contains(key));
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write the store out, atomically.
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .doing("create the cache directory")
                .map_err(|e| e.with_path(parent))?;
        }

        let json = serde_json::to_vec(&Stored {
            version: STORE_VERSION,
            entries: self.entries.clone(),
        })
        .map_err(|e| {
            AppError::internal("Could not encode measured package sizes.").with_cause(
                Cause::Other {
                    detail: e.to_string(),
                },
            )
        })?;

        // Sibling temporary then rename, the same pattern as the settings and scan stores: a crash
        // mid-write leaves the previous file intact rather than a truncated one.
        let tmp = self
            .path
            .with_extension(format!("json.tmp.{}", std::process::id()));
        std::fs::write(&tmp, &json)
            .doing("write measured package sizes")
            .map_err(|e| e.with_path(&tmp))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            std::fs::remove_file(&tmp).ok();
            AppError::from_io(&e, "save measured package sizes").with_path(&self.path)
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn measured(bytes: u64) -> Measured {
        Measured {
            apparent_bytes: bytes,
            disk_bytes: bytes + 4096,
            files: 3,
            directories: 1,
            unreadable: 0,
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nix-mstore-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("measured.json")
    }

    #[test]
    fn a_measurement_survives_a_round_trip() {
        let path = scratch("roundtrip");
        let mut store = MeasureStore::at(&path);
        store.put(Manager::Apt, "bash:amd64", "5.1-6", measured(1000));
        store.save().unwrap();

        let reopened = MeasureStore::at(&path);
        assert_eq!(
            reopened.get(Manager::Apt, "bash:amd64", "5.1-6"),
            Some(measured(1000))
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// The property the whole key design exists for.
    #[test]
    fn an_upgraded_package_does_not_return_the_old_measurement() {
        let mut store = MeasureStore::at(scratch("upgrade"));
        store.put(Manager::Apt, "bash:amd64", "5.1-6", measured(1000));

        assert_eq!(
            store.get(Manager::Apt, "bash:amd64", "5.1-7"),
            None,
            "a new version must miss, not return the previous size"
        );
    }

    /// Two architectures of one name are two packages, and must not share a measurement.
    #[test]
    fn architectures_are_stored_separately() {
        let mut store = MeasureStore::at(scratch("arch"));
        store.put(Manager::Apt, "libc6:amd64", "2.35", measured(13_594_000));
        store.put(Manager::Apt, "libc6:i386", "2.35", measured(12_482_000));

        assert_ne!(
            store.get(Manager::Apt, "libc6:amd64", "2.35"),
            store.get(Manager::Apt, "libc6:i386", "2.35")
        );
    }

    /// The same identity under two managers is two packages. Rare between system managers, and the
    /// reason the key carries the manager anyway: `PKG-3` brings snap and flatpak in, where a shared
    /// name is routine.
    #[test]
    fn managers_do_not_share_a_namespace() {
        let mut store = MeasureStore::at(scratch("managers"));
        store.put(Manager::Apt, "firefox", "1.0", measured(500));

        assert_eq!(
            store.get(Manager::Dnf, "firefox", "1.0"),
            None,
            "one manager's measurement is not another's"
        );
    }

    #[test]
    fn superseded_entries_are_dropped_when_the_inventory_is_seen() {
        let mut store = MeasureStore::at(scratch("retain"));
        store.put(Manager::Apt, "bash:amd64", "5.1-6", measured(1000));
        store.put(Manager::Apt, "bash:amd64", "5.1-7", measured(1100));
        store.put(Manager::Dnf, "code", "1.0", measured(2000));
        assert_eq!(store.len(), 3);

        store.retain_current(Manager::Apt, &[("bash:amd64".into(), "5.1-7".into())]);

        assert_eq!(store.get(Manager::Apt, "bash:amd64", "5.1-6"), None);
        assert!(store.get(Manager::Apt, "bash:amd64", "5.1-7").is_some());
        assert!(
            store.get(Manager::Dnf, "code", "1.0").is_some(),
            "pruning one manager must not touch another's entries"
        );
    }

    /// A cache is an optimisation. Losing it costs a re-measurement, and must never cost an error.
    #[test]
    fn an_unreadable_or_corrupt_store_opens_empty_rather_than_failing() {
        let missing = MeasureStore::at("/nonexistent/nix/measured.json");
        assert!(missing.is_empty());

        let path = scratch("corrupt");
        std::fs::write(&path, b"{ this is not json").unwrap();
        assert!(MeasureStore::at(&path).is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// A store written by a future version is discarded, not misread into the current shape.
    #[test]
    fn a_store_from_another_version_is_discarded() {
        let path = scratch("version");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "version": STORE_VERSION + 1,
                "entries": { "Apt/bash:amd64@5.1-6": measured(1000) }
            }))
            .unwrap(),
        )
        .unwrap();

        assert!(MeasureStore::at(&path).is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
