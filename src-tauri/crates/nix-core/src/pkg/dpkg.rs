// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The dpkg/APT backend, and kernel identification.
//!
//! # The rule that matters
//!
//! Old kernels are routinely the single largest reclaim available on a Debian or Ubuntu machine —
//! several hundred megabytes each, and a system that has been upgraded for a year may hold half a
//! dozen. Stacer offered none of them.
//!
//! Offering them safely rests on one rule, enforced here and again inside the privileged helper:
//!
//! > **Never the running kernel, and never the newest installed kernel.**
//!
//! The running kernel is obvious. The newest matters just as much: if the machine has been upgraded
//! but not yet rebooted, the newest installed kernel is the one it will boot into next, and removing
//! it leaves a system that boots to nothing. So the offer is restricted to versions strictly older
//! than *both*.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::space::Manager;

use super::{Backend, Measured, Package, RemovalPreview, ResidualConfig, measure_paths, query};

/// Prefixes of the packages that make up a kernel, longest first so `linux-modules-extra-` is
/// matched before `linux-modules-`.
const KERNEL_PREFIXES: &[&str] = &[
    "linux-modules-extra-",
    "linux-headers-",
    "linux-modules-",
    "linux-image-unsigned-",
    "linux-image-",
    "linux-tools-",
    "linux-cloud-tools-",
    "linux-buildinfo-",
];

/// A kernel's version-and-flavour, e.g. `6.8.0-138-generic`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct KernelVersion(pub String);

impl KernelVersion {
    /// Extract the version from a kernel package name, if it is one.
    ///
    /// `linux-image-6.8.0-138-generic` yields `6.8.0-138-generic`. Metapackages such as
    /// `linux-headers-generic` yield nothing: what follows the prefix must begin with a digit, or it
    /// is a name rather than a version. Getting that wrong would treat the metapackage that *pulls
    /// in* the current kernel as a removable old one.
    #[must_use]
    pub fn from_package(name: &str) -> Option<Self> {
        for prefix in KERNEL_PREFIXES {
            if let Some(rest) = name.strip_prefix(prefix) {
                if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    return Some(Self(rest.to_string()));
                }
                return None;
            }
        }
        None
    }

    /// The numeric core of the version, with any flavour suffix removed.
    ///
    /// `6.8.0-138-generic` and `6.8.0-138` are the same kernel: Ubuntu ships an
    /// architecture-independent headers package named for the bare version alongside the
    /// flavour-specific ones. Grouping by the raw string treated them as two different kernels, so a
    /// user would have been shown one kernel twice and offered a partial removal of each.
    #[must_use]
    pub fn base(&self) -> String {
        let mut segments = Vec::new();
        for segment in self.0.split('-') {
            // A version segment is digits and dots. The first segment that is not — `generic`,
            // `lowlatency`, `oem`, `aws` — begins the flavour.
            if segment.chars().all(|c| c.is_ascii_digit() || c == '.') && !segment.is_empty() {
                segments.push(segment);
            } else {
                break;
            }
        }
        segments.join("-")
    }

    /// The flavour, if the version names one.
    #[must_use]
    pub fn flavour(&self) -> Option<String> {
        let base = self.base();
        self.0
            .strip_prefix(&base)
            .and_then(|rest| rest.strip_prefix('-'))
            .filter(|f| !f.is_empty())
            .map(str::to_string)
    }

    /// Compare two kernel versions by their numeric segments.
    ///
    /// Segment-wise and numeric, because lexical comparison puts `6.8.0-9` after `6.8.0-10`, which
    /// on this question means offering to delete a kernel newer than the running one.
    #[must_use]
    pub fn compare(&self, other: &Self) -> std::cmp::Ordering {
        let segments = |s: &str| -> Vec<u64> {
            s.split(|c: char| !c.is_ascii_digit())
                .filter(|p| !p.is_empty())
                .filter_map(|p| p.parse().ok())
                .collect()
        };
        segments(&self.0).cmp(&segments(&other.0))
    }
}

/// The kernel this machine is running.
#[must_use]
pub fn running_kernel() -> Option<KernelVersion> {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| KernelVersion(s.trim().to_string()))
}

/// A kernel version and every package belonging to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelSet {
    pub version: KernelVersion,
    pub packages: Vec<Package>,
}

impl KernelSet {
    /// Total recorded bytes across the set.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.packages.iter().map(|p| p.recorded_bytes).sum()
    }
}

/// Group installed packages into kernel sets.
///
/// Grouped by the version's **numeric core**, so the flavour-specific and architecture-independent
/// packages of one kernel land together rather than appearing as two kernels.
#[must_use]
pub fn kernel_sets(packages: &[Package]) -> Vec<KernelSet> {
    let mut grouped: BTreeMap<String, (KernelVersion, Vec<Package>)> = BTreeMap::new();
    for package in packages {
        let Some(version) = KernelVersion::from_package(&package.name) else {
            continue;
        };
        let entry = grouped
            .entry(version.base())
            .or_insert_with(|| (version.clone(), Vec::new()));
        // Prefer the fuller, flavoured spelling for display: it is what `uname -r` prints and what
        // a user will recognise.
        if version.0.len() > entry.0.0.len() {
            entry.0 = version.clone();
        }
        entry.1.push(package.clone());
    }
    grouped
        .into_values()
        .map(|(version, packages)| KernelSet { version, packages })
        .collect()
}

/// Kernel sets that are safe to remove: strictly older than both the running kernel and the newest
/// installed one.
///
/// Returns them oldest first, so the largest wins come first in a list a user reads top-down.
#[must_use]
pub fn removable_kernels(packages: &[Package], running: Option<&KernelVersion>) -> Vec<KernelSet> {
    let mut sets = kernel_sets(packages);
    if sets.is_empty() {
        return Vec::new();
    }

    // The newest installed kernel is what the machine boots next if it has been upgraded but not
    // rebooted. Removing it leaves a system that boots to nothing.
    let Some(newest) = sets
        .iter()
        .max_by(|a, b| a.version.compare(&b.version))
        .map(|s| s.version.clone())
    else {
        return Vec::new();
    };

    sets.retain(|set| {
        let is_newest = set.version.base() == newest.base();
        // Matched on the numeric core, so a difference in how the flavour is spelled cannot let the
        // running kernel through.
        let is_running = running.is_some_and(|r| set.version.base() == r.base());
        !is_newest && !is_running
    });

    sets.sort_by(|a, b| a.version.compare(&b.version));
    sets
}

/// The query behind [`parse_installed`], kept next to the parser that depends on its field order.
///
/// `${binary:Summary}` is **last** on purpose. The parser splits on tabs positionally, and a summary
/// is the one field whose content comes from a package maintainer rather than from dpkg; last means a
/// stray tab in a description can only corrupt the description. Every one of the 2,658 rows on this
/// machine has exactly seven fields, but that is a fact about this machine, not a guarantee.
pub const INSTALLED_QUERY: &str = concat!(
    "-f=${Package}\t${Architecture}\t${Version}\t${Installed-Size}\t",
    "${db:Status-Status}\t${db:Status-Want}\t${binary:Summary}\n"
);

/// Parse the output of [`INSTALLED_QUERY`].
///
/// `Installed-Size` is in **kibibytes**, which is the field's documented unit and an easy thousand-
/// fold error to make. It is also a build-time estimate rather than a measurement — see the module
/// documentation of [`crate::pkg`].
#[must_use]
pub fn parse_installed(output: &str) -> Vec<Package> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next()?.trim();
            let arch = fields.next()?.trim();
            let version = fields.next()?.trim();
            let size = fields.next()?.trim();
            let status = fields.next()?.trim();
            let want = fields.next().unwrap_or("").trim();
            // Everything left is the summary, rejoined: a maintainer's tab cannot shift a field it
            // comes after.
            let summary = fields.collect::<Vec<_>>().join(" ");

            // Only genuinely installed packages. `config-files` means removed-but-configured, which
            // is residual configuration and reported separately.
            if status != "installed" || name.is_empty() {
                return None;
            }

            Some(Package::new(
                name.to_string(),
                arch.to_string(),
                version.to_string(),
                summary.trim().to_string(),
                size.parse::<u64>().unwrap_or(0).saturating_mul(1024),
                // `deinstall` means marked for removal; anything else that is installed and wanted
                // counts as explicit for display purposes.
                want == "install",
                Manager::Apt,
            ))
        })
        .collect()
}

/// Where dpkg keeps its per-package metadata, including the file lists.
const DPKG_INFO: &str = "/var/lib/dpkg/info";

/// The `.list` file holding a package's file list, if one exists.
///
/// Multi-arch-`same` packages get an arch-qualified name (`libc6:amd64.list`), everything else gets a
/// plain one (`bash.list`) — and which applies is not derivable from anything in the inventory query,
/// so both are tried. All 2,417 installed packages on this machine resolve.
fn list_file(info_dir: &Path, id: &str, name: &str) -> Option<PathBuf> {
    let qualified = info_dir.join(format!("{id}.list"));
    if qualified.is_file() {
        return Some(qualified);
    }
    let plain = info_dir.join(format!("{name}.list"));
    plain.is_file().then_some(plain)
}

/// Fill in [`Package::changed_at`] from each package's file-list mtime.
///
/// dpkg records no install date. The `.list` file is rewritten whenever the package's contents are
/// unpacked, so its mtime is **the last install or upgrade** — which is a real, useful fact, and not
/// the one the word "installed" would promise. One `stat` per package: 2,417 of them cost under
/// 10 ms, so this is not worth making lazy.
pub fn fill_changed_at(info_dir: &Path, packages: &mut [Package]) {
    for pkg in packages {
        pkg.changed_at = list_file(info_dir, &pkg.id, &pkg.name)
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_secs());
    }
}

/// Parse a `.list` file, or `dpkg-query -L` output: one absolute path per line.
///
/// dpkg writes `/.` for the root directory, which is not a path worth measuring.
#[must_use]
pub fn parse_file_list(output: &str) -> Vec<PathBuf> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('/') && *line != "/.")
        .map(PathBuf::from)
        .collect()
}

/// Names of packages in the `rc` state: removed, but their configuration remains.
///
/// Only the names. **Deliberately not `Installed-Size`**, which for a removed package is what it
/// occupied *when installed*, not what is left. dpkg reports `zoom` at 640 MiB in this state, and
/// `bridge-utils` at 105 KiB when a single 124-byte file is all that remains. Summing that field
/// overstated the category by three orders of magnitude — a spectacular overpromise, and a
/// spectacular failure of the specification's 2% accuracy criterion.
///
/// What actually remains is the package's conffiles, so those are what get measured.
#[must_use]
pub fn parse_residual_names(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next()?.trim();
            let abbrev = fields.next()?.trim();
            (abbrev.starts_with("rc") && !name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// Parse `dpkg-query -W -f='${Package}\n${Conffiles}\n'`.
///
/// A package name starts a line; each of its conffiles follows on an indented line as
/// `path md5sum [obsolete]`. Paths are absolute, so the leading whitespace is what distinguishes a
/// conffile line from the next package name.
#[must_use]
pub fn parse_conffiles(output: &str) -> Vec<(String, Vec<std::path::PathBuf>)> {
    let mut packages: Vec<(String, Vec<std::path::PathBuf>)> = Vec::new();

    for line in output.lines() {
        if line.is_empty() {
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            if let Some((_, files)) = packages.last_mut() {
                if let Some(path) = line.split_whitespace().next() {
                    files.push(std::path::PathBuf::from(path));
                }
            }
        } else {
            packages.push((line.trim().to_string(), Vec::new()));
        }
    }

    packages
}

/// On-disk bytes of a package's remaining configuration.
///
/// A conffile that is already gone contributes nothing, which is the honest answer: it is not there
/// to be reclaimed.
#[must_use]
pub fn conffile_bytes(files: &[std::path::PathBuf]) -> u64 {
    use std::os::unix::fs::MetadataExt;
    files
        .iter()
        .filter_map(|f| std::fs::symlink_metadata(f).ok())
        .filter(|m| m.is_file())
        .map(|m| m.blocks() * 512)
        .sum()
}

/// Parse `apt-get -s remove` output into a preview.
///
/// The simulation is the authority on what a removal actually does — guessing at dependencies
/// ourselves would be both wrong and dangerous.
#[must_use]
pub fn parse_removal_simulation(output: &str) -> RemovalPreview {
    let mut preview = RemovalPreview::default();

    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Remv ") {
            if let Some(name) = rest.split_whitespace().next() {
                preview.removing.push(name.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("Inst ") {
            if let Some(name) = rest.split_whitespace().next() {
                preview.installing.push(name.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("After this operation, ") {
            // "After this operation, 512 MB disk space will be freed."
            if rest.contains("will be freed") {
                preview.freed_bytes = parse_size_phrase(rest);
            }
        }
    }

    preview
}

/// Parse APT's "512 MB disk space will be freed" phrasing.
///
/// APT reports in powers of ten, and says so — its `MB` is 10^6, not 2^20. Treating it as binary
/// would overstate every figure by five percent.
fn parse_size_phrase(phrase: &str) -> u64 {
    let mut parts = phrase.split_whitespace();
    let Some(number) = parts
        .next()
        .and_then(|n| n.replace(',', "").parse::<f64>().ok())
    else {
        return 0;
    };
    let multiplier = match parts.next() {
        Some("kB") => 1_000.0,
        Some("MB") => 1_000_000.0,
        Some("GB") => 1_000_000_000.0,
        Some("TB") => 1_000_000_000_000.0,
        _ => 1.0,
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        (number * multiplier) as u64
    }
}

/// The dpkg/APT backend.
pub struct DpkgBackend;

impl DpkgBackend {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for DpkgBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for DpkgBackend {
    fn manager(&self) -> Manager {
        Manager::Apt
    }

    fn available(&self) -> bool {
        crate::caps::registry().has(crate::caps::Capability::Apt)
    }

    fn installed(&self) -> Result<Vec<Package>> {
        let output = query("dpkg-query", &["-W", INSTALLED_QUERY])?;
        let mut packages = parse_installed(&output);
        fill_changed_at(Path::new(DPKG_INFO), &mut packages);
        Ok(packages)
    }

    fn residual_config(&self) -> Result<Vec<ResidualConfig>> {
        let listing = query(
            "dpkg-query",
            &["-W", "-f=${Package}\t${db:Status-Abbrev}\n"],
        )?;
        let names = parse_residual_names(&listing);
        if names.is_empty() {
            return Ok(Vec::new());
        }

        // One query for every package's conffiles rather than one per package: on a machine with
        // two hundred residual packages that is one subprocess instead of two hundred.
        let mut args: Vec<&str> = vec!["-W", "-f=${Package}\n${Conffiles}\n"];
        args.extend(names.iter().map(String::as_str));
        let conffiles = query("dpkg-query", &args)?;

        Ok(parse_conffiles(&conffiles)
            .into_iter()
            .map(|(name, files)| ResidualConfig {
                bytes: conffile_bytes(&files),
                name,
            })
            // A package whose conffiles are already gone has nothing left to reclaim.
            .filter(|r| r.bytes > 0)
            .collect())
    }

    fn removal_preview(&self, names: &[String]) -> Result<RemovalPreview> {
        if names.is_empty() {
            return Ok(RemovalPreview::default());
        }
        let mut args = vec!["-s", "remove"];
        args.extend(names.iter().map(String::as_str));
        let output = query("apt-get", &args)?;
        Ok(parse_removal_simulation(&output))
    }

    fn measure(&self, id: &str) -> Result<Measured> {
        // The `.list` file directly, not `dpkg-query -L`: same content, no subprocess (§P4), and it
        // is the file whose mtime already gave us `changed_at`.
        let name = id.split_once(':').map_or(id, |(name, _)| name);
        let listing = match list_file(Path::new(DPKG_INFO), id, name) {
            Some(path) => std::fs::read_to_string(&path).map_err(|e| {
                crate::error::AppError::from_io(&e, "read a package's file list").with_path(&path)
            })?,
            None => {
                return Err(crate::error::AppError::new(
                    crate::error::ErrorCode::NotFound,
                    format!("dpkg has no file list for {id}."),
                )
                .with_remedy("Check the package name, including its architecture suffix."));
            }
        };

        let paths = parse_file_list(&listing);
        Ok(measure_paths(paths.iter().map(PathBuf::as_path)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Captured from a running Ubuntu 22.04 machine. Principle P8 asks for golden-file parser
    /// tests, and real output is the only fixture worth having: it contains the awkward cases —
    /// metapackages, `config-files` entries, a headers package with no flavour suffix — that
    /// invented samples never do.
    /// Real `dpkg-query` output, captured from this machine, in the field order of
    /// [`INSTALLED_QUERY`].
    ///
    /// The kernel names here are the real ones on purpose, and are the one place in the suite
    /// where that is true — see `docs/issues/01-privilege-and-security.md` §5. These tests exercise
    /// **version ordering** and the running-kernel rule, which a synthetic name cannot: comparing
    /// `nix-test-not-a-real-kernel` against itself proves nothing. Nothing in this module
    /// constructs an `Op`, a `Session` or an `Elevation`, so no name here can reach the helper;
    /// the fixtures that *do* feed operations are synthetic without exception.
    ///
    /// `libc6` appears twice, for two architectures. That is not padding — 41 packages on this
    /// machine are installed for both, and it is what makes a name-keyed inventory wrong.
    /// Real `dpkg-query` output, captured from this machine, in the field order of
    /// [`INSTALLED_QUERY`].
    ///
    /// The kernel names here are the real ones on purpose, and this is the one fixture in the suite
    /// where that is true — see `docs/issues/01-privilege-and-security.md` §5. These tests exercise
    /// **version ordering** and the running-kernel rule, and a synthetic name cannot: comparing
    /// `nix-test-not-a-real-kernel` against itself proves nothing about which kernel is newer. What
    /// makes that safe is a boundary rather than a convention — nothing in this module constructs an
    /// `Op`, a `Session` or an `Elevation`, so no name here has a path to the helper, and the fixtures
    /// that *do* feed operations are synthetic without exception.
    ///
    /// `libc6` appears twice, for two architectures. Not padding: 41 packages on this machine are
    /// installed for both, and it is what makes a name-keyed inventory wrong.
    const REAL_DPKG_OUTPUT: &str = "\
linux-headers-5.15.0-190\tamd64\t5.15.0-190.200\t76518\tinstalled\tinstall\tHeader files related to Linux kernel version 5.15.0
linux-headers-5.15.0-190-generic\tamd64\t5.15.0-190.200\t24696\tinstalled\tinstall\tLinux kernel headers for version 5.15.0 on 64 bit x86 SMP
linux-headers-6.8.0-136-generic\tamd64\t6.8.0-136.136~22.04.1\t28744\tinstalled\tinstall\tLinux kernel headers for version 6.8.0 on 64 bit x86 SMP
linux-headers-6.8.0-138-generic\tamd64\t6.8.0-138.138~22.04.1\t28745\tinstalled\tinstall\tLinux kernel headers for version 6.8.0 on 64 bit x86 SMP
linux-headers-generic\tamd64\t5.15.0.190.169\t22\tinstalled\tinstall\tGeneric Linux kernel headers
linux-headers-generic-hwe-22.04\tamd64\t6.8.0-138.138~22.04.1\t24\tinstalled\tinstall\tGeneric Linux kernel headers
linux-image-6.8.0-136-generic\tamd64\t6.8.0-136.136~22.04.1\t14892\tinstalled\tinstall\tSigned kernel image generic
linux-image-6.8.0-138-generic\tamd64\t6.8.0-138.138~22.04.1\t14893\tinstalled\tinstall\tSigned kernel image generic
linux-modules-6.8.0-136-generic\tamd64\t6.8.0-136.136~22.04.1\t98304\tinstalled\tinstall\tLinux kernel extra modules for version 6.8.0 on 64 bit x86 SMP
linux-modules-6.8.0-138-generic\tamd64\t6.8.0-138.138~22.04.1\t98310\tinstalled\tinstall\tLinux kernel extra modules for version 6.8.0 on 64 bit x86 SMP
linux-modules-extra-6.8.0-138-generic\tamd64\t6.8.0-138.138~22.04.1\t221184\tinstalled\tinstall\tLinux kernel extra modules for version 6.8.0 on 64 bit x86 SMP
linux-image-5.15.0-130-generic\tamd64\t5.15.0-130.140~20.04.1\t11337\tconfig-files\tdeinstall\tSigned kernel image generic
firefox\tamd64\t1:1snap1-0ubuntu5\t228\tinstalled\tinstall\tTransitional package - firefox -> firefox snap
bridge-utils\tamd64\t1.7.1-1ubuntu2\t105\tconfig-files\tdeinstall\tUtilities for configuring the Linux Ethernet bridge
libc6\tamd64\t2.35-0ubuntu3.11\t13594\tinstalled\tinstall\tGNU C Library: Shared libraries
libc6\ti386\t2.35-0ubuntu3.11\t12482\tinstalled\tinstall\tGNU C Library: Shared libraries
";

    /// The kernel this fixture is from.
    const RUNNING: &str = "6.8.0-138-generic";

    // ---- parsing ----

    #[test]
    fn installed_packages_are_parsed_from_real_output() {
        let packages = parse_installed(REAL_DPKG_OUTPUT);

        // `config-files` entries are removed-but-configured, not installed.
        assert!(
            packages.iter().all(|p| p.name != "bridge-utils"),
            "a config-files package is not installed"
        );
        assert!(packages.iter().any(|p| p.name == "firefox"));

        let firefox = packages.iter().find(|p| p.name == "firefox").unwrap();
        // Installed-Size is kibibytes, which is a thousand-fold error waiting to happen.
        assert_eq!(firefox.recorded_bytes, 228 * 1024);
        assert_eq!(firefox.manager, Manager::Apt);
        assert!(
            firefox.measured.is_none(),
            "nothing is measured until asked"
        );
        assert_eq!(firefox.arch, "amd64");
        assert_eq!(
            firefox.summary,
            "Transitional package - firefox -> firefox snap"
        );

        // # Regression
        //
        // `Package` carried no architecture, so the 41 names this machine has installed for two
        // architectures collapsed into indistinguishable rows — same name, different sizes, and
        // whichever one a removal picked was luck.
        let libc: Vec<&Package> = packages.iter().filter(|p| p.name == "libc6").collect();
        assert_eq!(libc.len(), 2, "two architectures, two packages");
        assert_ne!(
            libc[0].id,
            libc[1].id,
            "and two identities: {:?}",
            libc.iter().map(|p| &p.id).collect::<Vec<_>>()
        );
        assert_ne!(
            libc[0].recorded_bytes, libc[1].recorded_bytes,
            "they are not even the same size, so collapsing them loses real information"
        );
    }

    /// A maintainer's description is the one field whose content dpkg does not control, so it goes
    /// last and a tab inside it cannot shift the fields that matter.
    #[test]
    fn a_tab_inside_a_summary_cannot_corrupt_another_field() {
        let line = "thing\tamd64\t1.0\t512\tinstalled\tinstall\ta summary\twith a tab\n";
        let packages = parse_installed(line);

        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "thing");
        assert_eq!(packages[0].recorded_bytes, 512 * 1024);
        assert!(packages[0].explicit);
        assert_eq!(
            packages[0].summary, "a summary with a tab",
            "the stray tab is absorbed into the field it appeared in"
        );
    }

    #[test]
    fn residual_packages_are_identified_by_state() {
        let output = "bridge-utils\trc \nfirefox\tii \ncodux\trc \nlibfoo\tiU \n";
        let names = parse_residual_names(output);
        assert_eq!(names, vec!["bridge-utils", "codux"]);
    }

    /// The bug this replaced. dpkg's `Installed-Size` for a removed package is what it occupied
    /// **when installed**, not what remains: `zoom` reports 640 MiB in the `rc` state, and
    /// `bridge-utils` reports 105 KiB when a single 124-byte file is all that is left.
    #[test]
    fn conffiles_are_parsed_so_the_size_is_what_actually_remains() {
        let output = "bridge-utils\n /etc/default/bridge-utils 551c1fb3\nzoom\n /etc/zoom/a.conf abc\n /etc/zoom/b.conf def obsolete\nempty-package\n";
        let parsed = parse_conffiles(output);
        assert_eq!(parsed.len(), 3);

        assert_eq!(parsed[0].0, "bridge-utils");
        assert_eq!(
            parsed[0].1,
            vec![std::path::PathBuf::from("/etc/default/bridge-utils")]
        );

        assert_eq!(parsed[1].0, "zoom");
        assert_eq!(
            parsed[1].1.len(),
            2,
            "an obsolete marker is not a second path"
        );
        assert_eq!(parsed[1].1[1], std::path::PathBuf::from("/etc/zoom/b.conf"));

        assert!(
            parsed[2].1.is_empty(),
            "a package may have no conffiles at all"
        );
    }

    #[test]
    fn conffiles_that_no_longer_exist_contribute_nothing() {
        // Not there means not reclaimable, and claiming otherwise is the overpromise this whole
        // change exists to prevent.
        assert_eq!(
            conffile_bytes(&[std::path::PathBuf::from("/etc/definitely-not-here-at-all")]),
            0
        );
    }

    #[test]
    fn conffile_bytes_measures_a_real_file() {
        let path = std::env::temp_dir().join(format!("nix-conffile-{}.conf", std::process::id()));
        std::fs::write(&path, vec![b'x'; 4096]).unwrap();
        assert!(conffile_bytes(std::slice::from_ref(&path)) >= 4096);
        std::fs::remove_file(&path).ok();
    }

    /// End-to-end on this machine: whatever the figure is, it must be plausible for configuration
    /// files rather than for the packages they came from.
    #[test]
    fn residual_configuration_on_this_machine_is_a_plausible_size() {
        let backend = DpkgBackend::new();
        if !backend.available() {
            return;
        }
        let Ok(residual) = backend.residual_config() else {
            return;
        };
        let total: u64 = residual.iter().map(|r| r.bytes).sum();
        assert!(
            total < 1024 * 1024 * 1024,
            "residual configuration totalled {}, which is package sizes rather than config files",
            crate::format_bytes(total)
        );
    }

    #[test]
    fn malformed_lines_are_skipped_rather_than_fatal() {
        let output =
            "garbage\nname\tversion\n\nfirefox\tamd64\t1.0\t100\tinstalled\tinstall\tA browser\n";
        let packages = parse_installed(output);
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "firefox");
    }

    // ---- kernel version extraction ----

    #[test]
    fn kernel_versions_are_extracted_from_package_names() {
        for (name, expected) in [
            ("linux-image-6.8.0-138-generic", "6.8.0-138-generic"),
            ("linux-headers-6.8.0-138-generic", "6.8.0-138-generic"),
            ("linux-modules-6.8.0-138-generic", "6.8.0-138-generic"),
            // The longest prefix must win, or this becomes `extra-6.8.0-138-generic`.
            ("linux-modules-extra-6.8.0-138-generic", "6.8.0-138-generic"),
            (
                "linux-image-unsigned-6.8.0-138-generic",
                "6.8.0-138-generic",
            ),
            // A headers package with no flavour suffix, which really occurs.
            ("linux-headers-5.15.0-190", "5.15.0-190"),
        ] {
            assert_eq!(
                KernelVersion::from_package(name),
                Some(KernelVersion(expected.into())),
                "{name}"
            );
        }
    }

    /// The dangerous case. A metapackage is what *pulls in* the current kernel; treating it as a
    /// removable old one would remove the mechanism that keeps the machine's kernel up to date.
    #[test]
    fn metapackages_are_not_mistaken_for_kernel_versions() {
        for name in [
            "linux-headers-generic",
            "linux-headers-generic-hwe-22.04",
            "linux-image-generic",
            "linux-generic",
            "firefox",
            "linux-libc-dev",
        ] {
            assert_eq!(
                KernelVersion::from_package(name),
                None,
                "{name} is not a versioned kernel package"
            );
        }
    }

    /// The two parts of one kernel must be one kernel.
    #[test]
    fn the_flavour_suffix_does_not_split_a_kernel_in_two() {
        let v = |s: &str| KernelVersion(s.into());

        assert_eq!(v("6.8.0-138-generic").base(), "6.8.0-138");
        assert_eq!(v("5.15.0-190").base(), "5.15.0-190");
        assert_eq!(
            v("5.15.0-190-generic").base(),
            v("5.15.0-190").base(),
            "the architecture-independent headers belong to the same kernel as the flavoured ones"
        );
        assert_eq!(v("6.8.0-138-lowlatency").base(), "6.8.0-138");
        assert_eq!(v("5.14.0-1058-oem").base(), "5.14.0-1058");

        assert_eq!(v("6.8.0-138-generic").flavour(), Some("generic".into()));
        assert_eq!(v("5.15.0-190").flavour(), None);
    }

    #[test]
    fn kernel_versions_compare_numerically_not_lexically() {
        let v = |s: &str| KernelVersion(s.into());
        use std::cmp::Ordering;

        // Lexically "6.8.0-9" sorts after "6.8.0-10", which on this question means offering to
        // delete a kernel newer than the running one.
        assert_eq!(
            v("6.8.0-9-generic").compare(&v("6.8.0-10-generic")),
            Ordering::Less
        );
        assert_eq!(
            v("5.15.0-190").compare(&v("6.8.0-136-generic")),
            Ordering::Less
        );
        assert_eq!(
            v("6.8.0-138-generic").compare(&v("6.8.0-136-generic")),
            Ordering::Greater
        );
        assert_eq!(
            v("6.8.0-138-generic").compare(&v("6.8.0-138-generic")),
            Ordering::Equal
        );
    }

    // ---- the safety rule ----

    /// The rule this whole module exists to enforce.
    #[test]
    fn the_running_kernel_is_never_offered() {
        let packages = parse_installed(REAL_DPKG_OUTPUT);
        let running = KernelVersion(RUNNING.into());
        let removable = removable_kernels(&packages, Some(&running));

        for set in &removable {
            assert_ne!(
                set.version, running,
                "the running kernel was offered for removal — this breaks the machine"
            );
        }
    }

    /// Equally important, and less obvious: on an upgraded-but-not-rebooted machine the newest
    /// installed kernel is the one that boots next.
    #[test]
    fn the_newest_installed_kernel_is_never_offered() {
        let packages = parse_installed(REAL_DPKG_OUTPUT);

        // Pretend the machine is running something older, so the newest is *not* the running one
        // and only the newest rule can protect it.
        let running = KernelVersion("6.8.0-136-generic".into());
        let removable = removable_kernels(&packages, Some(&running));

        assert!(
            removable
                .iter()
                .all(|s| s.version != KernelVersion("6.8.0-138-generic".into())),
            "the newest kernel would boot next after an upgrade, so it must never be offered"
        );
    }

    #[test]
    fn only_strictly_older_kernels_are_offered() {
        let packages = parse_installed(REAL_DPKG_OUTPUT);
        let running = KernelVersion(RUNNING.into());
        let removable = removable_kernels(&packages, Some(&running));

        let versions: Vec<&str> = removable.iter().map(|s| s.version.0.as_str()).collect();
        assert_eq!(
            versions,
            vec!["5.15.0-190-generic", "6.8.0-136-generic"],
            "only the genuinely superseded ones, each counted once"
        );
        // And the 5.15 set holds both of its packages, not one each in two sets.
        let old_set = &removable[0];
        assert_eq!(old_set.packages.len(), 2, "{:?}", old_set.packages);
        // Oldest first, so the list a user reads top-down starts with the safest, largest wins.
        assert!(removable[0].version.compare(&removable[1].version) == std::cmp::Ordering::Less);
    }

    #[test]
    fn a_kernel_set_gathers_every_package_belonging_to_it() {
        let packages = parse_installed(REAL_DPKG_OUTPUT);
        let sets = kernel_sets(&packages);
        let newest = sets
            .iter()
            .find(|s| s.version.0 == "6.8.0-138-generic")
            .unwrap();

        let names: Vec<&str> = newest.packages.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"linux-image-6.8.0-138-generic"));
        assert!(names.contains(&"linux-headers-6.8.0-138-generic"));
        assert!(names.contains(&"linux-modules-6.8.0-138-generic"));
        assert!(names.contains(&"linux-modules-extra-6.8.0-138-generic"));
        // Removing a kernel means removing all of its parts, so the size is the whole set.
        assert!(newest.bytes() > 300 * 1024, "measured {}", newest.bytes());
    }

    #[test]
    fn a_single_installed_kernel_is_never_offered() {
        let only = "linux-image-6.8.0-138-generic\t6.8.0-138\t14893\tinstalled\tinstall\n";
        let packages = parse_installed(only);
        let running = KernelVersion("6.8.0-138-generic".into());
        assert!(
            removable_kernels(&packages, Some(&running)).is_empty(),
            "the only kernel on the machine is both newest and running"
        );
    }

    #[test]
    fn with_no_kernels_at_all_nothing_is_offered() {
        let packages = parse_installed("firefox\t1.0\t228\tinstalled\tinstall\n");
        assert!(removable_kernels(&packages, None).is_empty());
    }

    /// If the running kernel cannot be determined, the newest rule still protects the boot kernel —
    /// but the offer is more dangerous, so this test documents exactly what survives.
    #[test]
    fn without_a_known_running_kernel_the_newest_is_still_protected() {
        let packages = parse_installed(REAL_DPKG_OUTPUT);
        let removable = removable_kernels(&packages, None);
        assert!(
            removable
                .iter()
                .all(|s| s.version != KernelVersion("6.8.0-138-generic".into())),
            "the newest must be protected even without knowing what is running"
        );
    }

    #[test]
    fn the_running_kernel_is_readable_on_this_machine() {
        let running = running_kernel().expect("a Linux machine knows its kernel");
        assert!(!running.0.is_empty());
        assert!(running.0.chars().next().is_some_and(|c| c.is_ascii_digit()));
    }

    // ---- removal simulation ----

    #[test]
    fn a_removal_simulation_is_parsed_into_a_preview() {
        // Real `apt-get -s remove` shape.
        let output = "\
NOTE: This is only a simulation!
Reading package lists...
The following packages will be REMOVED:
  linux-image-6.8.0-136-generic linux-modules-6.8.0-136-generic
0 upgraded, 0 newly installed, 2 to remove and 0 not upgraded.
Remv linux-image-6.8.0-136-generic [6.8.0-136.136]
Remv linux-modules-6.8.0-136-generic [6.8.0-136.136]
After this operation, 116 MB disk space will be freed.
";
        let preview = parse_removal_simulation(output);
        assert_eq!(
            preview.removing,
            vec![
                "linux-image-6.8.0-136-generic",
                "linux-modules-6.8.0-136-generic"
            ]
        );
        assert!(preview.installing.is_empty());
        // APT reports powers of ten and says so; treating MB as 2^20 overstates by five percent.
        assert_eq!(preview.freed_bytes, 116_000_000);
    }

    /// A removal that would *install* something is unusual and worth surfacing rather than hiding.
    #[test]
    fn a_removal_that_installs_something_is_reported() {
        let output = "\
Inst replacement-package (1.0 Ubuntu:22.04 [amd64])
Remv old-package [0.9]
After this operation, 4096 kB disk space will be freed.
";
        let preview = parse_removal_simulation(output);
        assert_eq!(preview.installing, vec!["replacement-package"]);
        assert_eq!(preview.removing, vec!["old-package"]);
        assert_eq!(preview.freed_bytes, 4_096_000);
    }

    #[test]
    fn a_simulation_that_frees_nothing_reports_zero() {
        let output = "Remv thing [1.0]\nAfter this operation, 0 B disk space will be freed.\n";
        assert_eq!(parse_removal_simulation(output).freed_bytes, 0);
    }

    #[test]
    fn an_operation_that_uses_space_is_not_read_as_freeing_it() {
        let output = "Inst thing (1.0)\nAfter this operation, 50.0 MB of additional disk space will be used.\n";
        let preview = parse_removal_simulation(output);
        assert_eq!(
            preview.freed_bytes, 0,
            "an operation that consumes space must not be reported as freeing it"
        );
    }

    #[test]
    fn size_phrases_are_parsed_in_the_units_apt_uses() {
        assert_eq!(
            parse_size_phrase("512 kB disk space will be freed."),
            512_000
        );
        assert_eq!(
            parse_size_phrase("1.5 MB disk space will be freed."),
            1_500_000
        );
        assert_eq!(
            parse_size_phrase("2 GB disk space will be freed."),
            2_000_000_000
        );
        assert_eq!(
            parse_size_phrase("1,024 kB disk space will be freed."),
            1_024_000
        );
        assert_eq!(parse_size_phrase("nonsense"), 0);
    }

    #[test]
    fn an_empty_removal_asks_the_package_manager_nothing() {
        // No packages means no simulation to run, so no subprocess and no error.
        let preview = DpkgBackend::new().removal_preview(&[]).unwrap();
        assert_eq!(preview, RemovalPreview::default());
    }
}
