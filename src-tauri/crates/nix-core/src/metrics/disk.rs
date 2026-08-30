// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Disk throughput from `/sys/block/*/stat`. `MON-1`.
//!
//! # Which devices count
//!
//! This machine has **44 entries in `/sys/block` and one real disk**. The other 43 are `loop`
//! devices, one per installed snap. A monitor that charts all of them is charting nothing.
//!
//! The tempting filter is the `device` symlink, which the kernel creates for hardware and not for
//! loops. It is wrong, and wrong in the dangerous direction: `dm-*` (LVM, LUKS), `md*` (software
//! RAID) and `zram*` have no such link either, so an encrypted or RAID install — a very ordinary
//! Ubuntu setup — would show no disk activity at all.
//!
//! That is the same shape of mistake as widening a filesystem-type prefix to `fuse` and hiding every
//! NTFS volume, which this project made once already. So the rule is the narrow, safe one: exclude
//! the device classes that are definitionally not storage, and require a non-zero size.
//!
//! # Sectors are always 512 bytes here
//!
//! `/sys/block/*/stat` counts in 512-byte sectors regardless of the device's actual sector size —
//! this is a kernel ABI constant, not a property of the disk. Reading `queue/hw_sector_size` and
//! multiplying by that would be eight times wrong on a 4K-native drive.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The kernel reports `/sys/block/*/stat` in 512-byte units, always.
const SECTOR_BYTES: u64 = 512;

/// Device name prefixes that are never real storage.
///
/// A short, explicit list rather than a structural probe, because every structural signal tried
/// excluded something real. `loop` is reserved by the loop driver and `ram` by the ramdisk driver, so
/// neither can collide with a hardware device.
const NOT_STORAGE: &[&str] = &["loop", "ram"];

/// Cumulative counters for one block device.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DiskCounters {
    pub sectors_read: u64,
    pub sectors_written: u64,
}

impl DiskCounters {
    #[must_use]
    pub(crate) const fn bytes_read(&self) -> u64 {
        self.sectors_read * SECTOR_BYTES
    }

    #[must_use]
    pub(crate) const fn bytes_written(&self) -> u64 {
        self.sectors_written * SECTOR_BYTES
    }
}

/// Throughput over the last interval, in bytes per second.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DiskTotals {
    #[ts(type = "number")]
    pub read_per_second: u64,
    #[ts(type = "number")]
    pub written_per_second: u64,
}

/// What the UI is given for one tick.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DiskReading {
    /// Summed across every real device.
    pub totals: DiskTotals,
    /// Per device, so `MON-3` can break the chart down. Names are the kernel's.
    pub per_device: Vec<(String, DiskTotals)>,
}

/// Whether a `/sys/block` entry is worth counting.
///
/// `size` is in 512-byte sectors; zero means an unbacked loop or a card reader with no card.
#[must_use]
pub(crate) fn is_real_storage(name: &str, size_sectors: u64) -> bool {
    if size_sectors == 0 {
        return false;
    }
    !NOT_STORAGE.iter().any(|prefix| name.starts_with(prefix))
}

/// Parse one device's `stat` line.
///
/// Seventeen whitespace-separated fields. Only two are wanted, and they are taken by their documented
/// index *within this one line* — which is not the positional-parsing trap, because the line's shape
/// is the kernel's stable ABI rather than an accident of ordering between lines.
///
/// | Index | Meaning |
/// | --- | --- |
/// | 2 | sectors read |
/// | 6 | sectors written |
#[must_use]
pub(crate) fn parse_device_stat(text: &str) -> Option<DiskCounters> {
    let fields: Vec<&str> = text.split_whitespace().collect();
    // Fewer than seven fields is not a stat line, and guessing at a short one would invent traffic.
    if fields.len() < 7 {
        return None;
    }
    Some(DiskCounters {
        sectors_read: fields[2].parse().ok()?,
        sectors_written: fields[6].parse().ok()?,
    })
}

/// Reads `/sys/block`. **The single owner of disk delta state** (§P3).
#[derive(Debug, Default)]
pub(crate) struct DiskSampler {
    root: Option<PathBuf>,
    previous: HashMap<String, DiskCounters>,
}

impl DiskSampler {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A sampler rooted somewhere else, for tests.
    #[cfg(test)]
    #[must_use]
    fn rooted_at(root: PathBuf) -> Self {
        Self {
            root: Some(root),
            previous: HashMap::new(),
        }
    }

    fn root(&self) -> &Path {
        self.root
            .as_deref()
            .unwrap_or_else(|| Path::new("/sys/block"))
    }

    /// Every real device's counters, now.
    fn read_counters(&self) -> HashMap<String, DiskCounters> {
        let mut found = HashMap::new();
        let Ok(entries) = std::fs::read_dir(self.root()) else {
            return found;
        };

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let size: u64 = std::fs::read_to_string(entry.path().join("size"))
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            if !is_real_storage(&name, size) {
                continue;
            }
            if let Some(counters) = std::fs::read_to_string(entry.path().join("stat"))
                .ok()
                .and_then(|text| parse_device_stat(&text))
            {
                found.insert(name, counters);
            }
        }
        found
    }

    /// Take a reading. `None` until there is a previous one to subtract.
    pub(crate) fn sample(&mut self, elapsed: std::time::Duration) -> Option<DiskReading> {
        let now = self.read_counters();
        let first = self.previous.is_empty();

        let seconds = elapsed.as_secs_f64().max(f64::EPSILON);
        let mut per_device: Vec<(String, DiskTotals)> = Vec::new();
        let mut totals = DiskTotals::default();

        for (name, counters) in &now {
            let Some(before) = self.previous.get(name) else {
                // A device that appeared since the last tick — a USB stick — has no delta yet.
                continue;
            };
            // Counters can reset when a device is removed and re-added under the same name.
            let read = counters.bytes_read().checked_sub(before.bytes_read());
            let written = counters.bytes_written().checked_sub(before.bytes_written());
            let (Some(read), Some(written)) = (read, written) else {
                continue;
            };

            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let rate = DiskTotals {
                read_per_second: (read as f64 / seconds) as u64,
                written_per_second: (written as f64 / seconds) as u64,
            };
            totals.read_per_second += rate.read_per_second;
            totals.written_per_second += rate.written_per_second;
            per_device.push((name.clone(), rate));
        }

        self.previous = now;
        if first {
            return None;
        }

        per_device.sort_by(|a, b| a.0.cmp(&b.0));
        Some(DiskReading { totals, per_device })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Golden file, §P8. Captured from this machine's `nvme0n1`.
    const REAL_STAT: &str = " 4601473  1387578 78011750   699237  1287432  1433725 89087210  6020478        0   651845  6882982        0        0        0        0   134886   163266\n";

    #[test]
    fn a_device_stat_is_parsed_from_real_output() {
        let counters = parse_device_stat(REAL_STAT).unwrap();
        assert_eq!(counters.sectors_read, 78_011_750);
        assert_eq!(counters.sectors_written, 89_087_210);
        assert_eq!(counters.bytes_read(), 78_011_750 * 512);
        assert_eq!(counters.bytes_written(), 89_087_210 * 512);
    }

    #[test]
    fn a_short_line_yields_nothing_rather_than_inventing_traffic() {
        assert!(parse_device_stat("1 2 3").is_none());
        assert!(parse_device_stat("").is_none());
        assert!(parse_device_stat("a b c d e f g").is_none());
    }

    /// This machine: 43 loop devices, one real disk.
    #[test]
    fn loop_devices_are_not_storage() {
        assert!(!is_real_storage("loop0", 8));
        assert!(!is_real_storage("loop42", 1_000_000));
        assert!(is_real_storage("nvme0n1", 1_000_215_216));
    }

    /// # Regression risk
    ///
    /// The `device` symlink looks like the right structural filter and excludes every one of these,
    /// which on an encrypted or RAID install means the dashboard shows no disk at all. This is the
    /// `fuseblk` mistake in another costume.
    #[test]
    fn mapped_raid_and_compressed_devices_are_storage() {
        for name in ["dm-0", "dm-15", "md0", "md127", "zram0"] {
            assert!(
                is_real_storage(name, 1_000_000),
                "{name} is real storage — LUKS, LVM, RAID and zram all lack a `device` link"
            );
        }
    }

    #[test]
    fn a_device_with_no_size_is_skipped() {
        assert!(
            !is_real_storage("sdb", 0),
            "an empty card reader is not a disk"
        );
    }

    #[test]
    fn a_name_merely_containing_a_prefix_is_still_storage() {
        assert!(
            is_real_storage("nvme0n1-loopback", 1024),
            "the prefix rule must anchor at the start"
        );
    }

    fn sandbox(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nix-disk-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn device(root: &Path, name: &str, size: u64, read: u64, written: u64) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("size"), size.to_string()).unwrap();
        std::fs::write(
            dir.join("stat"),
            format!("1 2 {read} 4 5 6 {written} 8 9 10 11 12 13 14 15 16 17"),
        )
        .unwrap();
    }

    #[test]
    fn the_first_sample_is_not_a_measurement() {
        let root = sandbox("first");
        device(&root, "sda", 1000, 100, 200);

        let mut sampler = DiskSampler::rooted_at(root.clone());
        assert!(
            sampler.sample(std::time::Duration::from_secs(1)).is_none(),
            "one reading of a cumulative counter is not a rate"
        );

        device(&root, "sda", 1000, 1100, 2200);
        let reading = sampler.sample(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(reading.totals.read_per_second, 1000 * 512);
        assert_eq!(reading.totals.written_per_second, 2000 * 512);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn loops_do_not_reach_the_totals() {
        let root = sandbox("loops");
        device(&root, "nvme0n1", 1000, 0, 0);
        for i in 0..43 {
            device(&root, &format!("loop{i}"), 8, 0, 0);
        }

        let mut sampler = DiskSampler::rooted_at(root.clone());
        sampler.sample(std::time::Duration::from_secs(1));
        device(&root, "nvme0n1", 1000, 100, 100);
        for i in 0..43 {
            device(&root, &format!("loop{i}"), 8, 9999, 9999);
        }

        let reading = sampler.sample(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(
            reading.per_device.len(),
            1,
            "one real disk among forty-four"
        );
        assert_eq!(reading.per_device[0].0, "nvme0n1");
        assert_eq!(reading.totals.read_per_second, 100 * 512);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_counter_reset_is_skipped_rather_than_reported_as_a_burst() {
        let root = sandbox("reset");
        device(&root, "sda", 1000, 10_000, 10_000);
        let mut sampler = DiskSampler::rooted_at(root.clone());
        sampler.sample(std::time::Duration::from_secs(1));

        // Removed and re-added under the same name: counters start again.
        device(&root, "sda", 1000, 5, 5);
        let reading = sampler.sample(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(
            reading.totals.read_per_second, 0,
            "a negative delta is a reset, not a gigabyte read backwards"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_new_device_contributes_nothing_until_it_has_a_delta() {
        let root = sandbox("new");
        device(&root, "sda", 1000, 0, 0);
        let mut sampler = DiskSampler::rooted_at(root.clone());
        sampler.sample(std::time::Duration::from_secs(1));

        // A USB stick appears with a lifetime of counters already on it.
        device(&root, "sdb", 1000, 5_000_000, 5_000_000);
        let reading = sampler.sample(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(
            reading.totals.read_per_second, 0,
            "its counters since boot are not throughput in the last second"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rates_account_for_the_actual_elapsed_time() {
        let root = sandbox("elapsed");
        device(&root, "sda", 1000, 0, 0);
        let mut sampler = DiskSampler::rooted_at(root.clone());
        sampler.sample(std::time::Duration::from_secs(1));

        device(&root, "sda", 1000, 1000, 0);
        // Two seconds for the same bytes is half the rate.
        let reading = sampler.sample(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(reading.totals.read_per_second, 500 * 512);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_missing_directory_reads_as_no_devices() {
        let mut sampler = DiskSampler::rooted_at(PathBuf::from("/definitely/not/here"));
        assert!(sampler.sample(std::time::Duration::from_secs(1)).is_none());
        assert!(sampler.sample(std::time::Duration::from_secs(1)).is_none());
    }

    // ---- against this machine ----

    #[test]
    fn this_machines_block_devices_are_filtered_to_the_real_ones() {
        let Ok(entries) = std::fs::read_dir("/sys/block") else {
            return;
        };
        let mut total = 0;
        let mut real = 0;
        for entry in entries.flatten() {
            total += 1;
            let name = entry.file_name().to_string_lossy().into_owned();
            let size: u64 = std::fs::read_to_string(entry.path().join("size"))
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            if is_real_storage(&name, size) {
                real += 1;
            }
        }
        assert!(total > 0);
        assert!(
            real < total,
            "this machine has loop devices, so filtering must remove some"
        );
        assert!(real > 0, "and it must not remove the actual disk");
    }
}
