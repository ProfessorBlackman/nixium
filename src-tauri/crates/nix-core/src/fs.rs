// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Mount enumeration and per-filesystem accounting. Task 1.2 (`STO-1`), plus the free-space slice
//! of `STO-17` pulled forward.
//!
//! # Why btrfs is handled here and not in Phase 2
//!
//! `STO-17` is a Phase 2 feature, but `STO-1` reports *wrong numbers* on btrfs without it, and
//! Fedora Workstation is a Tier 1 target that uses btrfs by default. `statvfs` on btrfs ignores
//! metadata allocation and RAID-profile duplication, so shipping the space explorer with confident
//! `statvfs` figures would violate principle P8 on a Tier 1 default filesystem.
//!
//! Where the real figure cannot be obtained, [`Filesystem::accounting`] says so rather than
//! presenting an approximation as fact.
//!
//! # Why `/proc/self/mountinfo` and not `/proc/mounts`
//!
//! `mountinfo` distinguishes bind mounts from their sources, carries the mount and parent ids so
//! the mount tree is reconstructable, and escapes spaces and tabs unambiguously as octal. Stacer
//! used Qt's `QStorageInfo`, which reported every tmpfs and squashfs snap loop as a volume — which
//! is why its disk pie chart needed two filter combo boxes to be readable at all.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::caps::{self, Capability};
use crate::error::{AppError, Cause, ErrorCode, IoContext, Result};

const MOUNTINFO: &str = "/proc/self/mountinfo";

/// Filesystem types that are not real storage. Hidden by default, because a snap-heavy install
/// otherwise shows forty loop mounts and the useful volumes are lost among them.
const PSEUDO_FS: &[&str] = &[
    "autofs",
    "binfmt_misc",
    "bpf",
    "cgroup",
    "cgroup2",
    "configfs",
    "debugfs",
    "devpts",
    "devtmpfs",
    "efivarfs",
    "fusectl",
    "hugetlbfs",
    "mqueue",
    "nsfs",
    "overlay",
    "proc",
    "pstore",
    "ramfs",
    "resctrl",
    "rpc_pipefs",
    "securityfs",
    "selinuxfs",
    "squashfs",
    "sysfs",
    "tmpfs",
    "tracefs",
];

/// How much confidence to place in a filesystem's free-space figure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Accounting {
    /// `statvfs` is authoritative for this filesystem.
    Exact,
    /// Reported by a filesystem-specific tool that accounts for metadata and duplication.
    ToolReported,
    /// `statvfs` only, on a filesystem where it is known to be misleading. The figure is shown with
    /// a caveat rather than presented as fact.
    ///
    /// `reason` is owned rather than borrowed so the type can round-trip over IPC: a `&'static str`
    /// field forces `Deserialize` to require `'de: 'static`, which does not hold.
    Approximate { reason: String },
}

/// One mounted filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Filesystem {
    /// Where it is mounted.
    pub mount_point: PathBuf,
    /// Backing device, as the kernel reports it.
    pub device: String,
    /// Filesystem type, e.g. `ext4`, `btrfs`.
    pub fs_type: String,
    /// Total bytes.
    #[ts(type = "number")]
    pub total: u64,
    /// Bytes in use.
    #[ts(type = "number")]
    pub used: u64,
    /// Bytes available **to this user** — which is less than free space when a reserve is set.
    #[ts(type = "number")]
    pub available: u64,
    /// Mounted read-only.
    pub read_only: bool,
    /// Not real storage: tmpfs, squashfs, overlay and friends.
    pub pseudo: bool,
    /// How much to trust the figures above.
    pub accounting: Accounting,
    /// Device id, so the scanner can avoid crossing filesystem boundaries.
    #[ts(type = "number")]
    pub device_id: u64,
}

impl Filesystem {
    /// Fraction of the filesystem in use, in `0.0..=1.0`. `None` when the total is zero, which is
    /// normal for some pseudo-filesystems — dividing anyway would report a nonsense percentage.
    #[must_use]
    pub fn used_fraction(&self) -> Option<f64> {
        if self.total == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        Some((self.used as f64 / self.total as f64).clamp(0.0, 1.0))
    }
}

/// One line of `/proc/self/mountinfo`, reduced to what we use.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MountEntry {
    mount_point: PathBuf,
    fs_type: String,
    device: String,
    read_only: bool,
}

/// Decode `mountinfo`'s octal escapes for space, tab, newline and backslash.
///
/// Without this, a mount point containing a space parses as two fields. Stacer's equivalent parsing
/// split on whitespace and would have mis-read such a path.
fn unescape_octal(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let bytes = field.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let digits = &field[i + 1..i + 4];
            if let Ok(code) = u8::from_str_radix(digits, 8) {
                out.push(code as char);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Parse `mountinfo` content. Pure, so the format handling is tested without a real `/proc`.
///
/// Format: `id parent major:minor root mount-point options - fstype source super-options`.
/// The `-` separator matters: the optional-fields section before it has a variable length, so
/// fields after it must be located relative to the separator rather than by absolute index.
fn parse_mountinfo(content: &str) -> Vec<MountEntry> {
    let mut entries = Vec::new();

    for line in content.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let Some(sep) = fields.iter().position(|f| *f == "-") else {
            continue;
        };
        // Before the separator we need at least: id, parent, major:minor, root, mount point,
        // options. After it: fstype and source.
        if sep < 6 || fields.len() < sep + 3 {
            continue;
        }

        let mount_point = PathBuf::from(unescape_octal(fields[4]));
        let options = fields[5];
        let fs_type = unescape_octal(fields[sep + 1]);
        let device = unescape_octal(fields[sep + 2]);

        entries.push(MountEntry {
            mount_point,
            fs_type,
            device,
            read_only: options.split(',').any(|o| o == "ro"),
        });
    }

    entries
}

/// Whether a filesystem type is not real storage, judged by name.
///
/// A name list is inherently incomplete — the kernel gains pseudo-filesystems over time, and
/// `rpc_pipefs` was missing from the first version of this list until a test caught it. So this is
/// only half the answer; [`filesystems`] also applies the structural check in [`holds_no_storage`],
/// which catches the ones no list knows about. Same lesson as the helper's allow-list: back a
/// name-based rule with a property-based one.
/// Note the `fuse.` prefix has a dot: `fuse.gvfsd-fuse` and `fuse.portal` are pseudo, but
/// **`fuseblk` is real storage** — it is how NTFS and exFAT mount. Widening this to `fuse` would
/// hide a user's Windows partition from a tool whose whole job is finding disk space.
#[must_use]
pub fn is_pseudo(fs_type: &str) -> bool {
    PSEUDO_FS.contains(&fs_type) || fs_type.starts_with("fuse.")
}

/// Whether a filesystem reports no capacity at all.
///
/// A filesystem with zero blocks is not storage, whatever it calls itself. This is the backstop for
/// pseudo-filesystems that [`is_pseudo`] has never heard of.
#[must_use]
pub const fn holds_no_storage(total: u64) -> bool {
    total == 0
}

/// Bytes reported by `statvfs`.
struct Statvfs {
    total: u64,
    used: u64,
    available: u64,
}

fn statvfs(path: &Path) -> Result<Statvfs> {
    let stat = rustix::fs::statvfs(path).map_err(|e| {
        AppError::new(
            ErrorCode::Io,
            format!("Could not measure {}.", path.display()),
        )
        .with_path(path)
        .with_cause(Cause::Os {
            errno: Some(e.raw_os_error()),
            description: e.to_string(),
        })
    })?;

    // f_frsize is the fragment size, which is what the block counts are expressed in.
    let block = if stat.f_frsize == 0 {
        stat.f_bsize
    } else {
        stat.f_frsize
    };
    let total = stat.f_blocks.saturating_mul(block);
    // Free space excluding the root reserve is what a user can actually use.
    let available = stat.f_bavail.saturating_mul(block);
    // Used is total minus *all* free blocks, so the reserve counts as used rather than vanishing.
    let free = stat.f_bfree.saturating_mul(block);
    Ok(Statvfs {
        total,
        used: total.saturating_sub(free),
        available,
    })
}

/// Device id of a path, so the scanner can refuse to cross filesystem boundaries.
fn device_id(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).map(|m| m.dev()).unwrap_or(0)
}

/// Ask btrfs for its real usage.
///
/// `statvfs` on btrfs is misleading: it ignores chunk allocation and RAID-profile duplication, so a
/// RAID1 filesystem reports roughly twice the usable space it has. `btrfs filesystem usage --raw`
/// reports the truth.
fn btrfs_usage(mount_point: &Path) -> Result<Statvfs> {
    let btrfs = caps::registry()
        .resolve(Capability::BtrfsTools)
        .ok_or_else(|| AppError::unsupported(Capability::BtrfsTools.label()))?;

    let output = std::process::Command::new(&btrfs)
        .args(["filesystem", "usage", "--raw"])
        .arg(mount_point)
        .output()
        .map_err(|e| {
            AppError::from_io(&e, "ask btrfs about this filesystem").with_path(mount_point)
        })?;

    if !output.status.success() {
        return Err(AppError::new(
            ErrorCode::CommandFailed,
            "btrfs could not report usage for this filesystem.",
        )
        .with_path(mount_point)
        .with_cause(Cause::Command {
            program: btrfs.display().to_string(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }));
    }

    parse_btrfs_usage(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
        AppError::new(
            ErrorCode::Parse,
            "btrfs reported usage in a shape nix does not recognise.",
        )
        .with_path(mount_point)
    })
}

/// Parse `btrfs filesystem usage --raw`. Pure, so the format is tested without btrfs installed.
///
/// The lines that matter:
/// ```text
///     Device size:                  500107862016
///     Used:                         213674508288
///     Free (estimated):             283108618240      (min: 141554309120)
/// ```
fn parse_btrfs_usage(output: &str) -> Option<Statvfs> {
    let value_after = |prefix: &str| -> Option<u64> {
        output
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with(prefix))?
            .split(':')
            .nth(1)?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };

    let total = value_after("Device size:")?;
    let used = value_after("Used:")?;
    // "Free (estimated)" is btrfs's own best figure and accounts for the profile.
    let available = value_after("Free (estimated):").unwrap_or(total.saturating_sub(used));

    Some(Statvfs {
        total,
        used,
        available,
    })
}

/// Enumerate mounted filesystems.
///
/// `include_pseudo` controls whether tmpfs, squashfs, overlay and friends are returned. The default
/// in the UI is `false`: they are not storage, and including them is why Stacer's disk chart was
/// unreadable without filters.
pub fn filesystems(include_pseudo: bool) -> Result<Vec<Filesystem>> {
    let content = std::fs::read_to_string(MOUNTINFO)
        .doing("read the mount table")
        .map_err(|e| e.with_path(MOUNTINFO))?;

    let mut seen_mount_points = BTreeSet::new();
    let mut out = Vec::new();

    for entry in parse_mountinfo(&content) {
        // Provisional: the authoritative answer needs statvfs, which is below.
        if is_pseudo(&entry.fs_type) && !include_pseudo {
            continue;
        }
        // The same mount point can appear more than once (bind mounts, remounts). The last entry
        // wins in the kernel, so keep the first we see going backwards — i.e. dedupe forwards and
        // let earlier entries stand, which matches what `df` shows.
        if !seen_mount_points.insert(entry.mount_point.clone()) {
            continue;
        }

        // An unreadable mount point is normal — a fuse mount whose daemon has gone, an
        // autofs entry not yet triggered. Skip it rather than failing the whole enumeration.
        let Ok(stat) = statvfs(&entry.mount_point) else {
            continue;
        };

        // The structural check: no capacity means not storage, whatever the type is called.
        let pseudo = is_pseudo(&entry.fs_type) || holds_no_storage(stat.total);
        if pseudo && !include_pseudo {
            continue;
        }

        let (stat, accounting) = if entry.fs_type == "btrfs" {
            match btrfs_usage(&entry.mount_point) {
                Ok(real) => (real, Accounting::ToolReported),
                Err(e) => {
                    tracing::debug!(
                        mount = %entry.mount_point.display(),
                        error = %e,
                        "btrfs usage unavailable, falling back to statvfs"
                    );
                    (
                        stat,
                        Accounting::Approximate {
                            reason:
                                "btrfs tools are unavailable, so this figure comes from statvfs \
                                     and ignores metadata allocation and RAID duplication. Free \
                                     space may be overstated."
                                    .to_string(),
                        },
                    )
                }
            }
        } else {
            (stat, Accounting::Exact)
        };

        out.push(Filesystem {
            device_id: device_id(&entry.mount_point),
            mount_point: entry.mount_point,
            device: entry.device,
            fs_type: entry.fs_type,
            total: stat.total,
            used: stat.used,
            available: stat.available,
            read_only: entry.read_only,
            pseudo,
            accounting,
        });
    }

    // Largest first: the filesystem a user cares about is almost always the biggest.
    out.sort_by(|a, b| {
        b.total
            .cmp(&a.total)
            .then(a.mount_point.cmp(&b.mount_point))
    });
    Ok(out)
}

/// The filesystem containing a path, if it can be determined.
pub fn containing(path: &Path) -> Result<Option<Filesystem>> {
    let target = device_id(path);
    if target == 0 {
        return Ok(None);
    }
    Ok(filesystems(true)?
        .into_iter()
        .find(|fs| fs.device_id == target))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
25 30 0:23 / /proc rw,nosuid,nodev,noexec,relatime shared:12 - proc proc rw
26 30 0:24 / /sys rw,nosuid,nodev,noexec,relatime shared:2 - sysfs sysfs rw
30 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw,errors=remount-ro
48 30 0:44 / /tmp rw,nosuid,nodev shared:9 - tmpfs tmpfs rw,size=8146284k
99 30 7:1 / /snap/core22/1122 ro,nodev,relatime shared:52 - squashfs /dev/loop1 ro
120 30 8:3 /@home /home rw,relatime shared:60 - btrfs /dev/sda3 rw,ssd,subvol=/@home
131 30 0:52 / /mnt/my\\040drive rw,relatime shared:70 - ext4 /dev/sdb1 rw
";

    #[test]
    fn parses_the_variable_length_optional_fields() {
        let entries = parse_mountinfo(SAMPLE);
        assert_eq!(entries.len(), 7);

        let root = entries
            .iter()
            .find(|e| e.mount_point == Path::new("/"))
            .unwrap();
        assert_eq!(root.fs_type, "ext4");
        assert_eq!(root.device, "/dev/sda2");
        assert!(!root.read_only);
    }

    #[test]
    fn decodes_octal_escapes_in_mount_points() {
        let entries = parse_mountinfo(SAMPLE);
        let escaped = entries
            .iter()
            .find(|e| e.mount_point.to_string_lossy().contains("my drive"))
            .expect("a mount point with a space must parse as one field");
        assert_eq!(escaped.mount_point, Path::new("/mnt/my drive"));
    }

    #[test]
    fn unescape_handles_all_four_escapes_and_leaves_other_text_alone() {
        assert_eq!(unescape_octal("plain"), "plain");
        assert_eq!(unescape_octal("a\\040b"), "a b");
        assert_eq!(unescape_octal("a\\011b"), "a\tb");
        assert_eq!(unescape_octal("a\\012b"), "a\nb");
        assert_eq!(unescape_octal("a\\134b"), "a\\b");
        // A backslash that is not a valid escape is passed through rather than swallowed.
        assert_eq!(unescape_octal("a\\zzzb"), "a\\zzzb");
    }

    #[test]
    fn detects_read_only_mounts() {
        let entries = parse_mountinfo(SAMPLE);
        let snap = entries.iter().find(|e| e.fs_type == "squashfs").unwrap();
        assert!(snap.read_only, "a snap mount is read-only");
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        let content = "garbage\n30 1 8:2 / / rw - ext4 /dev/sda2 rw\nalso garbage - \n";
        let entries = parse_mountinfo(content);
        assert_eq!(entries.len(), 1, "only the well-formed line survives");
    }

    #[test]
    fn zero_capacity_is_treated_as_not_storage() {
        // The backstop for pseudo-filesystems no name list knows about. `rpc_pipefs` was exactly
        // this case: absent from the list, present on a real machine, reporting zero blocks.
        assert!(holds_no_storage(0));
        assert!(!holds_no_storage(1));
    }

    #[test]
    fn pseudo_filesystems_are_recognised() {
        for t in [
            "tmpfs",
            "squashfs",
            "overlay",
            "proc",
            "sysfs",
            "cgroup2",
            "devtmpfs",
            "rpc_pipefs",
            "resctrl",
        ] {
            assert!(is_pseudo(t), "{t} should be pseudo");
        }
        for t in ["ext4", "btrfs", "xfs", "zfs", "vfat", "ntfs3"] {
            assert!(!is_pseudo(t), "{t} is real storage");
        }
        assert!(is_pseudo("fuse.gvfsd-fuse"), "a fuse.* mount is pseudo");
        assert!(is_pseudo("fuse.portal"), "so is the portal");
    }

    /// Regression guard. Widening the fuse rule from `fuse.` to `fuse` would hide NTFS and exFAT
    /// volumes, which mount as `fuseblk` and are real storage a user very much wants to see.
    #[test]
    fn fuseblk_is_real_storage_not_pseudo() {
        assert!(!is_pseudo("fuseblk"), "NTFS and exFAT mount as fuseblk");
        assert!(
            !is_pseudo("fuse"),
            "a bare fuse mount may be real, e.g. mergerfs or rclone"
        );
    }

    #[test]
    fn btrfs_usage_is_parsed_from_the_real_output_shape() {
        let output = "\
Overall:
    Device size:                 500107862016
    Device allocated:            227633397760
    Device unallocated:          272474464256
    Used:                        213674508288
    Free (estimated):            283108618240      (min: 141554309120)
    Data ratio:                          1.00
";
        let stat = parse_btrfs_usage(output).unwrap();
        assert_eq!(stat.total, 500_107_862_016);
        assert_eq!(stat.used, 213_674_508_288);
        assert_eq!(stat.available, 283_108_618_240);
    }

    #[test]
    fn btrfs_usage_falls_back_when_free_is_absent() {
        let output = "    Device size:  1000\n    Used:  400\n";
        let stat = parse_btrfs_usage(output).unwrap();
        assert_eq!(stat.available, 600, "free is derived when btrfs omits it");
    }

    #[test]
    fn btrfs_usage_refuses_unrecognised_output() {
        assert!(parse_btrfs_usage("something else entirely").is_none());
        assert!(parse_btrfs_usage("").is_none());
    }

    #[test]
    fn used_fraction_is_honest_about_zero_sized_filesystems() {
        let mut fs = Filesystem {
            mount_point: PathBuf::from("/"),
            device: "/dev/sda1".into(),
            fs_type: "ext4".into(),
            total: 0,
            used: 0,
            available: 0,
            read_only: false,
            pseudo: false,
            accounting: Accounting::Exact,
            device_id: 1,
        };
        assert_eq!(fs.used_fraction(), None, "must not divide by zero");

        fs.total = 200;
        fs.used = 50;
        assert!((fs.used_fraction().unwrap() - 0.25).abs() < f64::EPSILON);

        // Over-reporting is clamped rather than exceeding 100%.
        fs.used = 400;
        assert!((fs.used_fraction().unwrap() - 1.0).abs() < f64::EPSILON);
    }

    // ---- against the real system ----

    #[test]
    fn enumerates_this_machine_and_agrees_with_the_kernel() {
        let real = filesystems(false).unwrap();
        assert!(
            !real.is_empty(),
            "there is always at least a root filesystem"
        );
        assert!(
            real.iter().all(|fs| !fs.pseudo),
            "excluding pseudo filesystems must exclude them"
        );
        // Sorted largest first.
        assert!(real.windows(2).all(|w| w[0].total >= w[1].total));

        for fs in &real {
            assert!(
                fs.used <= fs.total,
                "{} reports more used than total",
                fs.mount_point.display()
            );
            assert!(fs.total > 0, "a real filesystem has a size");
        }
    }

    #[test]
    fn including_pseudo_filesystems_returns_more() {
        let without = filesystems(false).unwrap().len();
        let with = filesystems(true).unwrap().len();
        assert!(with >= without);
    }

    #[test]
    fn root_is_locatable_by_path() {
        let fs = containing(Path::new("/")).unwrap();
        assert!(fs.is_some(), "the root filesystem must be findable");
    }

    #[test]
    fn filesystem_round_trips_over_the_wire() {
        let fs = filesystems(false).unwrap().remove(0);
        let json = serde_json::to_string(&fs).unwrap();
        let back: Filesystem = serde_json::from_str(&json).unwrap();
        assert_eq!(fs, back);
    }
}
