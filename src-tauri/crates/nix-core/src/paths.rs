// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! XDG base-directory resolution.
//!
//! Hand-rolled rather than pulled from a crate: the spec targets Linux only, the rules are short,
//! and every dependency in a tool that ships privileged code is a dependency worth not having.
//!
//! The resolution rules live in [`resolve`], which is pure, so they are tested without mutating
//! process environment (which is `unsafe` in edition 2024, and which the workspace denies).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Application directory name used under every XDG root.
const APP: &str = "nix";

/// Resolve one XDG directory from its raw inputs.
///
/// Per the XDG base-directory spec, a value that is not an absolute path must be ignored, and the
/// default relative to `$HOME` used instead.
fn resolve(var: Option<&OsStr>, home: Option<&Path>, fallback: &str) -> Option<PathBuf> {
    if let Some(v) = var.filter(|v| !v.is_empty()) {
        let p = Path::new(v);
        if p.is_absolute() {
            return Some(p.join(APP));
        }
    }
    home.map(|h| h.join(fallback).join(APP))
}

fn home_os() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn dir_for(var: &str, fallback: &str) -> Option<PathBuf> {
    let value = std::env::var_os(var);
    let home = home_os();
    resolve(value.as_deref(), home.as_deref(), fallback)
}

/// `$XDG_CONFIG_HOME/nix` — settings live here.
pub fn config_dir() -> Option<PathBuf> {
    dir_for("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_STATE_HOME/nix` — logs and growth history live here.
pub fn state_dir() -> Option<PathBuf> {
    dir_for("XDG_STATE_HOME", ".local/state")
}

/// `$XDG_CACHE_HOME/nix` — the scan cache lives here.
pub fn cache_dir() -> Option<PathBuf> {
    dir_for("XDG_CACHE_HOME", ".cache")
}

/// The user's home directory, if resolvable.
pub fn home_dir() -> Option<PathBuf> {
    home_os()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn absolute_xdg_value_wins() {
        let v = os("/custom/cfg");
        let got = resolve(Some(&v), Some(Path::new("/home/tester")), ".config");
        assert_eq!(got.unwrap(), PathBuf::from("/custom/cfg/nix"));
    }

    #[test]
    fn unset_falls_back_to_home() {
        let got = resolve(None, Some(Path::new("/home/tester")), ".config");
        assert_eq!(got.unwrap(), PathBuf::from("/home/tester/.config/nix"));
    }

    #[test]
    fn empty_value_falls_back_to_home() {
        let v = os("");
        let got = resolve(Some(&v), Some(Path::new("/home/tester")), ".config");
        assert_eq!(got.unwrap(), PathBuf::from("/home/tester/.config/nix"));
    }

    #[test]
    fn relative_value_is_ignored_per_spec() {
        let v = os("relative/path");
        let got = resolve(Some(&v), Some(Path::new("/home/tester")), ".config");
        assert_eq!(got.unwrap(), PathBuf::from("/home/tester/.config/nix"));
    }

    #[test]
    fn no_home_and_no_var_yields_nothing() {
        assert!(resolve(None, None, ".config").is_none());
    }

    #[test]
    fn no_home_but_absolute_var_still_resolves() {
        let v = os("/custom/cfg");
        assert_eq!(
            resolve(Some(&v), None, ".config").unwrap(),
            PathBuf::from("/custom/cfg/nix")
        );
    }
}
