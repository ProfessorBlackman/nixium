// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Virtualised table over a scan's entries. Task 1.6 (`STO-2`).
 *
 * **Virtualised** — D8's second binding rule. Only the rows in view exist in the DOM, so
 * reconciliation cost is bounded by viewport height rather than by result size. Hand-rolled rather
 * than pulled from a library: the requirement is one fixed-height list, and a windowing calculation
 * is a dozen lines.
 *
 * Rows are the children of the current directory, sorted largest first, with a share bar so
 * relative size reads at a glance rather than requiring the numbers to be compared.
 */
import { useMemo, useRef, useState } from "react";

import type { SpaceEntry, SpaceTree } from "../lib/ipc";
import { formatBytes, formatPercent } from "../lib/format";

const ROW_HEIGHT = 30;
/** Rows rendered beyond the viewport, so scrolling does not reveal blank space. */
const OVERSCAN = 6;

type SortKey = "size" | "name";

export function SpaceTable({
  tree,
  rootId,
  onOpen,
}: {
  tree: SpaceTree;
  rootId: string;
  onOpen: (entry: SpaceEntry) => void;
}) {
  const [sort, setSort] = useState<SortKey>("size");
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(400);
  const bodyRef = useRef<HTMLDivElement | null>(null);

  const rows = useMemo(() => {
    const root = tree.entries[rootId];
    if (!root) return [];
    const children = root.children
      .map((id) => tree.entries[id])
      .filter((e): e is SpaceEntry => Boolean(e));
    return children.sort((a, b) =>
      sort === "size" ? b.allocated - a.allocated : a.label.localeCompare(b.label),
    );
  }, [tree, rootId, sort]);

  const largest = rows.length > 0 ? Math.max(...rows.map((r) => r.allocated)) : 0;

  // The window: only these rows are mounted.
  const first = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN);
  const visibleCount = Math.ceil(viewportHeight / ROW_HEIGHT) + OVERSCAN * 2;
  const window = rows.slice(first, first + visibleCount);

  return (
    <div className="table">
      <div className="table-head" role="row">
        <button
          type="button"
          className={sort === "name" ? "th is-sorted" : "th"}
          onClick={() => setSort("name")}
        >
          Name
        </button>
        <button
          type="button"
          className={sort === "size" ? "th is-sorted" : "th"}
          onClick={() => setSort("size")}
        >
          On disk
        </button>
        <span className="th th-static">Share</span>
      </div>

      <div
        className="table-body"
        ref={bodyRef}
        onScroll={(e) => {
          setScrollTop(e.currentTarget.scrollTop);
          setViewportHeight(e.currentTarget.clientHeight);
        }}
        role="rowgroup"
      >
        {rows.length === 0 ? (
          <p className="empty">Nothing in this directory.</p>
        ) : (
          // A spacer of the full height gives the scrollbar its true size while only the window
          // exists in the DOM.
          <div style={{ height: rows.length * ROW_HEIGHT, position: "relative" }}>
            {window.map((entry, i) => {
              const index = first + i;
              const share = largest > 0 ? entry.allocated / largest : 0;
              return (
                <div
                  key={entry.id}
                  className={
                    entry.provenance.by === "aggregated"
                      ? "tr is-aggregate"
                      : entry.is_dir
                        ? "tr is-dir"
                        : "tr"
                  }
                  style={{ position: "absolute", top: index * ROW_HEIGHT, height: ROW_HEIGHT }}
                  role="row"
                  tabIndex={0}
                  onClick={() => entry.is_dir && onOpen(entry)}
                  onKeyDown={(e) => {
                    if ((e.key === "Enter" || e.key === " ") && entry.is_dir) {
                      e.preventDefault();
                      onOpen(entry);
                    }
                  }}
                >
                  <span className="td td-name" title={entry.path ?? entry.label}>
                    <span aria-hidden="true" className="td-icon">
                      {entry.provenance.by === "aggregated" ? "⋯" : entry.is_dir ? "▸" : "·"}
                    </span>
                    {entry.label}
                  </span>
                  <span className="td td-size">{formatBytes(entry.allocated)}</span>
                  <span className="td td-share">
                    <span className="share-track">
                      <span className="share-fill" style={{ width: `${share * 100}%` }} />
                    </span>
                    <span className="share-value">{formatPercent(share)}</span>
                  </span>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
