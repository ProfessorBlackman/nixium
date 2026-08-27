// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Cancellation and progress for long operations. Task 0.3 (`FND-2`).
//!
//! The plan is explicit that **the primitive is the deliverable**, not the individual commands: get
//! one long-operation pattern right and every scan, search and package query inherits it. Getting
//! it wrong means writing cancellation five times and getting it slightly different each time.
//!
//! Deliberately dependency-free and Tauri-free. A [`CancelToken`] is an `Arc<AtomicBool>` that
//! blocking walkers can poll cheaply in a tight loop, which is what the filesystem scanner
//! (task 1.3) needs; async consumers can poll it just as well.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, Result};

/// Identifies a running operation across the IPC boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
// ts-rs does not understand `serde(transparent)`, so the wire type is stated explicitly.
#[ts(export, type = "number")]
pub struct OperationId(pub u64);

impl OperationId {
    /// Next id in this process. Monotonic, so ids are never reused within a session.
    #[must_use]
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl std::fmt::Display for OperationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "op{}", self.0)
    }
}

/// A cooperative cancellation flag, cheap to clone and to poll.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Idempotent.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    /// `Err(AppError::cancelled())` if cancelled, so a worker loop can use `?`.
    ///
    /// ```
    /// # use nix_core::op::CancelToken;
    /// # fn walk(token: &CancelToken) -> nix_core::error::Result<()> {
    /// for _entry in 0..10 {
    ///     token.check()?;          // bail out cooperatively
    /// }
    /// Ok(())
    /// # }
    /// # let t = CancelToken::new();
    /// # assert!(walk(&t).is_ok());
    /// # t.cancel();
    /// # assert!(walk(&t).is_err());
    /// ```
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(AppError::cancelled())
        } else {
            Ok(())
        }
    }
}

/// A progress report. `total` is optional because a streaming scan does not know its size up front,
/// and claiming one would be dishonest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Progress {
    pub id: OperationId,
    /// Units completed. Units are operation-specific; the label says what they are.
    ///
    /// Crosses the wire as a JavaScript number rather than a `bigint`: a double holds integers
    /// exactly to 2^53, which is nine quadrillion files. Nothing we count will reach it, and
    /// `bigint` would make every arithmetic site in the frontend awkward for no benefit.
    #[ts(type = "number")]
    pub done: u64,
    /// Total units, when genuinely known.
    #[ts(type = "number | null")]
    pub total: Option<u64>,
    /// What is happening right now, phrased for a user.
    pub message: Option<String>,
}

impl Progress {
    #[must_use]
    pub const fn new(id: OperationId, done: u64) -> Self {
        Self {
            id,
            done,
            total: None,
            message: None,
        }
    }

    #[must_use]
    pub const fn with_total(mut self, total: u64) -> Self {
        self.total = Some(total);
        self
    }

    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Completed fraction in `0.0..=1.0`, when a total is known.
    #[must_use]
    pub fn fraction(&self) -> Option<f64> {
        match self.total {
            Some(0) | None => None,
            #[allow(clippy::cast_precision_loss)]
            Some(total) => Some((self.done as f64 / total as f64).clamp(0.0, 1.0)),
        }
    }
}

/// How an operation ended. Sent as the terminal event so the frontend never has to infer it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[ts(export)]
pub enum Completion {
    /// Ran to completion.
    Done { id: OperationId },
    /// Stopped on request. Not a fault.
    Cancelled { id: OperationId },
    /// Failed.
    Failed { id: OperationId, error: AppError },
}

impl Completion {
    /// Classify a worker's result, mapping the cancellation sentinel onto its own outcome so the
    /// UI never presents "you stopped this" as an error.
    #[must_use]
    pub fn from_result(id: OperationId, result: Result<()>) -> Self {
        match result {
            Ok(()) => Self::Done { id },
            Err(e) if !e.is_fault() => Self::Cancelled { id },
            Err(e) => Self::Failed { id, error: e },
        }
    }

    #[must_use]
    pub const fn id(&self) -> OperationId {
        match self {
            Self::Done { id } | Self::Cancelled { id } | Self::Failed { id, .. } => *id,
        }
    }
}

/// Tracks in-flight operations so they can be cancelled by id from another thread.
///
/// Tokens are removed when the operation finishes, so a completed id cannot be cancelled and the
/// map cannot grow without bound.
#[derive(Debug, Default)]
pub struct Registry {
    live: std::sync::Mutex<std::collections::HashMap<OperationId, CancelToken>>,
}

impl Registry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new operation and hand back its id and token.
    pub fn start(&self) -> (OperationId, CancelToken) {
        let id = OperationId::next();
        let token = CancelToken::new();
        if let Ok(mut live) = self.live.lock() {
            live.insert(id, token.clone());
        }
        (id, token)
    }

    /// Request cancellation. Returns whether an operation with that id was still running.
    pub fn cancel(&self, id: OperationId) -> bool {
        let Ok(live) = self.live.lock() else {
            return false;
        };
        match live.get(&id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Cancel everything in flight, e.g. on window close.
    pub fn cancel_all(&self) {
        if let Ok(live) = self.live.lock() {
            for token in live.values() {
                token.cancel();
            }
        }
    }

    /// Drop an operation's token. Must be called when the worker exits, however it exits.
    pub fn finish(&self, id: OperationId) {
        if let Ok(mut live) = self.live.lock() {
            live.remove(&id);
        }
    }

    /// Number of operations in flight.
    pub fn live_count(&self) -> usize {
        self.live.lock().map(|l| l.len()).unwrap_or(0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_monotonic() {
        let a = OperationId::next();
        let b = OperationId::next();
        assert!(b.0 > a.0);
    }

    #[test]
    fn token_check_is_the_cancellation_sentinel() {
        let t = CancelToken::new();
        assert!(t.check().is_ok());
        t.cancel();
        assert!(t.is_cancelled());
        let err = t.check().unwrap_err();
        assert!(
            !err.is_fault(),
            "cancellation must not be presented as a fault"
        );
    }

    #[test]
    fn cancel_is_visible_across_clones() {
        let a = CancelToken::new();
        let b = a.clone();
        a.cancel();
        assert!(b.is_cancelled(), "clones must share one flag");
    }

    #[test]
    fn cancel_crosses_threads() {
        let token = CancelToken::new();
        let worker = {
            let token = token.clone();
            std::thread::spawn(move || {
                let mut spins = 0u64;
                while !token.is_cancelled() {
                    spins += 1;
                    if spins > 100_000_000 {
                        return Err("token never observed cancellation");
                    }
                    std::hint::spin_loop();
                }
                Ok(spins)
            })
        };
        token.cancel();
        assert!(worker.join().expect("worker panicked").is_ok());
    }

    #[test]
    fn registry_tracks_and_releases() {
        let reg = Registry::new();
        let (id, token) = reg.start();
        assert_eq!(reg.live_count(), 1);
        assert!(reg.cancel(id), "a live operation is cancellable");
        assert!(token.is_cancelled());

        reg.finish(id);
        assert_eq!(reg.live_count(), 0);
        assert!(!reg.cancel(id), "a finished operation is not cancellable");
    }

    #[test]
    fn registry_cancels_everything() {
        let reg = Registry::new();
        let tokens: Vec<_> = (0..5).map(|_| reg.start().1).collect();
        reg.cancel_all();
        assert!(tokens.iter().all(CancelToken::is_cancelled));
    }

    #[test]
    fn completion_maps_cancellation_separately_from_failure() {
        let id = OperationId::next();
        assert!(matches!(
            Completion::from_result(id, Ok(())),
            Completion::Done { .. }
        ));
        assert!(matches!(
            Completion::from_result(id, Err(AppError::cancelled())),
            Completion::Cancelled { .. }
        ));
        assert!(matches!(
            Completion::from_result(id, Err(AppError::internal("boom"))),
            Completion::Failed { .. }
        ));
    }

    #[test]
    fn progress_fraction_is_honest_about_unknown_totals() {
        let id = OperationId::next();
        assert_eq!(
            Progress::new(id, 5).fraction(),
            None,
            "no total means no fraction"
        );
        assert_eq!(
            Progress::new(id, 0).with_total(0).fraction(),
            None,
            "must not divide by zero"
        );
        let half = Progress::new(id, 5).with_total(10).fraction().unwrap();
        assert!((half - 0.5).abs() < f64::EPSILON);
        // Overshoot is clamped rather than reported as more than complete.
        let over = Progress::new(id, 20).with_total(10).fraction().unwrap();
        assert!((over - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn events_round_trip_over_the_wire() {
        let id = OperationId::next();
        let p = Progress::new(id, 3)
            .with_total(9)
            .with_message("Scanning /home");
        let back: Progress = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(p, back);

        let c = Completion::from_result(id, Err(AppError::internal("boom")));
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"failed\""), "{json}");
        let back: Completion = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }
}
