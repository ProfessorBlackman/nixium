// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The process table, from `/proc`. `PRC-1`.
//!
//! # `%CPU` here means what `top` means by it, not what `ps` means
//!
//! `ps` reports CPU as **an average over the process's entire lifetime**. For a process that started
//! four minutes ago and burned a core for the first two seconds, `ps` says a number that was true once
//! and has not been true since. Stacer displayed exactly that figure in a table it refreshed every
//! second, so a column that looked live was a lifetime average that happened to be redrawn often.
//!
//! This computes the real thing: the change in `utime + stime` between two readings, over the wall
//! time that elapsed between them. On this machine `ps` currently claims 20% for a `zsh` sitting at a
//! prompt, which is that artefact exactly.
//!
//! The figure is a percentage of **one core**, as `top` reports it, so a process using four cores
//! reads 400%. Dividing by core count instead would show a fully-busy single-threaded program as 12%
//! on an eight-core machine, which is true and useless.
//!
//! # Reading `/proc/<pid>/stat` is a trap
//!
//! The second field is the executable name, in parentheses, and it **may contain spaces and
//! parentheses**. This is not hypothetical: on this machine right now there is a process whose name is
//! `next-server (v1` — a space *and* an unmatched opening bracket — and several called
//! `npm exec fireba`.
//!
//! So the line is split on the **last** `)`, not the first, and not on whitespace. Everything after
//! that bracket is fixed-width fields with no parentheses in them, which makes the last one
//! unambiguous. Splitting on whitespace loses the process; splitting on the first `)` works until a
//! name contains one.
//!
//! # A pid is not an identity
//!
//! Pids are reused. A process that exits and a new one that inherits its number are different
//! processes, and attributing the old one's CPU counters to the new one would report an enormous spike
//! for something that has just started. Delta state is therefore keyed on **(pid, start time)**,
//! which together are unique for the life of a boot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Clock ticks per second in `procfs`.
///
/// `sysconf(_SC_CLK_TCK)` — 100 on Linux, and fixed at 100 for `procfs` regardless of the kernel's
/// own `CONFIG_HZ`, because the kernel converts to `USER_HZ` on the way out. Hardcoded because
/// reading it needs `libc`, and the workspace denies `unsafe`.
const CLOCK_TICKS: f64 = 100.0;

/// What a process is doing, as the kernel's single letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ProcessState {
    Running,
    Sleeping,
    /// Uninterruptible sleep — usually waiting on I/O, and not killable.
    DiskSleep,
    Stopped,
    /// Exited, and waiting for a parent that has not reaped it.
    Zombie,
    Idle,
    Other,
}

impl ProcessState {
    #[must_use]
    fn parse(letter: &str) -> Self {
        match letter {
            "R" => Self::Running,
            "S" => Self::Sleeping,
            "D" => Self::DiskSleep,
            "T" | "t" => Self::Stopped,
            "Z" => Self::Zombie,
            "I" => Self::Idle,
            _ => Self::Other,
        }
    }

    /// Whether this state means the process cannot be signalled usefully.
    ///
    /// A zombie has already exited; signalling it does nothing, and telling a user their `TERM`
    /// "worked" would be a lie. `PRC-2` uses this to explain rather than to pretend.
    #[must_use]
    pub const fn is_signalable(self) -> bool {
        !matches!(self, Self::Zombie)
    }
}

/// One process, as the table shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Process {
    #[ts(type = "number")]
    pub pid: u32,
    #[ts(type = "number")]
    pub ppid: u32,
    /// The executable's own name, from `comm`. Truncated to fifteen characters by the kernel.
    pub name: String,
    /// The full command line, or the name in brackets for a kernel thread — which is what `ps` does,
    /// and it distinguishes "no arguments" from "not a userspace process at all".
    pub command: String,
    pub state: ProcessState,
    #[ts(type = "number")]
    pub uid: u32,
    /// Resolved from `/etc/passwd`, or the uid as text when it has no entry — a container's uid, say.
    pub user: String,
    /// Percentage of **one core**, as `top` reports it. Can exceed 100 for a threaded process.
    pub cpu_percent: f32,
    /// Resident set size: physical memory actually held.
    #[ts(type = "number")]
    pub memory_bytes: u64,
    #[ts(type = "number")]
    pub threads: u32,
    /// The kernel's start time in clock ticks since boot. Part of a process's identity, not a display
    /// field: with the pid it survives pid reuse.
    #[ts(type = "number")]
    pub started_ticks: u64,
}

/// Cumulative CPU time for one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CpuTime {
    ticks: u64,
}

/// What a `/proc/<pid>/stat` line yields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stat {
    pid: u32,
    comm: String,
    state: ProcessState,
    ppid: u32,
    utime: u64,
    stime: u64,
    threads: u32,
    started_ticks: u64,
    /// Resident pages. Multiplied by the page size to become bytes.
    rss_pages: u64,
}

/// Page size in bytes.
///
/// 4096 on every architecture nix targets. Like [`CLOCK_TICKS`], reading it properly needs `libc`.
const PAGE_SIZE: u64 = 4096;

/// Parse one `/proc/<pid>/stat` line.
///
/// Split on the **last** `)`, because the `comm` field is bracketed and may contain both spaces and
/// brackets of its own. Field numbers below are as `proc(5)` gives them, counting the pid as 1.
#[must_use]
pub(crate) fn parse_stat(line: &str) -> Option<Stat> {
    // Field 1, before the opening bracket.
    let open = line.find(" (")?;
    let pid: u32 = line[..open].trim().parse().ok()?;

    // Field 2. The last bracket closes it; everything after is numbers and single letters.
    let close = line.rfind(')')?;
    if close < open + 2 {
        return None;
    }
    let comm = line[open + 2..close].to_string();

    // Fields 3 onward, so index 0 here is field 3.
    let rest: Vec<&str> = line[close + 1..].split_whitespace().collect();
    let at = |field: usize| -> Option<&str> { rest.get(field - 3).copied() };
    let number = |field: usize| -> u64 { at(field).and_then(|v| v.parse().ok()).unwrap_or(0) };

    Some(Stat {
        pid,
        comm,
        state: ProcessState::parse(at(3)?),
        ppid: u32::try_from(number(4)).unwrap_or(0),
        utime: number(14),
        stime: number(15),
        threads: u32::try_from(number(20)).unwrap_or(1),
        started_ticks: number(22),
        rss_pages: number(24),
    })
}

/// A uid's name, or the number itself when it has none.
///
/// Split out from the sampler because the uid now comes from the `/proc/<pid>` directory's owner
/// rather than from a file a test can write — so a fixture cannot choose a process's uid without
/// being root. That is a real cost of the faster read: the *resolution* is still tested here, and the
/// sampler's test asserts only what it can honestly control.
#[must_use]
pub(crate) fn resolve_user(uid: u32, users: &HashMap<u32, String>) -> String {
    users.get(&uid).cloned().unwrap_or_else(|| uid.to_string())
}

/// Just the state letter from a `stat` line.
///
/// Used by the privileged helper, which needs to know whether a process is a zombie before signalling
/// it and must establish that for itself rather than trusting the caller.
#[must_use]
pub fn state_from_stat(line: &str) -> Option<ProcessState> {
    parse_stat(line).map(|stat| stat.state)
}

/// Parse `/etc/passwd` into uid → name.
///
/// Read once per sampler rather than per process: a table of 652 processes would otherwise re-read and
/// re-parse the file 652 times. Names come from the file rather than from `getpwuid`, which needs
/// `libc` — the cost is that NSS sources beyond `files` (LDAP, SSSD) are not consulted, so a
/// domain-joined machine shows numeric uids for domain users. Stated rather than hidden.
#[must_use]
pub(crate) fn parse_passwd(text: &str) -> HashMap<u32, String> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let _password = fields.next()?;
            let uid: u32 = fields.next()?.parse().ok()?;
            Some((uid, name.to_string()))
        })
        .collect()
}

/// Reads `/proc`. **The single owner of per-process CPU delta state** (§P3).
#[derive(Debug)]
pub struct ProcessSampler {
    root: PathBuf,
    /// Keyed on `(pid, started_ticks)`, so a reused pid starts fresh rather than inheriting counters.
    previous: HashMap<(u32, u64), CpuTime>,
    users: HashMap<u32, String>,
}

impl Default for ProcessSampler {
    fn default() -> Self {
        Self::new()
    }
}

/// How often the process table refreshes while its view is open.
///
/// A complete pass costs about 43 ms on this machine's 655 processes — the most expensive sampler in
/// the program, and irreducibly so: 16.7 ms of that is reading `stat` for each process, which is the
/// data. Every second would be 4.3% of one core for as long as the view is open.
///
/// Two seconds halves it, and a process table that updates every two seconds is not a worse process
/// table — `top` defaults to three. It samples only while the view is mounted (§P9), so a closed
/// window costs nothing at all.
pub const TABLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

impl ProcessSampler {
    #[must_use]
    pub fn new() -> Self {
        Self::rooted_at(PathBuf::from("/proc"), Path::new("/etc/passwd"))
    }

    /// A sampler reading elsewhere, for tests.
    #[must_use]
    pub fn rooted_at(root: PathBuf, passwd: &Path) -> Self {
        Self {
            root,
            previous: HashMap::new(),
            users: std::fs::read_to_string(passwd)
                .map(|text| parse_passwd(&text))
                .unwrap_or_default(),
        }
    }

    /// Every process, with instantaneous CPU where a previous reading exists.
    ///
    /// `elapsed` is the wall time since the last call; it is the denominator of the CPU figure and is
    /// passed in rather than measured here so the arithmetic is testable.
    ///
    /// Processes that vanish mid-read are skipped rather than reported as errors. On a machine with
    /// 652 processes that happens constantly and is not a fault: a pid can exit between the directory
    /// listing and the `stat` read.
    pub fn sample(&mut self, elapsed: std::time::Duration) -> Vec<Process> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };

        // At least one tick, so a zero or absurdly small interval cannot divide into a huge figure.
        let elapsed_ticks = (elapsed.as_secs_f64() * CLOCK_TICKS).max(1.0);
        let mut current = HashMap::new();
        let mut processes = Vec::new();

        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // Only numeric entries are processes; `/proc` is full of everything else.
            if !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let dir = entry.path();
            let Ok(line) = std::fs::read_to_string(dir.join("stat")) else {
                continue; // exited between the listing and now
            };
            let Some(stat) = parse_stat(&line) else {
                continue;
            };

            let key = (stat.pid, stat.started_ticks);
            let ticks = stat.utime + stat.stime;
            let cpu_percent = self
                .previous
                .get(&key)
                .and_then(|before| ticks.checked_sub(before.ticks))
                .map_or(0.0, |delta| {
                    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                    let percent = (delta as f64 / elapsed_ticks * 100.0) as f32;
                    percent
                });
            current.insert(key, CpuTime { ticks });

            let uid = read_uid(&dir).unwrap_or(0);
            processes.push(Process {
                pid: stat.pid,
                ppid: stat.ppid,
                command: read_cmdline(&dir).unwrap_or_else(|| format!("[{}]", stat.comm)),
                name: stat.comm,
                state: stat.state,
                uid,
                user: resolve_user(uid, &self.users),
                cpu_percent,
                memory_bytes: stat.rss_pages * PAGE_SIZE,
                threads: stat.threads,
                started_ticks: stat.started_ticks,
            });
        }

        self.previous = current;
        // Busiest first: the reason anyone opens a process table.
        processes.sort_by(|a, b| {
            b.cpu_percent
                .total_cmp(&a.cpu_percent)
                .then(b.memory_bytes.cmp(&a.memory_bytes))
        });
        processes
    }

    /// Whether this is the first sample, in which case every CPU figure is zero rather than wrong.
    #[must_use]
    pub fn has_history(&self) -> bool {
        !self.previous.is_empty()
    }
}

/// The owning uid, from the `/proc/<pid>` directory itself.
///
/// `procfs` sets each process directory's owner to the process's real uid, so one `stat` on the
/// directory answers what parsing `/proc/<pid>/status` would.
///
/// Measured over this machine's 655 processes, the alternative is the single most expensive read in a
/// pass:
///
/// | | |
/// | --- | --- |
/// | `read_dir` on `/proc` | 2.0 ms |
/// | `stat` per process | 16.7 ms |
/// | `cmdline` per process | 15.0 ms |
/// | **`metadata` for the uid** | **2.1 ms** |
/// | *`status` per process, the alternative* | *25.5 ms* |
fn read_uid(dir: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(dir).ok().map(|meta| meta.uid())
}

/// The command line, with its NUL separators turned into spaces.
///
/// `None` for a kernel thread, whose `cmdline` is empty — which the caller renders as the name in
/// brackets rather than as a blank cell.
fn read_cmdline(dir: &Path) -> Option<String> {
    let raw = std::fs::read(dir.join("cmdline")).ok()?;
    if raw.is_empty() {
        return None;
    }
    let text: String = String::from_utf8_lossy(&raw)
        .split('\0')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Golden line, §P8. `/proc/1` on this machine.
    const INIT: &str = "1 (systemd) S 0 1 1 0 -1 4194560 34457 4598526 92 2433 841 559 70932 6591 20 0 1 0 24 172380160 3216 18446744073709551615 1 1 0 0 0 0 671173123 4096 1260 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0";

    #[test]
    fn a_stat_line_is_parsed_from_real_output() {
        let stat = parse_stat(INIT).unwrap();
        assert_eq!(stat.pid, 1);
        assert_eq!(stat.comm, "systemd");
        assert_eq!(stat.state, ProcessState::Sleeping);
        assert_eq!(stat.ppid, 0);
        assert_eq!(stat.utime, 841);
        assert_eq!(stat.stime, 559);
        assert_eq!(stat.threads, 1);
        assert_eq!(stat.started_ticks, 24);
        assert_eq!(stat.rss_pages, 3216);
    }

    /// # The trap
    ///
    /// Both of these are real processes on the machine this was written on. `next-server (v1` has a
    /// space *and* an unmatched opening bracket, which defeats splitting on whitespace and defeats
    /// splitting on the first `)`.
    #[test]
    fn a_name_containing_spaces_and_brackets_survives() {
        let line = "4343 (next-server (v1) S 3554 4343 4343 0 -1 4194560 67584 28 559 0 1287 129 0 0 20 0 11 0 1255 22978703360 61063";
        let stat = parse_stat(line).unwrap();
        assert_eq!(stat.comm, "next-server (v1");
        assert_eq!(stat.pid, 4343);
        assert_eq!(stat.ppid, 3554);
        assert_eq!(
            stat.utime, 1287,
            "the fields after the bracket must still line up"
        );
        assert_eq!(stat.threads, 11);

        let npm = "168264 (npm exec fireba) S 168063 168063 165382 34821 168063 4194304 25364 0 0 0 369 38 0 0 20 0 11 0 266807 1371516928 0";
        let stat = parse_stat(npm).unwrap();
        assert_eq!(stat.comm, "npm exec fireba");
        assert_eq!(stat.utime, 369);
    }

    /// A closing bracket inside the name is why the *last* one is used.
    #[test]
    fn a_name_containing_a_closing_bracket_survives() {
        let line = "99 (weird)name) R 1 99 99 0 -1 0 0 0 0 0 7 3 0 0 20 0 2 0 500 100 25";
        let stat = parse_stat(line).unwrap();
        assert_eq!(
            stat.comm, "weird)name",
            "splitting on the first bracket would truncate here"
        );
        assert_eq!(stat.utime, 7);
        assert_eq!(stat.stime, 3);
    }

    #[test]
    fn a_malformed_line_yields_nothing_rather_than_a_wrong_process() {
        assert!(parse_stat("").is_none());
        assert!(parse_stat("no brackets here").is_none());
        assert!(parse_stat("notanumber (thing) S 1").is_none());
        assert!(parse_stat("1 (unclosed S 0 1").is_none());
    }

    #[test]
    fn every_state_letter_the_kernel_uses_is_understood() {
        for (letter, expected) in [
            ("R", ProcessState::Running),
            ("S", ProcessState::Sleeping),
            ("D", ProcessState::DiskSleep),
            ("T", ProcessState::Stopped),
            ("t", ProcessState::Stopped),
            ("Z", ProcessState::Zombie),
            ("I", ProcessState::Idle),
            ("X", ProcessState::Other),
        ] {
            assert_eq!(ProcessState::parse(letter), expected);
        }
    }

    /// A zombie has already exited, so signalling it cannot do anything.
    #[test]
    fn a_zombie_is_not_signalable() {
        assert!(!ProcessState::Zombie.is_signalable());
        for alive in [
            ProcessState::Running,
            ProcessState::Sleeping,
            ProcessState::DiskSleep,
            ProcessState::Stopped,
            ProcessState::Idle,
        ] {
            assert!(alive.is_signalable(), "{alive:?}");
        }
    }

    #[test]
    fn passwd_is_parsed_into_uids() {
        let users = parse_passwd(
            "root:x:0:0:root:/root:/bin/bash\ndaemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin\nbroken\n",
        );
        assert_eq!(users.get(&0).map(String::as_str), Some("root"));
        assert_eq!(users.get(&1).map(String::as_str), Some("daemon"));
        assert_eq!(users.len(), 2, "a malformed line is skipped");
    }

    fn sandbox(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nix-proc-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write a fake `/proc/<pid>`.
    fn process(root: &Path, pid: u32, comm: &str, started: u64, ticks: u64, uid: u32) {
        let dir = root.join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        // utime carries the whole figure and stime is zero, which keeps the fixture readable.
        std::fs::write(
            dir.join("stat"),
            format!(
                "{pid} ({comm}) S 1 {pid} {pid} 0 -1 0 0 0 0 0 {ticks} 0 0 0 20 0 4 0 {started} 100 512"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("status"),
            format!("Name:\t{comm}\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
        )
        .unwrap();
        std::fs::write(dir.join("cmdline"), format!("/usr/bin/{comm}\0--flag\0")).unwrap();
    }

    fn passwd_at(dir: &Path) -> PathBuf {
        let path = dir.join("passwd");
        std::fs::write(
            &path,
            "root:x:0:0:root:/root:/bin/sh\nalice:x:1000:1000::/home/alice:/bin/sh\n",
        )
        .unwrap();
        path
    }

    /// The first sample has nothing to subtract, so CPU is zero rather than a lifetime average.
    #[test]
    fn the_first_sample_reports_no_cpu_rather_than_an_average_since_start() {
        let root = sandbox("first");
        let passwd = passwd_at(&root);
        let proc = root.join("proc");
        std::fs::create_dir_all(&proc).unwrap();
        // A process that has burned a great deal of CPU over a long life.
        process(&proc, 100, "hog", 50, 999_999, 1000);

        let mut sampler = ProcessSampler::rooted_at(proc.clone(), &passwd);
        let first = sampler.sample(std::time::Duration::from_secs(1));
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].cpu_percent, 0.0,
            "999,999 ticks since boot is not what it is doing now — this is the ps artefact"
        );
        assert!(sampler.has_history());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cpu_is_the_delta_over_the_interval() {
        let root = sandbox("delta");
        let passwd = passwd_at(&root);
        let proc = root.join("proc");
        std::fs::create_dir_all(&proc).unwrap();
        process(&proc, 100, "worker", 50, 1000, 1000);

        let mut sampler = ProcessSampler::rooted_at(proc.clone(), &passwd);
        sampler.sample(std::time::Duration::from_secs(1));

        // 50 more ticks in one second, at 100 ticks a second, is half a core.
        process(&proc, 100, "worker", 50, 1050, 1000);
        let second = sampler.sample(std::time::Duration::from_secs(1));
        assert!(
            (second[0].cpu_percent - 50.0).abs() < 0.01,
            "got {}",
            second[0].cpu_percent
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// A threaded process can exceed one core, and the figure says so rather than clamping.
    #[test]
    fn a_process_using_several_cores_reads_above_one_hundred() {
        let root = sandbox("threaded");
        let passwd = passwd_at(&root);
        let proc = root.join("proc");
        std::fs::create_dir_all(&proc).unwrap();
        process(&proc, 100, "builder", 50, 0, 1000);

        let mut sampler = ProcessSampler::rooted_at(proc.clone(), &passwd);
        sampler.sample(std::time::Duration::from_secs(1));
        // 400 ticks in one second is four cores' worth.
        process(&proc, 100, "builder", 50, 400, 1000);

        let second = sampler.sample(std::time::Duration::from_secs(1));
        assert!(
            (second[0].cpu_percent - 400.0).abs() < 0.01,
            "clamping to 100 would hide that a build is using four cores: {}",
            second[0].cpu_percent
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// # The reason delta state is keyed on more than a pid
    ///
    /// Pids are reused. A new process inheriting the old one's counters would show an enormous spike
    /// the moment it appeared.
    #[test]
    fn a_reused_pid_does_not_inherit_the_previous_processes_cpu() {
        let root = sandbox("reuse");
        let passwd = passwd_at(&root);
        let proc = root.join("proc");
        std::fs::create_dir_all(&proc).unwrap();
        process(&proc, 100, "old", 50, 500_000, 1000);

        let mut sampler = ProcessSampler::rooted_at(proc.clone(), &passwd);
        sampler.sample(std::time::Duration::from_secs(1));

        // Same pid, different start time: a different process entirely.
        std::fs::remove_dir_all(proc.join("100")).unwrap();
        process(&proc, 100, "new", 900_000, 10, 1000);

        let second = sampler.sample(std::time::Duration::from_secs(1));
        assert_eq!(second[0].name, "new");
        assert_eq!(
            second[0].cpu_percent, 0.0,
            "a freshly started process has no history, whatever its pid used to belong to"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn counters_going_backwards_report_zero_rather_than_a_wrapped_figure() {
        let root = sandbox("backwards");
        let passwd = passwd_at(&root);
        let proc = root.join("proc");
        std::fs::create_dir_all(&proc).unwrap();
        process(&proc, 100, "thing", 50, 1000, 1000);

        let mut sampler = ProcessSampler::rooted_at(proc.clone(), &passwd);
        sampler.sample(std::time::Duration::from_secs(1));
        process(&proc, 100, "thing", 50, 5, 1000);

        let second = sampler.sample(std::time::Duration::from_secs(1));
        assert_eq!(second[0].cpu_percent, 0.0);

        std::fs::remove_dir_all(&root).ok();
    }

    /// The resolution, tested directly.
    ///
    /// A process's uid comes from its `/proc/<pid>` directory's owner, which a fixture cannot set
    /// without being root — so this tests the lookup rather than pretending to test the read.
    #[test]
    fn a_uid_resolves_to_a_name_or_to_its_own_number() {
        let users =
            parse_passwd("root:x:0:0::/root:/bin/sh\nalice:x:1000:1000::/home/alice:/bin/sh\n");
        assert_eq!(resolve_user(0, &users), "root");
        assert_eq!(resolve_user(1000, &users), "alice");
        assert_eq!(
            resolve_user(90210, &users),
            "90210",
            "a uid with no passwd entry — a container's — shows as itself rather than as blank"
        );
    }

    #[test]
    fn a_sampled_process_always_has_a_user_string() {
        let root = sandbox("users");
        let passwd = passwd_at(&root);
        let proc = root.join("proc");
        std::fs::create_dir_all(&proc).unwrap();
        process(&proc, 100, "mine", 50, 0, 1000);

        let mut sampler = ProcessSampler::rooted_at(proc.clone(), &passwd);
        let all = sampler.sample(std::time::Duration::from_secs(1));
        assert!(
            !all[0].user.is_empty(),
            "whatever the owner is, the column is never blank"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_kernel_thread_shows_its_name_in_brackets() {
        let root = sandbox("kthread");
        let passwd = passwd_at(&root);
        let proc = root.join("proc");
        let dir = proc.join("2");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("stat"),
            "2 (kthreadd) S 0 0 0 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 24 0 0",
        )
        .unwrap();
        std::fs::write(dir.join("status"), "Uid:\t0\t0\t0\t0\n").unwrap();
        std::fs::write(dir.join("cmdline"), "").unwrap();

        let mut sampler = ProcessSampler::rooted_at(proc.clone(), &passwd);
        let all = sampler.sample(std::time::Duration::from_secs(1));
        assert_eq!(
            all[0].command, "[kthreadd]",
            "an empty cmdline means a kernel thread, not a process with no arguments"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn non_numeric_entries_in_proc_are_not_processes() {
        let root = sandbox("nonnumeric");
        let passwd = passwd_at(&root);
        let proc = root.join("proc");
        std::fs::create_dir_all(proc.join("self")).unwrap();
        std::fs::create_dir_all(proc.join("meminfo")).unwrap();
        process(&proc, 100, "real", 50, 0, 1000);

        let mut sampler = ProcessSampler::rooted_at(proc.clone(), &passwd);
        let all = sampler.sample(std::time::Duration::from_secs(1));
        assert_eq!(all.len(), 1, "`self` and `meminfo` are not processes");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_vanished_process_is_skipped_rather_than_failing_the_sample() {
        let root = sandbox("vanished");
        let passwd = passwd_at(&root);
        let proc = root.join("proc");
        std::fs::create_dir_all(&proc).unwrap();
        process(&proc, 100, "alive", 50, 0, 1000);
        // A directory with no readable `stat`: exactly how a process that exited mid-listing looks.
        std::fs::create_dir_all(proc.join("999")).unwrap();

        let mut sampler = ProcessSampler::rooted_at(proc.clone(), &passwd);
        let all = sampler.sample(std::time::Duration::from_secs(1));
        assert_eq!(
            all.len(),
            1,
            "one exiting process must not lose the other 651"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_zero_interval_does_not_divide_into_an_absurd_figure() {
        let root = sandbox("zero");
        let passwd = passwd_at(&root);
        let proc = root.join("proc");
        std::fs::create_dir_all(&proc).unwrap();
        process(&proc, 100, "thing", 50, 0, 1000);

        let mut sampler = ProcessSampler::rooted_at(proc.clone(), &passwd);
        sampler.sample(std::time::Duration::from_secs(1));
        process(&proc, 100, "thing", 50, 100, 1000);

        let second = sampler.sample(std::time::Duration::ZERO);
        assert!(
            second[0].cpu_percent.is_finite() && second[0].cpu_percent <= 10_000.0,
            "got {}",
            second[0].cpu_percent
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_table_is_ordered_by_cpu_then_memory() {
        let root = sandbox("order");
        let passwd = passwd_at(&root);
        let proc = root.join("proc");
        std::fs::create_dir_all(&proc).unwrap();
        for pid in [100u32, 101, 102] {
            process(&proc, pid, &format!("p{pid}"), 50, 0, 1000);
        }
        let mut sampler = ProcessSampler::rooted_at(proc.clone(), &passwd);
        sampler.sample(std::time::Duration::from_secs(1));

        process(&proc, 100, "p100", 50, 10, 1000);
        process(&proc, 101, "p101", 50, 90, 1000);
        process(&proc, 102, "p102", 50, 50, 1000);

        let all = sampler.sample(std::time::Duration::from_secs(1));
        assert_eq!(all[0].pid, 101, "busiest first");
        assert_eq!(all[2].pid, 100);

        std::fs::remove_dir_all(&root).ok();
    }

    // ---- against this machine ----

    #[test]
    fn this_machines_processes_parse() {
        let mut sampler = ProcessSampler::new();
        let all = sampler.sample(std::time::Duration::from_secs(1));
        if all.is_empty() {
            return; // no /proc in this environment
        }

        assert!(all.len() > 10, "a running machine has processes");
        assert!(all.iter().any(|p| p.pid == 1), "pid 1 must be there");

        for process in &all {
            assert!(!process.name.is_empty(), "pid {} has no name", process.pid);
            assert!(!process.command.is_empty());
            assert!(!process.user.is_empty());
            assert!(process.cpu_percent >= 0.0);
            assert!(
                process.threads >= 1,
                "pid {} claims no threads",
                process.pid
            );
        }

        // Every stat line on this machine parsed, including the awkward names.
        let listed = std::fs::read_dir("/proc")
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| {
                        e.file_name()
                            .to_str()
                            .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
                    })
                    .count()
            })
            .unwrap_or(0);
        // Some will have exited between the two listings; a large shortfall means a parsing failure.
        assert!(
            all.len() * 10 > listed * 9,
            "parsed {} of about {listed} processes — too many were lost",
            all.len()
        );
    }
}
