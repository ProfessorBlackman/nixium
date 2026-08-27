//! Package manager caches, and the first backend implementations. Task 1.13 (`STO-8`).
//!
//! # Reclaimed through the owning tool, never by unlinking
//!
//! The specification is explicit: package caches are cleaned with `apt-get clean`,
//! `dnf clean packages`, `pacman -Sc` — not by deleting files out of the cache directory. Stacer
//! unlinked cache files directly, which can leave a package manager's own index disagreeing with
//! what is on disk.
//!
//! It also pointed its **DNF** branch at the *pacman* cache directory, so on Fedora the category
//! always reported zero. That is the acceptance criterion for this task: on a Fedora machine the
//! DNF cache is found and cleaned.
//!
//! # Detection is per-capability, never per-distribution
//!
//! Principle P7. A machine with both `apt` and `flatpak` reports both; nothing here reads a
//! distribution name. Stacer picked exactly one manager by probing `PATH` in a fixed order and
//! stored it in a `static const`, so a system with two never saw the second.

use std::path::{Path, PathBuf};

use crate::caps::{self, Capability};
use crate::error::Result;
use crate::op::CancelToken;
use crate::space::{Category as SpaceCategory, Manager, ReclaimMethod, Safety};

use super::registry::{Candidate, Category};

/// One package manager's cache.
struct Backend {
    manager: Manager,
    capability: Capability,
    /// Where downloaded packages accumulate.
    cache_dirs: &'static [&'static str],
    /// What the user sees.
    label: &'static str,
}

/// Every backend nix knows how to clean.
///
/// **All of them are probed**, not just the first that matches — a machine with `apt` and `dnf`
/// present reports both.
const BACKENDS: &[Backend] = &[
    Backend {
        manager: Manager::Apt,
        capability: Capability::Apt,
        cache_dirs: &["/var/cache/apt/archives"],
        label: "APT package cache",
    },
    Backend {
        manager: Manager::Dnf,
        capability: Capability::Dnf,
        // The directory Stacer's DNF branch never looked at.
        cache_dirs: &["/var/cache/dnf"],
        label: "DNF package cache",
    },
    Backend {
        manager: Manager::Pacman,
        capability: Capability::Pacman,
        cache_dirs: &["/var/cache/pacman/pkg"],
        label: "pacman package cache",
    },
    Backend {
        manager: Manager::Zypper,
        capability: Capability::Zypper,
        // Detected but never implemented by Stacer, so openSUSE users saw an empty list.
        cache_dirs: &["/var/cache/zypp/packages"],
        label: "zypper package cache",
    },
];

/// Bytes held in a backend's cache directories.
fn cache_bytes(dirs: &[&str]) -> u64 {
    dirs.iter()
        .map(|d| crate::fixture::directory_size(Path::new(d)))
        .sum()
}

/// The package caches present on this system.
pub struct PackageCacheCategory {
    /// Overridable for tests: `(manager, label, directories)`.
    over: Option<Vec<(Manager, &'static str, Vec<PathBuf>)>>,
}

impl PackageCacheCategory {
    #[must_use]
    pub fn new() -> Self {
        Self { over: None }
    }

    /// A category over explicit directories, so the behaviour can be tested without a package
    /// manager installed.
    #[must_use]
    pub fn with(entries: Vec<(Manager, &'static str, Vec<PathBuf>)>) -> Self {
        Self {
            over: Some(entries),
        }
    }
}

impl Default for PackageCacheCategory {
    fn default() -> Self {
        Self::new()
    }
}

impl Category for PackageCacheCategory {
    fn id(&self) -> &'static str {
        "package_cache"
    }

    fn label(&self) -> &'static str {
        "Package caches"
    }

    fn space_category(&self) -> SpaceCategory {
        SpaceCategory::PackageCache
    }

    fn available(&self) -> bool {
        if self.over.is_some() {
            return true;
        }
        BACKENDS.iter().any(|b| caps::registry().has(b.capability))
    }

    fn candidates(&self, token: &CancelToken) -> Result<Vec<Candidate>> {
        token.check()?;

        let mut candidates = Vec::new();

        match &self.over {
            // The test path: explicit directories, no capability probing.
            Some(entries) => {
                for (manager, label, dirs) in entries {
                    token.check()?;
                    let bytes: u64 = dirs.iter().map(|d| crate::fixture::directory_size(d)).sum();
                    if bytes > 0 {
                        candidates.push(candidate(*manager, label, dirs.first().cloned(), bytes));
                    }
                }
            }
            None => {
                for backend in BACKENDS {
                    token.check()?;
                    // Per-capability, and every backend is asked. A machine with two managers
                    // reports two.
                    if !caps::registry().has(backend.capability) {
                        continue;
                    }
                    let bytes = cache_bytes(backend.cache_dirs);
                    if bytes == 0 {
                        continue; // an offer that would free nothing is noise
                    }
                    candidates.push(candidate(
                        backend.manager,
                        backend.label,
                        backend.cache_dirs.first().map(PathBuf::from),
                        bytes,
                    ));
                }
            }
        }

        // Sorted once, here, so both paths behave identically — an earlier version sorted only the
        // real path, and the two disagreed.
        candidates.sort_by_key(|c| std::cmp::Reverse(c.bytes));
        Ok(candidates)
    }
}

fn candidate(manager: Manager, label: &str, path: Option<PathBuf>, bytes: u64) -> Candidate {
    Candidate {
        path: path.unwrap_or_default(),
        label: label.to_string(),
        bytes,
        // The specification's own example of `Safe`: downloaded package files are re-fetchable, and
        // nothing the user can see is lost.
        safety: Safety::Safe,
        // Through the package manager, never by unlinking — the manager's index and the directory
        // must not be allowed to disagree.
        method: ReclaimMethod::PackageManager { manager },
        cost: Some(format!(
            "Downloaded package files are removed. {} will fetch them again if a package needs reinstalling.",
            manager.name()
        )),
        category: "package_cache".to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    struct FakeCache {
        root: PathBuf,
    }

    impl FakeCache {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "nix-pkgcache-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn dir(&self, name: &str, bytes: usize) -> PathBuf {
            let path = self.root.join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("package.pkg"), vec![b'x'; bytes]).unwrap();
            path
        }
    }

    impl Drop for FakeCache {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    /// The bug this task's acceptance criterion names: Stacer's DNF branch returned the *pacman*
    /// cache directory, so on Fedora the category always reported zero.
    #[test]
    fn every_backend_points_at_its_own_cache_directory() {
        for backend in BACKENDS {
            assert!(
                !backend.cache_dirs.is_empty(),
                "{:?} has no cache directory",
                backend.manager
            );
            for dir in backend.cache_dirs {
                assert!(dir.starts_with('/'), "{dir} must be absolute");
                let expected = match backend.manager {
                    Manager::Apt => "apt",
                    Manager::Dnf => "dnf",
                    Manager::Pacman => "pacman",
                    Manager::Zypper => "zypp",
                };
                assert!(
                    dir.contains(expected),
                    "{:?} points at {dir}, which is not its own cache",
                    backend.manager
                );
            }
        }
    }

    #[test]
    fn every_manager_including_zypper_has_a_backend() {
        for manager in [Manager::Apt, Manager::Dnf, Manager::Pacman, Manager::Zypper] {
            assert!(
                BACKENDS.iter().any(|b| b.manager == manager),
                "{manager:?} has no backend — Stacer detected zypper and never implemented it"
            );
        }
    }

    #[test]
    fn backends_are_distinct_in_manager_capability_and_directory() {
        let mut managers = std::collections::HashSet::new();
        let mut capabilities = std::collections::HashSet::new();
        let mut dirs = std::collections::HashSet::new();
        for backend in BACKENDS {
            assert!(managers.insert(backend.manager), "duplicate manager");
            assert!(
                capabilities.insert(backend.capability),
                "duplicate capability"
            );
            for dir in backend.cache_dirs {
                assert!(dirs.insert(*dir), "{dir} is claimed by two backends");
            }
        }
    }

    #[test]
    fn caches_are_cleaned_through_the_package_manager_not_by_unlinking() {
        let fake = FakeCache::new("method");
        let dir = fake.dir("apt", 8192);
        let category =
            PackageCacheCategory::with(vec![(Manager::Apt, "APT package cache", vec![dir])]);

        let found = category.candidates(&CancelToken::new()).unwrap();
        assert_eq!(found.len(), 1);
        match &found[0].method {
            ReclaimMethod::PackageManager { manager } => assert_eq!(*manager, Manager::Apt),
            other => panic!("must delegate to the package manager, got {other:?}"),
        }
        // Specifically not an unlink: the manager's index and the directory must not disagree.
        assert!(!matches!(found[0].method, ReclaimMethod::SystemFile { .. }));
    }

    #[test]
    fn package_caches_are_safe_and_state_their_cost() {
        let fake = FakeCache::new("safe");
        let dir = fake.dir("apt", 4096);
        let category =
            PackageCacheCategory::with(vec![(Manager::Apt, "APT package cache", vec![dir])]);

        let found = category.candidates(&CancelToken::new()).unwrap();
        assert_eq!(
            found[0].safety,
            Safety::Safe,
            "downloaded packages are re-fetchable with no visible loss"
        );
        assert!(
            found[0].cost.is_some(),
            "even a Safe item should say what happens"
        );
    }

    #[test]
    fn several_managers_on_one_machine_are_all_reported() {
        let fake = FakeCache::new("multi");
        let apt = fake.dir("apt", 8192);
        let dnf = fake.dir("dnf", 16384);

        let category = PackageCacheCategory::with(vec![
            (Manager::Apt, "APT package cache", vec![apt]),
            (Manager::Dnf, "DNF package cache", vec![dnf]),
        ]);

        let found = category.candidates(&CancelToken::new()).unwrap();
        assert_eq!(
            found.len(),
            2,
            "Stacer picked one manager and stored it in a static, so a second was invisible"
        );
    }

    #[test]
    fn an_empty_cache_is_not_offered() {
        let fake = FakeCache::new("empty");
        std::fs::create_dir_all(fake.root.join("apt")).unwrap();
        let category = PackageCacheCategory::with(vec![(
            Manager::Apt,
            "APT package cache",
            vec![fake.root.join("apt")],
        )]);
        assert!(category.candidates(&CancelToken::new()).unwrap().is_empty());
    }

    #[test]
    fn a_missing_cache_directory_is_not_an_error() {
        let category = PackageCacheCategory::with(vec![(
            Manager::Dnf,
            "DNF package cache",
            vec![PathBuf::from("/definitely/not/here")],
        )]);
        assert!(category.candidates(&CancelToken::new()).unwrap().is_empty());
    }

    #[test]
    fn cancellation_is_honoured() {
        let token = CancelToken::new();
        token.cancel();
        assert!(PackageCacheCategory::new().candidates(&token).is_err());
    }

    /// Guards principle P7 for this module specifically.
    #[test]
    fn nothing_here_reads_a_distribution_name() {
        let source = include_str!("packages.rs");
        // Only the implementation, not this test module — which necessarily contains the very words
        // it is looking for. And not documentation, which legitimately discusses distributions.
        let implementation = source.split("#[cfg(test)]").next().unwrap_or(source);
        for line in implementation.lines().filter(|l| {
            let t = l.trim_start();
            !t.starts_with("//") && !t.starts_with("///") && !t.starts_with("//!")
        }) {
            for banned in [
                "ubuntu",
                "fedora",
                "debian",
                "arch linux",
                "opensuse",
                "os-release",
            ] {
                assert!(
                    !line.to_ascii_lowercase().contains(banned),
                    "detection must be per-capability, not per-distribution: {line}"
                );
            }
        }
    }

    #[test]
    fn candidates_are_ordered_largest_first() {
        let fake = FakeCache::new("order");
        let small = fake.dir("apt", 4096);
        let large = fake.dir("dnf", 65536);

        let category = PackageCacheCategory::with(vec![
            (Manager::Apt, "APT package cache", vec![small]),
            (Manager::Dnf, "DNF package cache", vec![large]),
        ]);
        let found = category.candidates(&CancelToken::new()).unwrap();
        assert!(found[0].bytes >= found[1].bytes);
    }
}
