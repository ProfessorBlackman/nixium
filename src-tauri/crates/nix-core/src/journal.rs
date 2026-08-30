// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Journal entries for a unit. `SVC-5`.
//!
//! # Why this one uses a subprocess
//!
//! §P4 says no subprocess **where an API exists**, and here one does not. Reading the journal means
//! `sd-journal`, a C library, which would mean either `libc` bindings or `unsafe` in a workspace that
//! denies it. There is no D-Bus interface for reading entries — `systemd-journal-gatewayd` exists but
//! is HTTP, is not installed by default, and would be a strange thing to require.
//!
//! So `journalctl` it is, with two things that keep it honest:
//!
//! - **`-o json`**, so nothing parses a human table. This is the same reason `SVC-1` uses D-Bus rather
//!   than `systemctl`: output built for people changes, and truncates.
//! - **A cursor instead of `--follow`**, so there is no long-lived child process. Each poll asks for
//!   what has appeared since the last entry, which is cheap and needs nothing kept alive between
//!   calls. That also means §P4's actual test — zero spawns in the steady-state monitoring loop — is
//!   still satisfied, because this runs only while a unit's logs are on screen.
//!
//! # A unit name reaches a command line
//!
//! So it is validated against systemd's own rules before it does, and the argument vector is otherwise
//! fixed. This is the same discipline as the snap revision in `STO-12`: the primary defence is that the
//! name came from systemd's own inventory, and the shape check is the cheap second one that does not
//! depend on a parser.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, Cause, ErrorCode, Result};
use crate::units::Scope;

/// Syslog severity, as the journal records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Severity {
    Emergency,
    Alert,
    Critical,
    Error,
    Warning,
    Notice,
    Info,
    Debug,
}

impl Severity {
    /// From a syslog priority, 0 (most severe) to 7.
    #[must_use]
    pub const fn from_priority(priority: u8) -> Self {
        match priority {
            0 => Self::Emergency,
            1 => Self::Alert,
            2 => Self::Critical,
            3 => Self::Error,
            4 => Self::Warning,
            5 => Self::Notice,
            6 => Self::Info,
            _ => Self::Debug,
        }
    }

    /// Whether this is worth colouring red. Anything at `Error` or above.
    #[must_use]
    pub const fn is_problem(self) -> bool {
        matches!(
            self,
            Self::Emergency | Self::Alert | Self::Critical | Self::Error
        )
    }
}

/// One journal entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Entry {
    /// Microseconds since the epoch.
    #[ts(type = "number")]
    pub at_us: u64,
    pub message: String,
    pub severity: Severity,
    /// What wrote it — usually the executable's name.
    pub identifier: String,
    #[ts(type = "number | null")]
    pub pid: Option<u32>,
    /// An opaque position in the journal, for asking what has appeared since.
    pub cursor: String,
}

/// A page of entries plus where to resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Page {
    pub entries: Vec<Entry>,
    /// The last entry's cursor, to pass back for the next poll. `None` when nothing was returned, in
    /// which case the caller keeps the cursor it already had.
    pub cursor: Option<String>,
}

/// Whether a string is shaped like a systemd unit name.
///
/// systemd's own rule: the characters permitted in a unit name are alphanumerics and `:-_.\@`, and it
/// must carry a recognised suffix. Anything else is refused before it reaches an argument vector.
#[must_use]
pub fn is_unit_name(name: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        ".service",
        ".socket",
        ".timer",
        ".mount",
        ".automount",
        ".target",
        ".path",
        ".slice",
        ".scope",
        ".device",
        ".swap",
    ];

    !name.is_empty()
        && name.len() <= 256
        && SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.' | '\\' | '@'))
}

/// Parse `journalctl -o json`, which is one JSON object per line.
///
/// Fields arrive as strings even when they are numbers, and any of them may be absent — a kernel
/// message has no `_PID`, and an entry with a binary message has `MESSAGE` as an array of bytes rather
/// than a string. Each of those is handled by leaving the field out rather than by failing the line,
/// because one odd entry must not lose the other forty-nine.
#[must_use]
pub fn parse(output: &str) -> Vec<Entry> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            let text =
                |key: &str| -> Option<String> { value.get(key)?.as_str().map(str::to_string) };

            // A binary message comes through as an array of byte values. Rendering it as bytes is
            // useless, so it is described instead — which is at least true.
            let message = text("MESSAGE").or_else(|| {
                value
                    .get("MESSAGE")?
                    .as_array()
                    .map(|bytes| format!("({} bytes of binary output)", bytes.len()))
            })?;

            Some(Entry {
                at_us: text("__REALTIME_TIMESTAMP")
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(0),
                severity: text("PRIORITY")
                    .and_then(|p| p.parse::<u8>().ok())
                    .map_or(Severity::Info, Severity::from_priority),
                identifier: text("SYSLOG_IDENTIFIER")
                    .or_else(|| text("_COMM"))
                    .unwrap_or_default(),
                pid: text("_PID").and_then(|p| p.parse().ok()),
                cursor: text("__CURSOR").unwrap_or_default(),
                message,
            })
        })
        .collect()
}

/// Recent entries for one unit, or everything since `after`. `SVC-5`.
///
/// `after` is a cursor from a previous [`Page`]. Passing one turns this into a follow: it returns only
/// what has appeared since, so polling costs almost nothing when a unit is quiet.
pub fn entries(scope: Scope, unit: &str, limit: u32, after: Option<&str>) -> Result<Page> {
    if !is_unit_name(unit) {
        return Err(AppError::invalid_input(format!(
            "{unit} is not shaped like a unit name."
        )));
    }

    let mut command = std::process::Command::new("journalctl");
    command.arg("--no-pager").arg("-o").arg("json");
    if scope == Scope::User {
        command.arg("--user");
    }
    command.arg("-u").arg(unit);

    let limit = limit.clamp(1, 1000).to_string();
    match after {
        // `--after-cursor` excludes the cursor's own entry, so a poll cannot repeat the last line.
        Some(cursor) => {
            command.arg("--after-cursor").arg(cursor);
            command.arg("-n").arg(&limit);
        }
        None => {
            command.arg("-n").arg(&limit);
        }
    }

    let output = command
        .output()
        .map_err(|e| AppError::from_io(&e, "read the journal"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        // Not being in `systemd-journal` or `adm` is the common case and is not a fault in nix.
        let denied = stderr.contains("Permission denied") || stderr.contains("not permitted");
        return Err(AppError::new(
            if denied {
                ErrorCode::AuthDenied
            } else {
                ErrorCode::CommandFailed
            },
            if denied {
                "You do not have permission to read the system journal.".to_string()
            } else {
                format!("Could not read the journal for {unit}.")
            },
        )
        .with_remedy(if denied {
            "Reading other units' logs needs membership of the `systemd-journal` or `adm` group."
        } else {
            "Nothing was changed."
        })
        .with_cause(Cause::Command {
            program: "journalctl".to_string(),
            status: output.status.code(),
            stderr,
        }));
    }

    let entries = parse(&String::from_utf8_lossy(&output.stdout));
    Ok(Page {
        cursor: entries.last().map(|e| e.cursor.clone()),
        entries,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Golden line, §P8. Captured from this machine, trimmed to the fields that are read.
    const REAL: &str = r#"{"SYSLOG_TIMESTAMP":"Aug 28 11:33:39 ","SYSLOG_IDENTIFIER":"anacron","_TRANSPORT":"syslog","PRIORITY":"5","_PID":"3821","MESSAGE":"Normal exit (0 jobs run)","__REALTIME_TIMESTAMP":"1787916819159054","__CURSOR":"s=021be64f;i=1a2b;b=e3ca7d10","_SYSTEMD_CGROUP":"/system.slice/anacron.service"}"#;

    #[test]
    fn an_entry_is_parsed_from_real_output() {
        let entries = parse(REAL);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.message, "Normal exit (0 jobs run)");
        assert_eq!(entry.identifier, "anacron");
        assert_eq!(entry.severity, Severity::Notice, "priority 5");
        assert_eq!(entry.pid, Some(3821));
        assert_eq!(entry.at_us, 1_787_916_819_159_054);
        assert!(!entry.cursor.is_empty());
    }

    /// Every field arrives as a string, even the numbers.
    #[test]
    fn numeric_fields_arrive_as_strings_and_are_parsed_anyway() {
        let entries =
            parse(r#"{"MESSAGE":"x","PRIORITY":"3","_PID":"42","__REALTIME_TIMESTAMP":"1000"}"#);
        assert_eq!(entries[0].severity, Severity::Error);
        assert_eq!(entries[0].pid, Some(42));
        assert_eq!(entries[0].at_us, 1000);
    }

    /// A kernel message has no `_PID`, and that is not a broken entry.
    #[test]
    fn a_missing_pid_is_none_rather_than_a_lost_entry() {
        let entries = parse(r#"{"MESSAGE":"kernel says something","PRIORITY":"4"}"#);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pid, None);
        assert_eq!(
            entries[0].identifier, "",
            "unknown, and blank rather than invented"
        );
    }

    /// A binary message comes through as an array of byte values, not a string.
    #[test]
    fn a_binary_message_is_described_rather_than_rendered() {
        let entries = parse(r#"{"MESSAGE":[104,105,0,255],"PRIORITY":"6"}"#);
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].message.contains("4 bytes"),
            "got {:?}",
            entries[0].message
        );
    }

    #[test]
    fn one_malformed_line_does_not_lose_the_others() {
        let output = format!("{REAL}\nnot json at all\n{{}}\n{REAL}");
        assert_eq!(
            parse(&output).len(),
            2,
            "an entry with no MESSAGE is not an entry, and a broken line is skipped"
        );
    }

    #[test]
    fn every_syslog_priority_maps_to_a_severity() {
        assert_eq!(Severity::from_priority(0), Severity::Emergency);
        assert_eq!(Severity::from_priority(3), Severity::Error);
        assert_eq!(Severity::from_priority(6), Severity::Info);
        assert_eq!(Severity::from_priority(7), Severity::Debug);
        assert_eq!(
            Severity::from_priority(99),
            Severity::Debug,
            "an out-of-range priority must not panic"
        );
    }

    #[test]
    fn errors_and_worse_are_problems() {
        for problem in [
            Severity::Emergency,
            Severity::Alert,
            Severity::Critical,
            Severity::Error,
        ] {
            assert!(problem.is_problem(), "{problem:?}");
        }
        for ordinary in [
            Severity::Warning,
            Severity::Notice,
            Severity::Info,
            Severity::Debug,
        ] {
            assert!(!ordinary.is_problem(), "{ordinary:?}");
        }
    }

    /// A unit name reaches a command line, so its shape is checked.
    #[test]
    fn unit_names_are_validated_before_reaching_a_command_line() {
        for good in [
            "nginx.service",
            "getty@tty1.service",
            "home.mount",
            "apt-daily-upgrade.timer",
            "dbus.socket",
            "user-1000.slice",
        ] {
            assert!(is_unit_name(good), "{good} is a unit name");
        }
        for bad in [
            "",
            "no-suffix",
            "nginx.service; rm -rf /",
            "--after-cursor",
            "../../etc/passwd",
            "unit with spaces.service",
            "$(whoami).service",
            "nginx.unknownsuffix",
        ] {
            assert!(!is_unit_name(bad), "{bad} must be refused");
        }
        assert!(!is_unit_name(&format!("{}.service", "a".repeat(300))));
    }

    #[test]
    fn a_refused_unit_name_never_runs_anything() {
        let error = entries(Scope::System, "; rm -rf /", 10, None).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }

    // ---- against this machine ----

    #[test]
    fn this_machines_journal_reads_for_a_real_unit() {
        let Ok(page) = entries(Scope::System, "systemd-logind.service", 5, None) else {
            return; // no journalctl, or no permission — both legitimate
        };
        if page.entries.is_empty() {
            return;
        }

        assert!(page.entries.len() <= 5, "the limit must be honoured");
        assert!(page.cursor.is_some());
        for entry in &page.entries {
            assert!(!entry.message.is_empty());
            assert!(entry.at_us > 0, "an entry is anchored in time");
        }
        // Oldest first, which is how a log is read.
        assert!(
            page.entries.windows(2).all(|w| w[0].at_us <= w[1].at_us),
            "entries must be in chronological order"
        );
    }

    /// The cursor turns a fetch into a follow: asking again returns nothing new from a quiet unit.
    #[test]
    fn a_cursor_returns_only_what_is_new() {
        let Ok(first) = entries(Scope::System, "systemd-logind.service", 5, None) else {
            return;
        };
        let Some(cursor) = first.cursor else { return };

        let Ok(second) = entries(Scope::System, "systemd-logind.service", 5, Some(&cursor)) else {
            return;
        };
        // A quiet unit produces nothing; a busy one produces only entries after the cursor.
        for entry in &second.entries {
            assert!(
                entry.cursor != cursor,
                "--after-cursor excludes the cursor's own entry, so a poll cannot repeat a line"
            );
        }
    }
}
