# 09 — Feature: Processes

**Files:** `stacer/Pages/Processes/processes_page.{h,cpp,ui}`
**Window title:** `Processes` · **Sidebar button:** `btnProcesses`

## UI

- `tableProcess` — a `QTableView` over `QStandardItemModel` → `QSortFilterProxyModel`.
- `txtProcessSearch` — live filter.
- `checkAllProcesses` — show all users' processes vs. only the current user's.
- `sliderRefresh` — refresh interval, 1–10 seconds.
- `lblProcessTitle` — `tr("Processes (%1)")`.
- `btnEndProcess` — kill the selected row.
- Right-click on the **header** opens a checkable column-visibility menu.

## Columns

Model order (index → header, all translated except PID/%CPU):

| # | Header | Source field | Display | Sort value (role 1) |
| --- | --- | --- | --- | --- |
| 0 | `PID` | `pid` | number | `pid` (int) |
| 1 | `Resident Memory` | `rss` | `formatBytes` | raw bytes |
| 2 | `%Memory` | `pmem` | number | double |
| 3 | `Virtual Memory` | `vsize` | `formatBytes` | raw bytes |
| 4 | `User` | `uname` | string | string |
| 5 | `%CPU` | `pcpu` | number | double |
| 6 | `Start Time` | `start_time` | string | string |
| 7 | `State` | `state` | string | string |
| 8 | `Group` | `group` | string | string |
| 9 | `Nice` | `nice` | number | int |
| 10 | `CPU Time` | `cputime` | string | string |
| 11 | `Session` | `session` | string | string |
| 12 | `Process` | `cmd` | full command line | string |

Every cell also sets `Qt::ToolTipRole`; the command column's tooltip is wrapped in `<p>…</p>`
so Qt word-wraps long command lines.

**Hidden by default:** columns 3, 6, 7, 8, 9, 10, 11 (their menu entries start unchecked).
Column 0 is resized to 70 px; the header is 36 px tall, movable, left-aligned, with a
pointing-hand cursor.

The proxy model uses `setSortRole(1)` — i.e. the *hidden numeric* role — so byte and percentage
columns sort numerically rather than lexicographically. Initial sort: **column 5 (%CPU),
descending**, with `setDynamicSortFilter(true)`.

## Refresh cycle

`loadProcesses()`, called immediately in `init()` and then by a `QTimer` (default 1000 ms):

1. Remember the currently selected rows.
2. `mItemModel->removeRows(0, rowCount())` — **the entire model is torn down and rebuilt** each
   tick.
3. `im->updateProcesses()` → one `ps` invocation.
4. Append one row per process, filtered by `checkAllProcesses`: when unchecked, only rows whose
   `uname` equals `InfoManager::getUserName()`.
5. Update the title count.
6. Restore selection by scanning the proxy for a row whose PID (role 1 of column 0) matches the
   previously selected PID.

Rebuilding the model every second is why selection has to be manually restored and why the
scroll position jumps; it also makes the filter re-apply from scratch. The interval slider is
wired to `mTimer->setInterval(i * 1000)` and relabels `lblRefresh` as `tr("Refresh (%1)")`.

## Filtering

```cpp
QRegExp query(val, Qt::CaseInsensitive, QRegExp::Wildcard);
mSortFilterModel->setFilterKeyColumn(mHeaders.count() - 1);   // the Process/cmd column
mSortFilterModel->setFilterRegExp(query);
```

Wildcard syntax (`*`, `?`), case-insensitive, matched against the **command line only** — not
the PID or user.

## Killing a process

```cpp
pid_t pid = mSeletedRowModel.data(1).toInt();
if (pid) {
    QString owner = mSortFilterModel->index(row, 4).data(1).toString();  // User column
    if (owner == im->getUserName()) CommandUtil::exec("kill", { pid });
    else                           CommandUtil::sudoExec("kill", { pid });
}
```

- Plain `kill` = SIGTERM. There is no SIGKILL escalation, no confirmation dialog, and no
  feedback on failure (a throw is logged with `qCritical` and swallowed).
- Own processes are killed directly; other users' processes go through `pkexec`.
- `mSeletedRowModel` is refreshed on each `loadProcesses()`; if nothing is selected it becomes
  an invalid `QModelIndex` and the button does nothing.
- The page calls `CommandUtil` directly rather than going through a manager.

## Column-visibility menu

`loadHeaderMenu()` builds one checkable `QAction` per header with the column index in
`action->data()`, then unchecks and hides the default-hidden set. The menu is opened from the
header's `customContextMenuRequested`, executed synchronously with `mHeaderMenu.exec(globalPos)`,
and the returned action's checked state is applied via `setSectionHidden`.

Note the slot is *named* as if it belonged to the table (`on_tableProcess_customContextMenuRequested`),
so Qt auto-connects it to `tableProcess::customContextMenuRequested` as well — but the table's
own `contextMenuPolicy` is left at the default in `processes_page.ui`, so that signal never
fires and only the explicit header connection is live. Right-clicking the table body does
nothing.

## Port notes

- Read `/proc/<pid>/{stat,statm,status,cmdline}` directly instead of shelling out to `ps` —
  avoids the whitespace-splitting fragility and the per-tick process spawn.
- Diff-update the row set instead of clearing the model; that removes the selection/scroll
  restoration hacks.
- `%CPU` from `ps` is *average since process start*, not instantaneous. If the rebuild wants a
  live per-process CPU figure it must compute deltas of `utime + stime` per PID itself.
- Add a confirmation and a SIGTERM→SIGKILL escalation path; surface failures.
