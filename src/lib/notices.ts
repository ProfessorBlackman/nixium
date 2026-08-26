/**
 * The notification centre's store. Task 0.5 (FND-3).
 *
 * Stacer had no error surface at all: it implemented a file logger and never installed it, and
 * discarded stderr and exit status everywhere, so a failed operation was indistinguishable from a
 * successful one. Every failure in nix lands here, and the panel that renders it is always
 * reachable.
 *
 * A tiny external store rather than React context, per the D8 architectural rules: a 1 Hz metrics
 * tick must not be able to re-render a subtree.
 */
import { useSyncExternalStore } from "react";

import type { AppError } from "./ipc";

export type NoticeLevel = "error" | "warning" | "info" | "success";

export type Notice = {
  id: number;
  level: NoticeLevel;
  /** One sentence, addressed to the user. */
  title: string;
  /** What they can do about it. */
  remedy: string | null;
  /** The underlying failure, shown on demand. */
  detail: string | null;
  /** Stable machine code, when this came from an AppError. */
  code: string | null;
  at: Date;
  read: boolean;
};

const MAX_NOTICES = 200;

let notices: Notice[] = [];
let nextId = 1;
const listeners = new Set<() => void>();

function emit() {
  // A new array identity is what tells useSyncExternalStore something changed.
  notices = [...notices];
  for (const l of listeners) l();
}

function push(notice: Omit<Notice, "id" | "at" | "read">) {
  notices.unshift({ ...notice, id: nextId++, at: new Date(), read: false });
  if (notices.length > MAX_NOTICES) notices.length = MAX_NOTICES;
  emit();
}

/** Render an AppError's cause into one readable line. */
function describeCause(error: AppError): string | null {
  const parts: string[] = [];

  if (error.context.length > 0) {
    // Outermost first, which is the order a user thinks in.
    parts.push([...error.context].reverse().map((c) => `while ${c}`).join(", "));
  }
  if (error.path) parts.push(error.path);

  const cause = error.cause;
  if (cause) {
    switch (cause.kind) {
      case "os":
        parts.push(cause.errno === null ? cause.description : `${cause.description} (errno ${cause.errno})`);
        break;
      case "command":
        parts.push(
          [
            `${cause.program} failed`,
            cause.status === null ? null : `status ${cause.status}`,
            cause.stderr.trim() || null,
          ]
            .filter(Boolean)
            .join(": "),
        );
        break;
      case "malformed":
        parts.push(`${cause.source}: ${cause.detail}`);
        break;
      case "other":
        parts.push(cause.detail);
        break;
    }
  }

  return parts.length > 0 ? parts.join(" — ") : null;
}

export const notify = {
  /**
   * Record a failure.
   *
   * Cancellation is not a fault, so it is filed as information rather than an error — the UI must
   * never tell someone their own "stop" was a problem.
   */
  error(error: AppError) {
    const cancelled = error.code === "cancelled";
    push({
      level: cancelled ? "info" : "error",
      title: error.message,
      remedy: error.remedy,
      detail: describeCause(error),
      code: error.code,
    });
  },
  warning(title: string, remedy: string | null = null, detail: string | null = null) {
    push({ level: "warning", title, remedy, detail, code: null });
  },
  info(title: string, detail: string | null = null) {
    push({ level: "info", title, remedy: null, detail, code: null });
  },
  success(title: string, detail: string | null = null) {
    push({ level: "success", title, remedy: null, detail, code: null });
  },
};

export function markAllRead() {
  for (const n of notices) n.read = true;
  emit();
}

export function clearNotices() {
  notices = [];
  emit();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function snapshot() {
  return notices;
}

/** Subscribe a component to the notice list. */
export function useNotices(): Notice[] {
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

/** Count of unread notices, for the indicator. */
export function useUnreadCount(): number {
  return useNotices().filter((n) => !n.read).length;
}

/** Test seam: reset module state between tests. */
export function __resetNoticesForTest() {
  notices = [];
  nextId = 1;
  listeners.clear();
}
