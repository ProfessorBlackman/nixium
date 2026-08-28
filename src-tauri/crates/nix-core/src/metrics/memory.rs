// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Memory from `/proc/meminfo`. `MON-1`.
//!
//! # Parsed as a map, which is the whole point
//!
//! §P8 exists partly because of this file. Stacer read `/proc/meminfo` **positionally** — line 0 is
//! total, line 1 is free, and so on — which is wrong the moment a kernel adds a field or reorders
//! one, and wrong silently. The numbers stay plausible; they just describe something else.
//!
//! `/proc/meminfo` is a map. It is read as one, and a field that is absent is absent rather than
//! being whatever happened to be on that line.
//!
//! # Matching `free`
//!
//! `MON-1`'s acceptance criterion is that memory figures cross-check against `free`, so the arithmetic
//! here is `free`'s, verified against it on a live machine to the byte rather than reimplemented from
//! a description:
//!
//! ```text
//! buff/cache = Buffers + Cached + SReclaimable
//! used       = MemTotal - MemFree - buff/cache
//! available  = MemAvailable        (the kernel's own estimate, not a derivation)
//! ```
//!
//! The last line matters most. **Available is not free plus caches.** That approximation was correct
//! before Linux 3.14 and has been wrong since, because not all cache is reclaimable and some
//! non-cache is. The kernel computes `MemAvailable` precisely so that nobody has to guess, and a tool
//! that guesses anyway will overstate what a machine can give back.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Memory as `free` would report it, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MemoryReading {
    #[ts(type = "number")]
    pub total: u64,
    /// In use: total, less free, less what the caches would give back.
    #[ts(type = "number")]
    pub used: u64,
    /// Genuinely unallocated. Almost always small on a healthy machine, and not the useful figure.
    #[ts(type = "number")]
    pub free: u64,
    /// What could be given to a new process without swapping. **The figure that matters.**
    #[ts(type = "number")]
    pub available: u64,
    /// Buffers plus page cache plus reclaimable slab, as `free` groups them.
    #[ts(type = "number")]
    pub buffers_cache: u64,
    /// Shared memory, which is counted inside the cache figure rather than beside it.
    #[ts(type = "number")]
    pub shared: u64,
    #[ts(type = "number")]
    pub swap_total: u64,
    #[ts(type = "number")]
    pub swap_used: u64,
    #[ts(type = "number")]
    pub swap_free: u64,
}

impl MemoryReading {
    /// Fraction of memory in use, `0.0..=1.0`.
    ///
    /// Computed from **available**, not from `used`, because that is the question a user is asking:
    /// how much of this machine is left. `None` when the total is zero, which would otherwise divide.
    #[must_use]
    pub fn pressure(&self) -> Option<f32> {
        if self.total == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        Some((1.0 - (self.available as f32 / self.total as f32)).clamp(0.0, 1.0))
    }

    /// Fraction of swap in use, or `None` when there is no swap.
    ///
    /// `None` rather than zero, because "no swap configured" and "swap entirely free" are different
    /// facts and a dashboard showing 0% for both is lying about one of them.
    #[must_use]
    pub fn swap_pressure(&self) -> Option<f32> {
        if self.swap_total == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        Some((self.swap_used as f32 / self.swap_total as f32).clamp(0.0, 1.0))
    }
}

/// Parse `/proc/meminfo` into a map of field name to bytes.
///
/// Values are `kB` in the file — which the kernel writes as `kB` and means **KiB**, 1024 bytes, a
/// naming error old enough to be load-bearing. Converted here so nothing downstream has to remember.
#[must_use]
pub(crate) fn parse_meminfo(text: &str) -> HashMap<String, u64> {
    text.lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let mut fields = rest.split_whitespace();
            let value: u64 = fields.next()?.parse().ok()?;
            // A field with no unit is a count, not a size — `HugePages_Total` for instance.
            let bytes = match fields.next() {
                Some("kB") => value.checked_mul(1024)?,
                _ => value,
            };
            Some((name.trim().to_string(), bytes))
        })
        .collect()
}

/// Build a reading from the map, using `free`'s arithmetic.
#[must_use]
pub(crate) fn reading_from(fields: &HashMap<String, u64>) -> MemoryReading {
    let get = |name: &str| -> u64 { fields.get(name).copied().unwrap_or(0) };

    let total = get("MemTotal");
    let free = get("MemFree");
    // `free` groups these three and calls the result buff/cache.
    let buffers_cache = get("Buffers") + get("Cached") + get("SReclaimable");

    let swap_total = get("SwapTotal");
    let swap_free = get("SwapFree");

    MemoryReading {
        total,
        // Saturating, because the three components are read at slightly different moments by the
        // kernel and can momentarily exceed the total. A wrapped subtraction would report sixteen
        // exabytes in use.
        used: total.saturating_sub(free).saturating_sub(buffers_cache),
        free,
        available: get("MemAvailable"),
        buffers_cache,
        shared: get("Shmem"),
        swap_total,
        swap_free,
        swap_used: swap_total.saturating_sub(swap_free),
    }
}

/// Read and parse `/proc/meminfo`.
///
/// Stateless: the kernel reports levels, not counters, so there is no delta to own.
#[must_use]
pub fn sample() -> Option<MemoryReading> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    Some(reading_from(&parse_meminfo(&text)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Golden file, §P8. Captured from this machine at the same instant as the `free -b` output the
    /// expectations below are taken from.
    const REAL_MEMINFO: &str = "\
MemTotal:       32636716 kB
MemFree:         1455408 kB
MemAvailable:   19708324 kB
Buffers:         4616876 kB
Cached:         10171592 kB
SwapCached:        62200 kB
Active:         10864432 kB
Inactive:       13774632 kB
SwapTotal:       2097148 kB
SwapFree:         316128 kB
Shmem:           1251528 kB
SReclaimable:    5247952 kB
HugePages_Total:       0
HugePages_Free:        0
Hugepagesize:       2048 kB
";

    /// The acceptance criterion: these figures are `free`'s, to the byte.
    ///
    /// Taken from `free -b` run against the same `/proc/meminfo` contents:
    ///
    /// ```text
    ///        total        used        free      shared  buff/cache   available
    /// Mem:   33419997184 11412365312 1490337792 1281564672 20517294080 20181323776
    /// Swap:   2147479552  1823764480  323715072
    /// ```
    #[test]
    fn the_figures_match_free_to_the_byte() {
        let reading = reading_from(&parse_meminfo(REAL_MEMINFO));

        assert_eq!(reading.total, 33_419_997_184);
        assert_eq!(reading.used, 11_412_365_312);
        assert_eq!(reading.free, 1_490_337_792);
        assert_eq!(reading.shared, 1_281_564_672);
        assert_eq!(reading.buffers_cache, 20_517_294_080);
        assert_eq!(reading.available, 20_181_323_776);

        assert_eq!(reading.swap_total, 2_147_479_552);
        assert_eq!(reading.swap_used, 1_823_764_480);
        assert_eq!(reading.swap_free, 323_715_072);
    }

    /// # Regression
    ///
    /// Stacer read this file by line index. A kernel that adds a field — and they do — shifts
    /// everything below it, and every number stays plausible while describing something else.
    #[test]
    fn fields_are_found_by_name_however_the_file_is_ordered() {
        let shuffled = "\
SReclaimable:    5247952 kB
Shmem:           1251528 kB
SomethingNewInLinux7:  99999 kB
MemAvailable:   19708324 kB
Cached:         10171592 kB
MemTotal:       32636716 kB
Buffers:         4616876 kB
SwapFree:         316128 kB
MemFree:         1455408 kB
SwapTotal:       2097148 kB
";
        let reading = reading_from(&parse_meminfo(shuffled));
        assert_eq!(reading.total, 33_419_997_184);
        assert_eq!(reading.used, 11_412_365_312);
        assert_eq!(
            reading.available, 20_181_323_776,
            "a new field between the old ones must change nothing"
        );
    }

    /// **Available is not free plus caches.** That approximation predates Linux 3.14.
    #[test]
    fn available_is_the_kernels_own_estimate_and_not_a_derivation() {
        let reading = reading_from(&parse_meminfo(REAL_MEMINFO));
        let naive = reading.free + reading.buffers_cache;
        assert_ne!(
            reading.available, naive,
            "free + caches is not what a machine can actually give back"
        );
        assert!(
            reading.available < naive,
            "not all cache is reclaimable, so the naive figure overstates by {} bytes",
            naive - reading.available
        );
    }

    #[test]
    fn a_field_without_a_unit_is_a_count_not_a_size() {
        let fields = parse_meminfo(REAL_MEMINFO);
        assert_eq!(fields.get("HugePages_Total").copied(), Some(0));
        assert_eq!(
            fields.get("Hugepagesize").copied(),
            Some(2048 * 1024),
            "this one does carry kB"
        );
    }

    #[test]
    fn a_missing_field_is_zero_rather_than_a_neighbours_value() {
        let reading = reading_from(&parse_meminfo("MemTotal: 1024 kB\n"));
        assert_eq!(reading.total, 1_048_576);
        assert_eq!(reading.available, 0);
        assert_eq!(reading.buffers_cache, 0);
    }

    #[test]
    fn pressure_is_computed_from_available_not_from_used() {
        let reading = reading_from(&parse_meminfo(REAL_MEMINFO));
        let pressure = reading.pressure().unwrap();
        // 1 - 20181323776/33419997184 = 0.396...
        assert!((pressure - 0.396).abs() < 0.01, "{pressure}");

        // And a used-based figure would be a very different number.
        #[allow(clippy::cast_precision_loss)]
        let from_used = reading.used as f32 / reading.total as f32;
        assert!(
            (from_used - pressure).abs() > 0.05,
            "the two differ enough that picking the wrong one matters"
        );
    }

    /// No swap and empty swap are different facts.
    #[test]
    fn no_swap_reports_nothing_rather_than_zero_percent() {
        let none = reading_from(&parse_meminfo("MemTotal: 1024 kB\n"));
        assert_eq!(none.swap_pressure(), None);

        let empty = reading_from(&parse_meminfo(
            "MemTotal: 1024 kB\nSwapTotal: 512 kB\nSwapFree: 512 kB\n",
        ));
        assert_eq!(empty.swap_pressure(), Some(0.0));
    }

    #[test]
    fn an_empty_file_divides_by_nothing() {
        let reading = reading_from(&parse_meminfo(""));
        assert_eq!(reading.pressure(), None);
        assert_eq!(reading.swap_pressure(), None);
    }

    /// The components are read at slightly different moments and can momentarily exceed the total.
    #[test]
    fn an_inconsistent_instant_does_not_wrap_to_exabytes() {
        let reading = reading_from(&parse_meminfo(
            "MemTotal: 100 kB\nMemFree: 80 kB\nBuffers: 80 kB\n",
        ));
        assert_eq!(reading.used, 0, "saturating, not wrapping");
    }

    // ---- against this machine ----

    #[test]
    fn this_machines_memory_is_internally_consistent() {
        let Some(reading) = sample() else { return };

        assert!(reading.total > 0, "a running machine has memory");
        assert_eq!(
            reading.total,
            reading.used + reading.free + reading.buffers_cache,
            "free's three columns must add up to the total"
        );
        assert!(reading.available <= reading.total);
        assert!(
            reading.free <= reading.available,
            "available includes what is free"
        );
        assert!(reading.swap_used + reading.swap_free == reading.swap_total);
        assert!(reading.pressure().is_some_and(|p| (0.0..=1.0).contains(&p)));
    }
}
