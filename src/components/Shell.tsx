// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * The application shell: sidebar, header, and a lazily-mounted content area. Task 0.4 (FND-1).
 *
 * Views are `React.lazy`, and nothing a view does starts until it mounts — principle P9. Stacer
 * built all twelve of its pages eagerly at startup, which is the only reason it needed a splash
 * screen; nix has neither.
 */
import { Suspense, lazy, useEffect, useState } from "react";

import { api, toAppError } from "../lib/ipc";
import { markAllRead, notify, useUnreadCount } from "../lib/notices";
import { NoticePanel } from "./NoticePanel";

const Overview = lazy(() => import("../views/Overview"));
const Explorer = lazy(() => import("../views/Explorer"));
const Reclaim = lazy(() => import("../views/Reclaim"));
const Find = lazy(() => import("../views/Find"));
const Trends = lazy(() => import("../views/Trends"));
const SettingsView = lazy(() => import("../views/SettingsView"));
const About = lazy(() => import("../views/About"));

/** A view's stable identifier. Never a display name — that is the Stacer bug. */
export type ViewId = "overview" | "explorer" | "find" | "trends" | "reclaim" | "settings" | "about";

type ViewDef = { id: ViewId; title: string; hint: string };

const VIEWS: ViewDef[] = [
  { id: "overview", title: "Overview", hint: "Storage and system at a glance" },
  { id: "explorer", title: "Space explorer", hint: "Where your disk went" },
  { id: "find", title: "Find", hint: "Largest files and duplicates" },
  { id: "trends", title: "Trends", hint: "What grew, and when" },
  { id: "reclaim", title: "Reclaim", hint: "Free space safely" },
  { id: "settings", title: "Settings", hint: "Preferences" },
  { id: "about", title: "About", hint: "Versions and diagnostics" },
];

function ViewBody({ id }: { id: ViewId }) {
  switch (id) {
    case "overview":
      return <Overview />;
    case "explorer":
      return <Explorer />;
    case "find":
      return <Find />;
    case "trends":
      return <Trends />;
    case "reclaim":
      return <Reclaim />;
    case "settings":
      return <SettingsView />;
    case "about":
      return <About />;
  }
}

export function Shell({ initialView }: { initialView: ViewId }) {
  const [view, setView] = useState<ViewId>(initialView);
  const [panelOpen, setPanelOpen] = useState(false);
  const unread = useUnreadCount();
  const active = VIEWS.find((v) => v.id === view) ?? VIEWS[0];

  // Collect any warning raised before the frontend existed, so a bad settings file is visible
  // rather than silently swallowed at startup.
  useEffect(() => {
    api
      .startupWarning()
      .then((warning) => {
        if (warning) notify.error(warning);
      })
      .catch((thrown) => notify.error(toAppError(thrown)));
  }, []);

  return (
    <div className="shell">
      <nav className="sidebar" aria-label="Views">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true">
            ◪
          </span>
          <span className="brand-name">nix</span>
        </div>
        <ul>
          {VIEWS.map((v) => (
            <li key={v.id}>
              <button
                type="button"
                className={v.id === view ? "nav-item is-active" : "nav-item"}
                aria-current={v.id === view ? "page" : undefined}
                onClick={() => setView(v.id)}
              >
                <span className="nav-title">{v.title}</span>
                <span className="nav-hint">{v.hint}</span>
              </button>
            </li>
          ))}
        </ul>
      </nav>

      <div className="main">
        <header className="header">
          <div>
            <h1>{active.title}</h1>
            <p className="header-hint">{active.hint}</p>
          </div>
          <button
            type="button"
            className="icon-button"
            onClick={() => {
              const opening = !panelOpen;
              setPanelOpen(opening);
              // Defer, so the badge does not vanish before the panel paints.
              if (opening) setTimeout(markAllRead, 400);
            }}
            aria-label={unread > 0 ? `Notifications, ${unread} unread` : "Notifications"}
          >
            <span aria-hidden="true">◎</span>
            {unread > 0 && <span className="badge">{unread}</span>}
          </button>
        </header>

        <div className="content">
          <main className="view">
            <Suspense fallback={<p className="empty">Loading…</p>}>
              <ViewBody id={view} />
            </Suspense>
          </main>
          {panelOpen && <NoticePanel onClose={() => setPanelOpen(false)} />}
        </div>
      </div>
    </div>
  );
}
