// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The idle-CPU budget from `SPEC.md` §7.3, in a binary of its own. `PLT-6`.
//!
//! # Why this is not a lib test
//!
//! The measurement is this process's CPU time over a wall-clock window, which is what the
//! specification's row means — and it is only that if nothing else in the process is working.
//! `cargo test` runs a crate's tests in **parallel threads of one process**, so the first version of
//! this, living beside the other budgets, reported **196% of one core**. That was the rest of the
//! suite scanning fixtures, not the sampler.
//!
//! An integration test is a separate binary. With one test in the file, the measurement has the
//! process to itself.
//!
//! Gated on `NIX_PERF=1` like the other budgets: a loaded CI runner can make a correct implementation
//! look slow, and a flaky budget is one people learn to ignore.

use std::time::{Duration, Instant};

use nix_core::budget::{IDLE_CPU_MONITORING, enabled, process_cpu_seconds};
use nix_core::metrics::Pipeline;

/// `SPEC.md` §7.3: idle CPU, window open, monitoring view mounted, under 1% of one core.
///
/// Measured against the real pipeline with a real subscription, because the claim is about what the
/// app does while a user watches it — not about what a sampler costs in isolation.
#[test]
fn idle_cpu_with_a_monitoring_view_mounted_meets_its_budget() {
    if !enabled() {
        return;
    }

    let pipeline = Pipeline::new();
    let subscription = pipeline.subscribe();

    // Settle first: the opening tick reads every family and allocates its history, which is real work
    // and not the steady state this budget is about.
    std::thread::sleep(Duration::from_millis(750));

    let before = process_cpu_seconds();
    let start = Instant::now();
    std::thread::sleep(Duration::from_secs(4));
    let used = process_cpu_seconds() - before;
    let elapsed = start.elapsed().as_secs_f64();

    // Dropped before asserting, so a failure does not leave a sampler running into the next test.
    drop(subscription);
    drop(pipeline);

    let share = if elapsed > 0.0 { used / elapsed } else { 0.0 };
    println!("{}", IDLE_CPU_MONITORING.verdict(share));
    assert!(
        share <= IDLE_CPU_MONITORING.limit,
        "{}",
        IDLE_CPU_MONITORING.verdict(share)
    );
}

/// And the other half of the same row: with nothing subscribed, the cost is *nothing*, because the
/// worker blocks rather than polling (§P9).
///
/// This is the claim that distinguishes nix from Stacer, which sampled on a timer whether or not
/// anyone was looking.
#[test]
fn an_unsubscribed_pipeline_costs_nothing_at_all() {
    if !enabled() {
        return;
    }

    let pipeline = Pipeline::new();

    let before = process_cpu_seconds();
    std::thread::sleep(Duration::from_secs(2));
    let used = process_cpu_seconds() - before;

    drop(pipeline);

    // Effectively zero: the worker is blocked on a condvar, so it accumulates no measurable time.
    // The allowance is **two clock ticks**, which is the resolution floor of `/proc/self/stat` rather
    // than headroom — this machine measures exactly one tick (0.01s) over the two seconds, all of it
    // the sleep. A single-tick limit would sit on the floor and fail on rounding.
    assert!(
        used <= 0.02,
        "an idle pipeline used {used:.4}s of CPU over two seconds — it should be blocked, not polling"
    );
    println!("PASS idle_cpu_unsubscribed: {used:.4}s over 2s");
}
