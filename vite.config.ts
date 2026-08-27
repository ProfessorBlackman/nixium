// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
// @ts-expect-error type error without @types/node package
import process from "node:process";
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [react()],

  // Pre-bundle the Tauri API up front.
  //
  // Every view is lazily imported, so these are only reached after the first paint. Vite therefore
  // discovered them mid-session, re-ran its optimiser and reloaded the page — the
  // "optimized dependencies changed. reloading" line on a first `tauri dev`. Naming them here makes
  // the pre-bundle deterministic instead of dependent on which view a developer happens to open first.
  optimizeDeps: {
    include: ["@tauri-apps/api/core", "@tauri-apps/api/event"],
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
