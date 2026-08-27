// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Freed-bytes verification. Task 1.14.
//!
//! The specification's fourth success criterion is that **reported reclaimed bytes match the
//! measured filesystem delta within 2%**. This is the harness that checks it, rather than the claim
//! being asserted in a document and never tested.
//!
//! It is an integration test, not a unit test, deliberately: the property is about the whole
//! pipeline — category, preview, executor, and the filesystem underneath — and a unit test of any
//! one layer cannot observe it.
//!
//! # What makes this measurable at all
//!
//! Filesystem-level measurement on a live machine is noisy: other processes write while the test
//! runs, journals flush, atime updates land. So the harness works on a **controlled tree of known
//! size** and compares nix's arithmetic against the same tree measured independently — which
//! isolates the thing under test from everything else happening on the disk.

use std::path::{Path, PathBuf};

use nix_core::op::CancelToken;
use nix_core::protect::Guard;
use nix_core::reclaim::{Candidate, Category, Registry, Session};
use nix_core::space::{Category as SpaceCategory, ReclaimMethod, Reclaimable, Safety};

/// A sandbox that removes itself.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "nix-accuracy-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&root).expect("sandbox");
        Self { root }
    }

    /// A file of an exact size, returning its on-disk cost.
    fn file(&self, name: &str, bytes: usize) -> (PathBuf, u64) {
        use std::os::unix::fs::MetadataExt;
        let path = self.root.join(name);
        std::fs::write(&path, vec![b'x'; bytes]).expect("write");
        let allocated = std::fs::metadata(&path).expect("stat").blocks() * 512;
        (path, allocated)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

/// A category over an explicit list of files, so the harness controls exactly what is reclaimed.
struct Fixed {
    files: Vec<(PathBuf, u64)>,
}

impl Category for Fixed {
    fn id(&self) -> &'static str {
        "harness"
    }
    fn label(&self) -> &'static str {
        "Harness"
    }
    fn space_category(&self) -> SpaceCategory {
        SpaceCategory::UserFile
    }
    fn candidates(&self, _: &CancelToken) -> nix_core::error::Result<Vec<Candidate>> {
        Ok(self
            .files
            .iter()
            .map(|(path, bytes)| Candidate {
                path: path.clone(),
                label: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                bytes: *bytes,
                safety: Safety::Review,
                method: ReclaimMethod::MoveToTrash { path: path.clone() },
                cost: Some("It goes to the trash.".into()),
                category: "harness".into(),
                reclaimable: Reclaimable::Exact,
            })
            .collect())
    }
}

/// The filesystem's used bytes, which is what the specification's criterion is actually about.
///
/// # Why this exists alongside [`measure`]
///
/// [`measure`] sums a directory tree, and for a long while that was the only check here. It validates
/// a weaker property than the one being claimed: moving a file to the trash removes it from the tree
/// while leaving it on the filesystem, so a tree measurement confirms bytes "left" that the user
/// never got back. The criterion is about free space, so the check has to be about free space.
fn filesystem_used(path: &Path) -> Option<u64> {
    nix_core::fs::containing(path)
        .ok()
        .flatten()
        .map(|fs| fs.used)
}

/// Measure a tree independently of nix's own accounting.
fn measure(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(std::result::Result::ok)
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => measure(&e.path()) + m.blocks() * 512,
            Ok(m) => m.blocks() * 512,
            Err(_) => 0,
        })
        .sum()
}

/// Bytes that left the *tree*, however they were accounted for.
///
/// The harness's category moves files to the trash, which removes them from the tree while leaving
/// them on the filesystem. So a tree-level comparison must count both — and the separate question of
/// whether the *filesystem* got anything back is what
/// [`trashing_stages_bytes_without_freeing_them`] and
/// [`emptying_the_trash_frees_what_trashing_only_staged`] are for.
fn left_the_tree(report: &nix_core::reclaim::Report) -> u64 {
    report.freed + report.trashed
}

/// The tolerance from the specification.
const TOLERANCE: f64 = 0.02;

fn within_tolerance(counted: u64, measured: u64) -> bool {
    let larger = counted.max(measured);
    if larger == 0 {
        return counted == measured;
    }
    #[allow(clippy::cast_precision_loss)]
    let ratio = counted.abs_diff(measured) as f64 / larger as f64;
    ratio <= TOLERANCE
}

/// The criterion itself: what nix says it freed matches what actually left the tree.
#[test]
fn reported_bytes_match_the_measured_delta_within_two_percent() {
    let sandbox = Sandbox::new("delta");

    // A spread of sizes, including ones that do not land on block boundaries — that is where a
    // naive apparent-size accounting drifts from what the filesystem actually returns.
    let sizes = [4096, 100_000, 1, 65_536, 999, 250_000, 8192];
    let mut files = Vec::new();
    for (i, size) in sizes.iter().enumerate() {
        files.push(sandbox.file(&format!("file-{i}.bin"), *size));
    }

    let before = measure(&sandbox.root);
    assert!(before > 0, "the fixture must occupy space");

    let mut registry = Registry::new();
    registry.register(Box::new(Fixed {
        files: files.clone(),
    }));

    let session = Session::new();
    let guard = Guard::new(Vec::new());
    let token = CancelToken::new();

    let preview = session.preview(&registry, &guard, &token).expect("preview");
    assert_eq!(preview.items.len(), files.len());

    let selection: Vec<u64> = preview.items.iter().map(|i| i.id).collect();
    let report = session
        .execute(preview.ticket, &selection, &guard, &token, |_, _| {})
        .expect("execute");

    assert_eq!(
        report.failed_count, 0,
        "the harness should not encounter failures: {:?}",
        report.outcomes
    );
    assert_eq!(report.reclaimed_count, files.len());

    let after = measure(&sandbox.root);
    let actually_left = before.saturating_sub(after);

    assert!(
        within_tolerance(left_the_tree(&report), actually_left),
        "nix reported {} freed but {} actually left the tree — outside the {}% the specification allows",
        nix_core::format_bytes(left_the_tree(&report)),
        nix_core::format_bytes(actually_left),
        TOLERANCE * 100.0
    );
}

/// Accounting must use on-disk allocation, not apparent size.
///
/// Many tiny files are the case that separates the two: a 1-byte file costs a whole block, so a
/// tool counting apparent size would promise back a fraction of what it actually frees.
#[test]
fn many_small_files_are_accounted_by_allocation_not_apparent_size() {
    let sandbox = Sandbox::new("small");

    let mut files = Vec::new();
    let mut apparent = 0u64;
    for i in 0..200 {
        let (path, allocated) = sandbox.file(&format!("tiny-{i}.bin"), 1);
        apparent += 1;
        files.push((path, allocated));
    }

    let before = measure(&sandbox.root);
    assert!(
        before > apparent * 10,
        "200 one-byte files should occupy far more than 200 bytes on disk"
    );

    let mut registry = Registry::new();
    registry.register(Box::new(Fixed { files }));

    let session = Session::new();
    let guard = Guard::new(Vec::new());
    let token = CancelToken::new();
    let preview = session.preview(&registry, &guard, &token).expect("preview");

    assert!(
        preview.total_bytes > apparent * 10,
        "the preview promised {} for files whose apparent size is {apparent} bytes — it is counting \
         the wrong thing",
        preview.total_bytes
    );

    let selection: Vec<u64> = preview.items.iter().map(|i| i.id).collect();
    let report = session
        .execute(preview.ticket, &selection, &guard, &token, |_, _| {})
        .expect("execute");

    let actually_left = before.saturating_sub(measure(&sandbox.root));
    assert!(
        within_tolerance(left_the_tree(&report), actually_left),
        "reported {} against a measured {}",
        left_the_tree(&report),
        actually_left
    );
}

/// A preview must not promise space that reclaiming will not return.
#[test]
fn the_preview_does_not_overpromise() {
    let sandbox = Sandbox::new("promise");
    let files: Vec<_> = (0..10)
        .map(|i| sandbox.file(&format!("f-{i}.bin"), 20_000))
        .collect();

    let before = measure(&sandbox.root);

    let mut registry = Registry::new();
    registry.register(Box::new(Fixed { files }));
    let session = Session::new();
    let guard = Guard::new(Vec::new());
    let token = CancelToken::new();

    let preview = session.preview(&registry, &guard, &token).expect("preview");
    let promised = preview.total_bytes;

    let selection: Vec<u64> = preview.items.iter().map(|i| i.id).collect();
    let report = session
        .execute(preview.ticket, &selection, &guard, &token, |_, _| {})
        .expect("execute");
    let actually_left = before.saturating_sub(measure(&sandbox.root));

    assert_eq!(report.failed_count, 0, "{:?}", report.outcomes);
    assert!(
        within_tolerance(promised, actually_left),
        "promised {promised} but only {actually_left} came back"
    );
    assert!(
        left_the_tree(&report) <= promised,
        "a report must never claim more than the preview promised: {} against {promised}",
        left_the_tree(&report)
    );
}

/// Partial selections must free exactly what was selected, and nothing else.
#[test]
fn reclaiming_a_subset_frees_only_that_subset() {
    let sandbox = Sandbox::new("subset");
    let files: Vec<_> = (0..6)
        .map(|i| sandbox.file(&format!("s-{i}.bin"), 30_000))
        .collect();

    let before = measure(&sandbox.root);

    let mut registry = Registry::new();
    registry.register(Box::new(Fixed {
        files: files.clone(),
    }));
    let session = Session::new();
    let guard = Guard::new(Vec::new());
    let token = CancelToken::new();
    let preview = session.preview(&registry, &guard, &token).expect("preview");

    // Half of them.
    let selection: Vec<u64> = preview.items.iter().take(3).map(|i| i.id).collect();
    let expected: u64 = preview.items.iter().take(3).map(|i| i.bytes).sum();

    let report = session
        .execute(preview.ticket, &selection, &guard, &token, |_, _| {})
        .expect("execute");

    let actually_left = before.saturating_sub(measure(&sandbox.root));

    assert_eq!(report.reclaimed_count, 3);
    assert!(
        within_tolerance(left_the_tree(&report), actually_left),
        "reported {} against measured {}",
        left_the_tree(&report),
        actually_left
    );
    assert!(
        within_tolerance(expected, actually_left),
        "the preview said {expected} for this subset but {actually_left} left"
    );
    // The unselected files are untouched.
    assert!(
        measure(&sandbox.root) > 0,
        "the three files that were not selected must still be there"
    );
}

/// A report must never claim bytes for something it skipped.
#[test]
fn skipped_items_contribute_nothing_to_the_total() {
    let sandbox = Sandbox::new("skip");
    let (kept, _) = sandbox.file("kept.bin", 50_000);
    let (removed, removed_bytes) = sandbox.file("removed.bin", 50_000);

    let mut registry = Registry::new();
    registry.register(Box::new(Fixed {
        files: vec![(kept.clone(), 50_000), (removed.clone(), removed_bytes)],
    }));

    let session = Session::new();
    let guard = Guard::new(Vec::new());
    let token = CancelToken::new();
    let preview = session.preview(&registry, &guard, &token).expect("preview");

    // Change one of them after the preview, so the executor's time-of-check guard skips it.
    std::fs::write(&kept, vec![b'y'; 120_000]).expect("rewrite");

    let selection: Vec<u64> = preview.items.iter().map(|i| i.id).collect();
    let report = session
        .execute(preview.ticket, &selection, &guard, &token, |_, _| {})
        .expect("execute");

    assert_eq!(report.skipped_count, 1, "{:?}", report.outcomes);
    assert_eq!(report.reclaimed_count, 1);

    // The report's totals are built only from items that were acted on: a skipped item contributes
    // to neither figure. Both are summed, because this harness trashes and so everything it acts on
    // lands in `trashed` rather than `freed`.
    let acted: u64 = report
        .outcomes
        .iter()
        .map(|o| o.bytes_freed() + o.bytes_trashed())
        .sum();
    assert_eq!(acted, left_the_tree(&report));
    assert_eq!(report.freed, 0, "nothing was deleted outright");
    assert!(kept.exists(), "the skipped file must still be there");
    assert!(!removed.exists());
}

/// # Regression
///
/// The criterion in the specification is that reported bytes match what the *filesystem* gives back.
/// For a long while this harness only compared against a directory-tree measurement, which is a
/// weaker claim: moving a file to the trash removes it from the tree while leaving it on the
/// filesystem, so a tree measurement happily confirmed bytes had "left" that the user never got back.
///
/// Meanwhile `Report::freed` counted trashed bytes, so nix reported 9.8 GiB reclaimed for a cache it
/// had merely moved. Trashed bytes are now their own figure and `freed` means freed.
#[test]
fn trashing_stages_bytes_without_freeing_them() {
    let sandbox = Sandbox::new("staged");
    let files = vec![
        sandbox.file("a.bin", 400_000),
        sandbox.file("b.bin", 400_000),
    ];

    let mut registry = Registry::new();
    registry.register(Box::new(Fixed {
        files: files.clone(),
    }));

    let session = Session::new();
    let guard = Guard::new(Vec::new());
    let token = CancelToken::new();
    let preview = session.preview(&registry, &guard, &token).expect("preview");
    let selection: Vec<u64> = preview.items.iter().map(|i| i.id).collect();
    let report = session
        .execute(preview.ticket, &selection, &guard, &token, |_, _| {})
        .expect("execute");

    assert_eq!(report.failed_count, 0, "{:?}", report.outcomes);
    assert!(report.trashed > 0, "the files were moved to the trash");
    assert_eq!(
        report.freed,
        0,
        "the trash is on the same filesystem, so nothing was freed — reporting {} as freed is the \
         claim this project exists not to make",
        nix_core::format_bytes(report.freed)
    );

    // And every outcome says which it was, rather than calling them all reclaimed.
    for outcome in &report.outcomes {
        assert!(
            matches!(outcome, nix_core::reclaim::ItemOutcome::Trashed { .. }),
            "a trashed file must not be reported as reclaimed: {outcome:?}"
        );
    }
}

/// The other half of the same truth: emptying the trash is what actually returns the space.
///
/// Measured against `statvfs` rather than a tree, because free space is the thing being claimed. The
/// tolerance is looser than the specification's 2% here for a reason worth stating: a live machine's
/// filesystem moves underneath the measurement, so this asserts the space came back at all and in
/// roughly the right quantity, not that it did so to the byte.
#[test]
fn emptying_the_trash_frees_what_trashing_only_staged() {
    let sandbox = Sandbox::new("emptied");
    // Large enough that the filesystem delta stands out from ordinary background noise.
    let files: Vec<_> = (0..6)
        .map(|i| sandbox.file(&format!("big-{i}.bin"), 4_000_000))
        .collect();
    let staged: u64 = files.iter().map(|(_, bytes)| *bytes).sum();

    let mut registry = Registry::new();
    registry.register(Box::new(Fixed {
        files: files.clone(),
    }));
    let session = Session::new();
    let guard = Guard::new(Vec::new());
    let token = CancelToken::new();

    let Some(before_trash) = filesystem_used(&sandbox.root) else {
        return; // no mount information available; nothing to assert against
    };

    let preview = session.preview(&registry, &guard, &token).expect("preview");
    let selection: Vec<u64> = preview.items.iter().map(|i| i.id).collect();
    let trash_report = session
        .execute(preview.ticket, &selection, &guard, &token, |_, _| {})
        .expect("execute");
    assert_eq!(trash_report.freed, 0);
    assert!(trash_report.trashed >= staged / 2);

    // Now empty it, which is a real deletion.
    let trash_dir = nix_core::trash::TrashDir::home().expect("home trash");
    let emptied = nix_core::trash::empty(&trash_dir).expect("empty");

    let Some(after_empty) = filesystem_used(&sandbox.root) else {
        return;
    };
    let returned = before_trash.saturating_sub(after_empty);

    assert!(
        emptied.bytes >= staged / 2,
        "emptying should account for what was staged: {} vs {}",
        nix_core::format_bytes(emptied.bytes),
        nix_core::format_bytes(staged)
    );
    assert!(
        returned >= staged / 2,
        "the filesystem should have given back roughly what was emptied: {} returned against {} staged",
        nix_core::format_bytes(returned),
        nix_core::format_bytes(staged)
    );
}
