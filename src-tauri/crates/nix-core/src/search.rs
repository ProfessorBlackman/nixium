// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Finding files. `SYS-2`.
//!
//! # Every filter maps to real behaviour
//!
//! That is `SYS-2`'s acceptance criterion, and it is there because Stacer's did not. Its search built
//! a `find` command line, and one of the checkboxes appended a flag that does not exist:
//!
//! ```cpp
//! if (ui->checkInvert->isChecked()) {
//!     findQuery.append("-invert");
//! }
//! ```
//!
//! `find` has no `-invert` predicate. It exits with a usage error, so ticking that box did not invert
//! anything — it made the search return **nothing at all**, silently, with no indication that the
//! query had been rejected rather than simply matching no files.
//!
//! Here the filters are code. There is no command line to get wrong, and inversion is a boolean on the
//! query that is applied to the predicate result.
//!
//! # No row cap
//!
//! Stacer showed `foundFiles.mid(1, 2000)` — the first 2,000 results, and its label said so. A search
//! that silently stops at a round number is worse than one that takes longer, because the answer to
//! "is my duplicate collection in here" becomes unknowable. Results stream in batches and the caller
//! decides when to stop.
//!
//! # Unprivileged, always
//!
//! Stacer ran `find` through `sudoExec` when the search root was outside `$HOME`, so looking for a file
//! meant a password prompt and a root-privileged traversal. Searching is a read: this walks as the user
//! and reports what it could not enter, which is information rather than a reason to escalate.
//!
//! # Regex is behind a feature, for the same reason `zbus` is
//!
//! `regex` is a real dependency, and `nix-helper` links `nix-core`. Adding it unconditionally would put
//! a regex engine in the binary that runs as root, for a feature the helper has no use for. So it sits
//! behind the `regex` feature that only `nix-app` enables — the arrangement `SPEC.md` §D10 already
//! established — and a regex query without it returns `Unsupported` rather than silently matching
//! nothing. Substring and glob matching are hand-written and always available.

use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, Result};
// Only the feature-off branch builds an `Unsupported` error, so an unconditional import warns in the
// build that has the feature on.
#[cfg(not(feature = "regex"))]
use crate::error::ErrorCode;
use crate::op::CancelToken;

/// How a name pattern is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NameMatch {
    /// Anywhere in the file name, case-insensitively. What people expect when they type a word.
    #[default]
    Contains,
    /// `*`, `?` and `[abc]`, against the whole file name.
    Glob,
    /// A full regular expression. Needs the `regex` feature.
    Regex,
}

/// What kind of thing to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
}

/// One search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Query {
    pub root: PathBuf,
    /// The name pattern. An empty pattern matches everything, so the other filters can stand alone.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub match_kind: NameMatch,
    /// Match on the whole path rather than the file name.
    #[serde(default)]
    pub whole_path: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    #[ts(type = "number | null")]
    pub min_bytes: Option<u64>,
    #[ts(type = "number | null")]
    pub max_bytes: Option<u64>,
    /// Modified at or after this many seconds since the epoch.
    #[ts(type = "number | null")]
    pub modified_after: Option<u64>,
    #[ts(type = "number | null")]
    pub modified_before: Option<u64>,
    pub kind: Option<FileKind>,
    /// Owner, by numeric uid.
    pub owner_uid: Option<u32>,
    /// Permission bits that must all be set — `0o111` for "executable by someone".
    #[ts(type = "number | null")]
    pub mode_all_of: Option<u32>,
    /// Empty files and directories only.
    ///
    /// "Empty" means no bytes for a file and **no entries** for a directory — different questions, and
    /// a directory's `st_size` answers neither.
    #[serde(default)]
    pub empty_only: bool,
    /// **Invert the match**, and actually invert it.
    #[serde(default)]
    pub invert: bool,
    #[serde(default)]
    pub cross_filesystems: bool,
    /// Stop after this many matches. `None` means no cap, which is the default.
    #[ts(type = "number | null")]
    pub limit: Option<u64>,
}

impl Query {
    /// A query matching everything under a root, for tests and for filter-only searches.
    #[must_use]
    pub fn under(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            name: String::new(),
            match_kind: NameMatch::Contains,
            whole_path: false,
            case_sensitive: false,
            min_bytes: None,
            max_bytes: None,
            modified_after: None,
            modified_before: None,
            kind: None,
            owner_uid: None,
            mode_all_of: None,
            empty_only: false,
            invert: false,
            cross_filesystems: false,
            limit: None,
        }
    }
}

/// One result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Hit {
    pub path: PathBuf,
    /// Size in bytes, and **zero for a directory**.
    ///
    /// A directory's own `st_size` is a filesystem artefact — 4,096 on ext4 whether it holds two
    /// entries or two hundred — and reporting it as a size means a `min_bytes` filter of 1 KB matches
    /// every directory on the machine. This project has already shipped that mistake once, reporting a
    /// 9.8 GiB cache as 4 KB by taking a directory's inode size as its contents
    /// (`docs/issues/03-reclaim-pipeline.md`), so directories are counted here and never measured —
    /// the same rule as [`crate::pkg::measure_paths`].
    #[ts(type = "number")]
    pub bytes: u64,
    pub kind: FileKind,
    /// Modified time, seconds since the epoch. `None` when the filesystem does not report one.
    #[ts(type = "number | null")]
    pub modified: Option<u64>,
    pub uid: u32,
    /// Permission bits, for display as an octal mode.
    pub mode: u32,
}

/// How a search ended.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Summary {
    #[ts(type = "number")]
    pub examined: u64,
    #[ts(type = "number")]
    pub matched: u64,
    /// Directories that could not be entered.
    ///
    /// Reported rather than ignored: a search of `/` as an ordinary user cannot read everything, and a
    /// result set that is quietly partial is worse than one that says so. Stacer's answer to this was
    /// to re-run `find` under `sudo`.
    #[ts(type = "number")]
    pub unreadable: u64,
    /// Whether the cap was reached. `false` unless a limit was asked for.
    pub truncated: bool,
    pub cancelled: bool,
}

/// A compiled name pattern.
enum Matcher {
    Everything,
    Contains(String),
    Glob(String),
    #[cfg(feature = "regex")]
    Regex(regex::Regex),
}

impl Matcher {
    fn compile(query: &Query) -> Result<Self> {
        if query.name.is_empty() {
            return Ok(Self::Everything);
        }
        let pattern = if query.case_sensitive {
            query.name.clone()
        } else {
            query.name.to_lowercase()
        };

        match query.match_kind {
            NameMatch::Contains => Ok(Self::Contains(pattern)),
            NameMatch::Glob => Ok(Self::Glob(pattern)),
            NameMatch::Regex => {
                #[cfg(feature = "regex")]
                {
                    let built = regex::RegexBuilder::new(&query.name)
                        .case_insensitive(!query.case_sensitive)
                        // A pathological pattern must not hang the search. The size limit bounds the
                        // compiled program rather than the input, which is what actually runs away.
                        .size_limit(1 << 20)
                        .build()
                        .map_err(|e| {
                            AppError::invalid_input(format!("That is not a valid expression: {e}"))
                                .with_remedy("Check the pattern, or search by glob instead.")
                        })?;
                    Ok(Self::Regex(built))
                }
                #[cfg(not(feature = "regex"))]
                {
                    Err(AppError::new(
                        ErrorCode::Unsupported,
                        "This build cannot search by regular expression.",
                    )
                    .with_remedy("Use a glob pattern such as *.log instead."))
                }
            }
        }
    }

    /// `subject` is the name as typed; `lowered` is it case-folded. Both are passed because the
    /// regex engine does its own case handling and must see the original.
    #[cfg_attr(
        not(feature = "regex"),
        expect(unused_variables, reason = "only the regex arm reads it")
    )]
    fn matches(&self, subject: &str, lowered: &str) -> bool {
        match self {
            Self::Everything => true,
            Self::Contains(needle) => lowered.contains(needle.as_str()),
            Self::Glob(pattern) => glob_match(pattern, lowered),
            #[cfg(feature = "regex")]
            Self::Regex(re) => re.is_match(subject),
        }
    }
}

/// Match a glob against a whole string: `*` any run, `?` one character, `[abc]` and `[a-z]` a set.
///
/// Hand-written rather than pulled from a crate, because it is thirty lines and because the
/// alternative would put another dependency into the crate the privileged helper links. Iterative
/// with backtracking on the last `*`, so a pattern of many stars cannot become exponential — which a
/// naive recursive matcher does, and which is a real hazard for a pattern typed by a user.
#[must_use]
pub fn glob_match(pattern: &str, subject: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = subject.chars().collect();

    let (mut pi, mut si) = (0usize, 0usize);
    // Where to resume if the current attempt fails: the `*` in the pattern, and the input position
    // after it that has not been tried yet.
    let mut star: Option<(usize, usize)> = None;

    while si < s.len() {
        let matched = if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some((pi, si));
                    pi += 1;
                    continue;
                }
                '?' => true,
                '[' => match class_match(&p, pi, s[si]) {
                    Some((ok, next)) => {
                        if ok {
                            pi = next;
                            si += 1;
                            continue;
                        }
                        false
                    }
                    // An unterminated `[` is a literal bracket rather than an error.
                    None => p[pi] == s[si],
                },
                literal => literal == s[si],
            }
        } else {
            false
        };

        if matched {
            pi += 1;
            si += 1;
        } else if let Some((sp, ss)) = star {
            // Give the star one more character and try again.
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }

    // Trailing stars may absorb nothing.
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Match a `[...]` class at `pi`. Returns whether it matched and where the class ends.
fn class_match(pattern: &[char], pi: usize, c: char) -> Option<(bool, usize)> {
    let close = pattern.iter().skip(pi + 1).position(|&x| x == ']')? + pi + 1;
    let inner = &pattern[pi + 1..close];
    let (negated, inner) = match inner.first() {
        Some('!' | '^') => (true, &inner[1..]),
        _ => (false, inner),
    };

    let mut hit = false;
    let mut i = 0;
    while i < inner.len() {
        // A range, unless the `-` is the last character in the class.
        if i + 2 < inner.len() && inner[i + 1] == '-' {
            if c >= inner[i] && c <= inner[i + 2] {
                hit = true;
            }
            i += 3;
        } else {
            if inner[i] == c {
                hit = true;
            }
            i += 1;
        }
    }
    Some((hit != negated, close + 1))
}

/// The predicates other than the name, so the inversion has one place to apply.
fn passes_filters(query: &Query, hit: &Hit, empty: bool) -> bool {
    if let Some(kind) = query.kind
        && kind != hit.kind
    {
        return false;
    }
    if let Some(min) = query.min_bytes
        && hit.bytes < min
    {
        return false;
    }
    if let Some(max) = query.max_bytes
        && hit.bytes > max
    {
        return false;
    }
    if query.empty_only && !empty {
        return false;
    }
    if let Some(uid) = query.owner_uid
        && hit.uid != uid
    {
        return false;
    }
    if let Some(bits) = query.mode_all_of
        && hit.mode & bits != bits
    {
        return false;
    }
    match (query.modified_after, hit.modified) {
        (Some(after), Some(modified)) if modified < after => return false,
        // A filter on time cannot be satisfied by a file with no time. Excluding it is the honest
        // reading; including it would put files in the results that were not asked for.
        (Some(_), None) => return false,
        _ => {}
    }
    match (query.modified_before, hit.modified) {
        (Some(before), Some(modified)) if modified > before => return false,
        (Some(_), None) => return false,
        _ => {}
    }
    true
}

/// Run a search, handing batches of results to `emit` as they are found.
///
/// Streaming by callback rather than by returning a `Vec`: a search of a home directory can match
/// hundreds of thousands of files, and the caller wants the first screen immediately. `emit` returning
/// `false` stops the walk, which is how a caller with its own cap imposes one.
pub fn run(
    query: &Query,
    token: &CancelToken,
    mut emit: impl FnMut(Vec<Hit>) -> bool,
) -> Result<Summary> {
    use std::os::unix::fs::MetadataExt;

    /// Results per callback. Large enough that the channel is not the bottleneck, small enough that
    /// the first batch appears immediately.
    const BATCH: usize = 256;

    let matcher = Matcher::compile(query)?;
    let mut summary = Summary::default();

    let root_device = std::fs::symlink_metadata(&query.root)
        .map(|m| m.dev())
        .map_err(|e| AppError::from_io(&e, "read the folder to search").with_path(&query.root))?;

    let mut batch: Vec<Hit> = Vec::with_capacity(BATCH);
    let mut stack = vec![query.root.clone()];
    let mut stopped = false;

    while let Some(dir) = stack.pop() {
        if token.is_cancelled() {
            summary.cancelled = true;
            break;
        }

        let Ok(reading) = std::fs::read_dir(&dir) else {
            summary.unreadable = summary.unreadable.saturating_add(1);
            continue;
        };

        for item in reading {
            let Ok(item) = item else {
                summary.unreadable = summary.unreadable.saturating_add(1);
                continue;
            };
            let path = item.path();

            // `symlink_metadata`, so a link is judged as a link and a loop cannot be followed.
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                summary.unreadable = summary.unreadable.saturating_add(1);
                continue;
            };
            summary.examined = summary.examined.saturating_add(1);

            let file_type = meta.file_type();
            let kind = if file_type.is_dir() {
                FileKind::Directory
            } else if file_type.is_symlink() {
                FileKind::Symlink
            } else {
                FileKind::File
            };

            if kind == FileKind::Directory && (query.cross_filesystems || meta.dev() == root_device)
            {
                stack.push(path.clone());
            }

            let subject = if query.whole_path {
                path.to_string_lossy().into_owned()
            } else {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            };
            let lowered = if query.case_sensitive {
                subject.clone()
            } else {
                subject.to_lowercase()
            };

            let hit = Hit {
                // Zero for a directory: see `Hit::bytes`.
                bytes: if kind == FileKind::Directory {
                    0
                } else {
                    meta.len()
                },
                kind,
                modified: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs()),
                uid: meta.uid(),
                mode: meta.mode() & 0o7777,
                path,
            };

            // One place where every predicate is combined, and one place where inversion applies —
            // so "invert" cannot be a flag that means nothing.
            // "Empty" means something different for a directory than for a file: no entries, rather
            // than no bytes. Only computed when asked for, since it costs a `read_dir` per directory.
            let empty = if query.empty_only {
                match kind {
                    FileKind::Directory => std::fs::read_dir(&hit.path)
                        .map(|mut r| r.next().is_none())
                        .unwrap_or(false),
                    FileKind::File => hit.bytes == 0,
                    // A symlink's length is its target's path, so it is never meaningfully empty.
                    FileKind::Symlink => false,
                }
            } else {
                false
            };

            let selected =
                matcher.matches(&subject, &lowered) && passes_filters(query, &hit, empty);
            if selected == query.invert {
                continue;
            }

            summary.matched = summary.matched.saturating_add(1);
            batch.push(hit);

            if batch.len() >= BATCH {
                if !emit(std::mem::take(&mut batch)) {
                    stopped = true;
                    break;
                }
                batch = Vec::with_capacity(BATCH);
            }

            if let Some(limit) = query.limit
                && summary.matched >= limit
            {
                summary.truncated = true;
                stopped = true;
                break;
            }
        }

        if stopped {
            break;
        }
    }

    if !batch.is_empty() {
        emit(batch);
    }
    Ok(summary)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nix-search-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A small tree: two logs, one big file, an empty file, and a subdirectory.
    fn tree(tag: &str) -> PathBuf {
        let dir = scratch(tag);
        std::fs::write(dir.join("alpha.log"), b"aaaa").unwrap();
        std::fs::write(dir.join("beta.log"), b"bb").unwrap();
        std::fs::write(dir.join("big.bin"), vec![0u8; 50_000]).unwrap();
        std::fs::write(dir.join("empty.txt"), b"").unwrap();
        std::fs::create_dir(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested").join("gamma.log"), b"ccc").unwrap();
        dir
    }

    fn collect(query: &Query) -> (Vec<Hit>, Summary) {
        let token = CancelToken::new();
        let mut hits = Vec::new();
        let summary = run(query, &token, |batch| {
            hits.extend(batch);
            true
        })
        .unwrap();
        (hits, summary)
    }

    fn names(hits: &[Hit]) -> Vec<String> {
        let mut out: Vec<String> = hits
            .iter()
            .map(|h| h.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    // ---- the glob matcher ----

    #[test]
    fn a_glob_matches_the_way_a_shell_would() {
        for (pattern, subject, expected) in [
            ("*.log", "alpha.log", true),
            ("*.log", "alpha.txt", false),
            ("alpha.*", "alpha.log", true),
            ("*", "anything", true),
            ("*", "", true),
            ("a?c", "abc", true),
            ("a?c", "ac", false),
            ("a?c", "abbc", false),
            ("[abc]x", "ax", true),
            ("[abc]x", "dx", false),
            ("[a-z]x", "qx", true),
            ("[a-z]x", "1x", false),
            ("[!a]x", "bx", true),
            ("[!a]x", "ax", false),
            ("a*b*c", "axxbyyc", true),
            ("a*b*c", "axxc", false),
            ("exact", "exact", true),
            ("exact", "exactly", false),
            ("*.tar.gz", "archive.tar.gz", true),
            ("*.tar.gz", "archive.tar.bz2", false),
        ] {
            assert_eq!(
                glob_match(pattern, subject),
                expected,
                "{pattern:?} against {subject:?}"
            );
        }
    }

    /// A `[` with no `]` is a literal bracket, not a parse error — the user typed something, and
    /// refusing to search is a worse answer than treating it literally.
    #[test]
    fn an_unterminated_class_is_a_literal_bracket() {
        assert!(glob_match("a[bc", "a[bc"));
        assert!(!glob_match("a[bc", "ab"));
    }

    /// # Regression risk
    ///
    /// A recursive glob matcher is exponential on patterns of many stars against a non-matching
    /// subject — the classic `a*a*a*a*a*a*b` case. This one backtracks iteratively on the most recent
    /// star, so the same pattern is linear-ish and returns rather than hanging the search.
    #[test]
    fn many_stars_against_a_non_matching_subject_still_returns() {
        let pattern = "a*a*a*a*a*a*a*a*a*a*a*a*b";
        let subject = "a".repeat(64);

        let start = std::time::Instant::now();
        assert!(!glob_match(pattern, &subject));
        assert!(
            start.elapsed() < std::time::Duration::from_millis(500),
            "took {:?}, which suggests exponential backtracking",
            start.elapsed()
        );
    }

    // ---- inversion, the filter Stacer's did not implement ----

    /// `SYS-2`'s criterion. Stacer appended `-invert` to a `find` command line; `find` has no such
    /// predicate, so it exited with a usage error and the search returned nothing.
    #[test]
    fn inverting_returns_exactly_what_the_query_did_not() {
        let dir = tree("invert");

        let mut query = Query::under(&dir);
        query.name = "*.log".into();
        query.match_kind = NameMatch::Glob;
        let (matched, _) = collect(&query);

        query.invert = true;
        let (inverted, _) = collect(&query);

        assert_eq!(names(&matched), vec!["alpha.log", "beta.log", "gamma.log"]);
        assert!(!inverted.is_empty(), "inverting must not return nothing");
        assert_eq!(names(&inverted), vec!["big.bin", "empty.txt", "nested"]);

        // And the two sets partition everything examined, which is what "invert" has to mean.
        let mut both = names(&matched);
        both.extend(names(&inverted));
        both.sort();
        assert_eq!(both.len(), 6);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Inversion applies to the whole predicate, not only the name — otherwise "files not larger than
    /// 10 KB" and "not (files larger than 10 KB)" would differ.
    #[test]
    fn inverting_applies_to_every_filter_together() {
        let dir = tree("invert-filters");

        let mut query = Query::under(&dir);
        query.min_bytes = Some(1000);
        let (big, _) = collect(&query);
        assert_eq!(
            names(&big),
            vec!["big.bin"],
            "a directory's own st_size is 4096 on ext4, and must not satisfy a size filter"
        );

        query.invert = true;
        let (rest, _) = collect(&query);
        assert!(!names(&rest).contains(&"big.bin".to_string()));
        assert!(names(&rest).contains(&"alpha.log".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- filters ----

    #[test]
    fn a_substring_search_is_case_insensitive_by_default() {
        let dir = tree("case");
        let mut query = Query::under(&dir);
        query.name = "ALPHA".into();
        let (hits, _) = collect(&query);
        assert_eq!(names(&hits), vec!["alpha.log"]);

        query.case_sensitive = true;
        let (none, _) = collect(&query);
        assert!(
            none.is_empty(),
            "case-sensitive must respect the case given"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn size_bounds_are_inclusive_and_can_be_combined() {
        let dir = tree("size");
        let mut query = Query::under(&dir);
        query.min_bytes = Some(2);
        query.max_bytes = Some(4);
        query.kind = Some(FileKind::File);

        let (hits, _) = collect(&query);
        assert_eq!(names(&hits), vec!["alpha.log", "beta.log", "gamma.log"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_kind_filter_separates_files_from_directories() {
        let dir = tree("kind");
        let mut query = Query::under(&dir);

        query.kind = Some(FileKind::Directory);
        let (dirs, _) = collect(&query);
        assert_eq!(names(&dirs), vec!["nested"]);

        query.kind = Some(FileKind::File);
        let (files, _) = collect(&query);
        assert!(!names(&files).contains(&"nested".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_symlink_is_matched_as_a_symlink_and_never_followed() {
        let dir = scratch("symlink");
        std::fs::create_dir(dir.join("real")).unwrap();
        std::fs::write(dir.join("real").join("inside.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();

        let mut query = Query::under(&dir);
        query.kind = Some(FileKind::Symlink);
        let (links, _) = collect(&query);
        assert_eq!(names(&links), vec!["link"]);

        // And the walk must not descend through it, or a loop would be fatal.
        let (all, _) = collect(&Query::under(&dir));
        let inside = all
            .iter()
            .filter(|h| h.path.to_string_lossy().contains("inside.txt"))
            .count();
        assert_eq!(
            inside, 1,
            "the file was reached twice, so the link was followed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_only_finds_zero_length_files() {
        let dir = tree("empty");
        let mut query = Query::under(&dir);
        query.empty_only = true;
        query.kind = Some(FileKind::File);

        let (hits, _) = collect(&query);
        assert_eq!(names(&hits), vec!["empty.txt"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// # Regression
    ///
    /// A directory's `st_size` is 4,096 on ext4 regardless of contents, so treating it as a size made
    /// every directory match `min_bytes: 1000` — and made every directory "non-empty" whether or not
    /// it held anything.
    #[test]
    fn a_directory_has_no_size_and_is_empty_when_it_has_no_entries() {
        let dir = scratch("diremptiness");
        std::fs::create_dir(dir.join("hollow")).unwrap();
        std::fs::create_dir(dir.join("occupied")).unwrap();
        std::fs::write(dir.join("occupied").join("x.txt"), b"x").unwrap();

        let (all, _) = collect(&Query::under(&dir));
        for hit in all.iter().filter(|h| h.kind == FileKind::Directory) {
            assert_eq!(hit.bytes, 0, "{} reported a size", hit.path.display());
        }

        let mut query = Query::under(&dir);
        query.empty_only = true;
        query.kind = Some(FileKind::Directory);
        let (empty, _) = collect(&query);

        assert_eq!(
            names(&empty),
            vec!["hollow"],
            "empty means no entries for a directory, not no bytes"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_mode_filter_matches_files_with_all_of_those_bits() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("mode");
        std::fs::write(dir.join("script.sh"), b"#!/bin/sh\n").unwrap();
        std::fs::write(dir.join("plain.txt"), b"x").unwrap();
        std::fs::set_permissions(
            dir.join("script.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::set_permissions(
            dir.join("plain.txt"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        let mut query = Query::under(&dir);
        query.mode_all_of = Some(0o111);
        let (hits, _) = collect(&query);
        assert_eq!(names(&hits), vec!["script.sh"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_owner_filter_matches_this_user_and_not_a_uid_that_owns_nothing_here() {
        let dir = tree("owner");
        let mine = std::fs::metadata(&dir).unwrap();
        use std::os::unix::fs::MetadataExt;

        let mut query = Query::under(&dir);
        query.owner_uid = Some(mine.uid());
        let (hits, _) = collect(&query);
        assert_eq!(hits.len(), 6, "this user owns everything just created");

        query.owner_uid = Some(u32::MAX - 1);
        let (none, _) = collect(&query);
        assert!(none.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A time filter cannot be satisfied by a file whose time is unknown. Including such files would
    /// put results in the list that were not asked for.
    #[test]
    fn a_time_filter_excludes_a_file_with_no_known_time() {
        let hit = Hit {
            path: PathBuf::from("/x"),
            bytes: 0,
            kind: FileKind::File,
            modified: None,
            uid: 0,
            mode: 0o644,
        };
        let mut query = Query::under("/x");
        query.modified_after = Some(1000);
        assert!(!passes_filters(&query, &hit, false));

        query.modified_after = None;
        assert!(
            passes_filters(&query, &hit, false),
            "and no filter means no exclusion"
        );
    }

    #[test]
    fn modified_bounds_select_by_age() {
        let dir = tree("time");
        let now = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut query = Query::under(&dir);
        query.modified_after = Some(now.saturating_sub(3600));
        let (recent, _) = collect(&query);
        assert_eq!(recent.len(), 6, "everything was just created");

        query.modified_after = None;
        query.modified_before = Some(now.saturating_sub(3600));
        let (old, _) = collect(&query);
        assert!(old.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- streaming, capping, cancelling ----

    /// No cap by default. Stacer showed `foundFiles.mid(1, 2000)`.
    #[test]
    fn there_is_no_row_cap_unless_one_is_asked_for() {
        let dir = scratch("uncapped");
        for i in 0..600 {
            std::fs::write(dir.join(format!("f{i}.txt")), b"x").unwrap();
        }

        let query = Query::under(&dir);
        let (hits, summary) = collect(&query);
        assert_eq!(hits.len(), 600);
        assert_eq!(summary.matched, 600);
        assert!(!summary.truncated);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_limit_stops_early_and_says_so() {
        let dir = scratch("capped");
        for i in 0..600 {
            std::fs::write(dir.join(format!("f{i}.txt")), b"x").unwrap();
        }

        let mut query = Query::under(&dir);
        query.limit = Some(100);
        let (hits, summary) = collect(&query);

        assert!(
            summary.truncated,
            "a truncated result must not look complete"
        );
        assert!(hits.len() >= 100, "at least the limit was delivered");
        assert!(summary.matched >= 100);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Results arrive in batches rather than all at the end, so a caller can show the first screen.
    #[test]
    fn results_are_delivered_in_batches_as_they_are_found() {
        let dir = scratch("batches");
        for i in 0..900 {
            std::fs::write(dir.join(format!("f{i}.txt")), b"x").unwrap();
        }

        let token = CancelToken::new();
        let mut batches = 0;
        run(&Query::under(&dir), &token, |_| {
            batches += 1;
            true
        })
        .unwrap();

        assert!(batches > 1, "900 results arrived in {batches} batch(es)");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A caller returning `false` stops the walk — how a UI with its own cap imposes one without the
    /// search needing to know about it.
    #[test]
    fn the_caller_can_stop_the_walk() {
        let dir = scratch("stop");
        for i in 0..900 {
            std::fs::write(dir.join(format!("f{i}.txt")), b"x").unwrap();
        }

        let token = CancelToken::new();
        let mut seen = 0;
        run(&Query::under(&dir), &token, |batch| {
            seen += batch.len();
            false // stop after the first batch
        })
        .unwrap();

        assert!(
            seen < 900,
            "the walk continued after being told to stop: {seen}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_cancelled_search_stops_and_reports_that_it_was_cancelled() {
        let dir = tree("cancel");
        let token = CancelToken::new();
        token.cancel();

        let summary = run(&Query::under(&dir), &token, |_| true).unwrap();
        assert!(summary.cancelled);
        assert_eq!(summary.matched, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An unreadable directory is counted, not ignored. A partial result that does not say it is
    /// partial is the failure Stacer answered by re-running `find` as root.
    #[test]
    fn a_directory_that_cannot_be_entered_is_counted() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("unreadable");
        let locked = dir.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("hidden.txt"), b"x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let (hits, summary) = collect(&Query::under(&dir));

        // Running as root would read it anyway, in which case there is nothing to assert.
        if !names(&hits).contains(&"hidden.txt".to_string()) {
            assert!(
                summary.unreadable > 0,
                "the locked directory was skipped without being reported"
            );
        }

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_root_that_does_not_exist_is_an_error_rather_than_an_empty_result() {
        let token = CancelToken::new();
        let query = Query::under("/nonexistent/nix-test-search-root");
        assert!(
            run(&query, &token, |_| true).is_err(),
            "an empty result would look like a successful search of nothing"
        );
    }

    #[test]
    fn matching_on_the_whole_path_finds_by_directory_name() {
        let dir = tree("wholepath");
        let mut query = Query::under(&dir);
        query.name = "nested".into();
        query.whole_path = true;

        let (hits, _) = collect(&query);
        assert!(
            names(&hits).contains(&"gamma.log".to_string()),
            "a file inside a matching directory matches on the whole path: {:?}",
            names(&hits)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- regex, behind its feature ----

    #[cfg(feature = "regex")]
    #[test]
    fn a_regex_query_matches_and_an_invalid_one_is_reported() {
        let dir = tree("regex");
        let mut query = Query::under(&dir);
        query.match_kind = NameMatch::Regex;
        query.name = r"^(alpha|beta)\.log$".into();

        let (hits, _) = collect(&query);
        assert_eq!(names(&hits), vec!["alpha.log", "beta.log"]);

        query.name = "(unclosed".into();
        let token = CancelToken::new();
        let error = run(&query, &token, |_| true).unwrap_err();
        assert_eq!(
            error.code,
            crate::error::ErrorCode::InvalidInput,
            "a bad pattern must be reported, not silently match nothing"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Without the feature, a regex query says so rather than quietly returning nothing — which is the
    /// exact failure mode of Stacer's `-invert`.
    #[cfg(not(feature = "regex"))]
    #[test]
    fn a_regex_query_without_the_feature_is_unsupported_not_empty() {
        let dir = tree("noregex");
        let mut query = Query::under(&dir);
        query.match_kind = NameMatch::Regex;
        query.name = "anything".into();

        let token = CancelToken::new();
        let error = run(&query, &token, |_| true).unwrap_err();
        assert_eq!(error.code, crate::error::ErrorCode::Unsupported);
        std::fs::remove_dir_all(&dir).ok();
    }
}
