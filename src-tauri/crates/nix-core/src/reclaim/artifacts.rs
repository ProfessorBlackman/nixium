// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Developer build artifacts and package stores. `STO-14`.
//!
//! # Why this is the largest category in the tool
//!
//! On the machine this was developed on, project build artifacts come to **71.3 GiB** across 397
//! directories, and package-manager stores outside `~/.cache` to another **52 GiB** — `.npm/_cacache`
//! at 16 GB, `.gradle/caches` at 15 GB, `pnpm` at 18 GB. Together that is more than every other
//! category in nix combined, and Stacer offered none of it.
//!
//! # Detection is by marker, never by name
//!
//! This is the specification's acceptance criterion and it is not a formality. `build`, `dist`, `venv`
//! and `target` are ordinary English words, and a directory called `build` may perfectly well be
//! hand-written source. Deleting it because of its name would be the worst thing this program could
//! do.
//!
//! So every artifact directory must be corroborated by a **marker** — a file that only the tool which
//! generates that directory would leave behind. `target/` counts only beside a `Cargo.toml`;
//! `build/` only beside a `pubspec.yaml`, `CMakeLists.txt`, `meson.build` or similar. A directory
//! whose marker is absent is not reported at all, not even as a refusal, because there is nothing to
//! suggest it is an artifact.
//!
//! # Nothing here is ever `Safe`
//!
//! Also from the specification: *a directory inside an active project is never rated `Safe`*. Rather
//! than try to decide what "active" means, nothing in this category is ever `Safe` — every entry is
//! `Review` and carries what it costs, which is always some version of "the next build will be slow".
//! Regenerable is not the same as unwanted.
//!
//! # Where the candidates come from
//!
//! Not from a walk. Finding these by traversal costs 33 seconds on this machine even while pruning at
//! every artifact directory, which is far too slow to sit in front of a preview.
//!
//! They come from the **cached scan** instead. `STO-19` bounds that tree by significance, which means
//! it already contains every directory big enough to be worth reclaiming — and a `node_modules` too
//! small to appear in it is too small to care about. Discovery is then a filter over some tens of
//! thousands of nodes plus one `stat` per candidate to confirm its marker, which is milliseconds.
//!
//! The consequence is that this category is **unavailable until a scan has been made**, and says so
//! rather than quietly reporting nothing.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::op::CancelToken;
use crate::space::{Category as SpaceCategory, ReclaimMethod, Reclaimable, Safety};

use super::registry::{Candidate, Category};

/// One recognised kind of build artifact.
struct Artifact {
    /// The directory's name, matched exactly.
    dir: &'static str,
    /// What generated it, for the label.
    tool: &'static str,
    /// Files that must exist **beside** the directory for it to count as an artifact.
    ///
    /// Empty means the directory needs no corroboration because its name is unambiguous and belongs
    /// to exactly one tool — `__pycache__` and `.mypy_cache` are nobody's source directory.
    markers_beside: &'static [&'static str],
    /// Files that must exist **inside** the directory. Used where the marker is internal, as with a
    /// Python virtual environment's `pyvenv.cfg`.
    markers_inside: &'static [&'static str],
    /// What reclaiming it costs. Required: everything here is `Review`.
    cost: &'static str,
}

/// The recognised artifacts.
///
/// Deliberately conservative. `vendor/` is absent because it is committed source in some PHP and Go
/// projects and generated in others, and the name cannot tell you which. `bin/` and `obj/` are absent
/// for the same reason — a `bin` directory holding hand-written scripts is entirely normal.
const ARTIFACTS: &[Artifact] = &[
    Artifact {
        dir: "target",
        tool: "Cargo",
        markers_beside: &["Cargo.toml"],
        markers_inside: &[],
        cost: "The next `cargo build` will recompile the whole dependency tree.",
    },
    Artifact {
        dir: "node_modules",
        tool: "npm, yarn or pnpm",
        markers_beside: &["package.json"],
        markers_inside: &[],
        cost: "Dependencies will need reinstalling before the project runs again.",
    },
    Artifact {
        dir: ".next",
        tool: "Next.js",
        markers_beside: &[
            "next.config.js",
            "next.config.ts",
            "next.config.mjs",
            "package.json",
        ],
        markers_inside: &[],
        cost: "The next build recompiles every route.",
    },
    Artifact {
        dir: ".nuxt",
        tool: "Nuxt",
        markers_beside: &["nuxt.config.js", "nuxt.config.ts", "package.json"],
        markers_inside: &[],
        cost: "The next build regenerates it.",
    },
    Artifact {
        dir: ".dart_tool",
        tool: "Dart",
        markers_beside: &["pubspec.yaml"],
        markers_inside: &[],
        cost: "`pub get` will rebuild it.",
    },
    Artifact {
        dir: "build",
        tool: "a build system",
        // The dangerous name, so the marker list is the whole safety argument. Every entry here is a
        // file that only a build system writes.
        markers_beside: &[
            "pubspec.yaml",
            "CMakeLists.txt",
            "meson.build",
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
            "setup.py",
            "pyproject.toml",
        ],
        markers_inside: &[],
        cost: "The next build regenerates it from source.",
    },
    Artifact {
        dir: "dist",
        tool: "a bundler",
        markers_beside: &[
            "package.json",
            "pyproject.toml",
            "setup.py",
            "rollup.config.js",
            "vite.config.js",
            "vite.config.ts",
        ],
        markers_inside: &[],
        cost: "The next build regenerates it from source.",
    },
    Artifact {
        dir: "_build",
        tool: "Dune, Mix or Rebar",
        markers_beside: &["dune-project", "mix.exs", "rebar.config"],
        markers_inside: &[],
        cost: "The next build regenerates it.",
    },
    Artifact {
        dir: ".venv",
        tool: "a Python virtual environment",
        markers_beside: &[],
        markers_inside: &["pyvenv.cfg"],
        cost: "The environment will need recreating and its packages reinstalling.",
    },
    Artifact {
        dir: "venv",
        tool: "a Python virtual environment",
        markers_beside: &[],
        markers_inside: &["pyvenv.cfg"],
        cost: "The environment will need recreating and its packages reinstalling.",
    },
    Artifact {
        dir: "__pycache__",
        tool: "Python",
        markers_beside: &[],
        markers_inside: &[],
        cost: "Python regenerates it on the next import, at a small one-off cost.",
    },
    Artifact {
        dir: ".mypy_cache",
        tool: "mypy",
        markers_beside: &[],
        markers_inside: &[],
        cost: "The next type-check will be slower.",
    },
    Artifact {
        dir: ".pytest_cache",
        tool: "pytest",
        markers_beside: &[],
        markers_inside: &[],
        cost: "pytest loses its record of the last run.",
    },
    Artifact {
        dir: ".ruff_cache",
        tool: "Ruff",
        markers_beside: &[],
        markers_inside: &[],
        cost: "The next lint will be slower.",
    },
    Artifact {
        dir: ".tox",
        tool: "tox",
        markers_beside: &["tox.ini"],
        markers_inside: &[],
        cost: "tox will rebuild each environment on the next run.",
    },
];

/// The cached scan of the home directory, if there is one.
///
/// **Load this once per `candidates()` call, never once per candidate.** Each load deserialises the
/// whole tree — some tens of thousands of nodes — and calling it per candidate made a preview *slower*
/// than the traversal it was introduced to avoid: 7.5 s became 11 s across fifty-eight lookups.
pub(super) fn cached_home_scan() -> Option<crate::cache::CachedScan> {
    let home = crate::paths::home_dir()?;
    crate::cache::Cache::discover().ok()?.load(&home)
}

/// A directory's size according to an already-loaded cached scan.
///
/// The cache is the only affordable source for a figure like this during a preview. It can be stale,
/// and that is acceptable here for a specific reason: the preview's number is an *estimate* the user
/// decides against, while the number in the report comes from measuring what was actually moved. A
/// stale estimate leads to a slightly wrong list, never a wrong account of what happened.
pub(super) fn cached_size_in(
    cached: Option<&crate::cache::CachedScan>,
    path: &Path,
) -> Option<u64> {
    let id = crate::space::EntryId::for_path(path);
    cached?.result.tree.get(id).map(|e| e.allocated)
}

/// Which artifact definition, if any, a directory name corresponds to.
fn definition_for(name: &str) -> Option<&'static Artifact> {
    ARTIFACTS.iter().find(|a| a.dir == name)
}

/// Whether a directory is corroborated as an artifact by a marker on disk.
///
/// Returns the marker that vouched for it, so the candidate can say *why* nix believes this is
/// generated rather than written.
pub(crate) fn corroborate(path: &Path) -> Option<(&'static str, &'static str)> {
    let name = path.file_name()?.to_str()?;
    let definition = definition_for(name)?;

    if definition.markers_beside.is_empty() && definition.markers_inside.is_empty() {
        // A name that belongs to exactly one tool and to no plausible source layout.
        return Some((definition.tool, "its name, which no source layout uses"));
    }

    let parent = path.parent()?;
    for marker in definition.markers_beside {
        if parent.join(marker).is_file() {
            return Some((definition.tool, marker));
        }
    }
    for marker in definition.markers_inside {
        if path.join(marker).is_file() {
            return Some((definition.tool, marker));
        }
    }
    None
}

/// A package-manager store that lives outside the user's cache directory.
struct Store {
    /// Path relative to the home directory.
    relative: &'static str,
    tool: &'static str,
    cost: &'static str,
}

/// Stores worth offering, all outside `$XDG_CACHE_HOME`.
///
/// Anything *inside* the cache directory belongs to [`super::AppCacheCategory`], which enumerates that
/// directory's children. Listing a path here that also lives there would have both categories propose
/// it and the preview would count it twice — so [`STORES`] is checked against the cache directory in a
/// test rather than by inspection.
const STORES: &[Store] = &[
    Store {
        relative: ".npm/_cacache",
        tool: "npm",
        cost: "npm will re-download packages it had kept locally.",
    },
    Store {
        relative: ".cargo/registry/cache",
        tool: "Cargo",
        cost: "Cargo will re-download crate archives.",
    },
    Store {
        relative: ".cargo/registry/src",
        tool: "Cargo",
        cost: "Cargo will re-extract crate sources from its archives, or re-download them.",
    },
    Store {
        relative: ".cargo/git/db",
        tool: "Cargo",
        cost: "Cargo will re-clone git dependencies.",
    },
    Store {
        relative: ".gradle/caches",
        tool: "Gradle",
        cost: "Gradle will re-download dependencies and rebuild its caches.",
    },
    Store {
        relative: ".m2/repository",
        tool: "Maven",
        cost: "Maven will re-download every dependency it had cached.",
    },
    Store {
        relative: "go/pkg/mod",
        tool: "Go",
        cost: "Go will re-download modules. Note it makes them read-only, so removal may need care.",
    },
    Store {
        relative: ".local/share/pnpm/store",
        tool: "pnpm",
        cost: "pnpm will re-download packages. Projects using its store will need reinstalling.",
    },
];

/// Build artifacts inside projects, discovered from the cached scan.
#[derive(Debug, Default)]
pub struct BuildArtifactCategory;

impl BuildArtifactCategory {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// The cached scan to draw candidates from, if there is one.
    fn cached_tree() -> Option<crate::cache::CachedScan> {
        cached_home_scan()
    }
}

impl Category for BuildArtifactCategory {
    fn id(&self) -> &'static str {
        "build_artifacts"
    }

    fn label(&self) -> &'static str {
        "Build artifacts"
    }

    fn explains(&self) -> &'static str {
        "Compiler output in project directories — target, node_modules, build. Regenerated by rebuilding, which for a large project is minutes rather than seconds and needs whatever the build downloads. Source files and version control are never touched."
    }

    fn space_category(&self) -> SpaceCategory {
        SpaceCategory::BuildArtifact
    }

    fn available(&self) -> bool {
        Self::cached_tree().is_some()
    }

    fn candidates(&self, token: &CancelToken) -> Result<Vec<Candidate>> {
        token.check()?;
        let Some(cached) = Self::cached_tree() else {
            return Ok(Vec::new());
        };

        let mut candidates = Vec::new();
        for entry in cached.result.tree.entries.values() {
            token.check()?;
            if !entry.is_dir {
                continue;
            }
            let Some(path) = entry.path.as_ref() else {
                continue;
            };
            // Still there, and still corroborated. The cache may be days old.
            if !path.is_dir() {
                continue;
            }
            let Some((tool, marker)) = corroborate(path) else {
                continue;
            };

            candidates.push(Candidate {
                path: path.clone(),
                label: format!(
                    "{} in {}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    path.parent()
                        .and_then(Path::file_name)
                        .unwrap_or_default()
                        .to_string_lossy()
                ),
                bytes: entry.allocated,
                // Never `Safe`, per the specification: regenerable is not the same as unwanted.
                safety: Safety::Review,
                method: ReclaimMethod::MoveToTrash { path: path.clone() },
                cost: Some(format!(
                    "Generated by {tool}, recognised by {marker}. {}",
                    definition_for(path.file_name().and_then(|n| n.to_str()).unwrap_or(""))
                        .map_or("", |d| d.cost)
                )),
                category: self.id().to_string(),
                reclaimable: Reclaimable::Exact,
            });
        }

        // Largest first: the decision is about where the space is.
        candidates.sort_by_key(|c| std::cmp::Reverse(c.bytes));
        Ok(candidates)
    }
}

/// Package-manager stores outside the user's cache directory.
#[derive(Debug, Default)]
pub struct PackageStoreCategory {
    /// Overridable for tests.
    home: Option<PathBuf>,
}

impl PackageStoreCategory {
    #[must_use]
    pub const fn new() -> Self {
        Self { home: None }
    }

    #[cfg(test)]
    fn rooted_at(home: PathBuf) -> Self {
        Self { home: Some(home) }
    }

    fn home(&self) -> Option<PathBuf> {
        self.home.clone().or_else(crate::paths::home_dir)
    }
}

impl Category for PackageStoreCategory {
    fn id(&self) -> &'static str {
        "package_stores"
    }

    fn label(&self) -> &'static str {
        "Package manager stores"
    }

    fn explains(&self) -> &'static str {
        "Downloaded packages and metadata kept by pnpm, npm, cargo and pip so a second install is instant. Refetched on the next install, which needs a network connection."
    }

    fn space_category(&self) -> SpaceCategory {
        SpaceCategory::PackageCache
    }

    fn available(&self) -> bool {
        self.home().is_some()
    }

    fn candidates(&self, token: &CancelToken) -> Result<Vec<Candidate>> {
        token.check()?;
        let Some(home) = self.home() else {
            return Ok(Vec::new());
        };

        let cached = cached_home_scan();

        let mut candidates = Vec::new();
        for store in STORES {
            token.check()?;
            let path = home.join(store.relative);
            if !path.is_dir() {
                continue;
            }
            // From the cached scan when it knows, because walking these is expensive: the three
            // largest stores on the development machine hold 46 GiB between them and measuring them
            // by traversal took 27 of the preview's 28 seconds. They are far too big to fall below
            // `STO-19`'s significance threshold, so the tree almost always has them.
            let bytes = cached_size_in(cached.as_ref(), &path)
                .unwrap_or_else(|| crate::fixture::directory_size(&path));
            if bytes == 0 {
                continue;
            }

            candidates.push(Candidate {
                path: path.clone(),
                label: format!("{} store", store.tool),
                bytes,
                safety: Safety::Review,
                method: ReclaimMethod::MoveToTrash { path },
                cost: Some(store.cost.to_string()),
                category: self.id().to_string(),
                reclaimable: Reclaimable::Exact,
            });
        }

        candidates.sort_by_key(|c| std::cmp::Reverse(c.bytes));
        Ok(candidates)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sandbox(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nix-artifacts-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The acceptance criterion, and the whole safety argument for this category.
    #[test]
    fn a_directory_is_never_recognised_by_name_alone() {
        let base = sandbox("byname");
        let project = base.join("not-a-project");
        std::fs::create_dir_all(project.join("build")).unwrap();
        std::fs::create_dir_all(project.join("target")).unwrap();
        std::fs::create_dir_all(project.join("dist")).unwrap();
        std::fs::create_dir_all(project.join("node_modules")).unwrap();

        for name in ["build", "target", "dist", "node_modules"] {
            assert!(
                corroborate(&project.join(name)).is_none(),
                "{name} has no marker beside it, so nothing suggests it is generated"
            );
        }

        std::fs::remove_dir_all(&base).ok();
    }

    /// `build` is the dangerous case: an ordinary word that may well be hand-written source.
    #[test]
    fn build_needs_a_build_system_beside_it() {
        let base = sandbox("build");
        let project = base.join("proj");
        std::fs::create_dir_all(project.join("build")).unwrap();

        assert!(corroborate(&project.join("build")).is_none());

        // A README is not a build system.
        std::fs::write(project.join("README.md"), b"docs").unwrap();
        assert!(
            corroborate(&project.join("build")).is_none(),
            "an unrelated file must not vouch for it"
        );

        std::fs::write(project.join("pubspec.yaml"), b"name: app").unwrap();
        let (tool, marker) = corroborate(&project.join("build")).unwrap();
        assert_eq!(marker, "pubspec.yaml");
        assert!(!tool.is_empty());

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn each_artifact_is_recognised_by_its_own_marker() {
        let base = sandbox("markers");
        for (dir, marker) in [
            ("target", "Cargo.toml"),
            ("node_modules", "package.json"),
            (".next", "next.config.ts"),
            (".dart_tool", "pubspec.yaml"),
            ("_build", "mix.exs"),
            (".tox", "tox.ini"),
            ("dist", "vite.config.ts"),
        ] {
            let project = base.join(format!("p-{dir}"));
            std::fs::create_dir_all(project.join(dir)).unwrap();
            assert!(
                corroborate(&project.join(dir)).is_none(),
                "{dir} should need {marker}"
            );
            std::fs::write(project.join(marker), b"x").unwrap();
            let (_, found) = corroborate(&project.join(dir))
                .unwrap_or_else(|| panic!("{dir} should be recognised by {marker}"));
            assert_eq!(found, marker);
        }
        std::fs::remove_dir_all(&base).ok();
    }

    /// A virtual environment's marker is inside it, not beside it.
    #[test]
    fn a_virtualenv_is_recognised_from_within() {
        let base = sandbox("venv");
        let venv = base.join(".venv");
        std::fs::create_dir_all(&venv).unwrap();
        assert!(corroborate(&venv).is_none(), "an empty .venv is not proven");

        std::fs::write(venv.join("pyvenv.cfg"), b"home = /usr").unwrap();
        let (_, marker) = corroborate(&venv).unwrap();
        assert_eq!(marker, "pyvenv.cfg");

        std::fs::remove_dir_all(&base).ok();
    }

    /// Names that belong to exactly one tool need no corroboration — but they must be *those* names.
    #[test]
    fn unambiguous_names_stand_alone_and_others_do_not() {
        let base = sandbox("unambiguous");
        for name in ["__pycache__", ".mypy_cache", ".pytest_cache", ".ruff_cache"] {
            let d = base.join(name);
            std::fs::create_dir_all(&d).unwrap();
            assert!(
                corroborate(&d).is_some(),
                "{name} is nobody's source directory"
            );
        }
        for name in ["src", "lib", "bin", "obj", "vendor", "assets", "buildings"] {
            let d = base.join(name);
            std::fs::create_dir_all(&d).unwrap();
            assert!(
                corroborate(&d).is_none(),
                "{name} must not be treated as an artifact"
            );
        }
        std::fs::remove_dir_all(&base).ok();
    }

    /// Matching is on the whole name, so a directory merely starting with one does not qualify.
    #[test]
    fn names_are_matched_whole() {
        let base = sandbox("whole");
        for name in [
            "target-old",
            "node_modules.bak",
            "__pycache__2",
            "distribution",
        ] {
            let d = base.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(base.join("Cargo.toml"), b"x").unwrap();
            std::fs::write(base.join("package.json"), b"{}").unwrap();
            assert!(
                corroborate(&d).is_none(),
                "{name} is not an artifact directory"
            );
        }
        std::fs::remove_dir_all(&base).ok();
    }

    /// Nothing in this category may be `Safe`, per the specification.
    #[test]
    fn nothing_is_ever_rated_safe() {
        let token = CancelToken::new();
        for candidates in [
            BuildArtifactCategory::new().candidates(&token),
            PackageStoreCategory::new().candidates(&token),
        ] {
            for c in candidates.unwrap_or_default() {
                assert_eq!(
                    c.safety,
                    Safety::Review,
                    "{} must not be pre-checkable: regenerable is not unwanted",
                    c.label
                );
                assert!(
                    c.cost.as_deref().is_some_and(|s| !s.is_empty()),
                    "{} offers no reason for its Review rating",
                    c.label
                );
            }
        }
    }

    /// # Double counting
    ///
    /// `AppCacheCategory` enumerates the children of `$XDG_CACHE_HOME`. A store listed here that also
    /// lives there would be proposed by both categories and counted twice in the preview total.
    #[test]
    fn no_store_overlaps_the_application_cache_directory() {
        for store in STORES {
            let relative = Path::new(store.relative);
            assert!(
                !relative.starts_with(".cache"),
                "{} belongs to AppCacheCategory and would be counted twice",
                store.relative
            );
        }
    }

    #[test]
    fn stores_are_found_under_the_given_home() {
        let home = sandbox("stores");
        let npm = home.join(".npm/_cacache");
        std::fs::create_dir_all(&npm).unwrap();
        std::fs::write(npm.join("blob"), vec![b'x'; 8192]).unwrap();

        let found = PackageStoreCategory::rooted_at(home.clone())
            .candidates(&CancelToken::new())
            .unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].bytes >= 8192);
        assert_eq!(found[0].label, "npm store");

        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn an_empty_store_is_not_offered() {
        let home = sandbox("emptystore");
        std::fs::create_dir_all(home.join(".m2/repository")).unwrap();
        let found = PackageStoreCategory::rooted_at(home.clone())
            .candidates(&CancelToken::new())
            .unwrap();
        assert!(found.is_empty(), "nothing to reclaim from an empty store");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn cancellation_is_honoured() {
        let cancelled = CancelToken::new();
        cancelled.cancel();
        for result in [
            BuildArtifactCategory::new().candidates(&cancelled),
            PackageStoreCategory::new().candidates(&cancelled),
        ] {
            match result {
                Err(e) => assert!(!e.is_fault(), "cancellation is not a fault"),
                Ok(found) => assert!(found.is_empty()),
            }
        }
    }

    #[test]
    fn artifact_definitions_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for a in ARTIFACTS {
            assert!(seen.insert(a.dir), "{} is defined twice", a.dir);
            assert!(!a.cost.is_empty(), "{} must say what it costs", a.dir);
        }
        let mut store_paths = std::collections::HashSet::new();
        for s in STORES {
            assert!(
                store_paths.insert(s.relative),
                "{} is listed twice",
                s.relative
            );
        }
    }
}
