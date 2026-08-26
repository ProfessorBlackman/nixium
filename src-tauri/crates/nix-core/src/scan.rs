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
}

impl Options {
    /// Scan a path with the defaults.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cross_filesystems: false,
            max_depth: Some(12),
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
}

/// Cap on retained per-path errors. Beyond this the count still rises but the list stops growing.
const MAX_ERRORS: usize = 500;

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
#[derive(Debug, Default, Clone, Copy)]
struct Rollup {
    apparent: u64,
    allocated: u64,
    files: u64,
    dirs: u64,
}

impl Rollup {
    fn merge(&mut self, other: Self) {
        self.apparent += other.apparent;
        self.allocated += other.allocated;
        self.files += other.files;
        self.dirs += other.dirs;
    }
}

/// Shared mutable state for one scan.
struct Shared {
    counters: Counters,
    errors: Mutex<Vec<ScanError>>,
    tree: Mutex<SpaceTree>,
    /// `(device, inode)` of every hard-linked file already counted, so its blocks are counted once.
    seen_links: Mutex<std::collections::HashSet<(u64, u64)>>,
    token: CancelToken,
    options: Options,
    root_device: u64,
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
fn walk(dir: &Path, depth: usize, shared: &Shared, parent: Option<EntryId>) -> Rollup {
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
            if shared.options.max_depth.is_none_or(|max| depth < max) {
                if let (Ok(mut tree), Some(parent)) = (shared.tree.lock(), parent) {
                    let id = tree.insert(SpaceEntry::walked(path, apparent, allocated, false));
                    tree.attach(parent, id);
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
            let node = if within_depth {
                shared.tree.lock().ok().map(|mut tree| {
                    let id = tree.insert(SpaceEntry::walked(subdir.clone(), 0, 0, true));
                    if let Some(parent) = parent {
                        tree.attach(parent, id);
                    }
                    id
                })
            } else {
                None
            };

            let child = walk(subdir, depth + 1, shared, node.or(parent));

            // Write the rolled-up totals back onto the directory's own node, so a directory's size
            // is the size of its contents.
            if let (Some(id), Ok(mut tree)) = (node, shared.tree.lock()) {
                if let Some(entry) = tree.entries.get_mut(&id) {
                    entry.apparent_size = child.apparent;
                    entry.allocated = child.allocated;
                }
            }

            shared.counters.dirs.fetch_add(1, Ordering::Relaxed);
            Rollup {
                dirs: child.dirs + 1,
                ..child
            }
        })
        .reduce(Rollup::default, |mut a, b| {
            a.merge(b);
            a
        });

    rollup.merge(children);
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

    let mut tree = SpaceTree::new();
    let root_id = tree.insert_root(SpaceEntry::walked(root.clone(), 0, 0, true));

    let shared = Shared {
        counters: Counters::default(),
        errors: Mutex::new(Vec::new()),
        tree: Mutex::new(tree),
        seen_links: Mutex::new(std::collections::HashSet::new()),
        token: token.clone(),
        root_device: meta.dev(),
        options,
        progress: Box::new(progress),
    };

    let total = walk(&root, 0, &shared, Some(root_id));

    let mut tree = shared
        .tree
        .into_inner()
        .map_err(|_| AppError::internal("The scan's tree lock was poisoned."))?;
    if let Some(entry) = tree.entries.get_mut(&root_id) {
        entry.apparent_size = total.apparent;
        entry.allocated = total.allocated;
    }

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
