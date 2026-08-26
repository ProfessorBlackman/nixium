//! The error taxonomy. Task 0.2 (`FND-3`).
//!
//! Written before the second command exists, deliberately: retrofitting an error type across
//! dozens of call sites is the most expensive refactor available to this project, and Stacer is
//! the cautionary tale for never doing it at all — it discarded stderr and exit codes everywhere,
//! implemented a file logger it never installed, and so shipped with no error surface whatsoever.
//!
//! Three rules this type exists to enforce:
//!
//! 1. **Every failure carries a machine-readable [`ErrorCode`]** so the frontend can branch on it
//!    without string matching.
//! 2. **Every failure carries a plain-language message and, where one exists, a remedy.** A
//!    message a user cannot act on is a bug.
//! 3. **Every layer wraps rather than swallows.** [`AppError::context`] appends a breadcrumb, so a
//!    failure deep in a scan still says which operation the user asked for.
//!
//! [`Cancelled`](ErrorCode::Cancelled) is a first-class outcome, not a fault: the UI must present
//! it as "you stopped this", never as an error. Stacer could not tell a cancelled authorisation
//! from a successful one, which is the single most damaging behaviour we are replacing.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Stable, machine-readable failure classes.
///
/// These strings cross the IPC boundary and may be branched on by the frontend, so they are API:
/// rename one and you break a caller. Add variants freely; change them deliberately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ErrorCode {
    /// The user stopped the operation. Not a fault.
    Cancelled,
    /// The user declined or failed authorisation, or no polkit agent was available.
    AuthDenied,
    /// The operation needs the privileged helper and it is unavailable.
    HelperUnavailable,
    /// The privileged helper rejected the request as outside its allow-list.
    HelperRejected,
    /// A path does not exist.
    NotFound,
    /// The process lacks permission and elevation would not help (or was not attempted).
    PermissionDenied,
    /// A filesystem or I/O failure.
    Io,
    /// A file or command output did not match the shape we require.
    Parse,
    /// A required external tool or kernel interface is absent on this system.
    Unsupported,
    /// An external command ran and failed.
    CommandFailed,
    /// Stored data was written by an incompatible version.
    VersionMismatch,
    /// Input failed validation before anything was attempted.
    InvalidInput,
    /// A guard refused the operation — a protected path, or a stale precondition.
    Refused,
    /// A defect in nix. If a user sees this, we have a bug to fix.
    Internal,
}

impl ErrorCode {
    /// The stable wire string. Kept explicit rather than derived so a variant rename cannot
    /// silently change the protocol.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::AuthDenied => "auth_denied",
            Self::HelperUnavailable => "helper_unavailable",
            Self::HelperRejected => "helper_rejected",
            Self::NotFound => "not_found",
            Self::PermissionDenied => "permission_denied",
            Self::Io => "io",
            Self::Parse => "parse",
            Self::Unsupported => "unsupported",
            Self::CommandFailed => "command_failed",
            Self::VersionMismatch => "version_mismatch",
            Self::InvalidInput => "invalid_input",
            Self::Refused => "refused",
            Self::Internal => "internal",
        }
    }

    /// Whether this outcome should be presented as a fault at all.
    #[must_use]
    pub const fn is_fault(self) -> bool {
        !matches!(self, Self::Cancelled)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What actually went wrong underneath, preserved so the UI can show it on demand and so a bug
/// report contains something actionable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum Cause {
    /// An OS error, with its raw errno where one was available.
    Os {
        errno: Option<i32>,
        description: String,
    },
    /// An external command that ran and failed. `stderr` is captured, never discarded —
    /// Stacer read only stdout and never checked exit status, so a clean failure looked
    /// indistinguishable from success.
    Command {
        program: String,
        status: Option<i32>,
        stderr: String,
    },
    /// Input that did not parse, with enough locator to find it again.
    Malformed { source: String, detail: String },
    /// A free-form cause that has no better home.
    Other { detail: String },
}

impl fmt::Display for Cause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Os { errno, description } => match errno {
                Some(e) => write!(f, "{description} (errno {e})"),
                None => write!(f, "{description}"),
            },
            Self::Command {
                program,
                status,
                stderr,
            } => {
                write!(f, "`{program}` failed")?;
                if let Some(s) = status {
                    write!(f, " with status {s}")?;
                }
                if !stderr.trim().is_empty() {
                    write!(f, ": {}", stderr.trim())?;
                }
                Ok(())
            }
            Self::Malformed { source, detail } => write!(f, "{source}: {detail}"),
            Self::Other { detail } => f.write_str(detail),
        }
    }
}

/// The single error type crossing every boundary in nix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AppError {
    /// Machine-readable class.
    pub code: ErrorCode,
    /// One sentence, plain language, addressed to the user.
    pub message: String,
    /// What the user can do about it, when there is something.
    pub remedy: Option<String>,
    /// The underlying failure, for the details view and for bug reports.
    ///
    /// Boxed because it is the largest field and the least often read: keeping it inline pushed
    /// `AppError` past clippy's `result_large_err` threshold, which would have made every
    /// `Result<T, AppError>` in the codebase expensive to move on the *success* path. Errors are
    /// rare, so an allocation here costs nothing that matters.
    pub cause: Option<Box<Cause>>,
    /// Breadcrumb from outermost operation inwards, appended by each layer that wraps.
    pub context: Vec<String>,
    /// The path involved, when the failure is about one.
    pub path: Option<PathBuf>,
}

impl AppError {
    /// A new error. Prefer the named constructors below where one fits.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            remedy: None,
            cause: None,
            context: Vec::new(),
            path: None,
        }
    }

    /// The user stopped this. Not a fault; the UI must not present it as one.
    #[must_use]
    pub fn cancelled() -> Self {
        Self::new(ErrorCode::Cancelled, "Stopped.")
    }

    /// A defect in nix rather than in the system or the user's input.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
            .with_remedy("This is a bug in nix. The details below are worth including in a report.")
    }

    /// A required tool or kernel interface is missing. Carries the capability that was absent so
    /// the message can name it.
    #[must_use]
    pub fn unsupported(what: impl fmt::Display) -> Self {
        Self::new(
            ErrorCode::Unsupported,
            format!("{what} is not available on this system."),
        )
    }

    /// Input rejected before anything was attempted.
    #[must_use]
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message)
    }

    /// A guard refused: a protected path, or a precondition that no longer holds.
    #[must_use]
    pub fn refused(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Refused, message)
    }

    /// Attach what the user can do about it.
    #[must_use]
    pub fn with_remedy(mut self, remedy: impl Into<String>) -> Self {
        self.remedy = Some(remedy.into());
        self
    }

    /// Attach the underlying failure.
    #[must_use]
    pub fn with_cause(mut self, cause: Cause) -> Self {
        self.cause = Some(Box::new(cause));
        self
    }

    /// The underlying cause, if one was recorded.
    #[must_use]
    pub fn cause(&self) -> Option<&Cause> {
        self.cause.as_deref()
    }

    /// Attach the path this failure concerns.
    #[must_use]
    pub fn with_path(mut self, path: impl AsRef<Path>) -> Self {
        self.path = Some(path.as_ref().to_path_buf());
        self
    }

    /// Append a breadcrumb. Call this at each boundary the error passes outward through, so the
    /// final message can say which user-facing operation was in progress.
    #[must_use]
    pub fn context(mut self, what: impl Into<String>) -> Self {
        self.context.push(what.into());
        self
    }

    /// Whether this should be surfaced as a fault.
    #[must_use]
    pub const fn is_fault(&self) -> bool {
        self.code.is_fault()
    }

    /// Build from an [`std::io::Error`], mapping the kind onto a code so callers do not have to.
    #[must_use]
    pub fn from_io(err: &std::io::Error, doing: impl fmt::Display) -> Self {
        let code = match err.kind() {
            std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            _ => ErrorCode::Io,
        };
        let message = match code {
            ErrorCode::NotFound => format!("Could not find what was needed to {doing}."),
            ErrorCode::PermissionDenied => format!("Not permitted to {doing}."),
            _ => format!("Could not {doing}."),
        };
        Self::new(code, message).with_cause(Cause::Os {
            errno: err.raw_os_error(),
            description: err.to_string(),
        })
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)?;
        if let Some(p) = &self.path {
            write!(f, " ({})", p.display())?;
        }
        for c in self.context.iter().rev() {
            write!(f, " while {c}")?;
        }
        if let Some(c) = &self.cause {
            write!(f, " — {c}")?;
        }
        Ok(())
    }
}

impl std::error::Error for AppError {}

/// Every fallible operation in nix returns this.
pub type Result<T> = std::result::Result<T, AppError>;

/// Adds [`AppError::context`] to a `Result` without unwrapping it first.
pub trait Contextual<T> {
    /// Append a breadcrumb to the error, if there is one.
    fn context(self, what: impl Into<String>) -> Result<T>;
}

impl<T> Contextual<T> for Result<T> {
    fn context(self, what: impl Into<String>) -> Result<T> {
        self.map_err(|e| e.context(what))
    }
}

/// Turns an `io::Result` into ours, describing what was being attempted.
pub trait IoContext<T> {
    /// `doing` completes the sentence "Could not …".
    fn doing(self, doing: impl fmt::Display) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn doing(self, doing: impl fmt::Display) -> Result<T> {
        self.map_err(|e| AppError::from_io(&e, doing))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Guards the reason `cause` is boxed. If this fails, `Result<T, AppError>` has become
    /// expensive to move everywhere in the codebase.
    #[test]
    fn app_error_stays_small_enough_to_return_by_value() {
        let size = std::mem::size_of::<AppError>();
        assert!(
            size <= 128,
            "AppError grew to {size} bytes; clippy::result_large_err warns above 128. \
             Box the new field rather than raising the ceiling."
        );
    }

    #[test]
    fn cancelled_is_not_a_fault() {
        assert!(!AppError::cancelled().is_fault());
        assert!(AppError::internal("x").is_fault());
    }

    #[test]
    fn wire_codes_are_stable_and_unique() {
        let all = [
            ErrorCode::Cancelled,
            ErrorCode::AuthDenied,
            ErrorCode::HelperUnavailable,
            ErrorCode::HelperRejected,
            ErrorCode::NotFound,
            ErrorCode::PermissionDenied,
            ErrorCode::Io,
            ErrorCode::Parse,
            ErrorCode::Unsupported,
            ErrorCode::CommandFailed,
            ErrorCode::VersionMismatch,
            ErrorCode::InvalidInput,
            ErrorCode::Refused,
            ErrorCode::Internal,
        ];
        let mut seen = std::collections::HashSet::new();
        for c in all {
            assert!(
                seen.insert(c.as_str()),
                "duplicate wire code {}",
                c.as_str()
            );
            // Codes are snake_case identifiers, safe to use as object keys or CSS classes.
            assert!(
                c.as_str()
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch == '_'),
                "unexpected characters in {}",
                c.as_str()
            );
        }
    }

    #[test]
    fn io_kinds_map_onto_codes() {
        let nf = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(
            AppError::from_io(&nf, "read the mount table").code,
            ErrorCode::NotFound
        );

        let pd = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            AppError::from_io(&pd, "read the mount table").code,
            ErrorCode::PermissionDenied
        );

        let other = std::io::Error::other("disk on fire");
        assert_eq!(
            AppError::from_io(&other, "read the mount table").code,
            ErrorCode::Io
        );
    }

    #[test]
    fn context_accumulates_outermost_last() {
        let e = AppError::new(ErrorCode::Io, "Could not read a file.")
            .context("scanning /home")
            .context("estimating reclaimable space");
        assert_eq!(
            e.context,
            vec!["scanning /home", "estimating reclaimable space"]
        );

        // Display reads outermost first, which is the order a user thinks in.
        let shown = e.to_string();
        let outer = shown.find("estimating").unwrap();
        let inner = shown.find("scanning").unwrap();
        assert!(
            outer < inner,
            "expected outermost context first, got: {shown}"
        );
    }

    #[test]
    fn contextual_trait_threads_through_results() {
        let r: Result<()> = Err(AppError::internal("boom"));
        let r = r.context("doing the thing");
        assert_eq!(r.unwrap_err().context, vec!["doing the thing"]);
    }

    #[test]
    fn command_cause_keeps_stderr() {
        let e = AppError::new(ErrorCode::CommandFailed, "Could not list packages.").with_cause(
            Cause::Command {
                program: "dpkg-query".into(),
                status: Some(2),
                stderr: "no packages found matching".into(),
            },
        );
        let shown = e.to_string();
        assert!(shown.contains("dpkg-query"), "{shown}");
        assert!(shown.contains("status 2"), "{shown}");
        assert!(shown.contains("no packages found"), "{shown}");
    }

    #[test]
    fn errors_round_trip_over_the_wire() {
        let e = AppError::new(ErrorCode::NotFound, "Could not find the mount table.")
            .with_path("/proc/self/mountinfo")
            .with_remedy("Check that /proc is mounted.")
            .with_cause(Cause::Os {
                errno: Some(2),
                description: "No such file or directory".into(),
            })
            .context("listing filesystems");

        let json = serde_json::to_string(&e).unwrap();
        let back: AppError = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
        assert!(
            json.contains("\"not_found\""),
            "code must serialise as its wire string: {json}"
        );
    }
}
