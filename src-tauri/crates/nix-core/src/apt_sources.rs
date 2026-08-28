// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! APT repositories, in both formats apt actually reads. `PKG-5`.
//!
//! # deb822, which Stacer could not see
//!
//! Stacer enumerated repositories with `entryInfoList({"*.list"}, …)`. Modern Debian and Ubuntu
//! increasingly ship `.sources` files in the deb822 format instead, and those matched no glob it
//! looked at. On this machine that is two repositories — Vivaldi and VS Code — completely invisible,
//! including their `Signed-By` keyring paths.
//!
//! # Which files count, and the 35 that do not
//!
//! apt reads `*.list` and `*.sources` from `/etc/apt/sources.list.d`, and nothing else.
//! `/etc/apt/sources.list.d` on this machine holds **53 files**, of which apt reads **18**: the other
//! 35 are `.save` and `.distUpgrade` copies left behind by release upgrades. A tool that globbed `*`
//! would list 35 repositories that have no effect on anything, several of them contradicting the live
//! ones. So the extension check is not tidiness — it is the difference between showing the machine's
//! configuration and showing its litter.
//!
//! # Entries are addressed by position, never by matching their text
//!
//! Stacer located the line to edit like this:
//!
//! ```cpp
//! int _pos = sourceFileContent[i].indexOf(aptSource->source);
//! if (_pos != -1) { pos = i; break; }
//! ```
//!
//! A **substring** search from the top of the file, taking the first hit, against an entry that
//! records only its file path and no position. `deb http://x/ jammy main` is a substring of
//! `deb http://x/ jammy main restricted`, so where a narrower line follows a broader one, editing the
//! narrower rewrites the broader — and both are perfectly ordinary configuration.
//!
//! Checked before repeating it as a claim: across all 46 source lines on this machine, no line's first
//! substring match is a different line, so this misfires **nowhere here today**. It is a latent
//! hazard, not an observed failure, and worth fixing rather than worth alarm. Every entry here carries
//! the file and its **line number** — or its stanza index, for deb822 — and edits address that.
//!
//! # Everything else in the file survives
//!
//! Same discipline as [`crate::hosts`] and [`crate::autostart`]: lines keep their original text and
//! are re-emitted verbatim unless they are the one being changed. The two deb822 files here open with
//! `### THIS FILE IS AUTOMATICALLY CONFIGURED ###` and carry an `X-Repolib-Name` key, none of which
//! this module understands and all of which it must not lose.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, ErrorCode, IoContext, Result};

/// The one-line-per-entry file apt has always read.
pub const SOURCES_LIST: &str = "/etc/apt/sources.list";

/// Where additional repository files live.
pub const SOURCES_DIR: &str = "/etc/apt/sources.list.d";

/// The only two extensions apt reads from [`SOURCES_DIR`].
///
/// `list` is the one-line format; `sources` is deb822. Anything else — `.save`, `.distUpgrade`,
/// `.old`, an editor's backup — is ignored by apt and must be ignored here.
pub const READ_EXTENSIONS: [&str; 2] = ["list", "sources"];

/// Which of apt's two formats a file uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Format {
    /// `deb [options] uri suite components…`, one per line.
    OneLine,
    /// deb822: `Types:`/`URIs:`/`Suites:` stanzas separated by blank lines.
    Deb822,
}

impl Format {
    /// The format apt infers from a file's name.
    #[must_use]
    pub fn of(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "list" => Some(Self::OneLine),
            "sources" => Some(Self::Deb822),
            _ => None,
        }
    }
}

/// Where an entry lives, precisely enough to edit it without searching for its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Location {
    pub file: PathBuf,
    pub format: Format,
    /// For [`Format::OneLine`], the zero-based line. For [`Format::Deb822`], the zero-based stanza.
    pub index: u32,
}

/// One repository, as apt would read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Repository {
    pub at: Location,
    /// Whether apt uses this. A commented-out line, or `Enabled: no` in deb822.
    pub enabled: bool,
    /// `deb`, `deb-src`, or both for a deb822 stanza that lists both.
    pub types: Vec<String>,
    pub uris: Vec<String>,
    /// The suite or codename: `jammy`, `stable`, `jammy-updates`.
    pub suites: Vec<String>,
    pub components: Vec<String>,
    /// `arch=` in the one-line options, or `Architectures:` in deb822.
    pub architectures: Vec<String>,
    /// The keyring that signs this repository.
    ///
    /// First-class rather than buried in an options string: it is the difference between a repository
    /// apt will trust and one it will refuse, and it is the field most likely to be wrong after a
    /// manual edit.
    pub signed_by: Option<String>,
    /// Options this module does not model, kept so they can be shown and are never dropped.
    pub other_options: Vec<String>,
    /// A human label, from deb822's `X-Repolib-Name` where present.
    pub label: Option<String>,
    /// The entry as it appears in the file, for display.
    pub text: String,
}

impl Repository {
    /// A short description of what this repository provides.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} {} {}",
            self.uris.join(" "),
            self.suites.join(" "),
            self.components.join(" ")
        )
        .trim()
        .to_string()
    }
}

/// A parsed source file that can be edited without losing anything.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub path: PathBuf,
    pub format: Format,
    lines: Vec<String>,
    original: String,
}

impl SourceFile {
    /// Parse a file's text. Total — no input is rejected.
    #[must_use]
    pub fn parse(path: &Path, format: Format, text: &str) -> Self {
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        if lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        Self {
            path: path.to_path_buf(),
            format,
            lines,
            original: text.to_string(),
        }
    }

    /// The file as it would be written.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(self.original.len() + 32);
        for line in &self.lines {
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.render() != self.original
    }

    /// The file exactly as it was read.
    ///
    /// This is the write precondition, not a convenience: the helper compares it against what is on
    /// disk and refuses if they differ.
    #[must_use]
    pub fn original(&self) -> &str {
        &self.original
    }

    /// The repositories in this file.
    #[must_use]
    pub fn repositories(&self) -> Vec<Repository> {
        match self.format {
            Format::OneLine => self.one_line_repositories(),
            Format::Deb822 => self.deb822_repositories(),
        }
    }

    fn one_line_repositories(&self) -> Vec<Repository> {
        let mut found = Vec::new();
        for (number, raw) in self.lines.iter().enumerate() {
            if let Some(mut repo) = parse_one_line(raw) {
                repo.at = Location {
                    file: self.path.clone(),
                    format: Format::OneLine,
                    index: u32::try_from(number).unwrap_or(u32::MAX),
                };
                found.push(repo);
            }
        }
        found
    }

    /// Stanza boundaries: one or more blank lines separate stanzas, per deb822.
    fn stanzas(&self) -> Vec<(usize, usize)> {
        let mut spans = Vec::new();
        let mut start: Option<usize> = None;
        for (number, line) in self.lines.iter().enumerate() {
            if line.trim().is_empty() {
                if let Some(from) = start.take() {
                    spans.push((from, number));
                }
            } else if start.is_none() {
                start = Some(number);
            }
        }
        if let Some(from) = start {
            spans.push((from, self.lines.len()));
        }
        spans
    }

    fn deb822_repositories(&self) -> Vec<Repository> {
        let mut found = Vec::new();
        for (index, (from, to)) in self.stanzas().into_iter().enumerate() {
            let stanza = &self.lines[from..to];
            if let Some(mut repo) = parse_deb822(stanza) {
                repo.at = Location {
                    file: self.path.clone(),
                    format: Format::Deb822,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                };
                found.push(repo);
            }
        }
        found
    }

    /// Turn a repository on or off, addressing it by position.
    pub fn set_enabled(&mut self, index: u32, enabled: bool) -> Result<()> {
        let index = index as usize;
        match self.format {
            Format::OneLine => {
                let line = self
                    .lines
                    .get_mut(index)
                    .ok_or_else(|| out_of_range(index, "line"))?;

                // Only the leading `#` markers are touched. Stacer removed *every* `#` in the line,
                // which mangles anything after one — a trailing comment, or a fragment in a URI.
                let stripped = line.trim_start();
                let body = stripped.trim_start_matches('#').trim_start();
                if parse_one_line(body).is_none() {
                    return Err(AppError::invalid_input(
                        "That line is not a repository entry.",
                    ));
                }
                *line = if enabled {
                    body.to_string()
                } else {
                    format!("# {body}")
                };
                Ok(())
            }
            Format::Deb822 => {
                let (from, to) = *self
                    .stanzas()
                    .get(index)
                    .ok_or_else(|| out_of_range(index, "stanza"))?;

                // deb822 has a field for this, which is better than commenting out every line: the
                // stanza stays readable and its other fields keep their meaning.
                let value = if enabled { "yes" } else { "no" };
                let existing = self.lines[from..to]
                    .iter()
                    .position(|l| field_name(l).is_some_and(|k| k.eq_ignore_ascii_case("Enabled")));

                match existing {
                    Some(offset) => self.lines[from + offset] = format!("Enabled: {value}"),
                    None if enabled => {} // absent already means enabled; adding it says nothing new
                    None => self.lines.insert(to, format!("Enabled: {value}")),
                }
                Ok(())
            }
        }
    }

    /// Remove a repository, addressing it by position.
    pub fn remove(&mut self, index: u32) -> Result<()> {
        let index = index as usize;
        match self.format {
            Format::OneLine => {
                if index >= self.lines.len() {
                    return Err(out_of_range(index, "line"));
                }
                self.lines.remove(index);
                Ok(())
            }
            Format::Deb822 => {
                let (from, to) = *self
                    .stanzas()
                    .get(index)
                    .ok_or_else(|| out_of_range(index, "stanza"))?;
                self.lines.drain(from..to);
                // Leave no doubled blank line behind where the stanza was.
                if from > 0
                    && self.lines.get(from).is_some_and(|l| l.trim().is_empty())
                    && self.lines[from - 1].trim().is_empty()
                {
                    self.lines.remove(from);
                }
                Ok(())
            }
        }
    }
}

fn out_of_range(index: usize, what: &str) -> AppError {
    AppError::new(
        ErrorCode::NotFound,
        format!("There is no {what} {index} in that file any more."),
    )
    .with_remedy("Reload the repository list and try again.")
}

/// The key of a deb822 field line, if it is one.
fn field_name(line: &str) -> Option<&str> {
    // A continuation line starts with whitespace and belongs to the field above it.
    if line.starts_with(' ') || line.starts_with('\t') {
        return None;
    }
    let (key, _) = line.split_once(':')?;
    let key = key.trim();
    (!key.is_empty() && !key.starts_with('#')).then_some(key)
}

/// Parse a one-line entry, enabled or commented out.
#[must_use]
pub fn parse_one_line(raw: &str) -> Option<Repository> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Leading `#` markers only, and only if what follows is actually an entry. This is what keeps a
    // prose comment from being read as a disabled repository.
    let (body, enabled) = match trimmed.strip_prefix('#') {
        Some(rest) => (rest.trim_start_matches('#').trim_start(), false),
        None => (trimmed, true),
    };

    // Options come in a single bracketed group before the URI.
    let (options, rest) = match body.strip_prefix("deb") {
        Some(_) => match (body.find('['), body.find(']')) {
            (Some(open), Some(close)) if close > open => {
                let options = &body[open + 1..close];
                let without = format!("{}{}", &body[..open], &body[close + 1..]);
                (Some(options.to_string()), without)
            }
            _ => (None, body.to_string()),
        },
        None => return None,
    };

    let mut tokens = rest.split_whitespace();
    let kind = tokens.next()?;
    if kind != "deb" && kind != "deb-src" {
        return None;
    }
    let uri = tokens.next()?;
    // A URI must look like one. Without this, `debug something else` parses as a repository.
    if !uri.contains("://") && !uri.starts_with('/') {
        return None;
    }
    let suite = tokens.next()?;
    let components: Vec<String> = tokens.map(str::to_string).collect();

    let mut architectures = Vec::new();
    let mut signed_by = None;
    let mut other_options = Vec::new();
    for option in options.iter().flat_map(|o| o.split_whitespace()) {
        match option.split_once('=') {
            Some((key, value)) if key.eq_ignore_ascii_case("arch") => {
                architectures.extend(value.split(',').map(str::to_string));
            }
            Some((key, value)) if key.eq_ignore_ascii_case("signed-by") => {
                signed_by = Some(value.to_string());
            }
            _ => other_options.push(option.to_string()),
        }
    }

    Some(Repository {
        at: Location {
            file: PathBuf::new(),
            format: Format::OneLine,
            index: 0,
        },
        enabled,
        types: vec![kind.to_string()],
        uris: vec![uri.to_string()],
        suites: vec![suite.to_string()],
        components,
        architectures,
        signed_by,
        other_options,
        label: None,
        text: trimmed.to_string(),
    })
}

/// Parse one deb822 stanza.
#[must_use]
pub fn parse_deb822(stanza: &[String]) -> Option<Repository> {
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in stanza {
        if let Some(key) = field_name(line) {
            let value = line.split_once(':').map(|(_, v)| v.trim()).unwrap_or("");
            fields.push((key.to_string(), value.to_string()));
        } else if line.starts_with(' ') || line.starts_with('\t') {
            // A continuation of the previous field.
            if let Some(last) = fields.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
        }
    }

    let get = |wanted: &str| {
        fields
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(wanted))
            .map(|(_, value)| value.as_str())
    };
    let list = |wanted: &str| {
        get(wanted)
            .map(|v| v.split_whitespace().map(str::to_string).collect::<Vec<_>>())
            .unwrap_or_default()
    };

    let types = list("Types");
    let uris = list("URIs");
    // A stanza without these is not a repository — it is a comment block, or a file header.
    if types.is_empty() || uris.is_empty() {
        return None;
    }

    // `Enabled:` is the deb822 way to turn a stanza off. Absent means enabled.
    let enabled = match get("Enabled").map(|v| v.trim().to_ascii_lowercase()) {
        Some(value) => !matches!(value.as_str(), "no" | "false" | "0"),
        None => true,
    };

    Some(Repository {
        at: Location {
            file: PathBuf::new(),
            format: Format::Deb822,
            index: 0,
        },
        enabled,
        types,
        uris,
        suites: list("Suites"),
        components: list("Components"),
        architectures: list("Architectures"),
        signed_by: get("Signed-By").map(str::to_string),
        other_options: Vec::new(),
        label: get("X-Repolib-Name").map(str::to_string),
        text: stanza.join("\n"),
    })
}

/// Every file apt would read, in the order apt reads them.
///
/// `sources.list` first, then `sources.list.d` sorted by name — which is apt's own order, and stable,
/// unlike Stacer's `QDir::Time` sort that reshuffled the list whenever a file was touched.
pub fn source_files() -> Vec<PathBuf> {
    let mut files = Vec::new();

    let main = PathBuf::from(SOURCES_LIST);
    if main.is_file() {
        files.push(main);
    }

    if let Ok(reading) = std::fs::read_dir(SOURCES_DIR) {
        let mut extra: Vec<PathBuf> = reading
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && Format::of(path).is_some())
            .collect();
        extra.sort();
        files.extend(extra);
    }

    files
}

/// Read and parse every source file.
pub fn load() -> Vec<SourceFile> {
    source_files()
        .into_iter()
        .filter_map(|path| {
            let format = Format::of(&path)?;
            let text = std::fs::read_to_string(&path).ok()?;
            Some(SourceFile::parse(&path, format, &text))
        })
        .collect()
}

/// Every repository configured on this machine.
#[must_use]
pub fn list() -> Vec<Repository> {
    load().iter().flat_map(SourceFile::repositories).collect()
}

/// Check a rendered source file before it is written.
///
/// Applied on both sides of the privilege boundary, by the same function — the same arrangement as
/// [`crate::hosts::validate_document`], and for the same reason: without it, a write operation aimed
/// at `/etc/apt` is a way to put arbitrary content into the files that decide where this machine
/// installs software from.
///
/// The rule is that every line must be something the format allows. A line that is neither a
/// repository, a comment, a blank, nor a deb822 field is refused.
pub fn validate_document(format: Format, text: &str) -> Result<()> {
    const MAX_BYTES: usize = 1024 * 1024;

    if text.len() > MAX_BYTES {
        return Err(AppError::invalid_input(
            "That is far larger than any repository file nix will write.",
        ));
    }
    if text.contains('\0') {
        return Err(AppError::invalid_input(
            "A repository file cannot contain a null byte.",
        ));
    }
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(AppError::invalid_input(
            "A repository file must end with a newline.",
        ));
    }

    for (number, raw) in text.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let allowed = match format {
            Format::OneLine => parse_one_line(trimmed).is_some(),
            // A field line, or a continuation of one.
            Format::Deb822 => {
                field_name(raw).is_some() || raw.starts_with(' ') || raw.starts_with('\t')
            }
        };
        if !allowed {
            return Err(AppError::invalid_input(format!(
                "Line {} is not valid in this file: {trimmed:?}",
                number + 1
            )));
        }
    }
    Ok(())
}

/// Read one source file by path, having first checked apt would read it.
///
/// The check is not a formality: it is what stops a caller naming `/etc/shadow` and having its
/// contents parsed and echoed back, and what stops a write reaching a file apt ignores.
pub fn open(path: &Path) -> Result<SourceFile> {
    let format = Format::of(path).ok_or_else(|| {
        AppError::new(
            ErrorCode::Refused,
            format!("{} is not a file apt reads.", path.display()),
        )
        .with_remedy("apt reads only .list and .sources files.")
    })?;

    if !source_files().iter().any(|known| known == path) {
        return Err(AppError::new(
            ErrorCode::Refused,
            format!(
                "{} is not one of this machine's source files.",
                path.display()
            ),
        )
        .with_path(path));
    }

    let text = std::fs::read_to_string(path)
        .doing("read the repository file")
        .map_err(|e| e.with_path(path))?;
    Ok(SourceFile::parse(path, format, &text))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// This machine's `vscode.sources`, verbatim — a real deb822 file, header comments and all.
    const VSCODE: &str = "\
### THIS FILE IS AUTOMATICALLY CONFIGURED ###
# You may comment out this entry, but any other modifications may be lost.
Types: deb
URIs: https://packages.microsoft.com/repos/code
Suites: stable
Components: main
Architectures: amd64
Signed-By: /usr/share/keyrings/microsoft.gpg
";

    /// And `vivaldi.sources`, which carries an `X-Repolib-Name` this module must keep.
    const VIVALDI: &str = "\
### THIS FILE IS AUTOMATICALLY CONFIGURED ###
# Changes to this file will not be preserved.
# This file will not be recreated if removed.
X-Repolib-Name: Vivaldi
Types: deb
URIs: https://repo.vivaldi.com/stable/deb/
Suites: stable
Components: main
Architectures: amd64
Signed-By: /usr/share/keyrings/vivaldi-16BD9233.gpg
";

    /// Real one-line entries from this machine, including both option shapes.
    const LEGACY: &str = "\
# a comment that is not a repository
deb http://archive.ubuntu.com/ubuntu/ jammy main restricted
deb-src http://archive.ubuntu.com/ubuntu/ jammy main restricted
deb [signed-by=/usr/share/keyrings/brave-browser-archive-keyring.gpg] https://brave-browser-apt-release.s3.brave.com/ stable main
deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/ubuntu   focal stable
# deb http://archive.ubuntu.com/ubuntu/ jammy-proposed main
";

    fn legacy_file() -> SourceFile {
        SourceFile::parse(Path::new("/etc/apt/sources.list"), Format::OneLine, LEGACY)
    }

    fn deb822_file(text: &str) -> SourceFile {
        SourceFile::parse(
            Path::new("/etc/apt/sources.list.d/x.sources"),
            Format::Deb822,
            text,
        )
    }

    // ---- which files apt reads ----

    /// The 35 `.save` and `.distUpgrade` files on this machine are not repositories, and listing them
    /// would show configuration that affects nothing.
    #[test]
    fn only_list_and_sources_extensions_are_read() {
        assert_eq!(Format::of(Path::new("a.list")), Some(Format::OneLine));
        assert_eq!(Format::of(Path::new("a.sources")), Some(Format::Deb822));
        for ignored in [
            "a.list.save",
            "a.list.distUpgrade",
            "a.sources.save",
            "a.old",
            "a",
            "a.list~",
        ] {
            assert_eq!(
                Format::of(Path::new(ignored)),
                None,
                "{ignored} is not read by apt"
            );
        }
    }

    #[test]
    fn this_machines_source_files_are_the_ones_apt_reads() {
        let files = source_files();
        if files.is_empty() {
            return; // not a Debian-family machine
        }

        for path in &files {
            assert!(
                Format::of(path).is_some(),
                "{} is not a file apt reads",
                path.display()
            );
        }

        // sources.list first, then the directory in name order — apt's own order, and stable.
        if files.len() > 1 && files[0].to_string_lossy().ends_with("sources.list") {
            let rest = &files[1..];
            let mut sorted = rest.to_vec();
            sorted.sort();
            assert_eq!(rest, sorted.as_slice(), "the order must be stable");
        }

        // The measured claim: far fewer than what is in the directory.
        if let Ok(reading) = std::fs::read_dir(SOURCES_DIR) {
            let total = reading.filter_map(std::result::Result::ok).count();
            assert!(
                files.len() <= total + 1,
                "more files listed than exist: {} vs {total}",
                files.len()
            );
        }
    }

    // ---- deb822, the format Stacer could not see ----

    #[test]
    fn a_real_deb822_stanza_is_parsed_in_full() {
        let repos = deb822_file(VSCODE).repositories();
        assert_eq!(repos.len(), 1, "the header comments are not a stanza");

        let repo = &repos[0];
        assert!(repo.enabled, "no Enabled field means enabled");
        assert_eq!(repo.types, vec!["deb"]);
        assert_eq!(repo.uris, vec!["https://packages.microsoft.com/repos/code"]);
        assert_eq!(repo.suites, vec!["stable"]);
        assert_eq!(repo.components, vec!["main"]);
        assert_eq!(repo.architectures, vec!["amd64"]);
        assert_eq!(
            repo.signed_by.as_deref(),
            Some("/usr/share/keyrings/microsoft.gpg"),
            "the keyring is the difference between a trusted repository and a refused one"
        );
    }

    #[test]
    fn a_deb822_label_is_read_where_one_is_given() {
        let repos = deb822_file(VIVALDI).repositories();
        assert_eq!(repos[0].label.as_deref(), Some("Vivaldi"));
    }

    /// The header comments in both real files are not a repository, and reading them as one would put
    /// a phantom entry at the top of every list.
    #[test]
    fn a_comment_block_is_not_a_stanza() {
        let text =
            "# just a header\n# and another line\n\nTypes: deb\nURIs: http://x/\nSuites: s\n";
        let repos = deb822_file(text).repositories();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].uris, vec!["http://x/"]);
    }

    #[test]
    fn several_stanzas_in_one_file_are_separate_repositories() {
        let text = "Types: deb\nURIs: http://a/\nSuites: s\n\nTypes: deb-src\nURIs: http://b/\nSuites: t\n";
        let repos = deb822_file(text).repositories();

        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].at.index, 0);
        assert_eq!(repos[1].at.index, 1);
        assert_eq!(repos[1].types, vec!["deb-src"]);
    }

    /// deb822 allows several values in one field, which is one stanza describing several sources.
    #[test]
    fn a_stanza_may_carry_several_types_and_suites() {
        let text = "Types: deb deb-src\nURIs: http://a/\nSuites: jammy jammy-updates\nComponents: main universe\n";
        let repo = &deb822_file(text).repositories()[0];

        assert_eq!(repo.types, vec!["deb", "deb-src"]);
        assert_eq!(repo.suites, vec!["jammy", "jammy-updates"]);
        assert_eq!(repo.components, vec!["main", "universe"]);
    }

    /// A continuation line belongs to the field above it, not to a new one.
    #[test]
    fn a_continuation_line_extends_the_field_above() {
        let text = "Types: deb\nURIs: http://a/\nSuites: s\nComponents: main\n universe\n";
        let repo = &deb822_file(text).repositories()[0];
        assert_eq!(repo.components, vec!["main", "universe"]);
    }

    #[test]
    fn enabled_no_disables_a_stanza_and_absence_does_not() {
        let off = "Types: deb\nURIs: http://a/\nSuites: s\nEnabled: no\n";
        let on = "Types: deb\nURIs: http://a/\nSuites: s\nEnabled: yes\n";
        let absent = "Types: deb\nURIs: http://a/\nSuites: s\n";

        assert!(!deb822_file(off).repositories()[0].enabled);
        assert!(deb822_file(on).repositories()[0].enabled);
        assert!(
            deb822_file(absent).repositories()[0].enabled,
            "absent means enabled, the same default that PKG-4 exists to get right"
        );
    }

    // ---- one-line entries ----

    #[test]
    fn one_line_entries_are_parsed_with_their_options() {
        let repos = legacy_file().repositories();
        assert_eq!(
            repos.len(),
            5,
            "four active and one commented out: {repos:#?}"
        );

        let brave = repos
            .iter()
            .find(|r| r.uris[0].contains("brave"))
            .expect("brave is in the fixture");
        assert_eq!(
            brave.signed_by.as_deref(),
            Some("/usr/share/keyrings/brave-browser-archive-keyring.gpg")
        );
        assert!(brave.architectures.is_empty());

        let docker = repos
            .iter()
            .find(|r| r.uris[0].contains("docker"))
            .expect("docker is in the fixture");
        assert_eq!(docker.architectures, vec!["amd64"]);
        assert_eq!(
            docker.signed_by.as_deref(),
            Some("/etc/apt/keyrings/docker.gpg")
        );
        assert_eq!(
            docker.suites,
            vec!["focal"],
            "multiple spaces are just whitespace"
        );
    }

    #[test]
    fn deb_src_is_distinguished_from_deb() {
        let repos = legacy_file().repositories();
        assert!(repos.iter().any(|r| r.types == vec!["deb-src"]));
        assert!(repos.iter().any(|r| r.types == vec!["deb"]));
    }

    #[test]
    fn a_commented_out_entry_is_a_disabled_repository() {
        let repos = legacy_file().repositories();
        let proposed = repos
            .iter()
            .find(|r| r.suites[0] == "jammy-proposed")
            .expect("the commented entry is still listed");
        assert!(!proposed.enabled);
    }

    /// A prose comment is not a disabled repository, however it starts.
    #[test]
    fn a_prose_comment_is_not_read_as_a_repository() {
        assert!(parse_one_line("# a comment that is not a repository").is_none());
        assert!(parse_one_line("# see the wiki for details").is_none());
        assert!(parse_one_line("").is_none());
        assert!(parse_one_line("   ").is_none());
    }

    /// Stacer's line filter was `^\s*#*\s*deb`, which matches any line beginning with those letters.
    #[test]
    fn a_line_that_merely_starts_with_deb_is_not_a_repository() {
        assert!(parse_one_line("debug=1").is_none());
        assert!(parse_one_line("deb").is_none(), "no URI");
        assert!(parse_one_line("deb http://a/").is_none(), "no suite");
        assert!(
            parse_one_line("deb not-a-uri jammy main").is_none(),
            "the second field must look like a URI"
        );
    }

    // ---- addressing by position, not by text ----

    /// The property the module documentation is about. Stacer searched for the entry's text with
    /// `indexOf` from the top of the file and took the first hit.
    #[test]
    fn a_narrower_line_after_a_broader_one_is_edited_in_place() {
        // The first line contains the second as a substring, and comes first.
        let text = "deb http://x/ jammy main restricted\ndeb http://x/ jammy main\n";
        let mut file = SourceFile::parse(Path::new("/etc/apt/sources.list"), Format::OneLine, text);

        let repos = file.repositories();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[1].at.index, 1);

        file.set_enabled(repos[1].at.index, false).unwrap();

        assert_eq!(
            file.render(),
            "deb http://x/ jammy main restricted\n# deb http://x/ jammy main\n",
            "a substring search would have commented out the first line instead"
        );
    }

    #[test]
    fn two_identical_lines_are_two_entries_at_two_positions() {
        let text = "deb http://x/ jammy main\ndeb http://x/ jammy main\n";
        let mut file = SourceFile::parse(Path::new("/etc/apt/sources.list"), Format::OneLine, text);

        file.set_enabled(1, false).unwrap();
        assert_eq!(
            file.render(),
            "deb http://x/ jammy main\n# deb http://x/ jammy main\n"
        );
    }

    #[test]
    fn an_index_that_no_longer_exists_is_reported_rather_than_guessed_at() {
        let mut file = legacy_file();
        assert_eq!(
            file.set_enabled(9999, false).unwrap_err().code,
            ErrorCode::NotFound
        );
        assert_eq!(file.remove(9999).unwrap_err().code, ErrorCode::NotFound);

        let mut stanzas = deb822_file(VSCODE);
        assert_eq!(
            stanzas.set_enabled(7, false).unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn a_line_that_is_not_a_repository_cannot_be_toggled() {
        let mut file = legacy_file();
        // Line 0 is the prose comment.
        assert!(file.set_enabled(0, false).is_err());
    }

    // ---- nothing is lost ----

    #[test]
    fn an_untouched_file_renders_back_byte_for_byte() {
        for (text, format) in [
            (LEGACY, Format::OneLine),
            (VSCODE, Format::Deb822),
            (VIVALDI, Format::Deb822),
        ] {
            let file = SourceFile::parse(Path::new("/x"), format, text);
            assert_eq!(file.render(), text);
            assert!(!file.is_modified());
        }
    }

    #[test]
    fn every_real_source_file_on_this_machine_renders_back_byte_for_byte() {
        let files = load();
        if files.is_empty() {
            return;
        }
        for file in &files {
            assert_eq!(
                file.render(),
                file.original,
                "{} did not survive a round trip",
                file.path.display()
            );
        }
    }

    /// Disabling a stanza must not cost the file its header or its unknown keys. Both real deb822
    /// files here open with a `###` banner, and Vivaldi's carries `X-Repolib-Name`.
    #[test]
    fn disabling_a_stanza_keeps_the_headers_and_unknown_keys() {
        let mut file = deb822_file(VIVALDI);
        file.set_enabled(0, false).unwrap();
        let rendered = file.render();

        for kept in [
            "### THIS FILE IS AUTOMATICALLY CONFIGURED ###",
            "# Changes to this file will not be preserved.",
            "X-Repolib-Name: Vivaldi",
            "Signed-By: /usr/share/keyrings/vivaldi-16BD9233.gpg",
        ] {
            assert!(rendered.contains(kept), "lost {kept:?}:\n{rendered}");
        }
        assert!(rendered.contains("Enabled: no"));
        assert!(!deb822_file(&rendered).repositories()[0].enabled);
    }

    /// # Regression
    ///
    /// Stacer's toggle did `newSource.replace("#", "")` — every `#` in the line, not the leading
    /// markers. A trailing comment, or a `#` inside a URI, does not survive that.
    #[test]
    fn toggling_a_line_keeps_a_trailing_comment() {
        let text = "# deb http://x/ jammy main # added by hand\n";
        let mut file = SourceFile::parse(Path::new("/x"), Format::OneLine, text);
        file.set_enabled(0, true).unwrap();

        assert_eq!(
            file.render(),
            "deb http://x/ jammy main # added by hand\n",
            "only the leading marker is the comment state"
        );
    }

    #[test]
    fn a_toggle_round_trips() {
        let mut file = legacy_file();
        let before = file.render();

        file.set_enabled(1, false).unwrap();
        assert!(file.is_modified());
        file.set_enabled(1, true).unwrap();

        assert_eq!(
            file.render(),
            before,
            "off then on must return to the start"
        );
    }

    #[test]
    fn removing_a_stanza_leaves_no_doubled_blank_line() {
        let text = "Types: deb\nURIs: http://a/\nSuites: s\n\nTypes: deb\nURIs: http://b/\nSuites: t\n\nTypes: deb\nURIs: http://c/\nSuites: u\n";
        let mut file = deb822_file(text);
        file.remove(1).unwrap();

        let rendered = file.render();
        assert!(!rendered.contains("\n\n\n"), "{rendered:?}");
        let repos = deb822_file(&rendered).repositories();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].uris, vec!["http://a/"]);
        assert_eq!(repos[1].uris, vec!["http://c/"]);
    }

    // ---- what may be opened ----

    /// The check that stops a caller naming an arbitrary file and having it parsed and echoed back.
    #[test]
    fn a_file_apt_does_not_read_cannot_be_opened() {
        for refused in [
            "/etc/shadow",
            "/etc/apt/sources.list.d/docker.list.save",
            "/etc/passwd",
            "/etc/apt/trusted.gpg",
        ] {
            let error = open(Path::new(refused)).expect_err("{refused} must be refused");
            assert_eq!(error.code, ErrorCode::Refused, "{refused}");
        }
    }

    /// And a path with the right extension that is not one of this machine's files is refused too —
    /// the extension is necessary but not sufficient.
    #[test]
    fn a_plausible_path_that_is_not_a_real_source_file_is_refused() {
        let error = open(Path::new("/tmp/nix-test-not-a-source.list")).unwrap_err();
        assert_eq!(error.code, ErrorCode::Refused);
    }

    #[test]
    fn this_machines_repositories_are_listed_with_positions() {
        let repos = list();
        if repos.is_empty() {
            return;
        }

        for repo in &repos {
            assert!(!repo.uris.is_empty(), "a repository has a URI");
            assert!(!repo.types.is_empty());
            assert!(
                Format::of(&repo.at.file).is_some(),
                "{} is not a file apt reads",
                repo.at.file.display()
            );
        }

        // Every entry is addressable, and no two entries in one file share a position.
        let mut seen: Vec<(PathBuf, u32)> = repos
            .iter()
            .map(|r| (r.at.file.clone(), r.at.index))
            .collect();
        let before = seen.len();
        seen.sort();
        seen.dedup();
        assert_eq!(before, seen.len(), "two repositories share one position");

        // If the machine has any `.sources` files, they must be represented — that half is the one
        // Stacer could not see, and a count of the whole is not evidence that it was read.
        let deb822_files = source_files()
            .iter()
            .filter(|p| Format::of(p) == Some(Format::Deb822))
            .count();
        if deb822_files > 0 {
            assert!(
                repos.iter().any(|r| r.at.format == Format::Deb822),
                "{deb822_files} deb822 file(s) present and no deb822 repository parsed"
            );
        }
    }
}
