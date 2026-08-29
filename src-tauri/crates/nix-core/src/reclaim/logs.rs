// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Logs and the systemd journal. Task 1.12 (`STO-6`).
//!
//! # journald is the point
//!
//! Stacer enumerated `/var/log` with a filter of "regular files only", and `/var/log/journal` is a
//! *directory* — so it skipped journald entirely. On a systemd machine the journal is routinely the
//! single largest log consumer, often gigabytes, which means Stacer's log cleaning missed the log
//! that actually mattered. It is a first-class category here.
//!
//! # Active logs are never offered
//!
//! An active log is rated [`Safety::Risky`] and, more importantly, **cannot be reclaimed at all**:
//! the privileged helper refuses any path whose filename does not look rotated, so even a caller
//! that asks is denied. Deleting a file a running service holds open frees nothing until the service
//! restarts, and can break its logging in the meantime.
//!
//! Open-handle detection is a second, independent signal: nix scans `/proc/*/fd` for handles on log
//! files and escalates anything it finds to `Risky` even if the name looks rotated. It is best
//! effort — other users' file descriptors are unreadable without privilege — so it *adds* caution
//! and never removes it.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::caps::{self, Capability};
use crate::error::Result;
use crate::op::CancelToken;
use crate::space::{
    Category as SpaceCategory, ReclaimKind, ReclaimMethod, Reclaimable, Safety, VacuumLimit,
};

use super::registry::{Candidate, Category};

/// Suffixes that mark a log as rotated. Kept in step with the helper's own list, which is the
/// authority — this copy only decides what to *offer*, and the helper decides what to allow.
const ROTATED_SUFFIXES: &[&str] = &[".gz", ".xz", ".bz2", ".zst", ".old"];

/// Whether a filename looks like a rotated log rather than a live one.
#[must_use]
pub(crate) fn is_rotated(name: &str) -> bool {
    if ROTATED_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        return true;
    }
    match name.rsplit_once('.') {
        Some((stem, digits)) => {
            !stem.is_empty() && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Paths currently held open by some process, as far as this process can see.
///
/// Best effort by nature: `/proc/<pid>/fd` is readable only for our own processes unless we are
/// root, so this under-reports. It is therefore used only to *add* caution — a file found here is
/// escalated to `Risky` — and never to clear a file that other rules doubt.
#[must_use]
pub(crate) fn open_paths() -> HashSet<PathBuf> {
    let mut open = HashSet::new();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return open;
    };

    for proc in procs.filter_map(std::result::Result::ok) {
        // Only numeric entries are processes.
        if !proc
            .file_name()
            .to_string_lossy()
            .chars()
            .all(|c| c.is_ascii_digit())
        {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(proc.path().join("fd")) else {
            continue; // another user's process, or it exited between listing and reading
        };
        for fd in fds.filter_map(std::result::Result::ok) {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                open.insert(target);
            }
        }
    }
    open
}

/// Rotated logs under `/var/log`.
pub struct LogCategory {
    root: PathBuf,
}

impl LogCategory {
    #[must_use]
    pub fn new() -> Self {
        Self {
            root: PathBuf::from("/var/log"),
        }
    }

    /// A category over an explicit root. For tests.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl Default for LogCategory {
    fn default() -> Self {
        Self::new()
    }
}

impl Category for LogCategory {
    fn id(&self) -> &'static str {
        "rotated_logs"
    }

    fn label(&self) -> &'static str {
        "Rotated logs"
    }

    fn explains(&self) -> &'static str {
        "Log files already rotated and compressed by logrotate — the numbered ones, not the file being written now. You lose the ability to look back at what happened on those days."
    }

    fn space_category(&self) -> SpaceCategory {
        SpaceCategory::Log
    }

    fn available(&self) -> bool {
        self.root.is_dir()
    }

    fn candidates(&self, token: &CancelToken) -> Result<Vec<Candidate>> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Ok(Vec::new());
        };
        let open = open_paths();
        let mut candidates = Vec::new();

        for entry in entries.filter_map(std::result::Result::ok) {
            token.check()?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();

            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            // Directories — journal/ among them — and symlinks are not rotated files.
            if !meta.is_file() || meta.file_type().is_symlink() {
                continue;
            }
            // An active log. The helper would refuse it regardless, so it is not even offered.
            if !is_rotated(&name) {
                continue;
            }
            // The independent signal: something still holds this open, so leave it alone.
            if open.contains(&path) {
                tracing::debug!(path = %path.display(), "skipping a rotated log that is still open");
                continue;
            }

            use std::os::unix::fs::MetadataExt;
            candidates.push(Candidate {
                label: format!("{name} (rotated log)"),
                bytes: meta.blocks() * 512,
                // The specification's own example of `Safe`: already superseded, nothing visible
                // is lost.
                safety: Safety::Safe,
                // One candidate per file, so a user can decline an individual log and so the
                // helper validates each path on its own rather than being handed a batch.
                method: ReclaimMethod::SystemFile {
                    kind: ReclaimKind::RotatedLog,
                    path: path.clone(),
                },
                cost: None,
                category: "rotated_logs".to_string(),
                reclaimable: Reclaimable::Exact,
                path,
            });
        }

        candidates.sort_by_key(|c| std::cmp::Reverse(c.bytes));
        Ok(candidates)
    }
}

/// The systemd journal, reclaimed by vacuuming rather than by deleting files.
pub struct JournalCategory {
    /// How much journal to keep. Vacuuming to a size rather than to nothing, because a journal with
    /// no history is a machine you cannot diagnose.
    keep: VacuumLimit,
    dirs: Vec<PathBuf>,
}

impl JournalCategory {
    #[must_use]
    pub fn new() -> Self {
        Self {
            // 200 MiB keeps a useful amount of recent history on a desktop.
            keep: VacuumLimit::Size { mebibytes: 200 },
            dirs: vec![
                PathBuf::from("/var/log/journal"),
                PathBuf::from("/run/log/journal"),
            ],
        }
    }

    /// A category with explicit directories and limit. For tests.
    #[must_use]
    pub fn with(keep: VacuumLimit, dirs: Vec<PathBuf>) -> Self {
        Self { keep, dirs }
    }

    /// Current on-disk size of the journal.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.dirs
            .iter()
            .map(|d| crate::fixture::directory_size(d))
            .sum()
    }

    /// Bytes the vacuum would leave behind.
    fn target_bytes(&self) -> u64 {
        match self.keep {
            VacuumLimit::Size { mebibytes } => mebibytes.saturating_mul(1024 * 1024),
            // An age limit's outcome cannot be predicted from size alone, so nothing is promised.
            VacuumLimit::Age { .. } => 0,
        }
    }
}

impl Default for JournalCategory {
    fn default() -> Self {
        Self::new()
    }
}

impl Category for JournalCategory {
    fn id(&self) -> &'static str {
        "journal"
    }

    fn label(&self) -> &'static str {
        "System journal"
    }

    fn explains(&self) -> &'static str {
        "Older systemd journal entries, vacuumed down to a limit you choose. The journal keeps recording; what goes is history. If you are investigating something that happened last week, do this afterwards."
    }

    fn space_category(&self) -> SpaceCategory {
        SpaceCategory::Journal
    }

    fn available(&self) -> bool {
        // Needs both a journal on disk and the tool that manages it: deleting journal files by hand
        // corrupts the journal, so without journalctl there is nothing safe to offer.
        caps::registry().has(Capability::Journalctl) && self.dirs.iter().any(|d| d.is_dir())
    }

    fn candidates(&self, token: &CancelToken) -> Result<Vec<Candidate>> {
        token.check()?;
        let current = self.size();
        let target = self.target_bytes();

        // Nothing to reclaim if the journal is already inside the limit. Offering a zero-byte
        // action would be noise.
        if current <= target {
            return Ok(Vec::new());
        }
        let reclaimable = current - target;

        let (label, cost) = match self.keep {
            VacuumLimit::Size { mebibytes } => (
                format!("System journal, trimmed to {mebibytes} MiB"),
                format!(
                    "Keeps the most recent {mebibytes} MiB of logs and discards the rest. Older entries cannot be recovered, so past problems become harder to diagnose."
                ),
            ),
            VacuumLimit::Age { days } => (
                format!("System journal, trimmed to {days} days"),
                format!(
                    "Discards journal entries older than {days} days. They cannot be recovered."
                ),
            ),
        };

        Ok(vec![Candidate {
            path: self.dirs.first().cloned().unwrap_or_default(),
            label,
            bytes: reclaimable,
            // Losing diagnostic history is a real cost, so never pre-checked.
            safety: Safety::Review,
            method: ReclaimMethod::JournalVacuum { limit: self.keep },
            cost: Some(cost),
            category: self.id().to_string(),
            reclaimable: Reclaimable::Exact,
        }])
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    struct FakeLogs {
        root: PathBuf,
    }

    impl FakeLogs {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "nix-logs-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn file(&self, name: &str, bytes: usize) -> PathBuf {
            let path = self.root.join(name);
            std::fs::write(&path, vec![b'x'; bytes]).unwrap();
            path
        }

        fn category(&self) -> LogCategory {
            LogCategory::at(&self.root)
        }
    }

    impl Drop for FakeLogs {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    // ---- what counts as rotated ----

    #[test]
    fn rotated_names_are_recognised_and_active_ones_are_not() {
        for rotated in [
            "syslog.1",
            "syslog.2.gz",
            "auth.log.4.xz",
            "messages.old",
            "kern.log.zst",
            "daemon.log.10",
            "dpkg.log.1.bz2",
        ] {
            assert!(is_rotated(rotated), "{rotated} should be rotated");
        }
        for active in [
            "syslog", "auth.log", "kern.log", "dmesg", "wtmp", "lastlog", "boot.log",
        ] {
            assert!(!is_rotated(active), "{active} is live");
        }
    }

    #[test]
    fn only_rotated_logs_are_offered() {
        let fake = FakeLogs::new("offer");
        fake.file("syslog", 8192); // active
        fake.file("syslog.1", 8192); // rotated
        fake.file("syslog.2.gz", 8192); // rotated
        fake.file("auth.log", 8192); // active

        let found = fake.category().candidates(&CancelToken::new()).unwrap();
        let names: Vec<&str> = found.iter().map(|c| c.label.as_str()).collect();

        assert_eq!(found.len(), 2, "{names:?}");
        assert!(
            names
                .iter()
                .all(|n| n.contains(".1") || n.contains(".2.gz")),
            "{names:?}"
        );
        assert!(
            !names.iter().any(|n| n.starts_with("syslog (")),
            "an active log must never be offered: {names:?}"
        );
    }

    /// The failure that made Stacer's log cleaning miss the biggest log on the machine.
    #[test]
    fn the_journal_directory_is_not_treated_as_a_log_file() {
        let fake = FakeLogs::new("journaldir");
        std::fs::create_dir_all(fake.root.join("journal")).unwrap();
        std::fs::write(fake.root.join("journal/system.journal"), vec![b'x'; 65536]).unwrap();
        fake.file("syslog.1", 4096);

        let found = fake.category().candidates(&CancelToken::new()).unwrap();
        assert_eq!(
            found.len(),
            1,
            "only the rotated file, never the journal directory"
        );
        assert!(found[0].label.contains("syslog.1"));
    }

    #[test]
    fn rotated_logs_are_rated_safe_and_route_through_the_helper() {
        let fake = FakeLogs::new("method");
        fake.file("syslog.1.gz", 4096);

        let found = fake.category().candidates(&CancelToken::new()).unwrap();
        assert_eq!(found[0].safety, Safety::Safe);
        match &found[0].method {
            ReclaimMethod::SystemFile { kind, path } => {
                assert_eq!(*kind, ReclaimKind::RotatedLog);
                assert!(path.ends_with("syslog.1.gz"));
            }
            other => panic!("a system log must be removed through the helper, got {other:?}"),
        }
    }

    #[test]
    fn symlinks_and_directories_are_skipped() {
        let fake = FakeLogs::new("skip");
        let real = fake.file("syslog.1", 4096);
        std::os::unix::fs::symlink(&real, fake.root.join("alias.1")).unwrap();
        std::fs::create_dir_all(fake.root.join("nginx")).unwrap();

        let found = fake.category().candidates(&CancelToken::new()).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn results_are_ordered_largest_first() {
        let fake = FakeLogs::new("order");
        fake.file("small.1", 4096);
        fake.file("large.1", 65536);
        fake.file("medium.1", 16384);

        let found = fake.category().candidates(&CancelToken::new()).unwrap();
        let sizes: Vec<u64> = found.iter().map(|c| c.bytes).collect();
        let mut sorted = sizes.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(sizes, sorted);
    }

    #[test]
    fn an_empty_or_absent_log_directory_offers_nothing() {
        let fake = FakeLogs::new("empty");
        assert!(
            fake.category()
                .candidates(&CancelToken::new())
                .unwrap()
                .is_empty()
        );

        let missing = LogCategory::at("/definitely/not/here");
        assert!(!missing.available());
        assert!(missing.candidates(&CancelToken::new()).unwrap().is_empty());
    }

    #[test]
    fn cancellation_is_honoured() {
        let fake = FakeLogs::new("cancel");
        for i in 0..5 {
            fake.file(&format!("log.{i}"), 4096);
        }
        let token = CancelToken::new();
        token.cancel();
        assert!(fake.category().candidates(&token).is_err());
    }

    // ---- open-handle detection ----

    #[test]
    fn open_handles_are_detected_for_our_own_process() {
        let path = std::env::temp_dir().join(format!("nix-openfd-{}.1", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        let handle = std::fs::File::open(&path).unwrap();

        let open = open_paths();
        assert!(
            open.contains(&path),
            "a file this process holds open must be detected"
        );

        drop(handle);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_rotated_log_still_held_open_is_not_offered() {
        let fake = FakeLogs::new("openlog");
        let path = fake.file("held.1", 4096);
        let handle = std::fs::File::open(&path).unwrap();

        let found = fake.category().candidates(&CancelToken::new()).unwrap();
        assert!(
            found.is_empty(),
            "a rotated-looking file that is still open must be left alone: {found:?}"
        );

        drop(handle);
    }

    // ---- the journal ----

    #[test]
    fn the_journal_is_offered_only_when_it_exceeds_the_limit() {
        let dir = std::env::temp_dir().join(format!("nix-journal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("system.journal"), vec![b'x'; 200_000]).unwrap();

        // A limit far above the journal's size: nothing to reclaim, so nothing offered.
        let under = JournalCategory::with(VacuumLimit::Size { mebibytes: 500 }, vec![dir.clone()]);
        assert!(
            under.candidates(&CancelToken::new()).unwrap().is_empty(),
            "an action that would free nothing is noise, not an offer"
        );

        // A limit below it: the difference is offered.
        let over = JournalCategory::with(VacuumLimit::Size { mebibytes: 0 }, vec![dir.clone()]);
        let found = over.candidates(&CancelToken::new()).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].bytes > 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_journal_is_vacuumed_not_deleted_and_is_never_pre_checked() {
        let dir = std::env::temp_dir().join(format!("nix-journal-vac-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("system.journal"), vec![b'x'; 100_000]).unwrap();

        let category = JournalCategory::with(VacuumLimit::Size { mebibytes: 0 }, vec![dir.clone()]);
        let found = category.candidates(&CancelToken::new()).unwrap();

        match &found[0].method {
            // Deleting journal files by hand corrupts the journal; the owning tool does it properly.
            ReclaimMethod::JournalVacuum { limit } => {
                assert_eq!(*limit, VacuumLimit::Size { mebibytes: 0 });
            }
            other => panic!("the journal must be vacuumed, not unlinked: {other:?}"),
        }
        assert_eq!(
            found[0].safety,
            Safety::Review,
            "losing diagnostic history is a real cost, so it is never pre-checked"
        );
        assert!(
            found[0]
                .cost
                .as_deref()
                .is_some_and(|c| c.contains("cannot be recovered")),
            "the cost must say the entries are gone for good: {:?}",
            found[0].cost
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_age_limit_promises_no_particular_size() {
        let dir = std::env::temp_dir().join(format!("nix-journal-age-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("system.journal"), vec![b'x'; 50_000]).unwrap();

        let category = JournalCategory::with(VacuumLimit::Age { days: 7 }, vec![dir.clone()]);
        let found = category.candidates(&CancelToken::new()).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].label.contains("7 days"), "{}", found[0].label);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_journal_needs_its_tool_to_be_available() {
        // Without journalctl there is nothing safe to offer, because deleting journal files by
        // hand corrupts the journal.
        let category = JournalCategory::with(
            VacuumLimit::Size { mebibytes: 1 },
            vec![PathBuf::from("/definitely/not/here")],
        );
        assert!(
            !category.available(),
            "no journal directory means nothing to do"
        );
    }
}
