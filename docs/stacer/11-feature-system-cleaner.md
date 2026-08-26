# 11 — Feature: System Cleaner

**Files:** `stacer/Pages/SystemCleaner/{system_cleaner_page,byte_tree_widget}.{h,cpp,ui}`
**Window title:** `System Cleaner` · **Sidebar button:** `btnSystemCleaner`

The most destructive page in the app: it enumerates disposable files and deletes the selected
ones with `pkexec rm -rf`.

## Two-step flow

A `QStackedWidget` with two indices:

- **Index 0 — categories.** Five checkboxes plus a "select all", and a Scan button.
- **Index 1 — results.** A two-column tree of found files, a sort combo, a "select all",
  a total-size label, a Clean button, a cleaned-size label, and a Back button.

## Categories

```cpp
enum CleanCategories { PACKAGE_CACHE, CRASH_REPORTS, APPLICATION_LOGS, APPLICATION_CACHES, TRASH };
```

| Category | Checkbox | Enumerated by | Path |
| --- | --- | --- | --- |
| Package cache | `checkPackageCache` | `ToolManager::getPackageCaches()` | `/var/cache/apt/archives/` (APT) or `/var/cache/pacman/pkg/` |
| Crash reports | `checkCrashReports` | `SystemInfo::getCrashReports()` | `/var/crash` (files) |
| Application logs | `checkAppLog` | `SystemInfo::getAppLogs()` | `/var/log` (**files only**, no subdirectories) |
| Application caches | `checkAppCache` | `SystemInfo::getAppCaches()` | `~/.cache` (files **and** directories) |
| Trash | `checkTrash` | hard-coded | `~/.local/share/Trash/` |

`ToolManager::getPackageCaches()` switches on `PackageTool::currentPackageTool`; note the
DNF/YUM branch returns the **pacman** cache directory (a copy-paste bug — see
[20-known-quirks-and-bugs.md](20-known-quirks-and-bugs.md)).

## Scanning

`on_btnScan_clicked()` → `QtConcurrent::run(this, &SystemCleanerPage::systemScan)`.

`systemScan()` runs **on a worker thread but manipulates widgets directly** — hiding the scan
button, disabling checkboxes, clearing and repopulating the `QTreeWidget`, switching the stacked
page. This is a Qt threading violation that happens to work most of the time; the constructor
even registers metatypes explicitly "to suppress qt warnings (signal/slot <> threads)".

For each enabled category it calls `addTreeRoot(cat, title, QFileInfoList, noChild = false)`:

```
root item:
  data(2,0) = category enum
  data(2,1) = title
  data(3,0) = parent directory of the first entry     # used to recompute size after cleaning
  checkState(0) = Unchecked
  text(0) = "<title> (<file count>)"
  text(1) = formatBytes(sum of child sizes)
children (one per QFileInfo):
  ByteTreeWidget with text = file name, size, and data(2,0) = absolute path
  icon = QIcon::fromTheme(fileName, "application-x-executable")
```

Trash is added with `noChild = true`: a single root row whose size is the recursive size of
`~/.local/share/Trash/`, with no children.

Sizes come from `FileUtil::getFileSize()`, which recurses into directories — so scanning
`~/.cache` walks the entire cache tree synchronously. This is the slow part of a scan.

After population: total size label (`tr("Total size: %1")`), sorting re-enabled, the current
sort applied, page switched to results, and all five category checkboxes reset to unchecked.

### Sorting

`cbSortBy` maps to `QTreeWidget::sortItems`:

| Index | Column | Order |
| --- | --- | --- |
| 0 | 0 (name) | ascending |
| 1 | 0 (name) | descending |
| 2 | 1 (size) | ascending |
| 3 | 1 (size) | descending |

`ByteTreeWidget::operator<` makes column 1 sort by the raw byte count stored in role `0x0100`
rather than by the formatted string; other columns fall back to case-insensitive text compare.

### Check-state propagation

`on_treeWidgetScanResult_itemClicked(item, column)` — when column 0 is clicked, copy the item's
new check state down to all of its children. Parent state is not derived from children (no
tri-state), and `on_checkSelectAll_clicked` sets every root and child directly.

## Cleaning

`on_btnClean_clicked()` → `QtConcurrent::run(this, &SystemCleanerPage::systemClean)` (again
touching widgets off-thread).

`cleanValid()` first verifies at least one root or child is checked.

Then:

1. Walk all roots. For non-`TRASH` categories, collect `data(2,0)` (absolute path) from every
   **checked child** into `filesToDelete`, remembering the `QTreeWidgetItem*`s.
2. For a checked `TRASH` root, delete `~/.local/share/Trash/files` and
   `~/.local/share/Trash/info` with `QDir::removeRecursively()` — unprivileged, no `rm`.
3. Sum `FileUtil::getFileSize()` over `filesToDelete` **before** deleting (that sum is the
   "cleaned" figure reported).
4. `CommandUtil::sudoExec("rm", {"-rf"} + filesToDelete)` — a **single** `pkexec rm -rf` with
   every selected path as an argument, so one authentication prompt for the whole operation.
5. Remove the deleted children from the tree (an O(roots × children) nested loop that removes
   each collected item from every root).
6. Recompute each root's label as `"<title> (<childCount>)"` and its size as
   `formatBytes(getFileSize(data(3,0)))` — i.e. the size of the *parent directory*, not the sum
   of remaining children.
7. Show `tr("%1 size files cleaned.")` and re-enable the UI.

Note there is **no confirmation dialog**, no dry run, no exclusion list, and no check that the
selected paths are safe to remove. Selecting everything under "Application logs" deletes files
that running daemons hold open, and everything under "Application caches" deletes whole
directories under `~/.cache`.

## Back navigation

`on_btnBackToCategories_clicked()` restores the scan button, clears the tree and the cleaned-size
label, re-enables all category checkboxes, and returns to page 0.

## Loading animations

Created inside the `sigChangedAppTheme` handler (because their paths are theme-interpolated):
`scanLoading.gif` on `lblLoadingScanner`, `loading.gif` on `lblLoadingCleaner`. Both `QMovie`s
are started immediately and their labels hidden; the labels are shown in place of the
scan/clean buttons while work runs. Each theme change **leaks a new `QMovie`** (the old one is
parented to `this` and never deleted).

## Port notes

- Enumerate and size in the backend (async, cancellable, with progress); the UI should never
  block on a `~/.cache` walk.
- Delete via a privileged helper that receives an explicit path list and validates each path
  against the category it came from — never a raw `rm -rf` argument list.
- Add a confirmation summary and protect obviously dangerous selections.
- `/var/log` handling should understand rotated logs (`*.gz`, `*.1`) and journald rather than
  "every regular file in /var/log".
- Package cache cleaning should use the package manager's own command
  (`apt-get clean`, `pacman -Sc`, `dnf clean packages`) instead of unlinking cache files.
- Trash deletion should follow the freedesktop trash spec (and consider `gio trash --empty`).
