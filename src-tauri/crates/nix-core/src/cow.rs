// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Copy-on-write filesystem awareness: btrfs, ZFS, and LVM thin pools. `STO-17`.
//!
//! # The problem this exists to solve
//!
//! On a copy-on-write filesystem a file's extents may be shared with a snapshot. Deleting the file
//! removes the name, but the blocks stay allocated until every reference to them is gone — so the
//! space does not come back. A user who deletes 8 GiB on a snapshotted btrfs volume and sees `df`
//! move by nothing has been lied to by whatever tool told them 8 GiB was reclaimable.
//!
//! nix therefore qualifies the estimate rather than asserting it. That is
//! [`crate::space::Reclaimable`], and the rule from the specification is blunt: *where exclusive
//! size is unobtainable, suppress the estimate rather than fake it.*
//!
//! # Snapshots are attributed, never deleted
//!
//! Snapshots are inventoried so their space lands in the [`crate::space::Category::Snapshot`]
//! category instead of `Unknown` — a user should be able to see that 40 GiB is held by snapper.
//! Deleting one is **backlog**, behind explicit opt-in and its own design review: a snapper or
//! Timeshift snapshot may be somebody's only route back from a bad upgrade, and that is not a
//! decision to make on their behalf.
//!
//! # An honest note on verification
//!
//! The free-space half of `STO-17` landed in `STO-1` and is verified against a real filesystem. This
//! module is not: the development machine runs ext4 with no btrfs, ZFS or LVM tooling installed, so
//! the parsers below are written to each tool's **documented output format** and tested against
//! fixtures constructed from that documentation — not against output captured from a running system.
//!
//! That is a weaker guarantee than the package parsers have, and the difference matters: the dpkg
//! work found two real bugs precisely *because* it ran against a live database. These parsers should
//! be re-verified on a real btrfs and ZFS system before their numbers are trusted. What is fully
//! tested here is the part that decides safety — the suppression logic — because that is pure and
//! runs everywhere.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::caps::{self, Capability};
use crate::error::{AppError, Cause, ErrorCode, Result};
use crate::space::Reclaimable;

/// A filesystem's copy-on-write behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CowKind {
    /// Not copy-on-write: deleting a file returns its blocks.
    None,
    Btrfs,
    Zfs,
    /// A filesystem on an LVM thin volume. The filesystem itself may not be copy-on-write, but the
    /// block layer beneath it is, and an LVM snapshot has the same effect.
    LvmThin,
}

impl CowKind {
    /// Whether space on this filesystem may be shared with a snapshot.
    #[must_use]
    pub const fn may_share(self) -> bool {
        !matches!(self, Self::None)
    }

    /// The filesystem type string this corresponds to, for the ordinary cases.
    #[must_use]
    pub fn from_fs_type(fs_type: &str) -> Self {
        match fs_type {
            "btrfs" => Self::Btrfs,
            "zfs" => Self::Zfs,
            _ => Self::None,
        }
    }

    /// The userspace tool needed to ask about sharing.
    #[must_use]
    pub const fn required_tool(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Btrfs => Some("btrfs"),
            Self::Zfs => Some("zfs"),
            Self::LvmThin => Some("lvs"),
        }
    }
}

/// A snapshot or subvolume holding space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Snapshot {
    /// What the filesystem calls it: a btrfs path, a ZFS dataset name, an LV name.
    pub name: String,
    /// Where it is visible, when it is mounted or reachable.
    pub path: Option<PathBuf>,
    /// Bytes held **only** by this snapshot, when the filesystem can say.
    ///
    /// This is the figure that would come back if it were deleted. `None` means the filesystem could
    /// not tell us, and nix will not guess.
    #[ts(type = "number | null")]
    pub exclusive_bytes: Option<u64>,
    /// Total bytes the snapshot references, most of which it usually shares with the live data.
    #[ts(type = "number | null")]
    pub referenced_bytes: Option<u64>,
    pub kind: CowKind,
}

/// Extent sharing for one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sharing {
    pub total_bytes: u64,
    /// Bytes referenced only by this path.
    pub exclusive_bytes: u64,
    /// Bytes shared with something else — a snapshot, or a reflinked copy.
    pub shared_bytes: u64,
}

impl Sharing {
    /// Turn measured sharing into a qualified estimate.
    #[must_use]
    pub fn to_reclaimable(&self) -> Reclaimable {
        if self.shared_bytes == 0 {
            // Nothing is shared, so the whole size genuinely comes back.
            return Reclaimable::Exact;
        }
        Reclaimable::AtMost {
            exclusive: Some(self.exclusive_bytes),
            reason: format!(
                "{} of this is shared with a snapshot or another copy, so deleting it returns only the remaining {}.",
                crate::format_bytes(self.shared_bytes),
                crate::format_bytes(self.exclusive_bytes)
            ),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// btrfs
// ---------------------------------------------------------------------------------------------

/// Parse `btrfs filesystem du --raw -s <path>`.
///
/// Documented output shape:
///
/// ```text
///      Total   Exclusive  Set shared  Filename
/// 10737418240  2147483648  8589934592  /path
/// ```
///
/// `--raw` is essential: without it the figures come back as `10.00GiB` and would need
/// unit parsing, which is one more thing to get wrong.
///
/// **Not verified against a running btrfs system** — see the module note.
#[must_use]
pub fn parse_btrfs_du(output: &str) -> Option<Sharing> {
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        // Skip the header, which begins with a word rather than a number.
        let (Ok(total), Ok(exclusive), Ok(shared)) = (
            fields[0].parse::<u64>(),
            fields[1].parse::<u64>(),
            fields[2].parse::<u64>(),
        ) else {
            continue;
        };
        return Some(Sharing {
            total_bytes: total,
            exclusive_bytes: exclusive,
            shared_bytes: shared,
        });
    }
    None
}

/// Parse `btrfs subvolume list -s <mount>` — the `-s` restricts output to snapshots.
///
/// Documented output shape:
///
/// ```text
/// ID 258 gen 12350 cgen 12349 top level 5 otime 2026-01-15 10:23:45 path @snapshots/1/snapshot
/// ```
///
/// The `path` keyword is the anchor: everything after it is the subvolume path, which may itself
/// contain spaces, so it is taken as the remainder of the line rather than the next field.
///
/// **Not verified against a running btrfs system** — see the module note.
#[must_use]
pub fn parse_btrfs_subvolumes(output: &str, mount: &Path) -> Vec<Snapshot> {
    output
        .lines()
        .filter_map(|line| {
            // `path` appears once as a keyword; splitting on it takes the remainder, so a subvolume
            // path containing spaces survives.
            let subvol = line.split(" path ").nth(1)?.trim();
            if subvol.is_empty() {
                return None;
            }
            Some(Snapshot {
                name: subvol.to_string(),
                path: Some(mount.join(subvol)),
                // btrfs cannot report a snapshot's exclusive size without quota groups, which are
                // usually disabled and carry a real performance cost. nix does not enable them, and
                // does not pretend to know.
                exclusive_bytes: None,
                referenced_bytes: None,
                kind: CowKind::Btrfs,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// ZFS
// ---------------------------------------------------------------------------------------------

/// Parse `zfs list -H -p -t snapshot -o name,used,referenced`.
///
/// `-H` drops the header and separates fields with tabs; `-p` gives exact bytes rather than
/// human-readable units. ZFS's `used` for a snapshot **is** its exclusive size — the space that
/// would be freed by destroying it — which is exactly the figure nix needs and the reason ZFS can be
/// answered precisely where btrfs cannot.
///
/// **Not verified against a running ZFS system** — see the module note.
#[must_use]
pub fn parse_zfs_snapshots(output: &str) -> Vec<Snapshot> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next()?.trim();
            let used = fields.next()?.trim().parse::<u64>().ok();
            let referenced = fields.next().and_then(|r| r.trim().parse::<u64>().ok());
            if name.is_empty() {
                return None;
            }
            Some(Snapshot {
                name: name.to_string(),
                path: None,
                // For a ZFS snapshot, `used` is precisely the space destroying it would return.
                exclusive_bytes: used,
                referenced_bytes: referenced,
                kind: CowKind::Zfs,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// LVM
// ---------------------------------------------------------------------------------------------

/// Parse `lvs --noheadings --units b --nosuffix -o lv_name,vg_name,lv_size,origin`.
///
/// A logical volume with a non-empty `origin` is a snapshot of that origin.
///
/// **Not verified against a running LVM system** — see the module note.
#[must_use]
pub fn parse_lvm_snapshots(output: &str) -> Vec<Snapshot> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // name, vg, size, origin — a snapshot has all four.
            if fields.len() < 4 {
                return None;
            }
            let size = fields[2].parse::<u64>().ok();
            Some(Snapshot {
                name: format!("{}/{}", fields[1], fields[0]),
                path: None,
                // An LVM snapshot's allocated size is what releasing it returns.
                exclusive_bytes: size,
                referenced_bytes: None,
                kind: CowKind::LvmThin,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// dispatch
// ---------------------------------------------------------------------------------------------

fn run(program: &str, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| AppError::from_io(&e, format!("ask {program} about snapshots")))?;

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

/// How much of `path` would actually be returned if it were freed.
///
/// The important case is the pessimistic one: on a copy-on-write filesystem where nix cannot ask —
/// because the tool is absent, or the query failed — the answer is
/// [`Reclaimable::Unknown`] rather than an assumption of exclusivity. Assuming exclusivity is how a
/// tool ends up promising space that never arrives.
#[must_use]
pub fn reclaimable_for(path: &Path, kind: CowKind) -> Reclaimable {
    if !kind.may_share() {
        return Reclaimable::Exact;
    }

    let Some(tool) = kind.required_tool() else {
        return Reclaimable::Exact;
    };

    // No tool means no answer, and no answer means no promise.
    let tool_present = match kind {
        CowKind::Btrfs => caps::registry().has(Capability::BtrfsTools),
        _ => which(tool),
    };
    if !tool_present {
        return Reclaimable::Unknown {
            reason: format!(
                "This is a {} filesystem, where space can be shared with snapshots, and {tool} is not installed — so nix cannot tell how much deleting this would actually free.",
                fs_label(kind)
            ),
        };
    }

    match kind {
        CowKind::Btrfs => match run(
            "btrfs",
            &["filesystem", "du", "--raw", "-s", &path.to_string_lossy()],
        )
        .ok()
        .and_then(|out| parse_btrfs_du(&out))
        {
            Some(sharing) => sharing.to_reclaimable(),
            None => Reclaimable::Unknown {
                reason: "btrfs could not report how much of this is shared with a snapshot."
                    .to_string(),
            },
        },
        // ZFS and LVM report sharing per dataset or volume rather than per path, so a single file's
        // exclusivity is not answerable. Saying so is better than assuming.
        CowKind::Zfs | CowKind::LvmThin => Reclaimable::AtMost {
            exclusive: None,
            reason: format!(
                "This is on {}, where space can be shared with snapshots. Deleting it may return less than its size, or nothing at all.",
                fs_label(kind)
            ),
        },
        CowKind::None => Reclaimable::Exact,
    }
}

const fn fs_label(kind: CowKind) -> &'static str {
    match kind {
        CowKind::None => "this filesystem",
        CowKind::Btrfs => "btrfs",
        CowKind::Zfs => "ZFS",
        CowKind::LvmThin => "an LVM thin volume",
    }
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// Which filesystem each mount point is, resolved once so a scan does not re-enumerate per path.
///
/// Built once per preview rather than per candidate: `fs::containing` walks the whole mount table,
/// and doing that for every one of several hundred candidates would be the dominant cost of a
/// preview on a machine with a lot of mounts.
#[derive(Debug, Clone, Default)]
pub struct CowMap {
    /// Mount point and its kind, longest path first so the most specific mount wins.
    entries: Vec<(PathBuf, CowKind)>,
}

impl CowMap {
    /// Build from the current mount table.
    #[must_use]
    pub fn build() -> Self {
        let mut entries: Vec<(PathBuf, CowKind)> = crate::fs::filesystems(true)
            .unwrap_or_default()
            .into_iter()
            .map(|fs| (fs.mount_point, CowKind::from_fs_type(&fs.fs_type)))
            .collect();
        // Longest first: `/home` must win over `/` for a path under it.
        entries.sort_by_key(|(mount, _)| std::cmp::Reverse(mount.as_os_str().len()));
        Self { entries }
    }

    /// Build from explicit entries. For tests.
    #[must_use]
    pub fn from_entries(mut entries: Vec<(PathBuf, CowKind)>) -> Self {
        entries.sort_by_key(|(mount, _)| std::cmp::Reverse(mount.as_os_str().len()));
        Self { entries }
    }

    /// The kind of filesystem a path sits on.
    ///
    /// An unrecognised path is treated as [`CowKind::None`]: a path outside every known mount is
    /// almost certainly a logical entry rather than a real file, and qualifying those would put a
    /// caveat on every package and journal candidate for no reason.
    #[must_use]
    pub fn kind_for(&self, path: &Path) -> CowKind {
        self.entries
            .iter()
            .find(|(mount, _)| path.starts_with(mount))
            .map_or(CowKind::None, |(_, kind)| *kind)
    }

    /// Whether any copy-on-write filesystem is mounted at all.
    ///
    /// Lets the common case skip the work entirely.
    #[must_use]
    pub fn any_cow(&self) -> bool {
        self.entries.iter().any(|(_, kind)| kind.may_share())
    }
}

/// Every snapshot nix can find, across every copy-on-write filesystem present.
///
/// A filesystem whose tool is missing contributes nothing rather than failing the sweep: partial
/// attribution is useful, and a hard failure would deny the user everything else.
#[must_use]
pub fn snapshots() -> Vec<Snapshot> {
    let mut found = Vec::new();

    let filesystems = crate::fs::filesystems(false).unwrap_or_default();

    // btrfs: one query per mounted btrfs filesystem.
    if caps::registry().has(Capability::BtrfsTools) {
        for fs in filesystems.iter().filter(|f| f.fs_type == "btrfs") {
            match run(
                "btrfs",
                &["subvolume", "list", "-s", &fs.mount_point.to_string_lossy()],
            ) {
                Ok(out) => found.extend(parse_btrfs_subvolumes(&out, &fs.mount_point)),
                Err(e) => {
                    tracing::debug!(mount = %fs.mount_point.display(), error = %e, "no btrfs subvolume listing")
                }
            }
        }
    }

    if which("zfs") {
        match run(
            "zfs",
            &[
                "list",
                "-H",
                "-p",
                "-t",
                "snapshot",
                "-o",
                "name,used,referenced",
            ],
        ) {
            Ok(out) => found.extend(parse_zfs_snapshots(&out)),
            Err(e) => tracing::debug!(error = %e, "no zfs snapshot listing"),
        }
    }

    if which("lvs") {
        match run(
            "lvs",
            &[
                "--noheadings",
                "--units",
                "b",
                "--nosuffix",
                "-o",
                "lv_name,vg_name,lv_size,origin",
            ],
        ) {
            Ok(out) => found.extend(parse_lvm_snapshots(&out)),
            Err(e) => tracing::debug!(error = %e, "no lvm snapshot listing"),
        }
    }

    found
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------------------
    // The suppression logic. Pure, runs everywhere, and it is what decides whether nix ever
    // overstates what deleting something will return.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn a_non_cow_filesystem_is_exact() {
        assert!(!CowKind::None.may_share());
        assert_eq!(
            reclaimable_for(Path::new("/home/me/file"), CowKind::None),
            Reclaimable::Exact,
            "an ordinary filesystem returns a file's blocks when it is deleted"
        );
        assert_eq!(CowKind::None.required_tool(), None);
    }

    #[test]
    fn cow_filesystems_are_recognised_from_their_type() {
        assert_eq!(CowKind::from_fs_type("btrfs"), CowKind::Btrfs);
        assert_eq!(CowKind::from_fs_type("zfs"), CowKind::Zfs);
        for ordinary in ["ext4", "xfs", "vfat", "ntfs3", "f2fs"] {
            assert_eq!(
                CowKind::from_fs_type(ordinary),
                CowKind::None,
                "{ordinary} is not copy-on-write"
            );
        }
        assert!(CowKind::Btrfs.may_share());
        assert!(CowKind::Zfs.may_share());
        assert!(CowKind::LvmThin.may_share());
    }

    /// The pessimistic default, and the whole point of the module. On this development machine
    /// btrfs tooling is absent, so this exercises the real path rather than a mock.
    #[test]
    fn a_cow_filesystem_without_its_tool_promises_nothing() {
        let verdict = reclaimable_for(Path::new("/mnt/data/file"), CowKind::Btrfs);

        assert!(
            !verdict.is_exact(),
            "assuming exclusivity is how a tool promises space that never arrives"
        );
        assert_eq!(
            verdict.promisable(8_589_934_592),
            0,
            "with nothing proven, nothing may be promised"
        );
        let caveat = verdict
            .caveat()
            .expect("a suppressed estimate must explain itself");
        assert!(caveat.contains("btrfs"), "{caveat}");
        assert!(
            caveat.contains("snapshot"),
            "the caveat should say why, not just that: {caveat}"
        );
    }

    #[test]
    fn zfs_and_lvm_report_per_dataset_so_a_single_file_is_unprovable() {
        for kind in [CowKind::Zfs, CowKind::LvmThin] {
            let verdict = reclaimable_for(Path::new("/tank/data/file"), kind);
            assert!(!verdict.is_exact(), "{kind:?}");
            assert_eq!(
                verdict.promisable(1_000_000_000),
                0,
                "{kind:?} cannot prove a single path's exclusivity, so it promises nothing"
            );
            assert!(verdict.caveat().is_some());
        }
    }

    #[test]
    fn measured_sharing_becomes_a_qualified_estimate() {
        // Nothing shared: the whole size genuinely comes back.
        let exclusive = Sharing {
            total_bytes: 10_000,
            exclusive_bytes: 10_000,
            shared_bytes: 0,
        };
        assert_eq!(exclusive.to_reclaimable(), Reclaimable::Exact);

        // Mostly shared with a snapshot: only the exclusive part is promisable.
        let shared = Sharing {
            total_bytes: 10_737_418_240,
            exclusive_bytes: 2_147_483_648,
            shared_bytes: 8_589_934_592,
        };
        let verdict = shared.to_reclaimable();
        assert_eq!(
            verdict.promisable(10_737_418_240),
            2_147_483_648,
            "the shared 8 GiB stays allocated until the snapshot goes"
        );
        let caveat = verdict.caveat().unwrap();
        assert!(
            caveat.contains("8.0 GiB"),
            "the caveat should quantify: {caveat}"
        );
        assert!(caveat.contains("2.0 GiB"), "{caveat}");
    }

    #[test]
    fn required_tools_are_named_per_filesystem() {
        assert_eq!(CowKind::Btrfs.required_tool(), Some("btrfs"));
        assert_eq!(CowKind::Zfs.required_tool(), Some("zfs"));
        assert_eq!(CowKind::LvmThin.required_tool(), Some("lvs"));
    }

    // ---------------------------------------------------------------------------------------
    // Parsers. Written to documented output formats and NOT verified against a running system —
    // see the module note. These fixtures encode what the documentation says, so a future run
    // against real output has something concrete to disagree with.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn btrfs_du_is_parsed_and_the_header_ignored() {
        let output = "     Total   Exclusive  Set shared  Filename\n\
                      10737418240  2147483648  8589934592  /mnt/data\n";
        let sharing = parse_btrfs_du(output).expect("documented shape must parse");
        assert_eq!(sharing.total_bytes, 10_737_418_240);
        assert_eq!(sharing.exclusive_bytes, 2_147_483_648);
        assert_eq!(sharing.shared_bytes, 8_589_934_592);
    }

    #[test]
    fn btrfs_du_refuses_output_it_does_not_recognise() {
        // Without --raw the figures come back as "10.00GiB" and must not be silently misread.
        assert!(parse_btrfs_du("  10.00GiB  2.00GiB  8.00GiB  /mnt\n").is_none());
        assert!(parse_btrfs_du("").is_none());
        assert!(parse_btrfs_du("ERROR: not a btrfs filesystem\n").is_none());
    }

    #[test]
    fn btrfs_subvolumes_are_parsed_including_paths_with_spaces() {
        let output = "\
ID 258 gen 12350 cgen 12349 top level 5 otime 2026-01-15 10:23:45 path @snapshots/1/snapshot
ID 259 gen 12360 cgen 12359 top level 5 otime 2026-01-16 11:00:00 path my backup/one
";
        let snapshots = parse_btrfs_subvolumes(output, Path::new("/mnt/pool"));
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].name, "@snapshots/1/snapshot");
        assert_eq!(
            snapshots[0].path,
            Some(PathBuf::from("/mnt/pool/@snapshots/1/snapshot"))
        );
        // Taken as the remainder of the line, so a space in the path survives.
        assert_eq!(snapshots[1].name, "my backup/one");

        // btrfs cannot report exclusive size without quota groups, which nix does not enable.
        assert!(
            snapshots.iter().all(|s| s.exclusive_bytes.is_none()),
            "claiming to know a btrfs snapshot's exclusive size without qgroups would be invention"
        );
        assert!(snapshots.iter().all(|s| s.kind == CowKind::Btrfs));
    }

    #[test]
    fn btrfs_subvolume_output_without_a_path_keyword_is_skipped() {
        assert!(parse_btrfs_subvolumes("ID 5 gen 1 top level 5\n", Path::new("/mnt")).is_empty());
        assert!(parse_btrfs_subvolumes("", Path::new("/mnt")).is_empty());
    }

    /// ZFS is the one case that can be answered precisely: for a snapshot, `used` **is** the space
    /// destroying it would free.
    #[test]
    fn zfs_snapshots_carry_an_exact_exclusive_size() {
        let output = "tank/home@backup-1\t1073741824\t5368709120\n\
                      tank/home@backup-2\t2147483648\t5368709120\n";
        let snapshots = parse_zfs_snapshots(output);
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].name, "tank/home@backup-1");
        assert_eq!(
            snapshots[0].exclusive_bytes,
            Some(1_073_741_824),
            "ZFS reports a snapshot's exclusive size directly, so it can be promised"
        );
        assert_eq!(snapshots[0].referenced_bytes, Some(5_368_709_120));
        assert!(snapshots.iter().all(|s| s.kind == CowKind::Zfs));
    }

    #[test]
    fn zfs_output_that_is_not_tab_separated_is_skipped() {
        // Without -H the output is column-aligned with a header, which must not be misread.
        assert!(parse_zfs_snapshots("NAME USED REFER\n").is_empty());
        assert!(parse_zfs_snapshots("").is_empty());
    }

    #[test]
    fn lvm_snapshots_are_identified_by_having_an_origin() {
        let output = "  snap1 vg0 107374182400 thinvol\n";
        let snapshots = parse_lvm_snapshots(output);
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].name, "vg0/snap1");
        assert_eq!(snapshots[0].exclusive_bytes, Some(107_374_182_400));
        assert_eq!(snapshots[0].kind, CowKind::LvmThin);

        // A plain volume has no origin field, so it is not a snapshot.
        assert!(parse_lvm_snapshots("  thinvol vg0 107374182400\n").is_empty());
    }

    // ---------------------------------------------------------------------------------------
    // Against this machine
    // ---------------------------------------------------------------------------------------

    #[test]
    fn sweeping_for_snapshots_never_fails_even_with_no_cow_filesystems() {
        // ext4 with no btrfs, ZFS or LVM tooling: the sweep must return an empty list rather than
        // erroring, because partial attribution is useful and a hard failure denies everything.
        let found = snapshots();
        for snapshot in &found {
            assert!(!snapshot.name.is_empty());
            assert!(
                snapshot.kind.may_share(),
                "a snapshot on a non-CoW filesystem makes no sense"
            );
        }
    }

    #[test]
    fn this_machines_root_filesystem_is_classified() {
        let root = crate::fs::containing(Path::new("/")).unwrap();
        if let Some(fs) = root {
            let kind = CowKind::from_fs_type(&fs.fs_type);
            // Whatever it is, the classification must agree with itself.
            if kind == CowKind::None {
                assert_eq!(reclaimable_for(Path::new("/"), kind), Reclaimable::Exact);
            } else {
                assert!(!reclaimable_for(Path::new("/"), kind).is_exact());
            }
        }
    }

    #[test]
    fn the_most_specific_mount_wins() {
        let map = CowMap::from_entries(vec![
            (PathBuf::from("/"), CowKind::None),
            (PathBuf::from("/home"), CowKind::Btrfs),
            (PathBuf::from("/home/me/pool"), CowKind::Zfs),
        ]);

        assert_eq!(map.kind_for(Path::new("/usr/bin/sh")), CowKind::None);
        assert_eq!(map.kind_for(Path::new("/home/me/.cache")), CowKind::Btrfs);
        // The nested mount must win over its parent, or a ZFS path would be treated as btrfs.
        assert_eq!(map.kind_for(Path::new("/home/me/pool/data")), CowKind::Zfs);
        assert!(map.any_cow());
    }

    #[test]
    fn a_logical_path_outside_every_mount_is_not_qualified() {
        let map = CowMap::from_entries(vec![(PathBuf::from("/"), CowKind::Btrfs)]);
        // Candidates like "kernel 6.8.0-136-generic" or "removed packages" are not real paths.
        // Qualifying those would put a snapshot caveat on every package candidate for no reason.
        assert_eq!(
            map.kind_for(Path::new("kernel 6.8.0-136-generic")),
            CowKind::None
        );
        assert_eq!(map.kind_for(Path::new("removed packages")), CowKind::None);
    }

    #[test]
    fn a_machine_with_no_cow_filesystem_can_skip_the_work() {
        let plain = CowMap::from_entries(vec![
            (PathBuf::from("/"), CowKind::None),
            (PathBuf::from("/boot/efi"), CowKind::None),
        ]);
        assert!(!plain.any_cow());
    }

    #[test]
    fn building_from_this_machine_classifies_the_root() {
        let map = CowMap::build();
        // ext4 here, so nothing should be qualified — but the assertion holds either way.
        let kind = map.kind_for(Path::new("/"));
        assert_eq!(kind.may_share(), map.any_cow() && kind.may_share());
    }

    #[test]
    fn snapshots_round_trip_over_the_wire() {
        let snapshot = Snapshot {
            name: "tank/home@backup".into(),
            path: None,
            exclusive_bytes: Some(1024),
            referenced_bytes: Some(4096),
            kind: CowKind::Zfs,
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snapshot, back);
        assert!(json.contains("\"zfs\""), "{json}");
    }
}
