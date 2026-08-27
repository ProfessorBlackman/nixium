// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Application caches. Task 1.11 (`STO-5`).
//!
//! # Attribution, not directory listing
//!
//! The specification's acceptance criterion is that **the top ten cache consumers are named
//! applications, not opaque directory names**. `~/.cache/mozilla` means nothing to most people;
//! "Firefox" does. So each directory is mapped to the application that owns it, and where nix does
//! not recognise a directory it says so rather than inventing a name.
//!
//! # Why most of these are `Review` rather than `Safe`
//!
//! The specification defines `Safe` as regenerable *with no user-visible loss*, and `Review` as
//! carrying a cost — "a slower next launch" is its own example. An application cache is exactly
//! that: nothing is lost, but the next start is slower and assets are re-downloaded. So caches are
//! `Review` with the cost stated, and only genuinely invisible things — thumbnails, which regenerate
//! on demand — are `Safe`.
//!
//! Rating a browser cache `Safe` would pre-check it in a quick clean, which decides on someone's
//! behalf that a slower launch and re-downloaded assets do not matter to them.

use std::path::PathBuf;

use crate::error::Result;
use crate::op::CancelToken;
use crate::space::{Category as SpaceCategory, ReclaimMethod, Safety};

use super::registry::{Candidate, Category};

/// Directory name inside the cache root, the application it belongs to, and what clearing it costs.
struct Known {
    dir: &'static str,
    app: &'static str,
    cost: &'static str,
}

/// Applications nix can name. Everything else is reported by its directory name, honestly labelled
/// as unrecognised.
const KNOWN: &[Known] = &[
    Known {
        dir: "mozilla",
        app: "Firefox",
        cost: "Firefox will re-download cached pages and assets.",
    },
    Known {
        dir: "google-chrome",
        app: "Chrome",
        cost: "Chrome will re-download cached pages and assets.",
    },
    Known {
        dir: "chromium",
        app: "Chromium",
        cost: "Chromium will re-download cached pages and assets.",
    },
    Known {
        dir: "BraveSoftware",
        app: "Brave",
        cost: "Brave will re-download cached pages and assets.",
    },
    Known {
        dir: "microsoft-edge",
        app: "Edge",
        cost: "Edge will re-download cached pages and assets.",
    },
    Known {
        dir: "vivaldi",
        app: "Vivaldi",
        cost: "Vivaldi will re-download cached pages and assets.",
    },
    Known {
        dir: "thumbnails",
        app: "Image thumbnails",
        cost: "Thumbnails regenerate as you browse folders.",
    },
    Known {
        dir: "fontconfig",
        app: "Font cache",
        cost: "The first application to start afterwards will rebuild it.",
    },
    Known {
        dir: "mesa_shader_cache",
        app: "Graphics shader cache",
        cost: "Games and 3D applications will recompile shaders on first run, which stutters briefly.",
    },
    Known {
        dir: "mesa_shader_cache_db",
        app: "Graphics shader cache",
        cost: "Games and 3D applications will recompile shaders on first run, which stutters briefly.",
    },
    Known {
        dir: "nvidia",
        app: "NVIDIA shader cache",
        cost: "Shaders will be recompiled on first run.",
    },
    Known {
        dir: "pip",
        app: "pip",
        cost: "Python packages will be downloaded again on next install.",
    },
    Known {
        dir: "npm",
        app: "npm",
        cost: "Node packages will be downloaded again on next install.",
    },
    Known {
        dir: "yarn",
        app: "Yarn",
        cost: "Node packages will be downloaded again on next install.",
    },
    Known {
        dir: "pnpm",
        app: "pnpm",
        cost: "Node packages will be downloaded again on next install.",
    },
    Known {
        dir: "go-build",
        app: "Go build cache",
        cost: "The next Go build will be slower.",
    },
    Known {
        dir: "sccache",
        app: "sccache",
        cost: "The next compile will be slower.",
    },
    Known {
        dir: "JetBrains",
        app: "JetBrains IDEs",
        cost: "Indexes will be rebuilt when you next open a project.",
    },
    Known {
        dir: "Code",
        app: "Visual Studio Code",
        cost: "VS Code will rebuild its caches on next start.",
    },
    Known {
        dir: "spotify",
        app: "Spotify",
        cost: "Spotify will re-download cached tracks.",
    },
    Known {
        dir: "gstreamer-1.0",
        app: "GStreamer",
        cost: "Media plugins will be rescanned once.",
    },
    Known {
        dir: "tracker3",
        app: "File indexer",
        cost: "The file index will be rebuilt in the background.",
    },
    Known {
        dir: "winetricks",
        app: "Winetricks",
        cost: "Downloads will be fetched again if needed.",
    },
    Known {
        dir: "flatpak",
        app: "Flatpak",
        cost: "Flatpak will re-fetch metadata.",
    },
    Known {
        dir: "snapd",
        app: "Snap",
        cost: "Snap will re-fetch metadata.",
    },
    Known {
        dir: "cargo",
        app: "Cargo",
        cost: "Crates will be downloaded again on next build.",
    },
    Known {
        dir: "composer",
        app: "Composer",
        cost: "PHP packages will be downloaded again.",
    },
    Known {
        dir: "ms-playwright",
        app: "Playwright browsers",
        cost: "Test browsers will be downloaded again.",
    },
    Known {
        dir: "puppeteer",
        app: "Puppeteer browsers",
        cost: "Test browsers will be downloaded again.",
    },
];

/// Directories whose loss is genuinely invisible, so they may be pre-checked.
const INVISIBLE: &[&str] = &["thumbnails", "fontconfig"];

/// Reclaimable application caches under the user's cache directory.
pub struct AppCacheCategory {
    /// Overridable for tests.
    root: Option<PathBuf>,
}

impl AppCacheCategory {
    #[must_use]
    pub fn new() -> Self {
        Self { root: None }
    }

    /// A category over an explicit cache root.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Some(root.into()),
        }
    }

    fn cache_root(&self) -> Option<PathBuf> {
        self.root.clone().or_else(|| {
            // `$XDG_CACHE_HOME`, defaulting to `~/.cache`. Note this is the *user's* cache root, not
            // nix's own subdirectory of it — which the protection rules exclude separately.
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .or_else(|| crate::paths::home_dir().map(|h| h.join(".cache")))
        })
    }

    /// Describe one cache directory.
    fn describe(name: &str) -> (String, Option<&'static str>, Safety) {
        match KNOWN.iter().find(|k| k.dir == name) {
            Some(known) => (
                known.app.to_string(),
                Some(known.cost),
                if INVISIBLE.contains(&name) {
                    Safety::Safe
                } else {
                    Safety::Review
                },
            ),
            // Honest about not knowing, rather than inventing a friendly name.
            None => (
                format!("{name} (unrecognised)"),
                Some("nix does not recognise this cache, so it cannot say what clearing it costs."),
                Safety::Review,
            ),
        }
    }
}

impl Default for AppCacheCategory {
    fn default() -> Self {
        Self::new()
    }
}

impl Category for AppCacheCategory {
    fn id(&self) -> &'static str {
        "app_cache"
    }

    fn label(&self) -> &'static str {
        "Application caches"
    }

    fn space_category(&self) -> SpaceCategory {
        SpaceCategory::AppCache
    }

    fn available(&self) -> bool {
        self.cache_root().is_some_and(|r| r.is_dir())
    }

    fn candidates(&self, token: &CancelToken) -> Result<Vec<Candidate>> {
        let Some(root) = self.cache_root() else {
            return Ok(Vec::new());
        };
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Ok(Vec::new());
        };

        let mut candidates = Vec::new();
        for entry in entries.filter_map(std::result::Result::ok) {
            token.check()?;

            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();

            // Only directories: a stray file in the cache root is not an application's cache, and
            // guessing would be worse than skipping it.
            if !entry.metadata().is_ok_and(|m| m.is_dir()) {
                continue;
            }
            // A symlink out of the cache root points somewhere nix has not reasoned about.
            if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
                continue;
            }

            let bytes = crate::fixture::directory_size(&path);
            // Nothing to offer, and an entry claiming zero bytes is just noise in the list.
            if bytes == 0 {
                continue;
            }

            let (label, cost, safety) = Self::describe(&name);
            candidates.push(Candidate {
                label,
                bytes,
                safety,
                // The user's own cache, so it goes to the trash: reversible, and it costs nothing
                // to offer that.
                method: ReclaimMethod::MoveToTrash { path: path.clone() },
                cost: cost.map(str::to_string),
                category: self.id().to_string(),
                path,
            });
        }

        Ok(candidates)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    struct Fake {
        root: PathBuf,
    }

    impl Fake {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "nix-cachecat-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn dir(&self, name: &str, bytes: usize) -> PathBuf {
            let path = self.root.join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("blob"), vec![b'x'; bytes]).unwrap();
            path
        }

        fn category(&self) -> AppCacheCategory {
            AppCacheCategory::at(&self.root)
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    /// The specification's acceptance criterion for this task.
    #[test]
    fn caches_are_named_by_application_not_by_directory() {
        let fake = Fake::new("names");
        fake.dir("mozilla", 8192);
        fake.dir("google-chrome", 8192);
        fake.dir("thumbnails", 4096);

        let found = fake.category().candidates(&CancelToken::new()).unwrap();
        let labels: Vec<&str> = found.iter().map(|c| c.label.as_str()).collect();

        assert!(labels.contains(&"Firefox"), "{labels:?}");
        assert!(labels.contains(&"Chrome"), "{labels:?}");
        assert!(
            !labels.contains(&"mozilla"),
            "a directory name is not a name: {labels:?}"
        );
    }

    #[test]
    fn an_unrecognised_cache_says_so_rather_than_inventing_a_name() {
        let fake = Fake::new("unknown");
        fake.dir("some-obscure-app", 4096);

        let found = fake.category().candidates(&CancelToken::new()).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].label.contains("some-obscure-app"));
        assert!(
            found[0].label.contains("unrecognised"),
            "{}",
            found[0].label
        );
        assert!(
            found[0]
                .cost
                .as_deref()
                .is_some_and(|c| c.contains("does not recognise")),
            "an unknown cache must be honest about the unknown cost"
        );
    }

    /// The rating rule from the specification: a slower next launch is a cost, so it is `Review`.
    #[test]
    fn caches_with_a_cost_are_review_and_only_invisible_ones_are_safe() {
        let fake = Fake::new("ratings");
        fake.dir("mozilla", 4096);
        fake.dir("thumbnails", 4096);
        fake.dir("fontconfig", 4096);

        let found = fake.category().candidates(&CancelToken::new()).unwrap();
        let by_label = |name: &str| found.iter().find(|c| c.label == name).unwrap();

        assert_eq!(
            by_label("Firefox").safety,
            Safety::Review,
            "a browser cache costs a slower launch and re-downloads, so it is never pre-checked"
        );
        assert_eq!(by_label("Image thumbnails").safety, Safety::Safe);
        assert_eq!(by_label("Font cache").safety, Safety::Safe);
    }

    #[test]
    fn every_candidate_states_a_cost() {
        let fake = Fake::new("costs");
        fake.dir("mozilla", 4096);
        fake.dir("pip", 4096);
        fake.dir("mystery", 4096);

        for candidate in fake.category().candidates(&CancelToken::new()).unwrap() {
            let cost = candidate.cost.as_deref().unwrap_or("");
            assert!(!cost.is_empty(), "{} has no stated cost", candidate.label);
            assert!(
                cost.ends_with('.'),
                "costs are shown to users, so they read as sentences: {cost:?}"
            );
        }
    }

    #[test]
    fn caches_are_trashed_rather_than_unlinked() {
        let fake = Fake::new("method");
        fake.dir("mozilla", 4096);

        let found = fake.category().candidates(&CancelToken::new()).unwrap();
        match &found[0].method {
            ReclaimMethod::MoveToTrash { .. } => {}
            other => panic!("a user's own cache should be recoverable, got {other:?}"),
        }
        assert!(!found[0].method.is_irreversible());
    }

    #[test]
    fn empty_directories_and_stray_files_are_not_offered() {
        let fake = Fake::new("empty");
        std::fs::create_dir_all(fake.root.join("empty-cache")).unwrap();
        std::fs::write(fake.root.join("stray-file"), b"not a cache").unwrap();
        fake.dir("mozilla", 4096);

        let found = fake.category().candidates(&CancelToken::new()).unwrap();
        assert_eq!(found.len(), 1, "only the non-empty directory: {found:?}");
        assert_eq!(found[0].label, "Firefox");
    }

    #[test]
    fn a_symlinked_cache_directory_is_skipped() {
        let fake = Fake::new("symlink");
        let real = fake.dir("mozilla", 4096);
        std::os::unix::fs::symlink(&real, fake.root.join("linked")).unwrap();

        let found = fake.category().candidates(&CancelToken::new()).unwrap();
        assert_eq!(
            found.len(),
            1,
            "a symlink points somewhere nix has not reasoned about: {found:?}"
        );
    }

    #[test]
    fn sizes_are_measured_not_guessed() {
        let fake = Fake::new("size");
        fake.dir("mozilla", 40960);
        let found = fake.category().candidates(&CancelToken::new()).unwrap();
        assert!(found[0].bytes >= 40960, "measured {}", found[0].bytes);
    }

    #[test]
    fn cancellation_is_honoured() {
        let fake = Fake::new("cancel");
        for i in 0..5 {
            fake.dir(&format!("app-{i}"), 4096);
        }
        let token = CancelToken::new();
        token.cancel();
        assert!(fake.category().candidates(&token).is_err());
    }

    #[test]
    fn an_absent_cache_root_reports_unavailable_rather_than_empty() {
        let category = AppCacheCategory::at("/definitely/not/here");
        assert!(!category.available());
        assert!(category.candidates(&CancelToken::new()).unwrap().is_empty());
    }

    #[test]
    fn known_applications_are_distinct_and_every_one_explains_its_cost() {
        let mut seen = std::collections::HashSet::new();
        for known in KNOWN {
            assert!(seen.insert(known.dir), "duplicate directory {}", known.dir);
            assert!(!known.app.is_empty());
            assert!(known.cost.ends_with('.'), "{:?}", known.cost);
        }
        // Every directory listed as invisible must actually be a known application.
        for invisible in INVISIBLE {
            assert!(
                KNOWN.iter().any(|k| k.dir == *invisible),
                "{invisible} is rated Safe but is not a known cache"
            );
        }
    }
}
