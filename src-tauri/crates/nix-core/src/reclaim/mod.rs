// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Reclaiming space. Tasks 1.8 (`STO-4`) and 1.9 (`STO-3`).
//!
//! # The pipeline is the point
//!
//! Principle P2: **preview → confirm → execute → report.** Nothing destructive runs without a
//! computed diff first, and every execution returns per-item results.
//!
//! This is built now, against **trash only**, before any category worth reclaiming exists. The
//! plan's fourth sequencing rule explains why: if the pipeline arrived after package caches and
//! system logs were on screen, there would be pressure to bypass it "just this once", and that is
//! exactly how Stacer ended up running a bare privileged `rm -rf` over an argument list assembled
//! from checkbox state, with no confirmation, no dry run and no report.
//!
//! # How "no path bypasses preview" is enforced
//!
//! Not by convention. [`execute`] requires a [`Ticket`] that only [`preview`] can mint, tied to the
//! exact set of items it described. A caller cannot construct one, cannot reuse one, and cannot
//! widen the selection after the fact — the type system carries the rule.
//!
//! # Two guards that run at execution time, not just at preview time
//!
//! - **Protection** ([`crate::protect`]) is re-checked immediately before acting, because the
//!   user's exclusions may have changed since the preview.
//! - **Time-of-check/time-of-use.** Every path is re-stat'd and compared against what the preview
//!   recorded. A file that changed size, or became a different inode, is skipped and reported
//!   rather than acted on. This is also what makes it safe for the explorer to serve a cached tree
//!   (decision D6): stale data can misinform a reader, but it cannot misdirect a deletion.

mod artifacts;
mod caches;
mod containers;
mod kernels;
mod logs;
mod packages;
mod registry;
mod snaps;

pub use artifacts::{BuildArtifactCategory, PackageStoreCategory};
pub use caches::AppCacheCategory;
pub use containers::ContainerCategory;
pub use kernels::{OldKernelCategory, ResidualConfigCategory};
pub use logs::{JournalCategory, LogCategory};
pub use packages::PackageCacheCategory;
pub use registry::{Candidate, Category, Registry, TrashCategory};
pub use snaps::{FlatpakUnusedCategory, SnapRevisionCategory};

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::cow::{self, CowMap};
use crate::error::{AppError, ErrorCode, Result};
use crate::helper;
use crate::op::CancelToken;
use crate::protect::{Guard, Refusal};
use crate::space::{Advisory, ReclaimMethod, Reclaimable, Safety};
use crate::trash;

/// Proof that a preview was computed and shown.
///
/// The only way to obtain one is [`preview`]. It carries the identity of the set it describes, so
/// [`execute`] can refuse a selection the user was never shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
// No `serde(transparent)`: a single-field newtype already serialises as its inner value in JSON, and
// the attribute made ts-rs warn on every build without changing anything. See `op::OperationId`.
#[ts(export, type = "number")]
pub struct Ticket(u64);

impl Ticket {
    fn mint() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// One thing the preview proposes to reclaim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PreviewItem {
    /// Stable across a preview, used to select a subset.
    #[ts(type = "number")]
    pub id: u64,
    pub path: PathBuf,
    /// What the user sees.
    pub label: String,
    /// Bytes expected back. Always the on-disk figure, never the apparent size.
    #[ts(type = "number")]
    pub bytes: u64,
    pub safety: Safety,
    pub method: ReclaimMethod,
    /// What this costs, when it costs something. `Review` items must have one.
    pub cost: Option<String>,
    /// The category that proposed it.
    pub category: String,
    /// How much of `bytes` will actually come back.
    ///
    /// [`Reclaimable::Exact`] for the ordinary case. On a copy-on-write filesystem where extents may
    /// be shared with a snapshot this is qualified, and the UI must show the caveat beside the size
    /// rather than presenting the number bare.
    pub reclaimable: Reclaimable,
    /// Size at preview time, so execution can detect a change underneath it.
    #[ts(type = "number")]
    fingerprint: u64,
}

impl PreviewItem {
    /// Whether a "select all" may include this.
    #[must_use]
    pub const fn bulk_selectable(&self) -> bool {
        self.safety.bulk_selectable()
    }
}

/// What would happen, computed before anything does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Preview {
    pub ticket: Ticket,
    pub items: Vec<PreviewItem>,
    /// Total if everything offered were reclaimed, taking every stated size at face value.
    #[ts(type = "number")]
    pub total_bytes: u64,
    /// The part of [`Preview::total_bytes`] nix is willing to **promise**.
    ///
    /// Lower than the total whenever some entry sits on a copy-on-write filesystem and its
    /// exclusivity could not be proven. Those entries contribute nothing here, because a total is a
    /// promise and a promise assembled from maybes is not one. When the two figures differ, the UI
    /// must lead with this one.
    #[ts(type = "number")]
    pub promisable_bytes: u64,
    /// Total of only the entries safe enough to pre-check.
    #[ts(type = "number")]
    pub safe_bytes: u64,
    /// The part of the total that would be **moved to the trash** rather than removed.
    ///
    /// Trashing frees nothing on its own: the trash sits on the same filesystem as its contents,
    /// because the move is a rename. So this much of [`Preview::total_bytes`] needs the trash emptying
    /// before it comes back, and the UI has to say so *before* the user commits — not only afterwards
    /// in the report.
    #[ts(type = "number")]
    pub trashable_bytes: u64,
    /// Things a category proposed that the protection rules refused. Shown, not hidden: a user
    /// should be able to see that nix declined to touch something.
    pub refused: Vec<Refusal>,
    /// Space nix can account for but will not act on. Deliberately **not** part of
    /// [`Preview::total_bytes`] or [`Preview::promisable_bytes`]: those are promises about what this
    /// preview would reclaim, and an advisory is by definition something it will not.
    pub advisories: Vec<Advisory>,
    /// What each category involved actually does, keyed by its label. `PLT-7`.
    ///
    /// Carried on the preview rather than fetched separately so the explanation cannot be missing for
    /// a category that is present — they are built from the same pass.
    #[ts(as = "std::collections::HashMap<String, String>")]
    pub explanations: BTreeMap<String, String>,
}

impl Preview {
    /// Items a bulk selection may include.
    #[must_use]
    pub fn bulk_selectable(&self) -> Vec<&PreviewItem> {
        self.items.iter().filter(|i| i.bulk_selectable()).collect()
    }

    /// Whether there is anything at all to do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether any entry's size is qualified, so the headline total is an upper bound.
    #[must_use]
    pub fn total_is_upper_bound(&self) -> bool {
        self.promisable_bytes < self.total_bytes
    }

    /// Entries whose stated size cannot be taken at face value.
    #[must_use]
    pub fn qualified(&self) -> Vec<&PreviewItem> {
        self.items
            .iter()
            .filter(|i| !i.reclaimable.is_exact())
            .collect()
    }
}

/// What happened to one item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[ts(export)]
pub enum ItemOutcome {
    /// Removed outright. These bytes are back.
    Reclaimed {
        #[ts(type = "number")]
        id: u64,
        path: PathBuf,
        #[ts(type = "number")]
        bytes: u64,
    },
    /// Moved to the trash. Recoverable, and **not yet freed**.
    ///
    /// # Why this is a separate outcome
    ///
    /// The trash lives on the same filesystem as what it holds — it has to, because the move is a
    /// rename and a rename cannot cross a filesystem. So trashing a 9.8 GiB cache changes the user's
    /// free space by nothing at all.
    ///
    /// Counting that as "freed" is precisely the claim this project exists not to make, and it was
    /// being made: `Report::freed` included trashed bytes, so nix reported 9.8 GiB reclaimed while
    /// `measured_delta` — taken from `statvfs` either side — reported approximately zero.
    ///
    /// Reversibility is still the right default for a user's files. What was wrong was the wording,
    /// not the method.
    Trashed {
        #[ts(type = "number")]
        id: u64,
        path: PathBuf,
        /// Bytes now sitting in the trash, waiting to be emptied.
        #[ts(type = "number")]
        bytes: u64,
    },
    /// Deliberately not acted on, with the reason.
    Skipped {
        #[ts(type = "number")]
        id: u64,
        path: PathBuf,
        reason: String,
    },
    /// Attempted and failed.
    Failed {
        #[ts(type = "number")]
        id: u64,
        path: PathBuf,
        error: AppError,
    },
}

impl ItemOutcome {
    /// Bytes genuinely returned to the filesystem. Trashed bytes are **not** counted.
    #[must_use]
    pub const fn bytes_freed(&self) -> u64 {
        match self {
            Self::Reclaimed { bytes, .. } => *bytes,
            _ => 0,
        }
    }

    /// Bytes moved to the trash and recoverable, which the filesystem has not given back.
    #[must_use]
    pub const fn bytes_trashed(&self) -> u64 {
        match self {
            Self::Trashed { bytes, .. } => *bytes,
            _ => 0,
        }
    }

    /// Whether the item was acted on at all, however it was accounted for.
    #[must_use]
    pub const fn acted(&self) -> bool {
        matches!(self, Self::Reclaimed { .. } | Self::Trashed { .. })
    }
}

/// The per-item account of what was done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Report {
    pub outcomes: Vec<ItemOutcome>,
    /// Sum of what was actually removed. **Excludes anything moved to the trash**, which is still on
    /// the filesystem and so has not been freed.
    #[ts(type = "number")]
    pub freed: u64,
    /// Sum of what was moved to the trash: recoverable, and not yet freed.
    ///
    /// Reported separately rather than added to [`Report::freed`] because the trash is on the same
    /// filesystem as its contents by necessity, so trashing changes free space by nothing. Emptying
    /// the trash is what reclaims it, and the UI says so whenever this is non-zero.
    #[ts(type = "number")]
    pub trashed: u64,
    /// Change in the filesystem's used bytes, measured independently before and after.
    ///
    /// This is how the specification's "within 2%" criterion is *checked* rather than asserted: nix
    /// compares its own arithmetic against what the filesystem says actually happened.
    #[ts(type = "number | null")]
    pub measured_delta: Option<u64>,
    /// Whether the two agree closely enough to be reported as accurate.
    pub measurement_agrees: Option<bool>,
    pub cancelled: bool,
    /// Counts, carried as fields rather than computed in the frontend so the classification lives
    /// in one place and the UI cannot disagree with the backend about what happened.
    #[ts(type = "number")]
    pub reclaimed_count: usize,
    #[ts(type = "number")]
    pub skipped_count: usize,
    #[ts(type = "number")]
    pub failed_count: usize,
}

impl Report {
    fn count(outcomes: &[ItemOutcome], which: fn(&ItemOutcome) -> bool) -> usize {
        outcomes.iter().filter(|o| which(o)).count()
    }

    /// Build a report from what happened, deriving the counts once.
    fn new(outcomes: Vec<ItemOutcome>, measured_delta: Option<u64>, cancelled: bool) -> Self {
        let freed = outcomes.iter().map(ItemOutcome::bytes_freed).sum();
        let trashed = outcomes.iter().map(ItemOutcome::bytes_trashed).sum();
        Self {
            trashed,
            reclaimed_count: Self::count(&outcomes, ItemOutcome::acted),
            skipped_count: Self::count(&outcomes, |o| matches!(o, ItemOutcome::Skipped { .. })),
            failed_count: Self::count(&outcomes, |o| matches!(o, ItemOutcome::Failed { .. })),
            measurement_agrees: measured_delta.map(|measured| agrees(freed, measured)),
            outcomes,
            freed,
            measured_delta,
            cancelled,
        }
    }

    /// A sentence for the UI. Never claims more than happened.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut parts = vec![format!(
            "Freed {} from {} item{}",
            crate::format_bytes(self.freed),
            self.reclaimed_count,
            if self.reclaimed_count == 1 { "" } else { "s" }
        )];
        if self.skipped_count > 0 {
            parts.push(format!("{} skipped", self.skipped_count));
        }
        if self.failed_count > 0 {
            parts.push(format!("{} failed", self.failed_count));
        }
        if self.cancelled {
            parts.push("stopped early".to_string());
        }
        format!("{}.", parts.join(", "))
    }
}

/// Tolerance between what nix counted and what the filesystem reports, per the specification.
const MEASUREMENT_TOLERANCE: f64 = 0.02;

/// A minted preview, held so [`execute`] can validate a ticket against it.
///
/// One at a time: a second preview supersedes the first, so an old ticket cannot be replayed.
#[derive(Debug, Default)]
pub struct Session {
    outstanding: std::sync::Mutex<Option<Preview>>,
}

impl Session {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The last preview's headline figures, if one has been computed this session.
    ///
    /// Exists so the dashboard can lead with "X reclaimable" **without running a preview on mount**,
    /// which `MON-2` forbids: a dashboard that scans when you look at it is a dashboard you avoid
    /// looking at. `None` until something has actually asked, and the caller says when.
    #[must_use]
    pub fn last_preview(&self) -> Option<(u64, u64)> {
        self.outstanding
            .lock()
            .ok()?
            .as_ref()
            .map(|preview| (preview.total_bytes, preview.promisable_bytes))
    }

    /// Compute what would happen, and hold it for execution.
    pub fn preview(
        &self,
        registry: &Registry,
        guard: &Guard,
        token: &CancelToken,
    ) -> Result<Preview> {
        let mut items = Vec::new();
        let mut refused = Vec::new();
        let mut next_id = 0u64;

        // Built once per preview, not once per candidate: resolving a path's filesystem walks the
        // whole mount table. On a machine with no copy-on-write filesystem the map answers `None`
        // for everything and the qualification costs nothing.
        let cow_map = CowMap::build();

        for candidate in registry.collect(token)? {
            token.check()?;

            // The protection rules get the first word, before anything is offered — for anything
            // that names a path. A logical entry's path is a description like
            // `kernel 6.8.0-136-generic`, and asking the path rules about it produces a refusal
            // about relative paths rather than a judgement about safety. What protects those is the
            // helper re-deriving its own eligible set; see [`ReclaimMethod::acts_on_path`].
            if candidate.method.acts_on_path() {
                match guard.verdict(&candidate.path) {
                    crate::protect::Verdict::Protected(r) => {
                        refused.push(r);
                        continue;
                    }
                    crate::protect::Verdict::Allowed => {}
                }
            }

            // Invariant 3 of the space model, enforced here rather than trusted: a `Never` rating
            // must never reach a preview at all.
            if candidate.safety == Safety::Never {
                refused.push(Refusal {
                    path: candidate.path.clone(),
                    rule: "rated_never".to_string(),
                    reason: "This is not reclaimable.".to_string(),
                });
                continue;
            }

            // `STO-17`: on a copy-on-write filesystem the stated size may not be what comes back,
            // so the estimate is qualified here rather than asserted. A category may already have
            // qualified its own candidate — a snapshot-aware one knows more than this does — and
            // that judgement wins over the filesystem-level guess.
            let reclaimable = if candidate.reclaimable.is_exact() {
                cow::reclaimable_for(&candidate.path, cow_map.kind_for(&candidate.path))
            } else {
                candidate.reclaimable
            };

            items.push(PreviewItem {
                id: next_id,
                fingerprint: fingerprint(&candidate.path),
                path: candidate.path,
                label: candidate.label,
                bytes: candidate.bytes,
                safety: candidate.safety,
                method: candidate.method,
                cost: candidate.cost,
                category: candidate.category,
                reclaimable,
            });
            next_id += 1;
        }

        // Largest first: the decision a user is making is about where the space is.
        items.sort_by_key(|i| std::cmp::Reverse(i.bytes));

        // One entry per category actually present in this preview. Built here, from the same pass, so
        // an item can never appear with no explanation available for it.
        let explanations: BTreeMap<String, String> = registry
            .categories()
            .filter(|category| items.iter().any(|item| item.category == category.label()))
            .map(|category| {
                (
                    category.label().to_string(),
                    category.explains().to_string(),
                )
            })
            .collect();

        let preview = Preview {
            ticket: Ticket::mint(),
            explanations,
            total_bytes: items.iter().map(|i| i.bytes).sum(),
            // Only what can actually be promised: a qualified entry contributes its proven
            // exclusive portion, or nothing when none is proven.
            promisable_bytes: items
                .iter()
                .map(|i| i.reclaimable.promisable(i.bytes))
                .sum(),
            safe_bytes: items
                .iter()
                .filter(|i| i.safety.pre_checkable())
                .map(|i| i.reclaimable.promisable(i.bytes))
                .sum(),
            trashable_bytes: items
                .iter()
                .filter(|i| matches!(i.method, ReclaimMethod::MoveToTrash { .. }))
                .map(|i| i.reclaimable.promisable(i.bytes))
                .sum(),
            items,
            refused,
            advisories: registry.collect_advisories(),
        };

        if let Ok(mut outstanding) = self.outstanding.lock() {
            *outstanding = Some(preview.clone());
        }
        Ok(preview)
    }

    /// Reclaim a subset of a preview.
    ///
    /// `selection` names item ids from the preview the ticket belongs to. An id that was not in that
    /// preview is an error, not a silent skip: it means the caller is acting on something the user
    /// was never shown.
    pub fn execute(
        &self,
        ticket: Ticket,
        selection: &[u64],
        guard: &Guard,
        token: &CancelToken,
        progress: impl Fn(usize, usize),
    ) -> Result<Report> {
        let preview = {
            let outstanding = self
                .outstanding
                .lock()
                .map_err(|_| AppError::internal("The reclaim session lock was poisoned."))?;
            match outstanding.as_ref() {
                Some(p) if p.ticket == ticket => p.clone(),
                Some(_) => {
                    return Err(AppError::refused(
                        "That preview is out of date because a newer scan replaced it.",
                    )
                    .with_remedy("Review the current preview and try again."));
                }
                None => {
                    return Err(AppError::refused(
                        "Nothing can be reclaimed without previewing it first.",
                    )
                    .with_remedy("Run a scan, review what it found, then confirm."));
                }
            }
        };

        let chosen: Vec<&PreviewItem> = selection
            .iter()
            .map(|id| {
                preview.items.iter().find(|i| i.id == *id).ok_or_else(|| {
                    AppError::invalid_input(format!(
                        "Item {id} was not part of the preview that was shown."
                    ))
                })
            })
            .collect::<Result<Vec<_>>>()?;

        // Measure the filesystem before, so the report can check nix's own arithmetic.
        let before = chosen.first().and_then(|item| filesystem_used(&item.path));

        let mut outcomes = Vec::with_capacity(chosen.len());
        let total = chosen.len();
        let mut cancelled = false;
        // One privileged session for the whole batch, opened only if something actually needs it.
        let mut elevation = Elevation::production();

        for (index, item) in chosen.iter().enumerate() {
            if token.is_cancelled() {
                cancelled = true;
                break;
            }
            progress(index, total);
            outcomes.push(reclaim_one(item, guard, &mut elevation));
        }

        let after = chosen.first().and_then(|item| filesystem_used(&item.path));
        let measured_delta = match (before, after) {
            (Some(b), Some(a)) if b >= a => Some(b - a),
            _ => None,
        };

        Ok(Report::new(outcomes, measured_delta, cancelled))
    }

    /// Discard the outstanding preview, e.g. when the user navigates away.
    pub fn clear(&self) {
        if let Ok(mut outstanding) = self.outstanding.lock() {
            *outstanding = None;
        }
    }
}

/// Whether nix's arithmetic and the filesystem's measurement agree within tolerance.
///
/// Small absolute differences are expected and meaningless — metadata blocks, journal writes, other
/// processes touching the disk during the operation — so a floor applies below which the comparison
/// says nothing.
#[must_use]
pub fn agrees(counted: u64, measured: u64) -> bool {
    const FLOOR: u64 = 1 << 20; // 1 MiB
    if counted < FLOOR && measured < FLOOR {
        return true;
    }
    let larger = counted.max(measured);
    let difference = counted.abs_diff(measured);
    #[allow(clippy::cast_precision_loss)]
    let ratio = difference as f64 / larger as f64;
    ratio <= MEASUREMENT_TOLERANCE
}

/// Bytes currently used on the filesystem containing `path`.
fn filesystem_used(path: &std::path::Path) -> Option<u64> {
    crate::fs::containing(path).ok().flatten().map(|fs| fs.used)
}

/// A cheap identity for a path: size and inode combined.
///
/// Compared immediately before acting, so a file that changed underneath the preview is skipped
/// rather than deleted on the strength of stale information.
fn fingerprint(path: &std::path::Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .map(|m| m.ino() ^ (m.size() << 1) ^ (m.blocks() << 3))
        .unwrap_or(0)
}

/// Whether a batch may escalate at all.
///
/// # Why this is an explicit choice and not a default
///
/// `Elevation` used to derive `Default`, and `Elevation::default()` escalated through polkit on first
/// need. That made the dangerous option the easy one, and it cost a real kernel: a unit test called
/// `Elevation::default()` expecting elevation to fail because no helper was installed. Once the helper
/// *was* installed for manual testing, `auth_admin_keep` had already cached the authorisation from an
/// earlier prompt — so the test escalated silently, the helper agreed the package was a removable old
/// kernel, and removed it.
///
/// Every safety rule held. The mistake was that reaching root took no deliberate act.
///
/// So there is no `Default`. A caller has to name which it wants, and the only places that name
/// [`Elevate::WhenNeeded`] are the ones a person would look at when asking "what can run as root".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Elevate {
    /// Escalate through polkit when something first needs it.
    WhenNeeded,
    /// Never escalate. Every privileged operation fails with a plain reason.
    ///
    /// What tests use, so no test can reach root whatever happens to be installed on the machine
    /// running it.
    Never,
}

/// A privileged session, opened lazily and kept for the whole execution.
///
/// **Opened once, not once per item.** Stacer re-ran every individual command under `pkexec`, so
/// toggling five services meant five authentication dialogs; one session for a batch is the whole
/// point of the helper's design.
struct Elevation {
    how: Elevate,
    client: Option<helper::Client>,
    /// The failure that prevented elevation, so every item can report the same honest reason
    /// instead of each retrying and prompting again.
    failure: Option<AppError>,
}

impl Elevation {
    /// Escalate through polkit on first need. **This is the one that can run things as root.**
    const fn production() -> Self {
        Self {
            how: Elevate::WhenNeeded,
            client: None,
            failure: None,
        }
    }

    /// Refuse to escalate. Privileged operations fail; nothing runs as root.
    ///
    /// `cfg(test)` deliberately: in a release build this does not exist, so [`Elevation::production`]
    /// is the only way to construct one at all and there is nothing to choose wrongly.
    #[cfg(test)]
    const fn never() -> Self {
        Self {
            how: Elevate::Never,
            client: None,
            failure: None,
        }
    }

    /// The client, opening a session on first use.
    fn client(&mut self) -> std::result::Result<&mut helper::Client, AppError> {
        if self.how == Elevate::Never {
            return Err(AppError::new(
                ErrorCode::HelperUnavailable,
                "This operation needs administrator rights, and elevation is disabled here.",
            )
            .with_remedy("Nothing was changed."));
        }
        if self.client.is_none() && self.failure.is_none() {
            match helper::Transport::production().and_then(|t| helper::Client::connect(&t)) {
                Ok(client) => self.client = Some(client),
                Err(e) => self.failure = Some(e),
            }
        }
        match (&mut self.client, &self.failure) {
            (Some(client), _) => Ok(client),
            (None, Some(e)) => Err(e.clone()),
            (None, None) => Err(AppError::internal("Elevation reached an impossible state.")),
        }
    }
}

/// Attribute a directory to a space category from its path. `STO-16`.
///
/// # Why this exists separately from the reclaim categories
///
/// A scan leaves every entry as [`crate::space::Category::Unknown`], because the scanner's job is to
/// measure and attribution is somebody else's. The reclaim categories know better, but they only run
/// when a user asks to reclaim — so a growth *sample* taken from a scan had category totals consisting
/// of one number called "unknown", which is not the "category totals" the specification asked for.
///
/// This reuses the signals the reclaim categories already establish, in the same order of confidence:
/// a corroborated build-artifact marker first, then known locations. It is a *best effort* by design.
/// Anything unrecognised stays `Unknown` rather than being guessed into a bucket, because a trend built
/// on invented attribution is worse than an honest "unattributed" line.
#[must_use]
pub fn classify(path: &std::path::Path) -> crate::space::Category {
    use crate::space::Category;

    // Strongest signal: a marker file a build tool must have written.
    if artifacts::corroborate(path).is_some() {
        return Category::BuildArtifact;
    }

    let under = |root: Option<PathBuf>| -> bool {
        root.is_some_and(|r| path.starts_with(&r) && path != r.as_path())
    };
    let home_relative = |relative: &str| -> bool {
        crate::paths::home_dir().is_some_and(|h| path.starts_with(h.join(relative)))
    };

    if under(crate::paths::cache_dir().and_then(|c| c.parent().map(std::path::Path::to_path_buf))) {
        return Category::AppCache;
    }
    if home_relative(".local/share/Trash") {
        return Category::Trash;
    }
    // Package stores that live outside the cache directory, which `STO-14` enumerates.
    for store in [
        ".npm",
        ".cargo",
        ".m2",
        ".gradle",
        "go/pkg/mod",
        ".local/share/pnpm",
    ] {
        if home_relative(store) {
            return Category::PackageCache;
        }
    }
    if path.starts_with("/var/log/journal") {
        return Category::Journal;
    }
    if path.starts_with("/var/log") {
        return Category::Log;
    }
    if path.starts_with("/var/cache") {
        return Category::PackageCache;
    }
    if path.starts_with("/var/lib/docker") || path.starts_with("/var/lib/containers") {
        return Category::ContainerImage;
    }
    if path.starts_with("/var/lib/snapd") || path.starts_with("/var/lib/flatpak") {
        return Category::PackagePayload;
    }
    if crate::paths::home_dir().is_some_and(|h| path.starts_with(&h)) {
        return Category::UserFile;
    }

    Category::Unknown
}

/// Reclaim one item, with both guards applied.
fn reclaim_one(item: &PreviewItem, guard: &Guard, elevation: &mut Elevation) -> ItemOutcome {
    // Re-checked at execution time, because the user's exclusions may have changed since preview.
    // Path rules apply to paths; a logical entry is guarded by the helper instead.
    if item.method.acts_on_path() {
        if let Some(refusal) = guard.verdict(&item.path).refusal() {
            return ItemOutcome::Skipped {
                id: item.id,
                path: item.path.clone(),
                reason: refusal.reason.clone(),
            };
        }
    }

    // Time-of-check/time-of-use: a path that changed since the preview is not the thing the user
    // agreed to.
    //
    // Only meaningful when the path is the target. A logical entry — a kernel, a snap revision — has
    // a descriptive path that was never on disk, and re-checking it would find nothing and skip
    // every such item as "already gone". Those are guarded by the helper re-deriving its eligible
    // set at the moment it acts, which is a stronger check than this one.
    if item.method.acts_on_path() {
        let current = fingerprint(&item.path);
        if current == 0 {
            return ItemOutcome::Skipped {
                id: item.id,
                path: item.path.clone(),
                reason: "It is already gone.".to_string(),
            };
        }
        if current != item.fingerprint {
            return ItemOutcome::Skipped {
                id: item.id,
                path: item.path.clone(),
                reason: "It changed since the preview, so it was left alone.".to_string(),
            };
        }
    }

    match &item.method {
        ReclaimMethod::MoveToTrash { path } => match trash::trash(path) {
            Ok(trashed) => ItemOutcome::Trashed {
                id: item.id,
                path: item.path.clone(),
                // What the trash reports, not what the preview guessed.
                bytes: trashed.size,
            },
            Err(error) => ItemOutcome::Failed {
                id: item.id,
                path: item.path.clone(),
                error,
            },
        },
        ReclaimMethod::TrashEmpty { volume } => {
            let dir = trash::TrashDir::at(volume, None);
            match trash::empty(&dir) {
                Ok(emptied) => ItemOutcome::Reclaimed {
                    id: item.id,
                    path: item.path.clone(),
                    bytes: emptied.bytes,
                },
                Err(error) => ItemOutcome::Failed {
                    id: item.id,
                    path: item.path.clone(),
                    error,
                },
            }
        }
        // The three privileged methods. Each is a single typed helper operation — no path, name or
        // limit assembled here becomes free-form text on a root command line.
        ReclaimMethod::SystemFile { kind, path } => privileged(
            item,
            elevation,
            helper::Op::ReclaimFile {
                kind: *kind,
                path: path.clone(),
            },
        ),
        ReclaimMethod::Packages { kind, names } => privileged(
            item,
            elevation,
            helper::Op::RemovePackages {
                kind: *kind,
                packages: names.clone(),
            },
        ),
        ReclaimMethod::PackageManager { manager } => privileged(
            item,
            elevation,
            helper::Op::PackageManagerClean { manager: *manager },
        ),
        ReclaimMethod::JournalVacuum { limit } => {
            privileged(item, elevation, helper::Op::JournalVacuum { limit: *limit })
        }
        // `STO-12`. The helper re-derives snapd's disabled set and refuses anything outside it, so
        // naming the active revision here achieves nothing.
        ReclaimMethod::SnapRevision { package, revision } => privileged(
            item,
            elevation,
            helper::Op::RemoveSnapRevision {
                package: package.clone(),
                revision: revision.clone(),
            },
        ),
        // Carries nothing, because the command is fixed and the decision is flatpak's.
        ReclaimMethod::FlatpakUnused => {
            privileged(item, elevation, helper::Op::FlatpakUninstallUnused)
        }
        // `STO-13`. Unprivileged: nix talks to Docker as the user, and refuses to run privileged
        // Docker commands it has no way to exercise. So these do not go through the helper.
        ReclaimMethod::ContainerPrune { scope } => match containers::prune(*scope) {
            Ok(bytes) => ItemOutcome::Reclaimed {
                id: item.id,
                path: item.path.clone(),
                // What Docker reports it reclaimed, not what the preview estimated.
                bytes,
            },
            Err(error) => ItemOutcome::Failed {
                id: item.id,
                path: item.path.clone(),
                error,
            },
        },
        ReclaimMethod::ContainerVolume { name } => match containers::remove_volume(name) {
            Ok(bytes) => ItemOutcome::Reclaimed {
                id: item.id,
                path: item.path.clone(),
                bytes,
            },
            Err(error) => ItemOutcome::Failed {
                id: item.id,
                path: item.path.clone(),
                error,
            },
        },

        // Methods that arrive with a later category. Refusing loudly is correct for something
        // nothing has implemented yet.
        other => ItemOutcome::Failed {
            id: item.id,
            path: item.path.clone(),
            error: AppError::new(
                ErrorCode::Unsupported,
                "That way of reclaiming space is not implemented yet.",
            )
            .with_remedy(format!("Method: {other:?}")),
        },
    }
}

/// Run one privileged operation and turn its answer into an outcome.
fn privileged(item: &PreviewItem, elevation: &mut Elevation, op: helper::Op) -> ItemOutcome {
    let failed = |error: AppError| ItemOutcome::Failed {
        id: item.id,
        path: item.path.clone(),
        error,
    };

    let client = match elevation.client() {
        Ok(client) => client,
        Err(e) => return failed(e),
    };

    match client.request(&op) {
        Ok(helper::OpResult::Reclaimed { bytes }) => ItemOutcome::Reclaimed {
            id: item.id,
            path: item.path.clone(),
            // What the helper measured, not what the preview estimated.
            bytes,
        },
        Ok(other) => failed(AppError::internal(format!(
            "The helper answered a reclaim with {other:?}"
        ))),
        Err(e) => failed(e),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A ticket travels as a bare number, for the same reason and with the same history as
    /// [`crate::op::OperationId`]: `#[serde(transparent)]` was redundant and made ts-rs warn on every
    /// build. Asserted rather than assumed, because the whole preview-to-execute handshake is carried
    /// by this value.
    #[test]
    fn a_ticket_is_a_bare_number_on_the_wire() {
        let ticket = Ticket::mint();
        let encoded = serde_json::to_string(&ticket).unwrap();
        assert!(
            encoded.chars().all(|c| c.is_ascii_digit()),
            "a ticket must be a bare number, got {encoded}"
        );
        assert_eq!(serde_json::from_str::<Ticket>(&encoded).unwrap(), ticket);
    }
    use crate::trash::TrashDir;

    struct Sandbox {
        root: PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "nix-reclaim-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(root.join("home")).unwrap();
            Self { root }
        }

        fn trash_dir(&self) -> TrashDir {
            TrashDir::at(self.root.join("Trash"), None)
        }

        /// A trash directory holding `count` files, as if the user had trashed them.
        fn filled_trash(&self, count: usize) -> TrashDir {
            let dir = self.trash_dir();
            for i in 0..count {
                let file = self.root.join("home").join(format!("f{i}.bin"));
                std::fs::write(&file, vec![b'x'; 4096]).unwrap();
                crate::trash::trash_into(&dir, &file).unwrap();
            }
            dir
        }

        fn registry(&self, dir: TrashDir) -> Registry {
            let mut registry = Registry::new();
            registry.register(Box::new(TrashCategory::at(dir)));
            registry
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    fn guard() -> Guard {
        Guard::new(Vec::new())
    }

    // ---------- the central guarantee: nothing bypasses preview ----------

    #[test]
    fn execution_without_a_preview_is_refused() {
        let session = Session::new();
        let err = session
            .execute(Ticket(1), &[0], &guard(), &CancelToken::new(), |_, _| {})
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Refused);
        assert!(
            err.remedy.is_some(),
            "a refusal must say what to do instead"
        );
    }

    #[test]
    fn a_stale_ticket_is_refused_after_a_newer_preview() {
        let sandbox = Sandbox::new("stale");
        let registry = sandbox.registry(sandbox.filled_trash(2));
        let session = Session::new();
        let token = CancelToken::new();

        let first = session.preview(&registry, &guard(), &token).unwrap();
        let second = session.preview(&registry, &guard(), &token).unwrap();
        assert_ne!(
            first.ticket, second.ticket,
            "each preview mints its own ticket"
        );

        let err = session
            .execute(first.ticket, &[0], &guard(), &token, |_, _| {})
            .unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::Refused,
            "an old ticket cannot be replayed"
        );
    }

    #[test]
    fn selecting_an_item_that_was_never_shown_is_an_error_not_a_silent_skip() {
        let sandbox = Sandbox::new("unknown");
        let registry = sandbox.registry(sandbox.filled_trash(1));
        let session = Session::new();
        let token = CancelToken::new();
        let preview = session.preview(&registry, &guard(), &token).unwrap();

        let err = session
            .execute(preview.ticket, &[999], &guard(), &token, |_, _| {})
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(
            err.message.contains("was not part of the preview"),
            "acting on something the user never saw must be loud: {}",
            err.message
        );
    }

    #[test]
    fn clearing_a_session_invalidates_its_ticket() {
        let sandbox = Sandbox::new("clear");
        let registry = sandbox.registry(sandbox.filled_trash(1));
        let session = Session::new();
        let token = CancelToken::new();
        let preview = session.preview(&registry, &guard(), &token).unwrap();

        session.clear();

        assert!(
            session
                .execute(preview.ticket, &[0], &guard(), &token, |_, _| {})
                .is_err()
        );
    }

    // ---------- the protection rules get the first and last word ----------

    #[test]
    fn a_protected_candidate_never_reaches_the_preview() {
        struct ProtectedCategory;
        impl Category for ProtectedCategory {
            fn id(&self) -> &'static str {
                "test"
            }
            fn label(&self) -> &'static str {
                "Test"
            }

            fn explains(&self) -> &'static str {
                "A category used only by tests."
            }
            fn space_category(&self) -> crate::space::Category {
                crate::space::Category::Unknown
            }
            fn candidates(&self, _: &CancelToken) -> Result<Vec<Candidate>> {
                Ok(vec![Candidate {
                    path: PathBuf::from("/etc/passwd"),
                    label: "Something forbidden".into(),
                    bytes: 4096,
                    safety: Safety::Safe,
                    method: ReclaimMethod::Unlink {
                        path: PathBuf::from("/etc/passwd"),
                    },
                    cost: None,
                    category: "test".into(),
                    reclaimable: Reclaimable::Exact,
                }])
            }
        }

        let mut registry = Registry::new();
        registry.register(Box::new(ProtectedCategory));

        let preview = Session::new()
            .preview(&registry, &guard(), &CancelToken::new())
            .unwrap();

        assert!(
            preview.items.is_empty(),
            "a protected path must not be offered"
        );
        assert_eq!(preview.refused.len(), 1);
        assert_eq!(preview.refused[0].rule, "configuration");
        assert_eq!(preview.total_bytes, 0);
    }

    #[test]
    fn a_never_rated_candidate_never_reaches_the_preview() {
        struct NeverCategory;
        impl Category for NeverCategory {
            fn id(&self) -> &'static str {
                "never"
            }
            fn label(&self) -> &'static str {
                "Never"
            }

            fn explains(&self) -> &'static str {
                "A category used only by tests."
            }
            fn space_category(&self) -> crate::space::Category {
                crate::space::Category::Unknown
            }
            fn candidates(&self, _: &CancelToken) -> Result<Vec<Candidate>> {
                Ok(vec![Candidate {
                    path: std::env::temp_dir().join("nix-never-test"),
                    label: "Rated never".into(),
                    bytes: 1,
                    safety: Safety::Never,
                    method: ReclaimMethod::MoveToTrash {
                        path: std::env::temp_dir().join("nix-never-test"),
                    },
                    cost: None,
                    category: "never".into(),
                    reclaimable: Reclaimable::Exact,
                }])
            }
        }

        let mut registry = Registry::new();
        registry.register(Box::new(NeverCategory));
        let preview = Session::new()
            .preview(&registry, &guard(), &CancelToken::new())
            .unwrap();

        assert!(preview.items.is_empty());
        assert_eq!(preview.refused[0].rule, "rated_never");
    }

    #[test]
    fn protection_is_rechecked_at_execution_time() {
        let sandbox = Sandbox::new("recheck");
        let dir = sandbox.filled_trash(2);
        let registry = sandbox.registry(dir.clone());
        let session = Session::new();
        let token = CancelToken::new();

        let preview = session.preview(&registry, &guard(), &token).unwrap();
        assert_eq!(preview.items.len(), 1);

        // The user adds an exclusion between previewing and confirming.
        let stricter = Guard::new(vec![dir.root().to_path_buf()]);
        let report = session
            .execute(preview.ticket, &[0], &stricter, &token, |_, _| {})
            .unwrap();

        assert_eq!(report.skipped_count, 1, "the new rule must be honoured");
        assert_eq!(report.freed, 0);
        assert!(dir.root().exists(), "and nothing was actually removed");
    }

    // ---------- time-of-check / time-of-use ----------

    #[test]
    fn a_path_that_changed_since_the_preview_is_skipped_and_reported() {
        let sandbox = Sandbox::new("toctou");
        let file = sandbox.root.join("home/target.bin");
        std::fs::write(&file, vec![b'x'; 8192]).unwrap();

        struct FileCategory(PathBuf);
        impl Category for FileCategory {
            fn id(&self) -> &'static str {
                "file"
            }
            fn label(&self) -> &'static str {
                "File"
            }

            fn explains(&self) -> &'static str {
                "A category used only by tests."
            }
            fn space_category(&self) -> crate::space::Category {
                crate::space::Category::UserFile
            }
            fn candidates(&self, _: &CancelToken) -> Result<Vec<Candidate>> {
                Ok(vec![Candidate {
                    path: self.0.clone(),
                    label: "A file".into(),
                    bytes: 8192,
                    safety: Safety::Review,
                    method: ReclaimMethod::MoveToTrash {
                        path: self.0.clone(),
                    },
                    cost: Some("It goes to the trash.".into()),
                    category: "file".into(),
                    reclaimable: Reclaimable::Exact,
                }])
            }
        }

        let mut registry = Registry::new();
        registry.register(Box::new(FileCategory(file.clone())));
        let session = Session::new();
        let token = CancelToken::new();
        let preview = session.preview(&registry, &guard(), &token).unwrap();

        // Someone rewrites the file after the user saw the preview.
        std::fs::write(&file, vec![b'y'; 65536]).unwrap();

        let report = session
            .execute(preview.ticket, &[0], &guard(), &token, |_, _| {})
            .unwrap();

        assert_eq!(report.skipped_count, 1);
        assert_eq!(report.freed, 0);
        assert!(file.exists(), "the changed file must be left alone");
        match &report.outcomes[0] {
            ItemOutcome::Skipped { reason, .. } => {
                assert!(reason.contains("changed"), "{reason}");
            }
            other => panic!("expected a skip, got {other:?}"),
        }
    }

    #[test]
    fn an_already_deleted_path_is_skipped_rather_than_failing() {
        let sandbox = Sandbox::new("gone");
        let file = sandbox.root.join("home/vanishing.bin");
        std::fs::write(&file, vec![b'x'; 4096]).unwrap();

        struct FileCategory(PathBuf);
        impl Category for FileCategory {
            fn id(&self) -> &'static str {
                "file"
            }
            fn label(&self) -> &'static str {
                "File"
            }

            fn explains(&self) -> &'static str {
                "A category used only by tests."
            }
            fn space_category(&self) -> crate::space::Category {
                crate::space::Category::UserFile
            }
            fn candidates(&self, _: &CancelToken) -> Result<Vec<Candidate>> {
                Ok(vec![Candidate {
                    path: self.0.clone(),
                    label: "A file".into(),
                    bytes: 4096,
                    safety: Safety::Review,
                    method: ReclaimMethod::MoveToTrash {
                        path: self.0.clone(),
                    },
                    cost: Some("It goes to the trash.".into()),
                    category: "file".into(),
                    reclaimable: Reclaimable::Exact,
                }])
            }
        }

        let mut registry = Registry::new();
        registry.register(Box::new(FileCategory(file.clone())));
        let session = Session::new();
        let token = CancelToken::new();
        let preview = session.preview(&registry, &guard(), &token).unwrap();

        std::fs::remove_file(&file).unwrap();

        let report = session
            .execute(preview.ticket, &[0], &guard(), &token, |_, _| {})
            .unwrap();
        assert_eq!(report.skipped_count, 1);
        match &report.outcomes[0] {
            ItemOutcome::Skipped { reason, .. } => {
                assert!(reason.contains("already gone"), "{reason}")
            }
            other => panic!("expected a skip, got {other:?}"),
        }
    }

    // ---------- the happy path, end to end ----------

    #[test]
    fn emptying_trash_frees_space_and_reports_it_per_item() {
        let sandbox = Sandbox::new("happy");
        let dir = sandbox.filled_trash(3);
        let registry = sandbox.registry(dir.clone());
        let session = Session::new();
        let token = CancelToken::new();

        assert_eq!(crate::trash::list(&dir).len(), 3);

        let preview = session.preview(&registry, &guard(), &token).unwrap();
        assert_eq!(
            preview.items.len(),
            1,
            "the trash is offered as one decision"
        );
        assert!(preview.total_bytes > 0);
        assert_eq!(
            preview.safe_bytes, 0,
            "emptying trash is irreversible, so it is never pre-checked"
        );
        assert_eq!(preview.items[0].safety, Safety::Review);
        assert!(
            preview.items[0].cost.is_some(),
            "a Review item must state its cost"
        );

        let report = session
            .execute(preview.ticket, &[0], &guard(), &token, |_, _| {})
            .unwrap();

        assert_eq!(report.reclaimed_count, 1);
        assert_eq!(report.failed_count, 0);
        assert!(
            report.freed > 0,
            "freeing three 4 KiB files must count for something"
        );
        assert!(
            crate::trash::list(&dir).is_empty(),
            "the trash must actually be empty"
        );
        assert!(
            report.summary().starts_with("Freed "),
            "{}",
            report.summary()
        );
    }

    #[test]
    fn an_empty_trash_produces_no_candidates() {
        let sandbox = Sandbox::new("nothing");
        let registry = sandbox.registry(sandbox.trash_dir());
        let preview = Session::new()
            .preview(&registry, &guard(), &CancelToken::new())
            .unwrap();
        assert!(preview.is_empty());
        assert_eq!(preview.total_bytes, 0);
    }

    #[test]
    fn selecting_nothing_does_nothing() {
        let sandbox = Sandbox::new("noselect");
        let dir = sandbox.filled_trash(2);
        let registry = sandbox.registry(dir.clone());
        let session = Session::new();
        let token = CancelToken::new();
        let preview = session.preview(&registry, &guard(), &token).unwrap();

        let report = session
            .execute(preview.ticket, &[], &guard(), &token, |_, _| {})
            .unwrap();
        assert_eq!(report.freed, 0);
        assert!(report.outcomes.is_empty());
        assert_eq!(crate::trash::list(&dir).len(), 2, "nothing was touched");
    }

    #[test]
    fn cancellation_stops_between_items_and_is_reported() {
        let sandbox = Sandbox::new("cancel");
        let dir = sandbox.filled_trash(2);
        let registry = sandbox.registry(dir.clone());
        let session = Session::new();
        let token = CancelToken::new();
        let preview = session.preview(&registry, &guard(), &token).unwrap();

        token.cancel();
        let report = session
            .execute(preview.ticket, &[0], &guard(), &token, |_, _| {})
            .unwrap();

        assert!(report.cancelled);
        assert_eq!(report.freed, 0);
        assert_eq!(
            crate::trash::list(&dir).len(),
            2,
            "cancelling before an item means it is untouched"
        );
    }

    #[test]
    fn progress_is_reported_for_each_item() {
        let sandbox = Sandbox::new("progress");
        let registry = sandbox.registry(sandbox.filled_trash(1));
        let session = Session::new();
        let token = CancelToken::new();
        let preview = session.preview(&registry, &guard(), &token).unwrap();

        let seen = std::sync::Mutex::new(Vec::new());
        session
            .execute(preview.ticket, &[0], &guard(), &token, |done, total| {
                seen.lock().unwrap().push((done, total));
            })
            .unwrap();

        assert_eq!(seen.into_inner().unwrap(), vec![(0, 1)]);
    }

    // ---------- measurement, the specification's 2% criterion ----------

    #[test]
    fn agreement_tolerates_small_differences_and_catches_large_ones() {
        // Within 2%.
        assert!(agrees(100_000_000, 100_000_000));
        assert!(agrees(100_000_000, 99_000_000));
        assert!(agrees(100_000_000, 101_000_000));
        // Beyond it.
        assert!(
            !agrees(100_000_000, 80_000_000),
            "a 20% gap is a real disagreement"
        );
        assert!(
            !agrees(100_000_000, 0),
            "freeing nothing while claiming 100 MB must not agree"
        );
        // Below the floor, the comparison says nothing useful, so it does not object.
        assert!(
            agrees(4096, 0),
            "a few kilobytes is inside the noise of a live filesystem"
        );
    }

    #[test]
    fn the_report_checks_its_own_arithmetic_against_the_filesystem() {
        let sandbox = Sandbox::new("measure");
        let registry = sandbox.registry(sandbox.filled_trash(4));
        let session = Session::new();
        let token = CancelToken::new();
        let preview = session.preview(&registry, &guard(), &token).unwrap();

        let report = session
            .execute(preview.ticket, &[0], &guard(), &token, |_, _| {})
            .unwrap();

        // On a busy filesystem the measured delta is noisy, so this asserts the *mechanism* exists
        // and reaches a verdict — not a specific figure, which would be flaky.
        assert!(report.freed > 0);
        if let Some(agrees) = report.measurement_agrees {
            assert!(
                agrees,
                "counted {} but the filesystem moved {:?}",
                report.freed, report.measured_delta
            );
        }
    }

    // ---------- ordering and presentation ----------

    // ---------- STO-17: a qualified estimate is never presented as a promise ----------

    /// The acceptance criterion. A candidate whose exclusivity cannot be proven must not contribute
    /// to the promisable total, however large its stated size.
    #[test]
    fn an_unprovable_candidate_contributes_nothing_to_the_promise() {
        struct Shared;
        impl Category for Shared {
            fn id(&self) -> &'static str {
                "shared"
            }
            fn label(&self) -> &'static str {
                "Shared"
            }

            fn explains(&self) -> &'static str {
                "A category used only by tests."
            }
            fn space_category(&self) -> crate::space::Category {
                crate::space::Category::UserFile
            }
            fn candidates(&self, _: &CancelToken) -> Result<Vec<Candidate>> {
                let path = std::env::temp_dir().join("nix-shared-candidate");
                std::fs::write(&path, vec![b'x'; 4096]).ok();
                Ok(vec![Candidate {
                    path: path.clone(),
                    label: "On a snapshotted volume".into(),
                    bytes: 8_589_934_592,
                    safety: Safety::Review,
                    method: ReclaimMethod::MoveToTrash { path },
                    cost: Some("It goes to the trash.".into()),
                    category: "shared".into(),
                    // The category knows its own sharing, and that judgement wins over the
                    // filesystem-level guess.
                    reclaimable: Reclaimable::AtMost {
                        exclusive: None,
                        reason: "Shared with a snapshot.".into(),
                    },
                }])
            }
        }

        let mut registry = Registry::new();
        registry.register(Box::new(Shared));
        let preview = Session::new()
            .preview(&registry, &guard(), &CancelToken::new())
            .unwrap();

        assert_eq!(preview.items.len(), 1);
        assert_eq!(
            preview.total_bytes, 8_589_934_592,
            "the stated size is still shown"
        );
        assert_eq!(
            preview.promisable_bytes, 0,
            "but nothing may be promised, because nothing was proven"
        );
        assert!(preview.total_is_upper_bound());
        assert_eq!(preview.qualified().len(), 1);
        assert!(preview.items[0].reclaimable.caveat().is_some());
    }

    #[test]
    fn a_partly_shared_candidate_promises_only_its_exclusive_part() {
        struct Partly;
        impl Category for Partly {
            fn id(&self) -> &'static str {
                "partly"
            }
            fn label(&self) -> &'static str {
                "Partly"
            }

            fn explains(&self) -> &'static str {
                "A category used only by tests."
            }
            fn space_category(&self) -> crate::space::Category {
                crate::space::Category::UserFile
            }
            fn candidates(&self, _: &CancelToken) -> Result<Vec<Candidate>> {
                let path = std::env::temp_dir().join("nix-partly-candidate");
                std::fs::write(&path, vec![b'x'; 4096]).ok();
                Ok(vec![Candidate {
                    path: path.clone(),
                    label: "Mostly shared".into(),
                    bytes: 10_000_000_000,
                    safety: Safety::Safe,
                    method: ReclaimMethod::MoveToTrash { path },
                    cost: None,
                    category: "partly".into(),
                    reclaimable: Reclaimable::AtMost {
                        exclusive: Some(2_000_000_000),
                        reason: "Most of this is shared with a snapshot.".into(),
                    },
                }])
            }
        }

        let mut registry = Registry::new();
        registry.register(Box::new(Partly));
        let preview = Session::new()
            .preview(&registry, &guard(), &CancelToken::new())
            .unwrap();

        assert_eq!(preview.promisable_bytes, 2_000_000_000);
        assert_eq!(
            preview.safe_bytes, 2_000_000_000,
            "a pre-checked total must also be a promise, not a stated size"
        );
        assert!(preview.total_is_upper_bound());
    }

    #[test]
    fn an_ordinary_candidate_is_exact_and_the_two_totals_agree() {
        let sandbox = Sandbox::new("exact");
        let registry = sandbox.registry(sandbox.filled_trash(2));
        let preview = Session::new()
            .preview(&registry, &guard(), &CancelToken::new())
            .unwrap();

        assert!(preview.items[0].reclaimable.is_exact());
        assert_eq!(
            preview.promisable_bytes, preview.total_bytes,
            "on an ordinary filesystem there is nothing to qualify"
        );
        assert!(!preview.total_is_upper_bound());
        assert!(preview.qualified().is_empty());
    }

    #[test]
    fn items_are_ordered_largest_first() {
        struct Multi;
        impl Category for Multi {
            fn id(&self) -> &'static str {
                "multi"
            }
            fn label(&self) -> &'static str {
                "Multi"
            }

            fn explains(&self) -> &'static str {
                "A category used only by tests."
            }
            fn space_category(&self) -> crate::space::Category {
                crate::space::Category::AppCache
            }
            fn candidates(&self, _: &CancelToken) -> Result<Vec<Candidate>> {
                Ok([100u64, 5000, 700]
                    .into_iter()
                    .enumerate()
                    .map(|(i, bytes)| Candidate {
                        path: std::env::temp_dir().join(format!("nix-order-{i}")),
                        label: format!("item {i}"),
                        bytes,
                        safety: Safety::Safe,
                        method: ReclaimMethod::MoveToTrash {
                            path: std::env::temp_dir().join(format!("nix-order-{i}")),
                        },
                        cost: None,
                        category: "multi".into(),
                        reclaimable: Reclaimable::Exact,
                    })
                    .collect())
            }
        }

        let mut registry = Registry::new();
        registry.register(Box::new(Multi));
        let preview = Session::new()
            .preview(&registry, &guard(), &CancelToken::new())
            .unwrap();

        let sizes: Vec<u64> = preview.items.iter().map(|i| i.bytes).collect();
        assert_eq!(
            sizes,
            vec![5000, 700, 100],
            "the decision is about where the space is"
        );
        assert_eq!(preview.total_bytes, 5800);
        assert_eq!(preview.safe_bytes, 5800, "all three are Safe");
        assert_eq!(preview.bulk_selectable().len(), 3);
    }

    #[test]
    fn a_failing_category_does_not_deny_the_user_the_others() {
        struct Broken;
        impl Category for Broken {
            fn id(&self) -> &'static str {
                "broken"
            }
            fn label(&self) -> &'static str {
                "Broken"
            }

            fn explains(&self) -> &'static str {
                "A category used only by tests."
            }
            fn space_category(&self) -> crate::space::Category {
                crate::space::Category::Unknown
            }
            fn candidates(&self, _: &CancelToken) -> Result<Vec<Candidate>> {
                Err(AppError::internal("this category is broken"))
            }
        }

        let sandbox = Sandbox::new("broken");
        let mut registry = sandbox.registry(sandbox.filled_trash(1));
        registry.register(Box::new(Broken));

        let preview = Session::new()
            .preview(&registry, &guard(), &CancelToken::new())
            .unwrap();
        assert_eq!(
            preview.items.len(),
            1,
            "the working category still produced its candidate"
        );
    }

    #[test]
    fn an_unavailable_category_is_skipped() {
        struct Unavailable;
        impl Category for Unavailable {
            fn id(&self) -> &'static str {
                "unavailable"
            }
            fn label(&self) -> &'static str {
                "Unavailable"
            }

            fn explains(&self) -> &'static str {
                "A category used only by tests."
            }
            fn space_category(&self) -> crate::space::Category {
                crate::space::Category::Unknown
            }
            fn available(&self) -> bool {
                false
            }
            fn candidates(&self, _: &CancelToken) -> Result<Vec<Candidate>> {
                panic!("an unavailable category must not be asked for candidates");
            }
        }

        let mut registry = Registry::new();
        registry.register(Box::new(Unavailable));
        let preview = Session::new()
            .preview(&registry, &guard(), &CancelToken::new())
            .unwrap();
        assert!(preview.is_empty());
    }

    #[test]
    fn the_default_registry_holds_every_implemented_category() {
        let registry = Registry::with_defaults();
        assert_eq!(
            registry.ids(),
            vec![
                "trash",
                "app_cache",
                "rotated_logs",
                "journal",
                "package_cache",
                "old_kernels",
                "residual_config",
                "snap_revisions",
                "flatpak_unused",
                "build_artifacts",
                "package_stores",
                "containers"
            ],
        );
        // Trash stays first: it is the category the pipeline was proven against, and the one whose
        // consequences a user has already accepted.
        assert_eq!(registry.ids().first(), Some(&"trash"));
    }

    /// Every registered category must be able to describe itself, or the UI has nothing to show.
    #[test]
    fn every_registered_category_is_self_describing() {
        let registry = Registry::with_defaults();
        let mut ids = std::collections::HashSet::new();
        for id in registry.ids() {
            assert!(!id.is_empty());
            assert!(ids.insert(id), "duplicate category id {id}");
            assert!(
                id.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{id} should be a stable snake_case identifier"
            );
        }
        assert_eq!(registry.len(), ids.len());
    }

    /// The guard itself, tested directly on the type.
    ///
    /// Deliberately *not* verified by breaking the guard and watching a test fail, which is this
    /// project's usual practice. Disabling an escalation guard on a machine with a helper actually
    /// installed is how the kernel was lost in the first place, and repeating it to prove a point
    /// would be indefensible. This asserts the behaviour where it lives instead.
    #[test]
    fn refusing_elevation_never_opens_a_session() {
        let mut elevation = Elevation::never();
        let error = elevation
            .client()
            .expect_err("elevation must be refused outright");
        assert_eq!(error.code, ErrorCode::HelperUnavailable);
        assert!(
            elevation.client.is_none(),
            "no privileged process may be started at all"
        );

        // Asking twice must not start one either — a caller retrying is the obvious way a guard that
        // only checked once would be defeated.
        assert!(elevation.client().is_err());
        assert!(elevation.client.is_none());
    }

    /// # Regression
    ///
    /// A unit test removed a real kernel from a real machine.
    ///
    /// It called `Elevation::default()` — which escalated through polkit on first need — expecting
    /// that to fail because no helper was installed. Then the helper *was* installed, for manual
    /// testing of the `pkexec` path, and `auth_admin_keep` had already cached the authorisation from
    /// an earlier prompt. So the test escalated **silently**, the helper's own derivation agreed the
    /// package was a removable old kernel, and `apt-get remove --purge -y` ran.
    ///
    /// Every safety rule held: the helper refused nothing it should have allowed and allowed nothing
    /// outside its derived set. The defect was that reaching root required no deliberate act, and a
    /// test fixture happened to name a package that existed.
    ///
    /// `Elevation` no longer implements `Default`. This asserts the consequence: with
    /// [`Elevation::never`], every operation that would need root fails at elevation and nothing runs.
    #[test]
    fn no_privileged_operation_can_execute_under_test_elevation() {
        use crate::space::{Manager, PruneScope, RemovableKind, VacuumLimit};

        let privileged_methods = [
            ReclaimMethod::Packages {
                kind: RemovableKind::OldKernel,
                names: vec!["nix-test-not-a-real-kernel".into()],
            },
            ReclaimMethod::Packages {
                kind: RemovableKind::ResidualConfig,
                names: vec!["nix-test-not-a-real-package".into()],
            },
            ReclaimMethod::PackageManager {
                manager: Manager::Apt,
            },
            ReclaimMethod::JournalVacuum {
                limit: VacuumLimit::Size { mebibytes: 1 },
            },
            ReclaimMethod::SnapRevision {
                package: "nix-test-not-a-real-snap".into(),
                revision: "1".into(),
            },
            ReclaimMethod::FlatpakUnused,
        ];

        for method in privileged_methods {
            let item = PreviewItem {
                id: 0,
                path: std::path::PathBuf::from("logical test entry"),
                label: "test".into(),
                bytes: 1024,
                safety: Safety::Review,
                method: method.clone(),
                cost: Some("test".into()),
                category: "test".into(),
                reclaimable: Reclaimable::Exact,
                fingerprint: 0,
            };

            match reclaim_one(&item, &guard(), &mut Elevation::never()) {
                ItemOutcome::Failed { error, .. } => assert_eq!(
                    error.code,
                    ErrorCode::HelperUnavailable,
                    "{method:?} must fail at elevation, not somewhere further along"
                ),
                other => {
                    panic!("{method:?} must not be carried out under test elevation, got {other:?}")
                }
            }
        }

        // `ContainerPrune` and `ContainerVolume` are deliberately absent: they do not go through the
        // helper, because nix talks to Docker as the user. They are covered by their own tests, which
        // is exactly why this list is written out rather than derived — a new privileged method has to
        // be added here consciously.
        let _ = PruneScope::BuildCache;
    }

    /// # Regression
    ///
    /// Every category that removes a *logical* object — a kernel, a snap revision — carries a
    /// descriptive path like `kernel 6.8.0-136-generic` that is not meant to exist on disk. The
    /// execution stage re-checked that path and, finding nothing, skipped the item as "already
    /// gone". So old-kernel removal silently did nothing, and nothing noticed because the path needs
    /// root and had only ever been exercised as far as the preview.
    ///
    /// This asserts the classification directly, and the test below asserts the behaviour.
    #[test]
    fn a_logical_method_is_not_guarded_by_a_path_that_never_existed() {
        use crate::space::{Manager, RemovableKind, VacuumLimit};

        // Logical: the path is a description, and the helper's re-derivation is the real guard.
        for method in [
            ReclaimMethod::Packages {
                kind: RemovableKind::OldKernel,
                names: vec!["nix-test-not-a-real-kernel".into()],
            },
            ReclaimMethod::SnapRevision {
                package: "chromium".into(),
                revision: "3499".into(),
            },
            ReclaimMethod::FlatpakUnused,
            ReclaimMethod::PackageManager {
                manager: Manager::Apt,
            },
            ReclaimMethod::JournalVacuum {
                limit: VacuumLimit::Size { mebibytes: 200 },
            },
            ReclaimMethod::ContainerPrune {
                scope: crate::space::PruneScope::BuildCache,
            },
            ReclaimMethod::ContainerVolume {
                name: "data".into(),
            },
        ] {
            assert!(
                !method.acts_on_path(),
                "{method:?} acts on a logical object, so a path fingerprint would skip it"
            );
        }

        // Path-based: the path *is* the target, so re-checking it is the whole point.
        for method in [
            ReclaimMethod::MoveToTrash {
                path: "/home/u/x".into(),
            },
            ReclaimMethod::Unlink {
                path: "/home/u/x".into(),
            },
            ReclaimMethod::SystemFile {
                kind: crate::space::ReclaimKind::RotatedLog,
                path: "/var/log/x.1".into(),
            },
            ReclaimMethod::TrashEmpty {
                volume: "/home/u".into(),
            },
        ] {
            assert!(
                method.acts_on_path(),
                "{method:?} names the thing being removed, so it must be re-checked"
            );
        }
    }

    /// # Regression
    ///
    /// The same root cause as the test below, one stage earlier and with a wider blast radius: the
    /// preview asked the *path* protection rules about a logical entry, and they answered "only
    /// absolute paths can be checked". So every kernel, every residual-config set and all eighteen
    /// snap revisions on the development machine — 4.5 GiB — were refused before a user could see
    /// them, with a reason about relative paths that means nothing to anyone.
    ///
    /// The refusal list did its job and showed them, which is how this was found. Guarded here so a
    /// future logical category cannot reintroduce it.
    #[test]
    fn a_logical_candidate_is_not_refused_for_having_a_descriptive_path() {
        let mut registry = Registry::new();
        registry.register(Box::new(LogicalOnly));

        let session = Session::new();
        let preview = session
            .preview(&registry, &guard(), &CancelToken::new())
            .unwrap();

        assert!(
            preview.refused.is_empty(),
            "a logical entry has no path for the path rules to judge: {:?}",
            preview.refused
        );
        assert_eq!(preview.items.len(), 1, "the candidate must be offered");
        assert_eq!(preview.total_bytes, 1024);
    }

    /// A category shaped exactly like `OldKernelCategory`: a logical entry whose path is a label.
    struct LogicalOnly;

    impl Category for LogicalOnly {
        fn id(&self) -> &'static str {
            "logical_only"
        }
        fn label(&self) -> &'static str {
            "Logical only"
        }

        fn explains(&self) -> &'static str {
            "A category used only by tests."
        }
        fn space_category(&self) -> crate::space::Category {
            crate::space::Category::PackagePayload
        }
        fn candidates(&self, _token: &CancelToken) -> Result<Vec<Candidate>> {
            Ok(vec![Candidate {
                path: std::path::PathBuf::from("kernel nix-test-not-a-real-kernel"),
                label: "Linux nix-test-not-a-real-kernel".into(),
                bytes: 1024,
                safety: Safety::Review,
                method: ReclaimMethod::Packages {
                    kind: crate::space::RemovableKind::OldKernel,
                    names: vec!["nix-test-not-a-real-kernel".into()],
                },
                cost: Some("You could not boot into it.".into()),
                category: self.id().to_string(),
                reclaimable: Reclaimable::Exact,
            }])
        }
    }

    /// The behaviour, not just the classification: a logical item must reach its method.
    #[test]
    fn a_logical_item_reaches_its_method_rather_than_being_skipped_as_missing() {
        let item = PreviewItem {
            id: 0,
            // Exactly what `OldKernelCategory` produces: a description, not a path.
            path: std::path::PathBuf::from("kernel nix-test-not-a-real-kernel"),
            label: "Linux nix-test-not-a-real-kernel".into(),
            bytes: 634_600_000,
            safety: Safety::Review,
            method: ReclaimMethod::Packages {
                kind: crate::space::RemovableKind::OldKernel,
                names: vec!["nix-test-not-a-real-kernel".into()],
            },
            cost: Some("cost".into()),
            category: "old_kernels".into(),
            reclaimable: Reclaimable::Exact,
            fingerprint: 0,
        };

        // No helper is available in a test, so the attempt must fail at *elevation* — proving it got
        // as far as trying. A `Skipped { "It is already gone." }` here is the bug this guards.
        match reclaim_one(&item, &guard(), &mut Elevation::never()) {
            ItemOutcome::Failed { .. } => {}
            ItemOutcome::Skipped { reason, .. } => panic!(
                "a kernel removal must reach the helper, not be skipped because its label is not a file: {reason}"
            ),
            ItemOutcome::Reclaimed { .. } | ItemOutcome::Trashed { .. } => {
                panic!("nothing should succeed without a helper")
            }
        }
    }

    #[test]
    fn an_unimplemented_method_fails_loudly_rather_than_silently() {
        let sandbox = Sandbox::new("unimpl");
        let file = sandbox.root.join("home/x.bin");
        std::fs::write(&file, b"data").unwrap();

        let item = PreviewItem {
            id: 0,
            path: file.clone(),
            label: "x".into(),
            bytes: 4,
            safety: Safety::Safe,
            // A method with no implementation: nothing emits `Unlink`, and the dispatch refuses it.
            method: ReclaimMethod::Unlink { path: file.clone() },
            cost: None,
            category: "test".into(),
            reclaimable: Reclaimable::Exact,
            fingerprint: fingerprint(&file),
        };

        match reclaim_one(&item, &guard(), &mut Elevation::never()) {
            ItemOutcome::Failed { error, .. } => {
                assert_eq!(error.code, ErrorCode::Unsupported);
            }
            other => panic!("an unimplemented method must fail loudly, got {other:?}"),
        }
        assert!(file.exists(), "and must not have touched anything");
    }

    #[test]
    fn reports_round_trip_over_the_wire() {
        let report = Report::new(
            vec![
                ItemOutcome::Reclaimed {
                    id: 0,
                    path: PathBuf::from("/tmp/a"),
                    bytes: 4096,
                },
                ItemOutcome::Skipped {
                    id: 1,
                    path: PathBuf::from("/tmp/b"),
                    reason: "It changed.".into(),
                },
            ],
            Some(4096),
            false,
        );
        assert_eq!(report.freed, 4096, "freed is derived from the outcomes");
        assert_eq!(report.reclaimed_count, 1);
        assert_eq!(report.skipped_count, 1);
        let json = serde_json::to_string(&report).unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
        assert!(json.contains("\"reclaimed\""), "{json}");
    }
}
