//! Core library for nix: the space model, filesystem scanners, and metric samplers.
//!
//! This crate deliberately has **no GUI and no Tauri dependency**, so that everything it does can
//! be exercised from tests and from the command line without an app running. The same separation
//! was the one clearly good architectural decision in Stacer, whose `stacer-core` static library
//! held all system access behind a GUI-free boundary.
//!
//! Nothing here may depend on `nix-app`. The dependency direction is one-way:
//! `nix-app` → `nix-core`, and `nix-helper` → `nix-core`.
//!
//! Module layout as it lands (see `docs/PLAN.md`):
//!
//! | Module | Task | Contents |
//! |---|---|---|
//! | `error` | 0.2 | `AppError` taxonomy, cause chain, remedy |
//! | `caps` | 0.7 | capability probe registry |
//! | `space` | 1.1 | the space model and its invariants |
//! | `fs` | 1.2 | mount enumeration, per-filesystem accounting |
//! | `scan` | 1.3 | streaming cancellable walker |

/// Crate version, surfaced so the app and helper can verify they were built together.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_populated() {
        assert!(!VERSION.is_empty());
    }
}
