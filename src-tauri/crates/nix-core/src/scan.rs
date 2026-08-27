// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The filesystem scanner. Task 1.3 (`STO-2`).
//!
//! Properties the specification requires, and why each matters:
//!
//! - **Streaming.** Entries are reported as they are found, so the treemap fills in progressively
//!   and the first useful paint does not wait for a full walk.
//! - **Cancellable.** Checked at every directory, so a stop is prompt rather than eventual.
//! - **Apparent *and* allocated size.** They differ on sparse, compressed and copy-on-write
//!   filesystems. Carrying one and calling it "the size" is how a tool ends up promising space it
//!   cannot free.
//! - **Hard links counted once.** A file with two links in the same scan is 4 KiB of disk, not
//!   8 KiB. Counting it twice inflates the total and the reclaim estimate with it.
//! - **Errors per entry, never fatal.** A permission-denied subdirectory is normal; it must reduce
//!   coverage, not abort the scan. Stacer discarded stderr entirely, so its user never learned that
//!   a scan had been partial.
//! - **Filesystem boundaries respected.** Following a mount into another filesystem would double
//!   count it against the wrong total, and descending into a network mount would hang.
//!
//! Symbolic links are recorded at their own (tiny) size and never followed. Following them means
//! counting the target twice and risking a cycle, and a storage tool that reports the same bytes
//! twice is worse than useless.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, ErrorCode, Result};
use crate::op::CancelToken;
use crate::space::{EntryId, SpaceEntry, SpaceTree};

/// What to scan and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Where to start.
    pub root: PathBuf,
    /// Descend into other filesystems. Off by default: a mount belongs to its own total, and a
    /// network mount would hang the walk.
    pub cross_filesystems: bool,
    /// Stop descending below this depth. `None` means no limit.
    ///
    /// The tree returned to the UI is capped so that a scan of a million files does not become a
    /// million-node payload; the *totals* are still complete, because a directory beyond the cap is
    /// summarised rather than dropped.
    pub max_depth: Option<usize>,
    /// Paths never to descend into, in addition to any protected set.
    pub exclude: Vec<PathBuf>,
    /// Roughly how many nodes the returned tree may hold. `STO-19`.
    ///
    /// # Why a tree needs a budget at all
    ///
    /// Without one, node count follows *file* count. A real home directory here holds 5,406,062 files
    /// in 782,107 directories, which produced 5,454,451 nodes and peaked at **4.2 GiB** resident —
    /// and would have been gigabytes of JSON to hand to the frontend, which is the harder limit of
    /// the two. `max_depth` was meant to bound this and does not: at 12, almost every file in a home
    /// directory is still inside the cap.
    ///
    /// The budget sets a size threshold instead. Children below it fold into one
    /// [`SpaceEntry::aggregated`] node per directory, whose bytes are exactly the sum of what it
    /// replaced — so totals stay complete and a parent still equals the sum of its children.
    ///
    /// This bounds *directories* as well as files, which matters because the directory count is what
    /// actually dominates: a per-directory rule such as "keep the largest sixteen" would still leave
    /// 782,107 directories to describe.
    pub node_budget: usize,
    /// Roughly how many bytes this tree holds, if the caller already knows. `STO-19`.
    ///
    /// The node threshold is a share of the tree's total, and the total is not knowable before
    /// walking. Absent a hint, it is estimated from the filesystem's used bytes — which is right for a
    /// scan rooted at a mount point and too coarse for a subtree, and when it is much too coarse the
    /// scan walks a second time to correct it.
    ///
    /// A hint removes that second walk. The obvious source is the previous scan of the same root, so
    /// a rescan pays the estimate cost once and never again; `/usr` is 1.1 s with the correction and
    /// 759 ms with a hint. Being wrong is not a correctness problem — the threshold only decides how
    /// much detail the tree carries, never what the totals are.
    pub size_hint: Option<u64>,
}

impl Options {
    /// Scan a path with the defaults.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cross_filesystems: false,
            max_depth: Some(12),
            node_budget: DEFAULT_NODE_BUDGET,
            size_hint: None,
            exclude: Vec::new(),
        }
    }

    #[must_use]
    pub fn cross_filesystems(mut self, yes: bool) -> Self {
        self.cross_filesystems = yes;
        self
    }

    #[must_use]
    pub fn max_depth(mut self, depth: Option<usize>) -> Self {
        self.max_depth = depth;
        self
    }

    #[must_use]
    pub fn exclude(mut self, paths: Vec<PathBuf>) -> Self {
        self.exclude = paths;
        self
    }

    #[must_use]
    pub fn node_budget(mut self, nodes: usize) -> Self {
        self.node_budget = nodes;
        self
    }

    #[must_use]
    pub fn size_hint(mut self, bytes: Option<u64>) -> Self {
        self.size_hint = bytes;
        self
    }

    /// The size at or above which a child earns its own node.
    ///
    /// Derived from the scan's measured total rather than guessed in advance, which is the whole
    /// reason the scan counts before it builds. The floor keeps small scans at full detail: a 50 MB
    /// project directory would otherwise get a threshold of a few hundred bytes, which is no
    /// threshold at all, so nothing below 4 KiB is ever considered significant either way.
    #[must_use]
    pub fn threshold_for(&self, total_allocated: u64) -> u64 {
        if self.node_budget == 0 {
            return u64::MAX;
        }
        (total_allocated / self.node_budget as u64).max(MIN_SIGNIFICANT_BYTES)
    }
}

/// One thing that went wrong, attributed to the path it happened at.
///
/// Collected rather than propagated: a scan with unreadable corners is a *partial* result, which is
/// useful, not a failure, which is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScanError {
    pub path: PathBuf,
    pub message: String,
}

/// What a completed scan produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScanResult {
    /// The tree, capped at [`Options::max_depth`].
    pub tree: SpaceTree,
    /// Files visited, including those only counted towards a parent's total.
    #[ts(type = "number")]
    pub files: u64,
    /// Directories visited.
    #[ts(type = "number")]
    pub dirs: u64,
    /// Total apparent bytes.
    #[ts(type = "number")]
    pub apparent_size: u64,
    /// Total on-disk bytes, with hard links counted once.
    #[ts(type = "number")]
    pub allocated: u64,
    /// Bytes skipped because they could not be read. Reported so coverage is visible rather than
    /// silently missing.
    #[ts(type = "number")]
    pub skipped: u64,
    /// Per-path failures, capped so a systematically unreadable tree cannot exhaust memory.
    pub errors: Vec<ScanError>,
    /// Whether the walk stopped early because it was cancelled.
    pub cancelled: bool,
    /// Whether [`ScanResult::errors`] was truncated.
    pub errors_truncated: bool,
    /// A sentence describing incomplete coverage, or `None` when the scan was complete.
    ///
    /// Carried as a field rather than computed in the frontend so the wording lives in one place,
    /// and so it is impossible to render a total without the caveat being available beside it.
    pub coverage_note: Option<String>,
    /// The size below which children were folded into per-directory aggregate nodes. `STO-19`.
    ///
    /// Zero when nothing was aggregated. Reported so the UI can say what the threshold was rather
    /// than leaving a user to wonder why a directory they know about is not listed — the bytes are
    /// always there, in the "*n* smaller items" entry beside its siblings.
    #[ts(type = "number")]
    pub aggregated_below: u64,
}

/// Cap on retained per-path errors. Beyond this the count still rises but the list stops growing.
const MAX_ERRORS: usize = 500;

/// Default node budget. `STO-19`.
///
/// Chosen against the frontend rather than against memory: 200,000 nodes is a payload the IPC channel
/// and a canvas treemap can both handle, and at 200 bytes a node it is about 40 MiB of model. The
/// memory ceiling in the specification (500 MiB for a five-million-file tree) follows comfortably.
pub const DEFAULT_NODE_BUDGET: usize = 200_000;

/// No child below this is ever given its own node, whatever the budget works out to.
///
/// Without a floor, a small scan computes a threshold of a few hundred bytes and aggregates nothing,
/// which costs a node per file for no benefit. 4 KiB is one filesystem block: below it, a file's
/// contribution to a total is rounding.
pub const MIN_SIGNIFICANT_BYTES: u64 = 4096;

/// How wrong the threshold estimate has to be before a second walk is worth its cost. `STO-19`.
///
/// The estimate comes from the filesystem's used bytes, so it overshoots for any scan that is not
/// rooted at a mount point. Overshooting by a little costs a little detail; overshooting by a lot
/// collapses a tree into a handful of nodes. Eight is the line between the two, and it keeps the
/// common cases — a whole home directory, a whole filesystem — to a single traversal.
const ESTIMATE_TOLERANCE: u64 = 8;

/// Progress counters, shared across worker threads.
#[derive(Debug, Default)]
struct Counters {
    files: AtomicU64,
    dirs: AtomicU64,
    apparent: AtomicU64,
    allocated: AtomicU64,
    skipped: AtomicU64,
    errors_seen: AtomicU64,
}

/// The summary of one directory, rolled up from its contents.
///
/// # Why the nodes travel with the totals
///
/// The tree used to be built through a shared `Mutex<SpaceTree>`, one lock acquisition per entry.
/// Measured on `/usr` — 422,330 files, 45,488 directories, eight cores — that cost about **2 µs per
/// node**, which for 454,129 nodes is roughly **1.0 s of a 1.5 s scan**: two thirds of the work was
/// threads queueing behind each other, not filesystem access. The parallel syscall floor for the
/// same tree is 344 ms.
///
/// So each thread accumulates its own fragment and fragments are merged as the recursion unwinds.
/// This is only possible because [`EntryId::for_path`] is a pure function of the path — there is no
/// central allocator to serialise on, and a node built in isolation has the same id it would have
/// had in a shared tree.
#[derive(Debug, Default)]
struct Rollup {
    apparent: u64,
    allocated: u64,
    files: u64,
    dirs: u64,
    /// Ids of the *direct* children of the directory this rollup describes, in discovery order.
    ///
    /// Only **ids** travel up the recursion, never the nodes themselves. A [`SpaceEntry`] is 200
    /// bytes and an [`EntryId`] is eight, and an earlier version of this merged whole node vectors
    /// upward — which memcpy'd every node once per level of tree above it, so a node twelve deep was
    /// copied twelve times. The nodes go straight into a shared sink instead, one batch per
    /// directory.
    direct: Vec<EntryId>,
    /// Nodes for this directory's *subdirectories*, which only the parent can build because only it
    /// has their rolled-up totals. At most one per subdirectory, so this stays small.
    subdir_nodes: Vec<SpaceEntry>,
    /// Children of *this* directory that fell below the threshold.
    ///
    /// Reported upward rather than turned into a node here, because whether this directory gets an
    /// aggregate node depends on whether the directory *itself* survives — which only the parent
    /// knows. Building it here pushed 633,035 aggregate nodes into the sink on a real home directory,
    /// and every one whose directory was then folded became an orphan: unreachable from the root, yet
    /// still occupying memory and payload.
    folded: Folded,
}

/// What a directory's aggregate node stands for.
#[derive(Debug, Default, Clone, Copy)]
struct Folded {
    count: u64,
    apparent: u64,
    allocated: u64,
}

impl Folded {
    fn add(&mut self, apparent: u64, allocated: u64) {
        self.count += 1;
        self.apparent += apparent;
        self.allocated += allocated;
    }

    fn merge(&mut self, other: Self) {
        self.count += other.count;
        self.apparent += other.apparent;
        self.allocated += other.allocated;
    }
}

impl Rollup {
    /// Merge a sibling subtree.
    fn merge(&mut self, mut other: Self) {
        self.apparent += other.apparent;
        self.allocated += other.allocated;
        self.files += other.files;
        self.dirs += other.dirs;
        self.direct.append(&mut other.direct);
        self.subdir_nodes.append(&mut other.subdir_nodes);
        // This directory's own fold total, accumulated across its subdirectories' verdicts. A
        // subdirectory's *inner* folds never arrive here: they were consumed into its own aggregate
        // node the moment it was kept, or discarded with it when it was not.
        self.folded.merge(other.folded);
    }
}

/// Shared mutable state for one scan.
struct Shared {
    counters: Counters,
    errors: Mutex<Vec<ScanError>>,
    /// Every node the walk has built, appended **one directory at a time**.
    ///
    /// Batching is the point. Inserting each node individually into a shared tree cost about 2 µs
    /// per node under contention — for `/usr`'s 454,129 nodes, roughly 1.0 s of a 1.5 s scan. One
    /// append per directory is 45,488 acquisitions rather than 454,129, and each holds the lock only
    /// long enough to move a `Vec`'s buffer.
    ///
    /// A staging vector rather than the finished map, because inserting into the map *under the
    /// lock* was measured at 2.3 s — worse than the original — since the map's rehashing then
    /// happens with every other thread waiting on it. The map is built once, pre-sized, at the end.
    ///
    /// The cost of staging is that the vector and the map are both alive during the transfer. That
    /// is fine for a system directory and not fine for a large home directory; see the memory note
    /// on [`scan`].
    nodes: Mutex<Vec<SpaceEntry>>,
    /// `(device, inode)` of every hard-linked file already counted, so its blocks are counted once.
    seen_links: Mutex<std::collections::HashSet<(u64, u64)>>,
    token: CancelToken,
    options: Options,
    root_device: u64,
    /// Bytes at or above which a child earns its own node. `STO-19`.
    ///
    /// Fixed for the whole walk, which is what makes the aggregation *local*: a directory can decide
    /// each child on the spot and its aggregate is exact, with nothing to revisit later. A threshold
    /// that moved during the walk would mean pruning nodes whose parents did not exist yet.
    threshold: u64,
    /// Whether to build nodes at all. False for the counting pass, which needs only the total.
    collect_nodes: bool,
    /// Called with cumulative counts as the walk proceeds.
    progress: Box<dyn Fn(u64, u64) + Send + Sync>,
}

impl Shared {
    fn record_error(&self, path: &Path, message: String) {
        self.counters.errors_seen.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut errors) = self.errors.lock() {
            if errors.len() < MAX_ERRORS {
                errors.push(ScanError {
                    path: path.to_path_buf(),
                    message,
                });
            }
        }
    }

    /// Whether a hard-linked file's blocks should count towards the total.
    ///
    /// The first link seen counts; later links contribute their apparent size but no allocation, so
    /// the on-disk figure stays honest.
    fn should_count_blocks(&self, meta: &std::fs::Metadata) -> bool {
        if meta.nlink() <= 1 {
            return true;
        }
        self.seen_links
            .lock()
            .map(|mut seen| seen.insert((meta.dev(), meta.ino())))
            .unwrap_or(true)
    }

    fn excluded(&self, path: &Path) -> bool {
        self.options.exclude.iter().any(|e| path == e)
    }
}

/// Bytes actually occupied on disk. `st_blocks` is always in 512-byte units regardless of the
/// filesystem's own block size — a detail that is easy to get wrong and yields figures off by a
/// factor of eight.
const fn allocated_bytes(meta_blocks: u64) -> u64 {
    meta_blocks * 512
}

/// Walk one directory, returning its rollup.
///
/// Recursion fans out with `par_iter().map().reduce()`, which is built on `rayon::join` and nests
/// correctly. An earlier version created a `rayon::scope` per directory: a scope *blocks* its
/// calling thread until every spawned child finishes, so a tree of nested scopes fills the shared
/// pool with threads waiting on children that need the very threads that are waiting. In isolation
/// it looked fine — 35 ms for 2,590 files — but with several scans running concurrently it
/// collapsed to fifteen seconds, and cancellation could not unwind through the blocked scopes.
fn walk(dir: &Path, depth: usize, shared: &Shared) -> Rollup {
    let mut rollup = Rollup::default();

    if shared.token.is_cancelled() {
        return rollup;
    }

    let reader = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            shared.record_error(dir, e.to_string());
            return rollup;
        }
    };

    // Subdirectories are collected first so they can be walked after this directory's files are
    // accounted for, which keeps the rollup arithmetic simple.
    let mut subdirs: Vec<PathBuf> = Vec::new();
    // Nodes for the files directly in this directory, handed to the shared sink in one batch once
    // the whole directory has been read.
    let mut mine: Vec<SpaceEntry> = Vec::new();

    for entry in reader {
        if shared.token.is_cancelled() {
            return rollup;
        }

        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                shared.record_error(dir, e.to_string());
                continue;
            }
        };
        let path = entry.path();

        if shared.excluded(&path) {
            continue;
        }

        // `symlink_metadata`, never `metadata`: a symlink is recorded at its own size and never
        // followed. Following would count the target twice and could loop.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                shared.record_error(&path, e.to_string());
                continue;
            }
        };

        if meta.is_dir() {
            // A different device id means a mount point. Descending would count another
            // filesystem's bytes against this one's total.
            if !shared.options.cross_filesystems && meta.dev() != shared.root_device {
                continue;
            }
            subdirs.push(path);
        } else {
            let apparent = meta.size();
            let allocated = if shared.should_count_blocks(&meta) {
                allocated_bytes(meta.blocks())
            } else {
                0
            };

            rollup.apparent += apparent;
            rollup.allocated += allocated;
            rollup.files += 1;

            shared.counters.files.fetch_add(1, Ordering::Relaxed);
            shared
                .counters
                .apparent
                .fetch_add(apparent, Ordering::Relaxed);
            let total_allocated = shared
                .counters
                .allocated
                .fetch_add(allocated, Ordering::Relaxed)
                + allocated;
            let total_files = shared.counters.files.load(Ordering::Relaxed);

            // Report every so often rather than per file: an event per file would swamp the IPC
            // channel and make the UI slower than the scan.
            if total_files % 2048 == 0 {
                (shared.progress)(total_files, total_allocated);
            }

            // Only entries within the depth cap become nodes. Beyond it their bytes still roll up
            // into an ancestor, so totals stay complete while the payload stays bounded.
            if shared.collect_nodes && shared.options.max_depth.is_none_or(|max| depth < max) {
                // `STO-19`: significance is judged on **allocated** bytes, not apparent ones,
                // because this is a tool about disk space. A sparse ten-gigabyte image occupying one
                // block is not one of the largest things on the disk, whatever it claims to be.
                if allocated >= shared.threshold {
                    let node = SpaceEntry::walked(path, apparent, allocated, false);
                    rollup.direct.push(node.id);
                    mine.push(node);
                } else {
                    rollup.folded.add(apparent, allocated);
                }
            }
        }
    }

    // Recurse. Each subdirectory gets its own node so the UI can drill in, and its rollup folds
    // back into this directory's totals.
    let within_depth = shared.options.max_depth.is_none_or(|max| depth < max);

    let children = subdirs
        .par_iter()
        .map(|subdir| {
            let child = walk(subdir, depth + 1, shared);

            let mut acc = Rollup {
                apparent: child.apparent,
                allocated: child.allocated,
                files: child.files,
                dirs: child.dirs + 1,
                direct: Vec::new(),
                subdir_nodes: Vec::new(),
                // The subdirectory's own folds are already accounted for in its aggregate node; what
                // travels up is only whether *this* subdirectory folded into its parent.
                folded: Folded::default(),
            };

            // The directory's own node carries its contents' rolled-up totals, so a directory's
            // size is the size of what is inside it. Built *after* the walk, because only then are
            // those totals known — the previous version inserted a zeroed node first and patched it
            // afterwards, which needed two more lock acquisitions per directory.
            if within_depth && shared.collect_nodes {
                if child.allocated >= shared.threshold {
                    let mut node =
                        SpaceEntry::walked(subdir.clone(), child.apparent, child.allocated, true);
                    node.children = child.direct;
                    // The subdirectory survives, so its own folded children get their stand-in here
                    // — created by the parent, at the moment the parent commits to keeping it.
                    if child.folded.count > 0 {
                        let aggregate = SpaceEntry::aggregated(
                            subdir,
                            child.folded.count,
                            child.folded.apparent,
                            child.folded.allocated,
                        );
                        node.children.push(aggregate.id);
                        acc.subdir_nodes.push(aggregate);
                    }
                    acc.direct.push(node.id);
                    acc.subdir_nodes.push(node);
                } else {
                    // The whole subtree folds, not just this directory — and since a child can never
                    // hold more than its parent, nothing inside it reached the threshold either, so
                    // there are no orphaned nodes to clean up. That property is what makes an
                    // absolute threshold bound the *directory* count, which is the count that
                    // actually dominates.
                    acc.folded.add(child.apparent, child.allocated);
                }
            }

            shared.counters.dirs.fetch_add(1, Ordering::Relaxed);
            acc
        })
        .reduce(Rollup::default, |mut a, b| {
            a.merge(b);
            a
        });

    rollup.merge(children);

    // One acquisition for this whole directory: its files, and the nodes for its subdirectories.
    if !mine.is_empty() || !rollup.subdir_nodes.is_empty() {
        if let Ok(mut sink) = shared.nodes.lock() {
            sink.append(&mut mine);
            sink.append(&mut rollup.subdir_nodes);
        }
    }

    rollup
}

/// Scan a directory tree.
///
/// `progress` is called with `(files_seen, bytes_allocated)` as the walk proceeds. It is invoked
/// from worker threads, so it must be cheap and must not block.
pub fn scan(
    options: Options,
    token: CancelToken,
    progress: impl Fn(u64, u64) + Send + Sync + 'static,
) -> Result<ScanResult> {
    let root = options.root.clone();

    let meta = std::fs::metadata(&root)
        .map_err(|e| AppError::from_io(&e, format!("scan {}", root.display())).with_path(&root))?;
    if !meta.is_dir() {
        return Err(
            AppError::invalid_input(format!("{} is not a directory.", root.display()))
                .with_path(&root),
        );
    }

    // `STO-19`: the threshold that decides which children earn their own node is a share of the
    // tree's total, and the total is not knowable before walking.
    //
    // Counting first, then building, was the obvious answer and was measured as the wrong one: on a
    // real home directory it took the scan from 34.7 s to 61.2 s, because that tree is syscall-bound
    // and a second traversal simply doubles the dominant cost. `/usr` hides this by being page-cache
    // hot.
    //
    // So the threshold is *estimated* from the filesystem's used bytes, which `statvfs` gives for
    // free, and the scan runs once. The estimate is good exactly when it matters: a scan rooted at a
    // home directory covers most of what the filesystem holds. It is bad for a small subdirectory —
    // where the threshold comes out far too coarse — and that is the case where a second pass costs
    // almost nothing, so [`ScanResult`] is rebuilt with the true figure below.
    let basis = options.size_hint.unwrap_or_else(|| {
        crate::fs::containing(&root)
            .ok()
            .flatten()
            .map(|f| f.used)
            .unwrap_or(0)
    });

    let mut threshold = options.threshold_for(basis);
    let mut attempt = 0;

    let progress = std::sync::Arc::new(progress);

    let (total, shared) = loop {
        attempt += 1;
        let shared = Shared {
            counters: Counters::default(),
            errors: Mutex::new(Vec::new()),
            nodes: Mutex::new(Vec::new()),
            seen_links: Mutex::new(std::collections::HashSet::new()),
            token: token.clone(),
            root_device: meta.dev(),
            options: options.clone(),
            // Reported from the first attempt only. Its counts are the true ones — a retry walks the
            // same tree, so nothing is lost — and this keeps progress monotonic, which callers rely
            // on. A retry therefore shows as a short pause after progress completes, and only ever on
            // a tree small enough for the estimate to have been wrong about it.
            progress: if attempt == 1 {
                let p = progress.clone();
                Box::new(move |files, bytes| p(files, bytes)) as Box<dyn Fn(u64, u64) + Send + Sync>
            } else {
                Box::new(|_, _| {})
            },
            threshold,
            collect_nodes: true,
        };
        let total = walk(&root, 0, &shared);

        // The estimate was only ever an estimate. If the tree turned out to be a small fraction of
        // the filesystem, the threshold was too coarse and detail was aggregated away that should not
        // have been — so it is recomputed from what was actually found and the walk is repeated.
        //
        // Bounded to one retry, and only downward: a smaller threshold keeps more, never less. Small
        // trees are the only ones that reach here, and they are cheap to walk twice.
        // The estimate is almost always somewhat high, because a scanned tree is almost always
        // smaller than the filesystem holding it. Retrying on *any* overshoot therefore retried
        // essentially every scan and doubled its cost — `/usr` went from 759 ms to 2.4 s and a home
        // directory from 34.7 s to 63.4 s. Only a materially wrong estimate is worth a second walk.
        //
        // A home directory comes out around 1.4x — not worth it. `/usr`, at 3% of the filesystem,
        // comes out 31x too coarse, which visibly costs detail and is.
        let corrected = options.threshold_for(total.allocated);
        if attempt == 1
            // A caller-supplied hint is taken at its word: it is a better basis than anything this
            // function can derive, and second-guessing it would spend the walk the hint exists to
            // save.
            && options.size_hint.is_none()
            && corrected.saturating_mul(ESTIMATE_TOLERANCE) <= threshold
            && !token.is_cancelled()
        {
            threshold = corrected;
            continue;
        }
        break (total, shared);
    };

    // The tree is assembled here, from nodes the walk accumulated thread-locally.
    let nodes = shared
        .nodes
        .into_inner()
        .map_err(|_| AppError::internal("The scan's node sink lock was poisoned."))?;

    // Pre-sized so the map never rehashes on the way to its final size: 454,129 nodes for `/usr`,
    // and a growing map would rebuild its table nineteen times getting there.
    let mut tree = SpaceTree::new();
    tree.entries.reserve(nodes.len() + 1);
    for node in nodes {
        tree.entries.insert(node.id, node);
    }

    let mut root_node = SpaceEntry::walked(root.clone(), total.apparent, total.allocated, true);
    root_node.children = total.direct;

    // The root's own folded children. Every other directory gets its aggregate built by its parent,
    // at the moment the parent commits to keeping it — and the root has no parent, so it is done
    // here. Missing this left the root claiming bytes its children did not account for, which is
    // exactly the discrepancy the structural test looks for.
    if total.folded.count > 0 {
        let aggregate = SpaceEntry::aggregated(
            &root,
            total.folded.count,
            total.folded.apparent,
            total.folded.allocated,
        );
        root_node.children.push(aggregate.id);
        tree.entries.insert(aggregate.id, aggregate);
    }
    tree.insert_root(root_node);

    let errors = shared
        .errors
        .into_inner()
        .map_err(|_| AppError::internal("The scan's error list lock was poisoned."))?;
    let errors_seen = shared.counters.errors_seen.load(Ordering::Relaxed);

    let cancelled = token.is_cancelled();
    Ok(ScanResult {
        tree,
        files: shared.counters.files.load(Ordering::Relaxed),
        dirs: shared.counters.dirs.load(Ordering::Relaxed),
        apparent_size: total.apparent,
        allocated: total.allocated,
        skipped: shared.counters.skipped.load(Ordering::Relaxed),
        errors_truncated: errors_seen as usize > errors.len(),
        aggregated_below: if threshold == u64::MAX { 0 } else { threshold },
        coverage_note: ScanResult::describe_coverage(cancelled, errors.len()),
        errors,
        cancelled,
    })
}

/// Scan with no progress reporting. Convenience for tests and for the growth-history job.
pub fn scan_quiet(options: Options, token: CancelToken) -> Result<ScanResult> {
    scan(options, token, |_, _| {})
}

impl ScanResult {
    /// Whether the result is complete: not cancelled, and nothing unreadable.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.cancelled && self.errors.is_empty()
    }

    /// Build the coverage sentence from a scan's outcome.
    ///
    /// Being explicit about partial coverage is the difference between a number a user can trust and
    /// one they cannot. Stacer showed a total with no indication that a scan had skipped anything.
    #[must_use]
    fn describe_coverage(cancelled: bool, error_count: usize) -> Option<String> {
        match (cancelled, error_count) {
            (false, 0) => None,
            (true, 0) => Some("Stopped early, so this is a partial total.".to_string()),
            (false, n) => Some(format!(
                "{n} location{} could not be read, so the total may be higher.",
                if n == 1 { "" } else { "s" }
            )),
            (true, n) => Some(format!(
                "Stopped early and {n} location{} could not be read, so this is a partial total.",
                if n == 1 { "" } else { "s" }
            )),
        }
    }
}

/// A cancellation that arrives before any work is a valid outcome, not an error.
#[must_use]
pub fn was_cancelled(result: &Result<ScanResult>) -> bool {
    match result {
        Ok(r) => r.cancelled,
        Err(e) => e.code == ErrorCode::Cancelled,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::fixture::{Fixture, Spec};

    fn scan_fixture(fx: &Fixture) -> ScanResult {
        scan_quiet(Options::new(fx.root()).max_depth(None), CancelToken::new()).unwrap()
    }

    // ---- `STO-19`: bounded trees ----

    /// Walk a fixture with a threshold high enough that most children fold.
    fn scan_bounded(fx: &Fixture, budget: usize) -> ScanResult {
        scan_quiet(
            Options::new(fx.root())
                .max_depth(None)
                .node_budget(budget)
                // Hinted so the threshold is exactly derivable in the test rather than depending on
                // how full the machine's disk happens to be.
                .size_hint(Some(1 << 30)),
            CancelToken::new(),
        )
        .unwrap()
    }

    /// The floor exists so a small scan is not aggregated into uselessness.
    #[test]
    fn the_threshold_never_falls_below_one_block() {
        let o = Options::new("/tmp");
        assert_eq!(o.threshold_for(0), MIN_SIGNIFICANT_BYTES);
        assert_eq!(o.threshold_for(1000), MIN_SIGNIFICANT_BYTES);
        // 200,000 nodes into 1 GiB is ~5 KiB, just above the floor.
        assert!(o.threshold_for(1 << 30) > MIN_SIGNIFICANT_BYTES);
        // A budget of zero means "no nodes but the root", not "divide by zero".
        assert_eq!(
            Options::new("/tmp").node_budget(0).threshold_for(1 << 30),
            u64::MAX
        );
    }

    /// The property the whole design rests on: an aggregate is a summary, never a rounding.
    #[test]
    fn aggregates_carry_exactly_the_bytes_they_replaced() {
        let fixture = Fixture::create(&Spec::default()).unwrap();
        let result = scan_bounded(&fixture, 8);

        assert!(
            result.aggregated_below > 0,
            "the threshold should have bitten"
        );

        // Every directory still equals the sum of its children, aggregates included. If an aggregate
        // were approximate, or double-counted, this is where it would show.
        for entry in result.tree.entries.values().filter(|e| e.is_dir) {
            let children: u64 = entry
                .children
                .iter()
                .filter_map(|c| result.tree.get(*c))
                .map(|c| c.allocated)
                .sum();
            assert_eq!(
                entry.allocated, children,
                "{} claims {} but its children hold {children}",
                entry.label, entry.allocated
            );
        }

        // And the root still reports the true total, which is the number a user acts on.
        let root = result.tree.get(result.tree.roots[0]).unwrap();
        assert_eq!(root.allocated, result.allocated);
    }

    /// # Regression
    ///
    /// The aggregate node was first built inside the directory it summarised, and pushed to the node
    /// sink there. But whether that directory survives is its *parent's* decision, made later — so
    /// every aggregate belonging to a folded directory became an orphan: unreachable from the root,
    /// yet still in the tree, holding bytes already counted in its ancestor's aggregate.
    ///
    /// On a real home directory that was 633,035 orphans out of 674,065 nodes, and it did not trip
    /// `check_invariants`, which only walks entries it can reach. Aggregates are now created by the
    /// parent at the moment it commits to keeping the child.
    #[test]
    fn a_folded_directory_leaves_no_orphaned_aggregate() {
        use std::collections::HashSet;

        let fixture = Fixture::create(&Spec::default()).unwrap();
        // A tiny budget forces a high threshold, so most directories fold — which is exactly the
        // condition that produced the orphans.
        let result = scan_bounded(&fixture, 4);

        let mut reachable: HashSet<EntryId> = HashSet::new();
        let mut stack = vec![result.tree.roots[0]];
        while let Some(id) = stack.pop() {
            if !reachable.insert(id) {
                panic!("{id} is reachable by more than one path");
            }
            if let Some(entry) = result.tree.get(id) {
                stack.extend(entry.children.iter().copied());
            }
        }

        assert_eq!(
            reachable.len(),
            result.tree.entries.len(),
            "{} nodes are unreachable from the root",
            result.tree.entries.len() - reachable.len()
        );
    }

    #[test]
    fn an_aggregate_names_its_count_and_claims_no_path() {
        let fixture = Fixture::create(&Spec::default()).unwrap();
        let result = scan_bounded(&fixture, 8);

        let aggregates: Vec<&SpaceEntry> = result
            .tree
            .entries
            .values()
            .filter(|e| matches!(e.provenance, crate::space::Provenance::Aggregated { .. }))
            .collect();
        assert!(!aggregates.is_empty(), "something should have been folded");

        for a in aggregates {
            let crate::space::Provenance::Aggregated { count } = a.provenance else {
                unreachable!()
            };
            assert!(
                count > 0,
                "an aggregate standing for nothing should not exist"
            );
            assert!(
                a.path.is_none(),
                "an aggregate is a set of places, not a place: {:?}",
                a.path
            );
            assert!(!a.is_dir, "it cannot be drilled into");
            assert!(a.children.is_empty());
            assert!(
                a.label.contains(&count.to_string()),
                "the label should say how many: {}",
                a.label
            );
        }
    }

    /// The point of the budget: node count stops following file count.
    #[test]
    fn a_smaller_budget_yields_a_smaller_tree() {
        let fixture = Fixture::create(&Spec::default()).unwrap();
        let generous = scan_bounded(&fixture, 100_000);
        let tight = scan_bounded(&fixture, 4);

        assert!(
            tight.tree.entries.len() < generous.tree.entries.len(),
            "tight={} generous={}",
            tight.tree.entries.len(),
            generous.tree.entries.len()
        );
        // Totals are unaffected by how much detail was kept. This is the line that matters: the
        // budget trades away *listing*, never accounting.
        assert_eq!(tight.allocated, generous.allocated);
        assert_eq!(tight.apparent_size, generous.apparent_size);
        assert_eq!(tight.files, generous.files);
        assert_eq!(tight.dirs, generous.dirs);
    }

    /// A hint is taken at its word, which is what saves the correcting walk.
    #[test]
    fn a_size_hint_settles_the_threshold_without_a_second_walk() {
        let fixture = Fixture::create(&Spec::default()).unwrap();
        let hinted = scan_quiet(
            Options::new(fixture.root())
                .max_depth(None)
                .node_budget(1000)
                .size_hint(Some(4_000_000_000)),
            CancelToken::new(),
        )
        .unwrap();

        // 4 GB over 1000 nodes is 4 MB — far coarser than this fixture warrants, and honoured anyway
        // because the caller said so. Left to its own devices the scan would have corrected it.
        assert_eq!(hinted.aggregated_below, 4_000_000);
    }

    #[test]
    fn a_scan_with_no_budget_keeps_every_node() {
        let fixture = Fixture::create(&Spec::default()).unwrap();
        let full = scan_quiet(
            Options::new(fixture.root())
                .max_depth(None)
                .size_hint(Some(0)),
            CancelToken::new(),
        )
        .unwrap();
        // A zero basis gives the floor, so only sub-block files fold. The generated fixture's files
        // are all under 4 KiB, so this checks the floor is applied rather than ignored.
        assert_eq!(full.aggregated_below, MIN_SIGNIFICANT_BYTES);
    }

    /// # Regression
    ///
    /// The tree used to be built by inserting each node into a shared `Mutex<SpaceTree>` as the walk
    /// went, with parent ids threaded down the recursion. It is now assembled from nodes each
    /// directory accumulates locally, with only ids travelling upward — a change made for speed
    /// (1.5 s to 759 ms on `/usr`) that touched every structural relationship in the tree at once.
    ///
    /// Counts alone would not have caught a mistake there: a tree can have exactly the right number
    /// of nodes and attach half of them to the wrong parent. So this checks the relationships.
    #[test]
    fn the_assembled_tree_is_fully_connected_and_rolls_up_correctly() {
        use std::collections::HashSet;

        let fixture = Fixture::create(&Spec::default()).unwrap();
        let result = scan_quiet(
            Options::new(fixture.root()).max_depth(None),
            CancelToken::new(),
        )
        .unwrap();

        assert_eq!(result.tree.roots.len(), 1, "one scan, one root");
        let root_id = result.tree.roots[0];

        // 1. Every node is reachable from the root exactly once. An orphan would be invisible in the
        //    UI while still counting towards totals; a node reached twice would be double-counted.
        let mut seen: HashSet<EntryId> = HashSet::new();
        let mut stack = vec![root_id];
        while let Some(id) = stack.pop() {
            assert!(seen.insert(id), "{id} is reachable by more than one path");
            let entry = result
                .tree
                .get(id)
                .unwrap_or_else(|| panic!("{id} is referenced as a child but has no node"));
            stack.extend(entry.children.iter().copied());
        }
        assert_eq!(
            seen.len(),
            result.tree.entries.len(),
            "{} nodes are not reachable from the root",
            result.tree.entries.len() - seen.len()
        );

        // 2. Every directory's children are exactly what is on disk at that path, so nothing was
        //    attached to the wrong parent.
        for entry in result.tree.entries.values().filter(|e| e.is_dir) {
            let Some(path) = entry.path.as_ref() else {
                continue;
            };
            let Ok(actual) = std::fs::read_dir(path) else {
                continue;
            };
            let on_disk: HashSet<EntryId> = actual
                .flatten()
                .map(|e| EntryId::for_path(&e.path()))
                .collect();
            let in_tree: HashSet<EntryId> = entry.children.iter().copied().collect();
            assert_eq!(
                in_tree,
                on_disk,
                "{}'s children do not match its contents",
                path.display()
            );
        }

        // 3. A directory's size is the size of what is inside it. `check_invariants` only asserts
        //    children *fit*; this asserts they add up.
        for entry in result.tree.entries.values().filter(|e| e.is_dir) {
            let children: u64 = entry
                .children
                .iter()
                .filter_map(|c| result.tree.get(*c))
                .map(|c| c.allocated)
                .sum();
            assert_eq!(
                entry.allocated, children,
                "{} claims {} but its children hold {children}",
                entry.label, entry.allocated
            );
        }

        assert!(
            result.tree.check_invariants().is_empty(),
            "{:?}",
            result.tree.check_invariants()
        );
    }

    #[test]
    fn counts_match_the_generated_fixture() {
        let spec = Spec {
            breadth: 3,
            depth: 2,
            files_per_dir: 4,
            ..Spec::default()
        };
        let fx = Fixture::create(&spec).unwrap();
        let result = scan_fixture(&fx);

        assert_eq!(
            result.files,
            fx.files(),
            "every generated file must be seen"
        );
        assert_eq!(
            result.dirs,
            fx.dirs(),
            "every generated directory must be seen"
        );
        assert_eq!(
            result.apparent_size,
            fx.bytes(),
            "apparent size must equal the bytes written"
        );
        assert!(result.is_complete(), "{:?}", result.errors);
        assert!(
            result.coverage_note.is_none(),
            "a complete scan has nothing to caveat"
        );
    }

    #[test]
    fn allocated_differs_from_apparent_and_is_block_aligned() {
        let fx = Fixture::create(&Spec {
            breadth: 2,
            depth: 1,
            files_per_dir: 6,
            min_file_bytes: 1,
            max_file_bytes: 100,
            ..Spec::default()
        })
        .unwrap();
        let result = scan_fixture(&fx);

        // Many small files: each occupies at least one block, so on-disk exceeds apparent. This is
        // the whole reason both figures are carried rather than one being called "the size".
        assert!(
            result.allocated >= result.apparent_size,
            "allocated {} should be at least apparent {}",
            result.allocated,
            result.apparent_size
        );
        assert_eq!(
            result.allocated % 512,
            0,
            "allocation is a whole number of 512-byte blocks"
        );
    }

    #[test]
    fn directory_totals_are_the_sum_of_their_contents() {
        let fx = Fixture::create(&Spec {
            breadth: 2,
            depth: 2,
            files_per_dir: 3,
            ..Spec::default()
        })
        .unwrap();
        let result = scan_fixture(&fx);

        // Invariant 1 from the space model, on a real tree.
        let violations = result.tree.check_invariants();
        assert!(violations.is_empty(), "{violations:?}");

        let root_id = result.tree.roots[0];
        let root = result.tree.get(root_id).unwrap();
        assert_eq!(
            root.apparent_size, result.apparent_size,
            "the root node carries the whole total"
        );
    }

    #[test]
    fn hard_links_are_counted_once() {
        let dir = std::env::temp_dir().join(format!("nix-scan-links-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("original");
        std::fs::write(&original, vec![0u8; 8192]).unwrap();
        std::fs::hard_link(&original, dir.join("link")).unwrap();

        let result = scan_quiet(Options::new(&dir).max_depth(None), CancelToken::new()).unwrap();

        assert_eq!(result.files, 2, "both links are files");
        assert_eq!(
            result.apparent_size, 16384,
            "apparent size counts each name, which is what du without -l reports per path"
        );
        // The point: on-disk blocks are counted once, so a reclaim estimate is not doubled.
        assert!(
            result.allocated < 16384,
            "allocated {} must not double-count the shared blocks",
            result.allocated
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn symlinks_are_recorded_but_never_followed() {
        let dir = std::env::temp_dir().join(format!("nix-scan-symlink-{}", std::process::id()));
        let target = dir.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("big"), vec![0u8; 4096]).unwrap();

        // A symlink pointing back up would loop if followed.
        std::os::unix::fs::symlink(&dir, dir.join("loop")).unwrap();
        std::os::unix::fs::symlink(target.join("big"), dir.join("alias")).unwrap();

        let result = scan_quiet(Options::new(&dir).max_depth(None), CancelToken::new()).unwrap();

        // Terminates at all, and counts the 4 KiB file exactly once.
        assert!(
            result.apparent_size < 8192,
            "the target must not be counted twice"
        );
        assert!(result.is_complete(), "{:?}", result.errors);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A fixture big enough to be interesting, small enough that a dozen of these running
    /// concurrently under `cargo test` do not saturate the disk. `Spec::perf()` — 93,620 files — is
    /// reserved for the release-mode budget measurement; using it here made every timing assertion
    /// in this module measure its neighbours' fixture creation instead of the scan.
    fn modest() -> Spec {
        Spec {
            breadth: 4,
            depth: 3,
            files_per_dir: 8,
            ..Spec::default()
        }
    }

    #[test]
    fn cancellation_stops_the_walk_and_is_reported() {
        let fx = Fixture::create(&modest()).unwrap();
        let token = CancelToken::new();
        token.cancel(); // cancelled before it starts: the strongest form of the check

        let result = scan_quiet(Options::new(fx.root()).max_depth(None), token).unwrap();
        assert!(result.cancelled, "the result must say it was cancelled");
        assert!(
            result.files < fx.files(),
            "a cancelled scan must not have visited everything"
        );
        assert!(
            result.coverage_note.is_some(),
            "a partial total must be caveated"
        );
    }

    /// Measures the latency that actually matters: from the user pressing stop to the work ceasing.
    ///
    /// Cancellation is triggered from inside the progress callback rather than after a sleep. Two
    /// earlier attempts got this wrong, and both failures were instructive:
    ///
    /// - Timing the whole run reported 37 seconds. The scan was not at fault; the figure was
    ///   dominated by sibling tests building 93,620-file fixtures on the same disk.
    /// - Sleeping 20 ms and then cancelling raced the scan. Isolated, the walk finished in 10 ms,
    ///   so the cancel arrived after completion and the assertion failed for the opposite reason.
    ///
    /// Cancelling from the callback is deterministic: progress only fires while work is in flight,
    /// so the cancel is guaranteed to land mid-walk, and the window measured contains only the
    /// unwind.
    #[test]
    fn cancellation_mid_flight_is_prompt() {
        // More files than the progress interval, so the callback is guaranteed to fire.
        let fx = Fixture::create(&Spec {
            breadth: 8,
            depth: 3,
            files_per_dir: 12,
            ..Spec::default()
        })
        .unwrap();

        let token = CancelToken::new();
        let cancel = token.clone();
        let cancelled_at: std::sync::Arc<Mutex<Option<std::time::Instant>>> =
            std::sync::Arc::new(Mutex::new(None));
        let stamp = cancelled_at.clone();

        let result = scan(
            Options::new(fx.root()).max_depth(None),
            token,
            move |_files, _bytes| {
                if let Ok(mut at) = stamp.lock() {
                    if at.is_none() {
                        *at = Some(std::time::Instant::now());
                        cancel.cancel();
                    }
                }
            },
        )
        .unwrap();

        let at = cancelled_at
            .lock()
            .ok()
            .and_then(|a| *a)
            .expect("progress must fire at least once on a fixture this size");
        let latency = at.elapsed();

        assert!(result.cancelled, "the result must say it was cancelled");
        assert!(
            result.files < fx.files(),
            "a cancelled scan visited {} of {} files, so it did not stop",
            result.files,
            fx.files()
        );
        assert!(
            result.coverage_note.is_some(),
            "a partial total must be caveated"
        );
        // Generous: a debug build, and other tests share the disk. The point is that the walk
        // unwinds rather than running to completion.
        assert!(
            latency < std::time::Duration::from_secs(3),
            "took {latency:?} to stop after cancellation"
        );
    }

    #[test]
    fn unreadable_directories_reduce_coverage_rather_than_failing() {
        let dir = std::env::temp_dir().join(format!("nix-scan-perm-{}", std::process::id()));
        let locked = dir.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("secret"), b"x").unwrap();
        std::fs::write(dir.join("readable"), vec![0u8; 1024]).unwrap();

        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let result = scan_quiet(Options::new(&dir).max_depth(None), CancelToken::new()).unwrap();

        // Running as root would make the directory readable anyway, so only assert the useful part.
        if !result.errors.is_empty() {
            assert!(!result.is_complete());
            let note = result
                .coverage_note
                .clone()
                .expect("partial coverage must be stated");
            assert!(note.contains("could not be read"), "{note}");
        }
        assert!(result.files >= 1, "the readable file must still be counted");

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn depth_cap_bounds_the_tree_but_not_the_totals() {
        let spec = Spec {
            breadth: 2,
            depth: 4,
            files_per_dir: 2,
            ..Spec::default()
        };
        let fx = Fixture::create(&spec).unwrap();

        let deep = scan_quiet(Options::new(fx.root()).max_depth(None), CancelToken::new()).unwrap();
        let shallow = scan_quiet(
            Options::new(fx.root()).max_depth(Some(1)),
            CancelToken::new(),
        )
        .unwrap();

        assert_eq!(
            shallow.apparent_size, deep.apparent_size,
            "capping the tree must not change the total — bytes below the cap roll up"
        );
        assert_eq!(shallow.files, deep.files, "every file is still visited");
        assert!(
            shallow.tree.len() < deep.tree.len(),
            "the payload must actually be smaller"
        );
        assert!(shallow.tree.check_invariants().is_empty());
    }

    #[test]
    fn excluded_paths_are_not_descended() {
        let spec = Spec {
            breadth: 2,
            depth: 1,
            files_per_dir: 2,
            ..Spec::default()
        };
        let fx = Fixture::create(&spec).unwrap();
        let excluded = fx.root().join("dir-000");

        let all = scan_quiet(Options::new(fx.root()).max_depth(None), CancelToken::new()).unwrap();
        let partial = scan_quiet(
            Options::new(fx.root())
                .max_depth(None)
                .exclude(vec![excluded]),
            CancelToken::new(),
        )
        .unwrap();

        assert!(
            partial.files < all.files,
            "excluding a directory must skip its files"
        );
        assert!(partial.apparent_size < all.apparent_size);
    }

    #[test]
    fn progress_is_reported_and_monotonic() {
        // Needs more files than the reporting interval, or there is nothing to observe.
        let fx = Fixture::create(&Spec {
            breadth: 8,
            depth: 3,
            files_per_dir: 12,
            ..Spec::default()
        })
        .unwrap();
        let seen = std::sync::Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
        let sink = seen.clone();

        let result = scan(
            Options::new(fx.root()).max_depth(None),
            CancelToken::new(),
            move |files, bytes| {
                if let Ok(mut s) = sink.lock() {
                    s.push((files, bytes));
                }
            },
        )
        .unwrap();

        let reports = seen.lock().unwrap();
        assert!(
            !reports.is_empty(),
            "a scan of {} files must report progress",
            result.files
        );
        // Counters come from atomics read by several threads, so file counts rise monotonically.
        let files: Vec<u64> = reports.iter().map(|(f, _)| *f).collect();
        let mut sorted = files.clone();
        sorted.sort_unstable();
        assert_eq!(files, sorted, "reported file counts must not go backwards");
    }

    #[test]
    fn scanning_a_file_rather_than_a_directory_is_rejected_clearly() {
        let path = std::env::temp_dir().join(format!("nix-scan-file-{}", std::process::id()));
        std::fs::write(&path, b"x").unwrap();

        let err = scan_quiet(Options::new(&path), CancelToken::new()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(err.message.contains("not a directory"), "{}", err.message);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_missing_root_reports_not_found_with_the_path() {
        let err = scan_quiet(
            Options::new("/definitely/not/here/at/all"),
            CancelToken::new(),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert!(err.path.is_some(), "the failure must name the path");
    }

    #[test]
    fn filesystem_boundaries_are_not_crossed_by_default() {
        // /proc is its own filesystem, so scanning / without crossing must not descend into it.
        let result = scan_quiet(Options::new("/").max_depth(Some(1)), CancelToken::new()).unwrap();

        let descended_into_proc = result
            .tree
            .entries
            .values()
            .any(|e| e.path.as_deref().is_some_and(|p| p.starts_with("/proc/")));
        assert!(!descended_into_proc, "the walk crossed into /proc");
    }

    #[test]
    fn errors_are_capped_so_a_hostile_tree_cannot_exhaust_memory() {
        let result = ScanResult {
            tree: SpaceTree::new(),
            files: 0,
            dirs: 0,
            apparent_size: 0,
            allocated: 0,
            skipped: 0,
            errors: vec![
                ScanError {
                    path: PathBuf::from("/x"),
                    message: "denied".into(),
                };
                MAX_ERRORS
            ],
            cancelled: false,
            errors_truncated: true,
            aggregated_below: 0,
            coverage_note: ScanResult::describe_coverage(false, MAX_ERRORS),
        };
        assert_eq!(result.errors.len(), MAX_ERRORS);
        assert!(result.errors_truncated);
        assert!(!result.is_complete());
    }

    #[test]
    fn allocation_uses_512_byte_units_regardless_of_filesystem_block_size() {
        // st_blocks is always 512-byte units. Treating it as the filesystem's block size yields
        // figures off by a factor of eight, which is an easy and invisible mistake.
        assert_eq!(allocated_bytes(1), 512);
        assert_eq!(allocated_bytes(8), 4096);
        assert_eq!(allocated_bytes(0), 0);
    }

    #[test]
    fn results_round_trip_over_the_wire() {
        let fx = Fixture::create(&Spec {
            breadth: 1,
            depth: 1,
            files_per_dir: 2,
            ..Spec::default()
        })
        .unwrap();
        let result = scan_fixture(&fx);
        let json = serde_json::to_string(&result).unwrap();
        let back: ScanResult = serde_json::from_str(&json).unwrap();
        assert_eq!(result, back);
    }
}
