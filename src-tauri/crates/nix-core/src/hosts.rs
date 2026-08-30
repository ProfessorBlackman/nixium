// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The hosts file, as a table you can edit without losing what is already in it. `SYS-1`.
//!
//! # Every line survives, byte for byte, unless you edit it
//!
//! `/etc/hosts` is a file people keep things in: comments explaining why an entry exists, blank lines
//! separating groups, entries commented out rather than deleted, and whatever the distribution or a
//! VPN client put there. Stacer's editor preserved comments, which is the one thing it got right here,
//! and this keeps that property and strengthens it.
//!
//! Every line carries its **original text**, and rendering emits that text verbatim for any line the
//! user has not touched. Only lines that were edited or added get canonically formatted. That is
//! stronger than "comments are preserved": it means tab-versus-space alignment, unusual spacing, and
//! anything this parser does not understand all come back out exactly as they went in. This machine's
//! own file uses both tab and space separators on different lines, which is precisely the kind of
//! detail a reformatting editor destroys.
//!
//! The property is testable, and is tested: `render(parse(text)) == text`, against the real file.
//!
//! # A commented-out entry is a disabled entry, not a comment
//!
//! `# 127.0.0.1 ads.example.com` is how people park an entry they might want back. Parsed as a
//! comment it is opaque text; parsed as a disabled entry it can be toggled, which is what the user
//! meant by writing it that way. Validation is what makes this safe to infer — the line only becomes
//! an entry if what follows the `#` starts with something that really parses as an IP address, so
//! `# The following lines are desirable for IPv6 capable hosts` stays a comment.
//!
//! # Writing is a compare-and-swap, and the comparison is the whole file
//!
//! `SYS-1`'s acceptance criterion is that a concurrent external edit is **detected and surfaced
//! rather than overwritten**. The client sends both the content it wants written and the exact bytes
//! it started from; the helper re-reads the file and refuses unless they match.
//!
//! Not a digest. A hosts file is a few hundred bytes — 233 on this machine — so comparing the whole
//! thing costs nothing and leaves no collision to reason about. A hash would have been the reflexive
//! choice and strictly worse.

use std::net::IpAddr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, ErrorCode, IoContext, Result};

/// Where the hosts file lives. **Fixed, never taken from a caller** — this is the only path this
/// module and its privileged counterpart will touch.
pub const HOSTS_PATH: &str = "/etc/hosts";

/// A ceiling on what will be written, so a bug upstream cannot fill the root filesystem.
///
/// A hosts file used as an ad blocker legitimately reaches a few hundred kilobytes; 4 MiB is far
/// beyond any real use and far below anything that matters to `/`.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;

/// The longest a DNS name may be, and the longest one label may be. RFC 1035 via RFC 1123.
const MAX_NAME: usize = 253;
const MAX_LABEL: usize = 63;

/// What a line in the file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LineKind {
    /// An address and the names it maps to. May be commented out — see [`HostLine::enabled`].
    Entry,
    /// A comment that is not a disabled entry.
    Comment,
    /// An empty or whitespace-only line.
    Blank,
    /// Something this parser does not understand.
    ///
    /// Kept and re-emitted untouched rather than dropped. A file nix cannot fully parse is still the
    /// user's file, and silently discarding a line would be the worst possible failure here.
    Unparsed,
}

/// One line of the hosts file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HostLine {
    /// Position in the file, from zero. Stable for as long as the document is not re-parsed, and what
    /// edits address — never the text of the line, which is the mistake that makes a UI act on the
    /// wrong row.
    pub id: u32,
    pub kind: LineKind,
    /// The line as it was read, without its newline. Emitted verbatim unless the line was edited.
    pub raw: String,
    /// The address, for an entry.
    pub ip: Option<String>,
    /// The names an entry maps to. The first is the canonical name; the rest are aliases.
    pub names: Vec<String>,
    /// A trailing `# …` comment on an entry, without the `#`.
    pub comment: Option<String>,
    /// Whether an entry is in effect. `false` for one that is commented out.
    ///
    /// Always `true` for the other kinds, which have no such notion.
    pub enabled: bool,
    /// Whether this line has been changed since it was read, and so must be re-rendered rather than
    /// emitted verbatim.
    pub edited: bool,
}

impl HostLine {
    fn blank(id: u32, raw: String) -> Self {
        Self {
            id,
            kind: LineKind::Blank,
            raw,
            ip: None,
            names: Vec::new(),
            comment: None,
            enabled: true,
            edited: false,
        }
    }

    /// The canonical text for an entry: address, tab, names, then any comment.
    ///
    /// Only used for lines the user actually changed. A tab because that is what the distribution's
    /// own first two lines use here, and it survives names of any length.
    fn canonical(&self) -> String {
        let mut out = String::new();
        if !self.enabled {
            out.push_str("# ");
        }
        if let Some(ip) = &self.ip {
            out.push_str(ip);
            out.push('\t');
        }
        out.push_str(&self.names.join(" "));
        if let Some(comment) = &self.comment {
            out.push_str("\t# ");
            out.push_str(comment);
        }
        out
    }

    /// What this line contributes to the file.
    #[must_use]
    pub fn text(&self) -> String {
        if self.edited && self.kind == LineKind::Entry {
            self.canonical()
        } else {
            self.raw.clone()
        }
    }
}

/// The whole file: its lines, and the bytes they came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HostsFile {
    pub lines: Vec<HostLine>,
    /// The file exactly as it was read.
    ///
    /// Carried so a write can be a compare-and-swap against it. Also what makes the round-trip test
    /// possible, which is the guarantee that nothing is lost.
    pub original: String,
}

impl HostsFile {
    /// Read and parse the hosts file.
    ///
    /// Unprivileged: `/etc/hosts` is world-readable (0644 here), so only writing needs the helper.
    pub fn load() -> Result<Self> {
        let text = std::fs::read_to_string(HOSTS_PATH)
            .doing("read the hosts file")
            .map_err(|e| e.with_path(HOSTS_PATH))?;
        Ok(parse(&text))
    }

    /// The entries, in file order, ignoring comments and blanks.
    pub fn entries(&self) -> impl Iterator<Item = &HostLine> {
        self.lines.iter().filter(|l| l.kind == LineKind::Entry)
    }

    fn line_mut(&mut self, id: u32) -> Result<&mut HostLine> {
        self.lines.iter_mut().find(|l| l.id == id).ok_or_else(|| {
            AppError::new(ErrorCode::NotFound, "That line is no longer in the file.")
        })
    }

    /// Change an entry's address, names and comment.
    ///
    /// Validates before mutating, so a rejected edit leaves the document exactly as it was — a
    /// half-applied edit would be a worse outcome than a refused one.
    pub fn set(
        &mut self,
        id: u32,
        ip: &str,
        names: &[String],
        comment: Option<&str>,
    ) -> Result<()> {
        let address = validate_ip(ip)?;
        validate_names(names)?;
        let comment = validate_comment(comment)?;

        let line = self.line_mut(id)?;
        if line.kind != LineKind::Entry {
            return Err(AppError::invalid_input(
                "That line is not a host entry, so it has no address to change.",
            ));
        }
        line.ip = Some(address);
        line.names = names.to_vec();
        line.comment = comment;
        line.edited = true;
        Ok(())
    }

    /// Add an entry at the end of the file.
    pub fn add(&mut self, ip: &str, names: &[String], comment: Option<&str>) -> Result<u32> {
        let address = validate_ip(ip)?;
        validate_names(names)?;
        let comment = validate_comment(comment)?;

        let id = self.lines.iter().map(|l| l.id).max().map_or(0, |m| m + 1);
        let mut line = HostLine::blank(id, String::new());
        line.kind = LineKind::Entry;
        line.ip = Some(address);
        line.names = names.to_vec();
        line.comment = comment;
        line.edited = true;
        self.lines.push(line);
        Ok(id)
    }

    /// Remove a line.
    ///
    /// **Actually removes it.** Stacer's delete rewrote the file from its table model, which dropped
    /// whatever the table did not represent; commenting the line out instead would be a different
    /// operation, and [`HostsFile::set_enabled`] is that operation.
    pub fn remove(&mut self, id: u32) -> Result<()> {
        let before = self.lines.len();
        self.lines.retain(|l| l.id != id);
        if self.lines.len() == before {
            return Err(AppError::new(
                ErrorCode::NotFound,
                "That line is no longer in the file.",
            ));
        }
        Ok(())
    }

    /// Comment an entry out, or bring it back.
    pub fn set_enabled(&mut self, id: u32, enabled: bool) -> Result<()> {
        let line = self.line_mut(id)?;
        if line.kind != LineKind::Entry {
            return Err(AppError::invalid_input(
                "Only a host entry can be turned on or off.",
            ));
        }
        if line.enabled != enabled {
            line.enabled = enabled;
            line.edited = true;
        }
        Ok(())
    }

    /// The file this document would be written as.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(self.original.len() + 64);
        for line in &self.lines {
            out.push_str(&line.text());
            out.push('\n');
        }
        out
    }

    /// Whether rendering would change the file.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.render() != self.original
    }
}

/// Parse a hosts file.
///
/// Total: every line becomes exactly one [`HostLine`], and no input is rejected. A parse that could
/// fail would mean a file nix refuses to show, which is not a useful behaviour for a file the user
/// may need to fix.
#[must_use]
pub fn parse(text: &str) -> HostsFile {
    let mut lines = Vec::new();

    // `split('\n')` rather than `lines()`: `lines()` cannot distinguish a file ending in a newline
    // from one that does not, and every real hosts file ends in one. Splitting leaves a trailing
    // empty piece for the final newline, which is dropped here and re-added by `render`.
    let mut pieces: Vec<&str> = text.split('\n').collect();
    if pieces.last().is_some_and(|last| last.is_empty()) {
        pieces.pop();
    }

    for (index, raw) in pieces.iter().enumerate() {
        let id = u32::try_from(index).unwrap_or(u32::MAX);
        lines.push(parse_line(id, raw));
    }

    HostsFile {
        lines,
        original: text.to_string(),
    }
}

fn parse_line(id: u32, raw: &str) -> HostLine {
    let mut line = HostLine::blank(id, raw.to_string());

    if raw.trim().is_empty() {
        return line;
    }

    let trimmed = raw.trim_start();
    let (body, enabled) = match trimmed.strip_prefix('#') {
        // A commented-out entry, if what follows really is one.
        Some(rest) => (rest.trim_start(), false),
        None => (trimmed, true),
    };

    // Split off a trailing comment first, so it does not end up parsed as a hostname.
    let (fields, comment) = match body.split_once('#') {
        Some((before, after)) => (before, Some(after.trim().to_string())),
        None => (body, None),
    };

    let mut tokens = fields.split_whitespace();
    let Some(address) = tokens.next() else {
        line.kind = if enabled {
            LineKind::Unparsed
        } else {
            LineKind::Comment
        };
        return line;
    };

    // The address is what decides. This is what keeps prose comments from being read as entries.
    if IpAddr::from_str(address).is_err() {
        line.kind = if enabled {
            LineKind::Unparsed
        } else {
            LineKind::Comment
        };
        return line;
    }

    let names: Vec<String> = tokens.map(str::to_string).collect();
    if names.is_empty() {
        // An address with no names is not a usable entry, and is not something to silently normalise.
        line.kind = if enabled {
            LineKind::Unparsed
        } else {
            LineKind::Comment
        };
        return line;
    }

    line.kind = LineKind::Entry;
    line.ip = Some(address.to_string());
    line.names = names;
    line.comment = comment;
    line.enabled = enabled;
    line
}

/// Check an address, returning it in the form the parser will read back.
///
/// `IpAddr` rather than a regular expression: it accepts exactly what the resolver accepts, including
/// every IPv6 abbreviation, and rejects `1.2.3.4.5` and `::g` without anyone having to think about
/// it. The normalised form is returned so `::1` and `0:0:0:0:0:0:0:1` do not both end up in the file
/// looking like different addresses.
pub fn validate_ip(ip: &str) -> Result<String> {
    let trimmed = ip.trim();
    if trimmed.is_empty() {
        return Err(AppError::invalid_input("An entry needs an IP address."));
    }
    IpAddr::from_str(trimmed)
        .map(|parsed| parsed.to_string())
        .map_err(|_| {
            AppError::invalid_input(format!("{trimmed} is not a valid IP address."))
                .with_remedy("Enter an IPv4 address like 127.0.0.1, or IPv6 like ::1.")
        })
}

/// Check the names of an entry.
pub fn validate_names(names: &[String]) -> Result<()> {
    if names.is_empty() {
        return Err(AppError::invalid_input(
            "An entry needs at least one hostname.",
        ));
    }
    for name in names {
        validate_hostname(name)?;
    }
    Ok(())
}

/// Check one hostname, by RFC 1123's rules.
pub fn validate_hostname(name: &str) -> Result<()> {
    let bad = |why: &str| {
        Err(
            AppError::invalid_input(format!("{name:?} {why}.")).with_remedy(
                "A hostname is letters, digits and hyphens, in dot-separated labels — like \
             db.internal.example.",
            ),
        )
    };

    if name.is_empty() {
        return bad("is empty");
    }
    if name.len() > MAX_NAME {
        return bad("is longer than a DNS name may be");
    }
    // A whitespace character would split into two names on the way back in, silently turning one
    // entry into a different one.
    if name.split_whitespace().count() != 1 {
        return bad("contains a space");
    }
    if name.contains('#') {
        return bad("contains a '#', which would comment out the rest of the line");
    }

    for label in name.split('.') {
        if label.is_empty() {
            return bad("has an empty label — check for a doubled or trailing dot");
        }
        if label.len() > MAX_LABEL {
            return bad("has a label longer than 63 characters");
        }
        if label.starts_with('-') || label.ends_with('-') {
            return bad("has a label starting or ending with a hyphen");
        }
        if !label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return bad("has a label with characters that are not letters, digits or hyphens");
        }
    }
    Ok(())
}

/// Check a trailing comment.
///
/// The only real rule is that it cannot contain a newline, which would turn one line into two.
fn validate_comment(comment: Option<&str>) -> Result<Option<String>> {
    match comment.map(str::trim) {
        None => Ok(None),
        Some("") => Ok(None),
        Some(text) if text.contains('\n') || text.contains('\r') => Err(AppError::invalid_input(
            "A comment cannot contain a line break.",
        )),
        Some(text) => Ok(Some(text.to_string())),
    }
}

/// Check a rendered file before it is handed to the helper.
///
/// Applied on both sides of the privilege boundary. Here it gives the user an error they can act on;
/// in the helper it is the check that stops this operation being a way to write arbitrary content to
/// `/etc/hosts`, which is why it is a shared function and not a convenience.
pub fn validate_document(text: &str) -> Result<()> {
    if text.len() > MAX_BYTES {
        return Err(AppError::invalid_input(format!(
            "A hosts file of {} is beyond anything nix will write.",
            crate::format_bytes(text.len() as u64)
        )));
    }
    if text.contains('\0') {
        return Err(AppError::invalid_input(
            "A hosts file cannot contain a null byte.",
        ));
    }
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(AppError::invalid_input(
            "A hosts file must end with a newline.",
        ));
    }

    // Every line must be something this format allows. `Unparsed` is allowed on the way *in*, because
    // the user's existing file is not ours to reject — but it may not be introduced on the way out,
    // or this operation becomes a general-purpose privileged write.
    for (number, raw) in text.lines().enumerate() {
        let line = parse_line(0, raw);
        if line.kind == LineKind::Unparsed {
            return Err(AppError::invalid_input(format!(
                "Line {} is not a valid hosts line: {raw:?}",
                number + 1
            )));
        }
        if line.kind == LineKind::Entry {
            validate_ip(line.ip.as_deref().unwrap_or_default())?;
            validate_names(&line.names)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// This machine's real `/etc/hosts`, captured. Tabs on the first two lines and spaces on the
    /// rest, which is exactly the detail a reformatting editor loses.
    const REAL: &str = "\
127.0.0.1\tlocalhost
127.0.1.1\tAMALITECH-PC-10423

# The following lines are desirable for IPv6 capable hosts
::1     ip6-localhost ip6-loopback
fe00::0 ip6-localnet
ff00::0 ip6-mcastprefix
ff02::1 ip6-allnodes
ff02::2 ip6-allrouters
";

    // ---- the guarantee: nothing is lost ----

    /// The property the whole design rests on. If this holds, no untouched line can be altered, so
    /// spacing, alignment and anything the parser does not understand all survive.
    #[test]
    fn an_untouched_file_renders_back_byte_for_byte() {
        let doc = parse(REAL);
        assert_eq!(doc.render(), REAL);
        assert!(!doc.is_modified());
    }

    /// And on the live file, not only the captured copy — the fixture is what I expected the file to
    /// look like, which is not the same thing.
    #[test]
    fn this_machines_real_hosts_file_renders_back_byte_for_byte() {
        let Ok(doc) = HostsFile::load() else {
            return; // no /etc/hosts, or unreadable
        };
        assert_eq!(
            doc.render(),
            doc.original,
            "the real file did not survive a round trip"
        );
        assert!(!doc.is_modified());
        assert!(doc.entries().count() >= 2, "a machine has loopback entries");
    }

    /// A file this parser does not fully understand is still the user's file.
    #[test]
    fn a_line_that_cannot_be_parsed_is_kept_verbatim() {
        let odd = "127.0.0.1 localhost\nthis is not a hosts line at all\n@@@ nor is this\n";
        let doc = parse(odd);

        assert_eq!(doc.lines[1].kind, LineKind::Unparsed);
        assert_eq!(doc.lines[2].kind, LineKind::Unparsed);
        assert_eq!(doc.render(), odd, "an unparsed line must not be dropped");
    }

    #[test]
    fn a_file_with_no_trailing_newline_gains_one_rather_than_losing_a_line() {
        let doc = parse("127.0.0.1 localhost");
        assert_eq!(doc.lines.len(), 1);
        assert_eq!(doc.render(), "127.0.0.1 localhost\n");
    }

    #[test]
    fn an_empty_file_is_not_a_parse_failure() {
        let doc = parse("");
        assert!(doc.lines.is_empty());
        assert_eq!(doc.render(), "");
    }

    // ---- parsing ----

    #[test]
    fn an_entry_yields_its_address_and_every_name() {
        let doc = parse("::1     ip6-localhost ip6-loopback\n");
        let entry = &doc.lines[0];

        assert_eq!(entry.kind, LineKind::Entry);
        assert_eq!(entry.ip.as_deref(), Some("::1"));
        assert_eq!(entry.names, vec!["ip6-localhost", "ip6-loopback"]);
        assert!(entry.enabled);
    }

    /// Prose after a `#` must stay prose. The distribution's own file has exactly this line, and
    /// reading it as an entry would put "The" in a hostname column.
    #[test]
    fn a_prose_comment_is_not_read_as_a_disabled_entry() {
        let doc = parse("# The following lines are desirable for IPv6 capable hosts\n");
        assert_eq!(doc.lines[0].kind, LineKind::Comment);
        assert!(doc.lines[0].ip.is_none());
    }

    /// And a commented-out entry is what the user meant by writing it that way.
    #[test]
    fn a_commented_out_entry_is_a_disabled_entry() {
        let doc = parse("# 127.0.0.1 ads.example.com\n");
        let line = &doc.lines[0];

        assert_eq!(line.kind, LineKind::Entry);
        assert!(!line.enabled);
        assert_eq!(line.ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(line.names, vec!["ads.example.com"]);
    }

    #[test]
    fn a_trailing_comment_is_kept_apart_from_the_names() {
        let doc = parse("10.0.0.5\tdb.internal  # the staging box\n");
        let line = &doc.lines[0];

        assert_eq!(line.names, vec!["db.internal"]);
        assert_eq!(
            line.comment.as_deref(),
            Some("the staging box"),
            "a trailing comment must not become a hostname"
        );
    }

    #[test]
    fn an_address_with_no_names_is_not_a_usable_entry() {
        assert_eq!(parse("127.0.0.1\n").lines[0].kind, LineKind::Unparsed);
    }

    #[test]
    fn blank_and_whitespace_lines_are_blank() {
        let doc = parse("\n   \n\t\n");
        assert!(doc.lines.iter().all(|l| l.kind == LineKind::Blank));
        assert_eq!(
            doc.render(),
            "\n   \n\t\n",
            "even their whitespace survives"
        );
    }

    // ---- editing ----

    #[test]
    fn editing_a_line_rewrites_only_that_line() {
        let mut doc = parse(REAL);
        doc.set(0, "127.0.0.2", &["localhost".into()], None)
            .unwrap();
        let rendered = doc.render();

        assert!(rendered.starts_with("127.0.0.2\tlocalhost\n"));
        assert!(
            rendered.contains("# The following lines are desirable"),
            "the comment survived"
        );
        assert!(
            rendered.contains("::1     ip6-localhost ip6-loopback"),
            "an untouched line keeps its own spacing: {rendered:?}"
        );
        assert!(doc.is_modified());
    }

    #[test]
    fn adding_an_entry_appends_it() {
        let mut doc = parse(REAL);
        let id = doc
            .add(
                "192.168.1.50",
                &["printer.local".into()],
                Some("the printer"),
            )
            .unwrap();

        assert!(
            doc.render()
                .ends_with("192.168.1.50\tprinter.local\t# the printer\n")
        );
        // And it parses back to what was asked for.
        let reparsed = parse(&doc.render());
        let added = reparsed.entries().last().unwrap();
        assert_eq!(added.names, vec!["printer.local"]);
        assert_eq!(added.comment.as_deref(), Some("the printer"));
        assert!(doc.lines.iter().any(|l| l.id == id));
    }

    /// Delete removes the line. Stacer's rewrote the file from its table model, which dropped
    /// whatever the table did not represent.
    #[test]
    fn removing_a_line_removes_it_and_leaves_the_rest_alone() {
        let mut doc = parse(REAL);
        let before = doc.lines.len();
        doc.remove(1).unwrap();

        assert_eq!(doc.lines.len(), before - 1);
        let rendered = doc.render();
        assert!(!rendered.contains("AMALITECH-PC-10423"));
        assert!(rendered.contains("127.0.0.1\tlocalhost"));
        assert!(rendered.contains("# The following lines are desirable"));
    }

    #[test]
    fn disabling_an_entry_comments_it_out_and_enabling_brings_it_back() {
        let mut doc = parse("10.0.0.5\tdb.internal\n");
        doc.set_enabled(0, false).unwrap();
        assert_eq!(doc.render(), "# 10.0.0.5\tdb.internal\n");

        // And back again, through a full round trip rather than by undoing in memory.
        let mut reparsed = parse(&doc.render());
        assert!(!reparsed.entries().next().unwrap().enabled);
        reparsed.set_enabled(0, true).unwrap();
        assert_eq!(reparsed.render(), "10.0.0.5\tdb.internal\n");
    }

    #[test]
    fn a_rejected_edit_leaves_the_document_untouched() {
        let mut doc = parse(REAL);
        let before = doc.render();

        assert!(
            doc.set(0, "not-an-ip", &["localhost".into()], None)
                .is_err()
        );
        assert!(doc.set(0, "127.0.0.1", &["bad host".into()], None).is_err());

        assert_eq!(
            doc.render(),
            before,
            "a refused edit must not half-apply — an address without its names would be worse than \
             no change"
        );
    }

    #[test]
    fn editing_a_comment_line_is_refused_rather_than_turning_it_into_an_entry() {
        let mut doc = parse(REAL);
        // Line 3 is the IPv6 prose comment.
        assert_eq!(doc.lines[3].kind, LineKind::Comment);
        assert!(doc.set(3, "127.0.0.1", &["x".into()], None).is_err());
    }

    #[test]
    fn editing_a_line_that_is_gone_reports_it_rather_than_panicking() {
        let mut doc = parse(REAL);
        assert_eq!(
            doc.set(9999, "127.0.0.1", &["x".into()], None)
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert_eq!(doc.remove(9999).unwrap_err().code, ErrorCode::NotFound);
    }

    // ---- validation ----

    #[test]
    fn addresses_are_accepted_exactly_as_the_resolver_would() {
        for good in [
            "127.0.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "fe00::0",
            "2001:db8::8a2e:370:7334",
        ] {
            assert!(validate_ip(good).is_ok(), "{good} is a valid address");
        }
        for bad in [
            "",
            "  ",
            "1.2.3",
            "1.2.3.4.5",
            "256.1.1.1",
            "::g",
            "localhost",
            "127.0.0.1/8",
            "127.0.0.1 ",
        ] {
            // The last two matter: a CIDR suffix and a trailing space both look close enough to pass
            // a hand-written check.
            if bad == "127.0.0.1 " {
                assert!(validate_ip(bad).is_ok(), "surrounding space is trimmed");
                continue;
            }
            assert!(validate_ip(bad).is_err(), "{bad:?} is not a valid address");
        }
    }

    /// An abbreviated address is normalised, so the file does not end up with two spellings of one
    /// address looking like two different entries.
    #[test]
    fn an_address_is_normalised_to_one_spelling() {
        assert_eq!(validate_ip("0:0:0:0:0:0:0:1").unwrap(), "::1");
        assert_eq!(validate_ip("::0001").unwrap(), "::1");
    }

    #[test]
    fn hostnames_follow_rfc_1123() {
        for good in [
            "localhost",
            "db.internal",
            "a",
            "my-box",
            "x1.y2.z3",
            "with_underscore",
        ] {
            assert!(validate_hostname(good).is_ok(), "{good} is a valid name");
        }
        for bad in [
            "",
            "has space",
            "-leading",
            "trailing-",
            "double..dot",
            ".leading",
            "trailing.",
            "has#hash",
        ] {
            assert!(
                validate_hostname(bad).is_err(),
                "{bad:?} is not a valid name"
            );
        }
        assert!(validate_hostname(&"a".repeat(MAX_LABEL)).is_ok());
        assert!(validate_hostname(&"a".repeat(MAX_LABEL + 1)).is_err());
    }

    /// A name containing whitespace would split into two names on the way back in, silently changing
    /// the entry into a different one.
    #[test]
    fn a_name_with_a_space_is_refused_because_it_would_reparse_as_two() {
        assert!(validate_names(&["one two".to_string()]).is_err());
    }

    /// A `#` in a name would comment out the rest of the line, which is a silent way to disable
    /// entries the user did not touch.
    #[test]
    fn a_name_with_a_hash_is_refused_because_it_would_comment_the_line_out() {
        assert!(validate_names(&["evil#".to_string()]).is_err());
    }

    #[test]
    fn a_comment_cannot_smuggle_in_a_second_line() {
        let mut doc = parse("10.0.0.5\tdb\n");
        assert!(
            doc.set(
                0,
                "10.0.0.5",
                &["db".into()],
                Some("one\n0.0.0.0 evil.example")
            )
            .is_err(),
            "a newline in a comment would append an entry nobody approved"
        );
    }

    // ---- what the helper checks before writing ----

    #[test]
    fn a_valid_document_passes_the_write_check() {
        validate_document(REAL).unwrap();
        validate_document("").unwrap();
        validate_document("# just a comment\n").unwrap();
    }

    /// The check that stops this being a general-purpose privileged write. An unparsed line is
    /// tolerated on the way *in*, because the user's existing file is not ours to reject — but it
    /// must never be introduced on the way out.
    #[test]
    fn a_document_containing_anything_that_is_not_a_hosts_line_is_refused() {
        for bad in [
            "#!/bin/sh\nrm -rf /\n",
            "127.0.0.1 localhost\nnonsense here\n",
            "127.0.0.1\n",
        ] {
            assert!(
                validate_document(bad).is_err(),
                "{bad:?} must not be writable to /etc/hosts"
            );
        }
    }

    #[test]
    fn a_document_missing_its_final_newline_is_refused() {
        assert!(validate_document("127.0.0.1 localhost").is_err());
    }

    #[test]
    fn a_document_with_a_null_byte_is_refused() {
        assert!(validate_document("127.0.0.1 localhost\0\n").is_err());
    }

    #[test]
    fn an_absurdly_large_document_is_refused() {
        let huge = format!("{}\n", "# padding".repeat(MAX_BYTES / 8));
        assert!(huge.len() > MAX_BYTES);
        assert!(validate_document(&huge).is_err());
    }

    /// A hosts file used as an ad blocker is legitimately large, and must still be writable.
    #[test]
    fn a_large_but_realistic_blocklist_is_accepted() {
        let mut text = String::new();
        for i in 0..20_000 {
            text.push_str(&format!("0.0.0.0\tads-{i}.example.com\n"));
        }
        assert!(text.len() > 400_000);
        validate_document(&text).unwrap();
    }

    /// Anything this module will write must be something it can read back. Checked over the whole
    /// edit surface rather than one case, since a canonical form that does not reparse is a defect
    /// that only shows up on the *next* load.
    #[test]
    fn everything_this_module_writes_can_be_read_back_unchanged() {
        let mut doc = parse(REAL);
        doc.add("10.0.0.5", &["db.internal".into()], Some("staging"))
            .unwrap();
        doc.add("::1", &["alias.one".into(), "alias.two".into()], None)
            .unwrap();
        doc.set(0, "127.0.0.9", &["localhost".into()], Some("edited"))
            .unwrap();
        doc.set_enabled(1, false).unwrap();

        let rendered = doc.render();
        validate_document(&rendered).unwrap();

        // And the second render of the reparsed document equals the first.
        let reparsed = parse(&rendered);
        assert_eq!(
            reparsed.render(),
            rendered,
            "a canonical form that does not survive reparsing breaks the next edit"
        );
    }
}
