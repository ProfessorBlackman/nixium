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
//! # Sizes are reported twice, deliberately
//!
//! Decision D2: the package database's own figure by default, and a measured walk on request. The
//! gap between them is post-install growth, which is information — so they are never conflated and
//! the measured figure never silently replaces the recorded one.

pub mod dpkg;
pub mod flatpak;
pub mod snap;

pub use dpkg::DpkgBackend;

use crate::caps::{self, Capability};
use crate::error::{AppError, Cause, ErrorCode, Result};
use crate::space::Manager;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One installed package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Package {
    pub name: String,
    pub version: String,
    /// Size the package database records, in bytes. Fast, and what the manager itself believes.
    #[ts(type = "number")]
    pub recorded_bytes: u64,
    /// Size measured by walking the package's files, when the user has asked for it.
    ///
    /// Never overwrites [`Package::recorded_bytes`]: the difference between them is post-install
    /// growth, and collapsing the two would hide it.
    #[ts(type = "number | null")]
    pub measured_bytes: Option<u64>,
    /// Whether the user asked for this, as opposed to it arriving as a dependency.
    pub explicit: bool,
    pub manager: Manager,
}

impl Package {
    /// The size to show: measured when it has been taken, recorded otherwise.
    #[must_use]
    pub const fn display_bytes(&self) -> u64 {
        match self.measured_bytes {
            Some(measured) => measured,
            None => self.recorded_bytes,
        }
    }

    /// Bytes the package grew after installation, when both figures are known.
    ///
    /// `None` rather than zero when unmeasured: "not measured" and "did not grow" are different
    /// answers, and a storage tool should not conflate them.
    #[must_use]
    pub fn growth(&self) -> Option<u64> {
        self.measured_bytes
            .map(|measured| measured.saturating_sub(self.recorded_bytes))
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

    fn package(recorded: u64, measured: Option<u64>) -> Package {
        Package {
            name: "thing".into(),
            version: "1.0".into(),
            recorded_bytes: recorded,
            measured_bytes: measured,
            explicit: true,
            manager: Manager::Apt,
        }
    }

    /// Decision D2: two figures, never conflated.
    #[test]
    fn recorded_and_measured_sizes_stay_distinct() {
        let unmeasured = package(1000, None);
        assert_eq!(unmeasured.display_bytes(), 1000);
        assert_eq!(
            unmeasured.growth(),
            None,
            "not measured and did not grow are different answers"
        );

        let measured = package(1000, Some(2500));
        assert_eq!(
            measured.recorded_bytes, 1000,
            "the recorded figure must survive measurement"
        );
        assert_eq!(measured.display_bytes(), 2500);
        assert_eq!(
            measured.growth(),
            Some(1500),
            "the gap is post-install growth"
        );
    }

    #[test]
    fn a_package_that_shrank_reports_no_negative_growth() {
        assert_eq!(package(2000, Some(1500)).growth(), Some(0));
    }

    #[test]
    fn packages_round_trip_over_the_wire() {
        let p = package(4096, Some(8192));
        let back: Package = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(p, back);
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
