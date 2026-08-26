//! Core library for nix: the space model, filesystem scanners, and metric samplers.
//!
//! This crate deliberately has **no GUI and no Tauri dependency**, so that everything it does can
//! be exercised from tests and from the command line without an app running. The same separation
//! was the one clearly good architectural decision in Stacer, whose `stacer-core` static library
//! held all system access behind a GUI-free boundary — and it is why that codebase could be
//! specified at all.
//!
//! Nothing here may depend on `nix-app`. The dependency direction is one-way:
//! `nix-app` → `nix-core`, and `nix-helper` → `nix-core`.
//!
//! # What is here (Phase 0)
//!
//! | Module | Task | Contents |
//! |---|---|---|
//! | [`error`] | 0.2 | `AppError` taxonomy, cause chain, remedy |
//! | [`op`] | 0.3 | cancellation and progress for long operations |
//! | [`settings`] | 0.6 | versioned, atomically written preferences |
//! | [`caps`] | 0.7 | capability probing — never distro detection |
//! | [`logging`] | 0.8 | structured logging and the diagnostics bundle |
//! | [`helper`] | 0.9 | the privileged helper: protocol, client, allow-list |
//! | [`fixture`] | 0.11 | reproducible filesystem fixtures |
//! | [`budget`] | 0.11 | performance budgets from the specification |
//! | [`paths`] | — | XDG base-directory resolution |
//!
//! # What is next (Phase 1)
//!
//! `space` (1.1), `fs` (1.2), `scan` (1.3), `cache` (1.4), `reclaim` (1.8–1.9).

pub mod budget;
pub mod caps;
pub mod error;
pub mod fixture;
pub mod helper;
pub mod logging;
pub mod op;
pub mod paths;
pub mod settings;

/// Crate version, surfaced so the app and helper can verify they were built together.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn version_is_populated() {
        assert!(!VERSION.is_empty());
    }

    /// The dependency rule from `docs/ARCHITECTURE.md`, asserted rather than trusted: this crate
    /// must not reach for a GUI toolkit. If someone adds `tauri` to `nix-core`, this fails.
    #[test]
    fn core_has_no_gui_dependency() {
        let manifest = include_str!("../Cargo.toml");
        for forbidden in ["tauri", "gtk", "webkit", "nix-app"] {
            assert!(
                !manifest.contains(forbidden),
                "nix-core must not depend on {forbidden} — see docs/ARCHITECTURE.md"
            );
        }
    }
}
