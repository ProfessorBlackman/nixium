// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Client side of the helper: spawns it, authenticates once, and exchanges typed messages.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::caps::{self, Capability};
use crate::error::{AppError, Cause, ErrorCode, Result};

use super::protocol::{Op, OpResult, PROTOCOL_VERSION, Request, Response};

/// How the helper process is started.
///
/// This exists so tests can exercise the whole protocol without `pkexec`, which cannot run in CI.
/// The only untested path is escalation itself, which is a single `Command` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// Production: escalate through polkit. One authentication for the session's lifetime.
    Pkexec { helper_path: PathBuf },
    /// Testing and development: run the helper as the current user, unprivileged.
    Direct { helper_path: PathBuf },
}

impl Transport {
    /// The installed helper, escalated. Fails if polkit is unavailable rather than pretending.
    pub fn production() -> Result<Self> {
        if !caps::registry().has(Capability::Pkexec) {
            return Err(AppError::new(
                ErrorCode::HelperUnavailable,
                "Actions that need administrator rights are unavailable.",
            )
            .with_remedy(
                "polkit (pkexec) was not found. Install polkit and a graphical authentication agent.",
            ));
        }
        Ok(Self::Pkexec {
            helper_path: default_helper_path(),
        })
    }

    fn helper_path(&self) -> &PathBuf {
        match self {
            Self::Pkexec { helper_path } | Self::Direct { helper_path } => helper_path,
        }
    }

    fn build_command(&self) -> Command {
        match self {
            Self::Pkexec { helper_path } => {
                let mut c = Command::new("pkexec");
                c.arg(helper_path).arg("--serve");
                c
            }
            Self::Direct { helper_path } => {
                let mut c = Command::new(helper_path);
                c.arg("--serve");
                c
            }
        }
    }
}

/// Where the helper is installed. Overridable for development via `NIX_HELPER_PATH`.
fn default_helper_path() -> PathBuf {
    if let Some(p) = std::env::var_os("NIX_HELPER_PATH").filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    PathBuf::from("/usr/libexec/nix/nix-helper")
}

/// A live privileged session.
///
/// Dropping it closes the helper's stdin, which is the helper's signal to exit — so the privileged
/// process cannot outlive the client that authorised it.
#[derive(Debug)]
pub struct Client {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: AtomicU64,
    uid: u32,
}

impl Client {
    /// Start the helper and complete a handshake.
    ///
    /// Refuses a helper whose protocol version differs: a version-skewed privileged process is not
    /// something to guess about.
    pub fn connect(transport: &Transport) -> Result<Self> {
        let helper = transport.helper_path();
        if !helper.exists() {
            return Err(AppError::new(
                ErrorCode::HelperUnavailable,
                "The nix helper is not installed.",
            )
            .with_path(helper)
            .with_remedy(
                "Reinstall nix, or set NIX_HELPER_PATH when running a development build.",
            ));
        }

        let mut child = transport
            .build_command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                AppError::from_io(&e, "start the helper that performs administrator actions")
                    .with_path(helper)
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::internal("Helper stdin was not captured."))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::internal("Helper stdout was not captured."))?;

        let mut client = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: AtomicU64::new(1),
            uid: u32::MAX,
        };

        match client.request(&Op::Ping) {
            Ok(OpResult::Pong { protocol, uid, .. }) => {
                if protocol != PROTOCOL_VERSION {
                    client.shutdown();
                    return Err(AppError::new(
                        ErrorCode::VersionMismatch,
                        format!(
                            "The installed helper speaks protocol {protocol}, this build speaks {PROTOCOL_VERSION}."
                        ),
                    )
                    .with_remedy("Reinstall nix so both halves come from the same build."));
                }
                client.uid = uid;
                Ok(client)
            }
            Ok(other) => {
                client.shutdown();
                Err(AppError::internal(format!(
                    "Helper answered a ping with {other:?}"
                )))
            }
            Err(e) => {
                client.shutdown();
                // A refused authorisation shows up as the child exiting before answering. Say so
                // plainly — this is precisely what Stacer reported as success.
                Err(
                    if e.code == ErrorCode::Io || e.code == ErrorCode::HelperUnavailable {
                        AppError::new(
                            ErrorCode::AuthDenied,
                            "Administrator rights were not granted.",
                        )
                        .with_remedy("The action was cancelled. Nothing was changed.")
                        .with_cause(Cause::Other {
                            detail: e.to_string(),
                        })
                    } else {
                        e
                    },
                )
            }
        }
    }

    /// Effective uid the helper reported. `0` means genuinely elevated.
    #[must_use]
    pub const fn helper_uid(&self) -> u32 {
        self.uid
    }

    /// Whether the helper is actually running with privilege.
    #[must_use]
    pub const fn is_elevated(&self) -> bool {
        self.uid == 0
    }

    /// Send one operation and wait for its response.
    pub fn request(&mut self, op: &Op) -> Result<OpResult> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = Request { id, op: op.clone() };

        let line = serde_json::to_string(&request).map_err(|e| {
            AppError::internal("Could not encode a helper request.").with_cause(Cause::Other {
                detail: e.to_string(),
            })
        })?;

        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .map_err(|e| AppError::from_io(&e, "send a request to the helper"))?;

        let mut response_line = String::new();
        let read = self
            .stdout
            .read_line(&mut response_line)
            .map_err(|e| AppError::from_io(&e, "read the helper's reply"))?;

        if read == 0 {
            return Err(AppError::new(
                ErrorCode::HelperUnavailable,
                "The helper stopped before answering.",
            ));
        }

        let response: Response = serde_json::from_str(response_line.trim()).map_err(|e| {
            AppError::new(ErrorCode::Parse, "The helper sent something unreadable.").with_cause(
                Cause::Malformed {
                    source: "helper response".into(),
                    detail: e.to_string(),
                },
            )
        })?;

        if response.id != id && response.id != 0 {
            return Err(AppError::internal(format!(
                "Helper answered request {} with id {}",
                id, response.id
            )));
        }

        response.result
    }

    /// Close stdin and reap the child. Called by [`Drop`], exposed for the error paths.
    fn shutdown(&mut self) {
        // Closing stdin is the helper's cue to exit; then reap so we leave no zombie.
        let _ = self.stdin.flush();
        if let Err(e) = self.child.kill() {
            tracing::debug!(error = %e, "helper had already exited");
        }
        if let Err(e) = self.child.wait() {
            tracing::debug!(error = %e, "could not reap the helper");
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Connect for a test, turning a stale-binary version mismatch into a clear instruction.
    ///
    /// The protocol version guard is doing its job when it refuses an old binary, but a raw panic
    /// on an unwrap does not say so. `cargo test` does not rebuild the helper; `make test` does.
    fn connect_for_test(transport: &Transport) -> Option<Client> {
        match Client::connect(transport) {
            Ok(client) => Some(client),
            Err(e) if e.code == ErrorCode::VersionMismatch => {
                panic!(
                    "the helper binary is stale ({}). Run `make test`, or \
                     `cargo build -p nix-helper` first — `cargo test` does not rebuild it.",
                    e.message
                );
            }
            Err(e) => panic!("handshake failed: {e}"),
        }
    }

    /// Path to the freshly built helper binary next to the test executable.
    fn built_helper() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        // .../target/debug/deps/nix_core-<hash>  ->  .../target/debug/nix-helper
        let dir = exe.parent()?.parent()?;
        let candidate = dir.join("nix-helper");
        candidate.exists().then_some(candidate)
    }

    #[test]
    fn direct_transport_completes_a_handshake_and_reads_a_file() {
        let Some(helper) = built_helper() else {
            eprintln!("skipping: nix-helper not built yet");
            return;
        };
        let transport = Transport::Direct {
            helper_path: helper,
        };
        let mut client = connect_for_test(&transport).expect("handshake should succeed");

        // Run as the current user in tests, so it must NOT claim elevation.
        assert!(!client.is_elevated(), "a direct-spawned helper is not root");

        let result = client
            .request(&Op::ReadTextFile {
                path: PathBuf::from("/proc/sys/kernel/osrelease"),
            })
            .unwrap();
        match result {
            OpResult::Text { content } => assert!(!content.trim().is_empty()),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn client_surfaces_a_rejection_as_a_typed_error() {
        let Some(helper) = built_helper() else {
            eprintln!("skipping: nix-helper not built yet");
            return;
        };
        let mut client = connect_for_test(&Transport::Direct {
            helper_path: helper,
        })
        .expect("handshake should succeed");
        let err = client
            .request(&Op::ReadTextFile {
                path: PathBuf::from("/home/someone/.ssh/id_rsa"),
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::HelperRejected);
        assert!(!err.message.is_empty());
    }

    #[test]
    fn missing_helper_is_reported_with_a_remedy() {
        let err = Client::connect(&Transport::Direct {
            helper_path: PathBuf::from("/nonexistent/nix-helper"),
        })
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::HelperUnavailable);
        assert!(
            err.remedy.is_some(),
            "an unavailable helper must say what to do"
        );
    }

    #[test]
    fn helper_path_is_overridable_for_development() {
        // Default is the installed location; the override is read from the environment at call
        // time, which is what lets a dev build point at target/debug.
        assert!(default_helper_path().is_absolute());
    }
}
