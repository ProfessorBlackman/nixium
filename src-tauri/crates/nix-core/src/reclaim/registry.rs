// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The category registry. Task 1.9 (`STO-3`).
//!
//! This module is the difference between nine categories and nine special cases. Each category
//! declares the same five things — its roots, how it enumerates, how it rates safety, how it
//! reclaims, and what it costs — so Phase 2's nine additions are nine small implementations rather
//! than nine bespoke code paths, and each is independently shippable and independently testable.
//!
//! Stacer's cleaner had five hardcoded checkboxes wired directly to five bespoke branches, which is
//! why adding a sixth category there meant touching the scan function, the clean function, the tree
//! builder and the UI.

use std::path::PathBuf;

use crate::error::Result;
use crate::op::CancelToken;
use crate::space::{Advisory, Category as SpaceCategory, ReclaimMethod, Reclaimable, Safety};

/// Something a category proposes reclaiming.
///
/// A proposal, not a decision: the protection rules and the preview both get a say before a user
/// ever sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    /// What the user sees. Should name the *thing*, not the path — "Firefox cache", not
    /// "~/.cache/mozilla".
    pub label: String,
    /// On-disk bytes. Never the apparent size: this figure is a promise about what comes back.
    pub bytes: u64,
    pub safety: Safety,
    pub method: ReclaimMethod,
    /// What reclaiming costs. **Required for `Review`** — a rating that says "this has a cost"
    /// without saying what it is gives a user nothing to decide with.
    pub cost: Option<String>,
    /// Which category proposed this.
    pub category: String,
    /// How much of `bytes` will actually come back, when the category knows better than the
    /// filesystem-level guess the preview would otherwise make.
    ///
    /// Defaults to [`Reclaimable::Exact`]; the preview downgrades it on a copy-on-write filesystem.
    /// A category that understands its own sharing — a snapshot-aware one — sets it here and its
    /// judgement wins.
    pub reclaimable: Reclaimable,
}

/// One kind of reclaimable space.
pub trait Category: Send + Sync {
    /// Stable identifier, used in reports and settings.
    fn id(&self) -> &'static str;

    /// What the user sees.
    fn label(&self) -> &'static str;

    /// Which space-model category these belong to.
    fn space_category(&self) -> SpaceCategory;

    /// Whether this category can run on this system. A category whose backing tool is absent
    /// reports `false` rather than producing an empty list, so the UI can say why.
    fn available(&self) -> bool {
        true
    }

    /// Space this category can see but will not act on itself.
    ///
    /// Defaults to none. A category overrides it when it can measure something real whose remedy is
    /// a tool nix does not have, or has not exercised — see [`Advisory`] for why that is reported
    /// rather than hidden.
    fn advisories(&self) -> Vec<Advisory> {
        Vec::new()
    }

    /// Find what could be reclaimed. Must honour cancellation.
    fn candidates(&self, token: &CancelToken) -> Result<Vec<Candidate>>;
}

/// Every registered category.
#[derive(Default)]
pub struct Registry {
    categories: Vec<Box<dyn Category>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("categories", &self.ids())
            .finish()
    }
}

impl Registry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The categories implemented so far.
    ///
    /// M3 registered trash alone, so the pipeline was proven against the one category where a
    /// mistake is recoverable. M4 adds the categories that actually hold space, three of which go
    /// through the privileged helper.
    #[must_use]
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(TrashCategory::new()));
        registry.register(Box::new(super::AppCacheCategory::new()));
        registry.register(Box::new(super::LogCategory::new()));
        registry.register(Box::new(super::JournalCategory::new()));
        registry.register(Box::new(super::PackageCacheCategory::new()));
        // Phase 2 opens with the largest real-world win: old kernels.
        registry.register(Box::new(super::OldKernelCategory::new()));
        registry.register(Box::new(super::ResidualConfigCategory::new()));
        // `STO-12`: the largest single figure found so far on this machine — 3.3 GiB of superseded
        // snap revisions.
        registry.register(Box::new(super::SnapRevisionCategory::new()));
        registry.register(Box::new(super::FlatpakUnusedCategory::new()));
        // `STO-14`: the largest category in the tool — 71 GiB of project artifacts and 52 GiB of
        // package stores on the development machine.
        registry.register(Box::new(super::BuildArtifactCategory::new()));
        registry.register(Box::new(super::PackageStoreCategory::new()));
        // `STO-13`: 17.5 GB of images and 3 GB of build cache on the development machine.
        registry.register(Box::new(super::ContainerCategory::new()));
        registry
    }

    pub fn register(&mut self, category: Box<dyn Category>) {
        self.categories.push(category);
    }

    #[must_use]
    pub fn ids(&self) -> Vec<&'static str> {
        self.categories.iter().map(|c| c.id()).collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.categories.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.categories.is_empty()
    }

    /// Collect candidates from every available category.
    ///
    /// A category that fails is logged and skipped rather than aborting the whole scan: one broken
    /// backend must not deny the user everything else that was found.
    pub fn collect(&self, token: &CancelToken) -> Result<Vec<Candidate>> {
        let mut all = Vec::new();
        for category in &self.categories {
            token.check()?;
            if !category.available() {
                tracing::debug!(
                    category = category.id(),
                    "category unavailable on this system"
                );
                continue;
            }
            match category.candidates(token) {
                Ok(found) => all.extend(found),
                Err(e) if !e.is_fault() => return Err(e), // cancellation propagates
                Err(e) => {
                    tracing::warn!(category = category.id(), error = %e, "category failed");
                }
            }
        }
        Ok(all)
    }

    /// Every category's advisories.
    ///
    /// Unlike [`Registry::collect`] this does not take a cancellation token: an advisory is derived
    /// from figures a category already has, so there is nothing long-running to cancel.
    #[must_use]
    pub fn collect_advisories(&self) -> Vec<Advisory> {
        self.categories
            .iter()
            .filter(|c| c.available())
            .flat_map(|c| c.advisories())
            .collect()
    }
}

/// Trash, as a reclaimable category. Task 1.10.
///
/// The first and, in M3, only category. Chosen because emptying trash is the one destructive
/// operation whose consequences a user has already accepted — they put those files there.
pub struct TrashCategory {
    /// Overridable for tests.
    dir: Option<crate::trash::TrashDir>,
}

impl TrashCategory {
    #[must_use]
    pub fn new() -> Self {
        Self { dir: None }
    }

    /// A category over an explicit trash directory.
    #[must_use]
    pub fn at(dir: crate::trash::TrashDir) -> Self {
        Self { dir: Some(dir) }
    }

    fn resolve(&self) -> Result<crate::trash::TrashDir> {
        match &self.dir {
            Some(dir) => Ok(dir.clone()),
            None => crate::trash::TrashDir::home(),
        }
    }
}

impl Default for TrashCategory {
    fn default() -> Self {
        Self::new()
    }
}

impl Category for TrashCategory {
    fn id(&self) -> &'static str {
        "trash"
    }

    fn label(&self) -> &'static str {
        "Trash"
    }

    fn space_category(&self) -> SpaceCategory {
        SpaceCategory::Trash
    }

    fn candidates(&self, token: &CancelToken) -> Result<Vec<Candidate>> {
        token.check()?;
        let dir = self.resolve()?;
        let items = crate::trash::list(&dir);
        if items.is_empty() {
            return Ok(Vec::new());
        }

        let bytes = dir.size();
        let count = items.len();

        // One candidate for the whole trash rather than one per file. Emptying is all-or-nothing at
        // the spec level, and a user deciding about their trash is deciding about the trash.
        Ok(vec![Candidate {
            path: dir.root().to_path_buf(),
            label: format!("Trash ({count} item{})", if count == 1 { "" } else { "s" }),
            bytes,
            // Not `Safe`: these are the user's own files, and emptying is irreversible. Rating it
            // Safe would pre-check it in a quick clean, which is not a decision to make for someone.
            safety: Safety::Review,
            method: ReclaimMethod::TrashEmpty {
                volume: dir.root().to_path_buf(),
            },
            cost: Some(format!(
                "Permanently deletes {count} item{} you moved to the trash. They cannot be restored afterwards.",
                if count == 1 { "" } else { "s" }
            )),
            category: self.id().to_string(),
            reclaimable: Reclaimable::Exact,
        }])
    }
}
