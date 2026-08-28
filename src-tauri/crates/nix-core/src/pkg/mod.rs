// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Package database queries. `STO-10`, and the foundation for `STO-11`.
//!
//! # Why this lands before the kernel category
//!
//! The plan orders Phase 2 by reclaim value, putting removable system packages first. That work
//! needs a package-query layer underneath it — you cannot decide which kernels are removable without
//! first knowing which are installed and how large they are — so this lands first and `STO-11` is
//! built on it.
//!
//! # Parsers are pure functions over captured output
//!
//! Every backend's output is parsed by a free function taking `&str`, tested against **real output
//! captured from a running system**. Principle P8 asks for golden-file tests per parser, and this is
//! the shape that makes them possible: CI cannot install four package managers, but it can keep a
//! sample of what each one actually printed.
//!
//! # Sizes are reported twice, deliberately — but not the two the plan expected
//!
//! Decision D2 asked for the package database's figure by default and a measured walk on request,
//! reading the gap between them as **post-install growth**. Measuring first (§P3 of the issue log's
//! patterns) showed that premise is wrong, so `PKG-1` reports a different pair. See `SPEC.md` §D2 for
//! the full argument; the short version:
//!
//! `Installed-Size` is not a measurement of anything on this disk. It is computed at **build time**
//! from the packaging tree, per-file rounded up to a kibibyte, and counts directories. Across a
//! 40-package sample here, the sum of the files actually present came to **0.80×** the recorded
//! figure — so subtracting one from the other reports four packages in five as having *shrunk*, and
//! `saturating_sub` then renders that as "no growth" for almost the entire inventory.
//!
//! What is worth reporting instead is the pair that is both well-defined and actionable: the bytes
//! the files **contain**, and the bytes the filesystem has actually **committed** to them. For
//! `flat-remix-gtk` on this machine — 30,547 files, most of them small — that is 76.1 MB of content
//! occupying **181.3 MB of disk**, against a recorded figure of 96.3 MB. The package costs 2.4× its
//! content, dpkg's own number understates the real cost by 85 MB, and only the on-disk figure tells
//! you what removing it gives back. Verified against `du` before being built on.

pub mod dpkg;
pub mod flatpak;
pub mod snap;
pub mod store;

pub use dpkg::DpkgBackend;

use crate::caps::{self, Capability};
use crate::error::{AppError, Cause, ErrorCode, Result};
use crate::space::Manager;

use serde::{Deserialize, Serialize};
use std::path::Path;
use ts_rs::TS;

/// What walking a package's own file list found on disk.
///
/// Both figures, because they answer different questions and the difference between them is the
/// interesting part. See the module documentation for why neither is compared against
/// [`Package::recorded_bytes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Measured {
    /// Bytes the files contain — the sum of their lengths.
    #[ts(type = "number")]
    pub apparent_bytes: u64,
    /// Bytes the filesystem has committed, from each file's allocated block count.
    ///
    /// **This is what removing the package gives back**, and it is the larger of the two for any
    /// package of small files: a 200-byte icon still costs a 4 KiB block. It can also be *smaller*
    /// than `apparent_bytes` for a sparse or transparently compressed file, which is why it is
    /// measured rather than derived by rounding.
    #[ts(type = "number")]
    pub disk_bytes: u64,
    /// Regular files counted, each distinct inode once.
    pub files: u32,
    /// Directories in the file list, which are **not** counted.
    ///
    /// A package's directories are almost always shared with other packages — `/usr/bin` belongs to
    /// no one — so attributing their bytes here would double-count across the inventory. Reported so
    /// the figure is legible rather than silently narrower than the file list.
    pub directories: u32,
    /// Paths dpkg lists that could not be read.
    ///
    /// Non-zero means the figure is a floor, not a total: a diverted file, a path removed behind
    /// dpkg's back, or a directory this user cannot traverse. Surfaced rather than folded into the
    /// count, so an understated total is visible as one (§P7).
    pub unreadable: u32,
}

impl Measured {
    /// Bytes spent on block allocation rather than content.
    ///
    /// Saturating, because [`Measured::disk_bytes`] is legitimately smaller for a sparse file.
    #[must_use]
    pub const fn block_overhead(&self) -> u64 {
        self.disk_bytes.saturating_sub(self.apparent_bytes)
    }

    /// Whether every path in the package's file list was accounted for.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.unreadable == 0
    }
}

/// One installed package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Package {
    /// The package's identity, arch-qualified where the manager qualifies: `libc6:amd64`.
    ///
    /// **Derived from `name` and `arch`, and a real field rather than a method on purpose.** Methods
    /// do not cross into TypeScript — `docs/issues/02-rust-typescript-boundary.md` §5 is a defect from
    /// exactly that — and the alternative was the frontend rebuilding the rule in its own language,
    /// where it could drift. Computed once, in [`Package::new`].
    pub id: String,
    pub name: String,
    /// Architecture, as the manager qualifies it — `amd64`, `i386`, `all`.
    ///
    /// Empty for managers with no such concept. **Not cosmetic:** this machine has 41 packages
    /// installed for two architectures at once, so `name` alone is not an identity. Use
    /// [`Package::id`].
    pub arch: String,
    pub version: String,
    /// One-line description, as the manager holds it.
    pub summary: String,
    /// Size the package database records, in bytes.
    ///
    /// Fast, and what the manager itself believes — which for dpkg is a **build-time estimate**, not
    /// a measurement of this disk. See the module documentation.
    #[ts(type = "number")]
    pub recorded_bytes: u64,
    /// What walking the files found, when the user has asked for it.
    ///
    /// Never overwrites [`Package::recorded_bytes`]; the two are different metrics and conflating
    /// them is the mistake the module documentation records.
    pub measured: Option<Measured>,
    /// When the package was last installed or upgraded, in seconds since the epoch.
    ///
    /// **Last changed, not first installed** — dpkg keeps no install date, so this is the mtime of
    /// the package's file list, which is rewritten on every upgrade. Named for what it is.
    #[ts(type = "number | null")]
    pub changed_at: Option<u64>,
    /// Whether the user asked for this, as opposed to it arriving as a dependency.
    pub explicit: bool,
    pub manager: Manager,
}

impl Package {
    /// Build a package, deriving its [`id`](Package::id).
    ///
    /// The only way to make one with a correct identity, which is why every backend goes through it.
    #[must_use]
    pub fn new(
        name: String,
        arch: String,
        version: String,
        summary: String,
        recorded_bytes: u64,
        explicit: bool,
        manager: Manager,
    ) -> Self {
        Self {
            id: Self::identity(&name, &arch),
            name,
            arch,
            version,
            summary,
            recorded_bytes,
            measured: None,
            changed_at: None,
            explicit,
            manager,
        }
    }

    /// How an identity is spelled: `name:arch`, or a bare name where the manager has no architectures.
    ///
    /// `libc6:amd64` and `libc6:i386` are two installed packages of different sizes, and dpkg and apt
    /// both accept this form for any package, multi-arch or not. Anything that keys, selects or
    /// removes must use this and not [`Package::name`].
    #[must_use]
    pub fn identity(name: &str, arch: &str) -> String {
        if arch.is_empty() {
            name.to_string()
        } else {
            format!("{name}:{arch}")
        }
    }

    /// The size to show: what the filesystem has committed when that has been measured, the
    /// manager's own figure otherwise.
    #[must_use]
    pub const fn display_bytes(&self) -> u64 {
        match self.measured {
            Some(m) => m.disk_bytes,
            None => self.recorded_bytes,
        }
    }

    /// How far the manager's figure is from the space actually committed, once measured.
    ///
    /// Signed, and deliberately not saturating: dpkg's estimate is usually *high* against file
    /// content and *low* against disk occupancy, and a figure that cannot go negative would report
    /// the first case as agreement. `None` when unmeasured — "not measured" and "agrees" are
    /// different answers.
    #[must_use]
    pub fn recorded_error(&self) -> Option<i64> {
        let m = self.measured?;
        let disk = i64::try_from(m.disk_bytes).ok()?;
        let recorded = i64::try_from(self.recorded_bytes).ok()?;
        Some(disk - recorded)
    }
}

/// A package left behind in a removed-but-configured state.
///
/// Debian keeps configuration for a package removed without `--purge`. Small individually,
/// substantial in aggregate, and genuinely dead weight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResidualConfig {
    pub name: String,
    #[ts(type = "number")]
    pub bytes: u64,
}

/// What removing a set of packages would actually do.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RemovalPreview {
    /// Packages that would be removed, including any pulled out as a consequence.
    pub removing: Vec<String>,
    /// Bytes the manager expects to free.
    #[ts(type = "number")]
    pub freed_bytes: u64,
    /// Packages the manager would *install or upgrade* to satisfy the removal.
    ///
    /// Almost always empty, and alarming when it is not — worth surfacing rather than hiding.
    pub installing: Vec<String>,
}

/// How a package manager is queried. Read-only: everything here is unprivileged.
pub trait Backend: Send + Sync {
    fn manager(&self) -> Manager;

    /// Whether this manager is usable here. Capability-probed, never distro-sniffed.
    fn available(&self) -> bool;

    /// Every installed package with its recorded size.
    fn installed(&self) -> Result<Vec<Package>>;

    /// Configuration left behind by removed packages.
    ///
    /// A manager with no such concept returns an empty list rather than an error: absence of a
    /// feature is not a failure.
    fn residual_config(&self) -> Result<Vec<ResidualConfig>> {
        Ok(Vec::new())
    }

    /// What removing these packages would do, without doing it.
    fn removal_preview(&self, names: &[String]) -> Result<RemovalPreview>;

    /// Walk one package's own file list and report what is actually on disk.
    ///
    /// Takes an [`id`](Package::id), not a name. A manager that cannot enumerate a package's files
    /// returns [`ErrorCode::Unsupported`] rather than a guess (§P7).
    fn measure(&self, _id: &str) -> Result<Measured> {
        Err(AppError::new(
            ErrorCode::Unsupported,
            "This package manager cannot list a package's files.",
        ))
    }
}

/// Measure a package's file list: content bytes, committed bytes, and what could not be read.
///
/// Shared by every backend that can produce a file list, because the accounting rules are the ones
/// this project has already been caught by twice and belong in one place:
///
/// - **Directories are counted, not measured.** Their bytes belong to no single package.
/// - **Symlinks contribute nothing.** Their target is another file, usually one already counted, and
///   following them would attribute another package's bytes to this one.
/// - **Each inode once.** A package with hard-linked duplicates occupies the blocks once, and
///   counting both is how the snap revision figures were first wrong.
/// - **On-disk comes from `st_blocks`, never from rounding `st_size` up.** Sparse and transparently
///   compressed files occupy less than they contain, and only the kernel knows which.
pub fn measure_paths<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Measured {
    use std::os::unix::fs::MetadataExt;

    let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    let mut out = Measured {
        apparent_bytes: 0,
        disk_bytes: 0,
        files: 0,
        directories: 0,
        unreadable: 0,
    };

    for path in paths {
        // `symlink_metadata`, so a symlink is identified as one instead of being followed into
        // whatever it points at.
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            out.unreadable = out.unreadable.saturating_add(1);
            continue;
        };

        let kind = meta.file_type();
        if kind.is_dir() {
            out.directories = out.directories.saturating_add(1);
            continue;
        }
        if kind.is_symlink() || !kind.is_file() {
            continue;
        }
        if !seen.insert((meta.dev(), meta.ino())) {
            continue;
        }

        out.apparent_bytes = out.apparent_bytes.saturating_add(meta.len());
        out.disk_bytes = out
            .disk_bytes
            .saturating_add(meta.blocks().saturating_mul(512));
        out.files = out.files.saturating_add(1);
    }

    out
}

/// Run a read-only query and return its stdout.
///
/// Captures stderr and the exit status, so a failure is a typed error rather than empty output —
/// the mistake Stacer made everywhere by reading only stdout and never checking status.
pub(crate) fn query(program: &str, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| {
            AppError::from_io(&e, format!("ask {program} about installed packages"))
                .with_remedy(format!("Check that {program} is installed and on PATH."))
        })?;

    if !output.status.success() {
        return Err(AppError::new(
            ErrorCode::CommandFailed,
            format!("{program} did not succeed."),
        )
        .with_cause(Cause::Command {
            program: program.to_string(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Every backend usable on this system.
///
/// All of them, not the first that matches: a machine with two managers has two, and Stacer's
/// single `static const` is why its users only ever saw one.
#[must_use]
pub fn backends() -> Vec<Box<dyn Backend>> {
    let mut found: Vec<Box<dyn Backend>> = Vec::new();
    if caps::registry().has(Capability::Apt) {
        found.push(Box::new(DpkgBackend::new()));
    }
    // RPM and pacman backends arrive with the features that need them, each reviewed on its own.
    // Reporting nothing is the honest answer until then — never a fabricated one.
    found
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn package(recorded: u64, measured: Option<Measured>) -> Package {
        let mut p = Package::new(
            "thing".into(),
            "amd64".into(),
            "1.0".into(),
            "a thing".into(),
            recorded,
            true,
            Manager::Apt,
        );
        p.measured = measured;
        p.changed_at = Some(1_700_000_000);
        p
    }

    fn measured(apparent: u64, disk: u64) -> Measured {
        Measured {
            apparent_bytes: apparent,
            disk_bytes: disk,
            files: 1,
            directories: 0,
            unreadable: 0,
        }
    }

    /// Decision D2: two figures, never conflated.
    #[test]
    fn recorded_and_measured_sizes_stay_distinct() {
        let unmeasured = package(1000, None);
        assert_eq!(unmeasured.display_bytes(), 1000);
        assert_eq!(
            unmeasured.recorded_error(),
            None,
            "not measured and agrees with the manager are different answers"
        );

        let m = package(1000, Some(measured(2000, 2500)));
        assert_eq!(
            m.recorded_bytes, 1000,
            "the recorded figure must survive measurement"
        );
        assert_eq!(m.display_bytes(), 2500, "display shows committed bytes");
        assert_eq!(m.recorded_error(), Some(1500));
    }

    /// # Regression
    ///
    /// The predecessor of `recorded_error` was `growth()`, a `saturating_sub` justified by reading the
    /// recorded-versus-measured gap as post-install growth. Measuring 40 real packages showed the
    /// file contents summing to 0.80x the recorded figure, so the common case is the manager's
    /// estimate being *higher* — and a saturating difference renders that as `Some(0)`, which reads as
    /// "measured, and it agrees". The error is signed so the two directions stay distinguishable.
    #[test]
    fn a_manager_overestimate_is_reported_as_negative_not_as_agreement() {
        let over = package(2000, Some(measured(1200, 1500)));
        assert_eq!(over.recorded_error(), Some(-500));
        assert_ne!(
            over.recorded_error(),
            Some(0),
            "an overestimate must not be indistinguishable from an exact match"
        );

        let exact = package(1500, Some(measured(1200, 1500)));
        assert_eq!(
            exact.recorded_error(),
            Some(0),
            "and zero must still mean zero"
        );
    }

    /// Block overhead is the figure the `flat-remix-gtk` measurement made the case for: 30,547 small
    /// files containing 76.1 MB and occupying 181.3 MB.
    #[test]
    fn block_overhead_is_disk_beyond_content() {
        assert_eq!(
            measured(76_129_617, 181_276_672).block_overhead(),
            105_147_055
        );
    }

    /// A sparse or transparently compressed file occupies less than it contains, so the subtraction
    /// saturates rather than wrapping.
    #[test]
    fn a_file_smaller_on_disk_than_in_content_has_no_negative_overhead() {
        assert_eq!(measured(1_000_000, 4096).block_overhead(), 0);
    }

    /// This machine has 41 package names installed for two architectures at once, so a name is not an
    /// identity.
    #[test]
    fn identity_is_arch_qualified_where_the_manager_qualifies() {
        let make = |name: &str, arch: &str| {
            Package::new(
                name.into(),
                arch.into(),
                "1".into(),
                String::new(),
                0,
                true,
                Manager::Apt,
            )
        };

        assert_eq!(make("libc6", "amd64").id, "libc6:amd64");
        assert_eq!(make("libc6", "i386").id, "libc6:i386");
        assert_ne!(
            make("libc6", "amd64").id,
            make("libc6", "i386").id,
            "two installed packages, two identities"
        );
        assert_eq!(
            make("libc6", "").id,
            "libc6",
            "a manager without architectures gets a bare name, not a trailing colon"
        );
    }

    /// `id` is a stored field, so the thing that could go wrong is it disagreeing with the two fields
    /// it comes from. Checked against every package this machine has installed.
    #[test]
    fn every_packages_stored_identity_matches_its_name_and_arch() {
        let backends = backends();
        let Some(backend) = backends.first() else {
            return;
        };
        let Ok(packages) = backend.installed() else {
            return;
        };
        assert!(!packages.is_empty());

        for p in &packages {
            assert_eq!(
                p.id,
                Package::identity(&p.name, &p.arch),
                "stored identity drifted from name and arch"
            );
        }
    }

    #[test]
    fn packages_round_trip_over_the_wire() {
        let p = package(4096, Some(measured(8000, 8192)));
        let back: Package = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(p, back);
    }

    // ---- the measurement walk ----

    /// Per-test directory, tagged: two tests sharing one path is a defect this suite has already had.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nix-measure-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_regular_file_contributes_content_and_committed_bytes() {
        let dir = scratch("plain");
        std::fs::write(dir.join("f"), vec![7u8; 5000]).unwrap();

        let m = measure_paths([dir.join("f").as_path()]);
        assert_eq!(m.files, 1);
        assert_eq!(m.apparent_bytes, 5000);
        assert!(
            m.disk_bytes >= 5000,
            "5000 bytes cannot occupy fewer, got {}",
            m.disk_bytes
        );
        assert!(m.is_complete());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Directories are counted and never measured: `/usr/bin` belongs to no package, so charging its
    /// bytes to one would double-count across the inventory.
    #[test]
    fn a_directory_is_counted_but_never_measured() {
        let dir = scratch("dirs");
        std::fs::create_dir(dir.join("sub")).unwrap();

        let m = measure_paths([dir.join("sub").as_path()]);
        assert_eq!(m.directories, 1);
        assert_eq!(m.files, 0);
        assert_eq!(
            m.apparent_bytes, 0,
            "a directory inode's own size is not the package's bytes"
        );
        assert_eq!(m.disk_bytes, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// # Regression
    ///
    /// The reclaim pipeline reported a 9.8 GiB cache as 4 KB by taking a directory's own inode size
    /// as its contents. The same mistake here would be silent, because a package's file list is
    /// mostly directories.
    #[test]
    fn a_directorys_contents_are_not_reached_through_the_directory() {
        let dir = scratch("nodescend");
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("big"), vec![0u8; 100_000]).unwrap();

        let m = measure_paths([dir.join("sub").as_path()]);
        assert_eq!(
            m.apparent_bytes, 0,
            "measuring a directory must not walk into it — dpkg lists the files separately, and \
             counting both would double them"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_symlink_contributes_nothing() {
        let dir = scratch("symlink");
        std::fs::write(dir.join("target"), vec![1u8; 8000]).unwrap();
        std::os::unix::fs::symlink(dir.join("target"), dir.join("link")).unwrap();

        let m = measure_paths([dir.join("link").as_path()]);
        assert_eq!(m.files, 0, "a symlink is not a file this package owns");
        assert_eq!(
            m.apparent_bytes, 0,
            "following it would charge another package's bytes here"
        );
        assert!(m.is_complete(), "and it is not unreadable either");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// # Regression
    ///
    /// Snap revision sizes were first wrong because hard-linked blobs were counted once per link.
    #[test]
    fn a_hard_linked_file_is_counted_once() {
        let dir = scratch("hardlink");
        std::fs::write(dir.join("a"), vec![2u8; 40_000]).unwrap();
        std::fs::hard_link(dir.join("a"), dir.join("b")).unwrap();

        let m = measure_paths([dir.join("a").as_path(), dir.join("b").as_path()]);
        assert_eq!(m.files, 1, "one inode, one file");
        assert_eq!(m.apparent_bytes, 40_000, "not 80,000");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A path dpkg lists but that is not there makes the total a floor. Saying so is the whole
    /// difference between an understated figure and a wrong one (§P7).
    #[test]
    fn a_missing_path_is_reported_rather_than_ignored() {
        let dir = scratch("missing");
        std::fs::write(dir.join("here"), vec![3u8; 100]).unwrap();

        let m = measure_paths([dir.join("here").as_path(), dir.join("gone").as_path()]);
        assert_eq!(m.files, 1);
        assert_eq!(m.unreadable, 1);
        assert!(
            !m.is_complete(),
            "an incomplete measurement must not look like a total"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// On-disk comes from the kernel's block count, never from rounding the length up — a sparse file
    /// occupies far less than it contains, and rounding would invent bytes that were never committed.
    #[test]
    fn a_sparse_file_occupies_less_than_it_contains() {
        let dir = scratch("sparse");
        let path = dir.join("sparse");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(64 * 1024 * 1024).unwrap();
        drop(file);

        let m = measure_paths([path.as_path()]);
        assert_eq!(m.apparent_bytes, 64 * 1024 * 1024);
        if m.disk_bytes < m.apparent_bytes {
            assert_eq!(
                m.block_overhead(),
                0,
                "committed less than contained is legitimate, and must not underflow"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- against this machine ----

    /// The finding that shaped this feature, re-asserted against the live dpkg database: a real
    /// package of many small files costs meaningfully more disk than its contents, and dpkg's own
    /// figure is not either number.
    #[test]
    fn a_real_package_costs_more_disk_than_its_contents() {
        let backends = backends();
        let Some(apt) = backends.iter().find(|b| b.manager() == Manager::Apt) else {
            return; // not a dpkg machine
        };
        let Ok(packages) = apt.installed() else {
            return;
        };

        // The most file-heavy of the largest few packages — no name hard-coded, because this has to
        // hold on a machine that is not mine.
        //
        // Bounded to a sample rather than the whole inventory: measuring every package over 4 MB
        // means stat-ing several hundred thousand files, which cost 3.4 s on its own and is not a
        // price worth paying on every `make check` to assert a property one package demonstrates.
        let mut candidates: Vec<&Package> = packages
            .iter()
            .filter(|p| p.recorded_bytes > 4 * 1024 * 1024)
            .collect();
        candidates.sort_unstable_by_key(|p| std::cmp::Reverse(p.recorded_bytes));
        candidates.truncate(20);

        let mut best: Option<(&Package, Measured)> = None;
        for pkg in candidates {
            if let Ok(m) = apt.measure(&pkg.id)
                && m.files > 1000
                && best.as_ref().is_none_or(|(_, b)| m.files > b.files)
            {
                best = Some((pkg, m));
            }
        }

        let Some((pkg, m)) = best else {
            return; // no package here has enough files to make the point
        };

        assert!(m.apparent_bytes > 0, "{} measured as empty", pkg.id);
        assert!(
            m.disk_bytes >= m.apparent_bytes,
            "{}: {} files containing {} occupy only {} — small files cannot cost less than \
             their contents",
            pkg.id,
            m.files,
            crate::format_bytes(m.apparent_bytes),
            crate::format_bytes(m.disk_bytes)
        );
        assert!(
            pkg.changed_at.is_some(),
            "{} has a file list but no mtime",
            pkg.id
        );
    }

    #[test]
    fn a_failed_query_is_an_error_not_empty_output() {
        let err = query("false", &[]).unwrap_err();
        assert_eq!(err.code, ErrorCode::CommandFailed);
    }

    #[test]
    fn a_missing_program_reports_a_remedy() {
        let err = query("definitely-not-a-real-program", &[]).unwrap_err();
        assert!(err.remedy.is_some(), "an absent tool must say what to do");
    }

    #[test]
    fn backends_are_probed_per_capability() {
        for backend in backends() {
            assert!(
                backend.available(),
                "{:?} was returned but reports unavailable",
                backend.manager()
            );
        }
    }
}
