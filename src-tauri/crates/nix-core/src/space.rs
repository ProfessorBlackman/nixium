// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The space model. Task 1.1.
//!
//! This is the spine of the product. Stacer's defining flaw was that disk concerns were scattered
//! across four pages that never spoke to each other — a five-category cleaner, an uninstaller with
//! no size information, a `find` form, and a pie chart of volume *capacity*. The question a user
//! actually arrives with, "what is eating my disk and what can I safely reclaim?", was answered by
//! no page and could not be, because nothing shared a model of "space attributed to a thing".
//!
//! Every storage view in nix is a projection of [`SpaceTree`]. Nothing scrapes the filesystem
//! independently.
//!
//! # Invariants
//!
//! [`SpaceTree::check_invariants`] enforces the structural rules from the specification:
//!
//! 1. An entry's allocated size never exceeds its parent's.
//! 2. Category totals plus `Unknown` equal filesystem usage — checked by the reclaim scan, which
//!    is the only layer that knows the filesystem total.
//! 3. An entry rated [`Safety::Never`] carries no reclaim method.
//! 4. [`ReclaimMethod::Unlink`] is only ever emitted for a path inside its category's declared root.
//! 5. Rescanning yields the same [`EntryId`] for the same thing, so selections survive a refresh.
//!
//! Rules 1, 3 and 4 are structural and checked here. Rule 5 is a property of how ids are derived
//! ([`EntryId::for_path`]) and is tested directly.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Identifies one entry, stably across rescans.
///
/// Derived from the entry's identity — its path, or its label for a logical entry — rather than
/// from insertion order, so a refresh does not invalidate the user's selection. That is invariant 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, TS)]
#[ts(export, type = "string")]
pub struct EntryId(pub u64);

/// Serialised as a **hex string**, not a number, deliberately.
///
/// An id is used both as a value (`roots`) and as a map key (`entries`). JSON object keys are always
/// strings, so a numeric id would cross the wire as a string in one position and a number in the
/// other — and an earlier version of this type did exactly that while declaring `string` to
/// TypeScript, which typechecked and would have failed at runtime. One representation everywhere
/// removes the whole class of problem.
impl Serialize for EntryId {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{self}"))
    }
}

impl<'de> Deserialize<'de> for EntryId {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        u64::from_str_radix(&raw, 16)
            .map(Self)
            .map_err(|e| serde::de::Error::custom(format!("invalid entry id {raw:?}: {e}")))
    }
}

impl EntryId {
    /// FNV-1a over the bytes of a key. Not cryptographic — it only needs to be stable and
    /// well-distributed, and it must produce the same value in every process.
    fn hash_bytes(bytes: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for b in bytes {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    /// The id for a filesystem path.
    #[must_use]
    pub fn for_path(path: &Path) -> Self {
        use std::os::unix::ffi::OsStrExt;
        Self(Self::hash_bytes(path.as_os_str().as_bytes()))
    }

    /// The id for a logical entry that has no path — a journal budget, a package, a snapshot.
    #[must_use]
    pub fn for_label(kind: &str, label: &str) -> Self {
        Self(Self::hash_bytes(format!("{kind}\u{0}{label}").as_bytes()))
    }
}

impl std::fmt::Display for EntryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// What kind of thing is holding the space.
///
/// `Unknown` is a first-class category, not a bug: space nix cannot attribute is shown as
/// unattributed rather than silently dropped, which is what makes invariant 2 meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Category {
    /// Files installed by a package.
    PackagePayload,
    /// A package manager's download cache.
    PackageCache,
    /// An application's regenerable cache.
    AppCache,
    /// Rotated or archived logs.
    Log,
    /// The systemd journal.
    Journal,
    /// Freedesktop trash.
    Trash,
    /// A filesystem snapshot.
    Snapshot,
    /// A container image, layer or build cache.
    ContainerImage,
    /// Build output or a language package cache.
    BuildArtifact,
    /// A thumbnail cache.
    Thumbnail,
    /// A crash dump or core file.
    CrashDump,
    /// Configuration left behind by removed software.
    OrphanedConfig,
    /// The user's own files. Never reclaimed without explicit selection.
    UserFile,
    /// A duplicate of another entry.
    Duplicate,
    /// Space nix could not attribute.
    Unknown,
}

impl Category {
    /// Every category, for reporting totals.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::PackagePayload,
            Self::PackageCache,
            Self::AppCache,
            Self::Log,
            Self::Journal,
            Self::Trash,
            Self::Snapshot,
            Self::ContainerImage,
            Self::BuildArtifact,
            Self::Thumbnail,
            Self::CrashDump,
            Self::OrphanedConfig,
            Self::UserFile,
            Self::Duplicate,
            Self::Unknown,
        ]
    }

    /// Human label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::PackagePayload => "Installed software",
            Self::PackageCache => "Package cache",
            Self::AppCache => "Application caches",
            Self::Log => "Logs",
            Self::Journal => "System journal",
            Self::Trash => "Trash",
            Self::Snapshot => "Snapshots",
            Self::ContainerImage => "Container images",
            Self::BuildArtifact => "Build artifacts",
            Self::Thumbnail => "Thumbnails",
            Self::CrashDump => "Crash reports",
            Self::OrphanedConfig => "Orphaned configuration",
            Self::UserFile => "Your files",
            Self::Duplicate => "Duplicates",
            Self::Unknown => "Unattributed",
        }
    }
}

/// How much of an entry's size would actually come back if it were reclaimed.
///
/// On a copy-on-write filesystem — btrfs, ZFS, or an LVM thin pool — a file's extents may be shared
/// with a snapshot. Deleting the file removes the name, but the blocks stay allocated until every
/// reference to them is gone, so the space does **not** come back. A tool that reports the file's
/// size as reclaimable in that situation is promising something it cannot deliver.
///
/// So the estimate is qualified rather than asserted. Specification `STO-17`: *where exclusive size
/// is unobtainable, suppress the estimate rather than fake it.*
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "confidence", rename_all = "snake_case")]
#[ts(export)]
pub enum Reclaimable {
    /// Freeing this returns the stated size. The ordinary case, and the only one that may be summed
    /// into a headline figure without qualification.
    ///
    /// The default, so an ordinary filesystem is unaffected by any of this.
    #[default]
    Exact,
    /// Freeing this returns **at most** the stated size, and possibly nothing.
    ///
    /// `exclusive` is the portion nix could prove is referenced only here, when a tool was able to
    /// tell us.
    AtMost {
        #[ts(type = "number | null")]
        exclusive: Option<u64>,
        reason: String,
    },
    /// nix cannot say how much would come back.
    ///
    /// Reported honestly rather than guessed: on a copy-on-write filesystem without the tools to
    /// ask, any number would be invention.
    Unknown { reason: String },
}

impl Reclaimable {
    /// Whether the stated size can be trusted as a figure that will actually be returned.
    #[must_use]
    pub const fn is_exact(&self) -> bool {
        matches!(self, Self::Exact)
    }

    /// The bytes safe to include in a headline total.
    ///
    /// An `AtMost` entry contributes only its *proven exclusive* portion, and nothing when that is
    /// unknown — because a total is a promise, and a promise built from maybes is not one.
    #[must_use]
    pub const fn promisable(&self, stated: u64) -> u64 {
        match self {
            Self::Exact => stated,
            Self::AtMost {
                exclusive: Some(exclusive),
                ..
            } => *exclusive,
            Self::AtMost {
                exclusive: None, ..
            }
            | Self::Unknown { .. } => 0,
        }
    }

    /// A phrase for the UI, or `None` when there is nothing to caveat.
    #[must_use]
    pub fn caveat(&self) -> Option<&str> {
        match self {
            Self::Exact => None,
            Self::AtMost { reason, .. } | Self::Unknown { reason } => Some(reason),
        }
    }
}

/// How safe it is to reclaim an entry.
///
/// **Computed, never hardcoded per category**: an open file handle, a recent access time, or a
/// protected-path match escalates the rating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Safety {
    /// Regenerable with no user-visible loss.
    Safe,
    /// Reclaimable, but it costs something — a slower next launch, a lost session.
    Review,
    /// May break a running service or lose data.
    Risky,
    /// Not reclaimable by nix at all. Shown for attribution only.
    Never,
}

impl Safety {
    /// Whether bulk selection may include this rating.
    ///
    /// `Risky` requires per-item confirmation, so it is excluded from "select all".
    #[must_use]
    pub const fn bulk_selectable(self) -> bool {
        matches!(self, Self::Safe | Self::Review)
    }

    /// Whether a quick-clean flow may pre-check this rating.
    #[must_use]
    pub const fn pre_checkable(self) -> bool {
        matches!(self, Self::Safe)
    }

    /// The stricter of two ratings. Used when rolling a directory up from its children.
    #[must_use]
    pub fn strictest(self, other: Self) -> Self {
        self.max(other)
    }
}

/// A category of reclaimable system file.
///
/// Each names a fixed set of roots inside the helper. Adding a variant means widening the privileged
/// surface, and belongs in the same review as the feature that needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReclaimKind {
    /// A package manager's downloaded-package cache.
    PackageCache,
    /// A **rotated** log. Never an active one: the helper checks the filename shape, so a live log
    /// cannot be deleted through this operation even by a caller that asks for it.
    RotatedLog,
    /// A crash dump under `/var/crash`.
    CrashDump,
}

impl ReclaimKind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PackageCache => "package_cache",
            Self::RotatedLog => "rotated_log",
            Self::CrashDump => "crash_dump",
        }
    }
}

/// A class of removable package.
///
/// Like [`ReclaimKind`], this names a derivation the privileged helper performs **itself**. Adding a
/// variant widens the privileged surface and belongs in the same review as the feature that needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RemovableKind {
    /// Kernels older than both the running one and the newest installed one.
    OldKernel,
    /// Configuration left behind by packages already removed.
    ResidualConfig,
}

impl RemovableKind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::OldKernel => "old_kernel",
            Self::ResidualConfig => "residual_config",
        }
    }
}

/// Package managers whose cache the helper can clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Manager {
    Apt,
    Dnf,
    Pacman,
    Zypper,
}

impl Manager {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Apt => "apt",
            Self::Dnf => "dnf",
            Self::Pacman => "pacman",
            Self::Zypper => "zypper",
        }
    }
}

/// How much journal to keep. Typed rather than a string so nothing caller-supplied is interpolated
/// into a command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "by", rename_all = "snake_case")]
#[ts(export)]
pub enum VacuumLimit {
    /// Keep at most this many mebibytes.
    Size {
        #[ts(type = "number")]
        mebibytes: u64,
    },
    /// Keep at most this many days.
    Age {
        #[ts(type = "number")]
        days: u64,
    },
}

/// How space is actually reclaimed. Always prefers the owning tool over `unlink`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "method", rename_all = "snake_case")]
#[ts(export)]
pub enum ReclaimMethod {
    /// Delegate to the package manager: `apt-get clean`, `dnf clean packages`, and so on.
    ///
    /// Carries the manager as an **enum, not a command string**. The argument vector is fixed inside
    /// the privileged helper, so there is no text here that could become part of a root command line.
    PackageManager { manager: Manager },
    /// Vacuum the journal, by size or by age. Typed for the same reason.
    JournalVacuum { limit: VacuumLimit },
    /// Remove packages through the privileged helper.
    ///
    /// The helper re-derives which packages qualify and refuses any name outside that set, so this
    /// cannot be used to remove an arbitrary package — including the running kernel.
    Packages {
        kind: RemovableKind,
        names: Vec<String>,
    },
    /// Delete one system file through the privileged helper.
    ///
    /// The `kind` travels with the path, and the helper independently re-derives which roots that
    /// category owns and refuses anything outside them — so this cannot be used to delete an
    /// arbitrary file even by a caller that constructs it deliberately.
    SystemFile { kind: ReclaimKind, path: PathBuf },
    /// Drop a superseded snap revision.
    ///
    /// The helper re-derives which revisions snapd has marked disabled and refuses anything outside
    /// that set, so the active revision cannot be removed even if named deliberately.
    SnapRevision { package: String, revision: String },
    /// Ask flatpak to uninstall the runtimes it considers unused.
    ///
    /// Carries nothing at all: the command is fixed and the decision of what qualifies is flatpak's.
    /// One entry covers the whole operation, because that is the granularity flatpak offers.
    FlatpakUnused,
    /// Prune container images or build caches.
    ContainerPrune { scope: String },
    /// Empty a volume's trash.
    TrashEmpty { volume: PathBuf },
    /// Move to trash — **the default for user files**, because it is reversible.
    MoveToTrash { path: PathBuf },
    /// Unlink. Last resort, only for `Safe` entries under a validated category root.
    Unlink { path: PathBuf },
}

impl ReclaimMethod {
    /// Whether this destroys data outright rather than moving it somewhere recoverable.
    #[must_use]
    pub const fn is_irreversible(&self) -> bool {
        !matches!(self, Self::MoveToTrash { .. })
    }

    /// Whether the entry's path **is** the thing being removed.
    ///
    /// # Why this distinction has to exist
    ///
    /// Reclaiming re-checks a path immediately before acting on it, so that something which changed
    /// since the preview is left alone. That check only means anything when the path is the target.
    ///
    /// Several methods act on a *logical* object instead — a kernel, a snap revision, a package
    /// manager's cache — and carry a descriptive path like `kernel 6.8.0-136-generic` that was never
    /// meant to exist on disk. Re-checking those would find nothing and conclude the item was
    /// "already gone", silently skipping every one of them.
    ///
    /// For those, the guard is stronger than a fingerprint rather than weaker: the privileged helper
    /// re-derives the eligible set at the moment it acts, so a kernel that stopped qualifying between
    /// preview and execution is refused by the process carrying out the removal.
    #[must_use]
    pub const fn acts_on_path(&self) -> bool {
        match self {
            Self::MoveToTrash { .. }
            | Self::Unlink { .. }
            | Self::SystemFile { .. }
            | Self::TrashEmpty { .. } => true,
            Self::PackageManager { .. }
            | Self::JournalVacuum { .. }
            | Self::Packages { .. }
            | Self::SnapRevision { .. }
            | Self::FlatpakUnused
            | Self::ContainerPrune { .. } => false,
        }
    }
}

/// Space nix can **see and account for but will not act on itself**.
///
/// # Why this exists
///
/// The failure mode this project exists to avoid is space that is real, large, and invisible. But
/// there is a second failure mode just as bad: automating a destructive operation that has never
/// been exercised. Some findings sit between the two — the bytes are certain, the remedy is known,
/// and the tool that would carry it out is either absent or unverified on this machine.
///
/// Hiding those bytes would repeat the first mistake; offering a button would commit the second. An
/// advisory is the honest third answer: report the size, name the remedy, and say plainly why nix is
/// not doing it. The user keeps the information and the decision.
///
/// The concrete case that prompted it: 701 MiB of unreferenced objects in this machine's flatpak
/// ostree repository, with no `ostree` binary installed to prune them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Advisory {
    /// Where it is, when it is a place.
    pub path: Option<PathBuf>,
    /// What the user sees.
    pub label: String,
    /// On-disk bytes, measured the same way a candidate's are.
    #[ts(type = "number")]
    pub bytes: u64,
    /// How much of `bytes` would actually come back, qualified as honestly as a candidate's.
    pub reclaimable: Reclaimable,
    /// Why nix will not do this itself. Required, because an advisory without this is just a
    /// refusal with no explanation.
    pub why_manual: String,
    /// What the user can run instead. A command they can read before they run it.
    pub remedy: String,
    /// Which category reported it.
    pub category: String,
}

/// How nix concluded what an entry is. Shown in the UI, so a user can judge our reasoning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "by", rename_all = "snake_case")]
#[ts(export)]
pub enum Provenance {
    /// Found by walking the filesystem, with no further attribution.
    Walked,
    /// Matched a known location — a cache directory, a trash root.
    KnownPath { rule: String },
    /// Reported by a package manager.
    PackageManager { backend: String },
    /// Recognised by a project marker file, e.g. `Cargo.toml` next to `target/`.
    ProjectMarker { marker: String },
    /// Reported by a filesystem-specific tool, e.g. btrfs.
    FilesystemTool { tool: String },
    /// A synthetic entry standing in for children too small to be worth their own node. `STO-19`.
    ///
    /// Its bytes are exactly the sum of what it replaced, so a parent's total still equals the sum of
    /// its children — the aggregate is a *summary*, never a rounding.
    Aggregated {
        #[ts(type = "number")]
        count: u64,
    },
}

/// One node: bytes attributed to a thing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SpaceEntry {
    pub id: EntryId,
    /// Absent for a logical entry that is not a path.
    pub path: Option<PathBuf>,
    /// Human name: "Firefox cache", "linux-image-6.5.0-21".
    pub label: String,
    /// Sum of file sizes.
    #[ts(type = "number")]
    pub apparent_size: u64,
    /// On-disk blocks. Differs from apparent on sparse, compressed and copy-on-write filesystems,
    /// which is why both are carried rather than one being called "the" size.
    #[ts(type = "number")]
    pub allocated: u64,
    pub category: Category,
    pub provenance: Provenance,
    pub safety: Safety,
    pub reclaim: Option<ReclaimMethod>,
    /// Seconds since the Unix epoch, when known.
    #[ts(type = "number | null")]
    pub last_used: Option<i64>,
    /// True for a directory, so the UI can offer to drill in.
    pub is_dir: bool,
    /// Immediate children. The model is a tree.
    pub children: Vec<EntryId>,
}

impl SpaceEntry {
    /// A plain filesystem entry, unattributed and unreclaimable until something classifies it.
    #[must_use]
    pub fn walked(path: PathBuf, apparent_size: u64, allocated: u64, is_dir: bool) -> Self {
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self {
            id: EntryId::for_path(&path),
            path: Some(path),
            label,
            apparent_size,
            allocated,
            category: Category::Unknown,
            provenance: Provenance::Walked,
            safety: Safety::Never,
            reclaim: None,
            last_used: None,
            is_dir,
            children: Vec::new(),
        }
    }
}

impl SpaceEntry {
    /// The stand-in for a directory's children that were too small for their own nodes. `STO-19`.
    ///
    /// Carries no path, because it is not a place — it is a statement about a set of places. The
    /// count is structured rather than only being in the label, so the UI can decide how to phrase it
    /// without parsing English.
    #[must_use]
    pub fn aggregated(parent: &Path, count: u64, apparent_size: u64, allocated: u64) -> Self {
        Self {
            // Derived from the parent's path, so it is stable across rescans like every other id.
            id: EntryId::for_label("aggregated", &parent.to_string_lossy()),
            path: None,
            label: format!(
                "{count} smaller {}",
                if count == 1 { "item" } else { "items" }
            ),
            apparent_size,
            allocated,
            category: Category::Unknown,
            provenance: Provenance::Aggregated { count },
            safety: Safety::Never,
            reclaim: None,
            last_used: None,
            is_dir: false,
            children: Vec::new(),
        }
    }
}

/// A violation of one of the model's structural rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub entry: EntryId,
    pub rule: &'static str,
    pub detail: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} [{}]: {}", self.entry, self.rule, self.detail)
    }
}

/// An arena of entries plus the roots, so the tree can be sent over IPC without cycles.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SpaceTree {
    /// Every entry, keyed by id.
    ///
    /// `#[ts(as = ...)]` rather than `#[ts(type = ...)]`: a raw type *string* is emitted verbatim
    /// with no import, which silently broke the generated module and collapsed every downstream
    /// type to `any`. Naming a real Rust type instead lets ts-rs collect `SpaceEntry` as a
    /// dependency and import it.
    #[ts(as = "HashMap<String, SpaceEntry>")]
    pub entries: HashMap<EntryId, SpaceEntry>,
    /// Top-level entries, in insertion order.
    pub roots: Vec<EntryId>,
}

impl SpaceTree {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert an entry, returning its id. Re-inserting the same id replaces it.
    pub fn insert(&mut self, entry: SpaceEntry) -> EntryId {
        let id = entry.id;
        self.entries.insert(id, entry);
        id
    }

    /// Insert an entry and record it as a root.
    pub fn insert_root(&mut self, entry: SpaceEntry) -> EntryId {
        let id = self.insert(entry);
        if !self.roots.contains(&id) {
            self.roots.push(id);
        }
        id
    }

    /// Attach `child` to `parent`.
    pub fn attach(&mut self, parent: EntryId, child: EntryId) {
        if let Some(p) = self.entries.get_mut(&parent) {
            if !p.children.contains(&child) {
                p.children.push(child);
            }
        }
    }

    #[must_use]
    pub fn get(&self, id: EntryId) -> Option<&SpaceEntry> {
        self.entries.get(&id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total allocated bytes across the roots.
    #[must_use]
    pub fn total_allocated(&self) -> u64 {
        self.roots
            .iter()
            .filter_map(|id| self.entries.get(id))
            .map(|e| e.allocated)
            .sum()
    }

    /// Allocated bytes per category, over every entry that has no children — leaves only, so a
    /// directory's bytes are not counted twice.
    #[must_use]
    pub fn allocated_by_category(&self) -> HashMap<Category, u64> {
        let mut totals: HashMap<Category, u64> = HashMap::new();
        for entry in self.entries.values().filter(|e| e.children.is_empty()) {
            *totals.entry(entry.category).or_default() += entry.allocated;
        }
        totals
    }

    /// Check the structural invariants. An empty result means the tree is sound.
    ///
    /// Invariant 2 is not checked here: only the caller knows the filesystem's used bytes.
    #[must_use]
    pub fn check_invariants(&self) -> Vec<Violation> {
        let mut violations = Vec::new();

        for entry in self.entries.values() {
            // 1. A child cannot hold more bytes than its parent.
            let children_allocated: u64 = entry
                .children
                .iter()
                .filter_map(|c| self.entries.get(c))
                .map(|c| c.allocated)
                .sum();
            if children_allocated > entry.allocated {
                violations.push(Violation {
                    entry: entry.id,
                    rule: "child_fits_parent",
                    detail: format!(
                        "children hold {children_allocated} bytes, parent claims {}",
                        entry.allocated
                    ),
                });
            }

            // 3. `Never` means not reclaimable, so it cannot carry a method.
            if entry.safety == Safety::Never && entry.reclaim.is_some() {
                violations.push(Violation {
                    entry: entry.id,
                    rule: "never_has_no_method",
                    detail: "rated Never but carries a reclaim method".to_string(),
                });
            }

            // 4. Unlink is only for Safe entries, and only at the entry's own path.
            if let Some(ReclaimMethod::Unlink { path }) = &entry.reclaim {
                if entry.safety != Safety::Safe {
                    violations.push(Violation {
                        entry: entry.id,
                        rule: "unlink_requires_safe",
                        detail: format!("unlink offered for a {:?} entry", entry.safety),
                    });
                }
                if entry.path.as_deref() != Some(path.as_path()) {
                    violations.push(Violation {
                        entry: entry.id,
                        rule: "unlink_matches_own_path",
                        detail: format!(
                            "unlink targets {} which is not this entry",
                            path.display()
                        ),
                    });
                }
            }

            // A child recorded but absent from the arena is a dangling reference.
            for child in &entry.children {
                if !self.entries.contains_key(child) {
                    violations.push(Violation {
                        entry: entry.id,
                        rule: "children_exist",
                        detail: format!("child {child} is not in the tree"),
                    });
                }
            }
        }

        for root in &self.roots {
            if !self.entries.contains_key(root) {
                violations.push(Violation {
                    entry: *root,
                    rule: "roots_exist",
                    detail: "root is not in the tree".to_string(),
                });
            }
        }

        violations
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn file(path: &str, allocated: u64) -> SpaceEntry {
        SpaceEntry::walked(PathBuf::from(path), allocated, allocated, false)
    }

    fn dir(path: &str, allocated: u64) -> SpaceEntry {
        SpaceEntry::walked(PathBuf::from(path), allocated, allocated, true)
    }

    // ---- invariant 5: ids are stable, and derived from identity ----

    #[test]
    fn ids_are_stable_across_calls_and_distinct_across_paths() {
        let a = EntryId::for_path(Path::new("/home/me/.cache"));
        let b = EntryId::for_path(Path::new("/home/me/.cache"));
        assert_eq!(a, b, "the same path must always yield the same id");

        let c = EntryId::for_path(Path::new("/home/me/.config"));
        assert_ne!(a, c, "different paths must yield different ids");
    }

    #[test]
    fn label_ids_are_namespaced_by_kind() {
        let pkg = EntryId::for_label("package", "firefox");
        let snap = EntryId::for_label("snapshot", "firefox");
        assert_ne!(pkg, snap, "the kind must participate in the id");
    }

    proptest! {
        /// Invariant 5, generatively: rebuilding a tree from the same paths reproduces the same ids,
        /// which is what lets a user's selection survive a rescan.
        #[test]
        fn rescanning_reproduces_ids(paths in prop::collection::vec("/[a-z]{1,8}(/[a-z]{1,8}){0,3}", 1..30)) {
            let first: Vec<EntryId> = paths.iter().map(|p| EntryId::for_path(Path::new(p))).collect();
            let second: Vec<EntryId> = paths.iter().map(|p| EntryId::for_path(Path::new(p))).collect();
            prop_assert_eq!(first, second);
        }

        /// Distinct paths should not collide. A 64-bit FNV hash over a few hundred short paths
        /// colliding would indicate the derivation is wrong, not merely unlucky.
        #[test]
        fn distinct_paths_do_not_collide(
            paths in prop::collection::hash_set("/[a-z]{1,10}(/[a-z]{1,10}){0,3}", 1..200)
        ) {
            let ids: std::collections::HashSet<EntryId> =
                paths.iter().map(|p| EntryId::for_path(Path::new(p))).collect();
            prop_assert_eq!(ids.len(), paths.len(), "id collision across distinct paths");
        }
    }

    // ---- invariant 1: children fit inside their parent ----

    #[test]
    fn sound_tree_has_no_violations() {
        let mut tree = SpaceTree::new();
        let root = tree.insert_root(dir("/data", 300));
        let a = tree.insert(file("/data/a", 100));
        let b = tree.insert(file("/data/b", 200));
        tree.attach(root, a);
        tree.attach(root, b);

        assert!(
            tree.check_invariants().is_empty(),
            "{:?}",
            tree.check_invariants()
        );
    }

    #[test]
    fn children_exceeding_parent_is_caught() {
        let mut tree = SpaceTree::new();
        let root = tree.insert_root(dir("/data", 100));
        let a = tree.insert(file("/data/a", 90));
        let b = tree.insert(file("/data/b", 90));
        tree.attach(root, a);
        tree.attach(root, b);

        let violations = tree.check_invariants();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "child_fits_parent");
    }

    #[test]
    fn dangling_child_and_root_references_are_caught() {
        let mut tree = SpaceTree::new();
        let root = tree.insert_root(dir("/data", 100));
        tree.attach(root, EntryId(0xdead_beef));
        tree.roots.push(EntryId(0xfeed_face));

        let rules: Vec<&str> = tree.check_invariants().iter().map(|v| v.rule).collect();
        assert!(rules.contains(&"children_exist"), "{rules:?}");
        assert!(rules.contains(&"roots_exist"), "{rules:?}");
    }

    // ---- invariant 3: Never carries no method ----

    #[test]
    fn never_with_a_reclaim_method_is_caught() {
        let mut tree = SpaceTree::new();
        let mut e = file("/boot/vmlinuz", 10);
        e.safety = Safety::Never;
        e.reclaim = Some(ReclaimMethod::Unlink {
            path: PathBuf::from("/boot/vmlinuz"),
        });
        tree.insert_root(e);

        let rules: Vec<&str> = tree.check_invariants().iter().map(|v| v.rule).collect();
        assert!(rules.contains(&"never_has_no_method"), "{rules:?}");
    }

    // ---- invariant 4: unlink is constrained ----

    #[test]
    fn unlink_requires_a_safe_rating() {
        let mut tree = SpaceTree::new();
        let mut e = file("/var/cache/x", 10);
        e.safety = Safety::Review;
        e.reclaim = Some(ReclaimMethod::Unlink {
            path: PathBuf::from("/var/cache/x"),
        });
        tree.insert_root(e);

        let rules: Vec<&str> = tree.check_invariants().iter().map(|v| v.rule).collect();
        assert!(rules.contains(&"unlink_requires_safe"), "{rules:?}");
    }

    #[test]
    fn unlink_may_not_target_another_path() {
        let mut tree = SpaceTree::new();
        let mut e = file("/var/cache/x", 10);
        e.safety = Safety::Safe;
        // The classic bug this guards: a method built from the wrong path.
        e.reclaim = Some(ReclaimMethod::Unlink {
            path: PathBuf::from("/etc/passwd"),
        });
        tree.insert_root(e);

        let rules: Vec<&str> = tree.check_invariants().iter().map(|v| v.rule).collect();
        assert!(rules.contains(&"unlink_matches_own_path"), "{rules:?}");
    }

    proptest! {
        /// Invariant 1, generatively: any tree built so that a parent's size is the sum of its
        /// children must always pass, whatever the sizes.
        #[test]
        fn rolled_up_parents_always_satisfy_invariants(sizes in prop::collection::vec(0u64..1_000_000, 1..40)) {
            let total: u64 = sizes.iter().sum();
            let mut tree = SpaceTree::new();
            let root = tree.insert_root(dir("/gen", total));
            for (i, size) in sizes.iter().enumerate() {
                let child = tree.insert(file(&format!("/gen/f{i}"), *size));
                tree.attach(root, child);
            }
            prop_assert!(tree.check_invariants().is_empty());
        }

        /// Invariant 3, generatively: whatever the rating, a `Never` entry must never carry a
        /// method, and every other rating may.
        #[test]
        fn never_never_carries_a_method(never in any::<bool>()) {
            let mut tree = SpaceTree::new();
            let mut e = file("/gen/x", 10);
            e.safety = if never { Safety::Never } else { Safety::Safe };
            e.reclaim = Some(ReclaimMethod::Unlink { path: PathBuf::from("/gen/x") });
            tree.insert_root(e);

            let caught = tree
                .check_invariants()
                .iter()
                .any(|v| v.rule == "never_has_no_method");
            prop_assert_eq!(caught, never);
        }
    }

    // ---- categories, safety, methods ----

    #[test]
    fn category_labels_are_present_and_distinct() {
        let mut seen = std::collections::HashSet::new();
        for c in Category::all() {
            assert!(!c.label().is_empty());
            assert!(seen.insert(c.label()), "duplicate label {}", c.label());
        }
        assert_eq!(seen.len(), 15, "the specification lists fifteen categories");
    }

    #[test]
    fn safety_gates_bulk_and_precheck_correctly() {
        assert!(Safety::Safe.bulk_selectable());
        assert!(Safety::Review.bulk_selectable());
        assert!(
            !Safety::Risky.bulk_selectable(),
            "risky needs per-item confirmation"
        );
        assert!(!Safety::Never.bulk_selectable());

        assert!(Safety::Safe.pre_checkable());
        assert!(
            !Safety::Review.pre_checkable(),
            "review has a cost, so never pre-checked"
        );
    }

    #[test]
    fn strictest_rolls_up_the_worse_rating() {
        assert_eq!(Safety::Safe.strictest(Safety::Risky), Safety::Risky);
        assert_eq!(Safety::Never.strictest(Safety::Safe), Safety::Never);
        assert_eq!(Safety::Review.strictest(Safety::Safe), Safety::Review);
    }

    // ---- reclaimable confidence ----

    #[test]
    fn an_exact_estimate_is_promisable_in_full() {
        let exact = Reclaimable::Exact;
        assert!(exact.is_exact());
        assert_eq!(exact.promisable(1_000_000), 1_000_000);
        assert_eq!(exact.caveat(), None);
    }

    /// The property this type exists for: a shared entry must not contribute its whole size to a
    /// headline figure, because that space will not come back.
    #[test]
    fn a_shared_entry_promises_only_what_is_proven_exclusive() {
        let partly = Reclaimable::AtMost {
            exclusive: Some(200_000),
            reason: "Shared with a snapshot.".into(),
        };
        assert!(!partly.is_exact());
        assert_eq!(
            partly.promisable(1_000_000),
            200_000,
            "only the portion proven to be referenced nowhere else"
        );
        assert!(
            partly.caveat().is_some(),
            "a qualified figure must explain itself"
        );
    }

    #[test]
    fn an_unprovable_entry_promises_nothing_at_all() {
        // The specification's rule: suppress the estimate rather than fake it. Zero is the only
        // honest contribution to a promise when nothing can be proven.
        let unprovable = Reclaimable::AtMost {
            exclusive: None,
            reason: "Extents may be shared with a snapshot.".into(),
        };
        assert_eq!(unprovable.promisable(1_000_000), 0);

        let unknown = Reclaimable::Unknown {
            reason: "btrfs tools are unavailable.".into(),
        };
        assert_eq!(unknown.promisable(1_000_000), 0);
        assert!(unknown.caveat().is_some());
    }

    #[test]
    fn exactness_is_the_default_so_ordinary_filesystems_are_unaffected() {
        assert!(Reclaimable::default().is_exact());
    }

    #[test]
    fn reclaimable_round_trips_over_the_wire() {
        for value in [
            Reclaimable::Exact,
            Reclaimable::AtMost {
                exclusive: Some(42),
                reason: "shared".into(),
            },
            Reclaimable::AtMost {
                exclusive: None,
                reason: "unknown sharing".into(),
            },
            Reclaimable::Unknown {
                reason: "no tools".into(),
            },
        ] {
            let json = serde_json::to_string(&value).unwrap();
            let back: Reclaimable = serde_json::from_str(&json).unwrap();
            assert_eq!(value, back, "{json}");
        }
    }

    #[test]
    fn only_trashing_is_reversible() {
        assert!(
            !ReclaimMethod::MoveToTrash {
                path: PathBuf::from("/x")
            }
            .is_irreversible()
        );
        assert!(
            ReclaimMethod::Unlink {
                path: PathBuf::from("/x")
            }
            .is_irreversible()
        );
        assert!(
            ReclaimMethod::JournalVacuum {
                limit: VacuumLimit::Size { mebibytes: 500 }
            }
            .is_irreversible(),
            "vacuuming the journal cannot be undone"
        );
        assert!(
            ReclaimMethod::SystemFile {
                kind: ReclaimKind::RotatedLog,
                path: PathBuf::from("/var/log/x.1.gz")
            }
            .is_irreversible()
        );
    }

    /// The privileged methods carry typed values, not text. If any of these becomes a `String`,
    /// caller-supplied text can reach a root command line.
    #[test]
    fn privileged_methods_carry_no_free_form_text() {
        // Constructing them requires an enum or a number — there is no string to smuggle.
        let _ = ReclaimMethod::PackageManager {
            manager: Manager::Apt,
        };
        let _ = ReclaimMethod::JournalVacuum {
            limit: VacuumLimit::Age { days: 7 },
        };
        let _ = ReclaimMethod::SystemFile {
            kind: ReclaimKind::PackageCache,
            path: PathBuf::from("/var/cache/apt/archives/x.deb"),
        };

        // And the wire form keeps them typed rather than collapsing to text.
        let json = serde_json::to_string(&ReclaimMethod::PackageManager {
            manager: Manager::Pacman,
        })
        .unwrap();
        assert!(json.contains("\"pacman\""), "{json}");
        assert!(
            !json.contains("-Sc"),
            "no command text should cross the wire: {json}"
        );
    }

    // ---- aggregation ----

    #[test]
    fn category_totals_count_leaves_only() {
        let mut tree = SpaceTree::new();
        let root = tree.insert_root(dir("/c", 300));
        let mut a = file("/c/a", 100);
        a.category = Category::AppCache;
        let mut b = file("/c/b", 200);
        b.category = Category::AppCache;
        let a = tree.insert(a);
        let b = tree.insert(b);
        tree.attach(root, a);
        tree.attach(root, b);

        let totals = tree.allocated_by_category();
        // The directory itself is not counted: only its leaves, so bytes appear exactly once.
        assert_eq!(totals.get(&Category::AppCache).copied(), Some(300));
        assert_eq!(totals.get(&Category::Unknown).copied(), None);
        assert_eq!(tree.total_allocated(), 300);
    }

    #[test]
    fn walked_entries_start_unattributed_and_unreclaimable() {
        let e = SpaceEntry::walked(PathBuf::from("/home/me/thing.iso"), 4096, 4096, false);
        assert_eq!(e.category, Category::Unknown);
        assert_eq!(
            e.safety,
            Safety::Never,
            "nothing is reclaimable until classified"
        );
        assert!(e.reclaim.is_none());
        assert_eq!(e.label, "thing.iso");
    }

    #[test]
    fn entry_ids_cross_the_wire_as_strings_in_every_position() {
        let mut tree = SpaceTree::new();
        let root = tree.insert_root(dir("/data", 10));
        let child = tree.insert(file("/data/a", 10));
        tree.attach(root, child);

        let json = serde_json::to_value(&tree).unwrap();
        // As a value.
        let roots = json["roots"].as_array().unwrap();
        assert!(
            roots[0].is_string(),
            "roots must hold strings, got {:?}",
            roots[0]
        );
        // As a map key — always a string in JSON, which is why values match it.
        let entries = json["entries"].as_object().unwrap();
        assert!(
            entries.keys().all(|k| k.len() == 16),
            "keys are 16-char hex: {:?}",
            entries.keys()
        );

        let back: SpaceTree = serde_json::from_value(json).unwrap();
        assert_eq!(tree, back);
    }

    #[test]
    fn a_malformed_entry_id_is_rejected_rather_than_defaulted() {
        let err = serde_json::from_str::<EntryId>("\"not hex\"").unwrap_err();
        assert!(err.to_string().contains("invalid entry id"), "{err}");
    }

    #[test]
    fn tree_round_trips_over_the_wire() {
        let mut tree = SpaceTree::new();
        let root = tree.insert_root(dir("/data", 100));
        let child = tree.insert(file("/data/a", 100));
        tree.attach(root, child);

        let json = serde_json::to_string(&tree).unwrap();
        let back: SpaceTree = serde_json::from_str(&json).unwrap();
        assert_eq!(tree, back);
    }
}
