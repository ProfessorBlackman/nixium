# 16 — Feature: Helpers → Host Manage

**Files:** `stacer/Pages/Helpers/{helpers_page,host_manage}.{h,cpp,ui}`
**Window title:** `Helpers` · **Sidebar button:** `btnHelpers`

`HelpersPage` is a shell for a `QStackedWidget` of utility tools, with a button per tool. In
1.1.0 there is exactly **one** tool — Host Manage — and a commented-out line where a second
would be added. The page is clearly an extension point that was never filled.

## Host Manage

A table editor for `/etc/hosts`.

### Model

```cpp
class HostItem { QString ip, fullQualified, aliases; };
QStringList        mHostFileContent;   // the whole file, line by line
QMap<int, HostItem> mHostItemList;     // line index → parsed entry
int                updatedLine;        // -1 = adding, else the line being edited
```

The full file is read **once** in `init()` (`FileUtil::readListFromFile("/etc/hosts")`) and kept
in memory; all edits mutate `mHostFileContent`, and the line index is the identity of a row.

### Parsing

`loadHostItems()` walks every line, skipping blanks and lines whose trimmed form starts with
`#`. Lines with ≥ 2 whitespace-separated tokens become a `HostItem`:
token 0 = `ip`, token 1 = `fullQualified`, tokens 2.. joined with spaces = `aliases`.
The map key is the **original line index**, which is how a row is later located for edit/delete.

Comments and blank lines are preserved in `mHostFileContent` and simply not displayed.

### Table

`QStandardItemModel` → `QSortFilterProxyModel` → `tableViewHosts`, sort role 1, dynamic sorting,
columns `tr("IP Address")`, `tr("Full Qualified")`, `tr("Aliases")` (first two resized to 195 px).
Each row's column-0 item carries the source line number in **role 9** — that is the handle used
by the context menu. Title: `tr("Hosts (%1)")`.

### Add / edit

`btnNewHost` reveals `widgetAddEditHost` (three inputs: `txtIP`, `txtFullyQualified`,
`txtAliases`) and sets `updatedLine = -1`.

`btnSave` validates that IP and FQDN are non-empty
(`tr("The IP and Fully Qualified fields are required.")`), formats
`QString("%1 %2 %3").arg(ip).arg(fqdn).arg(aliases)`, then **appends** it to
`mHostFileContent` (new) or **replaces** the line at `updatedLine` (edit), reloads the table and
hides the form. `btnCancel` hides the form and resets `updatedLine`.

Validation is minimal: the IP is not checked for being an address at all, and an empty alias
field still emits a trailing space.

### Context menu

- **Edit** — read the line number from role 9, populate the three fields from
  `mHostItemList`, show the form with `updatedLine` set.
- **Delete** — for each selected row, replace that line in `mHostFileContent` with an **empty
  string** (leaving a blank line rather than removing the entry), then reload.

### Committing to disk

`on_btnSaveChanges_clicked()`:

```cpp
FileUtil::writeFile("/tmp/stacer_etc_host_new_content", mHostFileContent.join("\n"));
CommandUtil::sudoExec("mv", {"/tmp/stacer_etc_host_new_content", "/etc/hosts"});
loadTableData();
```

So edits are staged in memory, then the whole file is rewritten in one privileged `mv`.
Consequences:

- Nothing is written to `/etc/hosts` until the user presses Save Changes — the table can be
  arbitrarily out of sync with the file.
- The staging path is **fixed and in `/tmp`** — a predictable, world-writable location.
  See [05-privilege-model.md](05-privilege-model.md) for the symlink/replace race this creates.
- `mv` from `/tmp` to `/etc` crosses filesystems on many setups, so this is a copy+unlink,
  and it **replaces the file's ownership/permissions with those of the temp file** (root:root
  0644 after the `mv` as root, which happens to match the usual `/etc/hosts` mode).
- The in-memory copy is never reconciled with the on-disk file, so an external change to
  `/etc/hosts` between load and save is silently overwritten.
- `loadTableData()` after saving re-parses the in-memory list, not the file, so a failed `mv`
  is invisible.

## Port notes

- Write via a privileged helper with an atomic replace that preserves mode and ownership, or
  stage in a root-only directory — never a fixed `/tmp` path.
- Re-read the file before saving and detect external modification.
- Delete should remove the line, not blank it.
- Validate IPv4/IPv6 literals and hostname syntax before accepting a row.
- Keep the "preserve comments and unparsed lines" behaviour — it is the one thing this editor
  does well.
