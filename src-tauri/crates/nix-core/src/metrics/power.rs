// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Battery and mains power from `/sys/class/power_supply`. `MON-5`.
//!
//! # Two kinds of battery, and a tool that reads only one shows nothing
//!
//! The kernel exposes a battery's charge in **one of two forms**, depending on what the firmware
//! reports:
//!
//! | Form | Fields | Unit |
//! | --- | --- | --- |
//! | Energy | `energy_now`, `energy_full`, `power_now` | µWh, µW |
//! | Charge | `charge_now`, `charge_full`, `current_now` | µAh, µA |
//!
//! Neither is more correct. This laptop reports the **charge** form and no `energy_*` at all, so an
//! implementation that reads only `energy_now` — the one most examples show — displays an empty
//! battery panel on it and on every other machine like it.
//!
//! Charge is converted to energy through `voltage_now`, which is what the kernel's own userspace
//! tools do: watt-hours are amp-hours times volts.
//!
//! # Health is a separate question from charge
//!
//! `capacity` says how full the battery is *now*. How much it can hold compared with when it was new
//! is `full / full_design`, and it is the number that tells someone their battery is wearing out.
//! This machine reads 97% charged and **74% healthy**, which are both true and mean different things.
//!
//! # Time remaining is only sometimes knowable
//!
//! It needs a non-zero rate, and the rate is zero whenever the battery is neither charging nor
//! discharging — on mains and full, as here. `None` then, rather than infinity or a zero that reads
//! as "about to die".

use std::path::Path;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What a battery is doing, as the kernel reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ChargeState {
    Charging,
    Discharging,
    Full,
    /// On mains, but deliberately not charging — a vendor charge limit, usually.
    NotCharging,
    Unknown,
}

impl ChargeState {
    #[must_use]
    fn parse(text: &str) -> Self {
        match text.trim() {
            "Charging" => Self::Charging,
            "Discharging" => Self::Discharging,
            "Full" => Self::Full,
            "Not charging" => Self::NotCharging,
            _ => Self::Unknown,
        }
    }
}

/// One battery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Battery {
    pub name: String,
    pub state: ChargeState,
    /// How full it is now, 0–100.
    #[ts(type = "number")]
    pub percent: u8,
    /// Capacity now against capacity when new, 0–100. `None` when the firmware does not say.
    #[ts(type = "number | null")]
    pub health_percent: Option<u8>,
    /// Current draw or charge rate in watts. Zero when idle.
    pub watts: f32,
    /// Seconds until full or empty. `None` whenever the rate is zero — which is most of the time on
    /// mains, and is not the same as "no time left".
    #[ts(type = "number | null")]
    pub seconds_remaining: Option<u64>,
    #[ts(type = "number | null")]
    pub cycles: Option<u32>,
    pub technology: Option<String>,
}

/// Mains and batteries together.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PowerReading {
    /// Whether a mains supply reports itself online. `None` when there is no mains supply at all,
    /// which is how a desktop with no `AC` node looks and is different from "unplugged".
    pub on_mains: Option<bool>,
    /// Empty on a desktop. `MON-5` hides the whole panel then rather than showing an empty one.
    pub batteries: Vec<Battery>,
}

impl PowerReading {
    /// Whether this machine has a battery at all, which is what decides if the panel appears.
    #[must_use]
    pub fn has_battery(&self) -> bool {
        !self.batteries.is_empty()
    }
}

fn read_number(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_text(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Charge in watt-hours, and the rate in watts, from whichever form the firmware uses.
///
/// Returns `(now_wh, full_wh, full_design_wh, watts)`.
fn energy_figures(dir: &Path) -> (Option<f64>, Option<f64>, Option<f64>, f64) {
    // Energy form: microwatt-hours and microwatts, directly.
    let micro_to_unit = |v: i64| -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let v = v as f64;
        v / 1_000_000.0
    };

    if let Some(now) = read_number(&dir.join("energy_now")) {
        return (
            Some(micro_to_unit(now)),
            read_number(&dir.join("energy_full")).map(micro_to_unit),
            read_number(&dir.join("energy_full_design")).map(micro_to_unit),
            read_number(&dir.join("power_now"))
                .map_or(0.0, micro_to_unit)
                .abs(),
        );
    }

    // Charge form: microamp-hours and microamps, which become watt-hours and watts through voltage.
    let volts = read_number(&dir.join("voltage_now")).map_or(0.0, micro_to_unit);
    let to_wh = |v: i64| -> f64 { micro_to_unit(v) * volts };

    (
        read_number(&dir.join("charge_now")).map(to_wh),
        read_number(&dir.join("charge_full")).map(to_wh),
        read_number(&dir.join("charge_full_design")).map(to_wh),
        read_number(&dir.join("current_now")).map_or(0.0, |a| (micro_to_unit(a) * volts).abs()),
    )
}

/// Read one power-supply node, if it is a battery.
fn battery_at(dir: &Path, name: &str) -> Option<Battery> {
    if read_text(&dir.join("type")).as_deref() != Some("Battery") {
        return None;
    }

    let state =
        read_text(&dir.join("status")).map_or(ChargeState::Unknown, |s| ChargeState::parse(&s));
    let (now_wh, full_wh, design_wh, watts) = energy_figures(dir);

    // `capacity` where the firmware gives it, else derived. Preferring the firmware's own figure
    // matters because some batteries report a capacity that does not equal now/full.
    let percent = read_number(&dir.join("capacity"))
        .and_then(|c| u8::try_from(c.clamp(0, 100)).ok())
        .or_else(|| {
            let (now, full) = (now_wh?, full_wh?);
            (full > 0.0).then(|| {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let p = ((now / full) * 100.0).clamp(0.0, 100.0) as u8;
                p
            })
        })?;

    let health_percent = match (full_wh, design_wh) {
        (Some(full), Some(design)) if design > 0.0 => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let h = ((full / design) * 100.0).clamp(0.0, 100.0) as u8;
            Some(h)
        }
        _ => None,
    };

    // Only knowable with a rate, and the rate is zero whenever the battery is idle.
    let seconds_remaining = if watts <= 0.0 {
        None
    } else {
        let hours = match state {
            ChargeState::Discharging => now_wh.map(|now| now / watts),
            ChargeState::Charging => match (now_wh, full_wh) {
                (Some(now), Some(full)) if full > now => Some((full - now) / watts),
                _ => None,
            },
            _ => None,
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        hours.map(|h| (h * 3600.0).max(0.0) as u64)
    };

    Some(Battery {
        name: name.to_string(),
        state,
        percent,
        health_percent,
        #[allow(clippy::cast_possible_truncation)]
        watts: watts as f32,
        seconds_remaining,
        cycles: read_number(&dir.join("cycle_count"))
            .and_then(|c| u32::try_from(c).ok())
            .filter(|c| *c > 0),
        technology: read_text(&dir.join("technology")),
    })
}

/// Read every power supply under `root`.
#[must_use]
pub(crate) fn read(root: &Path) -> PowerReading {
    let mut reading = PowerReading::default();
    let Ok(entries) = std::fs::read_dir(root) else {
        return reading;
    };

    for entry in entries.flatten() {
        let dir = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        match read_text(&dir.join("type")).as_deref() {
            Some("Mains") => {
                // Any mains supply reporting online counts: a dock and a charger are both mains.
                let online = read_number(&dir.join("online")).map(|v| v == 1);
                reading.on_mains = match (reading.on_mains, online) {
                    (Some(true), _) | (_, Some(true)) => Some(true),
                    (existing, None) => existing,
                    (_, Some(false)) => Some(false),
                };
            }
            Some("Battery") => {
                if let Some(battery) = battery_at(&dir, &name) {
                    reading.batteries.push(battery);
                }
            }
            _ => {}
        }
    }

    reading.batteries.sort_by(|a, b| a.name.cmp(&b.name));
    reading
}

/// Read this machine's power supplies.
#[must_use]
pub fn sample() -> PowerReading {
    read(Path::new("/sys/class/power_supply"))
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
            "nix-power-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn node(root: &Path, name: &str, fields: &[(&str, &str)]) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, value) in fields {
            std::fs::write(dir.join(file), value).unwrap();
        }
    }

    /// Golden values, §P8: exactly what this laptop reports. It uses the **charge** form.
    #[test]
    fn a_charge_form_battery_is_read_correctly() {
        let root = sandbox("charge");
        node(
            &root,
            "BAT0",
            &[
                ("type", "Battery"),
                ("status", "Not charging"),
                ("capacity", "97"),
                ("charge_now", "3334000"),
                ("charge_full", "3408000"),
                ("charge_full_design", "4590000"),
                ("current_now", "0"),
                ("voltage_now", "12456000"),
                ("cycle_count", "243"),
                ("technology", "Li-ion"),
            ],
        );

        let reading = read(&root);
        assert_eq!(reading.batteries.len(), 1);
        let battery = &reading.batteries[0];

        assert_eq!(battery.state, ChargeState::NotCharging);
        assert_eq!(battery.percent, 97);
        // 3408000 / 4590000 = 74.2%
        assert_eq!(
            battery.health_percent,
            Some(74),
            "97% charged and 74% healthy are both true and mean different things"
        );
        assert_eq!(battery.cycles, Some(243));
        assert_eq!(battery.technology.as_deref(), Some("Li-ion"));

        std::fs::remove_dir_all(&root).ok();
    }

    /// # The trap
    ///
    /// Most examples read `energy_now`. This laptop has no `energy_*` fields at all, so an
    /// implementation that reads only those shows an empty battery panel on it.
    #[test]
    fn both_the_energy_and_charge_forms_are_understood() {
        let root = sandbox("forms");
        node(
            &root,
            "BAT0",
            &[
                ("type", "Battery"),
                ("status", "Discharging"),
                ("energy_now", "30000000"),
                ("energy_full", "60000000"),
                ("energy_full_design", "80000000"),
                ("power_now", "15000000"),
            ],
        );
        let energy = read(&root);
        assert_eq!(energy.batteries[0].percent, 50, "derived from now/full");
        assert!((energy.batteries[0].watts - 15.0).abs() < 0.01);
        // 30 Wh at 15 W is two hours.
        assert_eq!(energy.batteries[0].seconds_remaining, Some(7200));

        std::fs::remove_dir_all(&root).ok();

        let root = sandbox("forms2");
        node(
            &root,
            "BAT0",
            &[
                ("type", "Battery"),
                ("status", "Discharging"),
                ("charge_now", "2000000"),
                ("charge_full", "4000000"),
                ("current_now", "1000000"),
                ("voltage_now", "12000000"),
            ],
        );
        let charge = read(&root);
        assert_eq!(charge.batteries[0].percent, 50);
        // 1 A at 12 V is 12 W.
        assert!(
            (charge.batteries[0].watts - 12.0).abs() < 0.01,
            "amp-hours become watt-hours through voltage: {}",
            charge.batteries[0].watts
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// A rate of zero is not a time of zero.
    #[test]
    fn an_idle_battery_reports_no_time_remaining_rather_than_none_left() {
        let root = sandbox("idle");
        node(
            &root,
            "BAT0",
            &[
                ("type", "Battery"),
                ("status", "Full"),
                ("capacity", "100"),
                ("charge_now", "4000000"),
                ("charge_full", "4000000"),
                ("current_now", "0"),
                ("voltage_now", "12000000"),
            ],
        );
        let reading = read(&root);
        assert_eq!(
            reading.batteries[0].seconds_remaining, None,
            "zero draw means unknowable, not imminent"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn charging_counts_down_to_full_not_to_empty() {
        let root = sandbox("charging");
        node(
            &root,
            "BAT0",
            &[
                ("type", "Battery"),
                ("status", "Charging"),
                ("energy_now", "30000000"),
                ("energy_full", "60000000"),
                ("power_now", "30000000"),
            ],
        );
        let reading = read(&root);
        // 30 Wh still to add at 30 W is one hour.
        assert_eq!(reading.batteries[0].seconds_remaining, Some(3600));

        std::fs::remove_dir_all(&root).ok();
    }

    /// A desktop has no battery, and that is different from a battery at 0%.
    #[test]
    fn a_machine_with_no_battery_reports_none() {
        let root = sandbox("desktop");
        node(&root, "AC", &[("type", "Mains"), ("online", "1")]);

        let reading = read(&root);
        assert!(
            !reading.has_battery(),
            "the panel is hidden, not shown empty"
        );
        assert_eq!(reading.on_mains, Some(true));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn no_mains_supply_at_all_is_not_the_same_as_unplugged() {
        let root = sandbox("nomains");
        node(&root, "BAT0", &[("type", "Battery"), ("capacity", "50")]);
        let reading = read(&root);
        assert_eq!(
            reading.on_mains, None,
            "a machine with no AC node is not a machine running on battery"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn any_online_mains_supply_counts() {
        let root = sandbox("dock");
        node(&root, "AC", &[("type", "Mains"), ("online", "0")]);
        node(&root, "ucsi-source", &[("type", "Mains"), ("online", "1")]);
        let reading = read(&root);
        assert_eq!(
            reading.on_mains,
            Some(true),
            "a dock supplying power is mains even when the barrel jack is empty"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn every_charge_state_the_kernel_reports_is_understood() {
        for (text, expected) in [
            ("Charging", ChargeState::Charging),
            ("Discharging", ChargeState::Discharging),
            ("Full", ChargeState::Full),
            ("Not charging", ChargeState::NotCharging),
            ("Something New", ChargeState::Unknown),
        ] {
            assert_eq!(ChargeState::parse(text), expected);
        }
    }

    #[test]
    fn a_missing_directory_is_an_empty_reading() {
        let reading = read(Path::new("/definitely/not/here"));
        assert!(!reading.has_battery());
        assert_eq!(reading.on_mains, None);
    }

    #[test]
    fn a_zero_cycle_count_is_treated_as_unreported() {
        let root = sandbox("cycles");
        node(
            &root,
            "BAT0",
            &[
                ("type", "Battery"),
                ("capacity", "50"),
                ("cycle_count", "0"),
            ],
        );
        let reading = read(&root);
        assert_eq!(
            reading.batteries[0].cycles, None,
            "firmware that does not track cycles reports zero, which is not zero cycles"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    // ---- against this machine ----

    #[test]
    fn this_machines_power_is_plausible() {
        let reading = sample();
        for battery in &reading.batteries {
            assert!(battery.percent <= 100);
            assert!(battery.health_percent.is_none_or(|h| h <= 100));
            assert!(
                battery.watts >= 0.0,
                "a rate is a magnitude here, not a direction"
            );
            if battery.watts == 0.0 {
                assert_eq!(battery.seconds_remaining, None);
            }
        }
    }
}
