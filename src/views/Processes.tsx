// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Processes — `PRC-1` and `PRC-2`.
 *
 * The acceptance criteria here are entirely about **what survives a refresh**: selection, scroll
 * position, sort order and column choices. A table that reloses your place every two seconds is one
 * you cannot use to watch anything, which is what a process table is for.
 *
 * Three things make that work:
 *
 * - **Rows are keyed on `pid:started_ticks`**, not on their position. React then moves rows rather
 *   than rebuilding them, so the scroll container is never recreated and the browser keeps the scroll
 *   offset for free.
 * - **Sort and filter live in this component's state**, so a refresh replaces the data underneath an
 *   unchanged view rather than resetting the view.
 * - **Selection is a pid, not an index.** A process that moves from row 3 to row 40 because it got
 *   busy is still the selected process.
 *
 * The `%CPU` column is instantaneous — the change since the last poll — not `ps`'s average over the
 * process's whole life. It is a percentage of one core, so a threaded build reads past 100%.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { t } from "../lib/i18n";
import { BusyInline } from "../components/Busy";
import { formatBytes } from "../lib/format";
import {
  api,
  toAppError,
  type Detail,
  type Process,
  type Settings,
  type Signal,
  type TreeNode,
} from "../lib/ipc";
import { notify } from "../lib/notices";

/** How often to poll while mounted. Matches the backend's documented table interval. */
const INTERVAL_MS = 2000;

type Column = "pid" | "user" | "cpu" | "memory" | "threads" | "state" | "name";
type Sort = { column: Column; descending: boolean };

const COLUMNS: Array<{ id: Column; label: string; numeric: boolean }> = [
  { id: "pid", label: "PID", numeric: true },
  { id: "user", label: "User", numeric: false },
  { id: "cpu", label: "CPU", numeric: true },
  { id: "memory", label: "Memory", numeric: true },
  { id: "threads", label: "Threads", numeric: true },
  { id: "state", label: "State", numeric: false },
  { id: "name", label: "Name", numeric: false },
];

/** A stable identity. A pid alone is not one — they are reused. */
function identify(process: Process): string {
  return `${process.pid}:${process.started_ticks}`;
}

function compare(a: Process, b: Process, column: Column): number {
  switch (column) {
    case "pid":
      return a.pid - b.pid;
    case "user":
      return a.user.localeCompare(b.user);
    case "cpu":
      return a.cpu_percent - b.cpu_percent;
    case "memory":
      return Number(a.memory_bytes) - Number(b.memory_bytes);
    case "threads":
      return a.threads - b.threads;
    case "state":
      return a.state.localeCompare(b.state);
    case "name":
      return a.name.localeCompare(b.name);
  }
}

/** One branch of the tree, indented rather than nested list-within-list. */
function TreeBranch({
  node,
  depth,
  onSelect,
}: {
  node: TreeNode;
  depth: number;
  onSelect: (pid: number) => void;
}) {
  return (
    <>
      <li style={{ paddingLeft: `${depth * 1.1}rem` }}>
        <button type="button" className="tree-row" onClick={() => onSelect(node.pid)}>
          <span className="tree-name">{node.name}</span>
          <span className="tree-figure">{node.subtree_cpu_percent.toFixed(1)}%</span>
          <span className="tree-figure">{formatBytes(node.subtree_memory_bytes)}</span>
          <span className="muted">
            {node.descendants > 0
              ? `${node.descendants} descendant${node.descendants === 1 ? "" : "s"}`
              : ""}
          </span>
        </button>
      </li>
      {/* Only branches that account for something, so a tree of 650 processes stays readable. */}
      {node.children
        .filter((child) => child.subtree_cpu_percent > 0.5 || child.descendants > 0)
        .slice(0, 12)
        .map((child) => (
          <TreeBranch key={child.pid} node={child} depth={depth + 1} onSelect={onSelect} />
        ))}
    </>
  );
}

export default function Processes() {
  const [processes, setProcesses] = useState<Process[]>([]);
  const [sort, setSort] = useState<Sort>({ column: "cpu", descending: true });
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [detail, setDetail] = useState<Detail | null>(null);
  const [tree, setTree] = useState<TreeNode[] | null>(null);
  /** So the first poll can say whether the CPU column is meaningful yet. */
  const polls = useRef(0);

  useEffect(() => {
    let live = true;
    const poll = async () => {
      try {
        const next = await api.processesList();
        if (live) {
          setProcesses(next);
          polls.current += 1;
        }
      } catch (thrown) {
        if (live) notify.error(toAppError(thrown));
      }
    };

    void poll();
    const timer = setInterval(() => void poll(), INTERVAL_MS);
    void api.settingsGet().then((s) => live && setSettings(s));

    return () => {
      live = false;
      clearInterval(timer);
      // Drop the delta state: reopening in ten minutes must not compute a percentage from a
      // ten-minute-old counter over an assumed interval.
      void api.processesForget();
    };
  }, []);

  // PRC-3. Loaded on selection rather than for every row: it is a dozen file reads per process, and
  // nobody wants the detail of six hundred of them.
  useEffect(() => {
    if (selected === null) {
      setDetail(null);
      return;
    }
    let live = true;
    void api
      .processDetail(selected)
      .then((d) => live && setDetail(d))
      .catch(() => live && setDetail(null));
    return () => {
      live = false;
    };
  }, [selected]);

  const hidden = useMemo(
    () => new Set(settings?.hidden_process_columns ?? []),
    [settings],
  );

  const visibleColumns = COLUMNS.filter((c) => !hidden.has(c.id));

  const shown = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const matched = needle
      ? processes.filter(
          (p) =>
            p.name.toLowerCase().includes(needle) ||
            p.user.toLowerCase().includes(needle) ||
            p.state.includes(needle) ||
            String(p.pid) === needle,
        )
      : processes;

    // Copied before sorting: mutating the state array in place would make React's comparison see no
    // change and skip the render.
    return [...matched].sort((a, b) => {
      const order = compare(a, b, sort.column);
      return sort.descending ? -order : order;
    });
  }, [processes, filter, sort]);

  const chosen = useMemo(
    () => shown.find((p) => p.pid === selected) ?? null,
    [shown, selected],
  );

  const act = useCallback(
    async (action: () => Promise<void>, success: string) => {
      setBusy(true);
      try {
        await action();
        notify.success(success);
        setProcesses(await api.processesList());
      } catch (thrown) {
        // The error carries the real errno and its remedy — no silent no-op.
        notify.error(toAppError(thrown));
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  const signal = useCallback(
    (process: Process, which: Signal) =>
      void act(
        () => api.processSignal(process.pid, which, process.state),
        `Sent ${which.toUpperCase()} to ${process.name}.`,
      ),
    [act],
  );

  const toggleColumn = useCallback(
    (column: Column) => {
      if (!settings) return;
      const next = hidden.has(column)
        ? (settings.hidden_process_columns ?? []).filter((c) => c !== column)
        : [...(settings.hidden_process_columns ?? []), column];
      void api
        .settingsSave({ ...settings, hidden_process_columns: next })
        .then(setSettings)
        .catch((thrown) => notify.error(toAppError(thrown)));
    },
    [settings, hidden],
  );

  return (
    <section className="view">
      <div className="card">
        <div className="row">
          <input
            type="search"
            placeholder={t("Filter by name, user, state or pid")}
            value={filter}
            onChange={(e) => setFilter(e.currentTarget.value)}
            aria-label={t("Filter processes")}
          />
          <span className="muted">
            {shown.length} of {processes.length}
          </span>
        </div>

        {polls.current <= 1 && (
          <p className="muted">
            CPU reads zero until the second sample — one reading of a counter is not a rate. Unlike{" "}
            <code>ps</code>, this is what each process is doing now, not its average since it started.
          </p>
        )}

        <div className="proc-scroll">
          <table className="proc-table">
            <thead>
              <tr>
                {visibleColumns.map((column) => (
                  <th
                    key={column.id}
                    className={column.numeric ? "is-numeric" : undefined}
                    aria-sort={
                      sort.column === column.id
                        ? sort.descending
                          ? "descending"
                          : "ascending"
                        : "none"
                    }
                  >
                    <button
                      type="button"
                      onClick={() =>
                        setSort((previous) =>
                          previous.column === column.id
                            ? { column: column.id, descending: !previous.descending }
                            : { column: column.id, descending: column.numeric }
                        )
                      }
                    >
                      {t(column.label)}
                      {sort.column === column.id && (sort.descending ? " ↓" : " ↑")}
                    </button>
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {shown.map((process) => (
                <tr
                  /* Keyed on identity, so a row that moves is moved rather than rebuilt — which is
                     what keeps the scroll position and the selection across a refresh. */
                  key={identify(process)}
                  className={process.pid === selected ? "is-selected" : undefined}
                  onClick={() => setSelected(process.pid)}
                  tabIndex={0}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      setSelected(process.pid);
                    }
                  }}
                >
                  {visibleColumns.map((column) => (
                    <td key={column.id} className={column.numeric ? "is-numeric" : undefined}>
                      {column.id === "pid" && process.pid}
                      {column.id === "user" && process.user}
                      {column.id === "cpu" && `${process.cpu_percent.toFixed(1)}%`}
                      {column.id === "memory" && formatBytes(process.memory_bytes)}
                      {column.id === "threads" && process.threads}
                      {column.id === "state" && process.state.replace("_", " ")}
                      {column.id === "name" && (
                        <span title={process.command}>{process.name}</span>
                      )}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>

      {chosen && (
        <div className="card">
          <h2>{chosen.name}</h2>
          <p className="muted">
            <code>{chosen.command}</code>
          </p>
          <p className="muted">
            pid {chosen.pid} · parent {chosen.ppid} · {chosen.user} · {chosen.threads} threads ·{" "}
            {chosen.state.replace("_", " ")}
          </p>

          {chosen.state === "zombie" ? (
            <p className="caveat">
              {t(
                "This process has already exited and is waiting for its parent to collect it. A signal to it would succeed and do nothing, so nix will not pretend otherwise — it disappears when its parent reaps it, or when its parent exits.",
              )}
            </p>
          ) : (
            <>
              <div className="row">
                {busy && <BusyInline label={t("Sending…")} />}
                <button type="button" disabled={busy} onClick={() => signal(chosen, "term")}>
                  {t("Ask it to stop (TERM)")}
                </button>
                <button
                  type="button"
                  className="danger"
                  disabled={busy}
                  onClick={() => signal(chosen, "kill")}
                >
                  {t("Force it to stop (KILL)")}
                </button>
                <button type="button" disabled={busy} onClick={() => signal(chosen, "hup")}>
                  {t("Reload (HUP)")}
                </button>
              </div>
              <p className="muted">
                {t(
                  "TERM lets it save its work. KILL cannot be caught, so anything unsaved is lost. If it belongs to another user, nix will ask for administrator rights — and if it is yours, it will not.",
                )}
              </p>

              <label className="field field-inline">
                <span>{t("Niceness")}</span>
                <input
                  type="number"
                  min={-20}
                  max={19}
                  defaultValue={0}
                  disabled={busy}
                  onBlur={(e) => {
                    const value = Number(e.currentTarget.value);
                    if (Number.isFinite(value)) {
                      void act(
                        () => api.processRenice(chosen.pid, value),
                        `${chosen.name} set to niceness ${value}.`,
                      );
                    }
                  }}
                />
              </label>
              <p className="muted">
                {t(
                  "Higher is politer. Lowering it needs administrator rights even for your own processes — the kernel lets anyone give up priority and nobody take it.",
                )}
              </p>
            </>
          )}
          {/* PRC-3. */}
          {detail && detail.pid === chosen.pid && (
            <>
              <h3>{t("Detail")}</h3>
              <ul className="detail-list">
                {detail.executable && (
                  <li>
                    <span>{t("Executable")}</span>
                    <code>{detail.executable}</code>
                  </li>
                )}
                {detail.working_directory && (
                  <li>
                    <span>{t("Working directory")}</span>
                    <code>{detail.working_directory}</code>
                  </li>
                )}
                {detail.cgroup && (
                  <li>
                    <span>{t("Control group")}</span>
                    <code>{detail.cgroup}</code>
                  </li>
                )}
                <li>
                  <span>{t("Threads")}</span>
                  <span>{detail.thread_count}</span>
                </li>
                {detail.io && (
                  <li>
                    <span>{t("Read / written")}</span>
                    <span>
                      {formatBytes(detail.io.read_chars)} / {formatBytes(detail.io.written_chars)}
                      <span className="muted">
                        {" "}
                        — of which {formatBytes(detail.io.read_bytes)} /{" "}
                        {formatBytes(detail.io.written_bytes)} actually reached a disk
                      </span>
                    </span>
                  </li>
                )}
                {detail.disk_footprint !== null && (
                  <li>
                    <span>{t("Disk footprint")}</span>
                    <span>
                      {formatBytes(detail.disk_footprint)}
                      <span className="muted"> {t("— its executable and the files it has open")}</span>
                    </span>
                  </li>
                )}
              </ul>

              {detail.open_files && detail.open_files.length > 0 && (
                <details>
                  <summary>
                    {detail.open_files.length} open file
                    {detail.open_files.length === 1 ? "" : "s"}
                  </summary>
                  <ul className="find-list">
                    {detail.open_files.slice(0, 40).map((file) => (
                      <li key={file.fd}>
                        <span className="find-bytes">
                          {file.bytes !== null ? formatBytes(file.bytes) : "—"}
                        </span>
                        <code>{file.target}</code>
                      </li>
                    ))}
                  </ul>
                </details>
              )}

              {detail.environment && detail.environment.length > 0 && (
                <details>
                  <summary>{detail.environment.length} environment variables</summary>
                  <ul className="find-list">
                    {detail.environment.map(([key, value]) => (
                      <li key={key}>
                        <span className="find-bytes">{key}</span>
                        <code>{value}</code>
                      </li>
                    ))}
                  </ul>
                </details>
              )}

              {detail.restricted.length > 0 && (
                <div className="detail-restricted">
                  <p className="muted">{t("Not shown, and why:")}</p>
                  <ul>
                    {detail.restricted.map((reason) => (
                      <li key={reason} className="muted">
                        {reason}
                      </li>
                    ))}
                  </ul>
                </div>
              )}
            </>
          )}

        </div>
      )}

      {/* PRC-4. Loaded on demand: the aggregation is cheap, the payload is every process. */}
      <div className="card">
        <h2>{t("Tree")}</h2>
        <p className="muted">
          {t(
            "A build system&rsquo;s cost is spread across dozens of short-lived children, and each one alone looks like nothing. The subtree figure is what explains a slow machine.",
          )}
        </p>
        <div className="row">
          <button
            type="button"
            onClick={() =>
              void api
                .processTree()
                .then(setTree)
                .catch((thrown) => notify.error(toAppError(thrown)))
            }
          >
            {tree ? "Refresh tree" : "Show tree"}
          </button>
          {tree && (
            <button type="button" onClick={() => setTree(null)}>
              {t("Hide")}
            </button>
          )}
        </div>
        {tree && (
          <ul className="tree">
            {tree
              .filter((node) => node.subtree_cpu_percent > 0.5 || node.descendants > 0)
              .slice(0, 40)
              .map((node) => (
                <TreeBranch key={node.pid} node={node} depth={0} onSelect={setSelected} />
              ))}
          </ul>
        )}
      </div>

      <div className="card">
        <h2>{t("Columns")}</h2>
        <p className="muted">{t("Kept in your settings, so the table looks the same next time.")}</p>
        <div className="row">
          {COLUMNS.map((column) => (
            <label key={column.id} className="field field-inline">
              <input
                type="checkbox"
                checked={!hidden.has(column.id)}
                disabled={!settings}
                onChange={() => toggleColumn(column.id)}
              />
              <span>{t(column.label)}</span>
            </label>
          ))}
        </div>
      </div>
    </section>
  );
}
