// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The RPM family: dnf and zypper. `PKG-3`.
//!
//! # One query layer, two front ends
//!
//! Fedora's dnf and openSUSE's zypper are different package managers over the **same database**.
//! Listing installed packages, their sizes and their file lists is `rpm` in both cases, so that work
//! lives here once. What differs is how a removal is simulated, and that is the only thing the two
//! backends implement separately.
//!
//! # `--qf`, never display output
//!
//! `rpm -qa --qf '%{NAME}\t…'` emits exactly the fields asked for, tab-separated, with no padding,
//! headers or truncation. That is the whole reason to use it rather than `dnf list installed`, whose
//! columns are laid out for a terminal.
//!
//! # What is verified here, and what is not
//!
//! **This machine has `rpm` but no RPM packages** — Ubuntu ships the tool for interoperability with an
//! empty database. That is less than a Fedora machine but more than nothing, and it verifies the thing
//! most likely to be wrong: `rpm --querytags` lists every tag the local rpm understands, so a test can
//! assert that every tag this module names is one of them. A typo'd tag would otherwise produce empty
//! output on a real machine and no error anywhere.
//!
//! The **parsers** are golden-file tested against output in the documented format. Nobody has run them
//! against a real Fedora or openSUSE installation, and that is recorded in
//! `docs/issues/README.md` as an open item alongside `STO-17`. §P7 is why they return `Unsupported`
//! rather than guessing when a tool is absent.

use std::collections::BTreeMap;

use crate::error::{AppError, ErrorCode, Result};
use crate::space::Manager;

use super::{Backend, Measured, Package, RemovalPreview, measure_paths, query};

/// The fields this module reads, in the order the parser expects them.
///
/// `LONGSIZE` rather than `SIZE`: `SIZE` is a 32-bit tag and silently wraps above 4 GiB, which is not
/// hypothetical for a package like a kernel-devel tree or a game asset bundle.
pub const INSTALLED_QUERY: &str =
    "%{NAME}\\t%{ARCH}\\t%{EPOCH}:%{VERSION}-%{RELEASE}\\t%{LONGSIZE}\\t%{SUMMARY}\\n";

/// Every rpm tag [`INSTALLED_QUERY`] names, for the test that checks they exist.
pub const QUERY_TAGS: [&str; 7] = [
    "NAME", "ARCH", "EPOCH", "VERSION", "RELEASE", "LONGSIZE", "SUMMARY",
];

/// Parse the output of [`INSTALLED_QUERY`].
///
/// `manager` decides only what the resulting packages are labelled with; the parsing is identical.
#[must_use]
pub fn parse_installed(output: &str, manager: Manager) -> Vec<Package> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next()?.trim();
            let arch = fields.next()?.trim();
            let version = fields.next()?.trim();
            let size = fields.next()?.trim();
            // Summary last, and rejoined: it is the one field a packager writes freely.
            let summary = fields.collect::<Vec<_>>().join(" ");

            if name.is_empty() {
                return None;
            }

            // rpm prints `(none)` for an absent epoch, and an epoch of 0 is conventionally not shown.
            let version = version
                .strip_prefix("(none):")
                .or_else(|| version.strip_prefix("0:"))
                .unwrap_or(version);

            Some(Package::new(
                name.to_string(),
                arch.to_string(),
                version.to_string(),
                summary.trim().to_string(),
                // Already bytes — unlike dpkg's `Installed-Size`, which is kibibytes. Getting these
                // two the same way round is a thousand-fold error waiting to happen.
                size.parse::<u64>().unwrap_or(0),
                // rpm records no explicit-versus-dependency flag in the header; dnf keeps that in its
                // own history database and zypper in its own. Reporting every package as explicit
                // would be a fabricated answer, so this reports the honest default and the UI shows
                // no dependency marker for RPM systems.
                true,
                manager,
            ))
        })
        .collect()
}

/// Parse `rpm -ql <name>` — one absolute path per line.
///
/// rpm prints `(contains no files)` for a package with an empty file list, which is prose rather than
/// a path and must not be measured.
#[must_use]
pub fn parse_file_list(output: &str) -> Vec<std::path::PathBuf> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('/'))
        .map(std::path::PathBuf::from)
        .collect()
}

/// Parse a dnf or zypper removal simulation.
///
/// Both print a human-readable transaction summary rather than anything machine-oriented, so this
/// reads the package names from the `Removing:`/`Erasing:` section. Deliberately narrow: it takes only
/// lines inside that section, so a package mentioned in a dependency explanation elsewhere in the
/// output is not counted as being removed.
#[must_use]
pub fn parse_removal_simulation(output: &str) -> RemovalPreview {
    let mut preview = RemovalPreview::default();
    let mut in_removals = false;

    for line in output.lines() {
        let trimmed = line.trim();

        // Section headings, as dnf and zypper spell them.
        let heading = trimmed.trim_end_matches(':').to_ascii_lowercase();
        if matches!(
            heading.as_str(),
            "removing" | "erasing" | "removing dependent packages" | "removing unused dependencies"
        ) {
            in_removals = true;
            continue;
        }
        // Any other unindented heading ends the section.
        if in_removals && !line.starts_with(' ') && !line.starts_with('\t') {
            if trimmed.is_empty() || trimmed.ends_with(':') {
                in_removals = false;
            }
            continue;
        }

        if in_removals {
            if let Some(name) = trimmed.split_whitespace().next() {
                if !name.is_empty() {
                    preview.removing.push(name.to_string());
                }
            }
            continue;
        }

        // "Freed space: 1.2 GiB" (zypper) / "Freed space: 1.2 G" (dnf).
        if let Some(rest) = trimmed.strip_prefix("Freed space:") {
            preview.freed_bytes = parse_size_phrase(rest);
        }
    }

    preview
}

/// Parse a size phrase like `1.2 GiB` or `512 k`.
///
/// Binary units throughout: both tools mean powers of two here, whichever suffix they print.
fn parse_size_phrase(phrase: &str) -> u64 {
    let cleaned = phrase.trim().trim_end_matches('.');
    let mut number = String::new();
    let mut rest = cleaned;

    for (index, c) in cleaned.char_indices() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
        } else {
            rest = cleaned[index..].trim();
            break;
        }
    }

    let value: f64 = number.parse().unwrap_or(0.0);
    let multiplier = match rest
        .chars()
        .next()
        .map(|c| c.to_ascii_lowercase())
        .unwrap_or(' ')
    {
        'k' => 1024.0,
        'm' => 1024.0 * 1024.0,
        'g' => 1024.0 * 1024.0 * 1024.0,
        't' => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a size phrase is non-negative and far below u64::MAX"
    )]
    let bytes = (value * multiplier) as u64;
    bytes
}

/// Shared behaviour for the two RPM front ends.
fn rpm_installed(manager: Manager) -> Result<Vec<Package>> {
    let output = query("rpm", &["-qa", "--qf", INSTALLED_QUERY])?;
    Ok(parse_installed(&output, manager))
}

fn rpm_measure(id: &str) -> Result<Measured> {
    let output = query("rpm", &["-ql", id])?;
    let paths = parse_file_list(&output);
    if paths.is_empty() {
        return Err(AppError::new(
            ErrorCode::NotFound,
            format!("rpm lists no files for {id}."),
        ));
    }
    Ok(measure_paths(paths.iter().map(std::path::PathBuf::as_path)))
}

/// Fedora and friends.
#[derive(Debug, Clone, Copy, Default)]
pub struct DnfBackend;

impl DnfBackend {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Backend for DnfBackend {
    fn manager(&self) -> Manager {
        Manager::Dnf
    }

    fn available(&self) -> bool {
        crate::caps::registry().has(crate::caps::Capability::Dnf)
    }

    fn installed(&self) -> Result<Vec<Package>> {
        rpm_installed(Manager::Dnf)
    }

    fn removal_preview(&self, names: &[String]) -> Result<RemovalPreview> {
        if names.is_empty() {
            return Ok(RemovalPreview::default());
        }
        // `--assumeno` answers the confirmation prompt with "no", so the transaction is printed and
        // then abandoned. It exits non-zero for that reason, which is why the output is read rather
        // than the status trusted.
        let mut args = vec!["remove", "--assumeno"];
        args.extend(names.iter().map(String::as_str));
        let output = super::query_allowing_failure("dnf", &args)?;

        let mut preview = parse_removal_simulation(&output);
        preview.requested = names.to_vec();
        preview.settle();
        Ok(preview)
    }

    fn measure(&self, id: &str) -> Result<Measured> {
        rpm_measure(id)
    }
}

/// openSUSE. Detected by Stacer and never implemented.
#[derive(Debug, Clone, Copy, Default)]
pub struct ZypperBackend;

impl ZypperBackend {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Backend for ZypperBackend {
    fn manager(&self) -> Manager {
        Manager::Zypper
    }

    fn available(&self) -> bool {
        crate::caps::registry().has(crate::caps::Capability::Zypper)
    }

    fn installed(&self) -> Result<Vec<Package>> {
        rpm_installed(Manager::Zypper)
    }

    fn removal_preview(&self, names: &[String]) -> Result<RemovalPreview> {
        if names.is_empty() {
            return Ok(RemovalPreview::default());
        }
        let mut args = vec!["--non-interactive", "remove", "--dry-run"];
        args.extend(names.iter().map(String::as_str));
        let output = super::query_allowing_failure("zypper", &args)?;

        let mut preview = parse_removal_simulation(&output);
        preview.requested = names.to_vec();
        preview.settle();
        Ok(preview)
    }

    fn measure(&self, id: &str) -> Result<Measured> {
        rpm_measure(id)
    }
}

/// Tags the local `rpm` understands, from `rpm --querytags`.
///
/// Used only by the test that checks this module's query against reality.
pub fn known_tags() -> Result<BTreeMap<String, ()>> {
    let output = query("rpm", &["--querytags"])?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| (line.to_string(), ()))
        .collect())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// `rpm -qa --qf` output in the format [`INSTALLED_QUERY`] asks for. Written to rpm's documented
    /// behaviour, including `(none)` for a missing epoch, which is what rpm actually prints.
    const RPM_OUTPUT: &str = "\
bash\tx86_64\t(none):5.2.26-3.fc40\t8388608\tThe GNU Bourne Again shell
glibc\tx86_64\t2:2.39-13.fc40\t6291456\tThe GNU libc libraries
kernel-core\tx86_64\t(none):6.8.9-300.fc40\t134217728\tThe Linux kernel
python3-pip\tnoarch\t0:24.0-1.fc40\t12582912\tA tool for installing Python packages
tab-in-summary\tnoarch\t(none):1.0-1\t1024\ta summary\twith a tab in it
";

    // ---- what this machine can actually verify ----

    /// # The one real check available here
    ///
    /// This machine has `rpm` and an empty RPM database, so no package can be listed — but
    /// `rpm --querytags` still reports every tag the local rpm understands. A typo'd tag is the most
    /// likely defect in a parser nobody can run for real: rpm would emit empty fields and no error,
    /// and the packages would silently have no sizes.
    #[test]
    fn every_tag_the_query_names_is_one_rpm_understands() {
        let Ok(tags) = known_tags() else {
            return; // no rpm here
        };
        assert!(
            tags.len() > 50,
            "rpm --querytags returned {} tags",
            tags.len()
        );

        for tag in QUERY_TAGS {
            assert!(
                tags.contains_key(tag),
                "rpm does not know the tag {tag}, so this query would return empty fields silently"
            );
        }
    }

    /// `LONGSIZE`, not `SIZE`. The 32-bit tag wraps above 4 GiB, which a kernel-devel tree or a game
    /// asset package reaches.
    #[test]
    fn the_query_uses_the_wide_size_tag() {
        assert!(INSTALLED_QUERY.contains("LONGSIZE"));
        assert!(
            !INSTALLED_QUERY.contains("%{SIZE}"),
            "the 32-bit SIZE tag wraps silently"
        );
    }

    /// And the local rpm accepts the format string as written — a malformed `--qf` is an error, so this
    /// checks the syntax rather than the tag names.
    #[test]
    fn the_local_rpm_accepts_the_query_format() {
        if crate::pkg::query("rpm", &["--version"]).is_err() {
            return;
        }
        // Empty database, so an empty result is the expected success.
        let result = crate::pkg::query("rpm", &["-qa", "--qf", INSTALLED_QUERY]);
        assert!(
            result.is_ok(),
            "rpm rejected the query format: {:?}",
            result.err()
        );
    }

    // ---- parsing ----

    #[test]
    fn installed_packages_are_parsed_with_their_sizes() {
        let packages = parse_installed(RPM_OUTPUT, Manager::Dnf);
        assert_eq!(packages.len(), 5);

        let bash = packages.iter().find(|p| p.name == "bash").unwrap();
        assert_eq!(bash.arch, "x86_64");
        assert_eq!(bash.id, "bash:x86_64");
        assert_eq!(
            bash.recorded_bytes,
            8 * 1024 * 1024,
            "rpm reports bytes, unlike dpkg's kibibytes"
        );
        assert_eq!(bash.manager, Manager::Dnf);
        assert_eq!(bash.summary, "The GNU Bourne Again shell");
    }

    /// rpm prints `(none)` for an absent epoch and `0:` for a zero one; neither belongs in a version
    /// the user reads, and a real epoch does.
    #[test]
    fn epochs_are_shown_only_when_they_mean_something() {
        let packages = parse_installed(RPM_OUTPUT, Manager::Dnf);
        let find = |name: &str| {
            packages
                .iter()
                .find(|p| p.name == name)
                .unwrap()
                .version
                .clone()
        };

        assert_eq!(find("bash"), "5.2.26-3.fc40", "(none): is stripped");
        assert_eq!(
            find("python3-pip"),
            "24.0-1.fc40",
            "a zero epoch is stripped"
        );
        assert_eq!(find("glibc"), "2:2.39-13.fc40", "a real epoch is kept");
    }

    #[test]
    fn a_tab_in_a_summary_cannot_corrupt_another_field() {
        let packages = parse_installed(RPM_OUTPUT, Manager::Dnf);
        let odd = packages
            .iter()
            .find(|p| p.name == "tab-in-summary")
            .unwrap();

        assert_eq!(odd.recorded_bytes, 1024);
        assert_eq!(odd.summary, "a summary with a tab in it");
    }

    #[test]
    fn the_manager_label_follows_the_front_end_not_the_database() {
        assert_eq!(
            parse_installed(RPM_OUTPUT, Manager::Zypper)[0].manager,
            Manager::Zypper,
            "the same rpm database, reported under whichever manager asked"
        );
    }

    #[test]
    fn a_file_list_ignores_rpms_prose_for_an_empty_package() {
        let output = "/usr/bin/thing\n/usr/share/doc/thing/README\n";
        assert_eq!(parse_file_list(output).len(), 2);

        assert!(
            parse_file_list("(contains no files)\n").is_empty(),
            "prose is not a path"
        );
    }

    // ---- removal simulations ----

    /// dnf's transaction summary, in the shape it prints.
    #[test]
    fn a_dnf_removal_summary_yields_the_packages_going() {
        let output = "\
Dependencies resolved.
================================================================================
 Package             Architecture   Version            Repository        Size
================================================================================
Removing:
 nano                x86_64         7.2-4.fc40         @fedora          2.4 M
Removing unused dependencies:
 nano-default-editor noarch         7.2-4.fc40         @fedora          8.0 k

Transaction Summary
================================================================================
Remove  2 Packages

Freed space: 2.4 M
Operation aborted.
";
        let preview = parse_removal_simulation(output);
        assert_eq!(preview.removing, vec!["nano", "nano-default-editor"]);
        assert_eq!(preview.freed_bytes, (2.4 * 1024.0 * 1024.0) as u64);
    }

    /// # The narrowness that matters
    ///
    /// A package named in an explanation elsewhere in the output is not being removed. Reading every
    /// line that looks like a package name would report packages the user is not losing.
    #[test]
    fn a_package_mentioned_outside_the_removal_section_is_not_counted() {
        let output = "\
Problem: conflicting requests
  - nothing provides libfoo needed by bar-1.0
Removing:
 nano                x86_64         7.2-4.fc40         @fedora          2.4 M

Transaction Summary
Remove  1 Package
";
        let preview = parse_removal_simulation(output);
        assert_eq!(
            preview.removing,
            vec!["nano"],
            "only the removal section counts: {:?}",
            preview.removing
        );
    }

    #[test]
    fn size_phrases_are_read_as_binary_units() {
        assert_eq!(parse_size_phrase(" 2.4 M"), (2.4 * 1024.0 * 1024.0) as u64);
        assert_eq!(parse_size_phrase("512 k"), 512 * 1024);
        assert_eq!(
            parse_size_phrase("1.5 GiB"),
            (1.5 * 1024.0 * 1024.0 * 1024.0) as u64
        );
        assert_eq!(parse_size_phrase("900"), 900, "no unit means bytes");
        assert_eq!(parse_size_phrase(""), 0);
        assert_eq!(parse_size_phrase("nonsense"), 0, "and never a panic");
    }

    /// An empty selection invokes nothing. The Stacer defect `PKG-2` exists to avoid.
    #[test]
    fn an_empty_selection_runs_no_command() {
        assert_eq!(
            DnfBackend::new().removal_preview(&[]).unwrap(),
            RemovalPreview::default()
        );
        assert_eq!(
            ZypperBackend::new().removal_preview(&[]).unwrap(),
            RemovalPreview::default()
        );
    }

    /// Absent tools report nothing rather than something. §P7.
    #[test]
    fn a_backend_whose_tool_is_absent_is_not_available() {
        let has = |c| crate::caps::registry().has(c);
        assert_eq!(
            DnfBackend::new().available(),
            has(crate::caps::Capability::Dnf)
        );
        assert_eq!(
            ZypperBackend::new().available(),
            has(crate::caps::Capability::Zypper)
        );

        // On this machine specifically, neither is present.
        if !has(crate::caps::Capability::Dnf) {
            assert!(
                !crate::pkg::backends()
                    .iter()
                    .any(|b| b.manager() == Manager::Dnf),
                "an absent manager must not appear in the backend list"
            );
        }
    }
}
