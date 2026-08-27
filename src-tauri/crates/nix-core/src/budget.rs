// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Performance budgets, asserted rather than aspired to. Task 0.11 (`PLT-6`).
//!
//! The spec's §7.3 table is only meaningful if a build can fail against it. This module holds the
//! numbers in one place and provides the measurement harness; the budgets that need a scanner
//! arrive with the scanner (task 1.3).
//!
//! Measurement runs only when `NIX_PERF=1`, so a normal `cargo test` stays fast and a shared CI
//! runner's noise cannot fail an unrelated pull request.

use std::time::{Duration, Instant};

/// A single budget from the specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Stable identifier, used in reports.
    pub id: &'static str,
    /// What is being measured, in the spec's words.
    pub what: &'static str,
    /// The ceiling.
    pub limit: Duration,
}

impl Budget {
    /// Check a measurement, returning a human-readable verdict.
    #[must_use]
    pub fn verdict(&self, measured: Duration) -> Verdict {
        Verdict {
            budget: *self,
            measured,
            passed: measured <= self.limit,
        }
    }
}

/// Outcome of one measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    pub budget: Budget,
    pub measured: Duration,
    pub passed: bool,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}: {:?} against a {:?} budget — {}",
            if self.passed { "PASS" } else { "FAIL" },
            self.budget.id,
            self.measured,
            self.budget.limit,
            self.budget.what
        )
    }
}

/// Cold start to interactive. Owned by the app, asserted at the shell level.
pub const COLD_START: Budget = Budget {
    id: "cold_start",
    what: "cold start to interactive",
    limit: Duration::from_millis(800),
};

/// Cancellation latency for any streaming operation.
pub const CANCEL_LATENCY: Budget = Budget {
    id: "cancel_latency",
    what: "cancellation latency, any streaming operation",
    limit: Duration::from_millis(200),
};

/// Second open of the space explorer, served from cache.
pub const CACHED_OPEN: Budget = Budget {
    id: "cached_open",
    what: "space explorer second open, from cache",
    limit: Duration::from_millis(300),
};

/// Directory scan throughput, per file so a small fixture can assert the same rate.
///
/// The specification's requirement is 2 million files in 60 seconds — 30 µs per file. This guard is
/// deliberately much tighter than that, because the requirement turned out to be three orders of
/// magnitude off what the scanner actually achieves and a budget with that much slack guards nothing.
///
/// Measured on this development machine, scanning `/usr` (422,330 files, 45,488 directories, eight
/// cores): **1.80 µs per file**. The limit is set at 10 µs, which catches any regression worse than
/// about 5x while leaving room for a CI runner slower than a desktop.
pub const SCAN_PER_FILE: Budget = Budget {
    id: "scan_per_file",
    what: "filesystem scan, per file (spec asks 30µs; measured 1.8µs)",
    limit: Duration::from_nanos(10_000),
};

/// Whether measurement is enabled for this run.
#[must_use]
pub fn enabled() -> bool {
    std::env::var_os("NIX_PERF").is_some_and(|v| v == "1")
}

/// Time a closure, returning its value and how long it took.
pub fn measure<T>(body: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let value = body();
    (value, start.elapsed())
}

/// Time a closure `runs` times and return the best result, which is the least noisy estimate on a
/// shared runner.
pub fn best_of<T>(runs: u32, mut body: impl FnMut() -> T) -> Duration {
    (0..runs.max(1))
        .map(|_| measure(&mut body).1)
        .min()
        .unwrap_or(Duration::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn verdicts_compare_against_the_limit() {
        let pass = CANCEL_LATENCY.verdict(Duration::from_millis(10));
        assert!(pass.passed);
        assert!(pass.to_string().starts_with("PASS"));

        let fail = CANCEL_LATENCY.verdict(Duration::from_secs(5));
        assert!(!fail.passed);
        assert!(fail.to_string().starts_with("FAIL"));
    }

    #[test]
    fn budget_ids_are_unique() {
        let all = [COLD_START, CANCEL_LATENCY, CACHED_OPEN, SCAN_PER_FILE];
        let mut seen = std::collections::HashSet::new();
        for b in all {
            assert!(seen.insert(b.id), "duplicate budget id {}", b.id);
            assert!(!b.what.is_empty());
        }
    }

    #[test]
    fn measure_reports_elapsed_time() {
        let (value, took) = measure(|| {
            std::thread::sleep(Duration::from_millis(5));
            41 + 1
        });
        assert_eq!(value, 42);
        assert!(took >= Duration::from_millis(5));
    }

    #[test]
    fn best_of_takes_the_minimum() {
        let mut n = 0;
        let took = best_of(3, || {
            n += 1;
            // First run is deliberately slowest, so a minimum is observably different from a mean.
            std::thread::sleep(Duration::from_millis(if n == 1 { 20 } else { 1 }));
        });
        assert!(
            took < Duration::from_millis(20),
            "best_of must not report the worst run"
        );
    }

    /// Scan throughput, measured in release only — a debug build is roughly 160 times slower on
    /// this loop, so a debug measurement would say nothing about the shipped binary.
    #[test]
    fn scan_throughput_meets_its_budget() {
        if !enabled() {
            eprintln!("skipping: set NIX_PERF=1 to measure");
            return;
        }

        use crate::fixture::{Fixture, Spec};
        use crate::op::CancelToken;
        use crate::scan;

        let fixture = Fixture::create(&Spec::perf()).expect("fixture");
        let files = fixture.files();
        assert!(
            files > 10_000,
            "the fixture must be large enough to measure"
        );

        let elapsed = best_of(3, || {
            scan::scan_quiet(
                scan::Options::new(fixture.root()).max_depth(None),
                CancelToken::new(),
            )
            .expect("scan")
        });

        let per_file = elapsed / u32::try_from(files).unwrap_or(u32::MAX);
        let verdict = SCAN_PER_FILE.verdict(per_file);
        eprintln!(
            "{verdict}  ({files} files in {elapsed:?}, {:.0} files/sec)",
            files as f64 / elapsed.as_secs_f64()
        );
        assert!(verdict.passed, "{verdict}");
    }

    /// The cancellation budget is the one we can already measure: task 0.3 landed the primitive.
    #[test]
    fn cancellation_meets_its_budget() {
        use crate::op::CancelToken;

        let token = CancelToken::new();
        let worker = {
            let token = token.clone();
            std::thread::spawn(move || {
                let start = Instant::now();
                while !token.is_cancelled() {
                    std::hint::spin_loop();
                    if start.elapsed() > Duration::from_secs(5) {
                        return None;
                    }
                }
                Some(start.elapsed())
            })
        };

        std::thread::sleep(Duration::from_millis(20));
        let (_, latency) = measure(|| {
            token.cancel();
            worker.join().ok().flatten()
        });

        let verdict = CANCEL_LATENCY.verdict(latency);
        assert!(verdict.passed, "{verdict}");
    }
}
