//! The privileged helper: protocol, client, and the operation allow-list. Task 0.9 (`FND-4`).
//!
//! # Design
//!
//! Stacer escalated by re-running individual commands under `pkexec`, which meant one
//! authentication prompt *per action* — five service toggles, five dialogs — and, because it read
//! only stdout and never checked exit status, a cancelled prompt was indistinguishable from
//! success. That is the single most damaging behaviour we are replacing.
//!
//! nix instead spawns **one** helper process under `pkexec` and keeps a privileged session for as
//! long as it is needed, exchanging typed messages over the child's stdin and stdout:
//!
//! ```text
//!   nix-app ──spawn── pkexec ──exec── nix-helper --serve      (one authentication)
//!       │                                    │
//!       └──── Request (JSON, one per line) ──┤
//!       ◄──── Response (JSON, one per line) ─┘
//! ```
//!
//! # Security boundary
//!
//! The boundary is **[`Op`], the operation enum** — not the transport. The helper accepts no
//! free-form command, no argument vector, and no path it has not validated. Every addition to `Op`
//! is reviewed as its own small diff, which is the property that makes this auditable at all.
//!
//! Also true by construction:
//!
//! - The helper reads line-delimited JSON and rejects anything else, counting malformed input and
//!   exiting once a threshold is passed, so a confused or hostile peer cannot spin it forever.
//! - It exits on EOF, so it cannot outlive the app that spawned it.
//! - Every request and outcome is written to an audit log before the response is sent.
//!
//! # Testing
//!
//! `pkexec` cannot run in CI, so [`Transport`] abstracts how the child is started: tests spawn the
//! helper binary **directly as the current user**, which exercises serialisation, dispatch,
//! validation, rejection and audit logging without needing root. Only the escalation itself is
//! untested by CI, and that is the one part that is a single `Command` invocation.

mod client;
mod protocol;
mod server;

pub use client::{Client, Transport};
pub use protocol::{Op, OpResult, PROTOCOL_VERSION, Request, Response};
pub use server::{Audit, FileAudit, serve};
