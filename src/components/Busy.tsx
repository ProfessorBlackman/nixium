// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Loading indicators.
 *
 * # Text first, animation second
 *
 * `prefers-reduced-motion` is honoured globally by killing every animation, which means a spinner
 * alone would sit frozen and say nothing at all to the users who most need to be told something is
 * happening. So every indicator here carries a **label**, and the movement is an enhancement on top of
 * it. The reduced-motion stylesheet turns the indeterminate bar into a full-width static one rather
 * than an invisible sliver.
 *
 * # Announced, not just drawn
 *
 * `role="status"` with `aria-live="polite"`, so a screen reader is told the work started and told
 * again when it finishes. A spinner nobody can see is not an indicator.
 */

/**
 * A small spinner for inside a button, beside text that already says what is happening.
 *
 * `aria-hidden`, deliberately: the button's own label changes to "Scanning…" while it works, so the
 * state is already announced and a second announcement would be noise.
 */
export function Spinner() {
  return <span className="spinner" aria-hidden="true" />;
}

/**
 * A block indicator for a card that is waiting on something.
 *
 * `fraction` draws a real progress bar when the work knows its size; without one the bar is
 * indeterminate, which is honest — a fake percentage that jumps is worse than one that does not
 * pretend to know.
 */
export function Busy({ label, fraction }: { label: string; fraction?: number | null }) {
  const known = typeof fraction === "number" && Number.isFinite(fraction);

  return (
    <div className="busy" role="status" aria-live="polite">
      <div className="progress">
        <div
          className={known ? "progress-bar" : "progress-bar progress-indeterminate"}
          style={known ? { width: `${Math.round(Math.min(1, Math.max(0, fraction)) * 100)}%` } : undefined}
        />
      </div>
      <p className="busy-label">
        <Spinner />
        {label}
      </p>
    </div>
  );
}

/**
 * A one-line indicator for somewhere a bar would be too heavy — a table header, or beside a control.
 */
export function BusyInline({ label }: { label: string }) {
  return (
    <span className="busy-inline" role="status" aria-live="polite">
      <Spinner />
      {label}
    </span>
  );
}
