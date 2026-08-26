# 15 — Feature: Search

**Files:** `stacer/Pages/Search/search_page.{h,cpp,ui}`
**Window title:** `Search` · **Sidebar button:** `btnSearch`

A GUI front-end for `find(1)`: it composes an argument vector from form controls, runs `find`,
and shows the results in a table with file-management actions.

## Controls → `find` arguments

Built in `searching()`, in this exact order:

| Control | Emitted arguments |
| --- | --- |
| browsed directory (`mSelectedDirectory`) | first positional argument (**required**) |
| `txtSearchInput` + `checkCaseInsensitive` + `checkRegEx` | `-name` \| `-iname` \| `-regex` \| `-iregex` followed by the pattern |
| `checkInvert` | `-invert` ← **not a real `find` predicate** (see quirks) |
| `checkEmpty` | `-empty` |
| `cmbSearchTypes` | `-type f\|d\|l` (skipped when "All") |
| `cmbTimeType` + `cmbTimeCriteria` + `spinTime` | `-amin\|-mmin\|-cmin` then `[-+]?<minutes>` |
| `checkPermReadable` | `-readable` |
| `checkPermWritable` | `-writable` |
| `checkPermExecutable` | `-executable` |
| `cmbSizeCriteria` + `spinSize` + `cmbSizeUnits` | `-size` then `[-+]?<n><c\|k\|M\|G>` |
| `cmbUsers` | `-user <name>` |
| `cmbGroups` | `-group <name>` |
| `checkSearchAsRoot` | runs via `pkexec find …` instead of `find …` |

Combo box contents (`initComboboxValues()`):

- Users: `tr("Choose")` + `/etc/passwd` names (`InfoManager::getUserList()`).
- Groups: `tr("Choose")` + `/etc/group` names.
- Types: All / File (`f`) / Directory (`d`) / Symbolic Link (`l`).
- Time type: Choose / Access (`-amin`) / Modify (`-mmin`) / Change (`-cmin`).
- Time criteria: Smaller (`-`) / Equal (``) / Greater (`+`).
- Size criteria: Choose / Smaller (`-`) / Equal (``) / Greater (`+`).
- Size units: Bytes (`c`) / Kibibytes (`k`) / Mebibytes (`M`) / Gibibytes (`G`).

Sentinel `"-1"` in item data means "not set". The advanced pane
(`advanceSearchPane`) is collapsed by default and toggled by `btnAdvancePaneToggle`, whose
label becomes `tr("Advanced Search %1")` with a ▲/▼ glyph.

## Execution

`on_btnSearchAdvance_clicked()` → `QtConcurrent::run(this, &SearchPage::searching)` and hides
the advanced pane. `searching()` — again **touching widgets from a worker thread** — validates
that a directory was chosen (`tr("Select the search directory.")` otherwise), shows the spinner,
disables the button, builds the argument list, then:

```cpp
result = checkSearchAsRoot ? CommandUtil::sudoExec("find", q) : CommandUtil::exec("find", q);
if (result.trimmed().isEmpty()) clear the table;
else loadDataToTable(result.split("\n"));
```

`CommandUtil` blocks up to 10 minutes and discards stderr, so `find`'s permission-denied noise
is invisible (good) but so is a syntax error (bad — the user sees "no results" or the generic
`tr("Somethings went wrong, try again.")` only on a spawn failure).

## Results table

`QStandardItemModel` → `QSortFilterProxyModel` → `tableFoundResults`, sort role `1`,
dynamic sorting, initial sort on column 1 descending. Header: movable, 32 px, left-aligned,
columns 0 and 1 resized to 150 px.

| # | Column | Value | Hidden by default |
| --- | --- | --- | --- |
| 0 | `Name` | `QFileInfo::fileName()` | no |
| 1 | `Path` | `QFileInfo::path()` | no |
| 2 | `Size` | `formatBytes(size())`, sort value = raw bytes | no |
| 3 | `User` | `owner()` | no |
| 4 | `Group` | `group()` | **yes** |
| 5 | `Creation Time` | `created()` | no |
| 6 | `Last Access` | `lastRead()` | **yes** |
| 7 | `Last Modification` | `lastModified()` | **yes** |
| 8 | `Last Change` | `metadataChangeTime()` | **yes** |

Date format: `dd.MM.yyyy hh:mm:ss`. Every cell also sets `Qt::ToolTipRole`. The header has the
same checkable column-visibility context menu as the Processes page.

`loadDataToTable()` is capped:

```cpp
for (const QString &file : foundFiles.mid(1, 2000)) mItemModel->appendRow(createRow(file));
ui->lblFoundFilesInfo->setText(tr("%1 files found. Showing %2 of them.")
                                 .arg(foundFiles.count()-1).arg(mItemModel->rowCount()));
```

`mid(1, …)` drops the first output line (which is the search root itself, since `find` prints
its starting point) and caps the table at **2000 rows**; the label reports the true total.
Each row constructs and then `delete`s a heap-allocated `QFileInfo` — needless, but harmless.

## Row actions (right-click menu)

Three actions, dispatched by `action->data()`:

**`open-folder`** — for each selected row, `QDesktopServices::openUrl(path)` — opens the
containing directory in the file manager. Double-clicking a row does the same.
Note the path is converted with `data(rowRole).toUrl()`, i.e. a bare filesystem path treated as
a URL; this works for absolute paths but has no `file://` scheme.

**`move-trash`** — implements the freedesktop trash by hand:

```cpp
trashPath = ~/.local/share/Trash
filePath  = <Path column> + "/" + <Name column>
isAnotherUser = QFileInfo(filePath).owner() != InfoManager::getUserName();
isAnotherUser ? sudoExec("mv", {filePath, trashPath + "/files"})
              : exec    ("mv", {filePath, trashPath + "/files"});
if (file still exists) deselect the row       // treat as failure
else {
    write trashPath + "/info/<name>.trashinfo":
        [Trash Info]
        Path=<absolute path>
        DeletionDate=<yyyy-MM-ddThh:mm:ss>
    remove the row from the model
}
```

**`delete`** — same owner check, then `rm -rf` (or `pkexec rm -rf`) on the path, removing the
row on success. **No confirmation dialog.**

Both loops iterate `while (!selectionModel->selectedRows().isEmpty())`, deselecting rows they
fail on so the loop terminates. A file moved to trash while owned by root leaves a
root-owned file in the user's trash directory.

## Port notes

- Walk the filesystem in Rust (`walkdir`/`jwalk`) instead of shelling out to `find`: you get
  real errors, streaming results, cancellation, and no argument-construction bugs (`-invert`
  disappears; "invert" becomes `!`/`-not` semantics you control).
- Stream results to the UI with virtual scrolling rather than a 2000-row cap.
- Use a trash implementation that follows the spec (relative `Path=` for files under the home
  directory, per-volume `.Trash-$uid` dirs) or delegate to `gio trash`.
- Confirm destructive actions; never elevate a `mv` into the user's own trash — refuse or copy
  instead.
