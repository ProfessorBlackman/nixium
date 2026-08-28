// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

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
import type { CachedScan } from "../bindings/CachedScan";
import type { Preview } from "../bindings/Preview";
import type { PreviewItem } from "../bindings/PreviewItem";
import type { Refusal } from "../bindings/Refusal";
import type { Snapshot as CowSnapshot } from "../bindings/Snapshot";
import type { Report } from "../bindings/Report";
import type { Ticket } from "../bindings/Ticket";
import type { Filesystem } from "../bindings/Filesystem";
import type { ScanResult } from "../bindings/ScanResult";
import type { Settings } from "../bindings/Settings";
import type { Capabilities } from "../bindings/Capabilities";
import type { DuplicateReport } from "../bindings/DuplicateReport";
import type { GrowthReport } from "../bindings/GrowthReport";
import type { Metric } from "../bindings/Metric";
import type { Action } from "../bindings/Action";
import type { Detail } from "../bindings/Detail";
import type { Page } from "../bindings/Page";
import type { Scope } from "../bindings/Scope";
import type { Timer } from "../bindings/Timer";
import type { Unit } from "../bindings/Unit";
import type { Package } from "../bindings/Package";
import type { Measured } from "../bindings/Measured";
import type { Manager } from "../bindings/Manager";
import type { ResidualConfig } from "../bindings/ResidualConfig";
import type { UnitFile } from "../bindings/UnitFile";
import type { Process } from "../bindings/Process";
import type { TreeNode } from "../bindings/TreeNode";
import type { ProcessState } from "../bindings/ProcessState";
import type { Signal } from "../bindings/Signal";
import type { Reading } from "../bindings/Reading";
import type { Rule } from "../bindings/Rule";
import type { Sample } from "../bindings/Sample";
import type { Series } from "../bindings/Series";
import type { State as TimerState } from "../bindings/State";
import type { SpaceEntry } from "../bindings/SpaceEntry";

export type { Action, AppError, CachedScan, Completion, Diagnostics, DuplicateReport, Filesystem, GrowthReport, Detail, Metric, Preview, PreviewItem, Process, ProcessState, Progress, Reading, Refusal, Report, Rule, Sample, ScanResult, Series, Settings, Page, Scope, Signal, SpaceEntry, Ticket, Timer, TimerState, TreeNode, Unit, UnitFile, Package, Measured, Manager, ResidualConfig };
export type { Capabilities };
export type { CowSnapshot };
export type { CowKind } from "../bindings/CowKind";
export type { ItemOutcome } from "../bindings/ItemOutcome";
export type { Reclaimable } from "../bindings/Reclaimable";
export type { ReclaimMethod } from "../bindings/ReclaimMethod";
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
/** A finished duplicate search. `STO-15`. */
export const EVENT_DUPLICATES_DONE = "duplicates://done";
/** One metrics reading, once a second while subscribed. `MON-1`. */
export const EVENT_METRICS_TICK = "metrics://tick";
/** The name of a unit that changed. `SVC-3`. */
export const EVENT_UNIT_CHANGED = "units://changed";

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
  capabilities: () => call<Capabilities>("capabilities"),
  capabilitiesRefresh: () => call<Capabilities>("capabilities_refresh"),
  settingsGet: () => call<Settings>("settings_get"),
  settingsSave: (settings: Settings) => call<Settings>("settings_save", { settings }),
  startupWarning: () => call<AppError | null>("startup_warning"),
  operationCancel: (id: OperationId) => call<boolean>("operation_cancel", { id }),
  operationCount: () => call<number>("operation_count"),
  helperProbe: () => call<HelperProbe>("helper_probe"),
  filesystems: () => call<Filesystem[]>("filesystems"),
  homeDirectory: () => call<string>("home_directory"),
  snapshots: () => call<CowSnapshot[]>("snapshots"),
  reclaimPreview: () => call<Preview>("reclaim_preview"),
  reclaimExecute: (ticket: Ticket, selection: number[]) =>
    call<Report>("reclaim_execute", { ticket, selection }),
  reclaimClear: () => call<void>("reclaim_clear"),
  protectedPaths: () => call<Refusal[]>("protected_paths"),
  scanCached: (path: string, maxDepth?: number) =>
    call<CachedScan | null>("scan_cached", { path, maxDepth: maxDepth ?? null }),
  largestFiles: (path: string, limit?: number) =>
    call<SpaceEntry[]>("largest_files", { path, limit: limit ?? null }),
  duplicatesFind: (path: string, minimumBytes?: number) =>
    call<OperationId>("duplicates_find", { path, minimumBytes: minimumBytes ?? null }),
  unitsList: (scope: Scope) => call<Unit[]>("units_list", { scope }),
  unitFiles: (scope: Scope) => call<UnitFile[]>("unit_files", { scope }),
  unitsTimers: (scope: Scope) => call<Timer[]>("units_timers", { scope }),
  unitAct: (scope: Scope, unit: string, action: Action) =>
    call<void>("unit_act", { scope, unit, action }),
  unitsWatch: () => call<void>("units_watch"),
  packagesList: () => call<Package[]>("packages_list"),
  packageMeasure: (manager: Manager, id: string, version: string) =>
    call<Measured>("package_measure", { manager, id, version }),
  packagesResidual: () => call<ResidualConfig[]>("packages_residual"),
  unitLogs: (scope: Scope, unit: string, limit?: number, after?: string) =>
    call<Page>("unit_logs", { scope, unit, limit: limit ?? null, after: after ?? null }),
  processesList: () => call<Process[]>("processes_list"),
  processesForget: () => call<void>("processes_forget"),
  processDetail: (pid: number) => call<Detail>("process_detail", { pid }),
  processTree: () => call<TreeNode[]>("process_tree"),
  processSignal: (pid: number, signal: Signal, processState: ProcessState) =>
    call<void>("process_signal", { pid, signal, processState }),
  processRenice: (pid: number, niceness: number) =>
    call<void>("process_renice", { pid, niceness }),
  alertsEvaluate: () => call<Metric[]>("alerts_evaluate"),
  reclaimLastTotal: () => call<[number, number] | null>("reclaim_last_total"),
  metricsSubscribe: () => call<Reading[]>("metrics_subscribe"),
  metricsUnsubscribe: () => call<void>("metrics_unsubscribe"),
  metricsHistory: () => call<Reading[]>("metrics_history"),
  metricsSampling: () => call<boolean>("metrics_sampling"),
  historySamples: () => call<Sample[]>("history_samples"),
  historySeries: (intervalSeconds?: number) =>
    call<Series>("history_series", { intervalSeconds: intervalSeconds ?? null }),
  historyGrowth: (sinceSeconds: number, limit?: number) =>
    call<GrowthReport>("history_growth", { sinceSeconds, limit: limit ?? null }),
  historyClear: () => call<void>("history_clear"),
  historySnapshotNow: (path: string) => call<Sample>("history_snapshot_now", { path }),
  timerState: () => call<TimerState>("timer_state"),
  timerInstall: () => call<TimerState>("timer_install"),
  timerUninstall: () => call<TimerState>("timer_uninstall"),
  scanCacheClear: () => call<void>("scan_cache_clear"),
  scanCacheSize: () => call<number>("scan_cache_size"),
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

/** Subscribe to unit changes, including ones made in a terminal. `SVC-3`. */
export function onUnitChanged(handler: (unit: string) => void): Promise<UnlistenFn> {
  return listen<string>(EVENT_UNIT_CHANGED, (event) => handler(event.payload));
}

/** Subscribe to live metrics readings. Only fires while something has subscribed. */
export function onMetricsTick(handler: (r: Reading) => void): Promise<UnlistenFn> {
  return listen<Reading>(EVENT_METRICS_TICK, (event) => handler(event.payload));
}

/** Subscribe to finished duplicate searches. */
export function onDuplicatesDone(handler: (r: DuplicateReport) => void): Promise<UnlistenFn> {
  return listen<DuplicateReport>(EVENT_DUPLICATES_DONE, (event) => handler(event.payload));
}

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
