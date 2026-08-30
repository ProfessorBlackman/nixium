// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Largest files, and duplicate detection. `STO-15`.
//!
//! # Largest files
//!
//! A projection over the scan that already happened, not a new search. Stacer made you fill in a
//! `find` dialogue — a path, a pattern, a size, a unit — and then showed you a list with no sizes on
//! it. The information was already on disk both times; only the asking was different.
//!
//! # Duplicates, and what "never a false positive" costs
//!
//! The specification's criterion is that duplicate detection **never** reports a false positive. That
//! rules out finishing on a hash, however strong: a hash says "almost certainly identical", and
//! "almost certainly" is not "never".
//!
//! So detection is staged, cheapest first, and ends with a byte-for-byte comparison:
//!
//! 1. **Size.** Files of different sizes cannot be duplicates. This eliminates almost everything for
//!    the cost of data the scan already collected.
//! 2. **Head.** The first 4 KiB. Different beginnings settle it without reading either file fully,
//!    and files that differ usually differ early.
//! 3. **Whole content.** A full hash over both files.
//! 4. **Byte-for-byte.** Only for files that survived stage 3 — so this reads a pair that is already
//!    near-certainly identical, and it is what turns "near-certainly" into a fact.
//!
//! The hash does not need to be cryptographic, because it is a *filter* and never the verdict. It is
//! FNV-1a over 64 bits, the same function [`crate::space::EntryId`] uses, which keeps this module free
//! of a dependency — and in a program that ships privileged code, a dependency avoided is worth
//! something.
//!
//! # Two things that are not duplicates
//!
//! - **Hard links to one inode.** Two paths, one file, one set of blocks. Reporting them as duplicates
//!   would invite a user to "reclaim" space that does not exist, and deleting one frees nothing.
//! - **Symbolic links.** Never followed, here as everywhere else in nix.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, Result};
use crate::op::CancelToken;
use crate::space::{SpaceEntry, SpaceTree};

/// How much of a file's head the second stage reads.
const HEAD_BYTES: usize = 4096;

/// Read buffer for whole-file work.
const CHUNK: usize = 64 * 1024;

/// Files below this are not considered for duplicate detection.
///
/// Duplicate small files are the normal state of a filesystem — every project has its own `LICENSE`,
/// every Python package an empty `__init__.py` — and reporting thousands of them buries the handful
/// that matter. The threshold is a policy about usefulness, not correctness.
pub const MIN_DUPLICATE_BYTES: u64 = 1024 * 1024;

/// The largest files in a scanned tree, biggest first.
///
/// Reads the tree the explorer already has, so this costs no filesystem access at all. Directories are
/// excluded — a directory's size is its contents', and listing both would show the same bytes twice.
/// Aggregate entries are excluded too: an entry standing for a thousand small files is not a file.
#[must_use]
pub fn largest_files(tree: &SpaceTree, limit: usize) -> Vec<SpaceEntry> {
    let mut files: Vec<SpaceEntry> = tree
        .entries
        .values()
        .filter(|e| !e.is_dir)
        .filter(|e| e.path.is_some())
        .cloned()
        .collect();

    files.sort_by_key(|e| std::cmp::Reverse(e.allocated));
    files.truncate(limit);
    files
}

/// A set of files with identical content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DuplicateGroup {
    /// Every path in the group. At least two, and each a distinct inode.
    pub paths: Vec<PathBuf>,
    /// On-disk bytes of one copy.
    #[ts(type = "number")]
    pub bytes: u64,
    /// What could be reclaimed by keeping one copy: `bytes * (paths.len() - 1)`.
    #[ts(type = "number")]
    pub recoverable: u64,
}

/// How much work duplicate detection did, so the cost is visible rather than guessed at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DuplicateStats {
    /// Files that reached stage one.
    #[ts(type = "number")]
    pub considered: u64,
    /// Files whose size was shared with at least one other.
    #[ts(type = "number")]
    pub size_matched: u64,
    /// Files whose first 4 KiB were hashed.
    #[ts(type = "number")]
    pub heads_hashed: u64,
    /// Files read in full.
    #[ts(type = "number")]
    pub fully_hashed: u64,
    /// Pairs compared byte for byte.
    #[ts(type = "number")]
    pub pairs_verified: u64,
}

/// What a completed duplicate search found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DuplicateReport {
    pub groups: Vec<DuplicateGroup>,
    pub stats: DuplicateStats,
    /// Sum of what keeping one copy of each group would return.
    #[ts(type = "number")]
    pub recoverable: u64,
    /// Whether the search was stopped early, so a short list is not mistaken for a clean bill.
    pub cancelled: bool,
}

/// FNV-1a over a byte slice, seeded so it can be chained across chunks.
const fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// Hash the first [`HEAD_BYTES`] of a file.
fn hash_head(path: &Path) -> Result<u64> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| AppError::from_io(&e, format!("read {}", path.display())).with_path(path))?;
    let mut buffer = vec![0u8; HEAD_BYTES];
    let mut filled = 0;
    // `read` may return short without being at end of file.
    while filled < HEAD_BYTES {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => {
                return Err(
                    AppError::from_io(&e, format!("read {}", path.display())).with_path(path)
                );
            }
        }
    }
    Ok(fnv1a(FNV_OFFSET, &buffer[..filled]))
}

/// Hash a whole file, checking for cancellation between chunks.
fn hash_whole(path: &Path, token: &CancelToken) -> Result<u64> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| AppError::from_io(&e, format!("read {}", path.display())).with_path(path))?;
    let mut buffer = vec![0u8; CHUNK];
    let mut hash = FNV_OFFSET;
    loop {
        token.check()?;
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => hash = fnv1a(hash, &buffer[..n]),
            Err(e) => {
                return Err(
                    AppError::from_io(&e, format!("read {}", path.display())).with_path(path)
                );
            }
        }
    }
    Ok(hash)
}

/// Whether two files are byte-for-byte identical.
///
/// The last stage, and the reason this module can promise no false positives. Only ever reached for a
/// pair whose sizes and full hashes already agree, so it almost always confirms — but "almost always"
/// is exactly the gap it exists to close.
fn identical(a: &Path, b: &Path, token: &CancelToken) -> Result<bool> {
    let mut fa = std::fs::File::open(a)
        .map_err(|e| AppError::from_io(&e, format!("read {}", a.display())).with_path(a))?;
    let mut fb = std::fs::File::open(b)
        .map_err(|e| AppError::from_io(&e, format!("read {}", b.display())).with_path(b))?;

    let mut ba = vec![0u8; CHUNK];
    let mut bb = vec![0u8; CHUNK];
    loop {
        token.check()?;
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
        if ba[..na] != bb[..nb] {
            return Ok(false);
        }
    }
}

/// Fill a buffer as far as the file allows, so two files are compared over the same spans.
fn read_full(file: &mut std::fs::File, buffer: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return Err(AppError::from_io(&e, "compare files")),
        }
    }
    Ok(filled)
}

/// One candidate file for duplicate detection.
#[derive(Debug, Clone)]
struct Entry {
    path: PathBuf,
    size: u64,
    allocated: u64,
    device: u64,
    inode: u64,
}

/// Collect candidates from a scanned tree.
fn candidates(tree: &SpaceTree, minimum: u64) -> Vec<Entry> {
    use std::os::unix::fs::MetadataExt;

    tree.entries
        .values()
        .filter(|e| !e.is_dir && e.allocated >= minimum)
        .filter_map(|e| {
            let path = e.path.clone()?;
            // `symlink_metadata`, so a symlink is never followed and never compared.
            let meta = std::fs::symlink_metadata(&path).ok()?;
            if !meta.is_file() {
                return None;
            }
            Some(Entry {
                size: meta.size(),
                allocated: meta.blocks() * 512,
                device: meta.dev(),
                inode: meta.ino(),
                path,
            })
        })
        .collect()
}

/// Find groups of files with identical content.
///
/// Staged so the expensive work only ever runs on what survived the cheap work, and cancellable at
/// every stage — including between chunks of a single large file, so cancelling does not wait for a
/// gigabyte to finish hashing.
pub fn duplicates(
    tree: &SpaceTree,
    minimum: u64,
    token: &CancelToken,
    progress: impl Fn(u64, u64),
) -> Result<(Vec<DuplicateGroup>, DuplicateStats)> {
    let mut stats = DuplicateStats::default();
    let all = candidates(tree, minimum);
    stats.considered = all.len() as u64;

    // Stage 1: size. Anything with a size of its own cannot have a twin.
    let mut by_size: HashMap<u64, Vec<Entry>> = HashMap::new();
    for entry in all {
        token.check()?;
        by_size.entry(entry.size).or_default().push(entry);
    }
    by_size.retain(|_, group| group.len() > 1);
    stats.size_matched = by_size.values().map(|g| g.len() as u64).sum();

    let total_to_hash = stats.size_matched;
    let mut hashed = 0u64;
    let mut groups = Vec::new();

    for (size, group) in by_size {
        token.check()?;

        // Stage 2: the head. Cheap, and settles most pairs that differ.
        let mut by_head: HashMap<u64, Vec<Entry>> = HashMap::new();
        for entry in group {
            token.check()?;
            let Ok(head) = hash_head(&entry.path) else {
                continue; // unreadable: not reported rather than guessed about
            };
            stats.heads_hashed += 1;
            by_head.entry(head).or_default().push(entry);
        }

        for (_, head_group) in by_head.into_iter().filter(|(_, g)| g.len() > 1) {
            token.check()?;

            // Stage 3: the whole file.
            let mut by_content: HashMap<u64, Vec<Entry>> = HashMap::new();
            for entry in head_group {
                let Ok(hash) = hash_whole(&entry.path, token) else {
                    continue;
                };
                stats.fully_hashed += 1;
                hashed += 1;
                progress(hashed, total_to_hash);
                by_content.entry(hash).or_default().push(entry);
            }

            for (_, mut matched) in by_content.into_iter().filter(|(_, g)| g.len() > 1) {
                token.check()?;

                // Hard links are one file with several names. The blocks are shared, so deleting a
                // name frees nothing, and calling them duplicates would promise space that is not
                // there. One name per inode survives.
                matched.sort_by(|a, b| a.path.cmp(&b.path));
                matched.dedup_by_key(|e| (e.device, e.inode));
                if matched.len() < 2 {
                    continue;
                }

                // Stage 4: byte for byte, against the first. This is what makes the promise.
                let first = matched[0].clone();
                let mut confirmed = vec![first.path.clone()];
                for other in &matched[1..] {
                    token.check()?;
                    stats.pairs_verified += 1;
                    if identical(&first.path, &other.path, token).unwrap_or(false) {
                        confirmed.push(other.path.clone());
                    }
                }

                if confirmed.len() > 1 {
                    let copies = confirmed.len() as u64 - 1;
                    groups.push(DuplicateGroup {
                        bytes: first.allocated,
                        recoverable: first.allocated * copies,
                        paths: confirmed,
                    });
                }
            }
        }
        let _ = size;
    }

    groups.sort_by_key(|g| std::cmp::Reverse(g.recoverable));
    Ok((groups, stats))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::space::EntryId;

    fn sandbox(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nix-find-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Build a tree naming exactly these paths, as a scan would.
    fn tree_of(paths: &[PathBuf]) -> SpaceTree {
        use std::os::unix::fs::MetadataExt;
        let mut tree = SpaceTree::new();
        for path in paths {
            let allocated = std::fs::symlink_metadata(path)
                .map(|m| m.blocks() * 512)
                .unwrap_or(0);
            let apparent = std::fs::symlink_metadata(path)
                .map(|m| m.size())
                .unwrap_or(0);
            tree.insert(SpaceEntry::walked(path.clone(), apparent, allocated, false));
        }
        tree
    }

    fn write(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    // ---- largest files ----

    #[test]
    fn largest_files_are_ordered_and_exclude_directories() {
        let mut tree = SpaceTree::new();
        tree.insert(SpaceEntry::walked(PathBuf::from("/a"), 100, 100, false));
        tree.insert(SpaceEntry::walked(PathBuf::from("/b"), 300, 300, false));
        tree.insert(SpaceEntry::walked(PathBuf::from("/c"), 200, 200, false));
        tree.insert(SpaceEntry::walked(PathBuf::from("/d"), 900, 900, true));

        let largest = largest_files(&tree, 10);
        assert_eq!(largest.len(), 3, "the directory must not be listed");
        assert_eq!(largest[0].allocated, 300);
        assert_eq!(largest[2].allocated, 100);
    }

    #[test]
    fn an_aggregate_is_not_a_file() {
        let mut tree = SpaceTree::new();
        tree.insert(SpaceEntry::walked(PathBuf::from("/a"), 100, 100, false));
        tree.insert(SpaceEntry::aggregated(Path::new("/dir"), 1000, 5000, 5000));

        let largest = largest_files(&tree, 10);
        assert_eq!(
            largest.len(),
            1,
            "an aggregate stands for files, it is not one"
        );
        assert_eq!(largest[0].path.as_deref(), Some(Path::new("/a")));
    }

    #[test]
    fn the_limit_is_respected() {
        let mut tree = SpaceTree::new();
        for i in 0..50u64 {
            tree.insert(SpaceEntry::walked(
                PathBuf::from(format!("/f{i}")),
                i * 10,
                i * 10,
                false,
            ));
        }
        assert_eq!(largest_files(&tree, 7).len(), 7);
        let _ = EntryId::for_path(Path::new("/f0"));
    }

    // ---- duplicates ----

    #[test]
    fn identical_files_are_found() {
        let dir = sandbox("dupes");
        let payload = vec![b'q'; 2 * 1024 * 1024];
        let a = write(&dir, "a.bin", &payload);
        let b = write(&dir, "b.bin", &payload);
        let c = write(&dir, "c.bin", &vec![b'z'; 2 * 1024 * 1024]);

        let tree = tree_of(&[a.clone(), b.clone(), c.clone()]);
        let (groups, stats) = duplicates(&tree, 1024, &CancelToken::new(), |_, _| {}).unwrap();

        assert_eq!(groups.len(), 1, "{groups:?}");
        assert_eq!(groups[0].paths.len(), 2);
        assert!(groups[0].paths.contains(&a) && groups[0].paths.contains(&b));
        assert!(!groups[0].paths.contains(&c));
        assert_eq!(groups[0].recoverable, groups[0].bytes);
        assert!(
            stats.pairs_verified >= 1,
            "the byte comparison must have run"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The criterion. Same size, same head, different tail — a hash-only design could report these.
    #[test]
    fn files_differing_only_at_the_end_are_not_duplicates() {
        let dir = sandbox("tail");
        let mut a_bytes = vec![b'x'; 2 * 1024 * 1024];
        let mut b_bytes = a_bytes.clone();
        *a_bytes.last_mut().unwrap() = b'1';
        *b_bytes.last_mut().unwrap() = b'2';

        let a = write(&dir, "a.bin", &a_bytes);
        let b = write(&dir, "b.bin", &b_bytes);

        let tree = tree_of(&[a, b]);
        let (groups, stats) = duplicates(&tree, 1024, &CancelToken::new(), |_, _| {}).unwrap();

        assert!(
            groups.is_empty(),
            "these differ in their last byte: {groups:?}"
        );
        assert!(
            stats.heads_hashed >= 2,
            "their heads are identical, so both should have been hashed"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two names for one inode are one file. Deleting a name would free nothing.
    #[test]
    fn hard_links_are_not_duplicates() {
        let dir = sandbox("links");
        let payload = vec![b'h'; 2 * 1024 * 1024];
        let a = write(&dir, "a.bin", &payload);
        let b = dir.join("b.bin");
        std::fs::hard_link(&a, &b).unwrap();

        let tree = tree_of(&[a, b]);
        let (groups, _) = duplicates(&tree, 1024, &CancelToken::new(), |_, _| {}).unwrap();
        assert!(
            groups.is_empty(),
            "one inode with two names shares its blocks: {groups:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A hard link alongside a genuine copy: the copy is reported, the link is not.
    #[test]
    fn a_hard_link_beside_a_real_copy_is_excluded_but_the_copy_is_not() {
        let dir = sandbox("mixed");
        let payload = vec![b'm'; 2 * 1024 * 1024];
        let a = write(&dir, "a.bin", &payload);
        let link = dir.join("a-link.bin");
        std::fs::hard_link(&a, &link).unwrap();
        let copy = write(&dir, "copy.bin", &payload);

        let tree = tree_of(&[a.clone(), link.clone(), copy.clone()]);
        let (groups, _) = duplicates(&tree, 1024, &CancelToken::new(), |_, _| {}).unwrap();

        assert_eq!(groups.len(), 1, "{groups:?}");
        let paths = &groups[0].paths;
        assert_eq!(paths.len(), 2, "three names, two inodes: {paths:?}");
        assert!(paths.contains(&copy));
        // Exactly one of the two names for the shared inode survives.
        assert_eq!(
            usize::from(paths.contains(&a)) + usize::from(paths.contains(&link)),
            1,
            "only one name per inode: {paths:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn symlinks_are_never_followed() {
        let dir = sandbox("symlink");
        let payload = vec![b's'; 2 * 1024 * 1024];
        let a = write(&dir, "a.bin", &payload);
        let link = dir.join("link.bin");
        std::os::unix::fs::symlink(&a, &link).unwrap();

        let tree = tree_of(&[a, link]);
        let (groups, _) = duplicates(&tree, 1024, &CancelToken::new(), |_, _| {}).unwrap();
        assert!(
            groups.is_empty(),
            "a symlink is not a copy of its target: {groups:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn three_copies_report_two_as_recoverable() {
        let dir = sandbox("three");
        let payload = vec![b'3'; 2 * 1024 * 1024];
        let paths: Vec<PathBuf> = (0..3)
            .map(|i| write(&dir, &format!("f{i}.bin"), &payload))
            .collect();

        let tree = tree_of(&paths);
        let (groups, _) = duplicates(&tree, 1024, &CancelToken::new(), |_, _| {}).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].paths.len(), 3);
        assert_eq!(
            groups[0].recoverable,
            groups[0].bytes * 2,
            "keeping one copy reclaims the other two"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn different_sizes_never_reach_a_hash() {
        let dir = sandbox("sizes");
        let a = write(&dir, "a.bin", &vec![b'a'; 2 * 1024 * 1024]);
        let b = write(&dir, "b.bin", &vec![b'a'; 2 * 1024 * 1024 + 1]);

        let tree = tree_of(&[a, b]);
        let (groups, stats) = duplicates(&tree, 1024, &CancelToken::new(), |_, _| {}).unwrap();
        assert!(groups.is_empty());
        assert_eq!(
            stats.heads_hashed, 0,
            "differing sizes are settled without reading anything"
        );
        assert_eq!(stats.size_matched, 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_minimum_size_is_respected() {
        let dir = sandbox("minimum");
        let payload = vec![b'p'; 4096];
        let a = write(&dir, "a.bin", &payload);
        let b = write(&dir, "b.bin", &payload);

        let tree = tree_of(&[a, b]);
        let (groups, stats) =
            duplicates(&tree, 1024 * 1024, &CancelToken::new(), |_, _| {}).unwrap();
        assert!(groups.is_empty(), "below the minimum, so not considered");
        assert_eq!(stats.considered, 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancellation_stops_the_work() {
        let dir = sandbox("cancel");
        let payload = vec![b'c'; 2 * 1024 * 1024];
        let a = write(&dir, "a.bin", &payload);
        let b = write(&dir, "b.bin", &payload);

        let tree = tree_of(&[a, b]);
        let token = CancelToken::new();
        token.cancel();

        let result = duplicates(&tree, 1024, &token, |_, _| {});
        match result {
            Err(e) => assert!(!e.is_fault(), "cancellation is not a fault"),
            Ok((groups, _)) => assert!(groups.is_empty()),
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Cancelling from inside the progress callback, so it happens while work is genuinely in flight
    /// rather than after a sleep and a hope.
    #[test]
    fn cancellation_from_progress_is_prompt() {
        let dir = sandbox("cancelprog");
        let payload = vec![b'c'; 2 * 1024 * 1024];
        let paths: Vec<PathBuf> = (0..6)
            .map(|i| write(&dir, &format!("f{i}.bin"), &payload))
            .collect();

        let tree = tree_of(&paths);
        let token = CancelToken::new();
        let seen = std::sync::atomic::AtomicU64::new(0);

        let result = duplicates(&tree, 1024, &token, |_, _| {
            seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            token.cancel();
        });

        assert!(
            seen.load(std::sync::atomic::Ordering::Relaxed) > 0,
            "progress should have been reported before cancelling"
        );
        match result {
            Err(e) => assert!(!e.is_fault()),
            Ok((groups, _)) => assert!(groups.len() <= 1),
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unreadable_file_is_skipped_rather_than_reported() {
        let dir = sandbox("unreadable");
        let payload = vec![b'u'; 2 * 1024 * 1024];
        let a = write(&dir, "a.bin", &payload);
        let missing = dir.join("gone.bin");
        std::fs::write(&missing, &payload).unwrap();

        let tree = tree_of(&[a, missing.clone()]);
        // Removed after the tree was built, exactly as a stale cache would present it.
        std::fs::remove_file(&missing).unwrap();

        let (groups, _) = duplicates(&tree, 1024, &CancelToken::new(), |_, _| {}).unwrap();
        assert!(
            groups.is_empty(),
            "a file that is gone cannot be a duplicate"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_input_is_not_an_error() {
        let tree = SpaceTree::new();
        let (groups, stats) = duplicates(&tree, 1024, &CancelToken::new(), |_, _| {}).unwrap();
        assert!(groups.is_empty());
        assert_eq!(stats.considered, 0);
    }

    #[test]
    fn the_hash_is_order_sensitive() {
        // A filter that ignored order would put many differing files in the same bucket, making the
        // expensive stages do far more work than they need to.
        assert_ne!(fnv1a(FNV_OFFSET, b"ab"), fnv1a(FNV_OFFSET, b"ba"));
        assert_eq!(fnv1a(FNV_OFFSET, b"same"), fnv1a(FNV_OFFSET, b"same"));
    }
}
