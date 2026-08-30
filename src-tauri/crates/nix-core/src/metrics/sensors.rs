// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Temperatures and fans from `/sys/class/hwmon`. `MON-4`.
//!
//! Absent from Stacer entirely, which is a strange omission in a tool whose main claim was telling
//! you what your machine was doing.
//!
//! # Every chip, not the first one
//!
//! A laptop has several. This one has eight `hwmon` nodes — `coretemp`, `nvme`, `acpitz`,
//! `pch_cannonlake`, the wireless card, the battery, the charger and a vendor node — and the
//! interesting temperature is on a different one depending on what you want to know. So all of them
//! are read and each reading carries the chip that produced it, rather than picking one and hoping.
//!
//! # A machine with no fans is not a machine with broken fans
//!
//! This laptop reports **no `fan*_input` at all** — its vendor chip exposes only `pwm1_enable`.
//! Plenty of hardware is like that, and passively cooled machines have nothing to report by
//! definition. An empty list is the correct answer and the UI says so, rather than showing `0 RPM`
//! and inviting someone to worry about a fan that does not exist.
//!
//! # Units
//!
//! `hwmon` reports temperatures in **milli-degrees Celsius** and fans in RPM. The `_input` suffix is
//! the current value; `_max` and `_crit` are the thresholds the hardware itself declares, which is a
//! better source for "is this hot" than any number this program could invent.

use std::path::Path;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One temperature sensor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Temperature {
    /// The chip, e.g. `coretemp`.
    pub chip: String,
    /// The sensor's own label where it has one, e.g. `Package id 0`.
    pub label: String,
    pub celsius: f32,
    /// The hardware's own "getting warm" threshold, where it declares one.
    pub high_celsius: Option<f32>,
    /// The hardware's own critical threshold.
    pub critical_celsius: Option<f32>,
}

impl Temperature {
    /// How close this is to its critical threshold, `0.0..=1.0`.
    ///
    /// `None` when the hardware declares no threshold — better than inventing one, because "hot" for
    /// an NVMe controller and for a CPU package are different numbers and neither is 80.
    #[must_use]
    pub fn severity(&self) -> Option<f32> {
        let critical = self.critical_celsius?;
        if critical <= 0.0 {
            return None;
        }
        Some((self.celsius / critical).clamp(0.0, 1.0))
    }
}

/// One fan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Fan {
    pub chip: String,
    pub label: String,
    #[ts(type = "number")]
    pub rpm: u32,
}

/// Everything the hardware reports about heat.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SensorReading {
    pub temperatures: Vec<Temperature>,
    /// Empty on hardware that reports no fans, which is common and not a fault.
    pub fans: Vec<Fan>,
}

impl SensorReading {
    /// The hottest sensor, which is the one worth putting on a dashboard.
    #[must_use]
    pub fn hottest(&self) -> Option<&Temperature> {
        self.temperatures
            .iter()
            .max_by(|a, b| a.celsius.total_cmp(&b.celsius))
    }
}

fn read_number(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_text(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[allow(clippy::cast_precision_loss)]
fn millidegrees(value: i64) -> f32 {
    value as f32 / 1000.0
}

/// Read every `hwmon` chip.
#[must_use]
pub(crate) fn read(root: &Path) -> SensorReading {
    let mut reading = SensorReading::default();
    let Ok(chips) = std::fs::read_dir(root) else {
        return reading;
    };

    for chip in chips.flatten() {
        let dir = chip.path();
        let chip_name = read_text(&dir.join("name"))
            .unwrap_or_else(|| chip.file_name().to_string_lossy().into_owned());

        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        // Sensors are numbered, and the numbering is not contiguous — a chip may expose temp1 and
        // temp3 with no temp2. So the files present are enumerated rather than counted up to a limit.
        let mut names: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();

        for name in &names {
            if let Some(stem) = name.strip_suffix("_input") {
                if let Some(index) = stem.strip_prefix("temp") {
                    let Some(raw) = read_number(&dir.join(name)) else {
                        continue;
                    };
                    reading.temperatures.push(Temperature {
                        chip: chip_name.clone(),
                        label: read_text(&dir.join(format!("temp{index}_label")))
                            .unwrap_or_else(|| format!("temp{index}")),
                        celsius: millidegrees(raw),
                        high_celsius: read_number(&dir.join(format!("temp{index}_max")))
                            .map(millidegrees),
                        critical_celsius: read_number(&dir.join(format!("temp{index}_crit")))
                            .map(millidegrees),
                    });
                } else if let Some(index) = stem.strip_prefix("fan") {
                    let Some(rpm) = read_number(&dir.join(name)) else {
                        continue;
                    };
                    // A stopped fan reports 0, which is real. A negative reading is not.
                    let Ok(rpm) = u32::try_from(rpm) else {
                        continue;
                    };
                    reading.fans.push(Fan {
                        chip: chip_name.clone(),
                        label: read_text(&dir.join(format!("fan{index}_label")))
                            .unwrap_or_else(|| format!("fan{index}")),
                        rpm,
                    });
                }
            }
        }
    }

    reading
        .temperatures
        .sort_by(|a, b| b.celsius.total_cmp(&a.celsius));
    reading
        .fans
        .sort_by(|a, b| a.chip.cmp(&b.chip).then(a.label.cmp(&b.label)));
    reading
}

/// Read this machine's sensors.
#[must_use]
pub fn sample() -> SensorReading {
    read(Path::new("/sys/class/hwmon"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sandbox(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nix-sensors-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn chip(root: &Path, dir: &str, name: &str) -> PathBuf {
        let path = root.join(dir);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("name"), name).unwrap();
        path
    }

    fn write(dir: &Path, file: &str, value: &str) {
        std::fs::write(dir.join(file), value).unwrap();
    }

    /// Golden values, §P8: captured from this machine's `coretemp`.
    #[test]
    fn temperatures_are_read_with_their_labels_and_thresholds() {
        let root = sandbox("coretemp");
        let chip = chip(&root, "hwmon4", "coretemp");
        write(&chip, "temp1_input", "60000");
        write(&chip, "temp1_label", "Package id 0");
        write(&chip, "temp1_max", "100000");
        write(&chip, "temp1_crit", "100000");

        let reading = read(&root);
        assert_eq!(reading.temperatures.len(), 1);
        let t = &reading.temperatures[0];
        assert_eq!(t.chip, "coretemp");
        assert_eq!(t.label, "Package id 0");
        assert!(
            (t.celsius - 60.0).abs() < 0.01,
            "milli-degrees, not degrees"
        );
        assert_eq!(t.critical_celsius, Some(100.0));

        std::fs::remove_dir_all(&root).ok();
    }

    /// This machine reports no fans at all. That is not a fault.
    #[test]
    fn a_machine_with_no_fans_reports_none_rather_than_zero_rpm() {
        let root = sandbox("nofans");
        let chip = chip(&root, "hwmon6", "hp");
        // Exactly what this laptop's vendor chip exposes: a control, and no reading.
        write(&chip, "pwm1_enable", "2");

        let reading = read(&root);
        assert!(
            reading.fans.is_empty(),
            "a passively cooled machine has nothing to report, not a fan at 0 RPM"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Sensor numbering is not contiguous.
    #[test]
    fn a_gap_in_the_numbering_does_not_stop_the_enumeration() {
        let root = sandbox("gap");
        let chip = chip(&root, "hwmon3", "nvme");
        write(&chip, "temp1_input", "40000");
        // no temp2
        write(&chip, "temp3_input", "45000");

        let reading = read(&root);
        assert_eq!(
            reading.temperatures.len(),
            2,
            "counting up until a miss would stop at temp1"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn every_chip_is_read_not_just_the_first() {
        let root = sandbox("chips");
        let a = chip(&root, "hwmon0", "coretemp");
        write(&a, "temp1_input", "60000");
        let b = chip(&root, "hwmon1", "nvme");
        write(&b, "temp1_input", "40000");

        let reading = read(&root);
        assert_eq!(reading.temperatures.len(), 2);
        let chips: Vec<&str> = reading
            .temperatures
            .iter()
            .map(|t| t.chip.as_str())
            .collect();
        assert!(chips.contains(&"coretemp") && chips.contains(&"nvme"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_sensor_without_a_label_falls_back_to_its_number() {
        let root = sandbox("nolabel");
        let chip = chip(&root, "hwmon1", "acpitz");
        write(&chip, "temp1_input", "42000");

        let reading = read(&root);
        assert_eq!(reading.temperatures[0].label, "temp1");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn severity_uses_the_hardwares_own_threshold_or_says_nothing() {
        let hot = Temperature {
            chip: "coretemp".into(),
            label: "Package id 0".into(),
            celsius: 90.0,
            high_celsius: Some(100.0),
            critical_celsius: Some(100.0),
        };
        assert!((hot.severity().unwrap() - 0.9).abs() < 0.01);

        let unknown = Temperature {
            critical_celsius: None,
            ..hot.clone()
        };
        assert_eq!(
            unknown.severity(),
            None,
            "hot for an NVMe controller and for a CPU are different numbers, and neither is 80"
        );
    }

    #[test]
    fn the_hottest_sensor_is_the_one_reported() {
        let root = sandbox("hottest");
        let c = chip(&root, "hwmon0", "coretemp");
        write(&c, "temp1_input", "60000");
        write(&c, "temp2_input", "75000");
        write(&c, "temp3_input", "55000");

        let reading = read(&root);
        assert!((reading.hottest().unwrap().celsius - 75.0).abs() < 0.01);
        assert!(
            reading.temperatures[0].celsius >= reading.temperatures[1].celsius,
            "hottest first"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_missing_hwmon_directory_is_an_empty_reading() {
        let reading = read(Path::new("/definitely/not/here"));
        assert!(reading.temperatures.is_empty());
        assert!(reading.fans.is_empty());
        assert_eq!(reading.hottest(), None);
    }

    #[test]
    fn an_unreadable_sensor_is_skipped_rather_than_reported_as_zero() {
        let root = sandbox("unreadable");
        let c = chip(&root, "hwmon0", "coretemp");
        write(&c, "temp1_input", "not a number");
        write(&c, "temp2_input", "50000");

        let reading = read(&root);
        assert_eq!(reading.temperatures.len(), 1, "0°C would look like a fault");

        std::fs::remove_dir_all(&root).ok();
    }

    // ---- against this machine ----

    #[test]
    fn this_machines_sensors_are_plausible() {
        let reading = sample();
        if reading.temperatures.is_empty() {
            return; // a virtual machine, legitimately
        }
        for t in &reading.temperatures {
            assert!(
                (-50.0..=150.0).contains(&t.celsius),
                "{} on {} reads {}°C",
                t.label,
                t.chip,
                t.celsius
            );
            assert!(!t.chip.is_empty());
        }
        assert!(reading.hottest().is_some());
    }
}
