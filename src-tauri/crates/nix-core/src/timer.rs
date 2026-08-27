// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The growth-history collection timer. `STO-16`, decision D5.
//!
//! # Why a systemd user timer and not a thread
//!
//! Trend data is only useful if it is collected while nix is closed, which is most of the time. A
//! session timer only records what happens while the window is open, and a week of that is a week of
//! samples taken at whatever hours the user happened to be looking — which is a biased series
//! masquerading as a trend.
//!
//! # What the unit is constrained to do
//!
//! Every one of these is a promise to the user's machine, and each is in the unit rather than in nix's
//! own code so that it holds even if nix misbehaves:
//!
//! | Directive | Why |
//! | --- | --- |
//! | `Nice=19` | Never competes with anything the user is doing |
//! | `IOSchedulingClass=idle` | A storage tool must not make the disk slow |
//! | `ConditionACPower=true` | Never runs on battery — a scan is not worth someone's train journey |
//! | `Persistent=true` | A run missed while the machine was off fires at next login instead of vanishing |
//! | `RandomizedDelaySec` | Several machines on one filesystem do not all wake at once |
//!
//! **No lingering.** `loginctl enable-linger` would let user units run with no session at all, which
//! is a change to how the account behaves and is out of scope for a storage tool. So collection
//! happens when the user is logged in, and `Persistent=true` catches up what was missed.
//!
//! # `ExecStart` is this binary
//!
//! A subcommand of the same executable, not a second artefact. Two binaries is two things to keep
//! versioned, package and sign — and an `ExecStart` pointing at something that no longer exists is a
//! timer that fails silently every day.
//!
//! # When there is no systemd
//!
//! Flatpak sandboxes and non-systemd systems cannot install a user unit. The capability probe reports
//! [`Tier::Session`] and the UI says plainly that *trend data will only be collected while nix is
//! open* — which is worse, and a user who is told so can decide what to do about it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, ErrorCode, Result};

/// Unit names. Stable, because they are what an orphan is recognised by.
pub const SERVICE: &str = "nix-snapshot.service";
/// The timer that starts [`SERVICE`].
pub const TIMER: &str = "nix-snapshot.timer";

/// How collection can happen on this system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Tier {
    /// A systemd user timer: collects whether or not nix is running.
    Timer,
    /// While nix is open, and only then. Reported as the limitation it is.
    Session,
}

impl Tier {
    /// What the UI has to tell the user about this tier.
    #[must_use]
    pub const fn caveat(self) -> Option<&'static str> {
        match self {
            Self::Timer => None,
            Self::Session => Some(
                "This system cannot install a user timer, so trend data will only be collected while \
                 nix is open. Expect gaps.",
            ),
        }
    }
}

/// What nix found when it looked at the installed units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct State {
    pub tier: Tier,
    /// Whether both unit files are present.
    pub installed: bool,
    /// Whether the timer is enabled.
    pub enabled: bool,
    /// Present, but not what this version of nix would write.
    ///
    /// The specification requires an orphan from a previous version to be *detected at startup and
    /// repairable*. A unit whose `ExecStart` points at a binary that has moved is a job that fails
    /// every day in silence, which is worse than no job at all.
    pub orphaned: bool,
    /// Where the units live, so a user can look at them.
    pub directory: Option<PathBuf>,
}

/// The user unit directory.
#[must_use]
pub fn unit_dir() -> Option<PathBuf> {
    crate::paths::config_dir()
        .and_then(|c| c.parent().map(Path::to_path_buf))
        .map(|config| config.join("systemd/user"))
}

/// The service unit's text.
///
/// `executable` is written in rather than discovered at run time, because a unit has to name an
/// absolute path and the binary's location is only knowable from inside the running process.
#[must_use]
pub fn service_unit(executable: &Path) -> String {
    format!(
        "\
[Unit]
Description=nix storage snapshot
Documentation=man:nix(1)
# Never on battery: a filesystem scan is not worth someone's train journey.
ConditionACPower=true

[Service]
Type=oneshot
ExecStart={} snapshot --quiet
# Never competes with what the user is doing.
Nice=19
IOSchedulingClass=idle
# A storage tool that makes the disk slow has defeated itself.
IOSchedulingPriority=7
",
        executable.display()
    )
}

/// The timer unit's text.
#[must_use]
pub fn timer_unit() -> String {
    "\
[Unit]
Description=Collect nix storage trends daily

[Timer]
OnCalendar=daily
# A run missed while the machine was off fires at next login rather than vanishing.
Persistent=true
# So several machines sharing a filesystem do not all wake at once.
RandomizedDelaySec=1h
Unit=nix-snapshot.service

[Install]
WantedBy=timers.target
"
    .to_string()
}

/// Whether a unit file on disk is what this version would write.
///
/// Compared on content rather than on a version marker, because the thing that matters is whether the
/// unit still does what nix believes it does — and an `ExecStart` pointing at a moved binary is the
/// failure mode being guarded against.
#[must_use]
pub fn matches_expected(existing: &str, expected: &str) -> bool {
    // Compared line-by-line with blank lines and comments ignored, so reformatting by a person or a
    // systemd upgrade is not mistaken for an orphan.
    let significant = |text: &str| -> Vec<String> {
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect()
    };
    significant(existing) == significant(expected)
}

/// Whether `systemctl --user` can be used at all.
#[must_use]
pub fn tier() -> Tier {
    if !crate::caps::registry().has(crate::caps::Capability::Systemctl) {
        return Tier::Session;
    }
    // Present is not the same as usable: inside a Flatpak sandbox, or without a session bus,
    // `systemctl --user` fails. Asking it something harmless is the only honest test.
    match std::process::Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .output()
    {
        // Any answer at all means the user manager is reachable; `degraded` and `starting` are fine.
        Ok(output) => {
            if output.status.success() || !output.stdout.is_empty() {
                Tier::Timer
            } else {
                Tier::Session
            }
        }
        Err(_) => Tier::Session,
    }
}

/// Inspect what is installed.
#[must_use]
pub fn state(executable: &Path) -> State {
    let tier = tier();
    let Some(dir) = unit_dir() else {
        return State {
            tier,
            installed: false,
            enabled: false,
            orphaned: false,
            directory: None,
        };
    };

    let service = std::fs::read_to_string(dir.join(SERVICE));
    let timer = std::fs::read_to_string(dir.join(TIMER));
    let installed = service.is_ok() && timer.is_ok();

    // Orphaned means present but not current. A file that is absent is not an orphan.
    let orphaned = match (&service, &timer) {
        (Ok(s), Ok(t)) => {
            !matches_expected(s, &service_unit(executable)) || !matches_expected(t, &timer_unit())
        }
        (Ok(_), Err(_)) | (Err(_), Ok(_)) => true, // half-installed is an orphan too
        (Err(_), Err(_)) => false,
    };

    State {
        tier,
        installed,
        enabled: installed && is_enabled(),
        orphaned,
        directory: Some(dir),
    }
}

/// Whether systemd reports the timer as enabled.
fn is_enabled() -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "is-enabled", TIMER])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "enabled")
}

/// Write both units and enable the timer.
///
/// Idempotent: installing over an orphan is how an orphan is repaired.
pub fn install(executable: &Path) -> Result<State> {
    if tier() == Tier::Session {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "This system cannot install a user timer.",
        )
        .with_remedy(
            "Trend data will be collected while nix is open instead. That leaves gaps, and nix will \
             show them as gaps.",
        ));
    }

    let dir = unit_dir()
        .ok_or_else(|| AppError::internal("Could not resolve the systemd user unit directory."))?;

    crate::fs::write_atomically(&dir.join(SERVICE), service_unit(executable).as_bytes())?;
    crate::fs::write_atomically(&dir.join(TIMER), timer_unit().as_bytes())?;

    run_systemctl(&["--user", "daemon-reload"])?;
    run_systemctl(&["--user", "enable", "--now", TIMER])?;

    Ok(state(executable))
}

/// Disable the timer, remove both units, and delete the collected data.
///
/// Deleting the data is not incidental. The specification requires it, and a feature that keeps
/// collecting — or keeps what it collected — after being switched off is something done *to* a user
/// rather than for them.
pub fn uninstall(executable: &Path) -> Result<State> {
    if tier() == Tier::Timer {
        // Failures here are not fatal: the goal is that the units are gone, and a disable that fails
        // because the timer was already absent has achieved that.
        for args in [
            vec!["--user", "disable", "--now", TIMER],
            vec!["--user", "daemon-reload"],
        ] {
            if let Err(e) = run_systemctl(&args) {
                tracing::debug!(error = %e, "systemctl step failed while uninstalling the timer");
            }
        }
    }

    if let Some(dir) = unit_dir() {
        for unit in [SERVICE, TIMER] {
            match std::fs::remove_file(dir.join(unit)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(AppError::from_io(&e, format!("remove {unit}"))),
            }
        }
    }

    crate::history::History::discover()?.clear()?;
    Ok(state(executable))
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| AppError::from_io(&e, "run systemctl"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(
        AppError::new(ErrorCode::CommandFailed, "systemctl did not succeed.").with_cause(
            crate::error::Cause::Command {
                program: "systemctl".to_string(),
                status: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
        ),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn exe() -> PathBuf {
        PathBuf::from("/usr/bin/nix")
    }

    /// Every constraint in the specification, asserted against the text that is actually written.
    #[test]
    fn the_service_unit_carries_every_promised_constraint() {
        let unit = service_unit(&exe());
        for required in [
            "ConditionACPower=true",
            "Nice=19",
            "IOSchedulingClass=idle",
            "Type=oneshot",
        ] {
            assert!(
                unit.contains(required),
                "the unit must contain {required}:\n{unit}"
            );
        }
    }

    #[test]
    fn the_timer_unit_survives_a_missed_run() {
        let unit = timer_unit();
        assert!(
            unit.contains("Persistent=true"),
            "a missed run must fire at next login"
        );
        assert!(unit.contains("OnCalendar=daily"));
        assert!(unit.contains("WantedBy=timers.target"));
        assert!(
            unit.contains("RandomizedDelaySec"),
            "several machines on one filesystem must not wake together"
        );
    }

    /// `ExecStart` names this binary and a subcommand of it, never a second artefact.
    #[test]
    fn exec_start_points_at_this_binary_with_a_subcommand() {
        let unit = service_unit(Path::new("/opt/nix/bin/nix"));
        assert!(
            unit.contains("ExecStart=/opt/nix/bin/nix snapshot --quiet"),
            "{unit}"
        );
    }

    /// Lingering is deliberately not used, and a test says so, because it would be an easy and
    /// tempting thing for someone to add.
    #[test]
    fn nothing_enables_lingering() {
        let text = format!("{}{}", service_unit(&exe()), timer_unit());
        assert!(
            !text.contains("linger"),
            "enable-linger is out of scope by decision"
        );
    }

    #[test]
    fn a_current_unit_is_not_an_orphan() {
        assert!(matches_expected(
            &service_unit(&exe()),
            &service_unit(&exe())
        ));
        assert!(matches_expected(&timer_unit(), &timer_unit()));
    }

    /// The case the orphan check exists for: a unit whose `ExecStart` points somewhere else.
    #[test]
    fn a_unit_naming_a_different_binary_is_an_orphan() {
        let old = service_unit(Path::new("/usr/local/bin/nix-old"));
        assert!(
            !matches_expected(&old, &service_unit(&exe())),
            "an ExecStart pointing at a moved binary fails silently every day"
        );
    }

    /// Reformatting is not an orphan. Being too strict here would make nix rewrite units forever.
    #[test]
    fn comments_and_blank_lines_do_not_make_an_orphan() {
        let expected = service_unit(&exe());
        let reformatted = expected
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| format!("  {l}  \n\n"))
            .collect::<String>();
        assert!(
            matches_expected(&reformatted, &expected),
            "whitespace and comments are not meaning"
        );
    }

    #[test]
    fn a_changed_directive_is_an_orphan() {
        let expected = service_unit(&exe());
        let weakened = expected.replace("Nice=19", "Nice=0");
        assert!(
            !matches_expected(&weakened, &expected),
            "a unit that no longer yields to the user is not the unit nix installed"
        );
    }

    #[test]
    fn the_session_tier_explains_itself() {
        assert!(Tier::Timer.caveat().is_none());
        let caveat = Tier::Session.caveat().unwrap();
        assert!(caveat.contains("gaps"), "{caveat}");
        assert!(caveat.contains("while nix is open"), "{caveat}");
    }

    #[test]
    fn unit_names_are_stable_and_distinct() {
        assert_eq!(SERVICE, "nix-snapshot.service");
        assert_eq!(TIMER, "nix-snapshot.timer");
        assert!(
            timer_unit().contains(SERVICE),
            "the timer must start the service"
        );
    }

    // ---- against this machine ----

    #[test]
    fn this_machines_state_is_readable_without_installing_anything() {
        let found = state(&exe());
        // Nothing has been installed by the test suite, so nothing may be reported as installed.
        assert!(!found.installed, "the test suite must not install units");
        assert!(!found.orphaned, "absent is not orphaned");
        if found.tier == Tier::Timer {
            assert!(found.directory.is_some());
        }
    }
}
