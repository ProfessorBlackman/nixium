//! Removable system packages: old kernels and residual configuration. `STO-11`.
//!
//! # Why this is first in Phase 2
//!
//! The plan orders Phase 2 by reclaim value, and on a Debian or Ubuntu machine that has been
//! upgraded for a year this is the single largest win available — a kernel set is several hundred
//! megabytes, and half a dozen may be sitting there. Stacer offered none of them: it listed packages
//! with no size information at all, so there was no way to see that they were the largest thing on
//! the disk.
//!
//! # The safety rule, enforced twice
//!
//! > Never the running kernel, and never the newest installed kernel.
//!
//! Checked here so nothing wrong is ever *offered*, and re-derived independently inside the
//! privileged helper so nothing wrong can be *carried out* — even by a caller that constructs the
//! request deliberately. Two enforcement points for one rule, because the consequence of getting it
//! wrong is a machine that does not boot.

use crate::error::Result;
use crate::op::CancelToken;
use crate::pkg::{Backend, DpkgBackend, dpkg};
use crate::space::{Category as SpaceCategory, ReclaimMethod, RemovableKind, Safety};

use super::registry::{Candidate, Category};

/// Superseded kernels.
pub struct OldKernelCategory {
    backend: DpkgBackend,
}

impl OldKernelCategory {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            backend: DpkgBackend::new(),
        }
    }
}

impl Default for OldKernelCategory {
    fn default() -> Self {
        Self::new()
    }
}

impl Category for OldKernelCategory {
    fn id(&self) -> &'static str {
        "old_kernels"
    }

    fn label(&self) -> &'static str {
        "Old kernels"
    }

    fn space_category(&self) -> SpaceCategory {
        SpaceCategory::PackagePayload
    }

    fn available(&self) -> bool {
        self.backend.available()
    }

    fn candidates(&self, token: &CancelToken) -> Result<Vec<Candidate>> {
        token.check()?;
        let installed = self.backend.installed()?;
        let running = dpkg::running_kernel();

        let mut candidates = Vec::new();
        for set in dpkg::removable_kernels(&installed, running.as_ref()) {
            token.check()?;

            let names: Vec<String> = set.packages.iter().map(|p| p.name.clone()).collect();
            let count = names.len();

            candidates.push(Candidate {
                // A kernel is a logical thing, not a path. The version is what the user recognises,
                // because it is what `uname -r` prints.
                path: std::path::PathBuf::from(format!("kernel {}", set.version.0)),
                label: format!("Linux {} ({count} packages)", set.version.0),
                bytes: set.bytes(),
                // Not `Safe`: a kernel you cannot boot into is a kernel you cannot fall back to.
                // Removing one is fine and routine, but it is a decision, so it is never pre-checked.
                safety: Safety::Review,
                method: ReclaimMethod::Packages {
                    kind: RemovableKind::OldKernel,
                    names,
                },
                cost: Some(format!(
                    "Removes Linux {} entirely. You will no longer be able to boot into it if a newer kernel turns out to have a problem.",
                    set.version.0
                )),
                category: self.id().to_string(),
            });
        }

        Ok(candidates)
    }
}

/// Configuration left behind by packages that were removed without purging.
pub struct ResidualConfigCategory {
    backend: DpkgBackend,
}

impl ResidualConfigCategory {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            backend: DpkgBackend::new(),
        }
    }
}

impl Default for ResidualConfigCategory {
    fn default() -> Self {
        Self::new()
    }
}

impl Category for ResidualConfigCategory {
    fn id(&self) -> &'static str {
        "residual_config"
    }

    fn label(&self) -> &'static str {
        "Leftover configuration"
    }

    fn space_category(&self) -> SpaceCategory {
        SpaceCategory::OrphanedConfig
    }

    fn available(&self) -> bool {
        self.backend.available()
    }

    fn candidates(&self, token: &CancelToken) -> Result<Vec<Candidate>> {
        token.check()?;
        let residual = self.backend.residual_config()?;
        if residual.is_empty() {
            return Ok(Vec::new());
        }

        let total: u64 = residual.iter().map(|r| r.bytes).sum();
        let names: Vec<String> = residual.iter().map(|r| r.name.clone()).collect();
        let count = names.len();

        // One decision rather than one per package: individually these are a few kilobytes each, and
        // a list of two hundred entries worth 40 KiB apiece is noise, not a choice.
        Ok(vec![Candidate {
            path: std::path::PathBuf::from("removed packages"),
            label: format!(
                "Configuration from {count} removed package{}",
                if count == 1 { "" } else { "s" }
            ),
            bytes: total,
            // The software is already gone; only its settings remain. Nothing running depends on
            // them, but a user who reinstalls would lose their customisation.
            safety: Safety::Review,
            method: ReclaimMethod::Packages {
                kind: RemovableKind::ResidualConfig,
                names,
            },
            cost: Some(format!(
                "Purges settings left behind by {count} package{} you already removed. If you reinstall one, it starts from defaults.",
                if count == 1 { "" } else { "s" }
            )),
            category: self.id().to_string(),
        }])
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Against this machine's real package database. It asserts the safety property rather than a
    /// particular result, so it is meaningful whatever kernels happen to be installed.
    #[test]
    fn no_offered_kernel_is_the_running_one_or_the_newest() {
        let category = OldKernelCategory::new();
        if !category.available() {
            return; // no APT here
        }

        let Ok(candidates) = category.candidates(&CancelToken::new()) else {
            return;
        };
        let Some(running) = dpkg::running_kernel() else {
            return;
        };

        let installed = DpkgBackend::new().installed().unwrap_or_default();
        let newest = dpkg::kernel_sets(&installed)
            .into_iter()
            .max_by(|a, b| a.version.compare(&b.version))
            .map(|s| s.version);

        for candidate in &candidates {
            let ReclaimMethod::Packages { names, .. } = &candidate.method else {
                panic!("a kernel must be removed through the package manager");
            };
            for name in names {
                let version = dpkg::KernelVersion::from_package(name)
                    .expect("every package in a kernel set is a kernel package");
                assert_ne!(
                    version.base(),
                    running.base(),
                    "{name} belongs to the running kernel"
                );
                if let Some(newest) = &newest {
                    assert_ne!(
                        version.base(),
                        newest.base(),
                        "{name} belongs to the newest installed kernel, which boots next"
                    );
                }
            }
        }
    }

    #[test]
    fn a_kernel_is_offered_as_one_decision_covering_all_its_packages() {
        let category = OldKernelCategory::new();
        if !category.available() {
            return;
        }
        let Ok(candidates) = category.candidates(&CancelToken::new()) else {
            return;
        };

        for candidate in &candidates {
            let ReclaimMethod::Packages { names, kind } = &candidate.method else {
                panic!("unexpected method");
            };
            assert_eq!(*kind, RemovableKind::OldKernel);
            assert!(
                !names.is_empty(),
                "a kernel set with no packages is not a set"
            );
            // Every package in one candidate belongs to the same kernel.
            let versions: std::collections::HashSet<String> = names
                .iter()
                .filter_map(|n| dpkg::KernelVersion::from_package(n))
                .map(|v| v.base())
                .collect();
            assert_eq!(
                versions.len(),
                1,
                "one candidate must be one kernel: {versions:?}"
            );
        }
    }

    #[test]
    fn kernels_are_review_and_say_what_is_lost() {
        let category = OldKernelCategory::new();
        if !category.available() {
            return;
        }
        let Ok(candidates) = category.candidates(&CancelToken::new()) else {
            return;
        };

        for candidate in &candidates {
            assert_eq!(
                candidate.safety,
                Safety::Review,
                "a kernel you cannot boot into is a fallback you no longer have, so never pre-checked"
            );
            let cost = candidate.cost.as_deref().unwrap_or("");
            assert!(
                cost.contains("boot"),
                "the cost must name what is actually lost: {cost}"
            );
        }
    }

    #[test]
    fn residual_configuration_is_one_decision_and_states_its_cost() {
        let category = ResidualConfigCategory::new();
        if !category.available() {
            return;
        }
        let Ok(candidates) = category.candidates(&CancelToken::new()) else {
            return;
        };

        // At most one: two hundred entries of 40 KiB each is noise, not a choice.
        assert!(candidates.len() <= 1, "{} candidates", candidates.len());
        for candidate in &candidates {
            assert_eq!(candidate.safety, Safety::Review);
            let cost = candidate.cost.as_deref().unwrap_or("");
            assert!(cost.contains("defaults"), "{cost}");
            let ReclaimMethod::Packages { kind, names } = &candidate.method else {
                panic!("unexpected method");
            };
            assert_eq!(*kind, RemovableKind::ResidualConfig);
            assert!(!names.is_empty());
        }
    }

    #[test]
    fn cancellation_is_honoured() {
        let token = CancelToken::new();
        token.cancel();
        assert!(OldKernelCategory::new().candidates(&token).is_err());
        assert!(ResidualConfigCategory::new().candidates(&token).is_err());
    }

    #[test]
    fn categories_describe_themselves() {
        assert_eq!(OldKernelCategory::new().id(), "old_kernels");
        assert_eq!(ResidualConfigCategory::new().id(), "residual_config");
        assert_eq!(
            OldKernelCategory::new().space_category(),
            SpaceCategory::PackagePayload
        );
        assert_eq!(
            ResidualConfigCategory::new().space_category(),
            SpaceCategory::OrphanedConfig
        );
    }
}
