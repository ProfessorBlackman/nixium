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

/// A budget measured in bytes rather than time.
///
/// Separate from [`Budget`] rather than folded into it with an enum: the two are compared, formatted
/// and reasoned about differently, and a single type covering both would need a unit tag that could
/// disagree with the value it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteBudget {
    pub id: &'static str,
    pub what: &'static str,
    pub limit: u64,
}

impl ByteBudget {
    /// Check a measurement, formatted for a test failure message.
    #[must_use]
    pub fn verdict(&self, measured: u64) -> String {
        format!(
            "{} {}: {} against a {} budget — {}",
            if measured <= self.limit {
                "PASS"
            } else {
                "FAIL"
            },
            self.id,
            crate::format_bytes(measured),
            crate::format_bytes(self.limit),
            self.what
        )
    }

    #[must_use]
    pub const fn passes(&self, measured: u64) -> bool {
        measured <= self.limit
    }
}

/// Resident memory in the steady state.
pub const MEMORY_STEADY: ByteBudget = ByteBudget {
    id: "memory_steady",
    what: "resident memory, steady state",
    limit: 150 * 1024 * 1024,
};

/// A budget on a fraction — a share of one core, in this case.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateBudget {
    pub id: &'static str,
    pub what: &'static str,
    /// The ceiling, as a fraction of one core. `0.01` is one percent.
    pub limit: f64,
}

impl RateBudget {
    #[must_use]
    pub fn verdict(&self, measured: f64) -> String {
        format!(
            "{} {}: {:.4}% against a {:.2}% budget — {}",
            if measured <= self.limit {
                "PASS"
            } else {
                "FAIL"
            },
            self.id,
            measured * 100.0,
            self.limit * 100.0,
            self.what
        )
    }
}

/// Idle CPU with a monitoring view mounted.
pub const IDLE_CPU_MONITORING: RateBudget = RateBudget {
    id: "idle_cpu_monitoring",
    what: "idle CPU, window open, monitoring view mounted",
    limit: 0.01,
};

/// Resident memory of this process, in bytes, from `/proc/self/statm`.
///
/// Field two is the resident set in pages. `statm` rather than `status`: it is a single line of
/// integers, so there is no risk of reading a field by position from a file whose layout varies —
/// which is the defect §P8 exists because of.
#[must_use]
pub fn resident_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    // `PAGE_SIZE` is 4096 on every platform nix targets, and the same constant `process.rs` uses.
    Some(pages.saturating_mul(4096))
}

/// This process's CPU time so far, in seconds, from `/proc/self/stat`.
///
/// Fields 14 and 15 are utime and stime in clock ticks. Read by splitting on the **last** `)`, for the
/// same reason `process.rs` does: a process name can contain spaces and brackets, so splitting on
/// whitespace from the start puts every later field in the wrong place.
///
/// # This is the whole process, which is a trap
///
/// It counts every thread, so measuring an idle share with it only means anything when nothing else in
/// the process is working. Inside `cargo test` that is false — the harness runs tests in parallel
/// threads — and the first version of the idle-CPU budget lived in the lib tests and reported **196%
/// of one core**, which was the rest of the suite. That measurement now lives in
/// `tests/idle_cpu.rs`, a binary with one test in it.
#[must_use]
pub fn process_cpu_seconds() -> f64 {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
        return 0.0;
    };
    let Some(after) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
        return 0.0;
    };
    let fields: Vec<&str> = after.split_whitespace().collect();
    // `after` starts at field 3, so utime and stime are indices 11 and 12.
    let utime: f64 = fields.get(11).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let stime: f64 = fields.get(12).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    (utime + stime) / 100.0
}

/// One row of the specification's §7.3 table, and what holds it to account.
#[derive(Debug, Clone, Copy)]
pub struct SpecRow {
    /// The spec's own wording for the budget, so drift is visible.
    pub target: &'static str,
    pub coverage: Coverage,
}

/// Whether a §7.3 row is actually asserted anywhere, and if not, why not.
///
/// The point of naming the gaps is that a budget table nobody checks is a wish list. Writing
/// `NotAsserted` with a reason is a claim someone can disagree with; leaving the row out entirely is
/// not.
#[derive(Debug, Clone, Copy)]
pub enum Coverage {
    /// Asserted by the named test, against a budget in this module.
    Asserted { by: &'static str },
    /// Asserted by a named test elsewhere in the codebase.
    AssertedElsewhere { by: &'static str },
    /// Not asserted. The reason is part of the record.
    NotAsserted { why: &'static str },
}

/// Every row of §7.3, in the order the specification lists them.
///
/// Kept beside the budgets rather than only in prose, and checked against `docs/SPEC.md` by
/// [`tests::every_spec_budget_row_is_accounted_for`] — so adding a row to the spec without deciding
/// how it will be held to account fails the build.
pub const SPEC_ROWS: &[SpecRow] = &[
    SpecRow {
        target: "Cold start to interactive",
        coverage: Coverage::NotAsserted {
            why: "measured by the app shell, which needs a display server; the CI runner is headless",
        },
    },
    SpecRow {
        target: "Idle CPU, window open, monitoring view mounted",
        coverage: Coverage::AssertedElsewhere {
            by: "tests/idle_cpu.rs — its own binary, because a process-wide CPU reading inside the \
                 parallel test harness measures the harness",
        },
    },
    SpecRow {
        target: "Idle CPU, hidden in tray, no alerts armed",
        coverage: Coverage::AssertedElsewhere {
            by: "metrics::tests::nothing_is_sampled_until_something_subscribes",
        },
    },
    SpecRow {
        target: "Resident memory, steady state",
        coverage: Coverage::AssertedElsewhere {
            by: "tests/steady_memory.rs — its own binary, since `/proc/self/statm` is process-wide \
                 and the parallel harness's own allocations counted toward it",
        },
    },
    SpecRow {
        target: "Subprocess spawns in the steady-state monitoring loop",
        coverage: Coverage::AssertedElsewhere {
            by: "metrics::tests::the_sampling_loop_spawns_no_subprocesses",
        },
    },
    SpecRow {
        target: "Space explorer, first useful paint",
        coverage: Coverage::AssertedElsewhere {
            by: "scan::tests — via SCAN_PER_FILE, since first paint is the scan plus a render",
        },
    },
    SpecRow {
        target: "Full scan, 2 M files",
        coverage: Coverage::Asserted {
            by: "budget::tests::scan_throughput_meets_its_budget",
        },
    },
    SpecRow {
        target: "Incremental rescan, unchanged tree",
        coverage: Coverage::NotAsserted {
            why: "STO-18 was superseded by STO-19; there is no incremental rescan to measure",
        },
    },
    SpecRow {
        target: "Service inventory, 400 units",
        coverage: Coverage::AssertedElsewhere {
            by: "units::tests::the_inventory_meets_its_budget",
        },
    },
    SpecRow {
        target: "Cancellation latency, any streaming op",
        coverage: Coverage::Asserted {
            by: "budget::tests::cancellation_meets_its_budget",
        },
    },
];

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

    /// # The drift guard
    ///
    /// A budget table nobody checks is a wish list. This reads §7.3 out of `docs/SPEC.md` and requires
    /// one [`SpecRow`] per row, matched on the spec's own wording — so adding a budget to the
    /// specification without deciding how it will be held to account fails the build, and so does
    /// rewording a row without updating the registry.
    #[test]
    fn every_spec_budget_row_is_accounted_for() {
        let spec = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/SPEC.md");
        let Ok(text) = std::fs::read_to_string(&spec) else {
            // Reading the spec from a test is only possible in the source tree. A packaged build has
            // no docs directory, and failing there would be failing for the wrong reason.
            return;
        };

        // The §7.3 table: everything between its heading and the next one.
        let after = text
            .split_once("### 7.3 Performance budgets")
            .expect("the spec has a §7.3")
            .1;
        let table = after.split_once("### ").map_or(after, |(before, _)| before);

        let targets: Vec<&str> = table
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let inner = line.strip_prefix('|')?.strip_suffix('|')?;
                let first = inner.split('|').next()?.trim();
                // Skip the header row and the `| --- |` separator.
                if first.is_empty() || first == "Budget" || first.starts_with("---") {
                    return None;
                }
                Some(first)
            })
            .collect();

        assert!(
            targets.len() >= 8,
            "only {} budget rows parsed from §7.3, which cannot be right: {targets:?}",
            targets.len()
        );

        for target in &targets {
            assert!(
                SPEC_ROWS.iter().any(|row| row.target == *target),
                "§7.3 has a budget with no entry in SPEC_ROWS: {target:?}\n\
                 Add one, with either an assertion or a stated reason there is none."
            );
        }

        for row in SPEC_ROWS {
            assert!(
                targets.contains(&row.target),
                "SPEC_ROWS names {:?}, which is no longer a row in §7.3 — the spec was reworded",
                row.target
            );
        }
    }

    /// Every row is decided one way or the other, and the reasons are readable rather than a shrug.
    #[test]
    fn no_spec_row_is_left_undecided() {
        for row in SPEC_ROWS {
            match row.coverage {
                Coverage::Asserted { by } | Coverage::AssertedElsewhere { by } => {
                    assert!(
                        !by.is_empty(),
                        "{} claims an assertion with no name",
                        row.target
                    );
                }
                Coverage::NotAsserted { why } => assert!(
                    why.len() > 20,
                    "{} is unasserted with a reason too short to be one: {why:?}",
                    row.target
                ),
            }
        }
    }

    // ---- the two budgets that were only ever measured by hand ----

    #[test]
    fn resident_memory_is_readable() {
        let bytes = resident_bytes().expect("/proc/self/statm exists on Linux");
        assert!(
            bytes > 1024 * 1024,
            "a running test process uses more than a megabyte, got {bytes}"
        );
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
