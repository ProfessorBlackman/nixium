//! The nix privileged helper.
//!
//! Runs as root, authorised by polkit, and accepts a **closed set** of typed operations over stdin
//! and stdout. It never accepts a free-form command or argument vector: the operation enum in
//! [`nix_core::helper`] is the security boundary, and every addition to it is reviewed as its own
//! small diff.
//!
//! Invoked as `nix-helper --serve`, normally by the application through `pkexec`. Reads
//! line-delimited JSON requests, writes line-delimited JSON responses, audits every decision, and
//! exits when its peer closes stdin — so it cannot outlive the app that authorised it.

use std::path::PathBuf;
use std::process::ExitCode;

use nix_core::helper::{Audit, FileAudit, serve};

/// Where privileged decisions are recorded. Root-owned, outside any user's control.
const AUDIT_LOG: &str = "/var/log/nix/helper-audit.log";

/// Audit destination: the root-owned log when we can write it, otherwise a per-user path.
///
/// A development run as an ordinary user must still leave an audit trail, so the fallback exists —
/// but it is a fallback, not a choice: in production the helper is root and writes [`AUDIT_LOG`].
fn audit_path() -> PathBuf {
    let primary = PathBuf::from(AUDIT_LOG);
    if let Some(parent) = primary.parent() {
        // Probe by trying to create the directory; only root will succeed under /var/log.
        if std::fs::create_dir_all(parent).is_ok() {
            return primary;
        }
    }
    nix_core::paths::state_dir()
        .map(|d| d.join("helper-audit.log"))
        .unwrap_or(primary)
}

fn usage() -> ExitCode {
    eprintln!(
        "nix-helper {}\n\n\
         The privileged helper for nix. Not intended to be run by hand.\n\n\
         Usage:\n  nix-helper --serve      serve requests on stdin/stdout\n  \
         nix-helper --version    print the version\n\n\
         Reads line-delimited JSON requests and writes line-delimited JSON responses.\n\
         Exits when stdin closes.",
        nix_core::VERSION
    );
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.as_slice() {
        [flag] if flag == "--serve" => {
            let mut audit = FileAudit::open(&audit_path());
            let stdin = std::io::stdin().lock();
            let stdout = std::io::stdout().lock();

            match serve(stdin, stdout, &mut audit) {
                Ok(handled) => {
                    audit.record(&format!("exit ok handled={handled}"));
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    audit.record(&format!("exit error: {e}"));
                    ExitCode::FAILURE
                }
            }
        }
        [flag] if flag == "--version" => {
            println!("nix-helper {}", nix_core::VERSION);
            ExitCode::SUCCESS
        }
        _ => usage(),
    }
}
