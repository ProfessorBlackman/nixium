//! Staleness watching. Task 1.15, completing decision D6.
//!
//! # What this is for
//!
//! D6's cached-first design opens the explorer on the previous scan. This module answers the
//! follow-up question — *is that result still true?* — by watching directories for change and
//! marking the affected subtrees stale, so a rescan only needs to walk what moved (task `STO-18`).
//!
//! # Why only the largest directories
//!
//! inotify watches are a per-user kernel resource, capped by
//! `/proc/sys/fs/inotify/max_user_watches` — commonly 8,192 on stock kernels and 65,536 on some
//! distributions, and **shared with every other program on the desktop**. Watching a whole home
//! directory recursively would consume tens of thousands of watches and could starve the user's file
//! manager, editor and sync client.
//!
//! So this watches the **top N directories by size** and no more. That is not a compromise on
//! correctness: the point of staleness detection is to notice the changes that move the totals, and
//! a directory too small to appear in the top N is by definition too small to matter to them.
//!
//! fanotify would allow whole-mount watching without per-directory cost, but it needs
//! `CAP_SYS_ADMIN` — a privileged capability for a cosmetic freshness hint is not a trade worth
//! making.

use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rustix::fs::inotify;

use crate::error::{AppError, Cause, ErrorCode, Result};
use crate::space::{SpaceEntry, SpaceTree};

/// How many directories to watch. Deliberately modest: these are shared kernel resources.
pub const DEFAULT_WATCH_LIMIT: usize = 64;

/// Where the kernel publishes the per-user watch ceiling.
const MAX_USER_WATCHES: &str = "/proc/sys/fs/inotify/max_user_watches";

/// The per-user watch ceiling, if it can be read.
#[must_use]
pub fn max_user_watches() -> Option<usize> {
    std::fs::read_to_string(MAX_USER_WATCHES)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Pick the directories worth watching: the largest, by allocated bytes.
///
/// Returns at most `limit` paths, largest first.
#[must_use]
pub fn directories_worth_watching(tree: &SpaceTree, limit: usize) -> Vec<PathBuf> {
    let mut dirs: Vec<&SpaceEntry> = tree
        .entries
        .values()
        .filter(|e| e.is_dir && e.allocated > 0)
        .collect();
    dirs.sort_by_key(|e| std::cmp::Reverse(e.allocated));
    dirs.into_iter()
        .filter_map(|e| e.path.clone())
        .take(limit)
        .collect()
}

/// What changed since the watch began.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Staleness {
    /// Watched directories that saw a change, largest-first order preserved.
    pub stale: Vec<PathBuf>,
    /// Whether the kernel dropped events because we did not drain them fast enough. When true the
    /// whole result must be treated as stale, because we cannot know what we missed.
    pub overflowed: bool,
}

impl Staleness {
    /// Whether anything at all is known to have changed.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        self.overflowed || !self.stale.is_empty()
    }
}

/// A live set of inotify watches over selected directories.
///
/// Dropping it closes the inotify file descriptor, which releases every watch — so watches cannot
/// outlive the view that asked for them.
pub struct Watcher {
    inotify: std::sync::Arc<rustix::fd::OwnedFd>,
    /// Watch descriptor to the path it was created for.
    paths: Arc<Mutex<HashMap<i32, PathBuf>>>,
    /// Paths observed to have changed.
    changed: Arc<Mutex<Vec<PathBuf>>>,
    overflowed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    /// Begin watching the given directories.
    ///
    /// Directories that cannot be watched — removed between the scan and now, or unreadable — are
    /// skipped rather than failing the whole watch: a partial freshness hint is still useful.
    pub fn start(dirs: &[PathBuf]) -> Result<Self> {
        let fd = inotify::init(inotify::CreateFlags::NONBLOCK).map_err(|e| {
            AppError::new(
                ErrorCode::Unsupported,
                "Could not watch directories for changes.",
            )
            .with_remedy("nix will still work; results simply will not know when they go stale.")
            .with_cause(Cause::Os {
                errno: Some(e.raw_os_error()),
                description: e.to_string(),
            })
        })?;

        let inotify = std::sync::Arc::new(fd);
        let mut paths = HashMap::new();

        // Only the events that can change a directory's size. Access-time changes are noise.
        let mask = inotify::WatchFlags::CREATE
            | inotify::WatchFlags::DELETE
            | inotify::WatchFlags::MODIFY
            | inotify::WatchFlags::MOVED_FROM
            | inotify::WatchFlags::MOVED_TO
            | inotify::WatchFlags::DELETE_SELF
            | inotify::WatchFlags::MOVE_SELF;

        for dir in dirs {
            match inotify::add_watch(&*inotify, dir, mask) {
                Ok(wd) => {
                    paths.insert(wd, dir.clone());
                }
                Err(e) => {
                    // ENOSPC here means the user's watch limit is exhausted — worth a log, not a
                    // failure, and precisely why this module watches only the largest directories.
                    tracing::debug!(dir = %dir.display(), error = %e, "could not watch directory");
                }
            }
        }

        let paths = Arc::new(Mutex::new(paths));
        let changed = Arc::new(Mutex::new(Vec::new()));
        let overflowed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));

        let thread = {
            let inotify = inotify.clone();
            let paths = paths.clone();
            let changed = changed.clone();
            let overflowed = overflowed.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("nix-watch".into())
                .spawn(move || {
                    let mut buffer = [MaybeUninit::<u8>::uninit(); 4096];
                    while !stop.load(Ordering::Relaxed) {
                        let mut reader = inotify::Reader::new(&*inotify, &mut buffer);
                        let mut drained_any = false;
                        // Non-blocking, so `next` erroring means the queue is simply empty.
                        while let Ok(event) = reader.next() {
                            drained_any = true;
                            if event.events().contains(inotify::ReadFlags::QUEUE_OVERFLOW) {
                                // We cannot know what was missed, so everything is suspect.
                                overflowed.store(true, Ordering::Relaxed);
                                continue;
                            }
                            let wd = event.wd();
                            if let (Ok(paths), Ok(mut changed)) = (paths.lock(), changed.lock()) {
                                if let Some(path) = paths.get(&wd) {
                                    if !changed.contains(path) {
                                        changed.push(path.clone());
                                    }
                                }
                            }
                        }
                        if !drained_any {
                            // Idle. Polling at this interval costs nothing measurable and avoids
                            // holding a blocking read that Drop would have to interrupt.
                            std::thread::sleep(std::time::Duration::from_millis(250));
                        }
                    }
                })
                .map_err(|e| AppError::from_io(&e, "start the directory-watching thread"))?
        };

        Ok(Self {
            inotify,
            paths,
            changed,
            overflowed,
            stop,
            thread: Some(thread),
        })
    }

    /// Begin watching the largest directories in a scan's tree.
    pub fn for_tree(tree: &SpaceTree, limit: usize) -> Result<Self> {
        Self::start(&directories_worth_watching(tree, limit))
    }

    /// How many directories are actually being watched.
    #[must_use]
    pub fn watch_count(&self) -> usize {
        self.paths.lock().map(|p| p.len()).unwrap_or(0)
    }

    /// What has changed so far. Non-destructive, so it can be polled.
    #[must_use]
    pub fn staleness(&self) -> Staleness {
        Staleness {
            stale: self.changed.lock().map(|c| c.clone()).unwrap_or_default(),
            overflowed: self.overflowed.load(Ordering::Relaxed),
        }
    }

    /// Forget what has changed, e.g. after a rescan.
    pub fn reset(&self) {
        if let Ok(mut changed) = self.changed.lock() {
            changed.clear();
        }
        self.overflowed.store(false, Ordering::Relaxed);
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // The reader thread wakes at most 250 ms after the flag is set, so this is bounded.
            if thread.join().is_err() {
                tracing::warn!("the directory-watching thread panicked");
            }
        }
        // Dropping the file descriptor releases every watch it held.
        let _ = &self.inotify;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::space::SpaceEntry;

    fn dir_entry(path: &str, allocated: u64) -> SpaceEntry {
        SpaceEntry::walked(PathBuf::from(path), allocated, allocated, true)
    }

    fn wait_for(mut condition: impl FnMut() -> bool) -> bool {
        // Generous, because inotify delivery and the reader's poll interval are both asynchronous.
        for _ in 0..40 {
            if condition() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn picks_the_largest_directories_and_respects_the_limit() {
        let mut tree = SpaceTree::new();
        tree.insert_root(dir_entry("/a", 100));
        tree.insert(dir_entry("/b", 900));
        tree.insert(dir_entry("/c", 500));
        // Files are not watched, only directories.
        tree.insert(SpaceEntry::walked(
            PathBuf::from("/big-file"),
            10_000,
            10_000,
            false,
        ));
        // A zero-byte directory cannot move a total, so it is not worth a watch.
        tree.insert(dir_entry("/empty", 0));

        let picked = directories_worth_watching(&tree, 2);
        assert_eq!(picked, vec![PathBuf::from("/b"), PathBuf::from("/c")]);

        let all = directories_worth_watching(&tree, 99);
        assert_eq!(all.len(), 3, "only non-empty directories: {all:?}");
        assert!(!all.contains(&PathBuf::from("/big-file")));
        assert!(!all.contains(&PathBuf::from("/empty")));
    }

    #[test]
    fn the_kernel_ceiling_is_readable_and_our_limit_is_well_under_it() {
        // Not asserted as present — a container may not expose it — but if it is, our default must
        // be a small fraction of a resource shared with every other program on the desktop.
        if let Some(ceiling) = max_user_watches() {
            assert!(ceiling > 0);
            assert!(
                DEFAULT_WATCH_LIMIT * 20 < ceiling,
                "watching {DEFAULT_WATCH_LIMIT} against a ceiling of {ceiling} is not modest enough"
            );
        }
    }

    #[test]
    fn notices_a_file_being_created() {
        let dir = std::env::temp_dir().join(format!("nix-watch-create-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let watcher = Watcher::start(std::slice::from_ref(&dir)).unwrap();
        assert_eq!(watcher.watch_count(), 1);
        assert!(!watcher.staleness().is_stale(), "nothing has happened yet");

        std::fs::write(dir.join("new-file"), b"hello").unwrap();

        assert!(
            wait_for(|| watcher.staleness().is_stale()),
            "a new file must mark the directory stale"
        );
        assert!(watcher.staleness().stale.contains(&dir));

        drop(watcher);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn notices_a_file_being_deleted_and_reset_clears_it() {
        let dir = std::env::temp_dir().join(format!("nix-watch-delete-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("doomed");
        std::fs::write(&victim, b"x").unwrap();

        let watcher = Watcher::start(std::slice::from_ref(&dir)).unwrap();
        std::fs::remove_file(&victim).unwrap();

        assert!(
            wait_for(|| watcher.staleness().is_stale()),
            "a deletion must be noticed"
        );

        watcher.reset();
        assert!(
            !watcher.staleness().is_stale(),
            "reset must clear what was seen"
        );

        drop(watcher);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unwatchable_directory_is_skipped_not_fatal() {
        let real = std::env::temp_dir().join(format!("nix-watch-mixed-{}", std::process::id()));
        std::fs::create_dir_all(&real).unwrap();

        let watcher =
            Watcher::start(&[PathBuf::from("/definitely/does/not/exist"), real.clone()]).unwrap();

        assert_eq!(
            watcher.watch_count(),
            1,
            "the missing directory is skipped, the real one is still watched"
        );

        drop(watcher);
        std::fs::remove_dir_all(&real).ok();
    }

    #[test]
    fn watching_nothing_is_valid() {
        let watcher = Watcher::start(&[]).unwrap();
        assert_eq!(watcher.watch_count(), 0);
        assert!(!watcher.staleness().is_stale());
    }

    #[test]
    fn dropping_the_watcher_stops_its_thread_promptly() {
        let dir = std::env::temp_dir().join(format!("nix-watch-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let start = std::time::Instant::now();
        {
            let _watcher = Watcher::start(std::slice::from_ref(&dir)).unwrap();
        }
        let elapsed = start.elapsed();

        // Bounded by the reader's poll interval, so a watcher can never outlive its view for long.
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "drop took {elapsed:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn staleness_reports_overflow_as_total() {
        let overflowed = Staleness {
            stale: Vec::new(),
            overflowed: true,
        };
        assert!(
            overflowed.is_stale(),
            "if the kernel dropped events we cannot know what changed, so everything is suspect"
        );
    }
}
