# 14 — Feature: APT Source Manager

**Files:** `stacer/Pages/AptSourceManager/{apt_source_manager_page,apt_source_repository_item,apt_source_edit}.{h,cpp,ui}`
**Window title:** `APT Source Manager` · **Sidebar button:** `btnAptSourceManager`
**Backend:** `stacer-core/Tools/apt_source_tool.*`
**Gate:** the page exists only if `/etc/apt/sources.list.d` exists.

## UI

- `listWidgetAptSources` — one `APTSourceRepositoryItem` per parsed entry.
- `txtSearchAptSource` — substring filter.
- `btnAddAPTSourceRepository` — a **checkable** button acting as an add-mode toggle:
  unchecked it reads `tr("Add Repository")`; checked it becomes `tr("Save")` and reveals
  `txtAptSource` + `checkEnableSource` + `btnCancel`, hiding Edit/Delete.
- `btnEditAptSource`, `btnDeleteAptSource` — act on the selected row.
- `lblAptSourceTitle` — `tr("APT Repositories (%1)")`.
- Placeholder text: `tr("example %1").arg("'deb http://archive.ubuntu.com/ubuntu xenial main'")`.

`APTSourceRepositoryItem` shows an enable checkbox plus the source line with its
`[options]` block stripped for readability; `deb-src` entries are labelled
`tr("%1 (Source Code)")`.

## Parsing

`AptSourceTool::getSourceList()` — see
[03-core-library-reference.md](03-core-library-reference.md#aptsourcetool-toolsapt_source_toolhcpp)
for the full algorithm. Summary:

- Files: `/etc/apt/sources.list.d/*.list` ordered by mtime, then `/etc/apt/sources.list`.
- Lines matching `^\s*#*\s*deb` are candidates; `isActive` = not commented.
- One-line (legacy) format only — `deb`/`deb-src`, optional `[options]`, uri, distribution,
  components. **deb822 `.sources` files are not supported.**
- The original text (minus `#`) is kept in `source` and used later as the *lookup key* when
  rewriting the file.

## Mutations

All four go through `ToolManager` → `AptSourceTool` and end in a privileged command:

| Action | Path |
| --- | --- |
| Toggle enable | `changeStatus()` — strip `#`, optionally prefix `"# "`, then `changeSource()` |
| Edit | `changeSource()` with the rebuilt line |
| Delete | `removeAPTSource()` = `changeSource(src, "")` — removes the line |
| Add | `pkexec add-apt-repository -y <string> [-s]` |

`changeSource()` reads the file, finds the **first line whose text contains** the stored
`source` string, replaces (or deletes) it, joins with `\n`, appends a trailing `\n`, and writes
via `pkexec tee <filePath>` with the content on stdin.

The substring lookup is the weak point: if two entries in one file share a prefix (e.g. the
same repo with different components, or a commented-out variant), the wrong line can be
rewritten. A line-index would have been safer, and the `APTSource` struct does not store one.

Also note that toggling or editing an entry does **not** re-read the list afterwards
(`loadAptSources()` is only called after add and delete), so a failed or cancelled `pkexec`
leaves the checkbox showing the state the user asked for rather than the state on disk.

## Selection model

`selectedAptSource` is a **static member** of `APTSourceManagerPage`, set from
`on_listWidgetAptSources_itemClicked` by casting the item widget back to
`APTSourceRepositoryItem` and taking its `aptSource()`. Double-clicking a row selects it and
immediately opens the editor. `APTSourceEdit::selectedAptSource` is a second static used to
hand the subject to the dialog.

## Edit dialog (`APTSourceEdit`)

Fields: `radioBinary` / `radioSource` (deb vs deb-src), `txtOptions`, `txtUri`,
`txtDistribution`, `txtComponents`, plus an error label.

Validation requires `uri` and `distribution` to be non-empty. On save it rebuilds the line as

```
QString("%1 %2 %3 %4 %5").arg(type).arg(options).arg(uri).arg(distribution).arg(components)
```

then calls `ToolManager::changeAPTSource(...)`, emits `saved()` (which reloads the list) and
closes. Because the format string always includes all five slots, an empty `options` field
yields a **double space** in the written line — harmless to APT, cosmetically visible in the file.

## Search

Hides list items whose `data(5)` (the original source string, stashed on the `QListWidgetItem`)
does not contain the query, case-insensitively. Uses the deprecated
`QListWidget::setItemHidden`.

## Port notes

- Support deb822 `.sources` files — on current Ubuntu/Debian this is where sources increasingly
  live, and a rebuild that only reads `.list` files will appear broken.
- Track file path **and line number** per entry; never locate lines by substring.
- Re-read after every write and reflect the actual on-disk state.
- Consider parsing/validating the entry before writing (`deb`/`deb-src`, valid URI, non-empty
  suite) and offering the signing-key fields that modern entries need
  (`[signed-by=/usr/share/keyrings/...]`).
- `add-apt-repository` is Ubuntu-specific (`software-properties-common`); check for it and
  degrade gracefully.
