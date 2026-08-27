//! The helper's request loop, validation and audit log. Runs as root.
//!
//! Everything here executes with full privilege, so the code is written to be readable rather than
//! clever, and every path a request can take ends in an audited outcome.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{AppError, Cause, ErrorCode, Result};

use super::protocol::{
    Manager, Op, OpResult, PROTOCOL_VERSION, ReclaimKind, Request, Response, VacuumLimit,
};

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

/// Roots each reclaimable category owns.
///
/// The category a caller names is **not taken on trust**: the helper looks the roots up here and
/// refuses anything outside them. This is specification invariant 4 enforced where it matters — a
/// compromised or buggy caller cannot claim `/etc/shadow` is a rotated log, because `/etc` is not a
/// root of any category.
const fn roots_for(kind: ReclaimKind) -> &'static [&'static str] {
    match kind {
        ReclaimKind::PackageCache => &[
            "/var/cache/apt/archives",
            "/var/cache/dnf",
            "/var/cache/pacman/pkg",
            "/var/cache/zypp/packages",
        ],
        ReclaimKind::RotatedLog => &["/var/log"],
        ReclaimKind::CrashDump => &["/var/crash"],
    }
}

/// Suffixes that mark a log as rotated, and therefore safe to delete.
///
/// The consequence is worth stating plainly: **an active log cannot be deleted through this
/// operation at all**, however the caller asks. `/var/log/syslog` is refused; `/var/log/syslog.1.gz`
/// is not. Deleting a file a running service holds open frees nothing until the service restarts and
/// can break its logging in the meantime.
const ROTATED_SUFFIXES: &[&str] = &[".gz", ".xz", ".bz2", ".zst", ".old", ".1"];

/// Whether a log filename is a rotated one.
fn is_rotated_log(name: &str) -> bool {
    if ROTATED_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        return true;
    }
    // Numeric rotation: `syslog.2`, `daemon.log.10`.
    match name.rsplit_once('.') {
        Some((stem, digits)) => {
            !stem.is_empty() && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

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

/// Refuse any path that is not a plain file inside one of `kind`'s declared roots.
///
/// Every check here fails closed. In order:
///
/// 1. Absolute, and free of `.` or `..` — a relative or traversing path could resolve anywhere.
/// 2. Inside a root **this category** owns, compared component-wise so `/var/logger` is not `/var/log`.
/// 3. Not a symbolic link — following one as root is how a caller reaches outside the allow-list,
///    and `symlink_metadata` is used precisely so the link itself is inspected rather than its target.
/// 4. A regular file. Directories are refused outright: a recursive delete is a much larger promise
///    than this operation makes, and nothing needs it.
/// 5. For a rotated log, the filename must actually look rotated.
fn validate_reclaimable(kind: ReclaimKind, path: &Path) -> Result<std::fs::Metadata> {
    let refused = |detail: String| {
        AppError::new(ErrorCode::HelperRejected, detail)
            .with_path(path)
            .with_remedy("The helper only removes specific kinds of file, in specific places.")
    };

    if !path.is_absolute() {
        return Err(refused("The helper only accepts absolute paths.".into()));
    }
    if path.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err(refused("Paths containing . or .. are refused.".into()));
    }

    let inside = roots_for(kind)
        .iter()
        .any(|root| path.starts_with(Path::new(root)));
    if !inside {
        return Err(refused(format!(
            "{} is not somewhere the helper may remove a {}.",
            path.display(),
            kind.name()
        )));
    }
    // A root itself is never removable, only things inside it.
    if roots_for(kind).iter().any(|root| path == Path::new(root)) {
        return Err(refused(
            "The helper does not remove a category's root directory.".into(),
        ));
    }

    // The name check comes *before* any filesystem access, deliberately. A policy refusal should
    // not depend on whether the file happens to exist: on a journald-only system `/var/log/syslog`
    // is absent, and an earlier version of this function returned NotFound there instead of a
    // refusal — which would have made the guard's behaviour vary by distribution. Checking policy
    // first also means no I/O happens on a request that was never going to be allowed.
    if kind == ReclaimKind::RotatedLog {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !is_rotated_log(&name) {
            return Err(refused(format!(
                "{name} is an active log, not a rotated one."
            ))
            .with_remedy(
                "Only rotated logs are removed. An active log would be recreated and its service disrupted.",
            ));
        }
    }

    let meta = std::fs::symlink_metadata(path).map_err(|e| {
        AppError::from_io(&e, format!("inspect {}", path.display())).with_path(path)
    })?;

    if meta.file_type().is_symlink() {
        return Err(refused(
            "The helper refuses to follow symbolic links.".into(),
        ));
    }
    if !meta.is_file() {
        return Err(refused("The helper only removes regular files.".into()));
    }

    Ok(meta)
}

/// On-disk bytes, matching what freeing the file actually returns.
fn allocated(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks() * 512
}

/// Total on-disk bytes under a category's roots.
fn measure(kind: ReclaimKind) -> u64 {
    roots_for(kind)
        .iter()
        .map(|root| crate::fixture::directory_size(Path::new(root)))
        .sum()
}

/// Files under a category's roots that the helper would agree to delete.
///
/// Applying the same predicate here as at delete time means the unprivileged side cannot present a
/// candidate the helper will later refuse — the list a user sees is exactly the list that can act.
fn list_reclaimable(kind: ReclaimKind) -> Vec<(PathBuf, u64)> {
    let mut found = Vec::new();
    for root in roots_for(kind) {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.filter_map(std::result::Result::ok) {
            let path = entry.path();
            if let Ok(meta) = validate_reclaimable(kind, &path) {
                found.push((path, allocated(&meta)));
            }
        }
    }
    found.sort_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
    found
}

/// The fixed argument vector for cleaning each manager's cache.
///
/// Fixed, and never assembled from anything a caller sent. The specification requires reclaiming
/// through the owning tool rather than by unlinking cache files, and this is that.
const fn clean_command(manager: Manager) -> (&'static str, &'static [&'static str]) {
    match manager {
        Manager::Apt => ("apt-get", &["clean"]),
        Manager::Dnf => ("dnf", &["clean", "packages"]),
        Manager::Pacman => ("pacman", &["-Sc", "--noconfirm"]),
        Manager::Zypper => ("zypper", &["clean"]),
    }
}

/// Run a fixed command and turn a non-zero exit into a typed error carrying its stderr.
fn run_fixed(program: &str, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| AppError::from_io(&e, format!("run {program}")))?;

    if output.status.success() {
        return Ok(());
    }
    Err(AppError::new(
        ErrorCode::CommandFailed,
        format!("{program} did not succeed."),
    )
    .with_cause(Cause::Command {
        program: program.to_string(),
        status: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }))
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

        Op::MeasureCategory { kind } => Ok(OpResult::Bytes {
            bytes: measure(*kind),
        }),

        Op::ListCategory { kind } => Ok(OpResult::Files {
            files: list_reclaimable(*kind),
        }),

        Op::ReclaimFile { kind, path } => {
            let meta = validate_reclaimable(*kind, path)?;
            let bytes = allocated(&meta);
            std::fs::remove_file(path).map_err(|e| {
                AppError::from_io(&e, format!("remove {}", path.display())).with_path(path)
            })?;
            Ok(OpResult::Reclaimed { bytes })
        }

        Op::PackageManagerClean { manager } => {
            let before = measure(ReclaimKind::PackageCache);
            let (program, args) = clean_command(*manager);
            run_fixed(program, args)?;
            let after = measure(ReclaimKind::PackageCache);
            Ok(OpResult::Reclaimed {
                bytes: before.saturating_sub(after),
            })
        }

        Op::JournalVacuum { limit } => {
            // The limit is typed, so the flag is constructed here from a number rather than
            // interpolated from anything the caller sent.
            let flag = match limit {
                VacuumLimit::Size { mebibytes } => format!("--vacuum-size={mebibytes}M"),
                VacuumLimit::Age { days } => format!("--vacuum-time={days}d"),
            };
            let before = journal_bytes();
            run_fixed("journalctl", &[&flag])?;
            let after = journal_bytes();
            Ok(OpResult::Reclaimed {
                bytes: before.saturating_sub(after),
            })
        }
    }
}

/// Bytes the journal currently occupies, by measuring its directories.
fn journal_bytes() -> u64 {
    ["/var/log/journal", "/run/log/journal"]
        .iter()
        .map(|d| crate::fixture::directory_size(Path::new(d)))
        .sum()
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
            // A destructive outcome is recorded distinctly: an audit trail whose deletions look
            // like its reads is not much of an audit trail.
            Ok(_) if request.op.is_destructive() => audit.record(&format!(
                "DESTRUCTIVE id={} op={} detail={:?}",
                request.id,
                request.op.name(),
                request.op
            )),
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

    // ---------------------------------------------------------------------------------------
    // Destructive operations. These are the most important tests in the codebase: each one is a
    // path by which a compromised or buggy caller could destroy a system, closed off and asserted.
    // ---------------------------------------------------------------------------------------

    /// The central property: the category a caller names does not grant access to paths outside
    /// that category's roots. Every entry below is something a caller might *ask* to delete by
    /// mislabelling it.
    #[test]
    fn a_category_cannot_be_used_to_reach_outside_its_own_roots() {
        let attempts = [
            // Claiming system files are rotated logs.
            (ReclaimKind::RotatedLog, "/etc/shadow"),
            (ReclaimKind::RotatedLog, "/etc/passwd.1"),
            (ReclaimKind::RotatedLog, "/boot/vmlinuz.old"),
            (ReclaimKind::RotatedLog, "/home/someone/.ssh/id_rsa.1"),
            // Claiming they are package cache.
            (ReclaimKind::PackageCache, "/usr/bin/rm"),
            (ReclaimKind::PackageCache, "/var/lib/dpkg/status"),
            (ReclaimKind::PackageCache, "/var/cache/private/secret"),
            // Claiming they are crash dumps.
            (ReclaimKind::CrashDump, "/var/log/syslog"),
            (ReclaimKind::CrashDump, "/etc/fstab"),
            // Near-misses that a string prefix check would wrongly admit.
            (ReclaimKind::RotatedLog, "/var/logger/thing.gz"),
            (
                ReclaimKind::PackageCache,
                "/var/cache/apt/archives-elsewhere/x.deb",
            ),
        ];

        for (kind, path) in attempts {
            let err = validate_reclaimable(kind, Path::new(path)).expect_err(&format!(
                "{path} was accepted as a {} — this destroys systems",
                kind.name()
            ));
            assert_eq!(err.code, ErrorCode::HelperRejected, "{path}");
        }
    }

    /// An active log must be unreachable through this operation, however it is asked for. Deleting a
    /// file a running service holds open frees nothing until it restarts, and breaks its logging.
    #[test]
    fn an_active_log_cannot_be_deleted_even_when_requested() {
        for name in ["syslog", "auth.log", "kern.log", "daemon.log", "dmesg"] {
            let path = format!("/var/log/{name}");
            let err = validate_reclaimable(ReclaimKind::RotatedLog, Path::new(&path))
                .expect_err(&format!("{path} is active and must be refused"));
            assert_eq!(err.code, ErrorCode::HelperRejected);
            assert!(
                err.message.contains("active log"),
                "the refusal should explain itself: {}",
                err.message
            );
        }
    }

    /// Guards the ordering. A policy refusal must not depend on whether the file exists, or the
    /// guard's behaviour varies by distribution — on a journald-only system there is no
    /// `/var/log/syslog` to stat.
    #[test]
    fn a_policy_refusal_does_not_depend_on_the_file_existing() {
        let missing_active = Path::new("/var/log/definitely-not-here-syslog");
        let err = validate_reclaimable(ReclaimKind::RotatedLog, missing_active).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::HelperRejected,
            "an active-looking name must be refused on policy, not reported as missing"
        );

        let missing_rotated = Path::new("/var/log/definitely-not-here.1.gz");
        let err = validate_reclaimable(ReclaimKind::RotatedLog, missing_rotated).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::NotFound,
            "a permissible name that is absent is genuinely NotFound"
        );
    }

    #[test]
    fn rotation_suffixes_are_recognised_and_active_names_are_not() {
        for rotated in [
            "syslog.1",
            "syslog.1.gz",
            "syslog.2.gz",
            "auth.log.4.xz",
            "messages.old",
            "daemon.log.10",
            "kern.log.zst",
            "x.bz2",
        ] {
            assert!(is_rotated_log(rotated), "{rotated} should count as rotated");
        }
        for active in ["syslog", "auth.log", "kern.log", "dmesg", "lastlog", "wtmp"] {
            assert!(!is_rotated_log(active), "{active} is active, not rotated");
        }
    }

    #[test]
    fn traversal_and_relative_paths_are_refused_for_every_category() {
        for kind in [
            ReclaimKind::PackageCache,
            ReclaimKind::RotatedLog,
            ReclaimKind::CrashDump,
        ] {
            for path in [
                "/var/log/../../etc/shadow",
                "/var/cache/apt/archives/../../../etc/passwd",
                "/var/log/./syslog.1.gz",
                "var/log/syslog.1.gz",
            ] {
                assert!(
                    validate_reclaimable(kind, Path::new(path)).is_err(),
                    "{path} must be refused for {}",
                    kind.name()
                );
            }
        }
    }

    #[test]
    fn a_categorys_root_directory_is_not_itself_removable() {
        for (kind, root) in [
            (ReclaimKind::RotatedLog, "/var/log"),
            (ReclaimKind::PackageCache, "/var/cache/apt/archives"),
            (ReclaimKind::CrashDump, "/var/crash"),
        ] {
            let err = validate_reclaimable(kind, Path::new(root)).unwrap_err();
            assert_eq!(err.code, ErrorCode::HelperRejected, "{root}");
        }
    }

    /// A symlink is how a caller reaches outside the allow-list, so it is refused rather than
    /// resolved — resolve-then-check would be a time-of-check/time-of-use race.
    #[test]
    fn symlinks_are_refused_rather_than_followed() {
        let dir = std::env::temp_dir().join(format!("nix-helper-symlink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let secret = dir.join("secret");
        std::fs::write(&secret, b"sensitive").unwrap();

        // Simulate a root that we control, so the link is genuinely inside an allowed prefix shape.
        let link = dir.join("thing.1.gz");
        std::os::unix::fs::symlink(&secret, &link).unwrap();

        // Even ignoring the root check, the symlink itself must be rejected. Assert via the
        // predicate that does that work.
        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink());

        // And the real validator refuses it (here because it is also outside any root, which is
        // itself correct — both gates hold).
        assert!(validate_reclaimable(ReclaimKind::RotatedLog, &link).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn directories_are_refused_because_this_operation_deletes_one_file() {
        // /var/log almost certainly contains a directory on any real system.
        let candidates = std::fs::read_dir("/var/log")
            .into_iter()
            .flatten()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.metadata().is_ok_and(|m| m.is_dir()))
            .map(|e| e.path())
            .take(2)
            .collect::<Vec<_>>();

        for dir in candidates {
            let err = validate_reclaimable(ReclaimKind::RotatedLog, &dir).unwrap_err();
            assert_eq!(
                err.code,
                ErrorCode::HelperRejected,
                "{} is a directory and must be refused",
                dir.display()
            );
        }
    }

    #[test]
    fn every_category_root_is_absolute_and_specific() {
        for kind in [
            ReclaimKind::PackageCache,
            ReclaimKind::RotatedLog,
            ReclaimKind::CrashDump,
        ] {
            let roots = roots_for(kind);
            assert!(!roots.is_empty(), "{} has no roots", kind.name());
            for root in roots {
                assert!(root.starts_with('/'), "{root} must be absolute");
                assert!(
                    !root.ends_with('/'),
                    "{root} must not have a trailing slash"
                );
                assert!(!root.contains(".."), "{root} must not traverse");
                // A root of "/" or "/var" would be far too broad to be safe.
                assert!(
                    root.matches('/').count() >= 2,
                    "{root} is too broad a root to be safe"
                );
            }
        }
    }

    #[test]
    fn clean_commands_are_fixed_and_take_no_caller_input() {
        for manager in [Manager::Apt, Manager::Dnf, Manager::Pacman, Manager::Zypper] {
            let (program, args) = clean_command(manager);
            assert!(!program.is_empty());
            assert!(
                !program.contains('/'),
                "{program} should be resolved on PATH, not by path"
            );
            for arg in args {
                // If an argument ever contains a separator or a shell metacharacter, something
                // caller-supplied has leaked into a privileged command line.
                assert!(
                    !arg.contains('/')
                        && !arg.contains(';')
                        && !arg.contains('&')
                        && !arg.contains('$'),
                    "{manager:?} argument {arg:?} looks like interpolated input"
                );
            }
        }
    }

    #[test]
    fn a_vacuum_flag_is_built_from_numbers_not_from_text() {
        // The typed limit is what makes this safe: there is no string a caller can send that
        // becomes part of journalctl's argument vector.
        let size = VacuumLimit::Size { mebibytes: 500 };
        let age = VacuumLimit::Age { days: 14 };
        let format = |limit: &VacuumLimit| match limit {
            VacuumLimit::Size { mebibytes } => format!("--vacuum-size={mebibytes}M"),
            VacuumLimit::Age { days } => format!("--vacuum-time={days}d"),
        };
        assert_eq!(format(&size), "--vacuum-size=500M");
        assert_eq!(format(&age), "--vacuum-time=14d");

        // A hostile value can only ever be a number, so it cannot escape the flag.
        let hostile = VacuumLimit::Size {
            mebibytes: u64::MAX,
        };
        let flag = format(&hostile);
        assert!(flag.starts_with("--vacuum-size="));
        assert!(
            !flag.contains(' '),
            "a flag must remain a single argument: {flag}"
        );
    }

    #[test]
    fn destructive_operations_are_audited_distinctly() {
        let destructive = Op::ReclaimFile {
            kind: ReclaimKind::RotatedLog,
            path: PathBuf::from("/var/log/syslog.1.gz"),
        };
        assert!(destructive.is_destructive());
        assert!(
            Op::PackageManagerClean {
                manager: Manager::Apt
            }
            .is_destructive()
        );
        assert!(
            Op::JournalVacuum {
                limit: VacuumLimit::Size { mebibytes: 100 }
            }
            .is_destructive()
        );

        // Reads are not.
        assert!(!Op::Ping.is_destructive());
        assert!(
            !Op::MeasureCategory {
                kind: ReclaimKind::RotatedLog
            }
            .is_destructive()
        );
        assert!(
            !Op::ListCategory {
                kind: ReclaimKind::CrashDump
            }
            .is_destructive()
        );
    }

    #[test]
    fn a_refused_delete_is_answered_and_audited_over_the_wire() {
        let (responses, audit, _) = run(
            "{\"id\":1,\"op\":\"reclaim_file\",\"kind\":\"rotated_log\",\"path\":\"/etc/shadow\"}\n",
        );
        let Err(err) = &responses[0].result else {
            panic!("/etc/shadow was deleted — this destroys systems");
        };
        assert_eq!(err.code, ErrorCode::HelperRejected);
        assert!(
            audit.0.iter().any(|l| l.contains("denied id=1")),
            "{:?}",
            audit.0
        );
    }

    #[test]
    fn listing_a_category_only_offers_what_a_delete_would_accept() {
        // Whatever this machine has, every entry offered must pass the same validation that the
        // delete applies — that is what makes the list a user sees actionable.
        for kind in [
            ReclaimKind::PackageCache,
            ReclaimKind::RotatedLog,
            ReclaimKind::CrashDump,
        ] {
            for (path, _bytes) in list_reclaimable(kind) {
                assert!(
                    validate_reclaimable(kind, &path).is_ok(),
                    "{} was listed for {} but would be refused",
                    path.display(),
                    kind.name()
                );
            }
        }
    }

    #[test]
    fn measuring_a_category_never_fails_even_when_its_roots_are_absent() {
        // A machine with no /var/crash must report zero rather than erroring.
        for kind in [
            ReclaimKind::PackageCache,
            ReclaimKind::RotatedLog,
            ReclaimKind::CrashDump,
        ] {
            let _ = measure(kind);
        }
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
