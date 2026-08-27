// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Flatpak installations. `STO-12`.
//!
//! # Two different things worth reclaiming
//!
//! 1. **Unused runtimes.** A runtime installed for an application that has since been removed keeps
//!    sitting there. Runtimes are the large objects in a flatpak installation — a platform runtime
//!    is comfortably over a gigabyte.
//! 2. **An orphaned repository.** Flatpak stores content in an ostree repository and *deploys* it by
//!    hard-linking objects out of that repository into place. Uninstalling drops the deployment but
//!    leaves the repository objects, which are reclaimed only when the repository is pruned.
//!
//! On this development machine the second case is the whole story: nothing at all is installed, and
//! `/var/lib/flatpak/repo` is still **609 MiB** of objects that nothing references.
//!
//! # Hard links again
//!
//! Because a deployment is hard links into the repository, uninstalling a runtime frees very little
//! on its own — the objects survive under the repository until a prune. Telling someone "remove this
//! runtime to get 1.2 GiB back" would be false. So a deployment whose files have a link count above
//! one is reported through [`Reclaimable::AtMost`], and the honest way to actually reclaim the space
//! is uninstall *followed by* a prune.
//!
//! This is the third distinct place the same problem has turned up — copy-on-write snapshots
//! ([`crate::cow`]), snap blobs ([`super::snap`]), and ostree deployments — which is a good sign that
//! [`crate::space::Reclaimable`] is modelling something real rather than a btrfs quirk.
//!
//! # What is verified and what is not
//!
//! The repository measurement and the "nothing is installed" case are exercised against this
//! machine. The runtime-listing parsers are covered by fixtures captured from flatpak's documented
//! column output but have **not** been run against a machine with flatpaks actually installed, for
//! the plain reason that this one has none. They are flagged in `PLAN.md` alongside the
//! copy-on-write parsers as code awaiting real exposure.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::space::Reclaimable;

/// The system installation. Flatpak also supports per-user installations under
/// `$XDG_DATA_HOME/flatpak`, handled by [`user_root`].
pub const SYSTEM_ROOT: &str = "/var/lib/flatpak";

/// A per-user flatpak installation, if the environment describes one.
#[must_use]
pub fn user_root(data_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    match (data_home, home) {
        (Some(data), _) if !data.is_empty() => Some(Path::new(data).join("flatpak")),
        (_, Some(home)) if !home.is_empty() => Some(Path::new(home).join(".local/share/flatpak")),
        _ => None,
    }
}

/// One installed flatpak ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    /// `runtime/org.freedesktop.Platform/x86_64/23.08`
    pub reference: String,
    pub kind: RefKind,
    pub id: String,
    pub arch: String,
    pub branch: String,
    /// For an application, the runtime it declares. Empty for a runtime.
    pub runtime: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    App,
    Runtime,
}

impl Ref {
    /// Parse one `ref` column value.
    #[must_use]
    pub fn parse(reference: &str, runtime: &str) -> Option<Self> {
        let mut parts = reference.split('/');
        let kind = match parts.next()? {
            "app" => RefKind::App,
            "runtime" => RefKind::Runtime,
            _ => return None,
        };
        let id = parts.next()?.to_string();
        let arch = parts.next().unwrap_or_default().to_string();
        let branch = parts.next().unwrap_or_default().to_string();
        if id.is_empty() {
            return None;
        }
        Some(Self {
            reference: reference.to_string(),
            kind,
            id,
            arch,
            branch,
            runtime: runtime.to_string(),
        })
    }

    /// Where this ref is deployed inside an installation root.
    #[must_use]
    pub fn deploy_dir(&self, root: &Path) -> PathBuf {
        let kind = match self.kind {
            RefKind::App => "app",
            RefKind::Runtime => "runtime",
        };
        root.join(kind)
            .join(&self.id)
            .join(&self.arch)
            .join(&self.branch)
    }

    /// Whether this ref is an extension of `parent` — `org.freedesktop.Platform.GL.default` extends
    /// `org.freedesktop.Platform`.
    ///
    /// Compared component-wise on the dotted identifier, so `org.freedesktop.PlatformOther` is not
    /// treated as an extension of `org.freedesktop.Platform`. This is the same reasoning as the
    /// path-component matching in [`crate::protect`], for the same reason: a prefix test on a
    /// delimited name is wrong at the delimiter.
    #[must_use]
    pub fn extends(&self, parent: &str) -> bool {
        self.id.len() > parent.len()
            && self.id.starts_with(parent)
            && self.id.as_bytes().get(parent.len()) == Some(&b'.')
    }
}

/// Parse `flatpak list --columns=ref,runtime`.
///
/// Columns are tab-separated. An application row carries its runtime; a runtime row leaves the
/// column empty, which arrives as a trailing tab or a missing field depending on version.
#[must_use]
pub fn parse_list(output: &str) -> Vec<Ref> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let reference = fields.next()?.trim();
            let runtime = fields.next().unwrap_or_default().trim();
            Ref::parse(reference, runtime)
        })
        .collect()
}

/// Runtimes that no installed application needs.
///
/// # The conservative rule
///
/// A runtime is offered only when **no** installed application declares it, and no application
/// declares a runtime it extends. Extensions are the trap: `org.freedesktop.Platform.GL.default` is
/// a separate ref that no application names directly, but removing it breaks every application using
/// `org.freedesktop.Platform`.
///
/// Even with that rule, flatpak's own dependency resolution is the authority — this derivation
/// decides what to *show*, and `flatpak uninstall --unused` decides what is actually removed. When
/// the two disagree, flatpak wins and the preview was an over-estimate, which is why the resulting
/// candidate is qualified rather than promised.
#[must_use]
pub fn unused_runtimes(refs: &[Ref]) -> Vec<Ref> {
    let needed: HashSet<&str> = refs
        .iter()
        .filter(|r| r.kind == RefKind::App)
        .map(|r| r.runtime.as_str())
        .filter(|r| !r.is_empty())
        .collect();

    // A needed runtime's identifier, so extensions of it can be spared too.
    let needed_ids: Vec<&str> = needed
        .iter()
        .filter_map(|reference| reference.split('/').nth(1))
        .collect();

    refs.iter()
        .filter(|r| r.kind == RefKind::Runtime)
        .filter(|r| !needed.contains(r.reference.as_str()))
        // Not named directly, but still an extension of something that is.
        .filter(|r| !needed_ids.iter().any(|parent| r.extends(parent)))
        // A runtime that is itself the parent of a needed extension stays as well.
        .filter(|r| !needed_ids.iter().any(|needed| needed == &r.id))
        .cloned()
        .collect()
}

/// What a directory occupies, and whether that space is shared with something outside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TreeSize {
    /// On-disk blocks, counting each inode once however many times it appears in the tree.
    pub bytes: u64,
    /// Files whose link count exceeds the number of links found inside this tree — so another
    /// reference exists elsewhere, typically the ostree repository.
    pub shared_files: u64,
}

impl TreeSize {
    /// How much of [`TreeSize::bytes`] a removal would actually return.
    #[must_use]
    pub fn reclaimable(self, what: &str) -> Reclaimable {
        if self.shared_files == 0 {
            return Reclaimable::Exact;
        }
        Reclaimable::AtMost {
            // Flatpak deploys by hard-linking out of its ostree repository, so the objects survive
            // the uninstall. How much a later prune returns is ostree's accounting, not ours.
            exclusive: None,
            reason: format!(
                "{} of this {} is hard-linked into flatpak's repository, so the space returns only when that repository is pruned.",
                if self.shared_files == 1 {
                    "Part".to_string()
                } else {
                    format!("{} files", self.shared_files)
                },
                what,
            ),
        }
    }
}

/// Measure a directory tree, counting each inode once and noticing links to outside it.
pub fn measure_tree(dir: &Path) -> TreeSize {
    use std::os::unix::fs::MetadataExt;

    // inode -> (blocks, declared link count, links seen inside this tree)
    let mut seen: HashMap<(u64, u64), (u64, u64, u64)> = HashMap::new();
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                // Never follow a symlink out of the tree.
                if !meta.file_type().is_symlink() {
                    stack.push(entry.path());
                }
                continue;
            }
            let key = (meta.dev(), meta.ino());
            let slot = seen
                .entry(key)
                .or_insert((meta.blocks() * 512, meta.nlink(), 0));
            slot.2 += 1;
        }
    }

    let mut size = TreeSize::default();
    for (blocks, declared, inside) in seen.into_values() {
        size.bytes += blocks;
        if declared > inside {
            size.shared_files += 1;
        }
    }
    size
}

/// Everything installed, across the system installation.
pub fn installed() -> Result<Vec<Ref>> {
    // `--columns` keeps the output machine-readable and avoids parsing a localised human size.
    let output = super::query("flatpak", &["list", "--columns=ref,runtime"])?;
    Ok(parse_list(&output))
}

/// The size of an installation's ostree repository.
///
/// This is the object store. When nothing is installed, every object in it is unreferenced.
pub fn repository_size(root: &Path) -> TreeSize {
    measure_tree(&root.join("repo"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Shaped after flatpak's documented `--columns=ref,runtime` output: tab-separated, with the
    /// runtime column empty for runtime rows.
    const LIST: &str = "\
app/org.gnome.Calculator/x86_64/stable\truntime/org.gnome.Platform/x86_64/45\n\
runtime/org.gnome.Platform/x86_64/45\t\n\
runtime/org.freedesktop.Platform/x86_64/23.08\t\n\
runtime/org.freedesktop.Platform.GL.default/x86_64/23.08\t\n\
runtime/org.gnome.Platform.Locale/x86_64/45\t\n\
";

    #[test]
    fn refs_are_parsed_into_their_parts() {
        let refs = parse_list(LIST);
        assert_eq!(refs.len(), 5);

        assert_eq!(refs[0].kind, RefKind::App);
        assert_eq!(refs[0].id, "org.gnome.Calculator");
        assert_eq!(refs[0].arch, "x86_64");
        assert_eq!(refs[0].branch, "stable");
        assert_eq!(refs[0].runtime, "runtime/org.gnome.Platform/x86_64/45");

        assert_eq!(refs[1].kind, RefKind::Runtime);
        assert!(refs[1].runtime.is_empty());
    }

    #[test]
    fn a_malformed_ref_is_dropped_rather_than_guessed_at() {
        assert!(Ref::parse("nonsense", "").is_none());
        assert!(Ref::parse("app/", "").is_none(), "an empty id is not a ref");
        assert!(parse_list("\n\n  \n").is_empty());
    }

    /// The core of the safety argument for this category.
    #[test]
    fn a_runtime_an_installed_app_needs_is_never_offered() {
        let unused = unused_runtimes(&parse_list(LIST));
        assert!(
            !unused.iter().any(|r| r.id == "org.gnome.Platform"),
            "the Calculator needs it"
        );
    }

    /// The trap: an extension no application names directly, but which is still in use.
    #[test]
    fn an_extension_of_a_needed_runtime_is_never_offered() {
        let unused = unused_runtimes(&parse_list(LIST));
        assert!(
            !unused.iter().any(|r| r.id == "org.gnome.Platform.Locale"),
            "removing the Locale extension of a runtime in use would break it"
        );
    }

    #[test]
    fn a_genuinely_unreferenced_runtime_is_offered() {
        let unused = unused_runtimes(&parse_list(LIST));
        let ids: Vec<&str> = unused.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"org.freedesktop.Platform"), "{ids:?}");
        assert!(
            ids.contains(&"org.freedesktop.Platform.GL.default"),
            "its parent is unused too, so the extension goes with it: {ids:?}"
        );
        assert_eq!(unused.len(), 2, "{ids:?}");
    }

    /// A prefix test on a dotted name is wrong at the delimiter — the same mistake `protect` avoids
    /// on path components.
    #[test]
    fn extension_matching_respects_the_dot() {
        let r = Ref::parse("runtime/org.freedesktop.PlatformOther/x86_64/1", "").unwrap();
        assert!(
            !r.extends("org.freedesktop.Platform"),
            "PlatformOther is a different runtime, not an extension"
        );

        let ext = Ref::parse("runtime/org.freedesktop.Platform.GL/x86_64/1", "").unwrap();
        assert!(ext.extends("org.freedesktop.Platform"));
        assert!(
            !ext.extends("org.freedesktop.Platform.GL"),
            "a ref does not extend itself"
        );
    }

    #[test]
    fn every_runtime_is_offered_when_no_app_is_installed() {
        let only_runtimes = parse_list(
            "runtime/org.gnome.Platform/x86_64/45\t\nruntime/org.kde.Platform/x86_64/6.6\t\n",
        );
        assert_eq!(unused_runtimes(&only_runtimes).len(), 2);
    }

    #[test]
    fn deploy_directories_are_built_from_the_ref() {
        let r = Ref::parse("runtime/org.gnome.Platform/x86_64/45", "").unwrap();
        assert_eq!(
            r.deploy_dir(Path::new("/var/lib/flatpak")),
            Path::new("/var/lib/flatpak/runtime/org.gnome.Platform/x86_64/45")
        );
    }

    #[test]
    fn user_installations_follow_xdg_then_home() {
        assert_eq!(
            user_root(Some("/x/data"), Some("/home/a")),
            Some(PathBuf::from("/x/data/flatpak"))
        );
        assert_eq!(
            user_root(None, Some("/home/a")),
            Some(PathBuf::from("/home/a/.local/share/flatpak"))
        );
        assert_eq!(
            user_root(Some(""), Some("/home/a")),
            Some(PathBuf::from("/home/a/.local/share/flatpak")),
            "an empty variable is unset, not a root at /flatpak"
        );
        assert_eq!(user_root(None, None), None);
    }

    // ---- tree measurement ----

    /// Distinct per call, because tests run in parallel and a shared name collides.
    fn sandbox(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nix-flatpak-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_tree_with_no_outside_links_measures_exactly() {
        let dir = sandbox("exact");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a"), vec![b'x'; 4096]).unwrap();
        std::fs::write(dir.join("sub/b"), vec![b'y'; 4096]).unwrap();

        let size = measure_tree(&dir);
        assert!(size.bytes >= 8192, "{size:?}");
        assert_eq!(size.shared_files, 0);
        assert_eq!(size.reclaimable("runtime"), Reclaimable::Exact);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The ostree case: the deployment is a hard link and the object lives elsewhere.
    #[test]
    fn a_link_out_of_the_tree_makes_the_estimate_an_upper_bound() {
        let base = sandbox("shared");
        let repo = base.join("repo");
        let deploy = base.join("deploy");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&deploy).unwrap();

        std::fs::write(repo.join("object"), vec![b'z'; 8192]).unwrap();
        std::fs::hard_link(repo.join("object"), deploy.join("file")).unwrap();

        let size = measure_tree(&deploy);
        assert_eq!(size.shared_files, 1, "the repository link must be noticed");

        let verdict = size.reclaimable("runtime");
        assert_eq!(
            verdict.promisable(size.bytes),
            0,
            "uninstalling does not free an object the repository still holds"
        );
        assert!(verdict.caveat().unwrap().contains("pruned"));

        std::fs::remove_dir_all(&base).ok();
    }

    /// Two links *inside* the tree are one file, and nothing is shared with the outside.
    #[test]
    fn links_within_the_tree_are_counted_once_and_are_not_shared() {
        let dir = sandbox("internal");
        std::fs::write(dir.join("a"), vec![b'q'; 8192]).unwrap();
        std::fs::hard_link(dir.join("a"), dir.join("b")).unwrap();

        let size = measure_tree(&dir);
        assert!(size.bytes < 16384, "one inode, counted once: {size:?}");
        assert_eq!(
            size.shared_files, 0,
            "both links are inside the tree, so removing it frees the space"
        );
        assert_eq!(size.reclaimable("runtime"), Reclaimable::Exact);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_directory_measures_zero_rather_than_failing() {
        assert_eq!(
            measure_tree(Path::new("/definitely/not/here")),
            TreeSize::default()
        );
    }

    #[test]
    fn a_symlinked_directory_is_not_followed() {
        let base = sandbox("symlink");
        let outside = base.join("outside");
        let tree = base.join("tree");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(outside.join("big"), vec![b'x'; 65536]).unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("link")).unwrap();

        let size = measure_tree(&tree);
        assert_eq!(
            size.bytes, 0,
            "following a symlink would attribute another directory's bytes here: {size:?}"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    // ---- against this machine ----

    #[test]
    fn this_machines_flatpak_state_is_consistent() {
        if !crate::caps::registry().has(crate::caps::Capability::Flatpak) {
            return;
        }
        let Ok(refs) = installed() else { return };

        // Whatever is installed, nothing an app needs may be offered.
        let unused = unused_runtimes(&refs);
        for runtime in &unused {
            assert!(
                runtime.kind == RefKind::Runtime,
                "only runtimes are offered"
            );
            let needed_by_app = refs
                .iter()
                .any(|r| r.kind == RefKind::App && r.runtime == runtime.reference);
            assert!(!needed_by_app, "{} is in use", runtime.reference);
        }

        // The repository exists independently of what is installed, which is the whole point.
        let root = Path::new(SYSTEM_ROOT);
        if root.join("repo").is_dir() {
            let repo = repository_size(root);
            assert!(
                repo.bytes > 0,
                "a repository directory that exists holds objects"
            );
        }
    }
}
