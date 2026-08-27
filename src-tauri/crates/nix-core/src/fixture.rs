//! Reproducible filesystem fixtures for correctness and budget tests. Task 0.11 (`PLT-6`).
//!
//! The plan is blunt about why this exists: **build the fixture early or the budgets are
//! decoration.** Without a tree of known shape and size, the performance targets in the spec are
//! aspirations nobody can fail.
//!
//! Trees are generated from a seed with a tiny deterministic PRNG, so the same [`Spec`] produces
//! byte-identical output on every machine and in CI.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{IoContext, Result};

/// Shape of a generated tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// Directories per level.
    pub breadth: usize,
    /// Levels below the root.
    pub depth: usize,
    /// Regular files in each directory.
    pub files_per_dir: usize,
    /// Smallest file, in bytes.
    pub min_file_bytes: u64,
    /// Largest file, in bytes.
    pub max_file_bytes: u64,
    /// Seed, so a shape is reproducible.
    pub seed: u64,
}

impl Default for Spec {
    /// A tree small enough for a unit test: 5 dirs × 3 levels × 4 files.
    fn default() -> Self {
        Self {
            breadth: 5,
            depth: 3,
            files_per_dir: 4,
            min_file_bytes: 0,
            max_file_bytes: 4096,
            seed: 0x5EED,
        }
    }
}

impl Spec {
    /// A tree sized for budget measurement rather than correctness: roughly 93,000 files.
    ///
    /// **Release-mode measurement only.** Building it takes seconds, and a debug test that creates
    /// one while its siblings run concurrently produces an I/O storm that swamps every timing
    /// assertion in the module — which is exactly what happened to the scanner's cancellation test.
    #[must_use]
    pub fn perf() -> Self {
        Self {
            breadth: 8,
            depth: 4,
            files_per_dir: 20,
            min_file_bytes: 0,
            max_file_bytes: 16 * 1024,
            seed: 0xC0FFEE,
        }
    }

    /// Number of directories the spec will create, root excluded.
    #[must_use]
    pub fn expected_dirs(&self) -> u64 {
        // A complete b-ary tree of the given depth: b + b^2 + ... + b^depth.
        let b = self.breadth as u64;
        (1..=self.depth as u32).map(|level| b.pow(level)).sum()
    }

    /// Number of regular files the spec will create.
    #[must_use]
    pub fn expected_files(&self) -> u64 {
        // Files live in the root and in every generated directory.
        (self.expected_dirs() + 1) * self.files_per_dir as u64
    }
}

/// Deterministic, non-cryptographic PRNG. `SplitMix64`, chosen for being four lines long.
struct Rng(u64);

impl Rng {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn in_range(&mut self, low: u64, high: u64) -> u64 {
        if high <= low {
            return low;
        }
        low + self.next_u64() % (high - low + 1)
    }
}

/// Total on-disk bytes under a directory, recursively.
///
/// Uses allocated blocks rather than apparent size, so it matches what freeing the tree would
/// actually return to the filesystem.
#[must_use]
pub fn directory_size(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;

    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(std::result::Result::ok)
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => directory_size(&entry.path()) + meta.blocks() * 512,
            Ok(meta) => meta.blocks() * 512,
            Err(_) => 0,
        })
        .sum()
}

/// A generated tree that removes itself when dropped.
#[derive(Debug)]
pub struct Fixture {
    root: PathBuf,
    dirs: u64,
    files: u64,
    bytes: u64,
}

impl Fixture {
    /// Root of the generated tree.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directories created, root excluded.
    #[must_use]
    pub const fn dirs(&self) -> u64 {
        self.dirs
    }

    /// Regular files created.
    #[must_use]
    pub const fn files(&self) -> u64 {
        self.files
    }

    /// Total apparent bytes written.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Generate a tree under a unique directory in the system temporary directory.
    ///
    /// The name includes a per-process counter, not just the seed: two tests that share a seed run
    /// in parallel under `cargo test`, and naming by seed alone made them collide and delete each
    /// other's trees.
    pub fn create(spec: &Spec) -> Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "nix-fixture-{}-{}-{n}",
            std::process::id(),
            spec.seed
        ));
        Self::create_at(spec, root)
    }

    /// Generate a tree at an explicit path.
    pub fn create_at(spec: &Spec, root: PathBuf) -> Result<Self> {
        if root.exists() {
            std::fs::remove_dir_all(&root)
                .doing("clear a previous fixture")
                .map_err(|e| e.with_path(&root))?;
        }
        std::fs::create_dir_all(&root)
            .doing("create the fixture root")
            .map_err(|e| e.with_path(&root))?;

        let mut rng = Rng::new(spec.seed);
        let mut fixture = Self {
            root: root.clone(),
            dirs: 0,
            files: 0,
            bytes: 0,
        };
        fixture.fill(spec, &root, 0, &mut rng)?;
        Ok(fixture)
    }

    fn fill(&mut self, spec: &Spec, dir: &Path, level: usize, rng: &mut Rng) -> Result<()> {
        for f in 0..spec.files_per_dir {
            let size = rng.in_range(spec.min_file_bytes, spec.max_file_bytes);
            let path = dir.join(format!("file-{f:03}.bin"));
            let mut handle = std::fs::File::create(&path)
                .doing("create a fixture file")
                .map_err(|e| e.with_path(&path))?;
            // A repeating byte pattern: cheap to generate, and compresses predictably, which
            // matters once we start comparing apparent size against on-disk allocation.
            let block = [0xABu8; 4096];
            let mut left = size;
            while left > 0 {
                let take = usize::try_from(left.min(block.len() as u64)).unwrap_or(block.len());
                handle
                    .write_all(&block[..take])
                    .doing("write a fixture file")
                    .map_err(|e| e.with_path(&path))?;
                left -= take as u64;
            }
            self.files += 1;
            self.bytes += size;
        }

        if level >= spec.depth {
            return Ok(());
        }

        for d in 0..spec.breadth {
            let child = dir.join(format!("dir-{d:03}"));
            std::fs::create_dir_all(&child)
                .doing("create a fixture directory")
                .map_err(|e| e.with_path(&child))?;
            self.dirs += 1;
            self.fill(spec, &child, level + 1, rng)?;
        }
        Ok(())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Best effort: a leftover fixture in the temp directory is untidy, not dangerous.
        std::fs::remove_dir_all(&self.root).ok();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn generated_shape_matches_the_spec() {
        let spec = Spec {
            breadth: 2,
            depth: 2,
            files_per_dir: 3,
            ..Spec::default()
        };
        let fx = Fixture::create(&spec).unwrap();
        // 2 + 4 = 6 directories, and files in those plus the root.
        assert_eq!(spec.expected_dirs(), 6);
        assert_eq!(fx.dirs(), spec.expected_dirs());
        assert_eq!(fx.files(), spec.expected_files());
        assert!(fx.root().is_dir());
    }

    #[test]
    fn generation_is_deterministic_for_a_seed() {
        let spec = Spec {
            seed: 42,
            depth: 1,
            breadth: 2,
            files_per_dir: 2,
            ..Spec::default()
        };
        let a = Fixture::create_at(&spec, std::env::temp_dir().join("nix-fx-det-a")).unwrap();
        let b = Fixture::create_at(&spec, std::env::temp_dir().join("nix-fx-det-b")).unwrap();
        assert_eq!(
            a.bytes(),
            b.bytes(),
            "same seed must produce the same bytes"
        );
        assert_eq!(a.files(), b.files());
    }

    #[test]
    fn different_seeds_differ() {
        let base = Spec {
            depth: 1,
            breadth: 2,
            files_per_dir: 4,
            ..Spec::default()
        };
        let a = Fixture::create_at(
            &Spec {
                seed: 1,
                ..base.clone()
            },
            std::env::temp_dir().join("nix-fx-s1"),
        )
        .unwrap();
        let b = Fixture::create_at(
            &Spec { seed: 2, ..base },
            std::env::temp_dir().join("nix-fx-s2"),
        )
        .unwrap();
        assert_ne!(
            a.bytes(),
            b.bytes(),
            "different seeds should differ in content size"
        );
    }

    #[test]
    fn fixture_removes_itself_on_drop() {
        let path = {
            let fx = Fixture::create(&Spec {
                depth: 1,
                breadth: 1,
                files_per_dir: 1,
                ..Spec::default()
            })
            .unwrap();
            fx.root().to_path_buf()
        };
        assert!(!path.exists(), "drop must clean up the tree");
    }

    #[test]
    fn expected_counts_are_arithmetic_not_measured() {
        let spec = Spec {
            breadth: 3,
            depth: 3,
            files_per_dir: 2,
            ..Spec::default()
        };
        // 3 + 9 + 27
        assert_eq!(spec.expected_dirs(), 39);
        assert_eq!(spec.expected_files(), 80);
    }
}
