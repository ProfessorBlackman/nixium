// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

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
//! | [`cow`] | STO-17 | copy-on-write sharing, so a reclaim estimate is never overstated |
//! | [`scan`] | 1.3 | streaming, cancellable, parallel walker |
//! | [`cache`] | 1.4 | scan persistence, so the explorer opens on the last result |
//! | [`watch`] | 1.15 | inotify staleness watching over the largest directories |
//! | [`protect`] | 1.7 | paths nix must never reclaim, checked by scanner and executor |
//! | [`trash`] | 1.10 | the freedesktop trash specification, properly |
//! | [`reclaim`] | 1.8–1.9 | the preview → confirm → execute → report pipeline, and the category registry |
//!
//! Next: `reclaim` (1.8–1.9).

pub mod autostart;
pub mod budget;
pub mod cache;
pub mod caps;
pub mod cow;
pub mod detail;
pub mod error;
pub mod find;
pub mod fixture;
pub mod fs;
pub mod helper;
pub mod history;
pub mod hosts;
pub mod journal;
pub mod logging;
pub mod metrics;
pub mod op;
pub mod paths;
pub mod pkg;
pub mod process;
pub mod protect;
pub mod reclaim;
pub mod scan;
pub mod settings;
pub mod signal;
pub mod space;
pub mod timer;
pub mod trash;
pub mod units;
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

    /// Two Rust types with the same name generate the same TypeScript file, and ts-rs silently lets
    /// the second overwrite the first — so one type ends up with no binding at all while the code
    /// still compiles. That happened once, with `caps::Snapshot` clobbering `cow::Snapshot`, and it
    /// only surfaced because a binding was noticed missing. This makes it a build failure instead.
    #[test]
    fn no_two_exported_types_share_a_name() {
        use std::collections::HashMap;

        let mut seen: HashMap<String, Vec<String>> = HashMap::new();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

        fn walk(dir: &std::path::Path, found: &mut Vec<(String, String)>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.filter_map(std::result::Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, found);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let Ok(source) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    // A type is exported when a `#[ts(export...)]` attribute precedes it.
                    let lines: Vec<&str> = source.lines().collect();
                    for (i, line) in lines.iter().enumerate() {
                        if !line.trim_start().starts_with("#[ts(export") {
                            continue;
                        }
                        // An explicit `rename` decides the exported name, and is how a genuine clash
                        // is *resolved* — so a guard that ignores it reports the resolved case as
                        // still broken. `autostart::Entry` is renamed to `AutostartEntry` for exactly
                        // this reason, and this test failed on it until it learned to look.
                        if let Some(renamed) = line
                            .split_once("rename = \"")
                            .and_then(|(_, rest)| rest.split_once('"'))
                            .map(|(name, _)| name)
                        {
                            found.push((renamed.to_string(), path.display().to_string()));
                            continue;
                        }

                        // The declaration is the next line that declares a type.
                        for next in lines.iter().skip(i + 1).take(6) {
                            if let Some(name) = next
                                .split_whitespace()
                                .skip_while(|w| *w != "struct" && *w != "enum")
                                .nth(1)
                            {
                                let name = name
                                    .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
                                if !name.is_empty() {
                                    found.push((name.to_string(), path.display().to_string()));
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }

        let mut found = Vec::new();
        walk(&root, &mut found);
        assert!(
            found.len() > 20,
            "the scan found only {} exported types",
            found.len()
        );

        for (name, file) in found {
            seen.entry(name).or_default().push(file);
        }

        let clashes: Vec<_> = seen.iter().filter(|(_, files)| files.len() > 1).collect();
        assert!(
            clashes.is_empty(),
            "these type names are exported more than once, so their bindings overwrite each \
             other: {clashes:?}"
        );
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
