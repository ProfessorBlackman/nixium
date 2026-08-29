// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Translation. `PLT-1`.
 *
 * # Keyed on the English text, not on an identifier
 *
 * `t("Reclaim safely")` rather than `t("reclaim.button")`. Two reasons, and the second is the one that
 * decided it.
 *
 * The first is ordinary: the source reads as the sentence it renders, so a reviewer sees the copy
 * rather than a key, and a missing translation degrades to correct English instead of to `reclaim.button`.
 *
 * The second is that Stacer's 26 locale files are keyed this way. Qt's `.ts` format identifies a
 * message by its English source, so the only way to inherit those 3,902 translations rather than
 * discard them is to use the same identity. An identifier scheme would have meant retranslating from
 * zero, in 26 languages, out of a preference for tidiness.
 *
 * # Live switching
 *
 * Stacer required a restart to change language. This holds the locale in a store components subscribe
 * to, so switching re-renders. RTL is applied to the document element at the same time, because a
 * language change that leaves the layout mirrored the wrong way is not a language change.
 */
import { useSyncExternalStore } from "react";

/** Locales harvested from Stacer, plus English. Loaded on demand. */
const CATALOGUES = import.meta.glob<Record<string, string>>("../locales/*.json");

/** Right-to-left scripts. Persian and Hebrew are here for when those locales arrive. */
const RTL = new Set(["ar", "fa", "he", "ur"]);

type Catalogue = Record<string, string>;

let current = "en";
let messages: Catalogue = {};
const listeners = new Set<() => void>();

/** Strings asked for that no catalogue answered, for the coverage report. */
const missing = new Set<string>();

function announce() {
  for (const listener of listeners) listener();
}

/**
 * Translate. Returns the English unchanged when there is no translation.
 *
 * That fallback is the whole reason for keying on English: an incomplete catalogue produces a mixed
 * but *correct* interface, never a screen of identifiers.
 */
export function t(english: string): string {
  if (current === "en") return english;
  const found = messages[english];
  if (found === undefined) {
    missing.add(english);
    return english;
  }
  return found;
}

/**
 * Interpolate `{name}` placeholders after translating.
 *
 * Separate from [`t`] so the translatable string stays a whole sentence — splitting a sentence around
 * a value produces fragments that cannot be reordered, and word order is exactly what differs between
 * languages.
 */
export function tf(english: string, values: Record<string, string | number>): string {
  return t(english).replace(/\{(\w+)\}/g, (whole, name: string) =>
    name in values ? String(values[name]) : whole,
  );
}

/** The locale in use. */
export function locale(): string {
  return current;
}

/** Every locale there is a catalogue for, English first. */
export function available(): string[] {
  const codes = Object.keys(CATALOGUES)
    .map((path) => path.replace(/.*\/(.+)\.json$/, "$1"))
    .filter((code) => code !== "en")
    .sort();
  return ["en", ...codes];
}

/**
 * Switch language. No restart, unlike Stacer.
 *
 * Loading is asynchronous because the catalogues are split out of the main bundle — shipping 26
 * languages to everyone to use one of them would be a cost paid by every user on every start.
 */
export async function setLocale(code: string): Promise<void> {
  if (code === current) return;

  if (code === "en") {
    current = "en";
    messages = {};
  } else {
    const loader = CATALOGUES[`../locales/${code}.json`];
    if (loader === undefined) {
      // An unknown locale falls back to English rather than throwing: a bad value in the settings file
      // should not be a blank window.
      current = "en";
      messages = {};
    } else {
      const loaded = await loader();
      // Vite gives either the module namespace or its default, depending on the import mode.
      messages = (loaded as { default?: Catalogue }).default ?? (loaded as Catalogue);
      current = code;
    }
  }

  applyDirection();
  announce();
}

/** Set `dir` and `lang` on the document, so the browser mirrors the layout and reads it correctly. */
function applyDirection() {
  const root = document.documentElement;
  root.lang = current;
  root.dir = RTL.has(current.split("-")[0]) ? "rtl" : "ltr";
}

/** Subscribe a component to language changes. */
export function useLocale(): string {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => current,
    () => "en",
  );
}

/**
 * Strings asked for during this session that no catalogue could answer.
 *
 * Exposed for the coverage report in the About view rather than kept private: the honest thing to show
 * a user considering a language is how much of the interface it actually covers, and the only way to
 * know that is to record what was asked for and missed.
 */
export function untranslated(): string[] {
  return [...missing].sort();
}
