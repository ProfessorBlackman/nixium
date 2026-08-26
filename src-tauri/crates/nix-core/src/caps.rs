//! Capability probing. Task 0.7 (`FND-7`).
//!
//! The rule this module exists to enforce is principle P7: **detect capabilities, never distro
//! names.** Stacer decided whether to show an entire settings page by matching `$DESKTOP_SESSION`
//! against the string `"ubuntu"`, and detected `zypper` only to never implement it. Both are the
//! same mistake — inferring what a system can do from what it is called.
//!
//! Results are cached for the session because a `PATH` walk per query would be wasteful, and
//! invalidated explicitly rather than on a timer, because a silent refresh is impossible to reason
//! about.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Something nix needs from the host in order to offer a feature.
///
/// One variant per *capability*, not per distro. A machine with both `apt` and `flatpak` reports
/// both, and the features that depend on each are offered independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Capability {
    // Package backends
    Apt,
    Dnf,
    Pacman,
    Zypper,
    Snap,
    Flatpak,
    // System interfaces
    Systemctl,
    Journalctl,
    /// A graphical polkit agent path — without it, privileged operations cannot be authorised.
    Pkexec,
    /// btrfs userspace tools, needed for honest accounting on btrfs (`STO-17`).
    BtrfsTools,
}

impl Capability {
    /// Every capability, for a full probe.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Apt,
            Self::Dnf,
            Self::Pacman,
            Self::Zypper,
            Self::Snap,
            Self::Flatpak,
            Self::Systemctl,
            Self::Journalctl,
            Self::Pkexec,
            Self::BtrfsTools,
        ]
    }

    /// The executable that, if present on `PATH`, means this capability is available.
    #[must_use]
    pub const fn probe_binary(self) -> &'static str {
        match self {
            Self::Apt => "apt-get",
            Self::Dnf => "dnf",
            Self::Pacman => "pacman",
            Self::Zypper => "zypper",
            Self::Snap => "snap",
            Self::Flatpak => "flatpak",
            Self::Systemctl => "systemctl",
            Self::Journalctl => "journalctl",
            Self::Pkexec => "pkexec",
            Self::BtrfsTools => "btrfs",
        }
    }

    /// Human name, used in "X is not available on this system" messages.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Apt => "APT",
            Self::Dnf => "DNF",
            Self::Pacman => "pacman",
            Self::Zypper => "zypper",
            Self::Snap => "Snap",
            Self::Flatpak => "Flatpak",
            Self::Systemctl => "systemd",
            Self::Journalctl => "the systemd journal",
            Self::Pkexec => "polkit (pkexec)",
            Self::BtrfsTools => "btrfs tools",
        }
    }
}

/// Find an executable on `PATH`. Pure with respect to its inputs so it can be tested.
fn find_on_path(binary: &str, path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let path_var = path_var?;
    std::env::split_paths(path_var)
        .map(|dir| dir.join(binary))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// The session-wide capability cache.
#[derive(Debug, Default)]
pub struct Registry {
    probed: Mutex<HashMap<Capability, Option<PathBuf>>>,
}

impl Registry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a capability is present, probing once and caching the answer.
    ///
    /// A poisoned cache lock is treated as an unprimed cache rather than a panic: a wrong-but-fresh
    /// probe is strictly better than taking the process down over a lock.
    pub fn has(&self, cap: Capability) -> bool {
        self.resolve(cap).is_some()
    }

    /// The resolved path of a capability's binary, if present.
    pub fn resolve(&self, cap: Capability) -> Option<PathBuf> {
        if let Ok(cache) = self.probed.lock() {
            if let Some(hit) = cache.get(&cap) {
                return hit.clone();
            }
        }
        let found = find_on_path(cap.probe_binary(), std::env::var_os("PATH").as_deref());
        if let Ok(mut cache) = self.probed.lock() {
            cache.insert(cap, found.clone());
        }
        found
    }

    /// Probe everything and report. Used by the diagnostics bundle and by the frontend on start.
    pub fn snapshot(&self) -> Snapshot {
        let present = Capability::all()
            .iter()
            .copied()
            .filter(|c| self.has(*c))
            .collect();
        Snapshot { present }
    }

    /// Drop cached probes. Call after anything that could change `PATH` or install a tool.
    ///
    /// Invalidation is explicit rather than time-based: Stacer cached its core count, its disk
    /// names and its default network interface forever, which is why hot-plugging a disk or
    /// switching networks was invisible to a running instance.
    pub fn invalidate(&self) {
        if let Ok(mut cache) = self.probed.lock() {
            cache.clear();
        }
    }
}

/// What the host can do, as sent to the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Snapshot {
    /// Capabilities found present, sorted for a stable wire representation.
    pub present: Vec<Capability>,
}

impl Snapshot {
    #[must_use]
    pub fn has(&self, cap: Capability) -> bool {
        self.present.contains(&cap)
    }
}

/// The process-wide registry.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::new)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn every_capability_has_distinct_probe_and_label() {
        let mut bins = std::collections::HashSet::new();
        for c in Capability::all() {
            assert!(!c.probe_binary().is_empty());
            assert!(!c.label().is_empty());
            assert!(
                bins.insert(c.probe_binary()),
                "two capabilities probe the same binary: {}",
                c.probe_binary()
            );
        }
        assert_eq!(bins.len(), Capability::all().len());
    }

    #[test]
    fn find_on_path_locates_an_executable() {
        let dir = std::env::temp_dir().join(format!("nix-caps-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("pretend-tool");
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();

        // Not executable yet: must not be found.
        let path = OsString::from(dir.as_os_str());
        assert!(find_on_path("pretend-tool", Some(&path)).is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(find_on_path("pretend-tool", Some(&path)).unwrap(), exe);
        }

        assert!(find_on_path("definitely-not-here", Some(&path)).is_none());
        assert!(find_on_path("pretend-tool", None).is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn probes_are_cached_and_invalidatable() {
        let reg = Registry::new();
        let first = reg.has(Capability::Systemctl);
        assert_eq!(
            reg.has(Capability::Systemctl),
            first,
            "cached answer must be stable"
        );
        reg.invalidate();
        assert_eq!(reg.has(Capability::Systemctl), first, "re-probe must agree");
    }

    #[test]
    fn snapshot_round_trips_and_answers_membership() {
        let snap = Snapshot {
            present: vec![Capability::Apt, Capability::Flatpak],
        };
        assert!(snap.has(Capability::Apt));
        assert!(!snap.has(Capability::Dnf));

        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("\"apt\""), "{json}");
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }

    /// Guards principle P7. If this ever fails, someone has inferred a capability from a name.
    #[test]
    fn no_capability_is_named_after_a_distribution() {
        for c in Capability::all() {
            let label = c.label().to_ascii_lowercase();
            for banned in ["ubuntu", "fedora", "debian", "arch", "suse", "mint"] {
                assert!(!label.contains(banned), "{label} names a distribution");
            }
        }
    }
}
