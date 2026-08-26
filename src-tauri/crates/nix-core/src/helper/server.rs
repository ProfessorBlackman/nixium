//! The helper's request loop, validation and audit log. Runs as root.
//!
//! Everything here executes with full privilege, so the code is written to be readable rather than
//! clever, and every path a request can take ends in an audited outcome.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{AppError, Cause, ErrorCode, Result};

use super::protocol::{Op, OpResult, PROTOCOL_VERSION, Request, Response};

/// Files that [`Op::ReadTextFile`] may read, matched **exactly**.
///
/// This started as a list of permitted *directory roots* — `/proc`, `/sys`, `/etc`, `/var/log` —
/// and the tests in this module caught why that was wrong: reading any file under `/etc` as root
/// includes `/etc/shadow`, and anywhere under `/proc` includes other users' `/proc/<pid>/environ`.
/// A prefix allow-list on a root-privileged read is privilege escalation with extra steps.
///
/// Exact paths instead. The properties this buys are worth the inconvenience:
///
/// - **No traversal is possible.** A path either is one of these strings or it is refused, so
///   `..`, `.`, and doubled separators need no special handling.
/// - **No symlink ambiguity.** The set is fixed, so there is no attacker-controlled component to
///   redirect. (`/etc/os-release` is itself a symlink on most systems; that is fine, because the
///   *request* is matched lexically and the target is not caller-controlled.)
/// - **It is auditable by reading it.**
///
/// Every entry here is currently world-readable, so this operation grants no privilege at all. That
/// is deliberate for Phase 0: it proves the transport, dispatch, validation and audit loop without
/// widening the privileged surface. Reads that genuinely need privilege arrive with the features
/// that need them, each reviewed as its own diff — which is the rule this list exists to enforce.
const READABLE_FILES: &[&str] = &[
    "/etc/fstab",
    "/etc/os-release",
    "/proc/self/mountinfo",
    "/proc/sys/kernel/osrelease",
];

/// Refuse after this many unparseable lines. A confused or hostile peer should not be able to keep
/// a root process spinning indefinitely.
const MAX_MALFORMED: u32 = 8;

/// Cap on a single request line, so a peer cannot exhaust memory in the helper.
const MAX_LINE_BYTES: u64 = 64 * 1024;

/// Where privileged actions are recorded.
pub trait Audit {
    /// Record one decision. Called **before** the response is written, so a crash mid-response
    /// still leaves evidence of what was attempted.
    fn record(&mut self, entry: &str);
}

/// Audit sink that appends to a file and mirrors to stderr.
///
/// stderr matters because the helper is normally a child of the app, and because under
/// `systemd-run` or a unit it lands in the journal.
pub struct FileAudit {
    file: Option<std::fs::File>,
}

impl FileAudit {
    /// Open the audit log, falling back to stderr alone if it cannot be written.
    #[must_use]
    pub fn open(path: &Path) -> Self {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok();
        if file.is_none() {
            eprintln!(
                "nix-helper: audit log unavailable at {}, using stderr only",
                path.display()
            );
        }
        Self { file }
    }
}

impl Audit for FileAudit {
    fn record(&mut self, entry: &str) {
        eprintln!("nix-helper: {entry}");
        if let Some(f) = &mut self.file {
            // A failed audit write must not take the process down, but it must be visible.
            if writeln!(f, "{entry}").is_err() {
                eprintln!("nix-helper: audit write failed");
            }
            f.flush().ok();
        }
    }
}

/// Effective uid of this process.
fn effective_uid() -> u32 {
    // Reading /proc avoids pulling in libc for one number. Field 3 of the `Uid:` line is the
    // effective uid. If it cannot be read we report `u32::MAX`, which no real uid equals, so the
    // client's "am I actually elevated?" check fails closed.
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|rest| rest.split_whitespace().nth(1).map(str::to_owned))
        })
        .and_then(|uid| uid.parse().ok())
        .unwrap_or(u32::MAX)
}

/// Refuse anything that is not an exact member of [`READABLE_FILES`].
///
/// Lexical, exact, and total: there is no normalisation step to get wrong. An earlier version of
/// this function checked path *prefixes* and tried to reject `.` and `..` segments — which failed
/// twice over, because [`Path::components`] silently normalises `.` away (so the check never
/// fired), and because prefix matching on a privileged read is unsafe in the first place.
fn validate_readable(path: &Path) -> Result<PathBuf> {
    let refused =
        |detail: &str| AppError::new(ErrorCode::HelperRejected, detail.to_string()).with_path(path);

    let Some(as_str) = path.to_str() else {
        return Err(refused("The helper only accepts valid UTF-8 paths."));
    };

    if !READABLE_FILES.contains(&as_str) {
        return Err(refused("The helper is not permitted to read that file.")
            .with_remedy("Only a fixed list of system files can be read this way."));
    }

    let meta = std::fs::metadata(path)
        .map_err(|e| AppError::from_io(&e, format!("read {}", path.display())).with_path(path))?;
    if !meta.is_file() {
        return Err(refused("That path is not a regular file."));
    }

    Ok(path.to_path_buf())
}

/// Execute one validated operation.
fn dispatch(op: &Op) -> Result<OpResult> {
    match op {
        Op::Ping => Ok(OpResult::Pong {
            protocol: PROTOCOL_VERSION,
            version: crate::VERSION.to_string(),
            uid: effective_uid(),
        }),
        Op::ReadTextFile { path } => {
            let path = validate_readable(path)?;
            let content = std::fs::read_to_string(&path).map_err(|e| {
                AppError::from_io(&e, format!("read {}", path.display())).with_path(&path)
            })?;
            Ok(OpResult::Text { content })
        }
    }
}

/// Serve requests until the peer closes the connection.
///
/// Returns the number of requests handled. Exits early — with an error — if the peer sends more
/// than [`MAX_MALFORMED`] unparseable lines.
pub fn serve<R: std::io::Read, W: Write, A: Audit>(
    input: R,
    mut output: W,
    audit: &mut A,
) -> Result<u64> {
    audit.record(&format!(
        "start version={} protocol={} uid={}",
        crate::VERSION,
        PROTOCOL_VERSION,
        effective_uid()
    ));

    let mut reader = BufReader::new(input);
    let mut handled = 0u64;
    let mut malformed = 0u32;
    let mut line = String::new();

    loop {
        line.clear();
        // Bounded read, so an endless line cannot exhaust memory.
        let read = {
            let mut limited = (&mut reader).take(MAX_LINE_BYTES);
            limited.read_line(&mut line)
        };

        match read {
            Ok(0) => {
                audit.record(&format!("eof handled={handled}"));
                return Ok(handled);
            }
            Ok(_) => {}
            Err(e) => {
                audit.record(&format!("read error: {e}"));
                return Err(AppError::from_io(&e, "read from the application"));
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Request = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                malformed += 1;
                audit.record(&format!("rejected malformed request ({malformed}): {e}"));
                // Reply with id 0 rather than staying silent, so a client bug is diagnosable.
                let response = Response {
                    id: 0,
                    result: Err(AppError::new(
                        ErrorCode::HelperRejected,
                        "The helper could not understand that request.",
                    )
                    .with_cause(Cause::Malformed {
                        source: "helper request".into(),
                        detail: e.to_string(),
                    })),
                };
                write_response(&mut output, &response)?;
                if malformed >= MAX_MALFORMED {
                    audit.record("too many malformed requests, exiting");
                    return Err(AppError::new(
                        ErrorCode::HelperRejected,
                        "Too many malformed requests.",
                    ));
                }
                continue;
            }
        };

        let result = dispatch(&request.op);
        match &result {
            Ok(_) => audit.record(&format!("ok id={} op={}", request.id, request.op.name())),
            Err(e) => audit.record(&format!(
                "denied id={} op={} code={} detail={}",
                request.id,
                request.op.name(),
                e.code,
                e.message
            )),
        }

        write_response(
            &mut output,
            &Response {
                id: request.id,
                result,
            },
        )?;
        handled += 1;
    }
}

fn write_response<W: Write>(output: &mut W, response: &Response) -> Result<()> {
    let line = serde_json::to_string(response).map_err(|e| {
        AppError::internal("Could not encode a helper response.").with_cause(Cause::Other {
            detail: e.to_string(),
        })
    })?;
    output
        .write_all(line.as_bytes())
        .and_then(|()| output.write_all(b"\n"))
        .and_then(|()| output.flush())
        .map_err(|e| AppError::from_io(&e, "reply to the application"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct VecAudit(Vec<String>);
    impl Audit for VecAudit {
        fn record(&mut self, entry: &str) {
            self.0.push(entry.to_string());
        }
    }

    fn run(input: &str) -> (Vec<Response>, VecAudit, Result<u64>) {
        let mut out = Vec::new();
        let mut audit = VecAudit::default();
        let res = serve(input.as_bytes(), &mut out, &mut audit);
        let responses = String::from_utf8(out)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        (responses, audit, res)
    }

    #[test]
    fn ping_reports_protocol_and_uid() {
        let (responses, audit, res) = run("{\"id\":1,\"op\":\"ping\"}\n");
        assert_eq!(res.unwrap(), 1);
        assert_eq!(responses.len(), 1);
        match &responses[0].result {
            Ok(OpResult::Pong { protocol, uid, .. }) => {
                assert_eq!(*protocol, PROTOCOL_VERSION);
                assert_ne!(*uid, u32::MAX, "uid must be readable on Linux");
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(audit.0.iter().any(|l| l.starts_with("start ")));
        assert!(audit.0.iter().any(|l| l.contains("op=ping")));
    }

    #[test]
    fn reads_a_file_on_the_allow_list() {
        let (responses, _, res) =
            run("{\"id\":2,\"op\":\"read_text_file\",\"path\":\"/proc/sys/kernel/osrelease\"}\n");
        assert_eq!(res.unwrap(), 1);
        match &responses[0].result {
            Ok(OpResult::Text { content }) => assert!(!content.trim().is_empty()),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// The escalation this operation must not enable. Each of these is either root-only or
    /// another user's private data, and an earlier prefix-based allow-list permitted all of them.
    #[test]
    fn refuses_the_files_a_prefix_allow_list_would_have_leaked() {
        for path in [
            "/etc/shadow",
            "/etc/gshadow",
            "/etc/passwd",
            "/proc/1/environ",
            "/proc/self/environ",
            "/var/log/auth.log",
            "/root/.bashrc",
            "/home/someone/.ssh/id_rsa",
        ] {
            let line = format!("{{\"id\":3,\"op\":\"read_text_file\",\"path\":\"{path}\"}}\n");
            let (responses, _, _) = run(&line);
            let Err(err) = &responses[0].result else {
                panic!("{path} was READ — this is an escalation");
            };
            assert_eq!(
                err.code,
                ErrorCode::HelperRejected,
                "{path} must be refused"
            );
        }
    }

    /// `Path::components()` normalises `.` away, so a normalisation-based check cannot be trusted.
    /// Exact matching sidesteps the whole class.
    #[test]
    fn refuses_traversal_relative_and_normalised_paths() {
        for path in [
            "/proc/../etc/shadow",
            "/etc/./passwd",
            "/etc/os-release/../shadow",
            "proc/sys/kernel/osrelease",
            "//etc//os-release",
        ] {
            let line = format!("{{\"id\":4,\"op\":\"read_text_file\",\"path\":\"{path}\"}}\n");
            let (responses, _, _) = run(&line);
            let Err(err) = &responses[0].result else {
                panic!("{path} was READ — this is an escalation");
            };
            assert_eq!(
                err.code,
                ErrorCode::HelperRejected,
                "{path} must be refused"
            );
        }
    }

    #[test]
    fn validate_readable_matches_exactly() {
        // Every allow-listed file that exists on this system must validate.
        for f in READABLE_FILES {
            let p = Path::new(f);
            if p.exists() {
                assert!(validate_readable(p).is_ok(), "{f} should validate");
            }
        }
        // A directory on the way to an allowed file is not itself allowed.
        assert!(validate_readable(Path::new("/etc")).is_err());
        assert!(validate_readable(Path::new("/proc/self")).is_err());
        assert!(validate_readable(Path::new("/")).is_err());
    }

    /// Guards the property that makes the list auditable: no entry may be a directory prefix of
    /// another, and none may be a bare directory.
    #[test]
    fn allow_list_contains_only_specific_files() {
        for f in READABLE_FILES {
            assert!(f.starts_with('/'), "{f} must be absolute");
            assert!(!f.ends_with('/'), "{f} must not be a directory");
            assert!(!f.contains("/.."), "{f} must not contain traversal");
            for other in READABLE_FILES {
                if f != other {
                    assert!(
                        !other.starts_with(&format!("{f}/")),
                        "{f} is a prefix of {other}; the list must be flat"
                    );
                }
            }
        }
    }

    #[test]
    fn malformed_requests_are_answered_and_audited_then_capped() {
        let mut input = String::new();
        for _ in 0..MAX_MALFORMED {
            input.push_str("not json at all\n");
        }
        let (responses, audit, res) = run(&input);
        assert!(res.is_err(), "must give up rather than spin forever");
        assert_eq!(
            responses.len() as u32,
            MAX_MALFORMED,
            "each bad line gets an answer"
        );
        assert!(responses.iter().all(|r| r.id == 0));
        assert!(audit.0.iter().any(|l| l.contains("too many malformed")));
    }

    #[test]
    fn an_unknown_op_is_rejected_not_guessed() {
        let (responses, _, _) = run("{\"id\":9,\"op\":\"rm_rf\",\"path\":\"/\"}\n");
        let err = responses[0].result.as_ref().unwrap_err();
        assert_eq!(err.code, ErrorCode::HelperRejected);
    }

    #[test]
    fn eof_ends_the_session_cleanly() {
        let (_, audit, res) = run("");
        assert_eq!(res.unwrap(), 0);
        assert!(audit.0.iter().any(|l| l.starts_with("eof ")));
    }

    #[test]
    fn blank_lines_are_ignored_not_counted_as_malformed() {
        let (responses, _, res) = run("\n\n{\"id\":1,\"op\":\"ping\"}\n\n");
        assert_eq!(res.unwrap(), 1);
        assert_eq!(responses.len(), 1);
    }

    #[test]
    fn every_outcome_is_audited_before_it_is_answered() {
        let (_, audit, _) = run(
            "{\"id\":1,\"op\":\"ping\"}\n{\"id\":2,\"op\":\"read_text_file\",\"path\":\"/tmp/nope\"}\n",
        );
        assert!(audit.0.iter().any(|l| l.contains("ok id=1")));
        assert!(audit.0.iter().any(|l| l.contains("denied id=2")));
    }
}
