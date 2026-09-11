<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Artwork

`nix-mascot.png` is the mascot as drawn: 1254×1254, opaque, a cream circle on a black square.

The application icons in `src-tauri/icons/` are generated from it, and this file is here so they can be
generated again. Losing it would mean the icon set could only ever be resized, never remade.

## `portraits/`

The gallery renders, as delivered, one per plate of `site/gallery.html` and named for the plate it
hangs on. Same reason as the mascot: the page serves 900px WebP cuts of about 100 kB, and those can
be made again from these but never the other way round.

**They live here rather than in `site/` because `site/` is what GitHub Pages publishes.** The set is
29 MB as PNG against 1.4 MB as WebP. `site/PORTRAITS.md` has the conversion command and the rest of
the rules.

## Regenerating the icons

```sh
make icons
```

Two steps, and the first is the one worth understanding.

**The black square is made transparent.** An icon is composited onto whatever the desktop's panel or
launcher happens to be, so an opaque square reads as a black box on a light theme — the artwork's
framing becomes a defect once it is 32 pixels wide. The circle is kept and everything outside it is
dropped.

It is done with a **flood fill from a corner**, not a colour threshold. The sunglasses are
`rgb(18,13,10)` and the background is `rgb(16,15,14)`: near-identical, so any threshold dark enough to
remove the background removes the sunglasses too. A flood fill only takes what is *connected to the
edge*, which is exactly the distinction that matters. Checked after masking — the corners are
transparent and the sunglasses are still there.

**Then `tauri icon` produces the set.** It also offers Android and iOS icons, which are deleted: nix is
a Linux application, and carrying icon sets for platforms it does not target would imply otherwise.

**And the sidebar logo falls out of the same source.** `public/nix-logo.png` is the masked artwork at
128px, which the shell draws at 28 — enough for a 2x display, 30 kB on disk. It is cut from the
*masked* intermediate rather than filled again from the original, so the mark in the sidebar and the
icon in the launcher cannot drift apart: one flood fill, two outputs.

Nothing in the app knows what the artwork is except its size and its shape. The sidebar's `img` is
`aria-hidden` with an empty `alt`, because the product's name is rendered right next to it — a screen
reader announcing "nix logo, nix" would read one thing twice.

## `icon.icns` is not reproducible, and no check may assume it is

Running `make icons` twice produces a byte-identical set **except** `icon.icns`, which comes out
differently every time — same length, 2.19 million bytes changed. The macOS encoder writes its tiles in
an order that is not stable, and nothing here can make it.

Worth writing down because the temptation is obvious: a CI step that regenerates the icons and fails if
they differ from what is committed. That step would fail on every run, and it is the same trap
`THIRD-PARTY-NOTICES.md` fell into — a generated artefact compared against a committed one, where the
generator's output depends on something other than its input.

`icon.icns` and `icon.ico` are also for platforms nix does not target. They stay because
`tauri.conf.json` lists them and the bundler expects the entries, not because they are used.
