# nix

**Linux storage insight and system utility.** Tells you where your disk went, reclaims it safely,
and shows you what your machine is doing while it does.

Rust + Tauri. A replacement for [Stacer](https://github.com/oguzhaninan/Stacer) — not a port of it.

> Status: **pre-alpha. Phase 1 complete** (Phase 0, M2, M3, M4). The explorer works, and five
> categories reclaim through the full preview pipeline. Next is Phase 2.
> See [docs/PLAN.md](docs/PLAN.md).

**What works today:** scan a filesystem or your home directory and see where the space went — a
canvas treemap and a drill-down table over one shared scan, streaming and cancellable, at roughly
450,000 files per second. It opens on the previous scan labelled with its age.

Reclaiming works for trash, application caches, rotated logs, the systemd journal and package
manager caches, through the full pipeline: **preview → confirm → execute → report**.
`execute` requires a ticket only `preview` can mint, so there is no path from the UI to a deletion
that skips the review step. Protected paths are re-checked at execution time, every path is
re-stat'd immediately before it is touched, and the report compares what nix counted against what
the filesystem actually gave back.

Underneath it: a typed error surface where no failure is silent, a privileged helper behind an
exact-path allow-list, versioned settings, capability probing, and one cancellation-and-progress
primitive every long operation reuses. All of it is exercisable from the **About** view.

## Documentation

| Doc | Contents |
| --- | --- |
| [docs/SPEC.md](docs/SPEC.md) | Product requirements: 57 features across 7 phases, the space model, resolved decisions |
| [docs/PLAN.md](docs/PLAN.md) | Development plan: milestones, task breakdown, risks, definition of done |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Crate layout, the helper design, and the rules that keep them honest |
| [packaging/README.md](packaging/README.md) | What each package format installs, and what it cannot do |
| [docs/stacer/](docs/stacer/README.md) | Reverse-engineered analysis of Stacer — the source of our requirements *and* our anti-requirements |

## Quick start

Requires Rust (pinned by `rust-toolchain.toml`), Node 22+, pnpm, and the Tauri Linux system
dependencies (`libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `librsvg2-dev`,
`libayatana-appindicator3-dev`).

```sh
pnpm install
make hooks     # point git at .githooks
make dev       # run the app
make check     # everything CI checks: fmt, clippy, test, typecheck
make help      # list all targets
```

## Layout

```
nix/
├── src/                    React frontend
├── src-tauri/              Cargo workspace root + the nix-app package
│   ├── src/                nix-app — Tauri shell, commands, events
│   └── crates/
│       ├── nix-core/       model, scanners, samplers. No GUI, no Tauri.
│       └── nix-helper/     privileged helper. Typed, allow-listed operations.
├── docs/                   specification, plan, and the Stacer analysis
└── .github/workflows/      CI
```

## Licence

**GPL-3.0-or-later.** See [LICENSE](LICENSE).

nix is a privileged tool that deletes files, so users have an unusually strong interest in being able
to audit and rebuild exactly what they run. Copyleft keeps that true of forks as well — which is the
same commitment as the rest of the project: no silent failures, no flattering numbers.

Every source file carries an `SPDX-License-Identifier` header. Generated files under
`src/bindings/` do not, because they are rewritten on each build; they are covered by this LICENSE
like everything else in the repository.

nix shares **no code** with Stacer, so this was a free choice rather than an inherited obligation —
Stacer is also GPL-3.0, which is a coincidence of category convention. The one place Stacer-derived
material appears is [docs/stacer/](docs/stacer/README.md), which quotes short excerpts for analysis.

All 551 dependencies (500 Rust crates, 51 npm packages) are permissive or MPL-2.0, and none are
GPL — so nothing in the tree constrained the choice. WebKitGTK, which Tauri links dynamically, is
`LGPL-2+`, and LGPL-2-or-later upgrades to LGPL-3.0, which GPL-3.0 permits.
