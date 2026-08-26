/**
 * React binding for the long-operation primitive. Task 0.3 (FND-2).
 *
 * The whole point of the backend primitive is that this hook is written once: every scan, search
 * and package query gets progress and cancellation for free, rather than each growing its own
 * slightly different version.
 */
import { useCallback, useEffect, useRef, useState } from "react";

import { api, onDone, onProgress, toAppError, type AppError, type Progress } from "./ipc";
import { notify } from "./notices";

export type OperationState = {
  /** Whether an operation is in flight. */
  running: boolean;
  /** Latest progress report, if any. */
  progress: Progress | null;
  /** How the last operation ended. */
  outcome: "done" | "cancelled" | "failed" | null;
  /** The failure, when the outcome was `failed`. */
  error: AppError | null;
};

const IDLE: OperationState = { running: false, progress: null, outcome: null, error: null };

/**
 * Track one operation at a time.
 *
 * `start` takes a launcher that returns the operation's id, so the same hook drives any command
 * that follows the convention.
 */
export function useOperation() {
  const [state, setState] = useState<OperationState>(IDLE);
  const activeId = useRef<number | null>(null);

  useEffect(() => {
    let disposed = false;
    const unlisteners: Array<() => void> = [];

    void onProgress((p) => {
      if (disposed || p.id !== activeId.current) return;
      setState((prev) => ({ ...prev, progress: p }));
    }).then((un) => unlisteners.push(un));

    void onDone((c) => {
      if (disposed || c.id !== activeId.current) return;
      activeId.current = null;
      if (c.outcome === "failed") {
        notify.error(c.error);
        setState({ running: false, progress: null, outcome: "failed", error: c.error });
      } else {
        if (c.outcome === "cancelled") notify.info("Stopped.");
        setState({ running: false, progress: null, outcome: c.outcome, error: null });
      }
    }).then((un) => unlisteners.push(un));

    return () => {
      disposed = true;
      for (const un of unlisteners) un();
    };
  }, []);

  const start = useCallback(async (launch: () => Promise<number>) => {
    setState({ running: true, progress: null, outcome: null, error: null });
    try {
      activeId.current = await launch();
    } catch (thrown) {
      const error = toAppError(thrown);
      notify.error(error);
      activeId.current = null;
      setState({ running: false, progress: null, outcome: "failed", error });
    }
  }, []);

  const cancel = useCallback(async () => {
    const id = activeId.current;
    if (id === null) return;
    try {
      await api.operationCancel(id);
    } catch (thrown) {
      notify.error(toAppError(thrown));
    }
  }, []);

  return { ...state, start, cancel };
}
