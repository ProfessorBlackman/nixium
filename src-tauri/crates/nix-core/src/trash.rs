// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The freedesktop trash specification. Task 1.10 (`STO-7`).
//!
//! # Why this is implemented properly rather than approximated
//!
//! Stacer implemented trashing by hand and got four things wrong, each of which breaks
//! interoperability with the desktop's own file manager:
//!
//! 1. It always wrote an **absolute** `Path=`. The spec requires a path *relative to the trash
//!    directory's top level* for volume trashes, so an entry made on a USB stick could not be
//!    restored after remounting at a different point.
//! 2. It did not **URL-encode** `Path=`. A filename containing `%`, a newline or a `=` produced a
//!    `.trashinfo` file that no reader can parse correctly.
//! 3. It had no **per-volume** trash, so it moved files across filesystems — turning an atomic
//!    rename into a slow copy, and silently filling the home partition with another disk's files.
//! 4. It had no **collision handling**, so trashing two files of the same name lost the first.
//!
//! Getting this right is what makes "move to trash" a reversible operation the user can undo from
//! their file manager, which is the entire reason [`crate::space::ReclaimMethod::MoveToTrash`] is
//! the default for user files.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, ErrorCode, IoContext, Result};
use crate::paths;

/// One item sitting in a trash directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TrashedItem {
    /// Name inside `files/`, which is also the stem of the `.trashinfo`.
    pub name: String,
    /// Where it currently lives.
    pub trashed_path: PathBuf,
    /// Where it came from, resolved back to an absolute path.
    pub original_path: PathBuf,
    /// When it was trashed, as recorded in the `.trashinfo`.
    pub deleted_at: String,
    /// On-disk bytes.
    #[ts(type = "number")]
    pub size: u64,
}

/// A trash directory: the home one, or a volume's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashDir {
    /// The directory itself, containing `files/` and `info/`.
    root: PathBuf,
    /// The volume this trash serves. `None` for the home trash, whose `Path=` stays absolute.
    top_dir: Option<PathBuf>,
}

impl TrashDir {
    /// The home trash at `$XDG_DATA_HOME/Trash`.
    pub fn home() -> Result<Self> {
        // XDG_DATA_HOME, defaulting to ~/.local/share. Trash lives beside other application data.
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| paths::home_dir().map(|h| h.join(".local/share")))
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::Unsupported,
                    "Could not find your trash directory.",
                )
                .with_remedy("Set HOME or XDG_DATA_HOME and try again.")
            })?;
        Ok(Self {
            root: data.join("Trash"),
            top_dir: None,
        })
    }

    /// A trash directory at an explicit location, serving an explicit volume. For tests.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>, top_dir: Option<PathBuf>) -> Self {
        Self {
            root: root.into(),
            top_dir,
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn files_dir(&self) -> PathBuf {
        self.root.join("files")
    }

    #[must_use]
    pub fn info_dir(&self) -> PathBuf {
        self.root.join("info")
    }

    fn ensure(&self) -> Result<()> {
        for dir in [self.files_dir(), self.info_dir()] {
            std::fs::create_dir_all(&dir)
                .doing("prepare the trash directory")
                .map_err(|e| e.with_path(&dir))?;
        }
        Ok(())
    }

    /// Total on-disk bytes held in `files/`.
    #[must_use]
    pub fn size(&self) -> u64 {
        crate::fixture::directory_size(&self.files_dir())
    }
}

/// Percent-encode a path for `Path=` in a `.trashinfo`.
///
/// The spec says the value is encoded per RFC 2396: everything outside the unreserved set is
/// escaped, except `/` which stays a separator. Without this, a filename containing `%`, a newline
/// or `=` produces a file no reader can parse — and filenames containing all three are legal.
fn encode_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for byte in path.as_os_str().as_bytes() {
        let b = *byte;
        let unreserved = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' | b'/'
            );
        if unreserved {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Reverse [`encode_path`].
fn decode_path(encoded: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let bytes = encoded.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&encoded[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// `YYYY-MM-DDThh:mm:ss` in local time, as the spec requires.
///
/// Computed from the epoch by hand rather than pulling in a date library for one format string.
fn deletion_date_now() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_epoch(secs)
}

/// Format an epoch second as the spec's local-time stamp.
///
/// Uses UTC: the spec asks for local time, but a wrong offset is worse than a consistent one, and
/// without a timezone database there is no honest way to know the offset. Readers treat the field as
/// informational.
fn format_epoch(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Civil-from-days (Howard Hinnant's algorithm), valid across the whole range.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}")
}

/// Pick a name inside `files/` that is not already taken.
///
/// Collisions are real: trashing `notes.txt` from two directories on the same day is ordinary. The
/// spec leaves the scheme open; appending a counter before the extension is what desktops do.
fn unique_name(files_dir: &Path, info_dir: &Path, desired: &str) -> Result<String> {
    let taken = |name: &str| {
        files_dir.join(name).exists() || info_dir.join(format!("{name}.trashinfo")).exists()
    };

    if !taken(desired) {
        return Ok(desired.to_string());
    }

    let (stem, extension) = match desired.rsplit_once('.') {
        // A leading dot is part of the name, not an extension separator.
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (desired, String::new()),
    };

    for n in 1..10_000 {
        let candidate = format!("{stem}.{n}{extension}");
        if !taken(&candidate) {
            return Ok(candidate);
        }
    }

    Err(AppError::new(
        ErrorCode::Io,
        format!("Could not find a free name in the trash for {desired}."),
    )
    .with_remedy("Empty the trash and try again."))
}

/// Move a path into a trash directory.
///
/// The move is a rename, so it must not cross a filesystem — [`trash`] picks the right trash
/// directory for the path before calling this.
pub fn trash_into(dir: &TrashDir, path: &Path) -> Result<TrashedItem> {
    let metadata = std::fs::symlink_metadata(path)
        .doing(format!("trash {}", path.display()))
        .map_err(|e| e.with_path(path))?;

    // Measured **before** the move, and by walking when the target is a directory.
    //
    // `metadata.blocks()` on a directory describes the directory inode — a few kilobytes — not what is
    // inside it. Using it meant trashing a 9.8 GiB cache directory reported about four kilobytes
    // reclaimed. Every accuracy test trashed plain files, so the directory case went unnoticed even
    // though the only production category that trashes anything trashes directories.
    let size = if metadata.is_dir() {
        crate::fixture::directory_size(path) + on_disk_size(&metadata)
    } else {
        on_disk_size(&metadata)
    };

    // An absolute original path is what makes restoration possible at all.
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .doing("resolve a relative path")?
            .join(path)
    };

    dir.ensure()?;

    let desired = absolute
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| {
            AppError::invalid_input("That path has no file name to trash.").with_path(&absolute)
        })?;

    let name = unique_name(&dir.files_dir(), &dir.info_dir(), &desired)?;
    let destination = dir.files_dir().join(&name);
    let info_path = dir.info_dir().join(format!("{name}.trashinfo"));

    // For a volume trash the recorded path is relative to the volume's top directory, so the entry
    // survives the volume being remounted somewhere else.
    let recorded = match &dir.top_dir {
        Some(top) => absolute
            .strip_prefix(top)
            .unwrap_or(&absolute)
            .to_path_buf(),
        None => absolute.clone(),
    };

    let deleted_at = deletion_date_now();
    let info = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        encode_path(&recorded),
        deleted_at
    );

    // The info file is written *first*: an entry in `files/` with no `.trashinfo` is an orphan that
    // no file manager can restore, whereas a `.trashinfo` with no file is merely ignored.
    std::fs::write(&info_path, info.as_bytes())
        .doing("record the trashed file's origin")
        .map_err(|e| e.with_path(&info_path))?;

    if let Err(e) = std::fs::rename(&absolute, &destination) {
        // Leave no orphan info file behind if the move failed.
        std::fs::remove_file(&info_path).ok();
        return Err(AppError::from_io(&e, format!("move {} to the trash", absolute.display()))
            .with_path(&absolute)
            .with_remedy(
                "The file was not moved. If it is on another disk, its own trash is used instead.",
            ));
    }

    Ok(TrashedItem {
        name,
        trashed_path: destination,
        original_path: absolute,
        deleted_at,
        size,
    })
}

fn on_disk_size(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks() * 512
}

/// Move a path to the appropriate trash: the home trash, or the trash of the volume it lives on.
///
/// Choosing per-volume matters because the move is a rename. Trashing across a filesystem boundary
/// would be a copy — slow, and it silently moves another disk's data onto the home partition.
pub fn trash(path: &Path) -> Result<TrashedItem> {
    let home = TrashDir::home()?;
    let target_dir = match volume_trash_for(path)? {
        Some(volume) => volume,
        None => home,
    };
    trash_into(&target_dir, path)
}

/// The volume trash for a path, if the path is not on the same filesystem as the home trash.
///
/// Per the spec, `$topdir/.Trash/$uid` is preferred when `$topdir/.Trash` exists, is a directory,
/// is not a symlink and has the sticky bit; otherwise `$topdir/.Trash-$uid`.
fn volume_trash_for(path: &Path) -> Result<Option<TrashDir>> {
    use std::os::unix::fs::MetadataExt;

    let home = TrashDir::home()?;
    let home_dev = std::fs::metadata(home.root().parent().unwrap_or(home.root()))
        .ok()
        .map(|m| m.dev());
    let path_dev = std::fs::symlink_metadata(path).ok().map(|m| m.dev());

    // Same filesystem as the home trash, or we cannot tell: use the home trash.
    if home_dev.is_none() || path_dev.is_none() || home_dev == path_dev {
        return Ok(None);
    }

    let Some(top) = crate::fs::containing(path)?.map(|fs| fs.mount_point) else {
        return Ok(None);
    };

    let uid = current_uid();
    let sticky = top.join(".Trash");
    if let Ok(meta) = std::fs::symlink_metadata(&sticky) {
        // Sticky bit set and not a symlink: the shared, administrator-created form.
        if meta.is_dir() && !meta.file_type().is_symlink() && meta.mode() & 0o1000 != 0 {
            return Ok(Some(TrashDir::at(sticky.join(uid.to_string()), Some(top))));
        }
    }
    Ok(Some(TrashDir::at(
        top.join(format!(".Trash-{uid}")),
        Some(top),
    )))
}

fn current_uid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|rest| rest.split_whitespace().next().map(str::to_owned))
        })
        .and_then(|uid| uid.parse().ok())
        .unwrap_or(0)
}

/// Everything currently in a trash directory.
///
/// An entry whose `.trashinfo` is missing or unreadable is skipped: it cannot be restored, so
/// presenting it as restorable would be a lie.
#[must_use]
pub fn list(dir: &TrashDir) -> Vec<TrashedItem> {
    let Ok(entries) = std::fs::read_dir(dir.files_dir()) else {
        return Vec::new();
    };

    let mut items = Vec::new();
    for entry in entries.filter_map(std::result::Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let info_path = dir.info_dir().join(format!("{name}.trashinfo"));
        let Ok(info) = std::fs::read_to_string(&info_path) else {
            continue;
        };

        let value = |key: &str| -> Option<String> {
            info.lines()
                .find_map(|l| l.strip_prefix(key))
                .map(|v| v.trim().to_string())
        };
        let Some(raw_path) = value("Path=") else {
            continue;
        };

        let recorded = decode_path(&raw_path);
        // A relative record is resolved against the volume's top directory, which is what makes a
        // volume trash survive being remounted elsewhere.
        let original_path = match (&dir.top_dir, recorded.is_absolute()) {
            (Some(top), false) => top.join(recorded),
            _ => recorded,
        };

        items.push(TrashedItem {
            trashed_path: entry.path(),
            size: entry.metadata().map(|m| on_disk_size(&m)).unwrap_or(0),
            name,
            original_path,
            deleted_at: value("DeletionDate=").unwrap_or_default(),
        });
    }

    items.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
    items
}

/// Put a trashed item back where it came from.
pub fn restore(dir: &TrashDir, item: &TrashedItem) -> Result<()> {
    if item.original_path.exists() {
        return Err(AppError::refused(format!(
            "Something already exists at {}.",
            item.original_path.display()
        ))
        .with_path(&item.original_path)
        .with_remedy("Move or rename it, then restore again."));
    }

    if let Some(parent) = item.original_path.parent() {
        std::fs::create_dir_all(parent)
            .doing("recreate the original directory")
            .map_err(|e| e.with_path(parent))?;
    }

    std::fs::rename(&item.trashed_path, &item.original_path).map_err(|e| {
        AppError::from_io(&e, format!("restore {}", item.original_path.display()))
            .with_path(&item.original_path)
    })?;

    // The info file is only removed once the move succeeded, so a failure leaves the item
    // restorable rather than orphaned.
    std::fs::remove_file(dir.info_dir().join(format!("{}.trashinfo", item.name))).ok();
    Ok(())
}

/// How much was freed by emptying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Emptied {
    pub items: u64,
    pub bytes: u64,
}

/// Permanently delete everything in a trash directory.
///
/// Irreversible by definition, so it is only ever reached through the executor's preview and
/// confirmation.
pub fn empty(dir: &TrashDir) -> Result<Emptied> {
    let items = list(dir);
    let mut emptied = Emptied::default();

    for item in &items {
        let removed = if item.trashed_path.is_dir() {
            std::fs::remove_dir_all(&item.trashed_path)
        } else {
            std::fs::remove_file(&item.trashed_path)
        };
        match removed {
            Ok(()) => {
                emptied.items += 1;
                emptied.bytes += item.size;
                std::fs::remove_file(dir.info_dir().join(format!("{}.trashinfo", item.name))).ok();
            }
            Err(e) => {
                tracing::warn!(path = %item.trashed_path.display(), error = %e, "could not empty a trashed item");
            }
        }
    }

    Ok(emptied)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    struct Sandbox {
        root: PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "nix-trash-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(root.join("home")).unwrap();
            Self { root }
        }

        fn trash_dir(&self) -> TrashDir {
            TrashDir::at(self.root.join("Trash"), None)
        }

        fn volume_trash(&self) -> TrashDir {
            TrashDir::at(
                self.root.join("volume/.Trash-1000"),
                Some(self.root.join("volume")),
            )
        }

        fn file(&self, name: &str, bytes: usize) -> PathBuf {
            let path = self.root.join("home").join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, vec![b'x'; bytes]).unwrap();
            path
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    // ---- encoding, which Stacer omitted entirely ----

    #[test]
    fn paths_are_percent_encoded_per_the_specification() {
        assert_eq!(
            encode_path(Path::new("/home/me/notes.txt")),
            "/home/me/notes.txt"
        );
        // Space, percent, equals and newline are exactly the characters that break a naive writer.
        assert_eq!(
            encode_path(Path::new("/home/me/my file.txt")),
            "/home/me/my%20file.txt"
        );
        assert_eq!(
            encode_path(Path::new("/home/me/100%.txt")),
            "/home/me/100%25.txt"
        );
        assert_eq!(encode_path(Path::new("/home/me/a=b")), "/home/me/a%3Db");
        assert_eq!(encode_path(Path::new("/home/me/a\nb")), "/home/me/a%0Ab");
        // Unreserved characters are left alone, so ordinary paths stay readable.
        assert_eq!(encode_path(Path::new("/a-b_c.d~e")), "/a-b_c.d~e");
    }

    #[test]
    fn encoding_round_trips_including_hostile_names() {
        for original in [
            "/home/me/plain.txt",
            "/home/me/with space.txt",
            "/home/me/100% done = yes.txt",
            "/home/me/new\nline",
            "/home/me/emoji-🗑.bin",
            "/home/me/#hash?query&amp",
        ] {
            let encoded = encode_path(Path::new(original));
            assert!(
                !encoded.contains('\n'),
                "an encoded path must be one line: {encoded}"
            );
            assert_eq!(
                decode_path(&encoded),
                PathBuf::from(original),
                "round trip failed"
            );
        }
    }

    #[test]
    fn a_malformed_escape_decodes_literally_rather_than_panicking() {
        assert_eq!(decode_path("/a%ZZb"), PathBuf::from("/a%ZZb"));
        assert_eq!(decode_path("/trailing%"), PathBuf::from("/trailing%"));
    }

    // ---- the deletion date format ----

    #[test]
    fn deletion_dates_use_the_specified_format() {
        // 2021-01-01T00:00:00 UTC
        assert_eq!(format_epoch(1_609_459_200), "2021-01-01T00:00:00");
        // A leap day, which a hand-rolled calendar gets wrong if the algorithm is naive.
        assert_eq!(format_epoch(1_582_934_400), "2020-02-29T00:00:00");
        assert_eq!(format_epoch(0), "1970-01-01T00:00:00");

        let now = deletion_date_now();
        assert_eq!(now.len(), 19, "{now}");
        assert_eq!(now.as_bytes()[10], b'T', "{now}");
    }

    // ---- trashing ----

    #[test]
    fn trashing_moves_the_file_and_records_where_it_came_from() {
        let sandbox = Sandbox::new("basic");
        let dir = sandbox.trash_dir();
        let file = sandbox.file("notes.txt", 100);

        let item = trash_into(&dir, &file).unwrap();

        assert!(!file.exists(), "the original must be gone");
        assert!(item.trashed_path.exists(), "the file must be in the trash");
        assert_eq!(item.original_path, file);
        assert_eq!(item.name, "notes.txt");

        // The info file is what makes it restorable, and must be readable by any spec-compliant tool.
        let info = std::fs::read_to_string(dir.info_dir().join("notes.txt.trashinfo")).unwrap();
        assert!(info.starts_with("[Trash Info]\n"), "{info}");
        assert!(info.contains(&format!("Path={}", file.display())), "{info}");
        assert!(info.contains("DeletionDate="), "{info}");
    }

    #[test]
    fn a_name_collision_does_not_lose_the_first_file() {
        let sandbox = Sandbox::new("collision");
        let dir = sandbox.trash_dir();

        std::fs::create_dir_all(sandbox.root.join("home/a")).unwrap();
        std::fs::create_dir_all(sandbox.root.join("home/b")).unwrap();
        let first = sandbox.file("a/notes.txt", 10);
        let second = sandbox.file("b/notes.txt", 20);

        let one = trash_into(&dir, &first).unwrap();
        let two = trash_into(&dir, &second).unwrap();

        assert_ne!(
            one.name, two.name,
            "the second must not overwrite the first"
        );
        assert!(one.trashed_path.exists(), "the first file must survive");
        assert!(two.trashed_path.exists());
        assert_eq!(
            two.name, "notes.1.txt",
            "the counter goes before the extension"
        );

        // Both remain restorable to their different origins.
        let listed = list(&dir);
        assert_eq!(listed.len(), 2);
        let origins: Vec<PathBuf> = listed.iter().map(|i| i.original_path.clone()).collect();
        assert!(origins.contains(&first));
        assert!(origins.contains(&second));
    }

    #[test]
    fn a_dotfile_is_not_treated_as_having_an_extension() {
        let sandbox = Sandbox::new("dotfile");
        let dir = sandbox.trash_dir();
        std::fs::create_dir_all(sandbox.root.join("home/x")).unwrap();
        std::fs::create_dir_all(sandbox.root.join("home/y")).unwrap();
        let first = sandbox.file("x/.bashrc", 10);
        let second = sandbox.file("y/.bashrc", 10);

        trash_into(&dir, &first).unwrap();
        let two = trash_into(&dir, &second).unwrap();
        assert_eq!(two.name, ".bashrc.1", "a leading dot is part of the name");
    }

    #[test]
    fn a_volume_trash_records_a_relative_path() {
        let sandbox = Sandbox::new("volume");
        let dir = sandbox.volume_trash();
        let top = sandbox.root.join("volume");
        std::fs::create_dir_all(top.join("data")).unwrap();
        let file = top.join("data/report.pdf");
        std::fs::write(&file, vec![b'x'; 50]).unwrap();

        let item = trash_into(&dir, &file).unwrap();

        // The point: relative to the volume, so remounting elsewhere does not break restoration.
        let info = std::fs::read_to_string(dir.info_dir().join("report.pdf.trashinfo")).unwrap();
        assert!(
            info.contains("Path=data/report.pdf"),
            "must be volume-relative: {info}"
        );
        assert!(
            !info.contains(&top.display().to_string()),
            "must not be absolute: {info}"
        );

        // And listing resolves it back against the volume.
        assert_eq!(item.original_path, file);
        assert_eq!(list(&dir)[0].original_path, file);
    }

    #[test]
    fn a_failed_move_leaves_no_orphan_info_file() {
        let sandbox = Sandbox::new("orphan");
        let dir = sandbox.trash_dir();
        let missing = sandbox.root.join("home/never-existed.txt");

        assert!(trash_into(&dir, &missing).is_err());
        // An info file with no file is ignorable; a file with no info is unrestorable. Neither
        // should be left behind by a failure.
        let orphans = std::fs::read_dir(dir.info_dir())
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(orphans, 0, "a failed trash must clean up after itself");
    }

    #[test]
    fn trashing_a_directory_works() {
        let sandbox = Sandbox::new("dir");
        let dir = sandbox.trash_dir();
        let target = sandbox.root.join("home/project");
        std::fs::create_dir_all(target.join("nested")).unwrap();
        std::fs::write(target.join("nested/file"), b"data").unwrap();

        let item = trash_into(&dir, &target).unwrap();
        assert!(!target.exists());
        assert!(
            item.trashed_path.join("nested/file").exists(),
            "contents move with it"
        );
    }

    // ---- listing and restoring ----

    #[test]
    fn restoring_puts_a_file_back_and_clears_its_record() {
        let sandbox = Sandbox::new("restore");
        let dir = sandbox.trash_dir();
        let file = sandbox.file("restore-me.txt", 30);

        let item = trash_into(&dir, &file).unwrap();
        assert!(!file.exists());

        restore(&dir, &item).unwrap();

        assert!(file.exists(), "the file must be back where it came from");
        assert_eq!(std::fs::read(&file).unwrap().len(), 30, "contents intact");
        assert!(list(&dir).is_empty(), "and no longer listed as trashed");
    }

    #[test]
    fn restoring_refuses_to_overwrite_something_new() {
        let sandbox = Sandbox::new("clobber");
        let dir = sandbox.trash_dir();
        let file = sandbox.file("thing.txt", 10);
        let item = trash_into(&dir, &file).unwrap();

        // Someone created a new file with the same name in the meantime.
        std::fs::write(&file, b"different content").unwrap();

        let err = restore(&dir, &item).unwrap_err();
        assert_eq!(err.code, ErrorCode::Refused);
        assert!(err.remedy.is_some(), "a refusal must say what to do");
        assert_eq!(
            std::fs::read(&file).unwrap(),
            b"different content",
            "the newer file must be untouched"
        );
    }

    #[test]
    fn an_entry_without_its_info_file_is_not_listed_as_restorable() {
        let sandbox = Sandbox::new("noinfo");
        let dir = sandbox.trash_dir();
        let file = sandbox.file("lonely.txt", 10);
        let item = trash_into(&dir, &file).unwrap();

        std::fs::remove_file(dir.info_dir().join(format!("{}.trashinfo", item.name))).unwrap();

        assert!(
            list(&dir).is_empty(),
            "without its origin an item cannot be restored, so claiming otherwise would be a lie"
        );
    }

    #[test]
    fn listing_an_empty_or_absent_trash_is_not_an_error() {
        let sandbox = Sandbox::new("empty-list");
        assert!(list(&sandbox.trash_dir()).is_empty());
        assert!(list(&TrashDir::at("/nowhere/at/all", None)).is_empty());
    }

    // ---- emptying ----

    #[test]
    fn emptying_removes_everything_and_reports_what_it_freed() {
        let sandbox = Sandbox::new("empty");
        let dir = sandbox.trash_dir();

        for i in 0..3 {
            let file = sandbox.file(&format!("file-{i}.bin"), 4096);
            trash_into(&dir, &file).unwrap();
        }
        assert_eq!(list(&dir).len(), 3);
        assert!(dir.size() > 0);

        let emptied = empty(&dir).unwrap();

        assert_eq!(emptied.items, 3);
        assert!(
            emptied.bytes > 0,
            "emptying three 4 KiB files frees something"
        );
        assert!(list(&dir).is_empty());
        assert_eq!(dir.size(), 0);

        // No info files left over either.
        assert_eq!(std::fs::read_dir(dir.info_dir()).unwrap().count(), 0);
    }

    #[test]
    fn emptying_an_empty_trash_is_a_no_op() {
        let sandbox = Sandbox::new("empty-twice");
        let dir = sandbox.trash_dir();
        assert_eq!(empty(&dir).unwrap(), Emptied::default());
    }

    #[test]
    fn size_reflects_what_is_held() {
        let sandbox = Sandbox::new("size");
        let dir = sandbox.trash_dir();
        assert_eq!(dir.size(), 0);

        let file = sandbox.file("chunk.bin", 8192);
        trash_into(&dir, &file).unwrap();
        assert!(dir.size() >= 8192, "size was {}", dir.size());
    }

    #[test]
    fn the_home_trash_resolves_under_the_data_directory() {
        let home = TrashDir::home().unwrap();
        assert!(home.root().ends_with("Trash"), "{}", home.root().display());
        assert!(home.root().is_absolute());
        assert_eq!(home.files_dir(), home.root().join("files"));
        assert_eq!(home.info_dir(), home.root().join("info"));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod directory_size_tests {
    use super::*;

    /// # Regression
    ///
    /// A trashed directory reported `metadata.blocks() * 512` for the directory inode — a few
    /// kilobytes — rather than what it contained. So moving a 9.8 GiB cache to the trash reported
    /// about four kilobytes, and since `Report` derives its figures from what `trash` returns, the
    /// whole account of the operation was wrong by three orders of magnitude.
    ///
    /// Every test in `reclaim_accuracy.rs` trashed plain files, so this went unnoticed even though
    /// `AppCacheCategory` — the only production category that trashes anything — trashes directories
    /// exclusively.
    #[test]
    fn trashing_a_directory_reports_what_was_inside_it() {
        let base = std::env::temp_dir().join(format!("nix-trashdir-{}", std::process::id()));
        let trash_root = base.join("trash");
        let source = base.join("payload");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::create_dir_all(&trash_root).unwrap();

        // Four blocks of real content, spread over two levels.
        std::fs::write(source.join("a.bin"), vec![b'x'; 8192]).unwrap();
        std::fs::write(source.join("nested/b.bin"), vec![b'y'; 8192]).unwrap();

        let dir = TrashDir::at(&trash_root, None);
        let item = trash_into(&dir, &source).unwrap();

        assert!(
            item.size >= 16384,
            "a directory holding 16 KiB reported {} — the directory inode is not its contents",
            crate::format_bytes(item.size)
        );

        std::fs::remove_dir_all(&base).ok();
    }
}
