# Architecture

Living document. Records structural decisions and the rules that keep them from eroding.
Feature-level detail belongs in [SPEC.md](SPEC.md); sequencing belongs in [PLAN.md](PLAN.md); the
defects that produced several of the rules below are logged in
[issues/](issues/README.md), which also lists what is known to be unverified.

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

## Module layout in `nix-core`

Built in Phase 0:

| Module | Task | Contents |
| --- | --- | --- |
| `error` | 0.2 | `AppError` taxonomy, cause chain, remedy, context breadcrumbs |
| `op` | 0.3 | `CancelToken`, `Progress`, `Completion`, operation registry |
| `settings` | 0.6 | versioned, atomically written preferences |
| `caps` | 0.7 | capability probing — never distro detection |
| `logging` | 0.8 | structured logging and the diagnostics bundle |
| `helper` | 0.9 | privileged helper: protocol, client, allow-list, audit |
| `fixture` | 0.11 | reproducible filesystem fixtures |
| `budget` | 0.11 | performance budgets from the specification |
| `paths` | — | XDG base-directory resolution |

Built in M2:

| Module | Task | Contents |
| --- | --- | --- |
| `space` | 1.1 | the space model, its invariants, and the checker that enforces them |
| `fs` | 1.2 | mount enumeration, per-filesystem accounting, btrfs honesty |
| `scan` | 1.3 | streaming, cancellable, parallel walker |
| `cache` | 1.4 | scan persistence, so the explorer opens on the last result |
| `watch` | 1.15 | inotify staleness watching over the largest directories |

Built in M3:

| Module | Task | Contents |
| --- | --- | --- |
| `protect` | 1.7 | paths nix must never reclaim, checked by scanner and executor |
| `trash` | 1.10 | the freedesktop trash specification |
| `reclaim` | 1.8–1.9 | the preview pipeline and the category registry |

Built in M4:

| Module | Task | Contents |
| --- | --- | --- |
| `reclaim::caches` | 1.11 | application caches, attributed to the apps that own them |
| `reclaim::logs` | 1.12 | rotated logs and the systemd journal |
| `reclaim::packages` | 1.13 | package manager caches, cleaned through the owning tool |
| `pkg` | STO-10 | package database queries; parsers tested against real captured output |
| `reclaim::kernels` | STO-11 | superseded kernels and residual configuration |
| `cow` | STO-17 | copy-on-write sharing, so an estimate is never overstated |
| `pkg::snap` | STO-12 | snap revisions, with hard-link-aware sizing |
| `pkg::flatpak` | STO-12 | flatpak refs, unused runtimes, ostree repository |
| `tests/reclaim_accuracy` | 1.14 | the specification's 2% criterion, verified end to end |

## How "nothing bypasses preview" is enforced

Not by convention, and not by a code review rule that erodes. `reclaim::execute` requires a
`Ticket`, and the only thing that mints one is `reclaim::preview`. The ticket is tied to the exact
item set the preview described, and a session holds one preview at a time — so a caller cannot
construct a ticket, cannot replay a superseded one, and cannot widen the selection after the user
agreed to it. Selecting an id that was not in the preview is an error, not a silent skip, because it
means acting on something nobody was shown.

Two guards then run again **at execution time**, per item:

- **Protection** is re-checked, because the user's exclusions may have changed since the preview.
- **Time-of-check/time-of-use**: every path is re-stat'd and compared against a fingerprint recorded
  at preview time. A file that changed size or became a different inode is skipped and reported.

That second guard is also what makes decision D6 safe: the explorer may serve a cached tree because
stale data can misinform a reader but cannot misdirect a deletion.

## The privileged surface, and how it is kept small

The helper's operation enum is the security boundary, and M4 grew it from two operations to seven.
The rule that makes that growth safe:

> **An operation carries its category, and the helper independently re-derives which roots that
> category owns.**

`ReclaimFile { kind: RotatedLog, path: "/etc/shadow" }` is refused, because `/etc` is not a root of
any category. The unprivileged side cannot widen its own access by mislabelling a path — which is
specification invariant 4 ("`Unlink` is only emitted for a path inside its category's declared
root") enforced where it matters rather than trusted from the caller.

Three further properties fall out of the same design:

- **An active log cannot be deleted at all.** The helper checks the *filename shape*, so
  `/var/log/syslog` is refused while `/var/log/syslog.1.gz` is not. Deleting a file a running
  service holds open frees nothing until it restarts and breaks its logging meanwhile. The policy
  check runs before any filesystem access, so the refusal does not depend on the file existing —
  an earlier version returned `NotFound` on journald-only systems, making the guard's behaviour vary
  by distribution.
- **No caller-supplied text reaches a root command line.** `ReclaimMethod::PackageManager` carries
  a `Manager` enum, not a command string; `JournalVacuum` carries a number, not a limit string. The
  argument vectors are fixed inside the helper.
- **Destructive operations are audited distinctly** from reads. An audit trail whose deletions look
  like its reads is not much of an audit trail.

One privileged session is opened per *batch*, not per item — Stacer re-ran every command under
`pkexec`, so toggling five services meant five dialogs.

## Copy-on-write filesystems: a qualified estimate, never a bare number

On btrfs, ZFS, or an LVM thin volume a file's extents may be shared with a snapshot. Deleting the
file removes the name, but the blocks stay allocated until every reference is gone — so the space
does not come back. A user who deletes 8 GiB and sees `df` move by nothing has been lied to by
whatever told them 8 GiB was reclaimable.

`space::Reclaimable` therefore qualifies every estimate:

| Variant | Meaning | Contributes to a headline total |
| --- | --- | --- |
| `Exact` | Freeing this returns the stated size | the whole size |
| `AtMost { exclusive: Some(n) }` | Partly shared; `n` is proven exclusive | `n` only |
| `AtMost { exclusive: None }` | Shared, and the exclusive part is unprovable | **nothing** |
| `Unknown` | A CoW filesystem nix could not ask about | **nothing** |

A `Preview` carries both `total_bytes` (every stated size at face value) and `promisable_bytes`
(what nix will actually promise). The UI leads with the second and states the difference. That is
`STO-17`'s rule made concrete: *where exclusive size is unobtainable, suppress the estimate rather
than fake it.*

The pessimistic default is the important part. A CoW filesystem whose tool is absent yields
`Unknown`, not an assumption of exclusivity — assuming exclusivity is exactly how a tool ends up
promising space that never arrives.

**Snapshots are attributed, never deleted.** They are inventoried so their space lands in the
`Snapshot` category rather than in `Unknown`, but nix does not offer to remove one: a snapper or
Timeshift snapshot may be somebody's only route back from a bad upgrade.

### An honest limitation

The free-space half of `STO-17` landed in `STO-1` and is verified against a real filesystem. The
`cow` module is not: the development machine is ext4 with no btrfs, ZFS or LVM tooling, so those
parsers are written to each tool's documented output format and tested against fixtures built from
that documentation — not against captured output. The package parsers found two real bugs precisely
*because* they ran against a live database, and these have had no equivalent exposure. **They need
re-verifying on a real btrfs and ZFS system before their numbers are trusted.** What is fully tested
is the suppression logic, because that is pure and runs everywhere.

`pkg::flatpak`'s runtime listing is in the same position for the same reason: this machine has a
flatpak installation but nothing installed in it, so the unused-runtime derivation and the
extension-matching rule are tested against fixtures rather than captured output. The repository
measurement *is* exercised against real data, because 699 MiB of orphaned objects are sitting there.

`pkg::snap` needs no such caveat — every parser and the hard-link accounting run against live snapd
output on each test run.

### Sharing is not a copy-on-write problem

`space::Reclaimable` was built for btrfs and ZFS snapshots, on the assumption that shared extents
were a filesystem-specific concern. They are not. The same problem turned up twice more on plain
ext4:

| Where | What shares the blocks | What that means |
| --- | --- | --- |
| btrfs, ZFS, LVM | snapshots holding old extents | freeing a file may free nothing |
| snapd | every blob hard-linked into `/var/lib/snapd/cache` | removing a revision frees nothing until snapd prunes |
| flatpak | deployments hard-linked out of the ostree repository | uninstalling frees nothing until the repository is pruned |

One type expresses all three, which is a reasonable sign it models something real. Where the sharing
can be *removed* it is removed rather than reported: the helper unlinks snapd's cache entry along with
the blob, matched by inode, so a snap revision's figure is exact instead of qualified.

### Paths and logical entries

Not everything reclaimable is a file. A kernel, a snap revision, a package manager's cache — these are
logical objects, and the `Candidate` for one carries a descriptive path like
`kernel 6.8.0-136-generic` that was never meant to exist on disk.

Two guards assume otherwise: the path protection rules, and the time-of-check fingerprint. Applied to
a logical entry the first refuses it ("only absolute paths can be checked") and the second concludes
it is already gone. Both fired silently, and between them they made every kernel and snap-revision
candidate inert — measured correctly, then discarded before the user saw it.

`ReclaimMethod::acts_on_path` is the distinction those guards were missing. Path rules and
fingerprints apply where the path *is* the target. A logical entry is guarded instead by the
privileged helper re-deriving its own eligible set at the moment it acts, which is stronger than a
fingerprint: a kernel that stopped qualifying between preview and execution is refused by the process
that would carry out the removal.

### Advisories: measured, not automated

`space::Advisory` covers space nix can account for but will not act on itself. There are two failure
modes to avoid and they pull in opposite directions — hiding real space is the failure this project
exists to correct, and automating an untested destructive operation is worse. An advisory is the third
answer: report the bytes, name the remedy as a command the user can read, and say plainly why nix is
not running it.

Advisories are deliberately excluded from `Preview::total_bytes` and `promisable_bytes`, because those
figures are promises about what the preview would reclaim.

The case that forced the type: 699 MiB of unreferenced objects in this machine's flatpak ostree
repository, and no `ostree` binary installed to prune them.

## The report checks its own arithmetic

`Report` carries both what nix counted and the filesystem's own before/after delta, and states
whether they agree within the specification's 2%. Where they disagree the UI says so plainly rather
than reporting the flattering number — copy-on-write filesystems and snapshots can hold onto space
that looks freed, and a tool that hides that is lying about the one thing it exists to do.

## Two rules the scanner exists to enforce

**Both sizes are carried, never one.** Apparent size and on-disk allocation diverge on sparse,
compressed and copy-on-write filesystems. Picking one and calling it "the size" is how a tool ends up
promising space it cannot free, so `SpaceEntry` holds both and the reclaim figure is always the
allocated one.

**Partial coverage is visible.** A scan that was cancelled, or that could not read everything, still
returns its tree — and carries a `coverage_note` field describing the gap. It is a field rather than
a computed method precisely so a total cannot be rendered without the caveat being at hand. Stacer
showed a total with no indication that a scan had skipped anything.

## Parallelism: `par_iter`, never nested `scope`

The scanner fans out with `par_iter().map().reduce()`. An earlier version used a `rayon::scope` per
directory, and the failure is worth recording: a scope **blocks its calling thread** until every
child finishes, so a tree of nested scopes fills the pool with threads waiting on children that need
those same threads. In isolation it measured 35 ms for 2,590 files; with concurrent scans it
collapsed to fifteen seconds, and cancellation could not unwind through the blocked scopes.
`par_iter` is built on `join` and nests correctly.

## The privileged helper

Stacer escalated by re-running individual commands under `pkexec`: one authentication prompt *per
action*, and because it read only stdout and never checked exit status, a cancelled prompt was
indistinguishable from success.

nix spawns **one** helper under `pkexec` and keeps a privileged session, exchanging line-delimited
JSON over the child's stdin and stdout:

```
  nix-app ──spawn── pkexec ──exec── nix-helper --serve      (one authentication)
      │                                    │
      └──── Request  (one JSON per line) ──┤
      ◄──── Response (one JSON per line) ──┘
```

**The security boundary is the `Op` enum, not the transport.** The helper accepts no free-form
command, no argument vector, and no path it has not validated. Properties that hold by construction:

- Malformed input is answered, audited, counted, and after a threshold the process exits — a
  confused or hostile peer cannot spin a root process forever.
- Reads are matched against an **exact-path allow-list**. This started as a list of permitted
  directory roots, and the tests caught why that was wrong: any file under `/etc` includes
  `/etc/shadow`, and anywhere under `/proc` includes other users' `/proc/<pid>/environ`. A prefix
  allow-list on a privileged read is escalation with extra steps.
- Exact matching also removes normalisation as a concern — worth noting because
  `Path::components()` silently drops `.` segments, so a check for them never fires.
- The helper exits on EOF, so it cannot outlive the app that authorised it.
- Every request and outcome is audited *before* the response is written.

### How it is tested

`pkexec` cannot run in CI, so `helper::Transport` abstracts how the child is started. Tests spawn
the helper **directly as the current user**, which exercises serialisation, dispatch, validation,
rejection and auditing without root. The only path CI does not cover is escalation itself, which is
a single `Command` invocation.

## Generated TypeScript

Rust types that cross the IPC boundary derive `ts_rs::TS`, and `.cargo/config.toml` — at the
**repository root**, deliberately — points `TS_RS_EXPORT_DIR` at `src/bindings/`.

The location matters: Cargo discovers config by walking up from the *current directory*, so a config
under `src-tauri/` is silently missed by `cargo --manifest-path src-tauri/…` run from the root. ts-rs
then falls back to its default of `./bindings` and quietly writes a second, stale copy of every type
— which is exactly what happened, and what `src-tauri/crates/*/bindings/` is now gitignored to catch. The files are **committed**, so the frontend type-checks
without first running the Rust suite, and CI regenerates and diffs them — a Rust type change that is
not reflected in the bindings fails the build rather than drifting.

`u64` counters that cross the wire are annotated `#[ts(type = "number")]`: a double holds integers
exactly to 2^53, and `bigint` would make every arithmetic site in the frontend awkward for no gain.

## Frontend structure

| Path | Contents |
| --- | --- |
| `src/lib/ipc.ts` | typed wrappers over every command; normalises any rejection into an `AppError` |
| `src/lib/notices.ts` | notification centre store — an external store, not context, per D8 rule 3 |
| `src/lib/useOperation.ts` | the React binding for progress and cancellation, written once |
| `src/lib/theme.ts` | three-state theme resolution (explicit light, explicit dark, follow desktop) |
| `src/components/Shell.tsx` | sidebar, header, lazily-mounted content |
| `src/views/*` | one file per view, each `React.lazy` |

Views are code-split: `pnpm build` emits a separate chunk per view, so principle P9 — nothing runs
until its view mounts — is verifiable at the bundler level rather than asserted.

## Packaging

See `packaging/README.md`. The short version: deb and rpm install the helper to
`/usr/libexec/nix/nix-helper` with a polkit policy and have a complete privilege story. AppImage and
Flatpak are relocatable, and polkit authorises by absolute executable path, so both are read-only
until the helper ships as a separate host-installed package (M9 / PLT-5). The app degrades honestly
in that case: the capability probe reports `pkexec` unavailable and privileged features say why.
