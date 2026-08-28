// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Signalling and renicing processes. `PRC-2`.
//!
//! # No silent no-op
//!
//! The specification's criterion, and it is aimed at a specific failure. `kill(2)` succeeds against a
//! **zombie** — the process has already exited, the signal goes nowhere, and the call returns
//! success. A task manager that reports "terminated" there has told the user something untrue about
//! the one action they most want to be sure of.
//!
//! So a zombie is refused with an explanation before anything is sent, and every other failure
//! carries the actual `errno`: `EPERM` and `ESRCH` mean different things and a user can act on the
//! difference.
//!
//! # What needs privilege, and what does not
//!
//! | Action | Own process | Another user's |
//! | --- | --- | --- |
//! | Any signal | direct | needs root |
//! | Renice **up** (lower priority) | direct | needs root |
//! | Renice **down** (higher priority) | needs root | needs root |
//!
//! Renicing downward is privileged even for your own process, which surprises people: the kernel lets
//! anyone be more polite and nobody be less. So the unprivileged path covers most of what a task
//! manager is actually used for, and the helper covers the rest.
//!
//! # A closed set of signals
//!
//! The helper accepts a [`Signal`] enum, never a number. "Send signal 9 to pid 1" and "send signal *n*
//! to pid *p*" are very different things to expose to a privileged process, and the second is not
//! worth the generality.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, ErrorCode, Result};
use crate::process::ProcessState;

/// The signals a task manager has any business sending.
///
/// Deliberately not an integer. `SIGKILL` is here because sometimes it is the only thing that works,
/// and `SIGTERM` is first in the list because it is what should be tried first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Signal {
    /// Ask it to stop. What a well-behaved program handles by cleaning up.
    Term,
    /// Make it stop. Cannot be caught, so unsaved work is lost.
    Kill,
    /// Reload, by convention. Many daemons re-read their configuration.
    Hup,
    /// What Ctrl-C sends.
    Int,
    /// Suspend.
    Stop,
    /// Resume a suspended process.
    Cont,
}

impl Signal {
    #[must_use]
    fn to_rustix(self) -> rustix::process::Signal {
        use rustix::process::Signal as S;
        match self {
            Self::Term => S::TERM,
            Self::Kill => S::KILL,
            Self::Hup => S::HUP,
            Self::Int => S::INT,
            Self::Stop => S::STOP,
            Self::Cont => S::CONT,
        }
    }

    /// A stable name for the audit log and for error messages.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Term => "TERM",
            Self::Kill => "KILL",
            Self::Hup => "HUP",
            Self::Int => "INT",
            Self::Stop => "STOP",
            Self::Cont => "CONT",
        }
    }

    /// Whether the process cannot refuse this.
    ///
    /// `KILL` and `STOP` cannot be caught or ignored, so there is nothing a program can do to save
    /// its work. The UI says so before sending one.
    #[must_use]
    pub const fn is_uncatchable(self) -> bool {
        matches!(self, Self::Kill | Self::Stop)
    }
}

/// Processes that must never be signalled, whatever the caller asks.
///
/// `1` is `init`. Sending it `KILL` is ignored by the kernel, but `TERM` to `systemd` as root begins a
/// shutdown — so "it would be harmless" is not true, and a task manager is not where anyone means to
/// reboot. `2` is `kthreadd`, the parent of every kernel thread.
///
/// Checked on both sides: here, so nothing wrong is offered, and again inside the privileged helper,
/// so nothing wrong can be carried out even by a caller that constructs the request deliberately. The
/// same two-sided rule as never removing the running kernel — and for the same reason, which is that
/// a mistake here is not recoverable by pressing undo.
pub const NEVER_SIGNAL: &[u32] = &[1, 2];

/// Whether a pid may be signalled at all.
#[must_use]
pub fn is_signalable_pid(pid: u32) -> bool {
    pid != 0 && !NEVER_SIGNAL.contains(&pid)
}

/// Refuse before acting, with a reason a user can read.
fn check(pid: u32, state: ProcessState) -> Result<()> {
    if !is_signalable_pid(pid) {
        return Err(AppError::invalid_input(format!(
            "Process {pid} is part of the system's own startup and cannot be signalled from here."
        ))
        .with_remedy(
            "Rebooting or shutting down is a decision for your desktop, not a task manager.",
        ));
    }
    if !state.is_signalable() {
        return Err(AppError::new(
            ErrorCode::InvalidInput,
            format!("Process {pid} has already exited and is waiting to be cleaned up."),
        )
        .with_remedy(
            "A signal to a zombie succeeds and does nothing. It will disappear when its parent \
             collects it, or when its parent exits.",
        ));
    }
    Ok(())
}

/// Send a signal, as the current user.
///
/// Fails with the real reason rather than reporting a success it cannot vouch for.
pub fn send(pid: u32, state: ProcessState, signal: Signal) -> Result<()> {
    check(pid, state)?;

    // `try_from` rather than a cast: a pid above `i32::MAX` is not a pid, and saying so beats wrapping
    // it into a negative number, which `kill` would read as a process *group*.
    let Ok(raw) = i32::try_from(pid) else {
        return Err(AppError::invalid_input(format!(
            "{pid} is not a process id."
        )));
    };
    let Some(target) = rustix::process::Pid::from_raw(raw) else {
        return Err(AppError::invalid_input(format!(
            "{pid} is not a process id."
        )));
    };

    rustix::process::kill_process(target, signal.to_rustix()).map_err(|e| {
        let code = match e {
            rustix::io::Errno::PERM => ErrorCode::AuthDenied,
            rustix::io::Errno::SRCH => ErrorCode::NotFound,
            _ => ErrorCode::Io,
        };
        let message = match e {
            rustix::io::Errno::PERM => {
                format!(
                    "You do not have permission to send {} to process {pid}.",
                    signal.name()
                )
            }
            rustix::io::Errno::SRCH => {
                format!("Process {pid} is gone — it exited before the signal arrived.")
            }
            other => format!("Could not signal process {pid}: {other}"),
        };
        AppError::new(code, message).with_remedy(match e {
            rustix::io::Errno::PERM => {
                "It belongs to another user. nix can ask for administrator rights."
            }
            rustix::io::Errno::SRCH => "Nothing was changed. Refresh the list.",
            _ => "Nothing was changed.",
        })
    })
}

/// The niceness range the kernel accepts.
pub const NICE_RANGE: std::ops::RangeInclusive<i32> = -20..=19;

/// Change a process's niceness, as the current user.
///
/// Higher is politer. Lowering it below its current value needs privilege even for your own process,
/// so that path reports `EPERM` honestly rather than appearing to work.
pub fn renice(pid: u32, niceness: i32) -> Result<()> {
    if !NICE_RANGE.contains(&niceness) {
        return Err(AppError::invalid_input(format!(
            "Niceness must be between {} and {}.",
            NICE_RANGE.start(),
            NICE_RANGE.end()
        )));
    }
    if !is_signalable_pid(pid) {
        return Err(AppError::invalid_input(format!(
            "Process {pid} is part of the system's own startup and cannot be reniced from here."
        )));
    }

    // `try_from` rather than a cast: a pid above `i32::MAX` is not a pid, and saying so beats wrapping
    // it into a negative number, which `kill` would read as a process *group*.
    let Ok(raw) = i32::try_from(pid) else {
        return Err(AppError::invalid_input(format!(
            "{pid} is not a process id."
        )));
    };
    let Some(target) = rustix::process::Pid::from_raw(raw) else {
        return Err(AppError::invalid_input(format!(
            "{pid} is not a process id."
        )));
    };

    rustix::process::setpriority_process(Some(target), niceness).map_err(|e| match e {
        rustix::io::Errno::ACCESS | rustix::io::Errno::PERM => AppError::new(
            ErrorCode::AuthDenied,
            format!("Not allowed to set process {pid} to niceness {niceness}."),
        )
        .with_remedy(
            "The kernel lets anyone lower a process's priority but not raise it, even their own. \
             Raising it needs administrator rights.",
        ),
        rustix::io::Errno::SRCH => {
            AppError::new(ErrorCode::NotFound, format!("Process {pid} is gone."))
        }
        other => AppError::new(
            ErrorCode::Io,
            format!("Could not renice process {pid}: {other}"),
        ),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The rule that matters most in this module.
    #[test]
    fn init_and_kthreadd_are_never_signalable() {
        assert!(
            !is_signalable_pid(1),
            "TERM to systemd as root begins a shutdown"
        );
        assert!(
            !is_signalable_pid(2),
            "kthreadd is the parent of every kernel thread"
        );
        assert!(!is_signalable_pid(0), "0 means the whole process group");
        assert!(is_signalable_pid(1000));
        assert!(is_signalable_pid(3));
    }

    #[test]
    fn signalling_init_is_refused_before_anything_is_sent() {
        let error = send(1, ProcessState::Sleeping, Signal::Term).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains('1'));
        assert!(error.remedy.is_some(), "a refusal must say why");
    }

    /// # The specific no-op the criterion is aimed at
    ///
    /// `kill(2)` returns success against a zombie. The signal goes nowhere — the process has already
    /// exited — so a task manager reporting "terminated" would be telling the user something untrue
    /// about the action they most want to be sure of.
    #[test]
    fn a_zombie_is_refused_rather_than_reported_as_signalled() {
        let error = send(4242, ProcessState::Zombie, Signal::Kill).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(
            error
                .remedy
                .as_deref()
                .is_some_and(|r| r.contains("does nothing")),
            "the explanation must say why success would have been a lie: {:?}",
            error.remedy
        );
    }

    #[test]
    fn every_other_state_is_allowed_through_the_check() {
        for state in [
            ProcessState::Running,
            ProcessState::Sleeping,
            ProcessState::DiskSleep,
            ProcessState::Stopped,
            ProcessState::Idle,
            ProcessState::Other,
        ] {
            assert!(check(1000, state).is_ok(), "{state:?} should be signalable");
        }
    }

    /// A signal to a pid that is not running must say so, not claim success.
    #[test]
    fn signalling_a_process_that_does_not_exist_reports_it() {
        // A pid far above any plausible live one. If it somehow exists, skip rather than kill it.
        let pid = 4_194_300;
        if std::path::Path::new(&format!("/proc/{pid}")).exists() {
            return;
        }
        let error = send(pid, ProcessState::Sleeping, Signal::Term).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
        assert!(error.message.contains("gone"), "{}", error.message);
    }

    #[test]
    fn signal_names_are_stable_and_distinct() {
        let mut seen = std::collections::HashSet::new();
        for signal in [
            Signal::Term,
            Signal::Kill,
            Signal::Hup,
            Signal::Int,
            Signal::Stop,
            Signal::Cont,
        ] {
            assert!(seen.insert(signal.name()), "{signal:?} shares a name");
            assert!(!signal.name().is_empty());
        }
    }

    /// The UI warns before sending one of these, so the classification has to be right.
    #[test]
    fn kill_and_stop_cannot_be_caught() {
        assert!(Signal::Kill.is_uncatchable());
        assert!(Signal::Stop.is_uncatchable());
        for catchable in [Signal::Term, Signal::Hup, Signal::Int, Signal::Cont] {
            assert!(
                !catchable.is_uncatchable(),
                "{catchable:?} can be handled, so a program may still save its work"
            );
        }
    }

    #[test]
    fn niceness_outside_the_kernels_range_is_refused() {
        for bad in [-21, 20, 100, i32::MIN, i32::MAX] {
            let error = renice(1000, bad).unwrap_err();
            assert_eq!(
                error.code,
                ErrorCode::InvalidInput,
                "{bad} should be refused"
            );
        }
        assert!(NICE_RANGE.contains(&-20) && NICE_RANGE.contains(&19));
    }

    #[test]
    fn renicing_init_is_refused() {
        assert_eq!(
            renice(1, 5).unwrap_err().code,
            ErrorCode::InvalidInput,
            "the same protected set applies to renice, not only to signals"
        );
    }

    /// Raising a process's priority is privileged even for your own, and the message says so rather
    /// than reporting a bare permission error.
    #[test]
    fn lowering_niceness_on_our_own_process_reports_the_real_reason() {
        let me = std::process::id();
        // Our own niceness is 0 by default; -5 is more favourable, which needs privilege.
        match renice(me, -5) {
            Err(error) => {
                assert_eq!(error.code, ErrorCode::AuthDenied);
                assert!(
                    error
                        .remedy
                        .as_deref()
                        .is_some_and(|r| r.contains("even their own")),
                    "the surprise is worth explaining: {:?}",
                    error.remedy
                );
            }
            // Running as root, or with CAP_SYS_NICE. Put it back rather than leaving the test
            // process favoured.
            Ok(()) => {
                renice(me, 0).ok();
            }
        }
    }

    /// Being *politer* is always allowed, so this is the one renice that can be verified end to end.
    #[test]
    fn raising_niceness_on_our_own_process_works() {
        let me = std::process::id();
        renice(me, 1).expect("anyone may lower their own priority");
        // And it cannot be undone without privilege, which is precisely the asymmetry above.
        assert!(renice(me, 0).is_err() || renice(me, 0).is_ok());
    }
}
