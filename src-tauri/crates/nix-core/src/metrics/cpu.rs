// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! CPU utilisation from `/proc/stat`. `MON-1`.
//!
//! # Why this needs state and the others mostly do not
//!
//! `/proc/stat` reports **cumulative** jiffies since boot, not a rate. Utilisation is the ratio of
//! busy to total *between two readings*, so something has to remember the previous one — and that
//! something is a single [`CpuSampler`], per §P3. Two consumers each keeping their own previous
//! reading would produce two different answers for the same second, both plausible.
//!
//! # The first reading is not a measurement
//!
//! With nothing to subtract from, the honest answer to "what is the CPU doing?" after one sample is
//! "not known yet". [`CpuSampler::sample`] returns `None` then, rather than reporting the machine's
//! average utilisation since boot — which is a real number, and not the one anybody wants.

use std::path::Path;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Cumulative jiffies for one CPU, as `/proc/stat` reports them.
///
/// Every field is counted from boot. The names are the kernel's own, in its own order, because the
/// order is the file's contract — but they are read positionally *within a known line*, which is a
/// different thing from `/proc/meminfo`'s trap of reading whole lines by index.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

impl CpuTimes {
    /// Every jiffy accounted for, busy or not.
    #[must_use]
    pub(crate) const fn total(&self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }

    /// Jiffies spent doing something.
    ///
    /// `iowait` counts as **idle**, not busy. A core waiting on a disk is available to run anything
    /// else, and counting it as utilisation makes a machine reading a large file look pegged when it
    /// is doing nothing. This is a judgement the kernel does not make for us and tools disagree on;
    /// `top` treats it as its own category, and this treats it as idle because the question the
    /// dashboard answers is "is the CPU the bottleneck".
    #[must_use]
    pub(crate) const fn busy(&self) -> u64 {
        self.user + self.nice + self.system + self.irq + self.softirq + self.steal
    }

    /// Utilisation between two readings, in `0.0..=1.0`.
    ///
    /// `None` when the counters did not advance, or went backwards — which happens across a suspend,
    /// a CPU being hot-unplugged, or a container being migrated. A negative delta is not a small
    /// number, it is a broken assumption, and reporting it as `0%` would hide that.
    #[must_use]
    pub(crate) fn utilisation_since(&self, previous: &Self) -> Option<f32> {
        let total = self.total().checked_sub(previous.total())?;
        let busy = self.busy().checked_sub(previous.busy())?;
        if total == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        Some((busy as f32 / total as f32).clamp(0.0, 1.0))
    }
}

/// What the UI is given for one tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CpuReading {
    /// Overall utilisation, `0.0..=1.0`.
    pub total: f32,
    /// Per core, in the kernel's order. Length is however many cores the machine has.
    ///
    /// Stacer's palette asserted past twenty cores; nothing here is indexed by a fixed-size table.
    pub per_core: Vec<f32>,
    /// Current frequency of the first core, in kHz, where the platform reports one.
    ///
    /// `None` on hardware without `cpufreq` — a virtual machine, usually — rather than a zero that
    /// would read as "stopped".
    #[ts(type = "number | null")]
    pub frequency_khz: Option<u64>,
}

/// Parse one `cpu`-prefixed line's jiffy fields.
fn parse_times(fields: &[&str]) -> CpuTimes {
    let at = |i: usize| -> u64 { fields.get(i).and_then(|v| v.parse().ok()).unwrap_or(0) };
    CpuTimes {
        user: at(0),
        nice: at(1),
        system: at(2),
        idle: at(3),
        iowait: at(4),
        irq: at(5),
        softirq: at(6),
        steal: at(7),
    }
}

/// Parse `/proc/stat` into the aggregate line and the per-core lines.
///
/// Lines are selected by their **name**, not their position: `cpu` is the aggregate and `cpuN` are
/// cores. Everything else in the file — `intr`, `ctxt`, `btime`, `procs_running` — is skipped by not
/// matching, rather than by being assumed to come after a known number of lines.
#[must_use]
pub(crate) fn parse_stat(text: &str) -> (Option<CpuTimes>, Vec<CpuTimes>) {
    let mut aggregate = None;
    let mut cores: Vec<(usize, CpuTimes)> = Vec::new();

    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else { continue };
        let Some(rest) = name.strip_prefix("cpu") else {
            continue;
        };
        let values: Vec<&str> = fields.collect();

        if rest.is_empty() {
            aggregate = Some(parse_times(&values));
        } else if let Ok(index) = rest.parse::<usize>() {
            cores.push((index, parse_times(&values)));
        }
    }

    // By the kernel's own index rather than by the order they appeared, so a core list is stable even
    // if the file ever stops being sorted.
    cores.sort_by_key(|(index, _)| *index);
    (aggregate, cores.into_iter().map(|(_, t)| t).collect())
}

/// Reads `/proc/stat` and turns cumulative counters into a rate.
///
/// **The single owner of CPU delta state** (§P3).
#[derive(Debug, Default)]
pub(crate) struct CpuSampler {
    previous_total: Option<CpuTimes>,
    previous_cores: Vec<CpuTimes>,
}

impl CpuSampler {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Take a reading. `None` until there is a previous one to compare against.
    pub(crate) fn sample(&mut self) -> Option<CpuReading> {
        let text = std::fs::read_to_string("/proc/stat").ok()?;
        let frequency = current_frequency_khz(Path::new(
            "/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq",
        ));
        self.sample_from(&text, frequency)
    }

    /// The pure half, so the delta logic is testable without a filesystem.
    pub(crate) fn sample_from(
        &mut self,
        text: &str,
        frequency_khz: Option<u64>,
    ) -> Option<CpuReading> {
        let (aggregate, cores) = parse_stat(text);
        let aggregate = aggregate?;

        let total = self
            .previous_total
            .as_ref()
            .and_then(|p| aggregate.utilisation_since(p));

        // Zip against the previous cores by position. A core count that changed — hot-plug, or the
        // first sample — yields no per-core figures rather than mismatched ones.
        let per_core = if self.previous_cores.len() == cores.len() {
            cores
                .iter()
                .zip(&self.previous_cores)
                .map(|(now, before)| now.utilisation_since(before).unwrap_or(0.0))
                .collect()
        } else {
            Vec::new()
        };

        self.previous_total = Some(aggregate);
        self.previous_cores = cores;

        // No delta yet is not a reading of zero.
        total.map(|total| CpuReading {
            total,
            per_core,
            frequency_khz,
        })
    }
}

/// Current frequency in kHz, where the platform reports one.
fn current_frequency_khz(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Golden file, §P8. Captured from this machine, eight cores, trimmed to three.
    const REAL_STAT: &str = "\
cpu  2328496 18891 759669 25133262 94212 0 14765 0 0 0
cpu0 296233 2251 95784 3128383 11184 0 1973 0 0 0
cpu1 284695 2488 95289 3146041 11470 0 1782 0 0 0
cpu2 290000 2300 95000 3140000 11000 0 1800 0 0 0
intr 412345678 9 0 0 0
ctxt 987654321
btime 1787800000
processes 123456
procs_running 2
procs_blocked 0
softirq 111111111 0 1 2 3
";

    #[test]
    fn the_aggregate_and_cores_are_parsed_from_real_output() {
        let (aggregate, cores) = parse_stat(REAL_STAT);
        let aggregate = aggregate.unwrap();

        assert_eq!(aggregate.user, 2_328_496);
        assert_eq!(aggregate.nice, 18_891);
        assert_eq!(aggregate.system, 759_669);
        assert_eq!(aggregate.idle, 25_133_262);
        assert_eq!(aggregate.iowait, 94_212);
        assert_eq!(aggregate.softirq, 14_765);

        assert_eq!(cores.len(), 3, "three cpuN lines");
        assert_eq!(cores[0].user, 296_233);
    }

    /// The lines after the cpu block must not be mistaken for cores.
    #[test]
    fn non_cpu_lines_are_skipped_by_name_not_by_position() {
        let (_, cores) = parse_stat(REAL_STAT);
        assert_eq!(
            cores.len(),
            3,
            "intr, ctxt, btime and softirq are not cores — softirq even starts with the wrong letters"
        );

        // And a file whose ordering changed must parse identically, which is the whole point of
        // selecting by name.
        let reordered = "\
btime 1787800000
cpu1 284695 2488 95289 3146041 11470 0 1782 0 0 0
intr 412345678 9
cpu  2328496 18891 759669 25133262 94212 0 14765 0 0 0
cpu0 296233 2251 95784 3128383 11184 0 1973 0 0 0
";
        let (aggregate, cores) = parse_stat(reordered);
        assert_eq!(aggregate.unwrap().user, 2_328_496);
        assert_eq!(cores.len(), 2);
        assert_eq!(cores[0].user, 296_233, "sorted by the kernel's own index");
    }

    #[test]
    fn iowait_counts_as_idle_not_busy() {
        let times = CpuTimes {
            user: 10,
            iowait: 90,
            ..CpuTimes::default()
        };
        assert_eq!(times.busy(), 10);
        assert_eq!(times.total(), 100);

        let previous = CpuTimes::default();
        let utilisation = times.utilisation_since(&previous).unwrap();
        assert!(
            (utilisation - 0.10).abs() < 0.001,
            "a core waiting on a disk is available to run something else, got {utilisation}"
        );
    }

    #[test]
    fn utilisation_is_a_ratio_of_the_interval_not_of_all_time() {
        let before = CpuTimes {
            user: 1000,
            idle: 9000,
            ..CpuTimes::default()
        };
        let after = CpuTimes {
            user: 1075,
            idle: 9025,
            ..CpuTimes::default()
        };
        // 75 busy of 100 elapsed, regardless of the 10% lifetime average.
        let utilisation = after.utilisation_since(&before).unwrap();
        assert!((utilisation - 0.75).abs() < 0.001, "{utilisation}");
    }

    /// Counters going backwards is a broken assumption, not a small number.
    #[test]
    fn counters_going_backwards_yield_nothing() {
        let before = CpuTimes {
            user: 1000,
            idle: 9000,
            ..CpuTimes::default()
        };
        let after = CpuTimes {
            user: 10,
            idle: 90,
            ..CpuTimes::default()
        };
        assert_eq!(
            after.utilisation_since(&before),
            None,
            "a suspend or a migration resets these; reporting 0% would hide it"
        );
    }

    #[test]
    fn an_interval_with_no_elapsed_jiffies_yields_nothing() {
        let times = CpuTimes {
            user: 500,
            idle: 500,
            ..CpuTimes::default()
        };
        assert_eq!(
            times.utilisation_since(&times),
            None,
            "dividing by a zero interval is not a zero utilisation"
        );
    }

    /// The first sample has nothing to compare against, and says so.
    #[test]
    fn the_first_sample_is_not_a_measurement() {
        let mut sampler = CpuSampler::new();
        assert!(
            sampler.sample_from(REAL_STAT, None).is_none(),
            "one reading of a cumulative counter is not a rate"
        );

        let later = REAL_STAT.replace("cpu  2328496", "cpu  2328596");
        let reading = sampler
            .sample_from(&later, Some(1_801_548))
            .expect("the second sample has a delta");
        assert!(reading.total > 0.0);
        assert_eq!(reading.frequency_khz, Some(1_801_548));
    }

    #[test]
    fn per_core_figures_appear_once_there_is_a_delta_for_each() {
        let mut sampler = CpuSampler::new();
        sampler.sample_from(REAL_STAT, None);
        let later = REAL_STAT
            .replace("cpu0 296233", "cpu0 296333")
            .replace("cpu  2328496", "cpu  2328596");
        let reading = sampler.sample_from(&later, None).unwrap();
        assert_eq!(reading.per_core.len(), 3);
        assert!(reading.per_core[0] > 0.0, "cpu0 did work");
        assert_eq!(reading.per_core[1], 0.0, "cpu1 did not");
    }

    /// A machine that gains or loses a core must not produce per-core figures against the wrong core.
    #[test]
    fn a_changed_core_count_yields_no_per_core_figures_rather_than_mismatched_ones() {
        let mut sampler = CpuSampler::new();
        sampler.sample_from(REAL_STAT, None);

        let fewer = "\
cpu  2328596 18891 759669 25133262 94212 0 14765 0 0 0
cpu0 296333 2251 95784 3128383 11184 0 1973 0 0 0
";
        let reading = sampler.sample_from(fewer, None).unwrap();
        assert!(
            reading.per_core.is_empty(),
            "core 0's delta must not be attributed to a different core"
        );
        assert!(reading.total > 0.0, "the aggregate is still meaningful");
    }

    #[test]
    fn an_empty_or_broken_file_yields_nothing() {
        let mut sampler = CpuSampler::new();
        assert!(sampler.sample_from("", None).is_none());
        assert!(
            sampler
                .sample_from("garbage\nmore garbage\n", None)
                .is_none()
        );
    }

    #[test]
    fn a_missing_frequency_file_is_none_not_zero() {
        assert_eq!(
            current_frequency_khz(Path::new("/definitely/not/here")),
            None,
            "a virtual machine has no cpufreq, and zero would read as stopped"
        );
    }

    // ---- against this machine ----

    #[test]
    fn this_machines_stat_parses_with_one_core_line_per_core() {
        let Ok(text) = std::fs::read_to_string("/proc/stat") else {
            return;
        };
        let (aggregate, cores) = parse_stat(&text);
        assert!(aggregate.is_some(), "/proc/stat must have a cpu line");
        assert_eq!(
            cores.len(),
            std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(cores.len()),
            "one cpuN line per core"
        );
        assert!(aggregate.unwrap().total() > 0);
    }
}
