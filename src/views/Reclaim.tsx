// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

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
 *
 * # The confirm stage is two columns
 *
 * Choosing what to reclaim and reviewing what you chose are one task done in both directions, so
 * they sit side by side: the item list on the left, the confirm panel on the right. **Each scrolls
 * on its own.** Stacked, a forty-item preview pushed the action off the bottom of the screen and a
 * long selection pushed the item list off the top; independently scrolled, neither can put the other
 * out of reach.
 *
 * Inside the confirm panel the action is **pinned to the top**, and what is pinned is the button
 * *together with both caveats* — risky items, and bytes that only reach the trash. A button that
 * stayed visible while those two scrolled away would be a worse design than one that scrolled with
 * them: the warnings are what make pressing it an informed act, and the safe items arrive
 * pre-checked, so the button is live from the first render. Only the itemised list moves.
 *
 * Below one column the grid collapses to a single column. Two cramped columns are worse than one
 * readable one, and the confirm panel is the last thing that should be squeezed to fit.
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
  // The part of the selection that would only be staged in the trash rather than freed.
  const selectedTrashable = selectedItems
    .filter((i) => i.method.method === "move_to_trash")
    .reduce((sum, i) => sum + i.bytes, 0);
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
        // Trashed bytes are not freed bytes: the trash is on the same filesystem by necessity, so
        // the move is a rename and free space does not change until it is emptied.
        notify.success(
          result.trashed > 0
            ? `Freed ${formatBytes(result.freed)}. ${formatBytes(result.trashed)} moved to the trash — empty it to reclaim that too.`
            : `Freed ${formatBytes(result.freed)}.`,
        );
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
                <span className="summary-figure">
                  {formatBytes(
                    preview.promisable_bytes < preview.total_bytes
                      ? preview.promisable_bytes
                      : preview.total_bytes,
                  )}
                </span>
                <span className="muted">
                  {preview.promisable_bytes < preview.total_bytes ? "certain to free" : "found"}
                </span>
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
            {/* When some entries are qualified, the difference is stated rather than papered over.
                The headline is the promise; this is the optimistic case beside it. */}
            {preview.promisable_bytes < preview.total_bytes && (
              <p className="caveat">
                Up to {formatBytes(preview.total_bytes)} was found, but only{" "}
                {formatBytes(preview.promisable_bytes)} is certain to come back. The rest sits on a
                copy-on-write filesystem where space can be shared with snapshots — deleting it may
                return less, or nothing.
              </p>
            )}
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

          {/* The two panels that are worked in together: pick on the left, review on the right.
              Each scrolls on its own so neither can push the other out of reach. */}
          <div className="reclaim-columns">
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
                        {/* A qualified size is shown as an upper bound, never as a bare figure:
                            on a copy-on-write filesystem the space may not come back at all. */}
                        <span className="reclaim-bytes">
                          {item.reclaimable.confidence === "exact"
                            ? formatBytes(item.bytes)
                            : `up to ${formatBytes(item.bytes)}`}
                        </span>
                      </span>
                      {item.path && <code className="reclaim-path">{item.path}</code>}
                    {/* What reclaiming this actually does, at the point the decision is made rather
                        than in a manual nobody opens. Shown on the first item of each category, so a
                        list of nine caches carries the sentence once. */}
                    {preview.explanations[item.category] !== undefined &&
                      preview.items.findIndex((other) => other.category === item.category) ===
                        preview.items.indexOf(item) && (
                        <span className="reclaim-explains">
                          {preview.explanations[item.category]}
                        </span>
                      )}
                      {/* A cost is shown wherever there is one — a rating that says "this costs
                          something" without saying what gives nothing to decide with. */}
                      {item.cost && <span className="reclaim-cost">{item.cost}</span>}
                      {item.reclaimable.confidence !== "exact" && (
                        <span className="reclaim-sharing">{item.reclaimable.reason}</span>
                      )}
                    </span>
                  </label>
                </li>
              ))}
            </ul>

            <div className="card card-confirm">
              {/* Sticky, and it holds more than the button. The itemised selection below can be long
                  enough to scroll, and a button that stayed visible while the two caveats scrolled
                  away would be worse than one that scrolled with them — the warnings are the reason
                  pressing it is an informed act. So the action and both caveats pin together, and
                  only the list of items moves. */}
              <div className="confirm-action">
                {/* The card lost its visible heading when the button took the top slot. Kept for
                    document structure, since a panel with no accessible name is worse than a
                    redundant one. */}
                <h2 className="visually-hidden">Confirm</h2>
                <button
                  type="button"
                  className="danger"
                  disabled={selectedItems.length === 0}
                  onClick={() => void execute()}
                >
                  Reclaim {selectedItems.length > 0 ? formatBytes(selectedBytes) : "nothing"}
                </button>

                {selectedItems.length === 0 ? (
                  <p className="muted">Nothing selected. Tick something in the list.</p>
                ) : (
                  <p className="muted">
                    from {selectedItems.length} item{selectedItems.length === 1 ? "" : "s"}
                  </p>
                )}

                {hasRisky && (
                  <p className="caveat">
                    Your selection includes items marked risky. These may break something that is
                    running, or lose data.
                  </p>
                )}
                {/* Said before committing, not only afterwards: trashing is a rename within the same
                    filesystem, so it frees nothing until the trash is emptied. */}
                {selectedTrashable > 0 && (
                  <p className="caveat">
                    {formatBytes(selectedTrashable)} of this goes to the trash, which is reversible
                    but on the same disk — so that space comes back only once you empty it. nix
                    offers emptying the trash as its own item.
                  </p>
                )}
              </div>

              {selectedItems.length > 0 && (
                <ul className="confirm-list">
                  {selectedItems.map((i) => (
                    <li key={i.id}>
                      <span>{i.label}</span>
                      <span>{formatBytes(i.bytes)}</span>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </div>

          {preview.advisories.length > 0 && (
            <div className="card">
              <h2>Worth knowing about</h2>
              <p className="muted">
                Space nix can measure but will not reclaim for you. Each one says why, and the
                command you can run yourself if you want to.
              </p>
              <ul className="advisory-list">
                {preview.advisories.map((a) => (
                  <li key={`${a.category}-${a.label}`}>
                    <div className="advisory-head">
                      <span className="advisory-label">{a.label}</span>
                      <span className="advisory-bytes">{formatBytes(a.bytes)}</span>
                    </div>
                    {a.path !== null && <code className="advisory-path">{a.path}</code>}
                    <p className="muted">{a.why_manual}</p>
                    <pre className="advisory-remedy">
                      <code>{a.remedy}</code>
                    </pre>
                  </li>
                ))}
              </ul>
              <p className="muted">
                These are not counted in the totals above, because those totals are what this
                preview would actually reclaim.
              </p>
            </div>
          )}

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
              {report.trashed > 0 && (
                <div>
                  <span className="summary-figure">{formatBytes(report.trashed)}</span>
                  <span className="muted">in the trash</span>
                </div>
              )}
              <div>
                <span className="summary-figure">{report.reclaimed_count}</span>
                <span className="muted">acted on</span>
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
                  ? `nix counted ${formatBytes(report.freed)} freed but the filesystem moved by ${formatBytes(report.measured_delta)}. The difference is worth knowing about — copy-on-write filesystems and snapshots can hold onto space that looks freed.`
                  : `The filesystem confirms it: ${formatBytes(report.measured_delta)} came back.`}
              </p>
            )}
            {report.trashed > 0 && (
              <p className="caveat">
                {formatBytes(report.trashed)} was moved to the trash, which sits on the same
                filesystem — so that space has not come back yet. Emptying the trash is what reclaims
                it, and nix offers that as its own item.
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
                    {o.outcome === "trashed" && `${formatBytes(o.bytes)} — recoverable from the trash`}
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
