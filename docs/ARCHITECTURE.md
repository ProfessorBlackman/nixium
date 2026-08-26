# Architecture

Living document. Records structural decisions and the rules that keep them from eroding.
Feature-level detail belongs in [SPEC.md](SPEC.md); sequencing belongs in [PLAN.md](PLAN.md).

## Crate layout

```
src-tauri/                    Cargo workspace root  +  the `nix-app` package
├── Cargo.toml                [workspace] and [package] in one file
├── tauri.conf.json
├── src/                      nix-app     — Tauri shell, command surface, event plumbing
└── crates/
    ├── nix-core/             nix-core    — model, scanners, samplers. No GUI, no Tauri.
    └── nix-helper/           nix-helper  — privileged helper binary
```

### Why the app package sits at the workspace root

The Tauri CLI expects `Cargo.toml` and `tauri.conf.json` in the same directory. Moving the app
crate under `crates/` would mean fighting that for no benefit, so `src-tauri/Cargo.toml` is both the
workspace manifest and the app package manifest. The asymmetry — one crate's sources at
`src-tauri/src`, the others under `src-tauri/crates/` — is deliberate and is the only concession
made to tooling.

### Dependency direction

```
nix-app  ──►  nix-core  ◄──  nix-helper
```

One-way, and enforced by review:

- **`nix-core` depends on neither of the others.** No Tauri, no GUI, no window. Everything it does
  is exercisable from `cargo test` and from a plain binary. This mirrors the one clearly good
  architectural decision in Stacer — its `stacer-core` library kept all system access behind a
  GUI-free boundary — and it is why the port had a specification to work from at all.
- **`nix-app` holds no system access.** Anything in it that starts reading `/proc` or spawning a
  process belongs in `nix-core`. Stacer violated its own version of this rule in nine of its twelve
  pages; the result was that page code and system code could not be tested or reasoned about
  separately.
- **`nix-helper` is a separate binary, not a module.** It runs as root under a different lifetime
  and a different threat model, so it does not share an address space with the app.

### Naming

The package is `nix-app` but the binary is `nix`, because the crate name `nix` belongs to the
well-known Unix-syscall crate we expect to depend on. Users type `nix`; Cargo sees `nix-app`.

## Quality gates

Encoded in the toolchain rather than in a checklist, so they cannot be forgotten:

| Gate | Where | Enforces |
| --- | --- | --- |
| `rust-toolchain.toml` | repo root | Local and CI cannot disagree about lint behaviour |
| `[workspace.lints]` | `src-tauri/Cargo.toml` | `unsafe_code` denied; `unwrap_used`, `dbg_macro`, `todo` warned |
| `cargo clippy -D warnings` | CI + `make clippy` | Those warnings block a merge |
| `cargo fmt --check` | pre-commit + CI | Formatting never enters review |
| `.githooks/pre-commit` | `make hooks` | Fast checks only — fmt, and a guard against committing `target/` |

`unwrap_used` is the machine-checkable half of the plan's definition of done ("no new `unwrap()` in
an operation path"). It warns locally so it does not interrupt exploration, and blocks in CI.

`unsafe_code = "deny"` is a workspace default. If a crate genuinely needs it, it gets an explicit,
reviewed, per-crate allow — not a silent relaxation of the default.

## Planned module layout in `nix-core`

Recorded so tasks land in predictable places rather than accreting into one module.

| Module | Task | Contents |
| --- | --- | --- |
| `error` | 0.2 | `AppError` taxonomy, cause chain, remedy field |
| `caps` | 0.7 | capability probe registry |
| `space` | 1.1 | the space model and its five invariants |
| `fs` | 1.2 | mount enumeration, per-filesystem accounting including btrfs |
| `scan` | 1.3 | streaming, cancellable walker |
| `cache` | 1.4 | scan persistence |
| `reclaim` | 1.8–1.9 | executor pipeline and the category registry |
