//! Protected paths. Task 1.7 (`STO-9`).
//!
//! **Built before the executor, deliberately.** The plan's fourth sequencing rule is that safety
//! machinery lands before the categories worth reclaiming, because once package caches and system
//! logs are on screen there is pressure to bypass a guard "just for this one" — which is precisely
//! how Stacer ended up running a bare privileged `rm -rf` over an argument list built from UI state.
//!
//! # Consulted twice, not once
//!
//! The specification requires this to be checked by the **scanner and the executor**. Checking only
//! at scan time would mean a path could be offered and then deleted after the rules changed;
//! checking only at execute time would mean showing a user reclaimable space that nix will refuse to
//! touch. Both are dishonest in different directions, so both layers ask.
//!
//! # What is protected, and why each entry is here
//!
//! The list is conservative on purpose. A false positive costs a few megabytes left unreclaimed; a
//! false negative costs someone their system or their keys.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::paths;

/// A rule that protects a path, and the reason a user is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rule {
    /// Absolute prefix this rule covers.
    prefix: &'static str,
    /// Stable identifier, used in reports and tests.
    id: &'static str,
    /// Why, phrased for a user.
    reason: &'static str,
}

/// Absolute locations nix must never reclaim from.
const PROTECTED_PREFIXES: &[Rule] = &[
    // Executables and libraries. Removing one breaks the system immediately, and package files
    // belong to the package manager regardless.
    Rule {
        prefix: "/bin",
        id: "system_binaries",
        reason: "System programs.",
    },
    Rule {
        prefix: "/sbin",
        id: "system_binaries",
        reason: "System programs.",
    },
    Rule {
        prefix: "/lib",
        id: "system_libraries",
        reason: "System libraries.",
    },
    Rule {
        prefix: "/lib32",
        id: "system_libraries",
        reason: "System libraries.",
    },
    Rule {
        prefix: "/lib64",
        id: "system_libraries",
        reason: "System libraries.",
    },
    Rule {
        prefix: "/usr",
        id: "system_files",
        reason: "Installed software. Remove it with the package manager instead.",
    },
    // The bootloader and kernels. Old kernels are reclaimable, but only through the package
    // manager (STO-11) — never by deleting files.
    Rule {
        prefix: "/boot",
        id: "boot",
        reason: "Kernels and the bootloader. Old kernels are removed with the package manager.",
    },
    Rule {
        prefix: "/efi",
        id: "boot",
        reason: "The EFI system partition.",
    },
    // Kernel and runtime pseudo-filesystems. Not storage at all.
    Rule {
        prefix: "/proc",
        id: "kernel_interface",
        reason: "A kernel interface, not stored data.",
    },
    Rule {
        prefix: "/sys",
        id: "kernel_interface",
        reason: "A kernel interface, not stored data.",
    },
    Rule {
        prefix: "/dev",
        id: "kernel_interface",
        reason: "Device nodes.",
    },
    Rule {
        prefix: "/run",
        id: "runtime_state",
        reason: "Runtime state for running programs.",
    },
    // Configuration. Reclaiming it silently reconfigures the machine.
    Rule {
        prefix: "/etc",
        id: "configuration",
        reason: "System configuration.",
    },
    // Package manager databases. Losing one means the system no longer knows what is installed.
    Rule {
        prefix: "/var/lib/dpkg",
        id: "package_database",
        reason: "The package database.",
    },
    Rule {
        prefix: "/var/lib/rpm",
        id: "package_database",
        reason: "The package database.",
    },
    Rule {
        prefix: "/var/lib/pacman",
        id: "package_database",
        reason: "The package database.",
    },
    Rule {
        prefix: "/var/lib/flatpak",
        id: "package_database",
        reason: "Flatpak's installation. Remove apps with flatpak instead.",
    },
    Rule {
        prefix: "/var/lib/snapd",
        id: "package_database",
        reason: "Snap's installation. Remove snaps with snap instead.",
    },
    // Live databases. Deleting files under a running database corrupts it.
    Rule {
        prefix: "/var/lib/mysql",
        id: "live_database",
        reason: "A database's live data.",
    },
    Rule {
        prefix: "/var/lib/postgresql",
        id: "live_database",
        reason: "A database's live data.",
    },
    Rule {
        prefix: "/var/lib/mongodb",
        id: "live_database",
        reason: "A database's live data.",
    },
    Rule {
        prefix: "/var/lib/influxdb",
        id: "live_database",
        reason: "A database's live data.",
    },
    // Container volumes hold data users expect to persist; images and caches are reclaimable, and
    // are handled separately in STO-13 through the runtime's own prune.
    Rule {
        prefix: "/var/lib/docker/volumes",
        id: "container_volume",
        reason: "Container volumes hold data your containers expect to keep.",
    },
    Rule {
        prefix: "/var/lib/containers/storage/volumes",
        id: "container_volume",
        reason: "Container volumes hold data your containers expect to keep.",
    },
    // Removable and network media. The user mounted it; nix does not get to clean it.
    Rule {
        prefix: "/media",
        id: "removable_media",
        reason: "Removable media.",
    },
    Rule {
        prefix: "/mnt",
        id: "mounted_volume",
        reason: "A volume you mounted yourself.",
    },
    // Other users' data.
    Rule {
        prefix: "/root",
        id: "other_user",
        reason: "Another user's home directory.",
    },
];

/// Directory names that protect their entire subtree wherever they appear.
const PROTECTED_NAMES: &[Rule] = &[
    Rule {
        prefix: ".git",
        id: "version_control",
        reason: "Version control history.",
    },
    Rule {
        prefix: ".hg",
        id: "version_control",
        reason: "Version control history.",
    },
    Rule {
        prefix: ".svn",
        id: "version_control",
        reason: "Version control history.",
    },
    Rule {
        prefix: ".ssh",
        id: "credentials",
        reason: "SSH keys.",
    },
    Rule {
        prefix: ".gnupg",
        id: "credentials",
        reason: "GPG keys.",
    },
    Rule {
        prefix: ".password-store",
        id: "credentials",
        reason: "Stored passwords.",
    },
];

/// Why a path may not be reclaimed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Refusal {
    pub path: PathBuf,
    /// Stable rule identifier.
    pub rule: String,
    /// Phrased for a user.
    pub reason: String,
}

/// The answer to "may nix reclaim this?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// No rule objects. Note this says nothing about whether reclaiming is *wise* — that is the
    /// entry's [`crate::space::Safety`] rating.
    Allowed,
    /// A rule objects, and here is which.
    Protected(Refusal),
}

impl Verdict {
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }

    #[must_use]
    pub const fn refusal(&self) -> Option<&Refusal> {
        match self {
            Self::Allowed => None,
            Self::Protected(r) => Some(r),
        }
    }
}

/// Decides whether a path may be reclaimed.
#[derive(Debug, Clone, Default)]
pub struct Guard {
    /// Paths the user added. Treated exactly like a built-in rule.
    user: Vec<PathBuf>,
    /// nix's own directories, resolved once.
    own: Vec<PathBuf>,
}

impl Guard {
    /// A guard with the built-in rules plus the user's own exclusions.
    #[must_use]
    pub fn new(user_exclusions: Vec<PathBuf>) -> Self {
        // nix must not reclaim its own settings, logs or scan cache: a tool that deletes its own
        // state mid-operation is a tool that cannot report what it did.
        let own = [paths::config_dir(), paths::state_dir(), paths::cache_dir()]
            .into_iter()
            .flatten()
            .collect();
        Self {
            user: user_exclusions,
            own,
        }
    }

    /// A guard configured from the user's saved settings.
    #[must_use]
    pub fn from_settings(settings: &crate::settings::Settings) -> Self {
        Self::new(settings.protected_paths.clone())
    }

    /// Whether `path` is inside `prefix`, comparing whole path components.
    ///
    /// Component-wise, never string prefixes: `/usr` must protect `/usr/bin` but must **not** match
    /// `/usrdata`, and a naive `starts_with` on strings gets that wrong.
    fn within(path: &Path, prefix: &Path) -> bool {
        path == prefix || path.starts_with(prefix)
    }

    /// May nix reclaim this path?
    #[must_use]
    pub fn verdict(&self, path: &Path) -> Verdict {
        // A relative path cannot be reasoned about safely — the rules are absolute.
        if !path.is_absolute() {
            return Verdict::Protected(Refusal {
                path: path.to_path_buf(),
                rule: "relative_path".to_string(),
                reason: "Only absolute paths can be checked against the protection rules."
                    .to_string(),
            });
        }

        // The root itself, and anything with a traversal segment.
        if path.parent().is_none() {
            return Verdict::Protected(Refusal {
                path: path.to_path_buf(),
                rule: "filesystem_root".to_string(),
                reason: "The root of the filesystem.".to_string(),
            });
        }
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Verdict::Protected(Refusal {
                path: path.to_path_buf(),
                rule: "traversal".to_string(),
                reason: "Paths containing .. are not accepted.".to_string(),
            });
        }

        for rule in PROTECTED_PREFIXES {
            if Self::within(path, Path::new(rule.prefix)) {
                return Verdict::Protected(Refusal {
                    path: path.to_path_buf(),
                    rule: rule.id.to_string(),
                    reason: rule.reason.to_string(),
                });
            }
        }

        for rule in PROTECTED_NAMES {
            if path
                .components()
                .any(|c| c.as_os_str() == std::ffi::OsStr::new(rule.prefix))
            {
                return Verdict::Protected(Refusal {
                    path: path.to_path_buf(),
                    rule: rule.id.to_string(),
                    reason: rule.reason.to_string(),
                });
            }
        }

        for own in &self.own {
            if Self::within(path, own) {
                return Verdict::Protected(Refusal {
                    path: path.to_path_buf(),
                    rule: "nix_state".to_string(),
                    reason: "nix's own settings, logs and cache.".to_string(),
                });
            }
        }

        for excluded in &self.user {
            if Self::within(path, excluded) {
                return Verdict::Protected(Refusal {
                    path: path.to_path_buf(),
                    rule: "user_exclusion".to_string(),
                    reason: format!("You excluded {}.", excluded.display()),
                });
            }
        }

        Verdict::Allowed
    }

    /// Convenience: whether the path may be reclaimed.
    #[must_use]
    pub fn allows(&self, path: &Path) -> bool {
        self.verdict(path).is_allowed()
    }

    /// Partition a set of paths into permitted and refused.
    #[must_use]
    pub fn partition(&self, paths: Vec<PathBuf>) -> (Vec<PathBuf>, Vec<Refusal>) {
        let mut allowed = Vec::new();
        let mut refused = Vec::new();
        for path in paths {
            match self.verdict(&path) {
                Verdict::Allowed => allowed.push(path),
                Verdict::Protected(r) => refused.push(r),
            }
        }
        (allowed, refused)
    }

    /// Every built-in prefix, for the settings view — a user should be able to read what nix
    /// refuses to touch.
    #[must_use]
    pub fn built_in_rules() -> Vec<Refusal> {
        PROTECTED_PREFIXES
            .iter()
            .chain(PROTECTED_NAMES)
            .map(|r| Refusal {
                path: PathBuf::from(r.prefix),
                rule: r.id.to_string(),
                reason: r.reason.to_string(),
            })
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn guard() -> Guard {
        Guard {
            user: Vec::new(),
            own: Vec::new(),
        }
    }

    /// The core promise. If any of these ever becomes reclaimable, someone loses a system.
    #[test]
    fn the_things_that_must_never_be_reclaimed_are_not() {
        let g = guard();
        for path in [
            "/bin/sh",
            "/sbin/init",
            "/usr/bin/rm",
            "/usr/lib/systemd/systemd",
            "/lib/x86_64-linux-gnu/libc.so.6",
            "/lib64/ld-linux-x86-64.so.2",
            "/boot/vmlinuz-6.5.0",
            "/boot/grub/grub.cfg",
            "/efi/EFI/BOOT/BOOTX64.EFI",
            "/etc/passwd",
            "/etc/fstab",
            "/proc/1/status",
            "/sys/block/sda",
            "/dev/sda1",
            "/run/user/1000",
            "/var/lib/dpkg/status",
            "/var/lib/rpm/Packages",
            "/var/lib/pacman/local",
            "/var/lib/mysql/ibdata1",
            "/var/lib/postgresql/16/main",
            "/var/lib/docker/volumes/my-data/_data/db",
            "/media/usb-stick/holiday.jpg",
            "/mnt/backup/archive.tar",
            "/root/.bashrc",
        ] {
            let verdict = g.verdict(Path::new(path));
            assert!(
                !verdict.is_allowed(),
                "{path} was permitted — this is a system-destroying bug"
            );
            let refusal = verdict.refusal().unwrap();
            assert!(!refusal.reason.is_empty(), "{path} refused with no reason");
        }
    }

    #[test]
    fn credentials_and_version_control_are_protected_wherever_they_appear() {
        let g = guard();
        for path in [
            "/home/me/.ssh/id_ed25519",
            "/home/me/projects/thing/.git/objects/ab/cdef",
            "/home/me/.gnupg/pubring.kbx",
            "/home/me/.password-store/email.gpg",
            "/home/me/work/repo/.hg/store",
            "/tmp/checkout/.svn/wc.db",
        ] {
            assert!(!g.allows(Path::new(path)), "{path} must be protected");
        }
    }

    /// The bug a string `starts_with` would introduce.
    #[test]
    fn prefixes_match_whole_components_not_string_prefixes() {
        let g = guard();
        // Protected.
        assert!(!g.allows(Path::new("/usr/share/doc")));
        // NOT protected: a different directory that merely shares a textual prefix.
        assert!(
            g.allows(Path::new("/usrdata/archive")),
            "/usrdata is not /usr"
        );
        assert!(
            g.allows(Path::new("/etcetera/notes")),
            "/etcetera is not /etc"
        );
        assert!(
            g.allows(Path::new("/bindings/generated")),
            "/bindings is not /bin"
        );
        assert!(
            g.allows(Path::new("/media-server/films")),
            "/media-server is not /media"
        );
    }

    #[test]
    fn ordinary_user_files_and_caches_are_allowed() {
        let g = guard();
        for path in [
            "/home/me/.cache/mozilla/firefox/abc.default",
            "/home/me/Downloads/big.iso",
            "/home/me/projects/thing/target/debug",
            "/var/cache/apt/archives/foo.deb",
            "/var/log/syslog.1.gz",
            "/tmp/scratch",
        ] {
            assert!(g.allows(Path::new(path)), "{path} should be reclaimable");
        }
    }

    #[test]
    fn the_filesystem_root_is_refused() {
        assert!(!guard().allows(Path::new("/")));
    }

    #[test]
    fn traversal_and_relative_paths_are_refused() {
        let g = guard();
        // Traversal could resolve anywhere, so it is refused rather than resolved — resolving and
        // then checking is a time-of-check/time-of-use race.
        let verdict = g.verdict(Path::new("/home/me/../../etc/shadow"));
        assert_eq!(verdict.refusal().unwrap().rule, "traversal");

        let verdict = g.verdict(Path::new("home/me/cache"));
        assert_eq!(verdict.refusal().unwrap().rule, "relative_path");
    }

    #[test]
    fn user_exclusions_are_honoured_and_named_in_the_reason() {
        let g = Guard {
            user: vec![PathBuf::from("/srv/important")],
            own: Vec::new(),
        };
        assert!(!g.allows(Path::new("/srv/important/data/file")));
        let refusal = g
            .verdict(Path::new("/srv/important"))
            .refusal()
            .unwrap()
            .clone();
        assert_eq!(refusal.rule, "user_exclusion");
        assert!(
            refusal.reason.contains("/srv/important"),
            "a user should see which of their rules applied: {}",
            refusal.reason
        );
        // A sibling is unaffected.
        assert!(g.allows(Path::new("/srv/important-other/data")));
    }

    #[test]
    fn nix_does_not_reclaim_its_own_state() {
        let g = Guard {
            user: Vec::new(),
            own: vec![PathBuf::from("/home/me/.cache/nix")],
        };
        assert!(!g.allows(Path::new("/home/me/.cache/nix/scans/abc.json")));
        assert_eq!(
            g.verdict(Path::new("/home/me/.cache/nix"))
                .refusal()
                .unwrap()
                .rule,
            "nix_state"
        );
        // The rest of the cache directory is still fair game.
        assert!(g.allows(Path::new("/home/me/.cache/other-app/blob")));
    }

    #[test]
    fn a_real_guard_protects_its_own_directories() {
        let g = Guard::new(Vec::new());
        if let Some(cache) = paths::cache_dir() {
            assert!(
                !g.allows(&cache.join("scans")),
                "nix must not reclaim its own cache"
            );
        }
        if let Some(config) = paths::config_dir() {
            assert!(!g.allows(&config.join("settings.json")));
        }
    }

    #[test]
    fn partition_splits_and_explains() {
        let g = Guard {
            user: vec![PathBuf::from("/srv/keep")],
            own: Vec::new(),
        };
        let (allowed, refused) = g.partition(vec![
            PathBuf::from("/home/me/.cache/a"),
            PathBuf::from("/etc/passwd"),
            PathBuf::from("/srv/keep/x"),
            PathBuf::from("/home/me/.cache/b"),
        ]);
        assert_eq!(allowed.len(), 2);
        assert_eq!(refused.len(), 2);
        let rules: Vec<&str> = refused.iter().map(|r| r.rule.as_str()).collect();
        assert!(rules.contains(&"configuration"));
        assert!(rules.contains(&"user_exclusion"));
    }

    #[test]
    fn built_in_rules_are_listable_and_every_one_explains_itself() {
        let rules = Guard::built_in_rules();
        assert!(rules.len() > 20, "the list should be substantial");
        for r in &rules {
            assert!(!r.reason.is_empty(), "{} has no reason", r.rule);
            assert!(!r.rule.is_empty());
            assert!(
                r.reason.ends_with('.'),
                "reasons are shown to users, so they read as sentences: {:?}",
                r.reason
            );
        }
    }

    #[test]
    fn settings_configure_the_guard() {
        let settings = crate::settings::Settings {
            protected_paths: vec![PathBuf::from("/data/archive")],
            ..crate::settings::Settings::default()
        };
        let g = Guard::from_settings(&settings);
        assert!(!g.allows(Path::new("/data/archive/2024")));
    }
}
