/**
 * Reclaim — milestone M3.
 *
 * The whole view is shaped by principle P2: **preview → confirm → execute → report**. Those are
 * literally the four states below, and there is no path between the first and the third.
 *
 * The safety rating decides what the UI allows, not just what it says:
 *
 * - `safe`   — pre-checked. Regenerable with no user-visible loss.
 * - `review` — selectable, never pre-checked, and its cost is always shown. Choosing for someone
 *              what they will lose is not a decision to make on their behalf.
 * - `risky`  — needs its own confirmation, and is excluded from "select all".
 * - `never`  — cannot reach this view at all; the backend refuses it before the preview.
 */
import { useCallback, useEffect, useMemo, useState } from "react";

import { formatBytes } from "../lib/format";
import {
  api,
  toAppError,
  type ItemOutcome,
  type Preview,
  type PreviewItem,
  type Report,
  type Safety,
} from "../lib/ipc";
import { notify } from "../lib/notices";

type Stage = "idle" | "previewing" | "confirming" | "executing" | "reported";

const SAFETY_LABEL: Record<Safety, string> = {
  safe: "Safe",
  review: "Review",
  risky: "Risky",
  never: "Never",
};

const SAFETY_EXPLAINS: Record<Safety, string> = {
  safe: "Regenerable. Nothing you can see is lost.",
  review: "Reclaimable, but it costs you something.",
  risky: "Could break something running, or lose data.",
  never: "Not reclaimable.",
};

function outcomeKey(o: ItemOutcome): number {
  return o.id;
}

export default function Reclaim() {
  const [stage, setStage] = useState<Stage>("idle");
  const [preview, setPreview] = useState<Preview | null>(null);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [report, setReport] = useState<Report | null>(null);

  // A preview is only good until it is replaced, so drop it on leaving.
  useEffect(() => () => void api.reclaimClear().catch(() => {}), []);

  const runPreview = useCallback(async () => {
    setStage("previewing");
    setReport(null);
    try {
      const next = await api.reclaimPreview();
      setPreview(next);
      // Pre-check only what is safe. Never pre-check something with a cost.
      setSelected(new Set(next.items.filter((i) => i.safety === "safe").map((i) => i.id)));
      setStage(next.items.length > 0 ? "confirming" : "idle");
      if (next.items.length === 0) notify.info("Nothing to reclaim right now.");
    } catch (thrown) {
      notify.error(toAppError(thrown));
      setStage("idle");
    }
  }, []);

  const selectedItems = useMemo(
    () => (preview ? preview.items.filter((i) => selected.has(i.id)) : []),
    [preview, selected],
  );
  const selectedBytes = selectedItems.reduce((sum, i) => sum + i.bytes, 0);
  const hasRisky = selectedItems.some((i) => i.safety === "risky");

  function toggle(item: PreviewItem) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(item.id)) next.delete(item.id);
      else next.add(item.id);
      return next;
    });
  }

  function selectAllSelectable() {
    if (!preview) return;
    // Risky items are deliberately excluded: bulk selection must never sweep up something that
    // needs its own decision.
    setSelected(new Set(preview.items.filter((i) => i.safety !== "risky").map((i) => i.id)));
  }

  async function execute() {
    if (!preview || selectedItems.length === 0) return;
    setStage("executing");
    try {
      const result = await api.reclaimExecute(preview.ticket, [...selected]);
      setReport(result);
      setStage("reported");
      setPreview(null);
      setSelected(new Set());

      if (result.failed_count > 0 || result.outcomes.some((o) => o.outcome === "failed")) {
        notify.warning(
          "Some items could not be reclaimed.",
          "The report below says which, and why.",
        );
      } else {
        notify.success(`Freed ${formatBytes(result.freed)}.`);
      }
    } catch (thrown) {
      notify.error(toAppError(thrown));
      setStage("confirming");
    }
  }

  return (
    <section className="stack stack-wide">
      {/* ---------- 1. preview ---------- */}
      {(stage === "idle" || stage === "previewing") && (
        <div className="card">
          <h2>Find reclaimable space</h2>
          <p className="muted">
            This looks, and shows you what it found. Nothing is removed until you review the list
            and confirm.
          </p>
          <button type="button" onClick={() => void runPreview()} disabled={stage === "previewing"}>
            {stage === "previewing" ? "Looking…" : "Look for reclaimable space"}
          </button>
        </div>
      )}

      {/* ---------- 2. confirm ---------- */}
      {stage === "confirming" && preview && (
        <>
          <div className="card">
            <div className="summary">
              <div>
                <span className="summary-figure">{formatBytes(preview.total_bytes)}</span>
                <span className="muted">found</span>
              </div>
              <div>
                <span className="summary-figure">{formatBytes(selectedBytes)}</span>
                <span className="muted">selected</span>
              </div>
              <div>
                <span className="summary-figure">{selectedItems.length}</span>
                <span className="muted">of {preview.items.length} items</span>
              </div>
            </div>
            <div className="row wrap">
              <button type="button" onClick={selectAllSelectable}>
                Select all except risky
              </button>
              <button type="button" onClick={() => setSelected(new Set())}>
                Select none
              </button>
              <button type="button" onClick={() => void runPreview()}>
                Look again
              </button>
            </div>
          </div>

          <ul className="reclaim-list">
            {preview.items.map((item) => (
              <li key={item.id} className={`reclaim-item safety-${item.safety}`}>
                <label className="reclaim-check">
                  <input
                    type="checkbox"
                    checked={selected.has(item.id)}
                    onChange={() => toggle(item)}
                  />
                  <span className="reclaim-body">
                    <span className="reclaim-head">
                      <strong>{item.label}</strong>
                      <span className={`safety-tag safety-${item.safety}`} title={SAFETY_EXPLAINS[item.safety]}>
                        {SAFETY_LABEL[item.safety]}
                      </span>
                      <span className="reclaim-bytes">{formatBytes(item.bytes)}</span>
                    </span>
                    {item.path && <code className="reclaim-path">{item.path}</code>}
                    {/* A cost is shown wherever there is one — a rating that says "this costs
                        something" without saying what gives nothing to decide with. */}
                    {item.cost && <span className="reclaim-cost">{item.cost}</span>}
                  </span>
                </label>
              </li>
            ))}
          </ul>

          {preview.refused.length > 0 && (
            <div className="card">
              <h2>Not touched</h2>
              <p className="muted">
                nix refused these on your behalf. They are listed rather than hidden, so you can see
                what was left alone and why.
              </p>
              <ul className="refusal-list">
                {preview.refused.slice(0, 20).map((r, i) => (
                  <li key={`${r.path}-${i}`}>
                    <code>{r.path}</code>
                    <span className="muted">{r.reason}</span>
                  </li>
                ))}
              </ul>
              {preview.refused.length > 20 && (
                <p className="muted">…and {preview.refused.length - 20} more.</p>
              )}
            </div>
          )}

          <div className="card card-confirm">
            <h2>Confirm</h2>
            {selectedItems.length === 0 ? (
              <p className="muted">Nothing selected.</p>
            ) : (
              <>
                <p>
                  Reclaim <strong>{formatBytes(selectedBytes)}</strong> from{" "}
                  <strong>{selectedItems.length}</strong> item
                  {selectedItems.length === 1 ? "" : "s"}.
                </p>
                {hasRisky && (
                  <p className="caveat">
                    Your selection includes items marked risky. These may break something that is
                    running, or lose data.
                  </p>
                )}
                <ul className="confirm-list">
                  {selectedItems.map((i) => (
                    <li key={i.id}>
                      <span>{i.label}</span>
                      <span>{formatBytes(i.bytes)}</span>
                    </li>
                  ))}
                </ul>
              </>
            )}
            <button
              type="button"
              className="danger"
              disabled={selectedItems.length === 0}
              onClick={() => void execute()}
            >
              Reclaim {selectedItems.length > 0 ? formatBytes(selectedBytes) : "nothing"}
            </button>
          </div>
        </>
      )}

      {/* ---------- 3. executing ---------- */}
      {stage === "executing" && (
        <div className="card">
          <h2>Reclaiming</h2>
          <div className="progress">
            <div className="progress-bar progress-indeterminate" />
          </div>
          <p className="muted">Each item is re-checked immediately before it is touched.</p>
        </div>
      )}

      {/* ---------- 4. report ---------- */}
      {stage === "reported" && report && (
        <>
          <div className="card">
            <h2>Done</h2>
            <div className="summary">
              <div>
                <span className="summary-figure">{formatBytes(report.freed)}</span>
                <span className="muted">freed</span>
              </div>
              <div>
                <span className="summary-figure">{report.reclaimed_count}</span>
                <span className="muted">reclaimed</span>
              </div>
              {report.skipped_count > 0 && (
                <div>
                  <span className="summary-figure">{report.skipped_count}</span>
                  <span className="muted">skipped</span>
                </div>
              )}
              {report.failed_count > 0 && (
                <div>
                  <span className="summary-figure">{report.failed_count}</span>
                  <span className="muted">failed</span>
                </div>
              )}
            </div>

            {/* nix checks its own arithmetic against the filesystem, and says so either way. */}
            {report.measured_delta !== null && (
              <p className={report.measurement_agrees === false ? "caveat" : "muted"}>
                {report.measurement_agrees === false
                  ? `nix counted ${formatBytes(report.freed)} but the filesystem moved by ${formatBytes(report.measured_delta)}. The difference is worth knowing about — copy-on-write filesystems and snapshots can hold onto space that looks freed.`
                  : `The filesystem confirms it: ${formatBytes(report.measured_delta)} came back.`}
              </p>
            )}
            {report.cancelled && <p className="caveat">Stopped early, so not everything was done.</p>}

            <button type="button" onClick={() => void runPreview()}>
              Look again
            </button>
          </div>

          <div className="card">
            <h2>What happened to each item</h2>
            <ul className="outcome-list">
              {report.outcomes.map((o) => (
                <li key={outcomeKey(o)} className={`outcome outcome-${o.outcome}`}>
                  <span className="outcome-tag">{o.outcome}</span>
                  <code>{o.path}</code>
                  <span className="muted">
                    {o.outcome === "reclaimed" && formatBytes(o.bytes)}
                    {o.outcome === "skipped" && o.reason}
                    {o.outcome === "failed" && o.error.message}
                  </span>
                </li>
              ))}
            </ul>
          </div>
        </>
      )}
    </section>
  );
}
