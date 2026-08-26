/**
 * The notification centre. Task 0.5 (FND-3).
 *
 * Always reachable, so no failure is ever silent. Each entry shows what happened, what to do about
 * it, and — on demand — the underlying cause, which is what makes a bug report useful.
 */
import { useState } from "react";

import { clearNotices, markAllRead, useNotices, type Notice } from "../lib/notices";

const LEVEL_LABEL: Record<Notice["level"], string> = {
  error: "Error",
  warning: "Warning",
  info: "Note",
  success: "Done",
};

function NoticeRow({ notice }: { notice: Notice }) {
  const [open, setOpen] = useState(false);

  return (
    <li className={`notice notice-${notice.level}`}>
      <div className="notice-head">
        <span className="notice-level">{LEVEL_LABEL[notice.level]}</span>
        <time dateTime={notice.at.toISOString()}>
          {notice.at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}
        </time>
      </div>
      <p className="notice-title">{notice.title}</p>
      {notice.remedy && <p className="notice-remedy">{notice.remedy}</p>}
      {notice.detail && (
        <>
          <button type="button" className="link" aria-expanded={open} onClick={() => setOpen(!open)}>
            {open ? "Hide details" : "Show details"}
          </button>
          {open && (
            <pre className="notice-detail">
              {notice.code ? `[${notice.code}] ` : ""}
              {notice.detail}
            </pre>
          )}
        </>
      )}
    </li>
  );
}

export function NoticePanel({ onClose }: { onClose: () => void }) {
  const notices = useNotices();

  return (
    <aside className="panel" aria-label="Notifications">
      <header className="panel-head">
        <h2>Notifications</h2>
        <div className="panel-actions">
          {notices.length > 0 && (
            <>
              <button type="button" className="link" onClick={markAllRead}>
                Mark all read
              </button>
              <button type="button" className="link" onClick={clearNotices}>
                Clear
              </button>
            </>
          )}
          <button type="button" className="icon-button" onClick={onClose} aria-label="Close notifications">
            ✕
          </button>
        </div>
      </header>

      {notices.length === 0 ? (
        <p className="empty">Nothing to report.</p>
      ) : (
        <ul className="notice-list">
          {notices.map((n) => (
            <NoticeRow key={n.id} notice={n} />
          ))}
        </ul>
      )}
    </aside>
  );
}
