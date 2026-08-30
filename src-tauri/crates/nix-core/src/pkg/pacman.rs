// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Arch Linux. `PKG-3`.
//!
//! # Entirely unverified against a real machine, and this says so
//!
//! There is no pacman on the machine this was written on — not even the tool, unlike `rpm`, which at
//! least let the RPM backend's query format be checked against `rpm --querytags`. So every parser here
//! is written to pacman's documented output and tested against golden files, and **nobody has run it
//! against an Arch installation**.
//!
//! That is recorded as an open item in `docs/issues/README.md` rather than left implicit, and it is why
//! the backend is capability-probed: on a machine without pacman it reports nothing at all, which is
//! §P7's answer — an absent manager returns nothing rather than a fabricated something.
//!
//! # `-Qi` rather than `-Q`
//!
//! `pacman -Q` gives name and version only. `-Qi` gives a `Key : Value` block per package including
//! `Installed Size`, which is the field this product exists to report. The cost is parsing a block
//! format rather than a line format, which is a fair trade for having the size at all.

use crate::error::Result;
use crate::space::Manager;

use super::{Backend, Measured, Package, RemovalPreview, measure_paths, query};

/// Parse `pacman -Qi` output: `Key : Value` lines, blank line between packages.
///
/// Continuation lines for multi-value fields are indented; they are ignored here because none of the
/// fields this reads are multi-valued.
#[must_use]
pub fn parse_installed(output: &str) -> Vec<Package> {
    let mut packages = Vec::new();
    let mut current: Vec<(String, String)> = Vec::new();

    let finish = |fields: &mut Vec<(String, String)>, out: &mut Vec<Package>| {
        if fields.is_empty() {
            return;
        }
        let get = |wanted: &str| {
            fields
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(wanted))
                .map(|(_, value)| value.as_str())
                .unwrap_or_default()
        };

        let name = get("Name");
        if !name.is_empty() {
            out.push(Package::new(
                name.to_string(),
                get("Architecture").to_string(),
                get("Version").to_string(),
                get("Description").to_string(),
                parse_size(get("Installed Size")),
                // pacman distinguishes these properly, and says so in this very field.
                get("Install Reason").starts_with("Explicitly"),
                Manager::Pacman,
            ));
        }
        fields.clear();
    };

    for line in output.lines() {
        if line.trim().is_empty() {
            finish(&mut current, &mut packages);
            continue;
        }
        // An indented line continues the previous field; none of the fields read here need it.
        if line.starts_with(' ') && !line.trim_start().contains(" : ") {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            current.push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    finish(&mut current, &mut packages);

    packages
}

/// Parse pacman's `Installed Size` — a number, a space, and a unit.
///
/// pacman prints `12.34 MiB`. **Binary units**, as the `iB` says, which is the detail that makes this
/// worth a function rather than a `parse()`: reading `MiB` as a million would understate every size by
/// five percent, consistently, in a tool whose whole purpose is reporting sizes.
#[must_use]
pub fn parse_size(value: &str) -> u64 {
    let cleaned = value.trim();
    let (number, unit) = match cleaned.find(|c: char| !c.is_ascii_digit() && c != '.' && c != ',') {
        Some(at) => (&cleaned[..at], cleaned[at..].trim()),
        None => (cleaned, ""),
    };

    let parsed: f64 = number.replace(',', "").parse().unwrap_or(0.0);
    let multiplier = match unit.chars().next().map(|c| c.to_ascii_uppercase()) {
        Some('K') => 1024.0,
        Some('M') => 1024.0 * 1024.0,
        Some('G') => 1024.0 * 1024.0 * 1024.0,
        Some('T') => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a package size is non-negative and far below u64::MAX"
    )]
    let bytes = (parsed * multiplier) as u64;
    bytes
}

/// Parse `pacman -Rp` — one `name-version` per line, on stdout.
///
/// `-p` prints the targets and does nothing else, which makes this the one removal simulation in this
/// crate that is genuinely machine-readable.
#[must_use]
pub fn parse_removal_simulation(output: &str) -> RemovalPreview {
    let mut preview = RemovalPreview::default();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with(':') || trimmed.starts_with("warning") {
            continue;
        }
        // `name-1.2.3-1`: strip the two trailing version components pacman appends.
        let name = trimmed
            .rsplitn(3, '-')
            .nth(2)
            .filter(|n| !n.is_empty())
            .unwrap_or(trimmed);
        preview.removing.push(name.to_string());
    }
    preview
}

/// Arch Linux and derivatives.
#[derive(Debug, Clone, Copy, Default)]
pub struct PacmanBackend;

impl PacmanBackend {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Backend for PacmanBackend {
    fn manager(&self) -> Manager {
        Manager::Pacman
    }

    fn available(&self) -> bool {
        crate::caps::registry().has(crate::caps::Capability::Pacman)
    }

    fn installed(&self) -> Result<Vec<Package>> {
        Ok(parse_installed(&query("pacman", &["-Qi"])?))
    }

    fn removal_preview(&self, names: &[String]) -> Result<RemovalPreview> {
        if names.is_empty() {
            return Ok(RemovalPreview::default());
        }
        let mut args = vec!["-Rp"];
        args.extend(names.iter().map(String::as_str));
        let output = super::query_allowing_failure("pacman", &args)?;

        let mut preview = parse_removal_simulation(&output);
        preview.requested = names.to_vec();
        preview.settle();
        Ok(preview)
    }

    fn measure(&self, id: &str) -> Result<Measured> {
        // `-Ql` prints `package /path` per line, so the path is the second field.
        let output = query("pacman", &["-Ql", id])?;
        let paths: Vec<std::path::PathBuf> = output
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(_, path)| std::path::PathBuf::from(path.trim()))
            .collect();
        Ok(measure_paths(paths.iter().map(std::path::PathBuf::as_path)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// `pacman -Qi` output, in pacman's documented shape. **Not captured from a real machine** — there
    /// is no pacman here — which is why this module's header says so plainly.
    const QI_OUTPUT: &str = "\
Name            : bash
Version         : 5.2.026-2
Description     : The GNU Bourne Again shell
Architecture    : x86_64
URL             : https://www.gnu.org/software/bash/bash.html
Licenses        : GPL3
Groups          : None
Provides        : sh
Depends On      : readline  libreadline.so=8-64  glibc  ncurses
Optional Deps   : bash-completion: programmable completion [installed]
Required By     : ca-certificates-utils  dbus  filesystem
Optional For    : None
Conflicts With  : None
Replaces        : None
Installed Size  : 8.65 MiB
Packager        : Frederik Schwan <freswa@archlinux.org>
Build Date      : Wed 01 May 2024 10:00:00 AM UTC
Install Date    : Thu 02 May 2024 11:00:00 AM UTC
Install Reason  : Installed as a dependency for another package
Install Script  : No
Validated By    : Signature

Name            : linux
Version         : 6.8.9.arch1-1
Description     : The Linux kernel and modules
Architecture    : x86_64
Installed Size  : 138.24 MiB
Install Reason  : Explicitly installed
Validated By    : Signature

Name            : tiny
Version         : 1.0-1
Description     : A very small package
Architecture    : any
Installed Size  : 512.00 B
Install Reason  : Explicitly installed
";

    #[test]
    fn packages_are_parsed_from_the_block_format() {
        let packages = parse_installed(QI_OUTPUT);
        assert_eq!(packages.len(), 3);

        let bash = &packages[0];
        assert_eq!(bash.name, "bash");
        assert_eq!(bash.version, "5.2.026-2");
        assert_eq!(bash.arch, "x86_64");
        assert_eq!(bash.summary, "The GNU Bourne Again shell");
        assert_eq!(bash.manager, Manager::Pacman);
        assert_eq!(bash.id, "bash:x86_64");
    }

    /// pacman is the one manager that records this properly, and says so in the field itself.
    #[test]
    fn the_install_reason_distinguishes_explicit_from_dependency() {
        let packages = parse_installed(QI_OUTPUT);
        assert!(!packages[0].explicit, "bash was installed as a dependency");
        assert!(packages[1].explicit, "the kernel was explicitly installed");
    }

    /// # The detail worth a function
    ///
    /// pacman prints `MiB`, and it means it. Reading that as a million understates every size by five
    /// percent — consistently, invisibly, in a tool whose entire purpose is reporting sizes.
    #[test]
    fn sizes_are_read_as_binary_units() {
        assert_eq!(parse_size("8.65 MiB"), (8.65 * 1024.0 * 1024.0) as u64);
        assert_eq!(parse_size("138.24 MiB"), (138.24 * 1024.0 * 1024.0) as u64);
        assert_eq!(parse_size("512.00 B"), 512);
        assert_eq!(parse_size("1.00 GiB"), 1024 * 1024 * 1024);
        assert_eq!(parse_size("4.00 KiB"), 4096);

        // The mistake this guards against, stated as a number.
        let mib = parse_size("100.00 MiB");
        assert_ne!(mib, 100_000_000, "MiB is not a million");
        assert_eq!(mib, 104_857_600);
    }

    #[test]
    fn a_malformed_size_is_zero_rather_than_a_panic() {
        for bad in ["", "   ", "unknown", "MiB", "-", "1.2.3 MiB"] {
            let _ = parse_size(bad); // must not panic
        }
        assert_eq!(parse_size(""), 0);
        assert_eq!(parse_size("unknown"), 0);
    }

    #[test]
    fn the_parsed_sizes_match_the_blocks_they_came_from() {
        let packages = parse_installed(QI_OUTPUT);
        assert_eq!(packages[2].name, "tiny");
        assert_eq!(packages[2].recorded_bytes, 512);
    }

    /// A block missing its name is not a package. Blank-line separation means a trailing newline or a
    /// stray blank must not produce an empty entry.
    #[test]
    fn stray_blank_lines_do_not_produce_empty_packages() {
        let output = format!("\n\n{QI_OUTPUT}\n\n\n");
        assert_eq!(parse_installed(&output).len(), 3);
        assert!(parse_installed("\n\n\n").is_empty());
        assert!(parse_installed("").is_empty());
    }

    /// `pacman -Rp` is the one genuinely machine-readable removal simulation in this crate.
    #[test]
    fn a_removal_simulation_strips_the_version_from_each_target() {
        let output = "\
bash-5.2.026-2
nano-default-editor-7.2-4
linux-6.8.9.arch1-1
";
        let preview = parse_removal_simulation(output);
        assert_eq!(
            preview.removing,
            vec!["bash", "nano-default-editor", "linux"],
            "pacman appends `-version-release`, and a name may itself contain hyphens"
        );
    }

    #[test]
    fn warnings_and_progress_lines_are_not_package_names() {
        let output = ":: some informational line\nwarning: something happened\nbash-5.2.026-2\n";
        assert_eq!(parse_removal_simulation(output).removing, vec!["bash"]);
    }

    #[test]
    fn an_empty_selection_runs_no_command() {
        assert_eq!(
            PacmanBackend::new().removal_preview(&[]).unwrap(),
            RemovalPreview::default()
        );
    }

    /// §P7: an absent manager reports nothing, never a fabricated something.
    #[test]
    fn pacman_is_not_available_on_a_machine_without_it() {
        let present = crate::caps::registry().has(crate::caps::Capability::Pacman);
        assert_eq!(PacmanBackend::new().available(), present);

        if !present {
            assert!(
                !crate::pkg::backends()
                    .iter()
                    .any(|b| b.manager() == Manager::Pacman)
            );
        }
    }
}
