// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Snap revisions. `STO-12`.
//!
//! # Why this is worth doing
//!
//! snapd keeps superseded revisions so a bad refresh can be rolled back. On this development
//! machine that is **3.3 GiB across eighteen revisions** — more than the old kernels — and Stacer
//! offered none of it: it listed snaps by name only, with no revisions and no sizes.
//!
//! # The rule
//!
//! > Only revisions snapd itself marks `disabled`.
//!
//! A `disabled` revision is one snapd has superseded; the active revision is what is running. Never
//! offering the active revision is the equivalent of never offering the running kernel, and like
//! that rule it is enforced here *and* re-derived inside the privileged helper.
//!
//! # Hard links, and why the sharing machinery applies on ext4 too
//!
//! snapd hard-links downloaded blobs into its own cache so a re-install is cheap. Fifteen of the
//! eighteen blobs on this machine have a link count above one — so removing the revision drops one
//! reference, and the space comes back only when snapd's cache prunes the other.
//!
//! That is the same problem [`crate::space::Reclaimable`] was built for on copy-on-write
//! filesystems, and it turns out not to be a copy-on-write problem at all: a hard link creates it on
//! plain ext4. A blob with more than one link is reported as `AtMost` with nothing proven, because
//! nix cannot see snapd's cache policy and will not guess at it.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::space::Reclaimable;

/// Where snapd keeps downloaded revision blobs.
const SNAP_BLOBS: &str = "/var/lib/snapd/snaps";

/// One installed snap revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub name: String,
    pub revision: String,
    pub version: String,
    /// Whether snapd has superseded this revision. Only these are ever removable.
    pub disabled: bool,
    /// Size of the revision's blob, when it can be found.
    pub bytes: u64,
    /// Number of filesystem links to the blob. Above one means the space is shared.
    pub links: u64,
    pub blob: Option<PathBuf>,
}

impl Revision {
    /// How much of [`Revision::bytes`] would actually come back.
    #[must_use]
    pub fn reclaimable(&self) -> Reclaimable {
        if self.links <= 1 {
            return Reclaimable::Exact;
        }
        Reclaimable::AtMost {
            // snapd's cache is not readable without privilege and its pruning policy is snapd's
            // own, so nothing here can be proven. Guessing would be an overpromise.
            exclusive: None,
            reason: format!(
                "snapd also keeps this {} blob in its download cache, so removing the revision may not return the space until that cache is pruned.",
                crate::format_bytes(self.bytes)
            ),
        }
    }
}

/// Parse `snap list --all`.
///
/// Real output from a running machine:
///
/// ```text
/// Name      Version           Rev   Tracking       Publisher    Notes
/// chromium  151.0.7922.108    3507  latest/stable  canonical**  -
/// chromium  150.0.7871.128    3499  latest/stable  canonical**  disabled
/// core18    20260105          2979  latest/stable  canonical**  base,disabled
/// ```
///
/// `Notes` is a comma-separated set, so `disabled` must be matched as a member rather than by
/// substring — `base,disabled` and a hypothetical `disabled-something` are different things.
#[must_use]
pub fn parse_snap_list(output: &str) -> Vec<Revision> {
    output
        .lines()
        .skip(1) // header
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // name, version, rev, tracking, publisher, notes
            if fields.len() < 6 {
                return None;
            }
            let notes = fields[5];
            Some(Revision {
                name: fields[0].to_string(),
                version: fields[1].to_string(),
                revision: fields[2].to_string(),
                disabled: notes.split(',').any(|n| n == "disabled"),
                bytes: 0,
                links: 1,
                blob: None,
            })
        })
        .collect()
}

/// Fill in a revision's blob path, size and link count.
///
/// The blob directory is world-readable even though the blobs themselves are not, so sizing needs no
/// privilege — only removal does.
pub fn measure(revision: &mut Revision, blob_dir: &Path) {
    use std::os::unix::fs::MetadataExt;

    let path = blob_dir.join(format!("{}_{}.snap", revision.name, revision.revision));
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        revision.bytes = meta.blocks() * 512;
        revision.links = meta.nlink();
        revision.blob = Some(path);
    }
}

/// Every installed revision, measured.
pub fn revisions() -> Result<Vec<Revision>> {
    let output = super::query("snap", &["list", "--all"])?;
    let mut found = parse_snap_list(&output);
    let blob_dir = Path::new(SNAP_BLOBS);
    for revision in &mut found {
        measure(revision, blob_dir);
    }
    Ok(found)
}

/// Revisions that are safe to remove: those snapd has marked `disabled`.
///
/// A snap whose *only* revision is somehow disabled is excluded as well — removing the sole revision
/// of a snap uninstalls it, which is a different decision from reclaiming a superseded copy.
#[must_use]
pub fn removable(all: &[Revision]) -> Vec<Revision> {
    use std::collections::HashMap;

    let mut per_snap: HashMap<&str, usize> = HashMap::new();
    for revision in all {
        *per_snap.entry(revision.name.as_str()).or_default() += 1;
    }

    all.iter()
        .filter(|r| r.disabled)
        // More than one revision present, so removing this one leaves the snap installed.
        .filter(|r| per_snap.get(r.name.as_str()).copied().unwrap_or(0) > 1)
        // Nothing to reclaim from a blob that is not there.
        .filter(|r| r.bytes > 0)
        .cloned()
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Captured from a running machine, including the awkward `base,disabled` composite note and a
    /// snap whose two revisions differ only by number.
    const REAL_SNAP_LIST: &str = "\
Name                       Version                         Rev    Tracking         Publisher        Notes
bare                       1.0                             5      latest/stable    canonical**      base
chromium                   151.0.7922.108                  3507   latest/stable    canonical**      -
chromium                   150.0.7871.128                  3499   latest/stable    canonical**      disabled
core18                     20260105                        2979   latest/stable    canonical**      base,disabled
core18                     20260204                        2999   latest/stable    canonical**      base
core22                     20260410                        2437   latest/stable    canonical**      base
core22                     20260225                        2411   latest/stable    canonical**      base,disabled
cups                       2.4.19-2                        1229   latest/stable    openprinting**   disabled
cups                       2.4.19-2                        1238   latest/stable    openprinting**   -
lonely                     1.0                             10     latest/stable    someone          disabled
";

    #[test]
    fn revisions_are_parsed_from_real_output() {
        let parsed = parse_snap_list(REAL_SNAP_LIST);
        assert_eq!(parsed.len(), 10, "the header must not become a revision");

        let chromium: Vec<&Revision> = parsed.iter().filter(|r| r.name == "chromium").collect();
        assert_eq!(chromium.len(), 2);
        assert_eq!(chromium[0].revision, "3507");
        assert!(!chromium[0].disabled, "the active revision is not disabled");
        assert_eq!(chromium[1].revision, "3499");
        assert!(chromium[1].disabled);
    }

    /// `Notes` is a comma-separated set, so `disabled` is a member rather than a substring.
    #[test]
    fn a_composite_note_still_marks_a_revision_disabled() {
        let parsed = parse_snap_list(REAL_SNAP_LIST);
        let superseded_base = parsed
            .iter()
            .find(|r| r.name == "core18" && r.revision == "2979")
            .unwrap();
        assert!(
            superseded_base.disabled,
            "base,disabled must be read as disabled"
        );

        let active_base = parsed
            .iter()
            .find(|r| r.name == "core18" && r.revision == "2999")
            .unwrap();
        assert!(!active_base.disabled, "plain 'base' is the active revision");
    }

    #[test]
    fn a_note_merely_containing_the_word_is_not_a_match() {
        let output = "Name Version Rev Tracking Publisher Notes\nthing 1.0 1 x y disabled-soon\n";
        let parsed = parse_snap_list(output);
        assert!(
            !parsed[0].disabled,
            "'disabled-soon' is not the 'disabled' note"
        );
    }

    /// The safety rule: only what snapd has superseded.
    #[test]
    fn only_disabled_revisions_are_removable() {
        let mut all = parse_snap_list(REAL_SNAP_LIST);
        // Give everything a blob so size is not what filters them.
        for r in &mut all {
            r.bytes = 1024;
        }

        let removable = removable(&all);
        assert!(
            removable.iter().all(|r| r.disabled),
            "an active revision is what is running, so it must never be offered"
        );
        // chromium 3507, core18 2999, core22 2437, cups 1238, bare 5 are active.
        assert!(
            !removable.iter().any(|r| r.revision == "3507"),
            "the active chromium must not be offered"
        );
    }

    /// Removing a snap's only revision uninstalls it, which is a different decision.
    #[test]
    fn a_snap_with_a_single_revision_is_never_offered() {
        let mut all = parse_snap_list(REAL_SNAP_LIST);
        for r in &mut all {
            r.bytes = 1024;
        }
        let removable = removable(&all);
        assert!(
            !removable.iter().any(|r| r.name == "lonely"),
            "removing the only revision of a snap uninstalls it, which is not reclaiming"
        );
    }

    #[test]
    fn a_revision_with_no_blob_is_not_offered() {
        let all = parse_snap_list(REAL_SNAP_LIST); // bytes all zero
        assert!(
            removable(&all).is_empty(),
            "nothing to reclaim from a blob that is not there"
        );
    }

    /// The hard-link case, which turns out not to be a copy-on-write problem at all.
    #[test]
    fn a_hard_linked_blob_promises_nothing() {
        let shared = Revision {
            name: "chromium".into(),
            revision: "3499".into(),
            version: "150".into(),
            disabled: true,
            bytes: 197_312_512,
            links: 2,
            blob: None,
        };
        let verdict = shared.reclaimable();
        assert!(!verdict.is_exact());
        assert_eq!(
            verdict.promisable(shared.bytes),
            0,
            "snapd's cache holds the other link and its pruning policy is not ours to guess"
        );
        let caveat = verdict.caveat().unwrap();
        assert!(caveat.contains("cache"), "{caveat}");
        assert!(
            caveat.contains("188.2 MiB"),
            "the caveat should quantify: {caveat}"
        );
    }

    #[test]
    fn a_blob_with_one_link_is_exact() {
        let sole = Revision {
            name: "thing".into(),
            revision: "1".into(),
            version: "1.0".into(),
            disabled: true,
            bytes: 4096,
            links: 1,
            blob: None,
        };
        assert_eq!(sole.reclaimable(), Reclaimable::Exact);
    }

    #[test]
    fn measuring_reads_size_and_link_count_from_disk() {
        let dir = std::env::temp_dir().join(format!("nix-snapblobs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let blob = dir.join("thing_42.snap");
        std::fs::write(&blob, vec![b'x'; 8192]).unwrap();
        std::fs::hard_link(&blob, dir.join("cached")).unwrap();

        let mut revision = Revision {
            name: "thing".into(),
            revision: "42".into(),
            version: "1.0".into(),
            disabled: true,
            bytes: 0,
            links: 1,
            blob: None,
        };
        measure(&mut revision, &dir);

        assert!(revision.bytes >= 8192);
        assert_eq!(revision.links, 2, "the hard link must be detected");
        assert_eq!(revision.blob, Some(blob));
        assert!(!revision.reclaimable().is_exact());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_blob_leaves_the_revision_unmeasured() {
        let mut revision = Revision {
            name: "absent".into(),
            revision: "1".into(),
            version: "1.0".into(),
            disabled: true,
            bytes: 0,
            links: 1,
            blob: None,
        };
        measure(&mut revision, Path::new("/definitely/not/here"));
        assert_eq!(revision.bytes, 0);
        assert!(revision.blob.is_none());
    }

    // ---- against this machine ----

    #[test]
    fn this_machines_revisions_are_readable_and_the_active_ones_protected() {
        if !crate::caps::registry().has(crate::caps::Capability::Snap) {
            return;
        }
        let Ok(all) = revisions() else { return };
        if all.is_empty() {
            return;
        }

        let removable = removable(&all);
        for revision in &removable {
            assert!(
                revision.disabled,
                "{} rev {} is active",
                revision.name, revision.revision
            );
            assert!(revision.bytes > 0);
        }
        // Every offered revision has a sibling that stays.
        for revision in &removable {
            let siblings = all.iter().filter(|r| r.name == revision.name).count();
            assert!(
                siblings > 1,
                "{} would be uninstalled, not reclaimed",
                revision.name
            );
        }
    }
}
