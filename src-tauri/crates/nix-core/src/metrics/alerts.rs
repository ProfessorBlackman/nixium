// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Threshold alerts. `MON-6`.
//!
//! # Why this is a state machine and not an `if`
//!
//! The naive version — notify whenever the value is over the line — produces a notification every
//! second while a build runs, and a burst of them whenever a value sits exactly on the boundary and
//! jitters across it. Both are how a monitoring tool teaches someone to ignore it.
//!
//! Two mechanisms prevent that, and they solve different problems:
//!
//! - **Hysteresis** stops flapping. A rule fires at its threshold but does not clear until the value
//!   has fallen a margin *below* it, so a value oscillating around the line fires once rather than
//!   forty times.
//! - **Cooldown** stops repetition. Once cleared, a rule cannot fire again for a while, so a load that
//!   genuinely comes and goes every few seconds still produces one notification.
//!
//! And a rule that is already firing never fires again while the condition holds, which is the
//! specification's own acceptance criterion.
//!
//! # No clock inside
//!
//! Time is passed in. That is what makes hysteresis and cooldown testable by asserting on a sequence
//! of values rather than by sleeping, and it is why this module has no `Instant::now()` anywhere.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What a rule watches.
///
/// Typed rather than a string, so a rule cannot name a metric that does not exist and the UI can
/// enumerate what is available.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(tag = "metric", rename_all = "snake_case")]
#[ts(export)]
pub enum Metric {
    /// Overall CPU utilisation, as a fraction.
    CpuUsage,
    /// Memory in use against total, as a fraction. Derived from available, not from `used`.
    MemoryPressure,
    /// Swap in use, as a fraction.
    SwapPressure,
    /// A filesystem's used fraction.
    DiskUsage { mount: String },
    /// A filesystem's remaining bytes. **Falls** through its threshold rather than rising through it.
    DiskSpaceRemaining { mount: String },
    /// The hottest sensor, in degrees.
    Temperature,
}

impl Metric {
    /// Whether the alarming direction is *downward*.
    ///
    /// Free space is the odd one out: every other metric is bad when it rises. Modelling that as a
    /// property of the metric rather than as a separate rule type means hysteresis and cooldown work
    /// identically for both, with the comparison flipped in exactly one place.
    #[must_use]
    pub const fn alarms_when_falling(&self) -> bool {
        matches!(self, Self::DiskSpaceRemaining { .. })
    }

    /// A stable key, so state survives a rule being edited.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::CpuUsage => "cpu_usage".into(),
            Self::MemoryPressure => "memory_pressure".into(),
            Self::SwapPressure => "swap_pressure".into(),
            Self::Temperature => "temperature".into(),
            Self::DiskUsage { mount } => format!("disk_usage:{mount}"),
            Self::DiskSpaceRemaining { mount } => format!("disk_space:{mount}"),
        }
    }
}

/// One threshold a user has asked to be told about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Rule {
    pub metric: Metric,
    /// The value at which it fires. A fraction for the usage metrics, bytes for free space, degrees
    /// for temperature — the metric decides the unit.
    pub threshold: f64,
    /// How far back the value must come before the rule is considered clear.
    ///
    /// Zero would mean a value sitting on the threshold fires every time it wobbles.
    pub hysteresis: f64,
    /// Seconds after clearing during which it will not fire again.
    #[ts(type = "number")]
    pub cooldown_seconds: u64,
    /// Off means silent **immediately**, which the specification requires: a disabled rule that
    /// delivers one more notification is a disabled rule the user does not believe.
    pub enabled: bool,
}

impl Rule {
    /// A rule with sensible defaults for a fractional metric.
    #[must_use]
    pub fn fraction(metric: Metric, threshold: f64) -> Self {
        Self {
            metric,
            threshold,
            hysteresis: 0.05,
            cooldown_seconds: 300,
            enabled: true,
        }
    }
}

/// What a rule did on one evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Crossed the threshold and was not already firing: notify.
    Fired,
    /// Still over, and already notified. Deliberately silent.
    StillFiring,
    /// Came back within its margin.
    Cleared,
    /// Nothing to say.
    Quiet,
    /// Over the threshold, but too soon after the last time. Deliberately silent.
    Suppressed,
}

impl Outcome {
    /// Whether this outcome should reach the user.
    #[must_use]
    pub const fn notifies(self) -> bool {
        matches!(self, Self::Fired)
    }
}

/// Per-rule state, which is what makes cooldown and "do not repeat" real rather than aspirational.
#[derive(Debug, Clone, Copy, Default)]
struct Firing {
    active: bool,
    last_fired_at: Option<i64>,
}

/// Evaluates rules against readings, remembering what it has already said.
#[derive(Debug, Default)]
pub struct Alerts {
    state: HashMap<String, Firing>,
}

impl Alerts {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Evaluate one rule against one value at one moment.
    ///
    /// `now` is seconds since the epoch, passed in rather than read, so the cooldown is testable.
    pub fn evaluate(&mut self, rule: &Rule, value: f64, now: i64) -> Outcome {
        // Disabled means silent immediately, and forgets its state so re-enabling does not
        // immediately fire from a condition that ended while it was off.
        if !rule.enabled {
            self.state.remove(&rule.metric.key());
            return Outcome::Quiet;
        }

        let key = rule.metric.key();
        let entry = self.state.entry(key).or_default();

        let over = if rule.metric.alarms_when_falling() {
            value <= rule.threshold
        } else {
            value >= rule.threshold
        };

        // Clearing needs the value to come back past the threshold *plus a margin*, which is the
        // whole of hysteresis.
        let clear = if rule.metric.alarms_when_falling() {
            value > rule.threshold + rule.hysteresis
        } else {
            value < rule.threshold - rule.hysteresis
        };

        if entry.active {
            if clear {
                entry.active = false;
                return Outcome::Cleared;
            }
            return Outcome::StillFiring;
        }

        if !over {
            return Outcome::Quiet;
        }

        // Over, and not already firing. Cooldown decides whether anybody hears about it.
        if let Some(last) = entry.last_fired_at {
            if now.saturating_sub(last) < i64::try_from(rule.cooldown_seconds).unwrap_or(i64::MAX) {
                return Outcome::Suppressed;
            }
        }

        entry.active = true;
        entry.last_fired_at = Some(now);
        Outcome::Fired
    }

    /// Whether a rule is currently firing.
    #[must_use]
    pub fn is_firing(&self, metric: &Metric) -> bool {
        self.state
            .get(&metric.key())
            .is_some_and(|state| state.active)
    }

    /// Forget everything. Used when sampling stops, so a resumed session does not inherit a firing
    /// state from a condition nobody has observed since.
    pub fn reset(&mut self) {
        self.state.clear();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn cpu(threshold: f64) -> Rule {
        Rule::fraction(Metric::CpuUsage, threshold)
    }

    /// The specification's own criterion.
    #[test]
    fn an_alert_does_not_refire_while_the_condition_persists() {
        let mut alerts = Alerts::new();
        let rule = cpu(0.9);

        assert_eq!(alerts.evaluate(&rule, 0.95, 1000), Outcome::Fired);
        for second in 1..60 {
            assert_eq!(
                alerts.evaluate(&rule, 0.97, 1000 + second),
                Outcome::StillFiring,
                "a build running for a minute is one notification, not sixty"
            );
        }
        assert!(alerts.is_firing(&Metric::CpuUsage));
    }

    /// Hysteresis: a value sitting on the line must not flap.
    #[test]
    fn a_value_oscillating_around_the_threshold_fires_once() {
        let mut alerts = Alerts::new();
        let rule = cpu(0.9); // hysteresis 0.05, so it clears below 0.85

        assert_eq!(alerts.evaluate(&rule, 0.91, 1000), Outcome::Fired);
        let mut fires = 0;
        for (i, value) in [0.89, 0.91, 0.88, 0.92, 0.87, 0.90].iter().enumerate() {
            #[allow(clippy::cast_possible_wrap)]
            let outcome = alerts.evaluate(&rule, *value, 1000 + i as i64 + 1);
            if outcome.notifies() {
                fires += 1;
            }
        }
        assert_eq!(
            fires, 0,
            "none of those crossed back below 0.85, so the rule never cleared and never re-fired"
        );
    }

    #[test]
    fn clearing_needs_the_full_margin_not_just_the_threshold() {
        let mut alerts = Alerts::new();
        let rule = cpu(0.9);
        alerts.evaluate(&rule, 0.95, 1000);

        assert_eq!(
            alerts.evaluate(&rule, 0.87, 1001),
            Outcome::StillFiring,
            "below the threshold but inside the margin is still firing"
        );
        assert_eq!(alerts.evaluate(&rule, 0.80, 1002), Outcome::Cleared);
        assert!(!alerts.is_firing(&Metric::CpuUsage));
    }

    /// Cooldown: a condition that genuinely comes and goes still notifies once.
    #[test]
    fn a_repeating_condition_is_suppressed_until_the_cooldown_expires() {
        let mut alerts = Alerts::new();
        let rule = Rule {
            cooldown_seconds: 300,
            ..cpu(0.9)
        };

        assert_eq!(alerts.evaluate(&rule, 0.95, 1000), Outcome::Fired);
        assert_eq!(alerts.evaluate(&rule, 0.10, 1010), Outcome::Cleared);
        assert_eq!(
            alerts.evaluate(&rule, 0.95, 1020),
            Outcome::Suppressed,
            "ten seconds later is not a new event worth waking someone for"
        );
        assert_eq!(
            alerts.evaluate(&rule, 0.95, 1000 + 301),
            Outcome::Fired,
            "after the cooldown it is news again"
        );
    }

    /// Free space alarms by falling, which is the one metric that runs the other way.
    #[test]
    fn free_space_alarms_when_it_falls_through_the_threshold() {
        let mut alerts = Alerts::new();
        let rule = Rule {
            metric: Metric::DiskSpaceRemaining {
                mount: "/".to_string(),
            },
            threshold: 5_000_000_000.0,
            hysteresis: 500_000_000.0,
            cooldown_seconds: 3600,
            enabled: true,
        };

        assert_eq!(
            alerts.evaluate(&rule, 20_000_000_000.0, 1000),
            Outcome::Quiet,
            "plenty of space is not an alert"
        );
        assert_eq!(
            alerts.evaluate(&rule, 4_000_000_000.0, 1001),
            Outcome::Fired
        );
        assert_eq!(
            alerts.evaluate(&rule, 5_200_000_000.0, 1002),
            Outcome::StillFiring,
            "back over the threshold but inside the margin"
        );
        assert_eq!(
            alerts.evaluate(&rule, 6_000_000_000.0, 1003),
            Outcome::Cleared
        );
    }

    /// Disabling is immediate, which the specification requires by name.
    #[test]
    fn disabling_silences_a_firing_rule_at_once() {
        let mut alerts = Alerts::new();
        let rule = cpu(0.9);
        assert_eq!(alerts.evaluate(&rule, 0.99, 1000), Outcome::Fired);
        assert!(alerts.is_firing(&Metric::CpuUsage));

        let off = Rule {
            enabled: false,
            ..rule
        };
        assert_eq!(alerts.evaluate(&off, 0.99, 1001), Outcome::Quiet);
        assert!(
            !alerts.is_firing(&Metric::CpuUsage),
            "a disabled rule that delivers one more notification is one the user stops believing"
        );
    }

    /// Re-enabling must not immediately fire from a cooldown or a firing state that has gone stale.
    #[test]
    fn re_enabling_starts_from_a_clean_state() {
        let mut alerts = Alerts::new();
        let rule = cpu(0.9);
        alerts.evaluate(&rule, 0.99, 1000);
        alerts.evaluate(
            &Rule {
                enabled: false,
                ..rule.clone()
            },
            0.99,
            1001,
        );

        // The condition ended while the rule was off. Re-enabled and quiet, it stays quiet.
        assert_eq!(alerts.evaluate(&rule, 0.10, 1002), Outcome::Quiet);
        // And a genuinely new crossing is news, rather than being suppressed by the old cooldown.
        assert_eq!(alerts.evaluate(&rule, 0.99, 1003), Outcome::Fired);
    }

    /// Each metric keeps its own state, including one per mount point.
    #[test]
    fn rules_do_not_share_state_with_each_other() {
        let mut alerts = Alerts::new();
        let root = Rule {
            metric: Metric::DiskUsage {
                mount: "/".to_string(),
            },
            ..cpu(0.9)
        };
        let home = Rule {
            metric: Metric::DiskUsage {
                mount: "/home".to_string(),
            },
            ..cpu(0.9)
        };

        assert_eq!(alerts.evaluate(&root, 0.95, 1000), Outcome::Fired);
        assert_eq!(
            alerts.evaluate(&home, 0.95, 1000),
            Outcome::Fired,
            "a full root filesystem says nothing about /home"
        );
        assert_eq!(alerts.evaluate(&root, 0.95, 1001), Outcome::StillFiring);
    }

    #[test]
    fn metric_keys_are_distinct_and_stable() {
        let mut seen = std::collections::HashSet::new();
        for metric in [
            Metric::CpuUsage,
            Metric::MemoryPressure,
            Metric::SwapPressure,
            Metric::Temperature,
            Metric::DiskUsage {
                mount: "/".to_string(),
            },
            Metric::DiskUsage {
                mount: "/home".to_string(),
            },
            Metric::DiskSpaceRemaining {
                mount: "/".to_string(),
            },
        ] {
            assert!(seen.insert(metric.key()), "{metric:?} shares a key");
        }
        // Usage and free space on the same mount must not collide: they alarm in opposite directions.
        assert_ne!(
            Metric::DiskUsage {
                mount: "/".to_string()
            }
            .key(),
            Metric::DiskSpaceRemaining {
                mount: "/".to_string()
            }
            .key()
        );
    }

    #[test]
    fn only_a_fresh_firing_notifies() {
        assert!(Outcome::Fired.notifies());
        for quiet in [
            Outcome::StillFiring,
            Outcome::Cleared,
            Outcome::Quiet,
            Outcome::Suppressed,
        ] {
            assert!(!quiet.notifies(), "{quiet:?} must not reach the user");
        }
    }

    #[test]
    fn resetting_forgets_everything() {
        let mut alerts = Alerts::new();
        let rule = cpu(0.9);
        alerts.evaluate(&rule, 0.99, 1000);
        alerts.reset();
        assert!(!alerts.is_firing(&Metric::CpuUsage));
        assert_eq!(
            alerts.evaluate(&rule, 0.99, 1001),
            Outcome::Fired,
            "a resumed session has observed nothing yet, so the first crossing is news"
        );
    }
}
