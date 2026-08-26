//! Wire protocol shared by the app and the helper.
//!
//! Both sides depend on this module, so a change here is a change to both — which is the point.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// Bumped whenever the message shape changes incompatibly. The client refuses to talk to a helper
/// that reports a different version, because a version-skewed privileged process is exactly the
/// thing not to guess about.
pub const PROTOCOL_VERSION: u32 = 1;

/// The **allow-list**. This enum is the security boundary of the entire privileged surface.
///
/// Rules for adding a variant:
///
/// 1. It must be a *specific* operation, never a general capability. `ReadTextFile` is acceptable;
///    `RunCommand` never is.
/// 2. It must validate its own inputs inside the helper, not trust the caller.
/// 3. It must be reviewed on its own, as a small diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Liveness and version check. Carries no privilege.
    Ping,
    /// Read a small UTF-8 system file that the unprivileged app cannot.
    ///
    /// Constrained inside the helper to an explicit allow-list of roots — see
    /// [`super::server`] — because "read any file as root" is not an operation, it is a
    /// vulnerability.
    ReadTextFile { path: PathBuf },
}

impl Op {
    /// Short stable name, used in the audit log and in error messages.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::ReadTextFile { .. } => "read_text_file",
        }
    }
}

/// One request. `id` is echoed back so responses can be correlated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub op: Op,
}

/// What an operation produced on success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ok", rename_all = "snake_case")]
pub enum OpResult {
    /// Reply to [`Op::Ping`].
    Pong {
        protocol: u32,
        version: String,
        /// Effective uid the helper is running as, so the client can confirm it really is elevated.
        uid: u32,
    },
    /// Reply to [`Op::ReadTextFile`].
    Text { content: String },
}

/// One response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(flatten)]
    pub result: std::result::Result<OpResult, AppError>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn requests_round_trip_as_single_lines() {
        let req = Request {
            id: 7,
            op: Op::ReadTextFile {
                path: PathBuf::from("/proc/1/cmdline"),
            },
        };
        let line = serde_json::to_string(&req).unwrap();
        assert!(
            !line.contains('\n'),
            "the framing is line-delimited: {line}"
        );
        assert!(line.contains("\"read_text_file\""), "{line}");
        let back: Request = serde_json::from_str(&line).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn responses_carry_either_a_result_or_a_typed_error() {
        let ok = Response {
            id: 1,
            result: Ok(OpResult::Pong {
                protocol: PROTOCOL_VERSION,
                version: "0.1.0".into(),
                uid: 0,
            }),
        };
        let back: Response = serde_json::from_str(&serde_json::to_string(&ok).unwrap()).unwrap();
        assert_eq!(ok, back);

        let err = Response {
            id: 2,
            result: Err(AppError::new(ErrorCode::HelperRejected, "Not allowed.")),
        };
        let line = serde_json::to_string(&err).unwrap();
        assert!(!line.contains('\n'));
        let back: Response = serde_json::from_str(&line).unwrap();
        assert_eq!(err, back);
    }

    #[test]
    fn unknown_operations_are_rejected_rather_than_guessed() {
        let bogus = r#"{"id":1,"op":"delete_everything","path":"/"}"#;
        assert!(
            serde_json::from_str::<Request>(bogus).is_err(),
            "an op outside the enum must not deserialise"
        );
    }

    #[test]
    fn operation_names_are_stable_and_distinct() {
        assert_eq!(Op::Ping.name(), "ping");
        assert_eq!(
            Op::ReadTextFile {
                path: PathBuf::from("/x")
            }
            .name(),
            "read_text_file"
        );
    }
}
