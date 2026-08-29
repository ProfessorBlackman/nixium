// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * The first-run introduction. `PLT-4`.
 *
 * # What this is for
 *
 * "Establishing trust before the first destructive action is the point." A user opening a disk cleaner
 * for the first time is being asked to let an unfamiliar program delete things as root, and the honest
 * response to that is to say what it will and will not do **before** they are anywhere near the button.
 *
 * So this leads with the limits, not the features. What nix never touches, what it does before deleting
 * anything, and what a preview means — then the two things worth setting up, and a way out.
 *
 * # Not a tour
 *
 * One screen, dismissible, shown once. A multi-step wizard for a tool with thirteen views would be a
 * wizard people click through without reading, which is worse than none: it converts "I was not told"
 * into "I was told and ignored it", while leaving the user equally uninformed.
 */
import { useState } from "react";

import { api, toAppError } from "../lib/ipc";
import { t } from "../lib/i18n";
import { notify } from "../lib/notices";

export function FirstRun({ onDone }: { onDone: () => void }) {
  const [busy, setBusy] = useState(false);

  /**
   * Record that the introduction has been seen, then leave.
   *
   * The flag is written before dismissing rather than after, so a failure to save means the screen
   * appears again — which is the right way round. Showing it twice is a small annoyance; never showing
   * it because a write silently failed defeats the purpose.
   */
  async function dismiss() {
    setBusy(true);
    try {
      // Read-modify-write: the store takes a whole `Settings`, and this must not clobber anything the
      // user changed in another window.
      const current = await api.settingsGet();
      await api.settingsSave({ ...current, introduced: true });
      onDone();
    } catch (thrown) {
      notify.error(toAppError(thrown));
      setBusy(false);
    }
  }

  return (
    <div className="firstrun" role="dialog" aria-modal="true" aria-labelledby="firstrun-title">
      <div className="firstrun-panel">
        <h1 id="firstrun-title">{t("Before you start")}</h1>
        <p className="muted">
          {t(
            "nix finds where your disk went and reclaims it. It can delete things, so here is what it will and will not do.",
          )}
        </p>

        <h2>{t("What it never touches")}</h2>
        <ul className="firstrun-list">
          <li>
            {t(
              "Your documents, pictures, projects and configuration. nix reclaims caches, logs, build output and superseded packages — never the things you made.",
            )}
          </li>
          <li>
            {t(
              "The kernel you are running, or the newest one installed. Both are refused by the tool and again by the privileged helper that would have to carry it out.",
            )}
          </li>
          <li>
            {t(
              "Anything you add to protected paths, in Settings. Those are refused before a scan even considers them.",
            )}
          </li>
        </ul>

        <h2>{t("Before anything is deleted")}</h2>
        <ul className="firstrun-list">
          <li>
            {t(
              "You see a preview: every item, its size, and what reclaiming it actually costs you. Nothing is selected for you except items that regenerate themselves.",
            )}
          </li>
          <li>
            {t(
              "Sizes say what they mean. Where space is shared with a filesystem snapshot, nix says 'up to' rather than promising a number it cannot deliver.",
            )}
          </li>
          <li>
            {t(
              "Afterwards it checks its own arithmetic against the filesystem and tells you if the two disagree.",
            )}
          </li>
        </ul>

        <h2>{t("Two things worth knowing")}</h2>
        <ul className="firstrun-list">
          <li>
            {t(
              "Administrator rights are asked for once per batch, not once per file — and only when an operation genuinely needs them.",
            )}
          </li>
          <li>
            {t(
              "Moving something to the trash frees nothing until the trash is emptied, because the trash is on the same disk. nix reports those separately rather than counting them as freed.",
            )}
          </li>
        </ul>

        <div className="row">
          <button type="button" className="danger" disabled={busy} onClick={() => void dismiss()}>
            {busy ? t("Saving…") : t("Got it — take me to the overview")}
          </button>
        </div>
        <p className="muted">
          {t("You can read this again in About, and change what is protected in Settings.")}
        </p>
      </div>
    </div>
  );
}
