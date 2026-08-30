// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Squarified treemap on canvas. Task 1.5 (`STO-2`).
 *
 * **Canvas, not DOM** — D8's first binding rule. A treemap over a real home directory has tens of
 * thousands of nodes; as DOM elements that is a non-starter in any framework, which is precisely
 * why the framework choice was decided on the assumption this would be canvas.
 *
 * Two things keep it fast at that scale:
 *
 * - **Sub-pixel aggregation.** Anything that would render smaller than a few pixels is folded into
 *   a single "smaller items" tile rather than drawn. Never render what cannot be seen.
 * - **One pass, no per-node state.** Layout is computed into a flat array each paint; there is no
 *   React node per tile and no reconciliation.
 *
 * The algorithm is squarified treemapping (Bruls, Huizing, van Wijk): tiles are laid out in rows
 * whose aspect ratios are kept as close to square as possible, because long thin slivers are
 * impossible to compare by eye — and comparing areas by eye is the entire purpose.
 */
import { useEffect, useMemo, useRef, useState } from "react";

import { t } from "../lib/i18n";
import type { SpaceEntry, SpaceTree } from "../lib/ipc";
import { formatBytes } from "../lib/format";

/** Below this many pixels of area, a tile is aggregated rather than drawn. */
const MIN_TILE_AREA = 24;
/** Tiles at least this tall get a label drawn inside them. */
const MIN_LABEL_HEIGHT = 22;
/** Tiles at least this wide get a label drawn inside them. */
const MIN_LABEL_WIDTH = 44;

type Rect = { x: number; y: number; w: number; h: number };

type Tile = Rect & {
  /** Absent for the synthetic "smaller items" tile. */
  entry: SpaceEntry | null;
  label: string;
  bytes: number;
  /** How many entries this tile stands for. More than one means it is an aggregate. */
  count: number;
};

/**
 * Squarify one row of children into `rect`, returning the tiles and the remaining space.
 *
 * Standard squarified layout: fill along the shorter side so rows stay close to square.
 */
function squarify(items: Array<{ bytes: number; entry: SpaceEntry | null; label: string; count: number }>, rect: Rect): Tile[] {
  const tiles: Tile[] = [];
  const total = items.reduce((sum, i) => sum + i.bytes, 0);
  if (total <= 0) return tiles;

  let remaining = { ...rect };
  let remainingTotal = total;
  let index = 0;

  while (index < items.length && remaining.w > 0.5 && remaining.h > 0.5) {
    const horizontal = remaining.w >= remaining.h;
    const side = horizontal ? remaining.h : remaining.w;

    // Grow the row while doing so improves its worst aspect ratio.
    let rowBytes = 0;
    let rowEnd = index;
    let bestRatio = Number.POSITIVE_INFINITY;

    while (rowEnd < items.length) {
      const candidateBytes = rowBytes + items[rowEnd].bytes;
      const rowLength = (candidateBytes / remainingTotal) * (horizontal ? remaining.w : remaining.h);
      if (rowLength <= 0) {
        rowEnd += 1;
        continue;
      }
      // Worst aspect ratio in the row if we stop here.
      let worst = 1;
      for (let i = index; i <= rowEnd; i++) {
        const share = items[i].bytes / candidateBytes;
        const thickness = share * side;
        if (thickness > 0) {
          worst = Math.max(worst, Math.max(rowLength / thickness, thickness / rowLength));
        }
      }
      if (worst > bestRatio) break;
      bestRatio = worst;
      rowBytes = candidateBytes;
      rowEnd += 1;
    }

    if (rowEnd === index) rowEnd = index + 1;
    if (rowBytes <= 0) rowBytes = items.slice(index, rowEnd).reduce((s, i) => s + i.bytes, 0);
    if (rowBytes <= 0) break;

    const rowLength = (rowBytes / remainingTotal) * (horizontal ? remaining.w : remaining.h);
    let offset = 0;

    for (let i = index; i < rowEnd; i++) {
      const share = items[i].bytes / rowBytes;
      const thickness = share * side;
      tiles.push({
        x: horizontal ? remaining.x : remaining.x + offset,
        y: horizontal ? remaining.y + offset : remaining.y,
        w: horizontal ? rowLength : thickness,
        h: horizontal ? thickness : rowLength,
        entry: items[i].entry,
        label: items[i].label,
        bytes: items[i].bytes,
        count: items[i].count,
      });
      offset += thickness;
    }

    if (horizontal) {
      remaining = { x: remaining.x + rowLength, y: remaining.y, w: remaining.w - rowLength, h: remaining.h };
    } else {
      remaining = { x: remaining.x, y: remaining.y + rowLength, w: remaining.w, h: remaining.h - rowLength };
    }
    remainingTotal -= rowBytes;
    index = rowEnd;
  }

  return tiles;
}

/**
 * Fold entries too small to see into one aggregate tile.
 *
 * Returns items sorted largest first, which squarified layout requires.
 */
function aggregateSmall(
  entries: SpaceEntry[],
  total: number,
  area: number,
): Array<{ bytes: number; entry: SpaceEntry | null; label: string; count: number }> {
  const sorted = [...entries].sort((a, b) => b.allocated - a.allocated);
  const items: Array<{ bytes: number; entry: SpaceEntry | null; label: string; count: number }> = [];
  let foldedBytes = 0;
  let foldedCount = 0;

  for (const entry of sorted) {
    if (entry.allocated <= 0) continue;
    const tileArea = (entry.allocated / total) * area;
    if (tileArea < MIN_TILE_AREA) {
      foldedBytes += entry.allocated;
      foldedCount += 1;
    } else {
      items.push({ bytes: entry.allocated, entry, label: entry.label, count: 1 });
    }
  }

  if (foldedCount > 0) {
    items.push({
      bytes: foldedBytes,
      entry: null,
      label: `${foldedCount} smaller item${foldedCount === 1 ? "" : "s"}`,
      count: foldedCount,
    });
  }

  return items;
}

/** Deterministic hue per label, so a directory keeps its colour between paints. */
function hueFor(label: string): number {
  let hash = 0;
  for (let i = 0; i < label.length; i++) hash = (hash * 31 + label.charCodeAt(i)) % 360;
  return hash;
}

export function Treemap({
  tree,
  rootId,
  onOpen,
}: {
  tree: SpaceTree;
  rootId: string;
  onOpen: (entry: SpaceEntry) => void;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [hovered, setHovered] = useState<Tile | null>(null);
  const tilesRef = useRef<Tile[]>([]);

  // Track the container's size, so the map fills whatever space it is given.
  useEffect(() => {
    const wrap = wrapRef.current;
    if (!wrap) return;
    const observer = new ResizeObserver((entries) => {
      const box = entries[0]?.contentRect;
      if (box) setSize({ w: Math.floor(box.width), h: Math.floor(box.height) });
    });
    observer.observe(wrap);
    return () => observer.disconnect();
  }, []);

  const children = useMemo(() => {
    const root = tree.entries[rootId];
    if (!root) return [];
    return root.children.map((id) => tree.entries[id]).filter((e): e is SpaceEntry => Boolean(e));
  }, [tree, rootId]);

  const total = useMemo(() => children.reduce((sum, c) => sum + c.allocated, 0), [children]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || size.w === 0 || size.h === 0) return;

    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.floor(size.w * dpr);
    canvas.height = Math.floor(size.h * dpr);
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    const style = getComputedStyle(document.documentElement);
    const ground = style.getPropertyValue("--surface").trim() || "#fff";
    const ink = style.getPropertyValue("--ink").trim() || "#000";
    const faint = style.getPropertyValue("--ink-faint").trim() || "#888";
    const dark = document.documentElement.dataset.theme === "dark";

    ctx.fillStyle = ground;
    ctx.fillRect(0, 0, size.w, size.h);

    if (total <= 0) {
      ctx.fillStyle = faint;
      ctx.font = "italic 13px system-ui, sans-serif";
      ctx.fillText("Nothing to show here.", 12, 24);
      tilesRef.current = [];
      return;
    }

    const items = aggregateSmall(children, total, size.w * size.h);
    const tiles = squarify(items, { x: 0, y: 0, w: size.w, h: size.h });
    tilesRef.current = tiles;

    for (const tile of tiles) {
      const hue = tile.entry ? hueFor(tile.label) : 0;
      const isAggregate = tile.entry === null;
      ctx.fillStyle = isAggregate
        ? dark
          ? "#2a303c"
          : "#e1e5eb"
        : `hsl(${hue} ${dark ? "42% 34%" : "58% 78%"})`;
      ctx.fillRect(tile.x, tile.y, tile.w, tile.h);

      ctx.strokeStyle = ground;
      ctx.lineWidth = 1;
      ctx.strokeRect(tile.x + 0.5, tile.y + 0.5, tile.w - 1, tile.h - 1);

      // Only label a tile that can actually hold text.
      if (tile.h >= MIN_LABEL_HEIGHT && tile.w >= MIN_LABEL_WIDTH) {
        ctx.fillStyle = ink;
        ctx.font = "500 11px system-ui, sans-serif";
        const padding = 5;
        const maxWidth = tile.w - padding * 2;
        let label = tile.label;
        while (label.length > 1 && ctx.measureText(label).width > maxWidth) {
          label = `${label.slice(0, -2)}…`;
        }
        ctx.fillText(label, tile.x + padding, tile.y + padding + 10);

        if (tile.h >= MIN_LABEL_HEIGHT + 12) {
          ctx.fillStyle = faint;
          ctx.font = "10px ui-monospace, monospace";
          ctx.fillText(formatBytes(tile.bytes), tile.x + padding, tile.y + padding + 23);
        }
      }
    }
  }, [children, total, size]);

  function tileAt(clientX: number, clientY: number): Tile | null {
    const canvas = canvasRef.current;
    if (!canvas) return null;
    const box = canvas.getBoundingClientRect();
    const x = clientX - box.left;
    const y = clientY - box.top;
    return (
      tilesRef.current.find((t) => x >= t.x && x < t.x + t.w && y >= t.y && y < t.y + t.h) ?? null
    );
  }

  return (
    <div className="treemap" ref={wrapRef}>
      <canvas
        ref={canvasRef}
        style={{ width: "100%", height: "100%", display: "block", cursor: hovered?.entry?.is_dir ? "pointer" : "default" }}
        onMouseMove={(e) => setHovered(tileAt(e.clientX, e.clientY))}
        onMouseLeave={() => setHovered(null)}
        onClick={(e) => {
          const tile = tileAt(e.clientX, e.clientY);
          if (tile?.entry?.is_dir) onOpen(tile.entry);
        }}
        role="img"
        aria-label={`Treemap of ${children.length} items totalling ${formatBytes(total)}`}
      />
      {hovered && (
        <div className="treemap-tip" role="status">
          <strong>{hovered.label}</strong>
          <span>{formatBytes(hovered.bytes)}</span>
          {hovered.entry?.path && <code>{hovered.entry.path}</code>}
          {hovered.entry === null && (
            <span className="muted">{t("Too small to show individually. Use the table below.")}</span>
          )}
        </div>
      )}
    </div>
  );
}
