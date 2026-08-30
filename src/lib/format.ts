// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Byte and count formatting.
 *
 * Reproduces Stacer's `FormatUtil::formatBytes` deliberately — binary units, one decimal, and the
 * "1 byte" / "N bytes" special cases — so figures match what a user of the old tool expects. It was
 * the single formatting function behind every chart label and table cell there.
 */

const KIBI = 1024;
const MEBI = 1024 * KIBI;
const GIBI = 1024 * MEBI;
const TEBI = 1024 * GIBI;

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes === 1) return "1 byte";
  if (bytes < KIBI) return `${bytes} bytes`;
  if (bytes < MEBI) return `${(bytes / KIBI).toFixed(1)} KiB`;
  if (bytes < GIBI) return `${(bytes / MEBI).toFixed(1)} MiB`;
  if (bytes < TEBI) return `${(bytes / GIBI).toFixed(1)} GiB`;
  return `${(bytes / TEBI).toFixed(1)} TiB`;
}

/** A percentage with no decimals, for space meters. */
export function formatPercent(fraction: number): string {
  return `${Math.round(fraction * 100)}%`;
}

/** Thousands-separated count. */
export function formatCount(n: number): string {
  return n.toLocaleString();
}

/**
 * Relative age in words, for the scan-age label.
 *
 * The explorer opens on the previous scan's result, so saying how old it is matters: a figure
 * presented without its age invites a user to trust a stale number.
 */
export function formatAge(from: Date, now: Date = new Date()): string {
  const seconds = Math.max(0, Math.round((now.getTime() - from.getTime()) / 1000));
  if (seconds < 10) return "just now";
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} minute${minutes === 1 ? "" : "s"} ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} hour${hours === 1 ? "" : "s"} ago`;
  const days = Math.round(hours / 24);
  return `${days} day${days === 1 ? "" : "s"} ago`;
}
