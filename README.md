# nix

**Linux storage insight and system utility.** Tells you where your disk went, reclaims it safely,
and shows you what your machine is doing while it does.

Rust + Tauri. A replacement for [Stacer](https://github.com/oguzhaninan/Stacer) — not a port of it.

> Status: **pre-alpha. Phase 0 and M2 complete.** The space explorer works and is read-only.
> Next is M3, the first safe reclaim. See [docs/PLAN.md](docs/PLAN.md).

**What works today:** scan a filesystem or your home directory and see where the space went — a
canvas treemap and a drill-down table over one shared scan, streaming and cancellable, at roughly
450,000 files per second. It opens on the previous scan labelled with its age, and it deletes
nothing: reclaiming arrives in M3 behind a preview and a confirmation.

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

**Not yet chosen.** nix shares no code with Stacer (which is GPL-3.0), so we are unconstrained —
but a licence needs picking before the first public release.
