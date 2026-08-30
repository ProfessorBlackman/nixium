// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Growth history: trends, not detail. `STO-16`.
//!
//! # What is stored, and what deliberately is not
//!
//! A [`Sample`] is category totals plus the largest directories — a few kilobytes. Not a tree, not a
//! file list. The question this feature answers is *"`~/.cache` grew 4 GB this week"*, and answering it
//! does not require keeping a copy of the filesystem.
//!
//! That restraint is the whole design. A storage tool whose own data grows without limit is
//! indefensible, so retention is capped at [`MAX_SAMPLES`] and the oldest are dropped on write. The
//! cap is enforced in the writer rather than left to a cleanup task that might never run.
//!
//! # Gaps are gaps
//!
//! Principle P8: a missing sample is rendered as a **gap**, never interpolated. The machine was off,
//! or on battery, or the user had not opened nix — inventing a point between two real ones would turn
//! "we do not know" into a number someone might act on. [`Series::points`] therefore yields
//! `Option<Sample>` per interval, and the absent ones are absent.
//!
//! # A note on the collection job
//!
//! The specification requires the periodic job to be *"an incremental refresh against the existing
//! scan cache, never a fresh walk"*. That requirement came from `STO-18`, which was superseded: the
//! scan is now roughly twice as fast and its memory is bounded, so a full scan of this machine's home
//! directory takes 28 seconds — and under `Nice=19` with `IOSchedulingClass=idle`, once a day, that is
//! an acceptable cost for a correct answer. An incremental refresh would have needed a second code
//! path and a staleness model to be wrong about; see `PLAN.md` for the measurements.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, IoContext, Result};
use crate::space::Category;

/// How many samples to keep. One a day, so a little over a year.
///
/// A hard cap rather than an age cut-off, because the file's size is what has to be bounded and a
/// count bounds it directly. At roughly 4 KiB a sample this is about 1.5 MiB, which a storage tool can
/// defend spending on itself.
pub const MAX_SAMPLES: usize = 400;

/// File name inside the state directory.
const FILE: &str = "history.jsonl";

/// One directory's size at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DirSample {
    pub path: PathBuf,
    #[ts(type = "number")]
    pub bytes: u64,
}

/// One observation of where the space was.
///
/// Named `Sample` rather than `Snapshot` because [`crate::cow::Snapshot`] is a filesystem snapshot and
/// means something entirely different. Two exported types sharing a name silently overwrote each
/// other's TypeScript binding once already.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Sample {
    /// Seconds since the Unix epoch.
    #[ts(type = "number")]
    pub at: i64,
    /// On-disk bytes across everything scanned.
    #[ts(type = "number")]
    pub total_allocated: u64,
    /// Per-category totals. A list rather than a map, so its order is stable on the wire.
    pub by_category: Vec<(Category, u64)>,
    /// The largest directories, biggest first. Bounded by the caller.
    pub top_directories: Vec<DirSample>,
}

impl Sample {
    /// Build a sample from a scan result.
    ///
    /// `top` bounds the directory list, because this is the part that could otherwise grow with the
    /// filesystem — which is exactly what the retention cap exists to prevent.
    #[must_use]
    pub fn from_scan(at: i64, result: &crate::scan::ScanResult, top: usize) -> Self {
        let mut directories: Vec<DirSample> = result
            .tree
            .entries
            .values()
            .filter(|e| e.is_dir)
            .filter_map(|e| {
                Some(DirSample {
                    path: e.path.clone()?,
                    bytes: e.allocated,
                })
            })
            .collect();
        directories.sort_by_key(|d| std::cmp::Reverse(d.bytes));
        directories.truncate(top);

        let mut by_category = attribute(&result.tree);
        by_category.sort_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));

        Self {
            at,
            total_allocated: result.allocated,
            by_category,
            top_directories: directories,
        }
    }

    /// Bytes attributed to one category, or zero.
    #[must_use]
    pub fn category(&self, wanted: Category) -> u64 {
        self.by_category
            .iter()
            .find(|(c, _)| *c == wanted)
            .map_or(0, |(_, bytes)| *bytes)
    }

    /// Bytes recorded for one directory, if it was among the largest at the time.
    #[must_use]
    pub fn directory(&self, path: &Path) -> Option<u64> {
        self.top_directories
            .iter()
            .find(|d| d.path == path)
            .map(|d| d.bytes)
    }
}

/// Attribute a tree's bytes to categories, top down.
///
/// # Why top down, and not leaf by leaf
///
/// The first attempt classified leaves. It reported 0.14 GiB of build artifacts on a machine holding
/// **71 GiB** of them, and 74.76 GiB as unattributed. Two reasons, both structural:
///
/// - A `node_modules` directory is recognised by its own name and a marker beside it. Its *contents*
///   are ordinary files with ordinary names, so a leaf inside one matches nothing.
/// - An [`crate::space::SpaceEntry::aggregated`] entry has no path at all — it stands for a set of
///   them — so it could never be classified, and after `STO-19` those entries hold most of the bytes
///   of most directories.
///
/// Descending instead, and stopping at the first directory that classifies as something specific,
/// fixes both: the whole subtree is attributed to what its root is, aggregates included. Bytes are
/// counted once because a subtree that has been attributed is not descended into.
#[must_use]
pub fn attribute(tree: &crate::space::SpaceTree) -> Vec<(Category, u64)> {
    use std::collections::HashMap;

    let mut totals: HashMap<Category, u64> = HashMap::new();
    // Each entry carries the category of the directory it was found in, so an entry with no path of
    // its own can inherit rather than fall to `Unknown`.
    let mut stack: Vec<(crate::space::EntryId, Category)> = tree
        .roots
        .iter()
        .map(|id| (*id, Category::Unknown))
        .collect();

    while let Some((id, inherited)) = stack.pop() {
        let Some(entry) = tree.get(id) else { continue };

        // An aggregate entry has no path — it stands for a set of them — so what it holds belongs to
        // whatever its parent directory is. Leaving it `Unknown` put 31 GiB of a 307 GiB home
        // directory into an "unattributed" line that was really just small files in known places.
        let category = match entry.path.as_deref() {
            Some(path) => crate::reclaim::classify(path),
            None => inherited,
        };

        // A specific answer claims the whole subtree. `UserFile` and `Unknown` are not specific: they
        // are what a path says when nothing recognised it, so keep looking underneath.
        let decided = !matches!(category, Category::UserFile | Category::Unknown);

        if decided || entry.children.is_empty() {
            *totals.entry(category).or_default() += entry.allocated;
            continue;
        }

        // Bytes held directly here rather than in any child — which is what an aggregate entry is,
        // plus any rounding — stay with this entry's own classification.
        let in_children: u64 = entry
            .children
            .iter()
            .filter_map(|c| tree.get(*c))
            .map(|c| c.allocated)
            .sum();
        *totals.entry(category).or_default() += entry.allocated.saturating_sub(in_children);

        stack.extend(entry.children.iter().map(|child| (*child, category)));
    }

    totals.into_iter().filter(|(_, bytes)| *bytes > 0).collect()
}

/// How one thing changed between two samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Change {
    #[ts(type = "number")]
    pub from: u64,
    #[ts(type = "number")]
    pub to: u64,
    /// Signed difference. Growth is positive.
    #[ts(type = "number")]
    pub delta: i64,
}

impl Change {
    #[must_use]
    fn between(from: u64, to: u64) -> Self {
        Self {
            from,
            to,
            #[allow(clippy::cast_possible_wrap)]
            delta: to as i64 - from as i64,
        }
    }
}

/// A run of samples, with the gaps left in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Series {
    /// One entry per interval between the first and last sample. `None` where nothing was recorded.
    ///
    /// **Never interpolated** (§P8). A missing point means the machine was off, on battery, or nix was
    /// not running — and putting a plausible number there would turn "we do not know" into something a
    /// user might act on.
    pub points: Vec<Option<Sample>>,
    /// Seconds per interval, so the caller can label an axis.
    #[ts(type = "number")]
    pub interval: i64,
    /// How many intervals have no sample.
    #[ts(type = "number")]
    pub gaps: u64,
}

/// Bucket samples onto a fixed interval, leaving holes where there is no data.
///
/// Where two samples land in the same bucket the later one wins: a sample is an observation of the
/// present, so the freshest is the truest.
#[must_use]
pub fn series(samples: &[Sample], interval: i64) -> Series {
    if samples.is_empty() || interval <= 0 {
        return Series {
            points: Vec::new(),
            interval: interval.max(1),
            gaps: 0,
        };
    }

    let first = samples.iter().map(|s| s.at).min().unwrap_or(0);
    let last = samples.iter().map(|s| s.at).max().unwrap_or(0);
    let buckets = ((last - first) / interval + 1).unsigned_abs() as usize;

    let mut points: Vec<Option<Sample>> = vec![None; buckets];
    for sample in samples {
        let index = ((sample.at - first) / interval).unsigned_abs() as usize;
        if let Some(slot) = points.get_mut(index) {
            let replace = slot
                .as_ref()
                .is_none_or(|existing| existing.at <= sample.at);
            if replace {
                *slot = Some(sample.clone());
            }
        }
    }

    let gaps = points.iter().filter(|p| p.is_none()).count() as u64;
    Series {
        points,
        interval,
        gaps,
    }
}

/// The change in total between the oldest and newest sample within a window.
///
/// Returns `None` when fewer than two samples fall inside it: a trend needs two points, and reporting
/// growth from one is arithmetic on a number that does not exist.
#[must_use]
pub fn growth(samples: &[Sample], since: i64) -> Option<Change> {
    let mut inside: Vec<&Sample> = samples.iter().filter(|s| s.at >= since).collect();
    if inside.len() < 2 {
        return None;
    }
    inside.sort_by_key(|s| s.at);
    Some(Change::between(
        inside.first()?.total_allocated,
        inside.last()?.total_allocated,
    ))
}

/// The directories that grew most between the oldest and newest sample in a window.
#[must_use]
pub fn fastest_growing(samples: &[Sample], since: i64, limit: usize) -> Vec<(PathBuf, Change)> {
    let mut inside: Vec<&Sample> = samples.iter().filter(|s| s.at >= since).collect();
    if inside.len() < 2 {
        return Vec::new();
    }
    inside.sort_by_key(|s| s.at);
    let (Some(oldest), Some(newest)) = (inside.first(), inside.last()) else {
        return Vec::new();
    };

    let mut changes: Vec<(PathBuf, Change)> = newest
        .top_directories
        .iter()
        .filter_map(|now| {
            // A directory absent from the older sample was not among the largest then. That is not the
            // same as having been empty, so no change is claimed for it.
            let before = oldest.directory(&now.path)?;
            Some((now.path.clone(), Change::between(before, now.bytes)))
        })
        .filter(|(_, change)| change.delta != 0)
        .collect();

    changes.sort_by_key(|(_, change)| std::cmp::Reverse(change.delta));
    changes.truncate(limit);
    changes
}

/// What grew, and by how much, over a window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GrowthReport {
    /// `None` when fewer than two samples fall in the window: a trend needs two points, and reporting
    /// growth from one is arithmetic on a number that does not exist.
    pub total: Option<Change>,
    /// Directories that changed, largest growth first.
    pub directories: Vec<(PathBuf, Change)>,
}

/// The stored history.
#[derive(Debug, Clone)]
pub struct History {
    dir: PathBuf,
}

impl History {
    /// History at an explicit directory. For tests.
    #[must_use]
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// History in the XDG state directory.
    pub fn discover() -> Result<Self> {
        let dir = crate::paths::state_dir().ok_or_else(|| {
            AppError::internal("Could not resolve a state directory to keep history in.")
        })?;
        Ok(Self::at(dir))
    }

    #[must_use]
    pub fn file(&self) -> PathBuf {
        self.dir.join(FILE)
    }

    /// Every stored sample, oldest first. A malformed line is skipped rather than failing the read.
    #[must_use]
    pub fn samples(&self) -> Vec<Sample> {
        let Ok(text) = std::fs::read_to_string(self.file()) else {
            return Vec::new();
        };
        let mut samples: Vec<Sample> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        samples.sort_by_key(|s| s.at);
        samples
    }

    /// Append a sample, enforcing the retention cap.
    ///
    /// Written whole and atomically rather than appended to, because the cap means the file is
    /// sometimes shorter than it was, and because a half-written line would poison every later read.
    pub fn record(&self, sample: &Sample) -> Result<()> {
        std::fs::create_dir_all(&self.dir).doing("create the history directory")?;

        let mut samples = self.samples();
        samples.push(sample.clone());
        samples.sort_by_key(|s| s.at);
        // Drop from the front: the cap bounds the file, and the oldest data is the least useful.
        if samples.len() > MAX_SAMPLES {
            let excess = samples.len() - MAX_SAMPLES;
            samples.drain(..excess);
        }

        let mut body = String::new();
        for sample in &samples {
            let line = serde_json::to_string(sample)
                .map_err(|e| AppError::internal(format!("could not serialise a sample: {e}")))?;
            body.push_str(&line);
            body.push('\n');
        }

        crate::fs::write_atomically(&self.file(), body.as_bytes())
    }

    /// Delete all collected history.
    ///
    /// The specification requires that disabling collection deletes the data. Offering collection
    /// without offering deletion would make the feature something done *to* a user.
    pub fn clear(&self) -> Result<()> {
        match std::fs::remove_file(self.file()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AppError::from_io(&e, "delete the history")),
        }
    }

    /// Bytes the history occupies. A storage tool should be able to answer this about itself.
    #[must_use]
    pub fn size_on_disk(&self) -> u64 {
        std::fs::metadata(self.file()).map(|m| m.len()).unwrap_or(0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::space::{SpaceEntry, SpaceTree};

    fn sandbox(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nix-history-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample(at: i64, total: u64) -> Sample {
        Sample {
            at,
            total_allocated: total,
            by_category: vec![(Category::AppCache, total / 2)],
            top_directories: vec![DirSample {
                path: PathBuf::from("/home/u/.cache"),
                bytes: total / 2,
            }],
        }
    }

    const DAY: i64 = 86_400;

    #[test]
    fn samples_round_trip_and_survive_a_restart() {
        let dir = sandbox("roundtrip");
        let history = History::at(&dir);

        history.record(&sample(1000, 500)).unwrap();
        history.record(&sample(2000, 700)).unwrap();

        // A fresh handle, as a new process would have.
        let reopened = History::at(&dir);
        let stored = reopened.samples();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].at, 1000);
        assert_eq!(stored[1].total_allocated, 700);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The retention cap is the whole defence against a storage tool growing without limit.
    #[test]
    fn retention_is_capped_and_drops_the_oldest() {
        let dir = sandbox("cap");
        let history = History::at(&dir);

        for i in 0..(MAX_SAMPLES as i64 + 25) {
            history.record(&sample(i, i.unsigned_abs())).unwrap();
        }

        let stored = history.samples();
        assert_eq!(
            stored.len(),
            MAX_SAMPLES,
            "the cap must be enforced on write"
        );
        assert_eq!(
            stored[0].at, 25,
            "the oldest samples are the ones dropped, not the newest"
        );
        assert_eq!(stored.last().unwrap().at, MAX_SAMPLES as i64 + 24);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clearing_removes_everything_and_is_idempotent() {
        let dir = sandbox("clear");
        let history = History::at(&dir);
        history.record(&sample(1, 1)).unwrap();
        assert!(history.size_on_disk() > 0);

        history.clear().unwrap();
        assert!(history.samples().is_empty());
        assert_eq!(history.size_on_disk(), 0);
        // Clearing again must not fail: the user may press it twice.
        history.clear().unwrap();

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_malformed_line_does_not_lose_the_rest() {
        let dir = sandbox("malformed");
        let history = History::at(&dir);
        history.record(&sample(1, 100)).unwrap();

        // As a partial write or a older format would leave it.
        let mut text = std::fs::read_to_string(history.file()).unwrap();
        text.push_str("{not json\n");
        text.push_str(&serde_json::to_string(&sample(2, 200)).unwrap());
        text.push('\n');
        std::fs::write(history.file(), text).unwrap();

        let stored = history.samples();
        assert_eq!(stored.len(), 2, "the readable lines must survive");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_file_reads_as_no_history_rather_than_an_error() {
        let history = History::at(sandbox("absent").join("nested"));
        assert!(history.samples().is_empty());
        assert_eq!(history.size_on_disk(), 0);
    }

    // ---- attribution ----

    /// The property that makes a sample's category list trustworthy: it accounts for everything, once.
    #[test]
    fn attribution_accounts_for_every_byte_exactly_once() {
        use crate::fixture::{Fixture, Spec};
        let fixture = Fixture::create(&Spec::default()).unwrap();
        let result = crate::scan::scan_quiet(
            crate::scan::Options::new(fixture.root()).max_depth(None),
            crate::op::CancelToken::new(),
        )
        .unwrap();

        let totals = attribute(&result.tree);
        let summed: u64 = totals.iter().map(|(_, bytes)| *bytes).sum();
        let root = result.tree.get(result.tree.roots[0]).unwrap();

        assert_eq!(
            summed, root.allocated,
            "attribution must neither lose bytes nor count them twice"
        );

        std::fs::remove_dir_all(fixture.root()).ok();
    }

    /// # Regression
    ///
    /// Attribution first classified **leaves**. On a machine holding 71 GiB of build artifacts it
    /// reported 0.14 GiB of them, because a `node_modules` directory is recognised by its own name
    /// while the files inside it have ordinary names and match nothing.
    #[test]
    fn a_recognised_directory_claims_its_whole_subtree() {
        let mut tree = SpaceTree::new();
        // Shaped like a scan of a Cargo project: the marker is checked on disk, so use a real one.
        let dir = sandbox("subtree");
        std::fs::write(dir.join("Cargo.toml"), b"[package]").unwrap();
        let target = dir.join("target");
        std::fs::create_dir_all(&target).unwrap();

        let mut root = SpaceEntry::walked(dir.clone(), 10_000, 10_000, true);
        let mut artifact = SpaceEntry::walked(target.clone(), 9_000, 9_000, true);
        let inside = SpaceEntry::walked(target.join("debug.o"), 9_000, 9_000, false);
        artifact.children = vec![inside.id];
        root.children = vec![artifact.id];

        tree.insert(inside);
        tree.insert(artifact);
        let root_id = tree.insert_root(root);
        let _ = root_id;

        let totals: std::collections::HashMap<Category, u64> =
            attribute(&tree).into_iter().collect();
        assert_eq!(
            totals.get(&Category::BuildArtifact).copied(),
            Some(9_000),
            "the whole target/ subtree is build output: {totals:?}"
        );
        assert_eq!(
            totals.values().sum::<u64>(),
            10_000,
            "and nothing is counted twice: {totals:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// # Regression
    ///
    /// An aggregate entry has no path, so it classified as `Unknown` — putting 31 GiB of a 307 GiB
    /// home directory into an "unattributed" line that was really small files in known places.
    ///
    /// The first version of this test passed with the bug reintroduced, because its aggregate sat
    /// inside a *recognised* directory — which claims its whole subtree, so the aggregate was never
    /// reached and its classification never ran. The fixture has to put the aggregate somewhere
    /// attribution actually descends into, which means a directory classified `UserFile`.
    #[test]
    fn an_aggregate_inherits_the_category_of_its_directory() {
        // Under the real home directory, so `classify` answers `UserFile` — which is deliberately
        // *not* decisive, so attribution descends and the aggregate is reached. No file needs to
        // exist: nothing here is a recognised artifact name, so `classify` never touches the disk.
        let Some(home) = crate::paths::home_dir() else {
            return;
        };
        let parent = home.join("nix-test-documents");

        let mut tree = SpaceTree::new();
        let aggregate = SpaceEntry::aggregated(&parent, 500, 8_000, 8_000);
        let mut dir = SpaceEntry::walked(parent.clone(), 8_000, 8_000, true);
        dir.children = vec![aggregate.id];
        tree.insert(aggregate);
        tree.insert_root(dir);

        let totals: std::collections::HashMap<Category, u64> =
            attribute(&tree).into_iter().collect();
        assert_eq!(
            totals.get(&Category::Unknown).copied().unwrap_or(0),
            0,
            "an aggregate in a user directory is user files, not unattributed: {totals:?}"
        );
        assert_eq!(
            totals.get(&Category::UserFile).copied(),
            Some(8_000),
            "{totals:?}"
        );
    }

    #[test]
    fn an_empty_tree_attributes_nothing() {
        assert!(attribute(&SpaceTree::new()).is_empty());
    }

    // ---- gaps ----

    /// Principle P8. The reason this feature has a `Series` type at all.
    #[test]
    fn gaps_are_left_as_gaps_and_never_interpolated() {
        let samples = vec![sample(0, 100), sample(DAY, 200), sample(4 * DAY, 500)];
        let s = series(&samples, DAY);

        assert_eq!(s.points.len(), 5, "day 0 through day 4");
        assert!(s.points[0].is_some());
        assert!(s.points[1].is_some());
        assert!(
            s.points[2].is_none(),
            "day 2 has no sample and must stay empty"
        );
        assert!(s.points[3].is_none(), "day 3 likewise");
        assert!(s.points[4].is_some());
        assert_eq!(s.gaps, 2);

        // Nothing between 200 and 500 was invented.
        let values: Vec<u64> = s
            .points
            .iter()
            .filter_map(|p| p.as_ref().map(|s| s.total_allocated))
            .collect();
        assert_eq!(values, vec![100, 200, 500]);
    }

    #[test]
    fn two_samples_in_one_interval_keep_the_later() {
        let samples = vec![sample(0, 100), sample(DAY / 2, 300)];
        let s = series(&samples, DAY);
        assert_eq!(s.points.len(), 1);
        assert_eq!(
            s.points[0].as_ref().unwrap().total_allocated,
            300,
            "a sample observes the present, so the freshest is the truest"
        );
    }

    #[test]
    fn an_empty_history_makes_an_empty_series() {
        let s = series(&[], DAY);
        assert!(s.points.is_empty());
        assert_eq!(s.gaps, 0);
        // A nonsense interval must not divide by zero.
        assert!(series(&[sample(0, 1)], 0).points.is_empty());
    }

    // ---- trends ----

    #[test]
    fn growth_needs_two_points() {
        assert!(growth(&[], 0).is_none());
        assert!(
            growth(&[sample(0, 100)], 0).is_none(),
            "a trend from one point is arithmetic on a number that does not exist"
        );

        let change = growth(&[sample(0, 100), sample(DAY, 250)], 0).unwrap();
        assert_eq!(change.from, 100);
        assert_eq!(change.to, 250);
        assert_eq!(change.delta, 150);
    }

    #[test]
    fn shrinking_is_reported_as_a_negative_delta() {
        let change = growth(&[sample(0, 900), sample(DAY, 400)], 0).unwrap();
        assert_eq!(change.delta, -500, "reclaiming space is a real answer too");
    }

    #[test]
    fn the_window_excludes_older_samples() {
        let samples = vec![sample(0, 100), sample(10 * DAY, 200), sample(11 * DAY, 260)];
        let change = growth(&samples, 10 * DAY).unwrap();
        assert_eq!(
            change.from, 200,
            "the sample at day 0 is outside the window"
        );
        assert_eq!(change.delta, 60);
    }

    #[test]
    fn fastest_growing_directories_are_ranked() {
        let mut old = sample(0, 1000);
        old.top_directories = vec![
            DirSample {
                path: PathBuf::from("/a"),
                bytes: 100,
            },
            DirSample {
                path: PathBuf::from("/b"),
                bytes: 500,
            },
            DirSample {
                path: PathBuf::from("/c"),
                bytes: 700,
            },
        ];
        let mut new = sample(DAY, 3000);
        new.top_directories = vec![
            DirSample {
                path: PathBuf::from("/a"),
                bytes: 1100,
            },
            DirSample {
                path: PathBuf::from("/b"),
                bytes: 600,
            },
            DirSample {
                path: PathBuf::from("/c"),
                bytes: 700,
            },
            DirSample {
                path: PathBuf::from("/d"),
                bytes: 900,
            },
        ];

        let ranked = fastest_growing(&[old, new], 0, 10);
        assert_eq!(ranked.len(), 2, "{ranked:?}");
        assert_eq!(ranked[0].0, PathBuf::from("/a"));
        assert_eq!(ranked[0].1.delta, 1000);
        assert_eq!(ranked[1].1.delta, 100);

        // `/c` did not change, so it is not listed. `/d` was not in the older sample, so nothing is
        // claimed about it — absent from the top-N is not the same as having been empty.
        assert!(!ranked.iter().any(|(p, _)| p == Path::new("/c")));
        assert!(!ranked.iter().any(|(p, _)| p == Path::new("/d")));
    }

    #[test]
    fn a_directory_absent_from_the_older_sample_makes_no_claim() {
        let old = sample(0, 100);
        let mut new = sample(DAY, 5000);
        new.top_directories = vec![DirSample {
            path: PathBuf::from("/brand/new"),
            bytes: 4000,
        }];
        assert!(
            fastest_growing(&[old, new], 0, 10).is_empty(),
            "it may simply not have been among the largest before"
        );
    }

    #[test]
    fn one_sample_ranks_nothing() {
        assert!(fastest_growing(&[sample(0, 100)], 0, 10).is_empty());
    }

    #[test]
    fn a_sample_is_built_from_a_scan_and_stays_small() {
        use crate::fixture::{Fixture, Spec};
        let fixture = Fixture::create(&Spec::default()).unwrap();
        let result = crate::scan::scan_quiet(
            crate::scan::Options::new(fixture.root()).max_depth(None),
            crate::op::CancelToken::new(),
        )
        .unwrap();

        let built = Sample::from_scan(1_700_000_000, &result, 20);
        assert_eq!(built.total_allocated, result.allocated);
        assert!(
            built.top_directories.len() <= 20,
            "the list must be bounded"
        );
        assert!(
            built
                .top_directories
                .windows(2)
                .all(|w| w[0].bytes >= w[1].bytes),
            "largest first"
        );

        // The point of the design: a sample is kilobytes, not a copy of the filesystem.
        let encoded = serde_json::to_string(&built).unwrap();
        assert!(
            encoded.len() < 16 * 1024,
            "a sample grew to {} bytes; it is meant to be a trend, not a tree",
            encoded.len()
        );
    }
}
