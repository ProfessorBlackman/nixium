// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! What starts when you log in. `PKG-4`.
//!
//! # Absent means enabled, and getting that backwards inverts the whole view
//!
//! The XDG Desktop Entry specification says an autostart entry runs unless something says otherwise:
//! `Hidden=true`, or GNOME's `X-GNOME-Autostart-enabled=false`. **Absence of both means the entry
//! runs.**
//!
//! Stacer read it the other way round:
//!
//! ```cpp
//! if (! hidden.isEmpty()) {
//!     enabled = (hidden != enabledStr);
//! } else {
//!     enabled = (gnomeEnabled == enabledStr);
//! }
//! ```
//!
//! With both keys absent, `gnomeEnabled` is the empty string, `"" == "true"` is false, and the entry
//! displays as **disabled**. That is not an edge case: neither key appears in a single one of the 44
//! autostart entries on this machine, so every entry Stacer listed was shown as disabled while
//! actually running at login. A user who ticked one to "enable" it wrote `Hidden=false` into a file
//! that was already starting.
//!
//! # Both scopes, and editing a system entry without root
//!
//! Stacer read only `$XDG_CONFIG_HOME/autostart`, so the 42 entries in `/etc/xdg/autostart` — the
//! ones a distribution actually ships, and the ones a user is most likely to want to stop — were
//! invisible.
//!
//! nix lists both, and disabling a system entry needs no privilege at all, because XDG already
//! answers the question: a file in the user directory **shadows** the system file of the same name.
//! So "stop this from starting" writes a copy carrying `Hidden=true` into the user directory, and the
//! system file is never touched. That is both the specified mechanism and the reason this whole
//! feature needs no privileged code.
//!
//! # Every key survives, including the ones this module has never heard of
//!
//! The same discipline as [`crate::hosts`]: each line keeps its original text and is re-emitted
//! verbatim unless it is the specific line being changed. That matters more here than it looks —
//! the system entries on this machine carry **338 localised keys** (`Name[de]`, `Comment[fr]`, …)
//! along with `X-GNOME-*`, `X-KDE-*` and `AutostartCondition` keys. An editor that rebuilt the file
//! from the fields it understands would silently delete all of it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, ErrorCode, IoContext, Result};
use crate::paths;

/// Where the distribution puts autostart entries. Read-only to nix.
pub const SYSTEM_DIR: &str = "/etc/xdg/autostart";

/// The section every key this module cares about lives in.
const DESKTOP_ENTRY: &str = "Desktop Entry";

/// Which directory an entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Origin {
    /// `/etc/xdg/autostart` — shipped by a package. Never written to.
    System,
    /// The user's own autostart directory.
    User,
}

/// One line of a desktop file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DesktopLine {
    /// The line as read, without its newline.
    raw: String,
    /// The section heading this line falls under, or empty before the first heading.
    ///
    /// Tracked because a key means nothing without it. A desktop file may carry
    /// `[Desktop Action new-window]` sections with their own `Name` and `Exec`, and reading those as
    /// if they were the entry's own is how the wrong `Exec` ends up on screen.
    section: String,
    /// The key, for a key-value line. Without its locale suffix.
    key: Option<String>,
    /// The locale from a `Name[de]`-style key, if any.
    locale: Option<String>,
    value: Option<String>,
}

/// A parsed desktop file that can be edited without losing anything.
#[derive(Debug, Clone)]
pub struct DesktopFile {
    lines: Vec<DesktopLine>,
    original: String,
}

impl DesktopFile {
    /// Parse a desktop file. Total — no input is rejected.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut lines = Vec::new();
        let mut section = String::new();

        let mut pieces: Vec<&str> = text.split('\n').collect();
        if pieces.last().is_some_and(|last| last.is_empty()) {
            pieces.pop();
        }

        for raw in pieces {
            let trimmed = raw.trim();

            if let Some(rest) = trimmed.strip_prefix('[') {
                if let Some(name) = rest.strip_suffix(']') {
                    section = name.to_string();
                }
                lines.push(DesktopLine {
                    raw: raw.to_string(),
                    section: section.clone(),
                    key: None,
                    locale: None,
                    value: None,
                });
                continue;
            }

            // A comment or a blank line. `#` only counts at the start of a line.
            if trimmed.is_empty() || trimmed.starts_with('#') {
                lines.push(DesktopLine {
                    raw: raw.to_string(),
                    section: section.clone(),
                    key: None,
                    locale: None,
                    value: None,
                });
                continue;
            }

            let (key, locale, value) = match raw.split_once('=') {
                Some((left, right)) => {
                    let left = left.trim();
                    // `Name[de_DE.UTF-8@euro]` — the locale is whatever is in the brackets.
                    let (bare, locale) = match left.split_once('[') {
                        Some((bare, rest)) => (
                            bare.trim(),
                            rest.strip_suffix(']').map(|l| l.trim().to_string()),
                        ),
                        None => (left, None),
                    };
                    (
                        Some(bare.to_string()),
                        locale,
                        Some(right.trim().to_string()),
                    )
                }
                None => (None, None, None),
            };

            lines.push(DesktopLine {
                raw: raw.to_string(),
                section: section.clone(),
                key,
                locale,
                value,
            });
        }

        Self {
            lines,
            original: text.to_string(),
        }
    }

    /// The unlocalised value of a key in `[Desktop Entry]`.
    ///
    /// Unlocalised deliberately: `Name` is the identity, `Name[de]` is a translation of it, and mixing
    /// them means the answer depends on which line came last.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines
            .iter()
            .find(|l| {
                l.section == DESKTOP_ENTRY
                    && l.locale.is_none()
                    && l.key
                        .as_deref()
                        .is_some_and(|k| k.eq_ignore_ascii_case(key))
            })
            .and_then(|l| l.value.as_deref())
    }

    /// A `;`-separated list value, as the specification defines them.
    #[must_use]
    pub fn get_list(&self, key: &str) -> Vec<String> {
        self.get(key)
            .map(|raw| {
                raw.split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A boolean value. The specification says `true` or `false`; case is tolerated.
    #[must_use]
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.get(key)?.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            // A value that is neither is not a boolean, and guessing at it would be inventing an
            // answer the file does not contain.
            _ => None,
        }
    }

    /// Set a key in `[Desktop Entry]`, replacing the existing line or appending one.
    ///
    /// Only the one line changes. Everything else — comments, ordering, localised variants, keys this
    /// module has never heard of — is left exactly as it was.
    pub fn set(&mut self, key: &str, value: &str) {
        if let Some(line) = self.lines.iter_mut().find(|l| {
            l.section == DESKTOP_ENTRY
                && l.locale.is_none()
                && l.key
                    .as_deref()
                    .is_some_and(|k| k.eq_ignore_ascii_case(key))
        }) {
            line.raw = format!("{key}={value}");
            line.value = Some(value.to_string());
            return;
        }

        // Append inside `[Desktop Entry]`, after its last line — not at the end of the file, which
        // for a file with `[Desktop Action …]` sections would put the key in the wrong section.
        let insert_at = self
            .lines
            .iter()
            .rposition(|l| l.section == DESKTOP_ENTRY)
            .map_or(self.lines.len(), |at| at + 1);

        self.lines.insert(
            insert_at,
            DesktopLine {
                raw: format!("{key}={value}"),
                section: DESKTOP_ENTRY.to_string(),
                key: Some(key.to_string()),
                locale: None,
                value: Some(value.to_string()),
            },
        );
    }

    /// Remove a key from `[Desktop Entry]`, if it is there. Localised variants are left alone.
    pub fn remove(&mut self, key: &str) {
        self.lines.retain(|l| {
            !(l.section == DESKTOP_ENTRY
                && l.locale.is_none()
                && l.key
                    .as_deref()
                    .is_some_and(|k| k.eq_ignore_ascii_case(key)))
        });
    }

    /// The file as it would be written.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(self.original.len() + 64);
        for line in &self.lines {
            out.push_str(&line.raw);
            out.push('\n');
        }
        out
    }

    /// Whether writing would change anything.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.render() != self.original
    }
}

/// One autostart entry, as the user needs to see it.
///
/// Exported to TypeScript as `AutostartEntry`: `journal::Entry` is also `Entry`, and ts-rs writes one
/// file per exported name, so two types called `Entry` silently overwrite each other's bindings. The
/// Rust name stays `Entry` because it is always read as `autostart::Entry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "AutostartEntry")]
pub struct Entry {
    /// The file name, which is the entry's identity and what shadowing is keyed on.
    pub id: String,
    pub path: PathBuf,
    pub origin: Origin,
    pub name: String,
    pub comment: Option<String>,
    pub exec: String,
    pub icon: Option<String>,
    /// Whether this will run at the next login.
    ///
    /// **`true` unless something says otherwise** — see the module documentation.
    pub enabled: bool,
    /// `NoDisplay=true`: the entry asks not to be shown in a UI.
    ///
    /// Honoured as a *sort order and a label*, not as a filter. 40 of the 42 system entries here set
    /// it, and hiding them would leave a "startup applications" screen that shows almost nothing —
    /// while the thing a user came to stop is very likely among them.
    pub no_display: bool,
    /// `OnlyShowIn` / `NotShowIn`, verbatim.
    pub only_show_in: Vec<String>,
    pub not_show_in: Vec<String>,
    /// Whether this entry applies to the session that is actually running.
    pub runs_in_this_session: bool,
    /// `TryExec`: the entry is skipped if this program is not present.
    pub try_exec: Option<String>,
    /// Whether `TryExec` names something that cannot be found, which means the entry will not run
    /// however enabled it looks.
    pub try_exec_missing: bool,
    /// For a system entry: whether the user directory already shadows it.
    pub shadowed: bool,
    /// Whether nix can change this entry's state. A system entry is changed by shadowing it, which is
    /// always possible, so this is about the file itself.
    pub writable: bool,
}

/// The user's autostart directory, per XDG.
pub fn user_dir() -> Result<PathBuf> {
    // `config_home`, not `config_dir` — the latter is `~/.config/nix`, which is nix's own directory.
    // The autostart directory belongs to the desktop, and using the wrong one finds no entries at all
    // while looking like it worked.
    paths::config_home()
        .map(|dir| dir.join("autostart"))
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::Unsupported,
                "Could not work out where your autostart directory is.",
            )
            .with_remedy("Set HOME or XDG_CONFIG_HOME and try again.")
        })
}

/// The desktop names of the running session, from `XDG_CURRENT_DESKTOP`.
///
/// A colon-separated list — this machine reports `ubuntu:GNOME` — and the specification says an entry
/// matches if *any* of them matches.
#[must_use]
pub fn current_desktops() -> Vec<String> {
    std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether an entry with these lists runs in a session with these desktop names.
///
/// Case-insensitive, because `XDG_CURRENT_DESKTOP` says `GNOME` while plenty of files say `gnome`.
#[must_use]
pub fn runs_in(only_show_in: &[String], not_show_in: &[String], desktops: &[String]) -> bool {
    let matches = |list: &[String]| {
        list.iter().any(|wanted| {
            desktops
                .iter()
                .any(|have| have.eq_ignore_ascii_case(wanted))
        })
    };

    // `NotShowIn` wins: an entry excluded from this desktop does not run here even if something else
    // in `OnlyShowIn` matched.
    if !not_show_in.is_empty() && matches(not_show_in) {
        return false;
    }
    if !only_show_in.is_empty() {
        return matches(only_show_in);
    }
    true
}

/// Whether a `TryExec` value names something that can be found.
///
/// An absolute path is checked directly; a bare name is looked for on `PATH`, which is what the
/// specification says and what a desktop environment actually does.
#[must_use]
pub fn try_exec_found(try_exec: &str) -> bool {
    let value = try_exec.trim();
    if value.is_empty() {
        return true;
    }
    if value.contains('/') {
        return Path::new(value).exists();
    }
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|dir| !dir.is_empty())
        .any(|dir| Path::new(dir).join(value).exists())
}

/// Read one entry from a parsed file.
fn entry_from(
    file: &DesktopFile,
    path: &Path,
    origin: Origin,
    desktops: &[String],
) -> Option<Entry> {
    let id = path.file_name()?.to_str()?.to_string();

    // A desktop file with no name is not something to display. Stacer did the same, and it is the one
    // filter that is right: an entry with no `Name` has nothing to show in a list.
    let name = file.get("Name")?.to_string();

    // The default that Stacer inverted. Either key saying "off" turns the entry off; both absent means
    // it runs.
    let hidden = file.get_bool("Hidden").unwrap_or(false);
    let gnome_enabled = file.get_bool("X-GNOME-Autostart-enabled").unwrap_or(true);
    let enabled = !hidden && gnome_enabled;

    let only_show_in = file.get_list("OnlyShowIn");
    let not_show_in = file.get_list("NotShowIn");
    let try_exec = file.get("TryExec").map(str::to_string);

    Some(Entry {
        id,
        path: path.to_path_buf(),
        origin,
        name,
        comment: file.get("Comment").map(str::to_string),
        exec: file.get("Exec").unwrap_or_default().to_string(),
        icon: file.get("Icon").map(str::to_string),
        enabled,
        no_display: file.get_bool("NoDisplay").unwrap_or(false),
        runs_in_this_session: runs_in(&only_show_in, &not_show_in, desktops),
        only_show_in,
        not_show_in,
        try_exec_missing: try_exec.as_deref().is_some_and(|t| !try_exec_found(t)),
        try_exec,
        shadowed: false,
        writable: origin == Origin::User,
    })
}

/// Read the `.desktop` files in one directory.
fn read_dir(dir: &Path, origin: Origin, desktops: &[String]) -> Vec<Entry> {
    let Ok(reading) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    for item in reading.filter_map(std::result::Result::ok) {
        let path = item.path();
        // Only `.desktop` files. The user directory on this machine also holds a `mimeinfo.cache`,
        // and a glob is the difference between reading it and trying to parse it as an entry.
        if path.extension().is_none_or(|ext| ext != "desktop") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(entry) = entry_from(&DesktopFile::parse(&text), &path, origin, desktops) {
            found.push(entry);
        }
    }
    found
}

/// Every autostart entry, from both directories.
///
/// A user entry **shadows** a system entry of the same file name, which is XDG's own mechanism. Only
/// the user's version is listed in that case, marked [`Entry::shadowed`] so the UI can say the system
/// default has been overridden rather than pretending the system entry does not exist.
pub fn list() -> Result<Vec<Entry>> {
    let desktops = current_desktops();
    let user_directory = user_dir()?;

    let user = read_dir(&user_directory, Origin::User, &desktops);
    let system = read_dir(Path::new(SYSTEM_DIR), Origin::System, &desktops);

    let mut by_id: BTreeMap<String, Entry> = BTreeMap::new();
    for entry in system {
        by_id.insert(entry.id.clone(), entry);
    }
    for mut entry in user {
        entry.shadowed = by_id.contains_key(&entry.id);
        by_id.insert(entry.id.clone(), entry);
    }

    let mut entries: Vec<Entry> = by_id.into_values().collect();
    // Things that will actually run first, then by name. A user looking for what to stop wants the
    // live ones at the top, not an alphabetical list led by disabled entries.
    entries.sort_by(|a, b| {
        b.enabled
            .cmp(&a.enabled)
            .then_with(|| b.runs_in_this_session.cmp(&a.runs_in_this_session))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Turn an entry on or off.
///
/// # Never writes to `/etc`
///
/// For a user entry this edits the file in place. For a **system** entry it writes a copy into the
/// user's autostart directory carrying the new state, which XDG defines as shadowing the system file.
/// The system file is not touched and no privilege is needed — the specification already solved the
/// problem that would otherwise have called for the helper.
pub fn set_enabled(id: &str, enabled: bool) -> Result<PathBuf> {
    let user_directory = user_dir()?;
    let target = user_directory.join(id);

    // Prefer the user's own copy if there is one; otherwise take the system file as the starting text
    // so a shadowing copy carries everything the original did.
    let (source, origin) = if target.is_file() {
        (target.clone(), Origin::User)
    } else {
        let system = Path::new(SYSTEM_DIR).join(id);
        if !system.is_file() {
            return Err(AppError::new(
                ErrorCode::NotFound,
                format!("There is no autostart entry called {id}."),
            ));
        }
        (system, Origin::System)
    };

    let text = std::fs::read_to_string(&source)
        .doing("read the autostart entry")
        .map_err(|e| e.with_path(&source))?;
    let mut file = DesktopFile::parse(&text);

    if enabled {
        // Removed rather than set to `false`: absence *is* enabled, and leaving `Hidden=false` behind
        // is how a file accumulates keys stating the default. GNOME's key is handled the same way.
        file.remove("Hidden");
        file.remove("X-GNOME-Autostart-enabled");
    } else {
        file.set("Hidden", "true");
        // The GNOME key is not added. `Hidden` is the specified mechanism and every desktop honours
        // it; writing both would mean two sources of truth in one file.
        if file.get("X-GNOME-Autostart-enabled").is_some() {
            file.set("X-GNOME-Autostart-enabled", "false");
        }
    }

    // Nothing to do, and for a system entry nothing worth creating a shadowing file over.
    if origin == Origin::User && !file.is_modified() {
        return Ok(target);
    }

    write_user_entry(&user_directory, &target, &file.render())?;
    Ok(target)
}

/// Add an entry to the user's autostart directory.
pub fn add(name: &str, exec: &str, comment: Option<&str>) -> Result<PathBuf> {
    let name = name.trim();
    let exec = exec.trim();
    if name.is_empty() {
        return Err(AppError::invalid_input("An entry needs a name."));
    }
    if exec.is_empty() {
        return Err(AppError::invalid_input("An entry needs a command to run."));
    }
    // A newline would end the key's line and let the rest be read as further keys.
    if [name, exec]
        .iter()
        .any(|s| s.contains('\n') || s.contains('\r'))
    {
        return Err(AppError::invalid_input(
            "A name or command cannot contain a line break.",
        ));
    }

    let user_directory = user_dir()?;
    let file_name = format!("{}.desktop", slug(name));
    let target = user_directory.join(&file_name);
    if target.exists() {
        return Err(AppError::new(
            ErrorCode::Refused,
            format!("{file_name} already exists in your autostart directory."),
        )
        .with_path(&target)
        .with_remedy("Edit the existing entry, or choose a different name."));
    }

    let mut text = format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\n");
    if let Some(comment) = comment.map(str::trim).filter(|c| !c.is_empty()) {
        text.push_str(&format!("Comment={comment}\n"));
    }
    // Written by nix, and worth saying so in the file itself rather than only in a UI.
    text.push_str("X-nix-Created=true\n");

    write_user_entry(&user_directory, &target, &text)?;
    Ok(target)
}

/// Remove a user entry.
///
/// A **system** entry cannot be removed, only shadowed — the file belongs to a package, and deleting
/// it would be undone by the next upgrade while breaking the package's file list in the meantime.
pub fn remove(id: &str) -> Result<()> {
    let target = user_dir()?.join(id);
    if !target.is_file() {
        return Err(AppError::new(
            ErrorCode::NotFound,
            format!("You have no autostart entry of your own called {id}."),
        )
        .with_remedy("A system entry cannot be deleted, but it can be turned off."));
    }
    std::fs::remove_file(&target)
        .doing("remove the autostart entry")
        .map_err(|e| e.with_path(&target))
}

/// A file name from a display name.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "entry".to_string()
    } else {
        trimmed
    }
}

/// Write a file in the user's autostart directory, atomically.
///
/// The same sibling-temporary-then-rename pattern as everywhere else in this project: a crash partway
/// leaves the previous file intact rather than a truncated one. Unprivileged throughout — this only
/// ever writes inside the user's own directory.
fn write_user_entry(directory: &Path, target: &Path, content: &str) -> Result<()> {
    std::fs::create_dir_all(directory)
        .doing("create your autostart directory")
        .map_err(|e| e.with_path(directory))?;

    let staging = target.with_extension(format!("desktop.nix-{}.tmp", std::process::id()));
    std::fs::write(&staging, content)
        .doing("write the autostart entry")
        .map_err(|e| e.with_path(&staging))?;
    std::fs::rename(&staging, target).map_err(|e| {
        std::fs::remove_file(&staging).ok();
        AppError::from_io(&e, "save the autostart entry").with_path(target)
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A real system entry from this machine, verbatim. `NoDisplay`, `NotShowIn`, `X-GNOME-*` and an
    /// `X-Ubuntu-*` key — and, like every entry here, neither `Hidden` nor
    /// `X-GNOME-Autostart-enabled`.
    const NM_APPLET: &str = "\
[Desktop Entry]
Name=Network
Comment=Manage your network connections
Icon=nm-device-wireless
Exec=nm-applet
Terminal=false
Type=Application
NoDisplay=true
NotShowIn=KDE;GNOME;
X-GNOME-Bugzilla-Bugzilla=GNOME
X-GNOME-Bugzilla-Product=NetworkManager
X-GNOME-Bugzilla-Component=nm-applet
X-GNOME-UsesNotifications=true
X-Ubuntu-Gettext-Domain=nm-applet
";

    fn desktops(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    // ---- the default Stacer inverted ----

    /// The whole point of this module. Neither key present means the entry runs.
    #[test]
    fn an_entry_with_neither_key_is_enabled() {
        let file = DesktopFile::parse(NM_APPLET);
        let entry = entry_from(
            &file,
            Path::new("/etc/xdg/autostart/nm-applet.desktop"),
            Origin::System,
            &desktops(&["GNOME"]),
        )
        .unwrap();

        assert!(
            entry.enabled,
            "absence of Hidden and X-GNOME-Autostart-enabled means the entry runs — reading it the \
             other way shows every entry on this machine as disabled"
        );
    }

    #[test]
    fn hidden_true_disables_and_hidden_false_does_not() {
        let on = format!("{NM_APPLET}Hidden=false\n");
        let off = format!("{NM_APPLET}Hidden=true\n");

        assert!(DesktopFile::parse(&on).get_bool("Hidden") == Some(false));
        assert!(DesktopFile::parse(&off).get_bool("Hidden") == Some(true));

        let enabled = |text: &str| {
            entry_from(
                &DesktopFile::parse(text),
                Path::new("/x/a.desktop"),
                Origin::User,
                &desktops(&["GNOME"]),
            )
            .unwrap()
            .enabled
        };
        assert!(enabled(&on));
        assert!(!enabled(&off));
    }

    /// GNOME's key disables on `false`, and its absence must not.
    #[test]
    fn the_gnome_key_disables_only_when_it_says_false() {
        let enabled = |text: &str| {
            entry_from(
                &DesktopFile::parse(text),
                Path::new("/x/a.desktop"),
                Origin::User,
                &desktops(&["GNOME"]),
            )
            .unwrap()
            .enabled
        };

        assert!(!enabled(&format!(
            "{NM_APPLET}X-GNOME-Autostart-enabled=false\n"
        )));
        assert!(enabled(&format!(
            "{NM_APPLET}X-GNOME-Autostart-enabled=true\n"
        )));
        assert!(enabled(NM_APPLET), "and absence is enabled");
    }

    /// Either key saying "off" is enough. An entry with `Hidden=false` and the GNOME key `false` is
    /// off, and taking only the first key found would call it on.
    #[test]
    fn either_key_can_turn_an_entry_off() {
        let text = format!("{NM_APPLET}Hidden=false\nX-GNOME-Autostart-enabled=false\n");
        let entry = entry_from(
            &DesktopFile::parse(&text),
            Path::new("/x/a.desktop"),
            Origin::User,
            &desktops(&["GNOME"]),
        )
        .unwrap();
        assert!(!entry.enabled);
    }

    /// A value that is not a boolean is not a boolean. Guessing would invent an answer the file does
    /// not contain, and the safe reading of a malformed `Hidden` is that it does not hide.
    #[test]
    fn a_non_boolean_value_is_not_read_as_one() {
        let file = DesktopFile::parse("[Desktop Entry]\nName=x\nExec=x\nHidden=maybe\n");
        assert_eq!(file.get_bool("Hidden"), None);

        let entry = entry_from(&file, Path::new("/x/a.desktop"), Origin::User, &[]).unwrap();
        assert!(entry.enabled, "an unreadable Hidden must not silently hide");
    }

    // ---- nothing is lost ----

    /// The guarantee. Comments, ordering, localised keys and keys this module has never heard of all
    /// survive, because untouched lines are re-emitted verbatim.
    #[test]
    fn an_untouched_file_renders_back_byte_for_byte() {
        let file = DesktopFile::parse(NM_APPLET);
        assert_eq!(file.render(), NM_APPLET);
        assert!(!file.is_modified());
    }

    /// And against every real entry on this machine, not just the captured one.
    #[test]
    fn every_real_entry_on_this_machine_renders_back_byte_for_byte() {
        let mut checked = 0;
        for dir in [PathBuf::from(SYSTEM_DIR), user_dir().unwrap_or_default()] {
            let Ok(reading) = std::fs::read_dir(&dir) else {
                continue;
            };
            for item in reading.filter_map(std::result::Result::ok) {
                let path = item.path();
                if path.extension().is_none_or(|e| e != "desktop") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                assert_eq!(
                    DesktopFile::parse(&text).render(),
                    text,
                    "{} did not survive a round trip",
                    path.display()
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "no desktop files were checked");
    }

    /// The 338 localised keys on this machine are exactly what an editor that rebuilds from known
    /// fields would delete.
    #[test]
    fn setting_a_key_leaves_localised_variants_and_unknown_keys_alone() {
        let text = "\
[Desktop Entry]
Name=Network
Name[de]=Netzwerk
Name[fr]=Réseau
Comment[de]=Netzwerkverbindungen verwalten
Exec=nm-applet
AutostartCondition=GSettings org.gnome.something enabled
X-KDE-autostart-phase=2
# a comment nobody should lose

X-Ubuntu-Gettext-Domain=nm-applet
";
        let mut file = DesktopFile::parse(text);
        file.set("Hidden", "true");
        let rendered = file.render();

        for kept in [
            "Name[de]=Netzwerk",
            "Name[fr]=Réseau",
            "Comment[de]=Netzwerkverbindungen verwalten",
            "AutostartCondition=GSettings org.gnome.something enabled",
            "X-KDE-autostart-phase=2",
            "# a comment nobody should lose",
            "X-Ubuntu-Gettext-Domain=nm-applet",
        ] {
            assert!(rendered.contains(kept), "lost {kept:?} from:\n{rendered}");
        }
        assert!(rendered.contains("Hidden=true"));
        // And the blank line is still there.
        assert!(rendered.contains("\n\nX-Ubuntu-Gettext-Domain"));
    }

    /// A localised key is a translation, not the value. Reading `Name[de]` as the name would make the
    /// display depend on which line came last.
    #[test]
    fn a_localised_key_is_never_returned_as_the_plain_value() {
        let file = DesktopFile::parse("[Desktop Entry]\nName[de]=Netzwerk\nName=Network\n");
        assert_eq!(file.get("Name"), Some("Network"));

        // And the other order, since "whichever came first" is the bug this guards.
        let flipped = DesktopFile::parse("[Desktop Entry]\nName=Network\nName[de]=Netzwerk\n");
        assert_eq!(flipped.get("Name"), Some("Network"));
    }

    /// A desktop file may carry action sections with their own `Name` and `Exec`. Reading those as the
    /// entry's own puts the wrong command on screen.
    #[test]
    fn keys_in_another_section_are_not_read_as_the_entrys_own() {
        let text = "\
[Desktop Entry]
Name=Terminal
Exec=gnome-terminal

[Desktop Action new-window]
Name=New Window
Exec=gnome-terminal --window
";
        let file = DesktopFile::parse(text);
        assert_eq!(file.get("Name"), Some("Terminal"));
        assert_eq!(file.get("Exec"), Some("gnome-terminal"));
    }

    /// And a key appended to such a file must land in `[Desktop Entry]`, not after the action section
    /// where it would mean nothing.
    #[test]
    fn an_appended_key_lands_in_the_desktop_entry_section() {
        let text = "\
[Desktop Entry]
Name=Terminal
Exec=gnome-terminal

[Desktop Action new-window]
Name=New Window
";
        let mut file = DesktopFile::parse(text);
        file.set("Hidden", "true");

        let rendered = file.render();
        let hidden_at = rendered.find("Hidden=true").unwrap();
        let action_at = rendered.find("[Desktop Action new-window]").unwrap();
        assert!(
            hidden_at < action_at,
            "Hidden landed in the wrong section:\n{rendered}"
        );

        // And it reads back from the right section.
        assert_eq!(DesktopFile::parse(&rendered).get_bool("Hidden"), Some(true));
    }

    #[test]
    fn removing_a_key_leaves_its_localised_variants() {
        let mut file = DesktopFile::parse(
            "[Desktop Entry]\nName=x\nComment=plain\nComment[de]=deutsch\nExec=x\n",
        );
        file.remove("Comment");
        let rendered = file.render();

        assert!(!rendered.contains("Comment=plain"));
        assert!(
            rendered.contains("Comment[de]=deutsch"),
            "a translation is not the key: {rendered}"
        );
    }

    // ---- session filtering ----

    #[test]
    fn only_show_in_restricts_to_the_listed_desktops() {
        assert!(runs_in(
            &desktops(&["GNOME"]),
            &[],
            &desktops(&["ubuntu", "GNOME"])
        ));
        assert!(!runs_in(
            &desktops(&["KDE"]),
            &[],
            &desktops(&["ubuntu", "GNOME"])
        ));
    }

    #[test]
    fn not_show_in_excludes_the_listed_desktops() {
        // nm-applet on this machine: NotShowIn=KDE;GNOME; in a GNOME session.
        assert!(!runs_in(
            &[],
            &desktops(&["KDE", "GNOME"]),
            &desktops(&["ubuntu", "GNOME"])
        ));
        assert!(runs_in(
            &[],
            &desktops(&["KDE"]),
            &desktops(&["ubuntu", "GNOME"])
        ));
    }

    /// Case differs in practice: `XDG_CURRENT_DESKTOP` says `GNOME` and plenty of files say `gnome`.
    #[test]
    fn desktop_matching_ignores_case() {
        assert!(runs_in(&desktops(&["gnome"]), &[], &desktops(&["GNOME"])));
        assert!(!runs_in(&[], &desktops(&["gnome"]), &desktops(&["GNOME"])));
    }

    /// An exclusion wins over an inclusion, so an entry listed in both does not run.
    #[test]
    fn an_exclusion_beats_an_inclusion() {
        assert!(!runs_in(
            &desktops(&["GNOME"]),
            &desktops(&["GNOME"]),
            &desktops(&["GNOME"])
        ));
    }

    #[test]
    fn neither_list_means_every_session() {
        assert!(runs_in(&[], &[], &desktops(&["anything"])));
        assert!(runs_in(&[], &[], &[]), "even an unknown session");
    }

    // ---- TryExec ----

    #[test]
    fn try_exec_finds_a_program_on_path_and_misses_one_that_is_not_there() {
        assert!(try_exec_found("sh"), "sh is on PATH");
        assert!(try_exec_found("/bin/sh"), "and by absolute path");
        assert!(!try_exec_found("nix-test-no-such-program-anywhere"));
        assert!(!try_exec_found("/nonexistent/nix-test-binary"));
        assert!(
            try_exec_found(""),
            "an empty value is not a missing program"
        );
    }

    /// An entry whose `TryExec` is gone will not run however enabled it looks, which is worth saying
    /// rather than leaving the user to wonder.
    #[test]
    fn a_missing_try_exec_is_reported_on_the_entry() {
        let text = "[Desktop Entry]\nName=x\nExec=x\nTryExec=nix-test-no-such-program\n";
        let entry = entry_from(
            &DesktopFile::parse(text),
            Path::new("/x/a.desktop"),
            Origin::User,
            &[],
        )
        .unwrap();

        assert!(entry.enabled, "nothing has disabled it");
        assert!(
            entry.try_exec_missing,
            "but it cannot run, and the UI needs to be able to say so"
        );
    }

    // ---- reading the machine ----

    #[test]
    fn this_machines_entries_are_listed_from_both_directories() {
        let Ok(entries) = list() else {
            return;
        };
        assert!(
            !entries.is_empty(),
            "a desktop machine has autostart entries"
        );

        assert!(
            entries.iter().any(|e| e.origin == Origin::System),
            "the 42 entries in /etc/xdg/autostart are the ones Stacer never showed"
        );

        // The inverted default, restated against reality: essentially everything here should read as
        // enabled, because neither key appears in any of these files.
        let enabled = entries.iter().filter(|e| e.enabled).count();
        assert!(
            enabled > entries.len() / 2,
            "only {enabled} of {} entries read as enabled, which suggests the default is inverted",
            entries.len()
        );

        // Identity is the file name, and shadowing means each appears once.
        let mut ids: Vec<&String> = entries.iter().map(|e| &e.id).collect();
        ids.sort();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "an entry was listed twice");

        // # Regression
        //
        // `user_dir` was built on `paths::config_dir()`, which is `~/.config/nix` — so it looked for
        // autostart entries in nix's own directory, found none, and this test passed anyway on the
        // strength of the 42 system entries. The count is not evidence; what the directory contains is.
        let user_files = user_dir()
            .ok()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .map(|reading| {
                reading
                    .filter_map(std::result::Result::ok)
                    .filter(|e| e.path().extension().is_some_and(|x| x == "desktop"))
                    .count()
            })
            .unwrap_or(0);

        if user_files > 0 {
            assert!(
                entries.iter().any(|e| e.origin == Origin::User),
                "the user autostart directory holds {user_files} .desktop file(s) and none was read \
                 — the directory being looked in is almost certainly the wrong one"
            );
        }
    }

    /// `mimeinfo.cache` sits in the user autostart directory on this machine. A glob is the difference
    /// between ignoring it and trying to parse it as an entry.
    #[test]
    fn only_desktop_files_are_read() {
        let dir = std::env::temp_dir().join(format!("nix-autostart-glob-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mimeinfo.cache"),
            "[MIME Cache]\nName=not an entry\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("real.desktop"),
            "[Desktop Entry]\nName=Real\nExec=x\n",
        )
        .unwrap();

        let found = read_dir(&dir, Origin::User, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Real");
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- writing ----

    #[test]
    fn a_name_becomes_a_reasonable_file_name() {
        assert_eq!(slug("Slack"), "slack");
        assert_eq!(slug("My App 2"), "my-app-2");
        assert_eq!(slug("  spaced  out  "), "spaced-out");
        assert_eq!(slug("!!!"), "entry", "and never an empty file name");
        assert_eq!(slug(""), "entry");
    }

    /// A newline in a value would end the line and let the rest be read as further keys — a way to set
    /// any key at all through the name field.
    #[test]
    fn a_value_containing_a_line_break_is_refused() {
        assert!(add("bad\nHidden=true", "sh", None).is_err());
        assert!(add("fine", "sh\nExec=other", None).is_err());
    }

    #[test]
    fn an_entry_needs_a_name_and_a_command() {
        assert!(add("", "sh", None).is_err());
        assert!(add("name", "", None).is_err());
        assert!(add("   ", "sh", None).is_err());
    }

    /// Disabling writes `Hidden=true`; enabling **removes** the key rather than writing
    /// `Hidden=false`, because absence is the specified default and a file should not accumulate keys
    /// restating it.
    #[test]
    fn enabling_removes_the_key_rather_than_asserting_the_default() {
        let mut file = DesktopFile::parse("[Desktop Entry]\nName=x\nExec=x\nHidden=true\n");
        file.remove("Hidden");
        file.remove("X-GNOME-Autostart-enabled");

        let rendered = file.render();
        assert!(!rendered.contains("Hidden"), "{rendered}");
        assert_eq!(rendered, "[Desktop Entry]\nName=x\nExec=x\n");
    }

    /// Removing a system entry is refused: the file belongs to a package, so deleting it breaks that
    /// package's file list and is undone by the next upgrade anyway. Shadowing is the answer.
    #[test]
    fn a_system_entry_cannot_be_removed() {
        let Ok(entries) = list() else {
            return;
        };
        let Some(system) = entries
            .iter()
            .find(|e| e.origin == Origin::System && !e.shadowed)
        else {
            return;
        };

        let error = remove(&system.id).expect_err("a system entry must not be deletable");
        assert_eq!(error.code, ErrorCode::NotFound);
        assert!(
            error
                .remedy
                .as_deref()
                .is_some_and(|r| r.contains("turned off")),
            "the refusal should point at what does work: {:?}",
            error.remedy
        );
        assert!(
            std::path::Path::new(SYSTEM_DIR).join(&system.id).is_file(),
            "and the file must still be there"
        );
    }
}
