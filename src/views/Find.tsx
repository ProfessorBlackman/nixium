// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Find — `STO-15`.
 *
 * Two questions a storage tool should be able to answer without being interrogated first:
 * *what are the biggest files?* and *what is here twice?*
 *
 * Stacer answered the first with a form — path, pattern, size, unit, then a results list with no
 * sizes in it. Both answers here come from the scan that already happened, so the largest-files list
 * needs no input at all.
 *
 * Duplicate detection does cost real work, so it is explicit, reports progress, and can be stopped.
 * It promises no false positives, which is why it finishes with a byte-for-byte comparison rather
 * than trusting a hash — and why hard links are excluded, since two names for one inode share their
 * blocks and deleting a name frees nothing.
 *
 * # Search — `SYS-2`
 *
 * A third question: *where is this file?* Stacer built a `find` command line, and its "invert"
 * checkbox appended `-invert` — which `find` has no predicate for, so it exited with a usage error and
 * the search returned nothing at all, silently. Every filter here is code, and inversion is applied to
 * the predicate result.
 *
 * Results **stream** and there is no row cap. Stacer displayed the first 2,000 and said so, which
 * makes "is it in there?" unanswerable. It also ran `find` under `sudo` for roots outside `$HOME`;
 * searching is a read, so this walks as the user and reports what it could not enter.
 */
import { useCallback, useEffect, useState } from "react";

import { t } from "../lib/i18n";
import { formatBytes, formatCount } from "../lib/format";
import {
  api,
  onDuplicatesDone,
  onSearchDone,
  onSearchHits,
  toAppError,
  type DuplicateReport,
  type FileKind,
  type Hit,
  type NameMatch,
  type OperationId,
  type SearchQuery,
  type SearchSummary,
  type SpaceEntry,
} from "../lib/ipc";
import { notify } from "../lib/notices";
import { useOperation } from "../lib/useOperation";

export default function Find() {
  const operation = useOperation();
  const [home, setHome] = useState<string | null>(null);
  const [largest, setLargest] = useState<SpaceEntry[] | null>(null);
  const [report, setReport] = useState<DuplicateReport | null>(null);
  const [searching, setSearching] = useState(false);

  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const path = await api.homeDirectory();
        if (!live) return;
        setHome(path);
        setLargest(await api.largestFiles(path, 100));
      } catch (thrown) {
        notify.error(toAppError(thrown));
      }
    })();
    return () => {
      live = false;
    };
  }, []);

  useEffect(() => {
    const subscription = onDuplicatesDone((r) => {
      setReport(r);
      setSearching(false);
      if (r.cancelled) {
        notify.info(t("Duplicate search stopped."), "Partial results are shown.");
      } else if (r.groups.length === 0) {
        notify.success(t("No duplicates found."));
      } else {
        notify.success(
          `${r.groups.length} duplicate set${r.groups.length === 1 ? "" : "s"} found.`,
          `${formatBytes(r.recoverable)} could be recovered by keeping one copy of each.`,
        );
      }
    });
    return () => {
      void subscription.then((un) => un());
    };
  }, []);

  const search = useCallback(async () => {
    if (!home) return;
    setReport(null);
    setSearching(true);
    await operation.start(() => api.duplicatesFind(home));
  }, [home, operation]);

  return (
    <section className="view">
      <div className="card">
        <h2>{t("Largest files")}</h2>
        {largest === null ? (
          <p className="muted">{t("Reading the last scan…")}</p>
        ) : largest.length === 0 ? (
          <p className="muted">
            {t(
              "Nothing to show yet — the space explorer has not scanned anything. This list is a view of that scan rather than a search of its own, so it costs nothing once a scan exists.",
            )}
          </p>
        ) : (
          <>
            <p className="muted">
              From the last scan of <code>{home}</code>. Files only: a directory&rsquo;s size is its
              contents&rsquo;, and listing both would show the same bytes twice.
            </p>
            <ul className="find-list">
              {largest.slice(0, 40).map((entry) => (
                <li key={entry.id}>
                  <span className="find-bytes">{formatBytes(entry.allocated)}</span>
                  <code title={entry.path ?? entry.label}>{entry.path ?? entry.label}</code>
                </li>
              ))}
            </ul>
          </>
        )}
      </div>

      <div className="card">
        <h2>{t("Duplicates")}</h2>
        <p className="muted">
          {t(
            "Compared by size, then by the first few kilobytes, then by full content, and finally byte for byte — so a reported set really is identical rather than merely very likely to be. Files under 1 MiB are skipped, and hard links are not counted: two names for one file share the same blocks, so deleting a name frees nothing.",
          )}
        </p>

        <div className="row">
          <button type="button" onClick={() => void search()} disabled={searching || !home}>
            {searching ? "Searching…" : "Look for duplicates"}
          </button>
          {searching && operation.running && (
            <button type="button" onClick={() => void operation.cancel()}>
              {t("Stop")}
            </button>
          )}
        </div>

        {searching && operation.progress && (
          <p className="muted">{operation.progress.message ?? "Working…"}</p>
        )}

        {report && (
          <>
            <div className="summary">
              <div>
                <span className="summary-figure">{formatBytes(report.recoverable)}</span>
                <span className="muted">{t("recoverable")}</span>
              </div>
              <div>
                <span className="summary-figure">{report.groups.length}</span>
                <span className="muted">{t("duplicate sets")}</span>
              </div>
            </div>
            <p className="muted">
              {report.stats.considered} files considered, {report.stats.size_matched} shared a size,{" "}
              {report.stats.fully_hashed} were read in full, and {report.stats.pairs_verified} pairs
              were compared byte for byte.
            </p>
            {report.cancelled && (
              <p className="caveat">
                {t("Stopped early, so this is not a complete answer — there may be more.")}
              </p>
            )}
            <ul className="dup-list">
              {report.groups.slice(0, 30).map((group) => (
                <li key={group.paths.join("|")}>
                  <div className="dup-head">
                    <span>
                      {group.paths.length} copies of {formatBytes(group.bytes)}
                    </span>
                    <span className="find-bytes">{formatBytes(group.recoverable)} recoverable</span>
                  </div>
                  {group.paths.map((path) => (
                    <code key={path}>{path}</code>
                  ))}
                </li>
              ))}
            </ul>
            {report.groups.length > 30 && (
              <p className="muted">…and {report.groups.length - 30} more sets.</p>
            )}
            <p className="muted">
              {t(
                "nix does not choose which copy to delete. Which one matters is a judgement about your work, not about storage.",
              )}
            </p>
          </>
        )}
      </div>
      <SearchPanel />
    </section>
  );
}

/** The default query: everything under the home directory, no filters. */
function blankQuery(): SearchQuery {
  return {
    root: "",
    name: "",
    match_kind: "contains",
    whole_path: false,
    case_sensitive: false,
    min_bytes: null,
    max_bytes: null,
    modified_after: null,
    modified_before: null,
    kind: null,
    owner_uid: null,
    mode_all_of: null,
    empty_only: false,
    invert: false,
    cross_filesystems: false,
    limit: null,
  };
}

/** Kibibytes from a text field, or null for an empty one. */
function bytesFrom(text: string): number | null {
  const value = Number.parseFloat(text);
  return Number.isFinite(value) && value >= 0 ? Math.round(value * 1024) : null;
}

function SearchPanel() {
  const [root, setRoot] = useState("");
  const [name, setName] = useState("");
  const [matchKind, setMatchKind] = useState<NameMatch>("contains");
  const [kind, setKind] = useState<FileKind | "">("");
  const [minKib, setMinKib] = useState("");
  const [maxKib, setMaxKib] = useState("");
  const [invert, setInvert] = useState(false);
  const [emptyOnly, setEmptyOnly] = useState(false);
  const [caseSensitive, setCaseSensitive] = useState(false);

  const [running, setRunning] = useState<OperationId | null>(null);
  const [hits, setHits] = useState<Hit[]>([]);
  const [summary, setSummary] = useState<SearchSummary | null>(null);

  // Subscribed once, filtered by operation id — two searches can overlap when the user changes their
  // mind, and the abandoned one's results must not appear in the new one's list.
  useEffect(() => {
    const hitsOff = onSearchHits((batch) => {
      setRunning((current: OperationId | null) => {
        if (current === batch.id) setHits((existing) => [...existing, ...batch.hits]);
        return current;
      });
    });
    const doneOff = onSearchDone((done) => {
      setRunning((current: OperationId | null) => {
        if (current === done.id) {
          setSummary(done.summary);
          return null;
        }
        return current;
      });
    });
    return () => {
      void hitsOff.then((off) => off());
      void doneOff.then((off) => off());
    };
  }, []);

  const start = useCallback(async () => {
    if (root.trim() === "") {
      notify.warning(t("Choose a folder to search in."));
      return;
    }
    setHits([]);
    setSummary(null);
    try {
      const query: SearchQuery = {
        ...blankQuery(),
        root: root.trim(),
        name: name.trim(),
        match_kind: matchKind,
        case_sensitive: caseSensitive,
        kind: kind === "" ? null : kind,
        min_bytes: bytesFrom(minKib),
        max_bytes: bytesFrom(maxKib),
        empty_only: emptyOnly,
        invert,
      };
      setRunning(await api.searchStart(query));
    } catch (thrown) {
      notify.error(toAppError(thrown));
    }
  }, [root, name, matchKind, caseSensitive, kind, minKib, maxKib, emptyOnly, invert]);

  const stop = useCallback(async () => {
    if (running === null) return;
    try {
      await api.operationCancel(running);
    } catch (thrown) {
      notify.error(toAppError(thrown));
    }
  }, [running]);

  return (
    <div className="card">
      <h2>{t("Search")}</h2>
      <p className="muted">
        {t(
          "Every filter here does what it says. Results arrive as they are found, and there is no cap on how many.",
        )}
      </p>

      <div className="search-form">
        <label className="field">
          <span>{t("In folder")}</span>
          <input
            value={root}
            placeholder={t("/home/you")}
            onChange={(e) => setRoot(e.target.value)}
          />
        </label>
        <label className="field">
          <span>{t("Name")}</span>
          <input
            value={name}
            placeholder={t("report, *.log, or a pattern")}
            onChange={(e) => setName(e.target.value)}
          />
          <small>{t("Leave empty to match everything and filter by the rest.")}</small>
        </label>
        <label className="field">
          <span>{t("Match as")}</span>
          <select value={matchKind} onChange={(e) => setMatchKind(e.target.value as NameMatch)}>
            <option value="contains">{t("contains")}</option>
            <option value="glob">{t("glob (* ? [abc])")}</option>
            <option value="regex">{t("regular expression")}</option>
          </select>
        </label>
        <label className="field">
          <span>{t("Type")}</span>
          <select value={kind} onChange={(e) => setKind(e.target.value as FileKind | "")}>
            <option value="">{t("anything")}</option>
            <option value="file">{t("files")}</option>
            <option value="directory">{t("folders")}</option>
            <option value="symlink">{t("symlinks")}</option>
          </select>
        </label>
        <label className="field">
          <span>{t("At least (KiB)")}</span>
          <input value={minKib} inputMode="decimal" onChange={(e) => setMinKib(e.target.value)} />
        </label>
        <label className="field">
          <span>{t("At most (KiB)")}</span>
          <input value={maxKib} inputMode="decimal" onChange={(e) => setMaxKib(e.target.value)} />
        </label>
      </div>

      <div className="row wrap">
        <label className="field field-inline">
          <input type="checkbox" checked={invert} onChange={(e) => setInvert(e.target.checked)} />
          <span>{t("Invert — everything that does not match")}</span>
        </label>
        <label className="field field-inline">
          <input
            type="checkbox"
            checked={emptyOnly}
            onChange={(e) => setEmptyOnly(e.target.checked)}
          />
          <span>{t("Empty only")}</span>
        </label>
        <label className="field field-inline">
          <input
            type="checkbox"
            checked={caseSensitive}
            onChange={(e) => setCaseSensitive(e.target.checked)}
          />
          <span>{t("Case sensitive")}</span>
        </label>
      </div>

      <div className="row">
        <button type="button" onClick={() => void start()} disabled={running !== null}>
          {running !== null ? "Searching…" : "Search"}
        </button>
        {running !== null && (
          <button type="button" onClick={() => void stop()}>
            {t("Stop")}
          </button>
        )}
      </div>

      {(hits.length > 0 || summary !== null) && (
        <p className="muted">
          {formatCount(hits.length)} found
          {summary !== null && ` · ${formatCount(Number(summary.examined))} examined`}
          {summary !== null && summary.unreadable > 0 && (
            <>
              {" · "}
              <span className="search-partial">
                {formatCount(Number(summary.unreadable))} folders could not be read, so this is not
                the whole picture
              </span>
            </>
          )}
          {summary !== null && summary.truncated && " · stopped at the limit"}
          {summary !== null && summary.cancelled && " · stopped"}
        </p>
      )}

      {hits.length > 0 && (
        <div className="search-scroll">
          <table className="pkg-table">
            <thead>
              <tr>
                <th scope="col">{t("Path")}</th>
                <th scope="col" className="pkg-num">
                  {t("Size")}
                </th>
                <th scope="col">{t("Modified")}</th>
              </tr>
            </thead>
            <tbody>
              {hits.slice(0, 1000).map((hit) => (
                <tr key={hit.path}>
                  <td>
                    <code className="search-path">{hit.path}</code>
                    {hit.kind !== "file" && <span className="pkg-dep">{hit.kind}</span>}
                  </td>
                  <td className="pkg-num">
                    {hit.kind === "directory" ? "—" : formatBytes(hit.bytes)}
                  </td>
                  <td className="pkg-date">
                    {hit.modified === null
                      ? "—"
                      : new Date(hit.modified * 1000).toLocaleDateString()}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {hits.length > 1000 && (
        <p className="muted">
          Showing the first 1,000 of {formatCount(hits.length)} found — all of them were searched
          for and counted; this is only what is rendered.
        </p>
      )}
    </div>
  );
}
