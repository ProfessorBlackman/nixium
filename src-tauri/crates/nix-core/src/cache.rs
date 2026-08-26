//! Scan persistence. Task 1.4, implementing decision D6.
//!
//! # Why this exists
//!
//! D6 settled the explorer's behaviour as **cached-first, on-demand refresh, incremental**. Strict
//! on-demand scanning means a wait on every visit, which is the thing that makes disk tools
//! annoying; a background indexer means a daemon, which the non-goals rule out. Persisting the last
//! scan gets most of indexing's felt speed with neither: after the first scan the view is never
//! empty again, it opens on the previous result labelled with its age, and refresh is offered.
//!
//! # Why stale data can be browsed but never acted on
//!
//! A cached tree is a *display convenience*. The rule, from D6, is: **you may browse stale data, you
//! may never reclaim from it.** The executor re-stats every path in a preview immediately before
//! acting — which time-of-check/time-of-use safety requires anyway — so a stale cache can misinform
//! a reader but can never misdirect a deletion. That is what makes serving old data acceptable here
//! and would not make it acceptable anywhere else.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, Cause, ErrorCode, IoContext, Result};
use crate::paths;
use crate::scan::{Options, ScanResult};

/// Format version of a cache entry. A mismatch is a miss, not an error: a cache is by definition
/// disposable, so an unreadable one costs a rescan and nothing else.
const CACHE_VERSION: u32 = 1;

/// One persisted scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CachedScan {
    version: u32,
    /// When the scan finished, in seconds since the Unix epoch.
    #[ts(type = "number")]
    pub scanned_at: i64,
    /// What was scanned.
    pub root: PathBuf,
    /// The depth cap in force, so a shallower cached tree is not served for a deeper request.
    #[ts(type = "number | null")]
    pub max_depth: Option<usize>,
    /// Whether the scan crossed filesystem boundaries.
    pub cross_filesystems: bool,
    /// The result itself.
    pub result: ScanResult,
}

impl CachedScan {
    /// Age in seconds, or `None` if the clock has moved backwards since the scan.
    ///
    /// Returning `None` rather than clamping to zero is deliberate: a negative age means something
    /// is wrong with the clock, and quietly presenting it as "just now" would hide that.
    #[must_use]
    pub fn age_seconds(&self) -> Option<i64> {
        let now = now_epoch();
        (now >= self.scanned_at).then(|| now - self.scanned_at)
    }

    /// Whether this entry can serve a request for the given options.
    ///
    /// A cached tree capped at depth 2 cannot answer a request for depth 4 — it would silently show
    /// less than was asked for. The reverse is fine.
    #[must_use]
    pub fn satisfies(&self, options: &Options) -> bool {
        if self.root != options.root || self.cross_filesystems != options.cross_filesystems {
            return false;
        }
        match (self.max_depth, options.max_depth) {
            // Cached is unlimited: it can answer anything.
            (None, _) => true,
            // Request is unlimited but the cache is capped: not good enough.
            (Some(_), None) => false,
            (Some(cached), Some(wanted)) => cached >= wanted,
        }
    }
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// FNV-1a of a path, used as a filename. Stable across processes, and never contains a separator.
fn key_for(root: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in root.as_os_str().as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Reads and writes persisted scans under a directory.
#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// A cache rooted at an explicit directory.
    #[must_use]
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The cache under `$XDG_CACHE_HOME/nix/scans`.
    pub fn discover() -> Result<Self> {
        let dir = paths::cache_dir().ok_or_else(|| {
            AppError::new(
                ErrorCode::Unsupported,
                "Could not work out where to keep the scan cache.",
            )
            .with_remedy("Set HOME or XDG_CACHE_HOME and try again.")
        })?;
        Ok(Self::at(dir.join("scans")))
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, root: &Path) -> PathBuf {
        self.dir.join(format!("{}.json", key_for(root)))
    }

    /// Persist a result. Never fails the caller's operation — a cache that cannot be written is a
    /// missed optimisation, not a failed scan, so the error is returned for logging and the scan
    /// still stands.
    pub fn store(&self, options: &Options, result: &ScanResult) -> Result<()> {
        // A cancelled scan holds a partial tree. Caching it would mean opening on an
        // understated total that looks authoritative, so it is not stored.
        if result.cancelled {
            return Ok(());
        }

        std::fs::create_dir_all(&self.dir)
            .doing("create the scan cache directory")
            .map_err(|e| e.with_path(&self.dir))?;

        let entry = CachedScan {
            version: CACHE_VERSION,
            scanned_at: now_epoch(),
            root: options.root.clone(),
            max_depth: options.max_depth,
            cross_filesystems: options.cross_filesystems,
            result: result.clone(),
        };

        let json = serde_json::to_vec(&entry).map_err(|e| {
            AppError::internal("Could not encode a scan for the cache.").with_cause(Cause::Other {
                detail: e.to_string(),
            })
        })?;

        // Same atomic pattern as the settings store: a sibling temporary, then rename.
        let target = self.path_for(&options.root);
        let tmp = target.with_extension(format!("json.tmp.{}", std::process::id()));
        std::fs::write(&tmp, &json)
            .doing("write the scan cache")
            .map_err(|e| e.with_path(&tmp))?;
        std::fs::rename(&tmp, &target).map_err(|e| {
            std::fs::remove_file(&tmp).ok();
            AppError::from_io(&e, "save the scan cache").with_path(&target)
        })
    }

    /// Load the cached scan for a root, if there is a usable one.
    ///
    /// A missing, unreadable, malformed or version-mismatched entry is a **miss**, not an error: the
    /// only cost is a rescan.
    #[must_use]
    pub fn load(&self, root: &Path) -> Option<CachedScan> {
        let path = self.path_for(root);
        let raw = std::fs::read(&path).ok()?;
        let entry: CachedScan = serde_json::from_slice(&raw).ok()?;
        if entry.version != CACHE_VERSION {
            tracing::debug!(
                path = %path.display(),
                found = entry.version,
                expected = CACHE_VERSION,
                "ignoring a scan cache entry from another version"
            );
            return None;
        }
        // A cache file for one root should never contain another; if it does, the key derivation is
        // broken and serving it would show the wrong tree.
        if entry.root != root {
            tracing::warn!(
                expected = %root.display(),
                found = %entry.root.display(),
                "scan cache key collision, ignoring the entry"
            );
            return None;
        }
        Some(entry)
    }

    /// Load only if the entry can answer the given options.
    #[must_use]
    pub fn load_for(&self, options: &Options) -> Option<CachedScan> {
        self.load(&options.root).filter(|e| e.satisfies(options))
    }

    /// Forget one root's cached scan.
    pub fn forget(&self, root: &Path) -> Result<()> {
        let path = self.path_for(root);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AppError::from_io(&e, "clear the scan cache").with_path(&path)),
        }
    }

    /// Forget everything. Offered in settings, because a cache the user cannot clear is a cache
    /// they cannot trust.
    pub fn clear(&self) -> Result<()> {
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AppError::from_io(&e, "clear the scan cache").with_path(&self.dir)),
        }
    }

    /// Total bytes the cache occupies. nix should be able to answer this about itself.
    #[must_use]
    pub fn size_on_disk(&self) -> u64 {
        std::fs::read_dir(&self.dir)
            .map(|entries| {
                entries
                    .filter_map(std::result::Result::ok)
                    .filter_map(|e| e.metadata().ok())
                    .map(|m| m.len())
                    .sum()
            })
            .unwrap_or(0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::fixture::{Fixture, Spec};
    use crate::op::CancelToken;
    use crate::scan;

    fn tmp_cache(tag: &str) -> Cache {
        Cache::at(std::env::temp_dir().join(format!("nix-cache-{tag}-{}", std::process::id())))
    }

    fn small_scan() -> (Fixture, Options, ScanResult) {
        let fx = Fixture::create(&Spec {
            breadth: 2,
            depth: 1,
            files_per_dir: 3,
            ..Spec::default()
        })
        .unwrap();
        let options = Options::new(fx.root()).max_depth(None);
        let result = scan::scan_quiet(options.clone(), CancelToken::new()).unwrap();
        (fx, options, result)
    }

    #[test]
    fn round_trips_a_scan() {
        let cache = tmp_cache("round");
        let (_fx, options, result) = small_scan();

        cache.store(&options, &result).unwrap();
        let loaded = cache.load(&options.root).expect("a stored scan must load");

        assert_eq!(loaded.result, result);
        assert_eq!(loaded.root, options.root);
        assert_eq!(loaded.max_depth, None);

        cache.clear().unwrap();
    }

    #[test]
    fn a_missing_entry_is_a_miss_not_an_error() {
        let cache = tmp_cache("missing");
        assert!(cache.load(Path::new("/nowhere/at/all")).is_none());
    }

    #[test]
    fn a_malformed_entry_is_a_miss() {
        let cache = tmp_cache("malformed");
        std::fs::create_dir_all(cache.dir()).unwrap();
        std::fs::write(cache.path_for(Path::new("/x")), b"{ not json").unwrap();

        assert!(
            cache.load(Path::new("/x")).is_none(),
            "a bad cache file costs a rescan, nothing more"
        );
        cache.clear().unwrap();
    }

    #[test]
    fn an_entry_from_another_version_is_ignored() {
        let cache = tmp_cache("version");
        let (_fx, options, result) = small_scan();
        cache.store(&options, &result).unwrap();

        // Rewrite with a future version.
        let path = cache.path_for(&options.root);
        let mut entry: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        entry["version"] = serde_json::json!(9999);
        std::fs::write(&path, serde_json::to_vec(&entry).unwrap()).unwrap();

        assert!(
            cache.load(&options.root).is_none(),
            "must not adopt an entry written by another format"
        );
        cache.clear().unwrap();
    }

    #[test]
    fn a_cancelled_scan_is_not_cached() {
        let cache = tmp_cache("cancelled");
        let (_fx, options, mut result) = small_scan();
        result.cancelled = true;

        cache.store(&options, &result).unwrap();
        assert!(
            cache.load(&options.root).is_none(),
            "caching a partial tree would open on an understated total that looks authoritative"
        );
    }

    #[test]
    fn depth_is_respected_when_matching() {
        let root = PathBuf::from("/data");
        let deep = CachedScan {
            version: CACHE_VERSION,
            scanned_at: now_epoch(),
            root: root.clone(),
            max_depth: Some(4),
            cross_filesystems: false,
            result: ScanResult {
                tree: crate::space::SpaceTree::new(),
                files: 0,
                dirs: 0,
                apparent_size: 0,
                allocated: 0,
                skipped: 0,
                errors: Vec::new(),
                cancelled: false,
                errors_truncated: false,
                coverage_note: None,
            },
        };

        // A deeper cache answers a shallower request.
        assert!(deep.satisfies(&Options::new(&root).max_depth(Some(2))));
        assert!(deep.satisfies(&Options::new(&root).max_depth(Some(4))));
        // But not a deeper one, which would silently show less than was asked for.
        assert!(!deep.satisfies(&Options::new(&root).max_depth(Some(6))));
        assert!(!deep.satisfies(&Options::new(&root).max_depth(None)));

        // An unlimited cache answers anything.
        let unlimited = CachedScan {
            max_depth: None,
            ..deep.clone()
        };
        assert!(unlimited.satisfies(&Options::new(&root).max_depth(None)));
        assert!(unlimited.satisfies(&Options::new(&root).max_depth(Some(99))));

        // A different root, or a different boundary policy, never matches.
        assert!(!deep.satisfies(&Options::new("/other").max_depth(Some(2))));
        assert!(
            !deep.satisfies(
                &Options::new(&root)
                    .max_depth(Some(2))
                    .cross_filesystems(true)
            )
        );
    }

    #[test]
    fn age_is_reported_and_a_backwards_clock_is_not_hidden() {
        let root = PathBuf::from("/data");
        let base = ScanResult {
            tree: crate::space::SpaceTree::new(),
            files: 0,
            dirs: 0,
            apparent_size: 0,
            allocated: 0,
            skipped: 0,
            errors: Vec::new(),
            cancelled: false,
            errors_truncated: false,
            coverage_note: None,
        };

        let recent = CachedScan {
            version: CACHE_VERSION,
            scanned_at: now_epoch() - 90,
            root: root.clone(),
            max_depth: None,
            cross_filesystems: false,
            result: base.clone(),
        };
        let age = recent.age_seconds().expect("a past scan has an age");
        assert!((85..=95).contains(&age), "age was {age}");

        // A scan stamped in the future means the clock moved. Say nothing rather than "just now".
        let future = CachedScan {
            scanned_at: now_epoch() + 3600,
            ..recent
        };
        assert!(future.age_seconds().is_none());
    }

    #[test]
    fn forget_removes_one_root_and_is_idempotent() {
        let cache = tmp_cache("forget");
        let (_fx, options, result) = small_scan();
        cache.store(&options, &result).unwrap();
        assert!(cache.load(&options.root).is_some());

        cache.forget(&options.root).unwrap();
        assert!(cache.load(&options.root).is_none());
        // Forgetting what is already gone is not an error.
        cache.forget(&options.root).unwrap();

        cache.clear().unwrap();
    }

    #[test]
    fn clear_is_idempotent_and_size_is_reportable() {
        let cache = tmp_cache("clear");
        let (_fx, options, result) = small_scan();
        cache.store(&options, &result).unwrap();
        assert!(cache.size_on_disk() > 0, "a stored scan occupies space");

        cache.clear().unwrap();
        cache.clear().unwrap();
        assert_eq!(cache.size_on_disk(), 0);
    }

    #[test]
    fn keys_are_stable_and_path_specific() {
        assert_eq!(
            key_for(Path::new("/home/me")),
            key_for(Path::new("/home/me"))
        );
        assert_ne!(
            key_for(Path::new("/home/me")),
            key_for(Path::new("/home/you"))
        );
        // Never contains a separator, so it is always a valid single filename.
        assert!(!key_for(Path::new("/a/b/c")).contains('/'));
    }

    #[test]
    fn storing_leaves_no_temporary_files() {
        let cache = tmp_cache("tmpfiles");
        let (_fx, options, result) = small_scan();
        cache.store(&options, &result).unwrap();

        let strays: Vec<String> = std::fs::read_dir(cache.dir())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(strays.is_empty(), "left behind: {strays:?}");

        cache.clear().unwrap();
    }
}
