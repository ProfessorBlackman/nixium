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
 */
import { useCallback, useEffect, useState } from "react";

import { formatBytes } from "../lib/format";
import {
  api,
  onDuplicatesDone,
  toAppError,
  type DuplicateReport,
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
        notify.info("Duplicate search stopped.", "Partial results are shown.");
      } else if (r.groups.length === 0) {
        notify.success("No duplicates found.");
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
        <h2>Largest files</h2>
        {largest === null ? (
          <p className="muted">Reading the last scan…</p>
        ) : largest.length === 0 ? (
          <p className="muted">
            Nothing to show yet — the space explorer has not scanned anything. This list is a view of
            that scan rather than a search of its own, so it costs nothing once a scan exists.
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
        <h2>Duplicates</h2>
        <p className="muted">
          Compared by size, then by the first few kilobytes, then by full content, and finally byte
          for byte — so a reported set really is identical rather than merely very likely to be.
          Files under 1 MiB are skipped, and hard links are not counted: two names for one file share
          the same blocks, so deleting a name frees nothing.
        </p>

        <div className="row">
          <button type="button" onClick={() => void search()} disabled={searching || !home}>
            {searching ? "Searching…" : "Look for duplicates"}
          </button>
          {searching && operation.running && (
            <button type="button" onClick={() => void operation.cancel()}>
              Stop
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
                <span className="muted">recoverable</span>
              </div>
              <div>
                <span className="summary-figure">{report.groups.length}</span>
                <span className="muted">duplicate sets</span>
              </div>
            </div>
            <p className="muted">
              {report.stats.considered} files considered, {report.stats.size_matched} shared a size,{" "}
              {report.stats.fully_hashed} were read in full, and {report.stats.pairs_verified} pairs
              were compared byte for byte.
            </p>
            {report.cancelled && (
              <p className="caveat">
                Stopped early, so this is not a complete answer — there may be more.
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
              nix does not choose which copy to delete. Which one matters is a judgement about your
              work, not about storage.
            </p>
          </>
        )}
      </div>
    </section>
  );
}
