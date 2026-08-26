# nix

**Linux storage insight and system utility.** Tells you where your disk went, reclaims it safely,
and shows you what your machine is doing while it does.

Rust + Tauri. A replacement for [Stacer](https://github.com/oguzhaninan/Stacer) — not a port of it.

> Status: **pre-alpha, Phase 0.** No user-facing features yet. See [docs/PLAN.md](docs/PLAN.md).

## Documentation

| Doc | Contents |
| --- | --- |
| [docs/SPEC.md](docs/SPEC.md) | Product requirements: 57 features across 7 phases, the space model, resolved decisions |
| [docs/PLAN.md](docs/PLAN.md) | Development plan: milestones, task breakdown, risks, definition of done |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Crate layout and the rules that keep it honest |
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
