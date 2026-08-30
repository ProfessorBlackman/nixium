// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Snap revisions and flatpak runtimes. `STO-12`.
//!
//! # Why these are worth a milestone of their own
//!
//! On the Ubuntu machine this was developed on, superseded snap revisions come to **3.3 GiB** — more
//! than every old kernel put together, and the single largest reclaimable figure nix has found so
//! far. Stacer listed snaps by name with no revisions and no sizes at all, so none of it was visible.
//!
//! # What each category promises
//!
//! [`SnapRevisionCategory`] is exact, and had to be *made* exact. snapd hard-links every blob into
//! its own download cache, so dropping a revision leaves the blocks allocated until snapd prunes.
//! Rather than warn about that, the privileged helper removes the matching cache link — selected by
//! inode, so the only file it can touch is the blob just dropped. That turns a caveat into a promise.
//!
//! [`FlatpakUnusedCategory`] is deliberately **not** exact. Which runtimes are genuinely unused is
//! flatpak's decision, because it resolves runtime extensions and nix should not reproduce that
//! logic; nix's own derivation is an upper bound used only to show the user what to expect. On top of
//! that, a flatpak deployment is hard links into an ostree repository, so an uninstall frees the
//! objects only when the repository is pruned.
//!
//! # The advisory
//!
//! Pruning that repository needs the `ostree` binary, which is not installed on this machine — so
//! nix can measure 701 MiB of unreferenced objects and cannot safely act on them. Shipping an
//! automated privileged prune that has never once been run would be worse than not shipping one. It
//! is reported through [`crate::space::Advisory`] instead: the size, the reason, and the command.

use crate::error::Result;
use crate::op::CancelToken;
use crate::pkg::{flatpak, snap};
use crate::space::{Advisory, Category as SpaceCategory, ReclaimMethod, Reclaimable, Safety};

use super::registry::{Candidate, Category};

/// Snap revisions snapd has superseded.
#[derive(Debug, Default)]
pub struct SnapRevisionCategory;

impl SnapRevisionCategory {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Category for SnapRevisionCategory {
    fn id(&self) -> &'static str {
        "snap_revisions"
    }

    fn label(&self) -> &'static str {
        "Old snap revisions"
    }

    fn explains(&self) -> &'static str {
        "Superseded versions of installed snaps, which snapd keeps so you can roll back. Removing them means you cannot revert those apps to their previous version. The running revision of each snap is never touched."
    }

    fn space_category(&self) -> SpaceCategory {
        SpaceCategory::PackagePayload
    }

    fn available(&self) -> bool {
        crate::caps::registry().has(crate::caps::Capability::Snap)
    }

    fn candidates(&self, token: &CancelToken) -> Result<Vec<Candidate>> {
        token.check()?;
        let all = snap::revisions()?;

        let mut candidates = Vec::new();
        for revision in snap::removable(&all) {
            token.check()?;
            candidates.push(Candidate {
                // A revision is identified by name and number, not by a path the user would
                // recognise. The blob path is an implementation detail of snapd's.
                path: std::path::PathBuf::from(format!(
                    "snap {} revision {}",
                    revision.name, revision.revision
                )),
                label: format!(
                    "{} {} (revision {})",
                    revision.name, revision.version, revision.revision
                ),
                bytes: revision.bytes,
                // Not `Safe`. A superseded revision is what `snap revert` rolls back to, so removing
                // it gives up the ability to undo a bad refresh. Routine, but a decision.
                safety: Safety::Review,
                method: ReclaimMethod::SnapRevision {
                    package: revision.name.clone(),
                    revision: revision.revision.clone(),
                },
                cost: Some(format!(
                    "You will no longer be able to `snap revert {}` to revision {} if the current one turns out to have a problem.",
                    revision.name, revision.revision
                )),
                category: self.id().to_string(),
                // Exact, because the helper removes snapd's cache link along with the blob. Without
                // that this would have to be qualified — see the module documentation.
                reclaimable: Reclaimable::Exact,
            });
        }

        Ok(candidates)
    }
}

/// Flatpak runtimes no installed application needs.
#[derive(Debug, Default)]
pub struct FlatpakUnusedCategory;

impl FlatpakUnusedCategory {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// The system installation root. A field would let tests point elsewhere, but the privileged
    /// operation is fixed to the system installation, so a configurable root here would only be able
    /// to disagree with it.
    fn root() -> &'static std::path::Path {
        std::path::Path::new(flatpak::SYSTEM_ROOT)
    }
}

impl Category for FlatpakUnusedCategory {
    fn id(&self) -> &'static str {
        "flatpak_unused"
    }

    fn label(&self) -> &'static str {
        "Unused flatpak runtimes"
    }

    fn explains(&self) -> &'static str {
        "Runtimes no installed flatpak still depends on. Downloaded again if you install something that needs one, typically a few hundred megabytes."
    }

    fn space_category(&self) -> SpaceCategory {
        SpaceCategory::PackagePayload
    }

    fn available(&self) -> bool {
        crate::caps::registry().has(crate::caps::Capability::Flatpak)
    }

    fn candidates(&self, token: &CancelToken) -> Result<Vec<Candidate>> {
        token.check()?;
        let refs = flatpak::installed()?;
        let unused = flatpak::unused_runtimes(&refs);
        if unused.is_empty() {
            return Ok(Vec::new());
        }

        // Measured per runtime, then summed, because flatpak offers no way to uninstall them
        // individually as "the unused ones" — the operation is all-or-nothing, so the candidate is
        // too. Presenting per-runtime rows that cannot be selected separately would be a lie about
        // what the button does.
        let mut bytes = 0u64;
        let mut shared = 0u64;
        let mut names = Vec::new();
        for runtime in &unused {
            token.check()?;
            let size = flatpak::measure_tree(&runtime.deploy_dir(Self::root()));
            bytes += size.bytes;
            shared += size.shared_files;
            names.push(format!("{} {}", runtime.id, runtime.branch));
        }

        let count = unused.len();
        Ok(vec![Candidate {
            path: Self::root().to_path_buf(),
            label: format!(
                "{count} unused {}",
                if count == 1 { "runtime" } else { "runtimes" }
            ),
            bytes,
            safety: Safety::Review,
            method: ReclaimMethod::FlatpakUnused,
            cost: Some(format!(
                "Removes {}. Installing an application that needs one again will re-download it.",
                names.join(", ")
            )),
            category: self.id().to_string(),
            reclaimable: Reclaimable::AtMost {
                // Two independent reasons nothing can be promised, either of which alone would be
                // enough: flatpak decides what is genuinely unused, and its deployments are hard
                // links into a repository that outlives them.
                exclusive: None,
                reason: if shared > 0 {
                    "flatpak decides for itself which runtimes are unused, and its files are hard-linked into its repository — so the space returns only when that repository is pruned.".to_string()
                } else {
                    "flatpak decides for itself which runtimes are genuinely unused, so it may keep more than this estimate assumes.".to_string()
                },
            },
        }])
    }

    /// The orphaned ostree repository: measurable, and not something nix will prune itself.
    fn advisories(&self) -> Vec<Advisory> {
        let root = Self::root();
        let repo = root.join("repo");
        if !repo.is_dir() {
            return Vec::new();
        }

        // Objects are only orphaned if little references them. With nothing installed, essentially
        // the whole store is unreferenced — which is the case on this machine, and the case worth
        // telling someone about.
        let installed_refs = flatpak::installed().unwrap_or_default();
        if !installed_refs.is_empty() {
            // With applications installed, how much of the store is unreferenced is ostree's
            // accounting and nix has no honest figure to offer.
            return Vec::new();
        }

        let size = flatpak::measure_tree(&repo);
        if size.bytes == 0 {
            return Vec::new();
        }

        vec![Advisory {
            path: Some(repo),
            label: "Orphaned flatpak repository objects".to_string(),
            bytes: size.bytes,
            // Nothing is installed, so nothing references these objects — that much is certain, and
            // it is why this figure can be stated at all.
            reclaimable: Reclaimable::AtMost {
                exclusive: Some(size.bytes),
                reason: "Nothing is installed from this repository, so its objects are unreferenced."
                    .to_string(),
            },
            why_manual:
                "Pruning an ostree repository needs the `ostree` command, which is not installed. nix will not ship a privileged operation it has never been able to run."
                    .to_string(),
            remedy: "sudo ostree prune --repo=/var/lib/flatpak/repo --refs-only".to_string(),
            category: self.id().to_string(),
        }]
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::op::CancelToken;

    fn token() -> CancelToken {
        CancelToken::new()
    }

    #[test]
    fn identifiers_and_labels_are_present() {
        let snaps = SnapRevisionCategory::new();
        assert_eq!(snaps.id(), "snap_revisions");
        assert!(!snaps.label().is_empty());

        let flat = FlatpakUnusedCategory::new();
        assert_eq!(flat.id(), "flatpak_unused");
        assert!(!flat.label().is_empty());
    }

    /// The specification requires a `Review` entry to say what it costs.
    #[test]
    fn every_review_candidate_explains_its_cost() {
        for candidates in [
            SnapRevisionCategory::new().candidates(&token()),
            FlatpakUnusedCategory::new().candidates(&token()),
        ] {
            let Ok(candidates) = candidates else { continue };
            for c in candidates {
                if c.safety == Safety::Review {
                    let cost = c.cost.as_deref().unwrap_or("");
                    assert!(
                        !cost.is_empty(),
                        "{} offers no reason for its Review rating",
                        c.label
                    );
                }
            }
        }
    }

    #[test]
    fn categories_are_unavailable_without_their_tool() {
        // Cannot be forced either way on this machine, so assert the two agree rather than a value.
        let has_snap = crate::caps::registry().has(crate::caps::Capability::Snap);
        assert_eq!(SnapRevisionCategory::new().available(), has_snap);

        let has_flatpak = crate::caps::registry().has(crate::caps::Capability::Flatpak);
        assert_eq!(FlatpakUnusedCategory::new().available(), has_flatpak);
    }

    /// An unavailable category must not produce candidates through the registry.
    #[test]
    fn nothing_is_offered_when_the_tool_is_absent() {
        let category = FlatpakUnusedCategory::new();
        if category.available() {
            return;
        }
        assert!(category.candidates(&token()).unwrap_or_default().is_empty());
    }

    #[test]
    fn cancellation_is_honoured() {
        let cancelled = CancelToken::new();
        cancelled.cancel();
        for result in [
            SnapRevisionCategory::new().candidates(&cancelled),
            FlatpakUnusedCategory::new().candidates(&cancelled),
        ] {
            match result {
                Err(e) => assert!(!e.is_fault(), "cancellation is not a fault"),
                Ok(found) => assert!(found.is_empty()),
            }
        }
    }

    // ---- against this machine ----

    /// The safety rule, checked against real snapd output.
    #[test]
    fn no_active_snap_revision_is_ever_offered() {
        let category = SnapRevisionCategory::new();
        if !category.available() {
            return;
        }
        let Ok(candidates) = category.candidates(&token()) else {
            return;
        };
        let Ok(all) = snap::revisions() else { return };

        for candidate in &candidates {
            let ReclaimMethod::SnapRevision { package, revision } = &candidate.method else {
                panic!("a snap candidate must use the snap method");
            };
            let matching = all
                .iter()
                .find(|r| &r.name == package && &r.revision == revision)
                .expect("the candidate must correspond to a real revision");
            assert!(
                matching.disabled,
                "{package} revision {revision} is active and must never be offered"
            );
        }
    }

    #[test]
    fn snap_candidates_are_exact_because_the_cache_link_goes_too() {
        let category = SnapRevisionCategory::new();
        if !category.available() {
            return;
        }
        let Ok(candidates) = category.candidates(&token()) else {
            return;
        };
        for candidate in &candidates {
            assert_eq!(
                candidate.reclaimable,
                Reclaimable::Exact,
                "{}: the helper removes snapd's cache link, so this is a promise",
                candidate.label
            );
            assert!(candidate.bytes > 0);
        }
    }

    /// The advisory is the point of this test: 701 MiB that would otherwise be invisible.
    #[test]
    fn an_orphaned_flatpak_repository_is_reported_rather_than_hidden() {
        let category = FlatpakUnusedCategory::new();
        if !category.available() {
            return;
        }
        let repo = std::path::Path::new(flatpak::SYSTEM_ROOT).join("repo");
        if !repo.is_dir() || !flatpak::installed().unwrap_or_default().is_empty() {
            return;
        }

        let advisories = category.advisories();
        assert_eq!(advisories.len(), 1, "the repository holds objects");
        let advisory = &advisories[0];
        assert!(advisory.bytes > 0);
        assert!(
            !advisory.why_manual.is_empty() && !advisory.remedy.is_empty(),
            "an advisory without a reason and a remedy is just a refusal"
        );
        assert!(
            advisory.remedy.contains("ostree"),
            "the remedy should name the tool: {}",
            advisory.remedy
        );
    }
}
