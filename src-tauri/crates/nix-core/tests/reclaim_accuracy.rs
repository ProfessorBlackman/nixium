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
use nix_core::reclaim::{Candidate, Category, ItemOutcome, Registry, Session};
use nix_core::space::{Category as SpaceCategory, ReclaimMethod, Safety};

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
            })
            .collect())
    }
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
        within_tolerance(report.freed, actually_left),
        "nix reported {} freed but {} actually left the tree — outside the {}% the specification allows",
        nix_core::format_bytes(report.freed),
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
        within_tolerance(report.freed, actually_left),
        "reported {} against a measured {}",
        report.freed,
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
        report.freed <= promised,
        "a report must never claim more than the preview promised: {} against {promised}",
        report.freed
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
        within_tolerance(report.freed, actually_left),
        "reported {} against measured {}",
        report.freed,
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

    // The freed figure counts only the item that was actually reclaimed.
    let claimed: u64 = report.outcomes.iter().map(ItemOutcome::bytes_freed).sum();
    assert_eq!(claimed, report.freed);
    assert!(kept.exists(), "the skipped file must still be there");
    assert!(!removed.exists());
}
