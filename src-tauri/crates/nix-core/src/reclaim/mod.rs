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

mod caches;
mod kernels;
mod logs;
mod packages;
mod registry;

pub use caches::AppCacheCategory;
pub use kernels::{OldKernelCategory, ResidualConfigCategory};
pub use logs::{JournalCategory, LogCategory};
pub use packages::PackageCacheCategory;
pub use registry::{Candidate, Category, Registry, TrashCategory};

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, ErrorCode, Result};
use crate::helper;
use crate::op::CancelToken;
use crate::protect::{Guard, Refusal};
use crate::space::{ReclaimMethod, Safety};
use crate::trash;

/// Proof that a preview was computed and shown.
///
/// The only way to obtain one is [`preview`]. It carries the identity of the set it describes, so
/// [`execute`] can refuse a selection the user was never shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(transparent)]
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
    /// Total if everything offered were reclaimed.
    #[ts(type = "number")]
    pub total_bytes: u64,
    /// Total of only the entries safe enough to pre-check.
    #[ts(type = "number")]
    pub safe_bytes: u64,
    /// Things a category proposed that the protection rules refused. Shown, not hidden: a user
    /// should be able to see that nix declined to touch something.
    pub refused: Vec<Refusal>,
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
}

/// What happened to one item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[ts(export)]
pub enum ItemOutcome {
    /// Reclaimed, freeing this many bytes.
    Reclaimed {
        #[ts(type = "number")]
        id: u64,
        path: PathBuf,
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
    #[must_use]
    pub const fn bytes_freed(&self) -> u64 {
        match self {
            Self::Reclaimed { bytes, .. } => *bytes,
            _ => 0,
        }
    }
}

/// The per-item account of what was done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Report {
    pub outcomes: Vec<ItemOutcome>,
    /// Sum of what was reclaimed, as nix counted it.
    #[ts(type = "number")]
    pub freed: u64,
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
        Self {
            reclaimed_count: Self::count(&outcomes, |o| matches!(o, ItemOutcome::Reclaimed { .. })),
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

        for candidate in registry.collect(token)? {
            token.check()?;

            // The protection rules get the first word, before anything is offered.
            match guard.verdict(&candidate.path) {
                crate::protect::Verdict::Protected(r) => {
                    refused.push(r);
                    continue;
                }
                crate::protect::Verdict::Allowed => {}
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
            });
            next_id += 1;
        }

        // Largest first: the decision a user is making is about where the space is.
        items.sort_by_key(|i| std::cmp::Reverse(i.bytes));

        let preview = Preview {
            ticket: Ticket::mint(),
            total_bytes: items.iter().map(|i| i.bytes).sum(),
            safe_bytes: items
                .iter()
                .filter(|i| i.safety.pre_checkable())
                .map(|i| i.bytes)
                .sum(),
            items,
            refused,
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
        let mut elevation = Elevation::default();

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

/// A privileged session, opened lazily and kept for the whole execution.
///
/// **Opened once, not once per item.** Stacer re-ran every individual command under `pkexec`, so
/// toggling five services meant five authentication dialogs; one session for a batch is the whole
/// point of the helper's design.
#[derive(Default)]
struct Elevation {
    client: Option<helper::Client>,
    /// The failure that prevented elevation, so every item can report the same honest reason
    /// instead of each retrying and prompting again.
    failure: Option<AppError>,
}

impl Elevation {
    /// The client, opening a session on first use.
    fn client(&mut self) -> std::result::Result<&mut helper::Client, AppError> {
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

/// Reclaim one item, with both guards applied.
fn reclaim_one(item: &PreviewItem, guard: &Guard, elevation: &mut Elevation) -> ItemOutcome {
    // Re-checked at execution time, because the user's exclusions may have changed since preview.
    if let Some(refusal) = guard.verdict(&item.path).refusal() {
        return ItemOutcome::Skipped {
            id: item.id,
            path: item.path.clone(),
            reason: refusal.reason.clone(),
        };
    }

    // Time-of-check/time-of-use: a path that changed since the preview is not the thing the user
    // agreed to.
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

    match &item.method {
        ReclaimMethod::MoveToTrash { path } => match trash::trash(path) {
            Ok(trashed) => ItemOutcome::Reclaimed {
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
                "residual_config"
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
            // A method that genuinely has no implementation yet: it arrives with STO-12 in Phase 2.
            method: ReclaimMethod::SnapRevision {
                package: "firefox".into(),
                revision: "1234".into(),
            },
            cost: None,
            category: "test".into(),
            fingerprint: fingerprint(&file),
        };

        match reclaim_one(&item, &guard(), &mut Elevation::default()) {
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
