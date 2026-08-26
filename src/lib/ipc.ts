/**
 * Typed wrappers over the Tauri command surface. Task 0.3 (FND-2).
 *
 * Every command goes through here, and every one returns `AppError` on failure — the same typed
 * shape the backend produced, never a string. The types under `src/bindings/` are generated from
 * the Rust definitions by ts-rs, so a backend change that is not reflected here fails the
 * type-check rather than drifting silently.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { AppError } from "../bindings/AppError";
import type { Completion } from "../bindings/Completion";
import type { Diagnostics } from "../bindings/Diagnostics";
import type { OperationId } from "../bindings/OperationId";
import type { Progress } from "../bindings/Progress";
import type { Filesystem } from "../bindings/Filesystem";
import type { ScanResult } from "../bindings/ScanResult";
import type { Settings } from "../bindings/Settings";
import type { Snapshot } from "../bindings/Snapshot";
import type { SpaceEntry } from "../bindings/SpaceEntry";

export type { AppError, Completion, Diagnostics, Filesystem, Progress, ScanResult, Settings, Snapshot, SpaceEntry };
export type { Accounting } from "../bindings/Accounting";
export type { Category } from "../bindings/Category";
export type { EntryId } from "../bindings/EntryId";
export type { Safety } from "../bindings/Safety";
export type { SpaceTree } from "../bindings/SpaceTree";
export type { Capability } from "../bindings/Capability";
export type { Cause } from "../bindings/Cause";
export type { ErrorCode } from "../bindings/ErrorCode";
export type { LogLevel } from "../bindings/LogLevel";
export type { StartView } from "../bindings/StartView";
export type { Theme } from "../bindings/Theme";

export const EVENT_PROGRESS = "op://progress";
export const EVENT_DONE = "op://done";
export const EVENT_SCAN_DONE = "scan://done";

/** Whether a rejected value is one of our typed errors rather than an unexpected throw. */
export function isAppError(value: unknown): value is AppError {
  return (
    typeof value === "object" &&
    value !== null &&
    "code" in value &&
    "message" in value &&
    typeof (value as { message: unknown }).message === "string"
  );
}

/**
 * Coerce anything thrown into an AppError.
 *
 * A panic in a command, or a bug in this layer, arrives as a plain string. Wrapping it as
 * `internal` keeps the invariant that the UI only ever handles one error shape.
 */
export function toAppError(thrown: unknown): AppError {
  if (isAppError(thrown)) return thrown;
  return {
    code: "internal",
    message: typeof thrown === "string" ? thrown : "Something went wrong inside nix.",
    remedy: "This is a bug in nix. The details below are worth including in a report.",
    cause: { kind: "other", detail: String(thrown) },
    context: [],
    path: null,
  };
}

/** Invoke a command, normalising any rejection into an AppError. */
async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (thrown) {
    throw toAppError(thrown);
  }
}

export type Versions = { app: string; core: string };
export type HelperProbe = { uid: number; elevated: boolean; kernel: string };

export const api = {
  versions: () => call<Versions>("versions"),
  diagnostics: () => call<Diagnostics>("diagnostics"),
  capabilities: () => call<Snapshot>("capabilities"),
  capabilitiesRefresh: () => call<Snapshot>("capabilities_refresh"),
  settingsGet: () => call<Settings>("settings_get"),
  settingsSave: (settings: Settings) => call<Settings>("settings_save", { settings }),
  startupWarning: () => call<AppError | null>("startup_warning"),
  operationCancel: (id: OperationId) => call<boolean>("operation_cancel", { id }),
  operationCount: () => call<number>("operation_count"),
  helperProbe: () => call<HelperProbe>("helper_probe"),
  filesystems: () => call<Filesystem[]>("filesystems"),
  homeDirectory: () => call<string>("home_directory"),
  scanStart: (path: string, maxDepth?: number, crossFilesystems?: boolean) =>
    call<OperationId>("scan_start", {
      path,
      maxDepth: maxDepth ?? null,
      crossFilesystems: crossFilesystems ?? null,
    }),
  /** Phase 0 scaffolding: a slow operation, so progress and cancellation can be verified. */
  demoOperation: (steps: number, failAt?: number) =>
    call<OperationId>("demo_operation", { steps, failAt: failAt ?? null }),
  /** Phase 0 scaffolding: fail on purpose, to exercise the error surface. */
  demoFailure: (code: string) => call<void>("demo_failure", { code }),
};

/** Subscribe to progress for all operations. */
export function onProgress(handler: (p: Progress) => void): Promise<UnlistenFn> {
  return listen<Progress>(EVENT_PROGRESS, (event) => handler(event.payload));
}

/** Subscribe to terminal outcomes for all operations. */
export function onDone(handler: (c: Completion) => void): Promise<UnlistenFn> {
  return listen<Completion>(EVENT_DONE, (event) => handler(event.payload));
}

/** Subscribe to completed scan results. A cancelled scan still delivers its partial tree. */
export function onScanDone(handler: (r: ScanResult) => void): Promise<UnlistenFn> {
  return listen<ScanResult>(EVENT_SCAN_DONE, (event) => handler(event.payload));
}
