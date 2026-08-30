// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The resident-memory budget from `SPEC.md` §7.3, in a binary of its own. `PLT-6`.
//!
//! # Why this is not a lib test
//!
//! The same reason as `tests/idle_cpu.rs`, and I made the mistake twice: `/proc/self/statm` reports the
//! **whole process**, and `cargo test` runs a crate's tests in parallel threads of one process. Beside
//! the other budgets this measured the entire suite's allocations — 317.5 MiB against a 150 MiB budget,
//! where an isolated run of the same code measures **88.2 MiB**.
//!
//! It passed the first time I ran it, because I ran it alone. That is the more useful half of the
//! lesson: a measurement that is correct in isolation and wrong in company will pass exactly when you
//! are looking at it.
//!
//! Unlike the CPU budget this is **not** gated on `NIX_PERF`. Memory is far less sensitive to a shared
//! runner than timing is, and this is the budget that caught a 4.2 GiB scanner (`STO-19`) — a
//! regression here is worth failing an ordinary test run for.

use nix_core::budget::{MEMORY_STEADY, resident_bytes};
use nix_core::{fixture, op, scan};

/// `SPEC.md` §7.3: resident memory in the steady state, under 150 MB.
///
/// Measured after a scan, which is the largest allocation the app makes — the point of the budget is
/// the tree, not the baseline.
#[test]
fn steady_state_memory_meets_its_budget() {
    let baseline = resident_bytes().expect("/proc/self/statm exists on Linux");

    let spec = fixture::Spec::perf();
    let Ok(built) = fixture::Fixture::create(&spec) else {
        return;
    };

    let options = scan::Options::new(built.root());
    let result = scan::scan_quiet(options, op::CancelToken::new());
    assert!(
        result.is_ok(),
        "the fixture scan failed: {:?}",
        result.err()
    );

    let resident = resident_bytes().unwrap_or(0);
    println!(
        "{} (baseline before the scan: {})",
        MEMORY_STEADY.verdict(resident),
        nix_core::format_bytes(baseline)
    );
    assert!(
        MEMORY_STEADY.passes(resident),
        "{}",
        MEMORY_STEADY.verdict(resident)
    );
}
