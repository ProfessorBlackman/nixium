// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The `nix snapshot` subcommand. `STO-16`.
//!
//! The growth-history timer's `ExecStart` names **this** binary with a subcommand, not a second
//! executable. Two artefacts is two things to keep versioned, packaged and signed, and an `ExecStart`
//! pointing at something that has moved is a job that fails silently every day.
//!
//! So `run()` checks the argument list before Tauri is touched. This path must never open a window: it
//! runs from a systemd timer with no display, and a graphical toolkit trying to connect to one would
//! fail in a way that looks like the scan failing.

use std::time::{SystemTime, UNIX_EPOCH};

/// Handle a subcommand and exit, when invoked with one.
///
/// Returns the process exit code to use, or `None` when this is an ordinary launch.
pub(crate) fn run_if_requested() -> Option<i32> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("snapshot") => Some(take_sample(std::env::args().any(|a| a == "--quiet"))),
        Some("helper-probe") => Some(probe_helper()),
        // Without these, `nix --version` opened a window. That is not merely unhelpful: it is the
        // reason `PLT-5`'s "installs and runs" could not be checked in CI, where there is no display
        // to open one on. A packaged binary you cannot ask a question of is a packaged binary nobody
        // verifies.
        Some("--version" | "-V") => {
            println!("nix {}", env!("CARGO_PKG_VERSION"));
            Some(0)
        }
        Some("--help" | "-h") => {
            print_help();
            Some(0)
        }
        _ => None,
    }
}

/// What this binary does when asked.
///
/// Deliberately short. It is a graphical application with two maintenance subcommands, not a CLI, and
/// a help text that pretends otherwise sends people looking for flags that do not exist.
fn print_help() {
    println!(
        "\
nix {version} — find out where your disk went, and reclaim it safely

Usage:
  nix                    open the application
  nix snapshot [--quiet] record one storage sample, for the growth history
  nix helper-probe       check the privileged helper end to end (asks for a password)
  nix --version          print the version
  nix --help             print this

Everything else happens in the application. Logs are written to
$XDG_STATE_HOME/nix/logs.",
        version = env!("CARGO_PKG_VERSION")
    );
}

/// Start the helper under `pkexec`, complete a handshake, read one allow-listed file, and exit.
///
/// The same thing the About view's button does, reachable from a terminal — which matters because
/// this is the one path the test suite cannot cover. Every helper test spawns the binary directly, so
/// `pkexec` itself, the polkit policy's wording, and `auth_admin_keep`'s one-prompt-per-session are
/// only ever exercised by a person.
///
/// Run from a terminal, `pkexec` uses its own text agent, so this needs no desktop session.
fn probe_helper() -> i32 {
    use nix_core::helper::{Client, Op, OpResult, Transport};

    let transport = match Transport::production() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("nix helper-probe: {e}");
            if let Some(remedy) = e.remedy {
                eprintln!("  {remedy}");
            }
            return 1;
        }
    };

    println!("nix helper-probe: starting the helper under pkexec — expect one prompt");
    let mut client = match Client::connect(&transport) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("nix helper-probe: {e}");
            if let Some(remedy) = e.remedy {
                eprintln!("  {remedy}");
            }
            return 1;
        }
    };

    println!("  handshake ok");
    println!("  helper uid: {}", client.helper_uid());
    println!("  elevated:   {}", client.is_elevated());

    // A read the helper's allow-list permits, so the whole request/response path is exercised.
    match client.request(&Op::ReadTextFile {
        path: std::path::PathBuf::from("/proc/sys/kernel/osrelease"),
    }) {
        Ok(OpResult::Text { content }) => println!("  kernel:     {}", content.trim()),
        Ok(other) => {
            eprintln!("nix helper-probe: unexpected answer {other:?}");
            return 1;
        }
        Err(e) => {
            eprintln!("nix helper-probe: {e}");
            return 1;
        }
    }

    // And one the allow-list must refuse, so the boundary is shown working rather than assumed.
    match client.request(&Op::ReadTextFile {
        path: std::path::PathBuf::from("/etc/shadow"),
    }) {
        Err(e) => println!("  refused /etc/shadow, as it must: {e}"),
        Ok(_) => {
            eprintln!("nix helper-probe: FAILED — the helper read /etc/shadow");
            return 1;
        }
    }

    if !client.is_elevated() {
        eprintln!("nix helper-probe: the helper is not running as root");
        return 1;
    }
    println!("nix helper-probe: ok");
    0
}

/// Scan the home directory and record one sample.
///
/// A full scan rather than an incremental refresh. The specification asked for the latter on the basis
/// of `STO-18`, which was superseded — the scan is now about twice as fast with bounded memory, so a
/// home directory here takes 28 seconds, and under `Nice=19` with `IOSchedulingClass=idle` once a day
/// that buys a correct answer for less than a second code path and a staleness model would cost.
fn take_sample(quiet: bool) -> i32 {
    let say = |message: &str| {
        if !quiet {
            println!("nix snapshot: {message}");
        }
    };

    let Some(home) = nix_core::paths::home_dir() else {
        eprintln!("nix snapshot: could not resolve a home directory to scan");
        return 1;
    };

    let history = match nix_core::history::History::discover() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("nix snapshot: {e}");
            return 1;
        }
    };

    // The previous sample's total is a good size hint, which lets the scan settle its node threshold
    // without a second traversal.
    let previous = history.samples();
    let hint = previous.last().map(|s| s.total_allocated);

    say(&format!("scanning {}", home.display()));
    let options = nix_core::scan::Options::new(&home).size_hint(hint);
    let result = match nix_core::scan::scan_quiet(options.clone(), nix_core::op::CancelToken::new())
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("nix snapshot: {e}");
            return 1;
        }
    };

    // Keep the scan cache warm too, so the next interactive open is instant. The timer is doing the
    // expensive part anyway.
    if let Ok(cache) = nix_core::cache::Cache::discover() {
        if let Err(e) = cache.store(&options, &result) {
            // Not fatal: a cache that cannot be written is a missed optimisation, not a failed job.
            say(&format!("could not update the scan cache: {e}"));
        }
    }

    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let sample = nix_core::history::Sample::from_scan(at, &result, 40);
    if let Err(e) = history.record(&sample) {
        eprintln!("nix snapshot: {e}");
        return 1;
    }

    say(&format!(
        "recorded {} across {} files",
        nix_core::format_bytes(sample.total_allocated),
        result.files
    ));
    0
}
