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
import { t, tf, useLocale } from "../lib/i18n";
import { markAllRead, notify, useNotices, useUnreadCount } from "../lib/notices";
import { NoticePanel } from "./NoticePanel";

const Overview = lazy(() => import("../views/Overview"));
const Explorer = lazy(() => import("../views/Explorer"));
const Reclaim = lazy(() => import("../views/Reclaim"));
const Find = lazy(() => import("../views/Find"));
const Trends = lazy(() => import("../views/Trends"));
const Processes = lazy(() => import("../views/Processes"));
const Services = lazy(() => import("../views/Services"));
const SettingsView = lazy(() => import("../views/SettingsView"));
const Software = lazy(() => import("../views/Software"));
const Repositories = lazy(() => import("../views/Repositories"));
const Startup = lazy(() => import("../views/Startup"));
const Hosts = lazy(() => import("../views/Hosts"));
const About = lazy(() => import("../views/About"));

/** A view's stable identifier. Never a display name — that is the Stacer bug. */
export type ViewId =
  | "overview"
  | "explorer"
  | "find"
  | "trends"
  | "processes"
  | "services"
  | "software"
  | "repositories"
  | "startup"
  | "hosts"
  | "reclaim"
  | "settings"
  | "about";

type ViewDef = { id: ViewId; title: string; hint: string };

/*
 * The titles and hints below are the English source text, and stay English here.
 *
 * `t()` is applied where they are **rendered**, not where they are declared: this array is evaluated
 * once when the module loads, so translating it here would freeze whichever language was active at
 * that moment and never update on a switch. That is the shape of bug that makes people believe live
 * language switching "mostly works".
 */

const VIEWS: ViewDef[] = [
  { id: "overview", title: "Overview", hint: "Storage and system at a glance" },
  { id: "explorer", title: "Space explorer", hint: "Where your disk went" },
  { id: "find", title: "Find", hint: "Largest files, duplicates and search" },
  { id: "trends", title: "Trends", hint: "What grew, and when" },
  { id: "processes", title: "Processes", hint: "What is running, and what it costs" },
  { id: "services", title: "Services", hint: "systemd units, timers and their logs" },
  { id: "software", title: "Software", hint: "What is installed, and what it really costs" },
  { id: "repositories", title: "Sources", hint: "Where software comes from" },
  { id: "startup", title: "Startup", hint: "What runs when you log in" },
  { id: "hosts", title: "Hosts", hint: "Names this machine resolves itself" },
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
    case "processes":
      return <Processes />;
    case "services":
      return <Services />;
    case "software":
      return <Software />;
    case "repositories":
      return <Repositories />;
    case "startup":
      return <Startup />;
    case "hosts":
      return <Hosts />;
    case "reclaim":
      return <Reclaim />;
    case "settings":
      return <SettingsView />;
    case "about":
      return <About />;
  }
}

/**
 * A polite live region carrying the most recent notification.
 *
 * Visually hidden, because the notice is already on screen for anyone who can see it; this exists so
 * it also reaches anyone who cannot. Keyed on the notice id so an identical message arriving twice is
 * announced twice — re-rendering the same text into a live region does not.
 */
function Announcer() {
  const notices = useNotices();
  const latest = notices[0];

  return (
    <div className="visually-hidden" role="status" aria-live="polite" aria-atomic="true">
      {latest === undefined ? "" : `${latest.level}: ${latest.title}`}
    </div>
  );
}

export function Shell({ initialView }: { initialView: ViewId }) {
  // Subscribing to the locale is what makes switching live: without it the shell keeps whatever text
  // it rendered first, and only views that happened to re-render would change language.
  useLocale();
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
      <nav className="sidebar" aria-label={t("Views")}>
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
                <span className="nav-title">{t(v.title)}</span>
                <span className="nav-hint">{t(v.hint)}</span>
              </button>
            </li>
          ))}
        </ul>
      </nav>

      <div className="main">
        <header className="header">
          <div>
            <h1>{t(active.title)}</h1>
            <p className="header-hint">{t(active.hint)}</p>
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
            aria-label={
              unread > 0 ? tf("Notifications, {n} unread", { n: unread }) : t("Notifications")
            }
          >
            <span aria-hidden="true">◎</span>
            {unread > 0 && <span className="badge">{unread}</span>}
          </button>
        </header>

        {/* Notifications are announced here, not only listed in the panel.
            A screen-reader user who never opens the panel would otherwise get no indication that
            "Freed 4.2 GB" or "Some items could not be reclaimed" had happened at all — and those are
            the outcomes of the destructive actions, which is the worst possible thing to be silent
            about. Polite rather than assertive: it waits for a pause rather than interrupting. */}
        <Announcer />

        <div className="content">
          <main className="view">
            <Suspense fallback={<p className="empty">{t("Loading…")}</p>}>
              <ViewBody id={view} />
            </Suspense>
          </main>
          {panelOpen && <NoticePanel onClose={() => setPanelOpen(false)} />}
        </div>
      </div>
    </div>
  );
}
