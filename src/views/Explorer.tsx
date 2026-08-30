// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * The space explorer — milestone M2, the view that makes nix worth installing.
 *
 * Answers "where did my disk went?" with a treemap and a drill-down table over one shared scan.
 * Two properties are deliberate and load-bearing:
 *
 * - **Nothing is deleted from here.** M2 is read-only by design, so it can be used and trusted
 *   before any reclaim code exists. Reclaiming arrives in M3, behind the preview pipeline.
 * - **Partial results are labelled as partial.** A cancelled scan still delivers its tree, and a
 *   scan that could not read everything says so. Stacer showed a total with no indication that a
 *   scan had skipped anything.
 */
import { useEffect, useMemo, useState } from "react";

import { Busy, Spinner } from "../components/Busy";
import { SpaceTable } from "../components/SpaceTable";
import { Treemap } from "../components/Treemap";
import { t } from "../lib/i18n";
import { formatAge, formatBytes, formatCount, formatPercent } from "../lib/format";
import {
  api,
  onScanDone,
  toAppError,
  type Filesystem,
  type ScanResult,
  type SpaceEntry,
} from "../lib/ipc";
import { notify } from "../lib/notices";
import { useOperation } from "../lib/useOperation";

export default function Explorer() {
  const [filesystems, setFilesystems] = useState<Filesystem[] | null>(null);
  const [result, setResult] = useState<ScanResult | null>(null);
  const [scanRoot, setScanRoot] = useState<string | null>(null);
  /** When the shown result was produced. Null means it is from this session's own scan. */
  const [scannedAt, setScannedAt] = useState<Date | null>(null);
  /** Drill-down stack: the last id is the directory on screen. */
  const [path, setPath] = useState<string[]>([]);
  const op = useOperation();

  useEffect(() => {
    api
      .filesystems()
      .then(setFilesystems)
      .catch((thrown) => notify.error(toAppError(thrown)));
  }, []);

  // Cached-first (D6): open on the previous scan so the view is never empty after the first use.
  // Browsing stale data is fine; acting on it is not, and nothing here acts.
  useEffect(() => {
    let cancelled = false;
    api
      .homeDirectory()
      .then((home) => api.scanCached(home).then((cached) => ({ home, cached })))
      .then(({ home, cached }) => {
        if (cancelled || !cached) return;
        setResult(cached.result);
        setScanRoot(cached.root);
        setScannedAt(new Date(cached.scanned_at * 1000));
        setPath(cached.result.tree.roots.length > 0 ? [cached.result.tree.roots[0]] : []);
        void home;
      })
      .catch(() => {
        // No cache and no home directory is not worth reporting: the user simply picks a root.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // The completed tree arrives on its own event, so a cancelled scan still delivers what it found.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void onScanDone((r) => {
      setResult(r);
      setScannedAt(null);
      setPath(r.tree.roots.length > 0 ? [r.tree.roots[0]] : []);
      if (r.cancelled) {
        notify.info(t("Scan stopped."), r.coverage_note ?? null);
      } else if (r.errors.length > 0) {
        notify.warning(
          `Scanned with gaps: ${r.errors.length} location${r.errors.length === 1 ? "" : "s"} could not be read.`,
          "The total may be higher than shown.",
          r.errors
            .slice(0, 8)
            .map((e) => `${e.path}: ${e.message}`)
            .join("\n"),
        );
      }
    }).then((un) => {
      unlisten = un;
    });
    return () => unlisten?.();
  }, []);

  async function startScan(root: string) {
    setResult(null);
    setScannedAt(null);
    setPath([]);
    setScanRoot(root);
    await op.start(() => api.scanStart(root));
  }

  async function scanHome() {
    try {
      await startScan(await api.homeDirectory());
    } catch (thrown) {
      notify.error(toAppError(thrown));
    }
  }

  const currentId = path.at(-1) ?? null;
  const current = currentId ? result?.tree.entries[currentId] : undefined;

  const crumbs = useMemo(
    () =>
      path
        .map((id) => result?.tree.entries[id])
        .filter((e): e is SpaceEntry => Boolean(e)),
    [path, result],
  );

  const fraction = op.progress?.total ? op.progress.done / op.progress.total : null;

  return (
    <section className="stack stack-wide">
      {/* ---- pick something to scan ---- */}
      <div className="card">
        <h2>{t("Scan")}</h2>
        <div className="row wrap">
          <button type="button" onClick={() => void scanHome()} disabled={op.running}>
            {op.running && <Spinner />}
            {op.running ? t("Scanning…") : t("Scan my home directory")}
          </button>
          {op.running && (
            <button type="button" onClick={() => void op.cancel()}>
              {t("Stop")}
            </button>
          )}
        </div>

        {filesystems && filesystems.length > 0 && (
          <>
            <p className="muted" style={{ marginTop: "0.9rem" }}>
              {t("Or a whole filesystem. Pseudo-filesystems are hidden — they are not storage.")}
            </p>
            <ul className="fs-list">
              {filesystems.map((fs) => (
                  <li key={fs.mount_point}>
                    <button
                      type="button"
                      className="fs-row"
                      disabled={op.running}
                      onClick={() => void startScan(fs.mount_point)}
                    >
                      <span className="fs-mount">
                        <code>{fs.mount_point}</code>
                        <small>
                          {fs.fs_type} · {fs.device}
                          {fs.read_only && " · read-only"}
                        </small>
                      </span>
                      <span className="fs-meter" aria-hidden="true">
                        <span
                          className="fs-meter-fill"
                          style={{
                            width: `${Math.round(((fs.total ? fs.used / fs.total : 0) as number) * 100)}%`,
                          }}
                        />
                      </span>
                      <span className="fs-figures">
                        <strong>{formatBytes(fs.used)}</strong>
                        <small>
                          of {formatBytes(fs.total)}
                          {fs.total > 0 && ` · ${formatPercent(fs.used / fs.total)}`}
                        </small>
                      </span>
                    </button>
                    {typeof fs.accounting === "object" && "approximate" in fs.accounting && (
                      <p className="fs-caveat">{fs.accounting.approximate.reason}</p>
                    )}
                  </li>
              ))}
            </ul>
          </>
        )}
      </div>

      {/* ---- progress ---- */}
      {op.running && (
        <div className="card">
          <h2>Scanning {scanRoot}</h2>
          <Busy label={op.progress?.message ?? t("Starting…")} fraction={fraction} />
          <p className="muted">
            {t("Results appear as soon as the walk finishes. Stopping keeps whatever was found.")}
          </p>
        </div>
      )}

      {/* ---- results ---- */}
      {result && current && (
        <>
          <div className="card">
            <div className="summary">
              <div>
                <span className="summary-figure">{formatBytes(result.allocated)}</span>
                <span className="muted">{t("on disk")}</span>
              </div>
              <div>
                <span className="summary-figure">{formatBytes(result.apparent_size)}</span>
                <span className="muted">{t("apparent size")}</span>
              </div>
              <div>
                <span className="summary-figure">{formatCount(result.files)}</span>
                <span className="muted">{t("files")}</span>
              </div>
              <div>
                <span className="summary-figure">{formatCount(result.dirs)}</span>
                <span className="muted">{t("directories")}</span>
              </div>
            </div>
            {scannedAt && scanRoot && (
              <p className="stale">
                <span>
                  Scanned <strong>{formatAge(scannedAt)}</strong> — this is the last saved result,
                  not a fresh measurement.
                </span>
                <button type="button" onClick={() => void startScan(scanRoot)} disabled={op.running}>
                  {t("Rescan")}
                </button>
              </p>
            )}
            {result.coverage_note && <p className="caveat">{result.coverage_note}</p>}
            {result.aggregated_below > 0 && (
              <p className="muted">
                Anything under {formatBytes(result.aggregated_below)} is grouped into a &ldquo;smaller
                items&rdquo; row beside its siblings, so a directory listing stays readable. Those
                bytes are still counted in every total on this page.
              </p>
            )}
            {result.allocated !== result.apparent_size && (
              <p className="muted">
                {t(
                  "On-disk and apparent size differ because of block rounding, sparse files, and filesystem compression. Reclaimable space is always the on-disk figure.",
                )}
              </p>
            )}
          </div>

          <nav className="crumbs" aria-label={t("Location")}>
            {crumbs.map((entry, i) => (
              <button
                key={entry.id}
                type="button"
                className="crumb"
                disabled={i === crumbs.length - 1}
                onClick={() => setPath(path.slice(0, i + 1))}
              >
                {i === 0 ? (entry.path ?? entry.label) : entry.label}
              </button>
            ))}
          </nav>

          <div className="card card-flush">
            <Treemap
              tree={result.tree}
              rootId={current.id}
              onOpen={(entry) => setPath([...path, entry.id])}
            />
          </div>

          <div className="card card-flush">
            <SpaceTable
              tree={result.tree}
              rootId={current.id}
              onOpen={(entry) => setPath([...path, entry.id])}
            />
          </div>
        </>
      )}

      {!result && !op.running && (
        <div className="card">
          <h2>{t("Nothing scanned yet")}</h2>
          <p className="muted">
            {t(
              "Pick somewhere above. Scanning is read-only — nothing here deletes anything, and reclaiming arrives in a later milestone behind a preview and a confirmation.",
            )}
          </p>
        </div>
      )}
    </section>
  );
}
