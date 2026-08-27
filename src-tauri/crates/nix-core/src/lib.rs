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
//! # Phase 1
//!
//! | Module | Task | Contents |
//! |---|---|---|
//! | [`space`] | 1.1 | the space model and its invariants |
//! | [`fs`] | 1.2 | mount enumeration, per-filesystem accounting, btrfs honesty |
//! | [`scan`] | 1.3 | streaming, cancellable, parallel walker |
//! | [`cache`] | 1.4 | scan persistence, so the explorer opens on the last result |
//! | [`watch`] | 1.15 | inotify staleness watching over the largest directories |
//! | [`protect`] | 1.7 | paths nix must never reclaim, checked by scanner and executor |
//! | [`trash`] | 1.10 | the freedesktop trash specification, properly |
//! | [`reclaim`] | 1.8–1.9 | the preview → confirm → execute → report pipeline, and the category registry |
//!
//! Next: `reclaim` (1.8–1.9).

pub mod budget;
pub mod cache;
pub mod caps;
pub mod error;
pub mod fixture;
pub mod fs;
pub mod helper;
pub mod logging;
pub mod op;
pub mod paths;
pub mod protect;
pub mod reclaim;
pub mod scan;
pub mod settings;
pub mod space;
pub mod trash;
pub mod watch;

/// Crate version, surfaced so the app and helper can verify they were built together.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Binary-unit byte formatting, matching what the frontend renders.
///
/// Duplicated deliberately rather than shared: this is for log lines and report summaries, which are
/// produced where the numbers are, and a round trip through the frontend to format a log message
/// would be absurd.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const KIBI: u64 = 1024;
    const MEBI: u64 = KIBI * 1024;
    const GIBI: u64 = MEBI * 1024;
    const TEBI: u64 = GIBI * 1024;

    #[allow(clippy::cast_precision_loss)]
    match bytes {
        1 => "1 byte".to_string(),
        b if b < KIBI => format!("{b} bytes"),
        b if b < MEBI => format!("{:.1} KiB", b as f64 / KIBI as f64),
        b if b < GIBI => format!("{:.1} MiB", b as f64 / MEBI as f64),
        b if b < TEBI => format!("{:.1} GiB", b as f64 / GIBI as f64),
        b => format!("{:.1} TiB", b as f64 / TEBI as f64),
    }
}

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
