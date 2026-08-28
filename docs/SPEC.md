# nix — Product Requirements & Specification

**Status:** draft v0.2 — all eight open decisions resolved (§9) · **Owner:** methuselah.nwodobeh@amalitech.com · **Last updated:** 2026-08-26

Master specification for **nix**, a Linux system utility built with Rust and Tauri. nix is the
replacement for [Stacer](docs/stacer/README.md) — not a port of it. Stacer's behaviour is
documented in `docs/stacer/` and is used here as a source of requirements and of
anti-requirements, never as a design to copy.

---

## 1. Product thesis

**Storage is the centre of the product. Everything else supports it.**

Stacer's defining flaw was that it was twelve unrelated tools sharing a sidebar. Disk concerns
in particular were scattered across four pages that never spoke to each other — a five-category
cleaner, an uninstaller with no size information, a `find` form, and a pie chart of volume
*capacity*. The question a user actually arrives with — *"what is eating my disk, and what can I
safely reclaim?"* — was answered by no page and could not be, because nothing shared a model of
"space attributed to a thing."

nix is built the other way round: a single **space model** (§5) is the product's spine. Every
storage view is a projection of it. Monitoring, process and service management are supporting
features, valuable in their own right but subordinate in priority.

### One-line positioning

> nix tells you where your disk went, reclaims it safely, and shows you what your machine is
> doing while it does.

---

## 2. Non-goals

| Not building | Why |
| --- | --- |
| A Stacer clone | Different UI, different page structure, different workflows are all expected |
| A partition editor | Destructive filesystem geometry is GParted's job; nix never writes partition tables |
| A file manager | We locate and reclaim; we don't browse, rename, or organise |
| A general terminal replacement | Where a CLI is better, we say so and get out of the way |
| A resident daemon or system service | Nothing of ours runs as root or stays resident. An **opt-in systemd user timer** running a bounded periodic job is permitted (§STO-16); no lingering, no system units |
| Cross-platform | Linux only. No Windows/macOS abstraction tax |
| Telemetry of any kind | No analytics, no crash reporting to us, no phone-home |
| Kernel or boot tuning | Out of scope; too much risk for too little payoff |

---

## 3. Engineering principles

These are binding constraints, written to be testable. Every feature below inherits them.

| # | Principle | Test |
| --- | --- | --- |
| P1 | **Errors are values.** Every fallible operation returns a typed `Result` carrying exit status, stderr and context. Every failure reaches the UI. | No `unwrap()` in operation paths; a forced failure of any privileged op produces a visible, specific message |
| P2 | **Preview → confirm → execute → report.** Nothing destructive runs without a computed diff first, and every execution returns per-item results. | Every destructive command has a paired `preview_*` command; no destructive command is callable without a preview token |
| P3 | **One owner of state.** The backend samples; the frontend subscribes. No two consumers share mutable sampler state. | Sampler state is owned by a single task; no `static mut`, no shared delta counters |
| P4 | **No subprocess where an API exists.** `/proc` over `ps`; systemd D-Bus over `systemctl`; a native walker over `find`. | Zero subprocess spawns in the steady-state monitoring loop |
| P5 | **Async, streaming, cancellable.** Anything that may exceed 100 ms streams partial results and honours a cancellation token. | Every scan can be cancelled mid-flight and leaves no partial mutation |
| P6 | **Least privilege.** One typed, allow-listed helper. Prefer APIs that already integrate with polkit (systemd, PackageKit) and write no helper for them. | The helper accepts no free-form argv; no `rm -rf` with a UI-built argument list, ever |
| P7 | **Capability detection, never distro-name detection.** Probe for the schema, the binary, the filesystem type. Degrade the control, not the feature. | No branch anywhere reads a distro or desktop *name* to decide whether a feature exists |
| P8 | **Honest numbers.** Parse into maps, never by line index. Distinguish apparent size from on-disk allocation. Show usage, not capacity. Chart maxima decay. | Golden-file tests for every parser; sizes cross-checked against `du`/`df` within tolerance |
| P9 | **Lazy views.** Nothing samples, scans, or queries until its view is mounted. | Cold start performs no scan; idle CPU with the window closed is ~0 |

---

## 4. Platform support

| Tier | Scope |
| --- | --- |
| **Tier 1** — CI-tested, must work | Ubuntu 22.04+/24.04, Debian 12+, Fedora 39+, Arch (rolling). GNOME and KDE. Wayland and X11. ext4, btrfs, xfs. |
| **Tier 2** — supported, best effort | openSUSE Tumbleweed/Leap, Linux Mint, Pop!_OS. Xfce, Cinnamon. ZFS, LVM. |
| **Tier 3** — degrade gracefully, don't crash | Immutable/ostree distros (Silverblue, Bazzite) — read-only storage insight, no package mutation. Containers, WSL2, headless. |

**Package backends:** apt/dpkg, dnf/rpm, pacman, zypper, snap, flatpak.
Detection is per-capability (§P7): a machine with both apt and flatpak reports both.

**Minimum:** Linux 5.15, glibc 2.35 or musl, polkit for privileged operations.
Absent polkit, nix runs read-only and says so.

---

## 5. The space model

### Trashing is not freeing

An invariant the model has to state, because it was got wrong: **moving a file to the trash frees no
space.** The freedesktop trash must live on the same filesystem as its contents, since the move is a
rename, so free space does not change until the trash is emptied.

A report therefore carries two figures — `freed` for what was removed outright and `trashed` for what
is staged and recoverable — and never adds them together. Reversibility remains the default for a
user's files; what is not permitted is calling it a reclaim. See
[issues §03](issues/03-reclaim-pipeline.md) for the version that did.

The spec's core. Everything in Phase 1 and 2 is a producer or a consumer of this model.

### 5.1 `SpaceEntry`

A scan produces a stream of entries. Each entry attributes bytes to a *thing*, with enough
provenance that the UI can explain itself and the executor can act safely.

```rust
struct SpaceEntry {
    id:            EntryId,          // stable across rescans
    path:          Option<PathBuf>,  // None for logical entries (e.g. a journal budget)
    label:         String,           // human name: "Firefox cache", "linux-image-6.5.0-21"
    apparent_size: u64,              // sum of file sizes
    allocated:     u64,              // on-disk, from stat blocks — differs on sparse/CoW/compressed
    category:      Category,
    provenance:    Provenance,       // how we identified it — shown in the UI
    safety:        Safety,
    reclaim:       ReclaimMethod,
    last_used:     Option<SystemTime>,
    children:      Vec<EntryId>,     // the model is a tree
}
```

### 5.2 Categories

`PackagePayload` · `PackageCache` · `AppCache` · `Log` · `Journal` · `Trash` ·
`Snapshot` · `ContainerImage` · `BuildArtifact` · `Thumbnail` · `CrashDump` ·
`OrphanedConfig` · `UserFile` · `Duplicate` · `Unknown`

`Unknown` is a first-class category, not a bug. Space nix cannot attribute is shown as
unattributed rather than silently dropped, and the sum of all categories plus `Unknown` must
equal filesystem usage (§P8).

### 5.3 Safety ratings

| Rating | Meaning | UI treatment |
| --- | --- | --- |
| `Safe` | Regenerable with no user-visible loss. Package caches, thumbnails, rotated logs. | Selectable in bulk, pre-checked in "quick clean" |
| `Review` | Reclaimable but has a cost — a slower next launch, lost browser session, lost build cache. | Selectable, never pre-checked, cost stated inline |
| `Risky` | May break a running service or lose data. Active logs, container volumes, anything a process holds open. | Requires per-item confirmation; blocked from bulk selection |
| `Never` | Not reclaimable by nix at all. Live system files, mounted volumes, anything under a protected path. | Displayed for attribution only, no selection control |

The rating is computed, not hardcoded per category: an open file handle, a recent access time, or
a protected-path match escalates the rating.

### 5.4 Reclaim methods

Reclamation always prefers the owning tool over `unlink`:

| Method | Used for |
| --- | --- |
| `PackageManager(cmd)` | `apt-get clean`, `dnf clean packages`, `pacman -Sc`, `flatpak uninstall --unused` |
| `JournalVacuum{size,time}` | `journalctl --vacuum-size=` / `--vacuum-time=` |
| `SnapRevision(pkg,rev)` | Drop superseded snap revisions |
| `ContainerPrune(scope)` | `podman`/`docker` image and build-cache prune |
| `TrashEmpty(volume)` | Spec-compliant per-volume trash |
| `MoveToTrash(path)` | **Default for user files** — reversible |
| `Unlink(path)` | Last resort, only for `Safe` entries under a validated category root |

### 5.5 Model invariants

1. Every entry's `allocated` ≤ its parent's `allocated`.
2. Category totals + `Unknown` = filesystem used bytes, within 1%.
3. An entry with `safety: Never` has no reclaim method.
4. `Unlink` is only ever emitted for a path whose ancestor is the category's declared root.
5. Rescanning produces the same `EntryId` for the same thing, so selections survive a refresh.

---

## 6. Feature catalogue

57 features across seven phases. Priorities: **P0** ship-blocking for that phase · **P1**
expected · **P2** desirable.

---

### Phase 0 — Foundation

*Goal: an empty app that already has every load-bearing mechanism. No user-facing features.*

| ID | Feature | Pri |
| --- | --- | --- |
| FND-1 | App shell & navigation | P0 |
| FND-2 | IPC contract & command conventions | P0 |
| FND-3 | Error & notification surface | P0 |
| FND-4 | Privileged helper | P0 |
| FND-5 | Settings store | P0 |
| FND-6 | Theme tokens & light/dark | P1 |
| FND-7 | Capability probe registry | P0 |
| FND-8 | Structured logging & diagnostics | P0 |
| FND-9 | Build, CI & packaging skeleton | P1 |

**FND-1 App shell & navigation.** Lazy-mounted views (§P9), keyboard-navigable, no splash
screen. *Accepts:* cold start to interactive < 800 ms; mounting a view is the only thing that
starts its data flow.

**FND-2 IPC contract.** Typed commands and events with a single naming convention; every
command returns `Result<T, AppError>`; long operations return a handle plus a progress event
stream and accept a cancellation token. *Accepts:* a generated TS type surface with no `any`;
cancelling any streaming command stops backend work within 200 ms.

**FND-3 Error & notification surface.** Every `AppError` carries a code, a plain-language
message, the underlying cause (exit status, stderr, errno) and an optional remedy. A persistent
notification centre keeps the last N failures. *Accepts:* a deliberately failed privileged
operation shows a specific message naming the operation and the cause — never "something went
wrong". This is the direct answer to Stacer's total absence of an error surface.

**FND-4 Privileged helper.** A separate binary with a typed, allow-listed operation set,
reached over a socket, authorised by polkit, with an audit log. No free-form command execution.
*Accepts:* the helper rejects any operation not in its enum; a fuzzed IPC payload cannot cause
it to execute an arbitrary path; every invocation is logged with caller, operation and result.

**FND-5 Settings store.** Versioned, schema-validated, atomically written to
`$XDG_CONFIG_HOME/nix/settings.json`. Stable machine keys — never localised strings (Stacer
persisted its start page as a translated name). *Accepts:* corrupt or future-version settings
load defaults and warn instead of failing.

**FND-6 Theme tokens.** Complete light and dark palettes as tokens; system-preference default
with an explicit override. The Stacer `values.ini` palettes are the starting point and are
already complete for both themes. *Accepts:* no colour is defined only inside a theme block.

**FND-7 Capability probe registry.** One place that answers "is `flatpak` present", "does this
schema exist", "is this filesystem btrfs", cached per session with explicit invalidation.
*Accepts:* no feature branches on a distro or desktop name (§P7); removing a backend binary at
runtime degrades exactly the affected controls after invalidation.

**FND-8 Structured logging.** `tracing` to stderr and to a rotating file under
`$XDG_STATE_HOME/nix/`, with a level control in Settings and a "copy diagnostics" action.
*Accepts:* logging is actually initialised — verified by a test that asserts the file exists
after startup. (Stacer implemented a logger and never installed it.)

**FND-9 Build, CI & packaging skeleton.** Reproducible builds, clippy + fmt + test gates,
artefacts for `.deb`, `.rpm`, AppImage and Flatpak from day one. *Accepts:* a tagged commit
produces all four artefacts unattended.

---

### Phase 1 — Storage core

*Goal: nix is already worth installing for storage alone. This phase is the product.*

| ID | Feature | Pri |
| --- | --- | --- |
| STO-1 | Filesystem overview | P0 |
| STO-2 | Space explorer | P0 |
| STO-3 | Reclaim scan | P0 |
| STO-4 | Reclaim executor | P0 |
| STO-5 | App & user caches | P0 |
| STO-6 | Logs & journal | P0 |
| STO-7 | Trash | P1 |
| STO-8 | Package manager caches | P0 |
| STO-9 | Protected paths & exclusions | P0 |

**STO-1 Filesystem overview.** Real mounts with used/free/total, filesystem type, device, and
mount options. Pseudo-filesystems (tmpfs, squashfs, overlay, devtmpfs) hidden by default behind
a disclosure. Source: `/proc/self/mountinfo` + `statvfs`. *Accepts:* reported usage matches `df`
within 1%; a snap-heavy Ubuntu install shows a handful of real volumes, not forty loop mounts.

**STO-2 Space explorer.** The answer to "what is eating my disk". A treemap plus a sortable
tree table over the space model, drilling from filesystem → directory → entry, with
attribution and category shown at every level. Scanning is streamed, cancellable, and
progressive — the treemap fills in as results arrive. Results **persist**, so
every visit after the first opens on the last scan immediately, labelled with its age, with
refresh offered — the view is never empty again (D6). While the view is mounted, the top-N
largest directories are watched via inotify so subtrees can be marked stale; inotify watches are
capped per user, which is why it is top-N and not the whole tree. *Accepts:* first useful paint
within 2 s on a 500 GB home directory; full scan of 2 M files under 60 s; cancellation is
immediate; unattributed space is visible as `Unknown`, never hidden; a second open renders from
cache in under 300 ms with a visible scan age.

**STO-3 Reclaim scan.** Produces categorised, safety-rated reclaimable entries with a total
"you can free X" figure. Runs per-category so partial results are usable. Replaces Stacer's
five fixed checkboxes with the full category set from §5.2. *Accepts:* every entry states its
provenance and its cost; the sum of `Safe` entries is achievable without any user-visible loss.

**STO-4 Reclaim executor.** The §P2 pipeline made concrete: a preview listing every affected
path and its method, an explicit confirm, execution with progress, then a per-item report of
what was freed and what failed. User files default to `MoveToTrash`, not `Unlink`.
**Stale
cache is never an action basis:** the executor re-stats every path in the preview immediately
before acting, which §7.4 requires anyway for TOCTOU safety. You may browse stale data; you may
never reclaim from it. *Accepts:* the reported freed bytes match the measured filesystem delta
within 2%; a partial failure reports exactly which items failed and why; no operation runs
without a preview; a path that changed between preview and execute is skipped and reported.

**STO-5 App & user caches.** `~/.cache` attributed to owning applications, plus thumbnails,
browser caches, and known per-app cache locations outside `~/.cache`. Each with a stated cost
("Firefox will re-download cached assets"). *Accepts:* the top ten cache consumers are named
applications, not opaque directory names.

**STO-6 Logs & journal.** Rotated and archived logs under `/var/log`, **and journald** — sized
via `journalctl --disk-usage` and reclaimed via vacuum by size or age. Active log files held
open by a running process are rated `Risky` and excluded from bulk selection. Stacer skipped
journald entirely (it filtered `/var/log` to regular files, and `journal/` is a directory) —
which usually meant skipping the single largest log consumer. *Accepts:* journal usage is
reported and vacuumable; no bulk operation can truncate a log a running service holds open.

**STO-7 Trash.** Freedesktop-spec trash: per-volume `.Trash-$uid`, correct relative `Path=` for
files under `$HOME`, restore support, and per-volume emptying. *Accepts:* a file trashed by nix
is restorable by the desktop's own file manager, and vice versa.

**STO-8 Package manager caches.** Reclaimed with the owning tool (`apt-get clean`,
`dnf clean packages`, `pacman -Sc`, snap/flatpak equivalents), never by unlinking cache files.
*Accepts:* on a Fedora machine the DNF cache is found and cleaned — Stacer pointed its DNF
branch at the pacman directory and always reported zero.

**STO-9 Protected paths & exclusions.** A built-in never-touch set (system binaries, live
databases, mounted media, `.git`, active container volumes) plus user-editable exclusions
consulted by both the scanner and the executor. *Accepts:* a protected path is never emitted
with a reclaim method (§5.5 invariant 3); user exclusions survive a rescan.

---

### Phase 2 — Storage depth

*Goal: cover the reclaimable space that actually dominates real machines.*

| ID | Feature | Pri |
| --- | --- | --- |
| STO-10 | Package storage attribution | P0 |
| STO-11 | Removable system packages | P0 |
| STO-12 | Snap & flatpak revisions | P1 |
| STO-13 | Container & image storage | P1 — done |
| STO-14 | Developer build artifacts | P1 — done |
| STO-15 | Large files & duplicates | P1 — done |
| STO-16 | Growth history | P2 — done |
| STO-17 | btrfs, LVM & ZFS awareness | **P0** — built, **unverified** |
| STO-18 | Incremental rescan | ~~P1~~ superseded |
| STO-19 | Bounded scan memory | **P0** — done |

**STO-10 Package storage attribution.** Installed size per package, joined into the space model
so a directory can name the package that owns it. This is what makes an uninstaller a storage
tool — Stacer's showed no sizes at all. *Accepts:* installed sizes are available for every
Tier-1 backend; sorting installed software by size is possible.

**STO-11 Removable system packages.** Superseded kernels and their headers/modules, orphaned
dependencies, and residual configuration — with a cascade preview. On Ubuntu these are
routinely the largest single reclaim available, and Stacer covered none of them. *Accepts:* the
currently booted kernel is never offered for removal; the preview matches what the package
manager actually does.

**STO-12 Snap & flatpak revisions.** Superseded snap revisions, unused flatpak runtimes, and
per-app data sizes. *Accepts:* only non-current revisions are offered; reclaim uses the native
tooling.

**STO-13 Container & image storage.** Docker/podman images, stopped containers, dangling layers
and build caches, with a prune preview. Volumes are `Risky` and never bulk-selected.
*Accepts:* preview totals match `system df`; no volume is removed without per-item confirmation.

**Met.** On the development machine `docker system df` reports 17.5 GB of reclaimable images, 3.04 GB
of build cache, 1.49 GB of unused volumes and 94 kB of stopped containers. Each prune quotes Docker's
own reclaimable figure, and every unused volume is its own `Risky` candidate rather than a single
"prune volumes" button — which is what per-item confirmation means in practice.

Docker formats sizes with Go's `units.HumanSize`, which is **decimal**: its `GB` is 10⁹. Verified
rather than assumed — `docker images` reports an image as `314MB` where `docker image inspect` gives
`314319387` bytes. Reading it as binary would overstate by 7%, more than three times the tolerance.
This is the second tool whose own units were the trap; APT was the first.

Docker is reached as the user's own account via `docker` group membership. Where that is absent the
daemon needs root, and rather than ship a privileged Docker path that has never been exercised the
category reports itself unavailable and explains why — the same line drawn for `ostree` in STO-12.

**STO-14 Developer build artifacts.** `target/`, `node_modules/`, `.venv`, `__pycache__`,
`build/`, plus package-manager caches for cargo, npm/pnpm, pip and Go — recognised by marker
files, rated `Review` with an honest cost ("next build will be slow"). *Accepts:* detection is
by project marker, not by name alone; a directory inside an active project is never rated `Safe`.

**Met, and it is the largest category in the tool.** On the development machine: **71.1 GiB** across
805 project artifact directories, plus **48.1 GiB** of package-manager stores outside `~/.cache`
(pnpm 17.1, npm 15.1, Gradle 14.3, Maven 2.7, Cargo 0.8). The whole preview went from 43.9 GiB to
**161.0 GiB**.

Every artifact directory must be corroborated by a marker a build tool would have left — `target/`
only beside a `Cargo.toml`, `build/` only beside a `pubspec.yaml`, `CMakeLists.txt`, `meson.build` or
similar. `build`, `dist`, `venv` and `target` are ordinary words, and a directory called `build` may
be hand-written source. `vendor/`, `bin/` and `obj/` are deliberately **not** recognised at all,
because the name cannot distinguish generated from committed. Nothing in the category is ever `Safe`:
regenerable is not the same as unwanted.

Candidates come from the cached scan rather than a walk. Traversal costs 33 s on this machine even
pruning at every artifact directory; `STO-19`'s tree already holds every directory big enough to
matter, so discovery is a filter plus one `stat` per candidate to confirm the marker. The category is
therefore unavailable until a scan exists, and says so.

**STO-15 Large files & duplicates.** Largest-files view over the scan (no separate query form —
Stacer made you fill in a `find` dialogue), and content-hash duplicate detection with
size-then-partial-hash-then-full-hash staging. *Accepts:* duplicate detection never reports a
false positive; hashing is incremental and cancellable.

**Met.** "Never a false positive" rules out finishing on a hash, however strong — a hash says "almost
certainly identical", and almost certainly is not never. So detection is staged and ends with a
**byte-for-byte comparison**: size, then the first 4 KiB, then whole content, then bytes. The
comparison only runs on a pair that already agrees on all three, so it is cheap and it is what turns
near-certainty into fact. That makes the hash a *filter* and never the verdict, which also means it
needs no cryptographic strength and no new dependency.

Two things a naive implementation would report are excluded:

- **Hard links.** Two names for one inode share their blocks, so deleting a name frees nothing and
  reporting them promises space that does not exist. Verified by removing the guard, which made the
  tool claim 2 MiB recoverable from a link.
- **Symbolic links**, never followed, here as everywhere else.

Cancellation is checked between chunks of a single file, so stopping does not wait for a gigabyte to
finish hashing. The largest-files list is a projection over the existing scan and costs no filesystem
access at all.

nix does not choose which copy to delete. Which one matters is a judgement about the user's work, not
about storage.

**STO-16 Growth history.** Periodic snapshots of **category totals plus top-N directory
sizes** — trends, not detail — enabling "`~/.cache` grew 4 GB this week". Collected by an
**opt-in systemd user timer** (D5), off by default, whose `ExecStart` is a subcommand of the
same binary (`nix snapshot --quiet`) rather than a second artefact. The job is an *incremental*
refresh against the existing scan cache, never a fresh walk, and is constrained in the unit:
`Nice=19`, `IOSchedulingClass=idle`, `ConditionACPower=true`, `Persistent=true` so a run missed
while the machine was off fires at next login. **No lingering** — `enable-linger` would make user
units run without a session and is out of scope. Where a user timer cannot be installed
(Flatpak sandboxes, non-systemd systems) the capability probe falls back to **session-timer
collection and says so**: "trend data will only be collected while nix is open". Storage is
bounded — a hard retention cap, because a storage tool whose own data grows without limit is
indefensible. *Accepts:* history survives restarts; the timer job completes in seconds and never
runs on battery; disabling removes the unit and deletes collected data; an orphaned unit from a
previous version is detected at startup and repairable; gaps in the series are rendered as gaps,
never interpolated (§P8).

**Met, with one requirement superseded and one criterion qualified.**

*Superseded:* the job was specified as "an *incremental* refresh against the existing scan cache,
never a fresh walk". That came from STO-18, which measurement retired — the scan is now about twice as
fast with bounded memory, so a full walk of this machine's home directory takes 28 s and buys a correct
answer without a second code path and a staleness model to be wrong about.

*Qualified:* "completes in seconds" is 33 s here for 5.4 million files, not the near-instant the
incremental design implied. Under `Nice=19` with `IOSchedulingClass=idle`, once a day, that is a
defensible cost — but it is seconds plural and worth saying so rather than leaving the impression of
something quicker.

Verified end to end by running `nix snapshot` twice as separate processes: 307.3 GiB recorded across
5,366,515 files, **3.1 KiB per sample**, so the 400-sample cap bounds the file at about 1.2 MiB.

Attribution needed work the specification did not anticipate. A scan leaves every entry
`Category::Unknown` — the scanner measures and does not judge — so "category totals" was one number
called unknown. `history::attribute` walks the tree top-down using the signals the reclaim categories
establish, and its output sums **exactly** to the scan total and independently agrees with what those
categories find: build artifacts 68.3 GiB against STO-14's 71.1, package caches 48.4 against 48.1,
application caches 36.3 against 36.3.

Not verified: installing the systemd units. Doing so enables a daily job on the developer's own
machine, which is not a side effect to create without being asked, so the install path is exercised
only through its unit text and orphan detection. Recorded beside `pkexec` in
[issues/README.md](issues/README.md).

**STO-17 btrfs, LVM & ZFS awareness.** **P0, not P1** — Fedora is Tier 1 and Fedora
Workstation is btrfs by default, so wrong numbers here are wrong numbers on a Tier-1 default.
Report-aware, reclaim-naive, never dishonest (D3):

- **Correct free space.** `statvfs` is misleading on btrfs — it ignores metadata allocation and
  RAID-profile duplication. Read `btrfs filesystem usage` instead. This is §P8 applied to a
  filesystem we already claim to support, not a btrfs nicety.
- **Subvolume and snapshot inventory** as a `Snapshot` category, so the space is attributed
  rather than landing in `Unknown`.
- **Exclusive vs shared extents** via `btrfs filesystem du`, which reports total/exclusive/
  set-shared per path **without requiring qgroups**. Quotas are usually disabled and carry a real
  performance cost; nix must never enable them silently.
- **Where exclusive size is unobtainable, suppress the estimate rather than fake it** — "shared
  with a snapshot, freeing this may reclaim nothing" instead of a number.

Snapshot *deletion* is backlog, behind explicit opt-in and its own design review: removing a
snapper or Timeshift snapshot can destroy a user's only rollback point.
*Accepts:* free space matches `btrfs filesystem usage`, not `df`; no reclaim estimate is shown
for extents nix cannot prove are exclusive; nix never claims space it cannot free.

**STO-18 Incremental rescan.** *Superseded — see below.* Originally: the optimisation half of the
cache STO-2 already owns, keyed by (path, mtime, size) so a rescan only walks what changed, and what
makes STO-16's timer job cheap enough to run daily.

Measurement showed the premise was wrong, so the feature was replaced by a direct optimisation of the
scan itself. Three findings, all on `/usr` (422,330 files, 45,488 directories, eight cores):

1. **The original criterion — under 10% of initial scan time — is unreachable for any *correct*
   rescan.** A directory's mtime does not change when a file inside it is modified in place, so no
   stat-based summary can prove a subtree unchanged without walking it; `readdir` alone is 47% of a
   full scan; and complete inotify coverage would need 782,060 watches for this machine's home
   directory against a kernel ceiling of 524,228. A ratio criterion is also the wrong shape, because
   it rewards a slow baseline.
2. **The scan was not filesystem-bound at all.** The parallel syscall floor for `/usr` is 344 ms; the
   scan took 1.5 s. The difference was building 454,129 tree nodes — first through a per-node mutex,
   then, after a first attempt at fixing it, by copying 200-byte nodes up every level of the
   recursion.
3. **Fixing that was worth more than the feature would have been**, and applies to the *first* scan
   as well as later ones, with no cache to invalidate and no staleness to trust.

*Accepts:* a filesystem scan sustains **under 10 µs per file**, asserted in CI by
`budget::SCAN_PER_FILE` against a generated fixture. Measured 1.59–1.80 µs per file, ~629,000
files/sec, against the 30 µs the throughput requirement in §8 implies.

**STO-19 Bounded scan memory.** *New, found while measuring STO-18.* Scan memory currently scales
with file count rather than with anything the user can see: a real home directory here produces
5,454,451 nodes at 200 bytes each and peaks at **4.2 GiB** resident, which is not shippable. The depth
cap was meant to bound the payload and does not, because depth 12 still reaches almost every file.

*Accepts:* a scan of a 5-million-file tree peaks under 500 MiB; the tree the UI receives is bounded by
significance rather than by file count, and says plainly how many entries it aggregated.

**Met.** Measured on the same home directory — 5,409,614 files in 782,119 directories:

| | before | after |
| --- | --- | --- |
| peak resident memory | 4,211.8 MiB | **94.9 MiB** |
| tree nodes | 5,454,451 | **48,848** |
| time | 34.7 s | **28.0 s** |
| invariant violations | 0 | 0 |
| root total vs `du` | 307.2 / 310.4 GiB | unchanged |

Children below a threshold fold into one `SpaceEntry::aggregated` node per directory, carrying exactly
the bytes they replaced — so a parent still equals the sum of its children and no total moves. The
threshold is a share of the tree's total, estimated from the filesystem's used bytes and corrected by a
second walk only when that estimate is more than 8x too coarse; callers who know the size (a rescan,
from the previous result) pass `size_hint` and skip the correction entirely.

The budget is an upper bound and realised counts sit well under it, because nesting means far fewer
entries clear the threshold than `total / threshold` suggests. Under-shooting is the safe direction: it
costs listing detail, never accounting.

---

### Phase 3 — Monitoring

*Goal: replace Stacer's Dashboard and Resources pages, honestly and cheaply.*

| ID | Feature | Pri |
| --- | --- | --- |
| MON-1 | Live metrics pipeline | P0 — done |
| MON-2 | Overview dashboard | P0 — done |
| MON-3 | History charts | P0 — done |
| MON-4 | Sensors — temperature & fans | P1 — done |
| MON-5 | Battery & power | P1 — done |
| MON-6 | Threshold alerts | P1 — done |
| MON-7 | Per-interface network | P2 — done |

**MON-1 Live metrics pipeline.** One sampler task per metric family, single owner of delta
state, fixed tick, 60-sample ring buffers held in the backend, paused when nothing subscribes.
Sources: `/proc/stat`, `/proc/meminfo` (parsed into a **map**, §P8 — Stacer's positional parse
was silently wrong), `/proc/loadavg`, `/sys/block/*/stat`, `/sys/class/net/*/statistics/*`,
`scaling_cur_freq`. Zero subprocesses (§P4). *Accepts:* a late-mounting view immediately
receives the full 60-second history; idle CPU under 1% of one core; memory figures cross-checked
against `free`.

**Met, all three, measured.**

| Criterion | Result |
| --- | --- |
| Idle CPU, nothing subscribed | **0 ms over 12 s — 0.0000%** |
| CPU while sampling once a second | 20 ms over 12 s — **0.167% of one core** |
| Memory against `free -b` | **every figure identical, byte for byte** |
| Late-mounting view | handed the existing window on subscribe |

Idle is *exactly* zero rather than merely small because the worker blocks on a condition variable
while nothing is subscribed — not a cheap tick, not a short sleep in a loop. A subscription's `Drop`
is what pauses it, so a view that goes away cannot leave the machine sampling.

Two device-filtering decisions this machine forced, both of which a naive implementation gets wrong:

- **43 of its 44 block devices are `loop`**, one per installed snap. The obvious filter — the kernel's
  `device` symlink — also excludes `dm-*`, `md*` and `zram*`, so an LVM, LUKS or RAID install would
  show no disk activity at all. That is the `fuseblk` mistake in another costume, so the rule is the
  narrow one: exclude `loop` and `ram` by name, require a non-zero size.
- **29 of its 31 network interfaces are virtual.** Summing them would not merely be noisy but *wrong*:
  a container's packet crosses its `veth`, then a bridge, then the card, incrementing three counters.
  The aggregate counts hardware-backed interfaces only, so each byte is counted once; every interface
  is still recorded individually for MON-7.

**MON-2 Overview dashboard.** At-a-glance CPU, memory, disk and network, plus **storage headline
figures** — this is a storage-first product, so the dashboard leads with "X GB reclaimable"
rather than burying it. *Accepts:* renders from cached state with no scan on mount.

**MON-3 History charts.** 60-second rolling charts per metric family; per-core CPU with an
n-core-safe palette (Stacer asserted past 20 cores); byte axes formatted in binary units;
**maxima decay** instead of only growing. *Accepts:* a traffic burst does not permanently
flatten the axis; a 64-core machine renders correctly.

**MON-4 Sensors.** `/sys/class/hwmon` temperatures, fan speeds and throttling state, absent
entirely from Stacer. *Accepts:* machines with no hwmon show a clean empty state.

**MON-5 Battery & power.** Charge, health, rate, time remaining, power profile. *Accepts:*
hidden on desktops via capability probe.

**MON-6 Threshold alerts.** CPU, memory, disk-usage and **disk-space** thresholds with
desktop notifications, hysteresis, and per-alert cooldown held in real state.
*Accepts:* an alert does not re-fire while the condition persists; disabling is immediate.

**MON-7 Per-interface network.** All interfaces with rates, addresses and link state; user
selects which to feature. Stacer picked the first non-loopback interface once at startup and
never re-evaluated. *Accepts:* switching Ethernet → Wi-Fi is reflected without restart.

**Phase 3 complete.** What the remaining six added, and what each one had to get right:

- **MON-2** leads with storage, because this is a storage-first product — Stacer's dashboard led with
  CPU and buried the disk, telling you about the resource you could do least about. Nothing is scanned
  on mount: filesystem figures come from `statvfs`, and the reclaimable figure is whatever the last
  preview found or an honest "not measured yet".
- **MON-3** decays its maxima. An axis that only grows is flattened permanently by one burst — download
  at 100 MB/s and every later chart of a 200 KB/s link is a flat line. The per-core palette is generated
  by golden-angle hue rotation, so sixty-four cores render as well as four; Stacer indexed a fixed table
  and asserted past twenty.
- **MON-4** reads every `hwmon` chip, not the first. This machine has eight and **no fan sensors at
  all** — an empty list is the right answer, not `0 RPM`.
- **MON-5** understands both battery forms. The kernel reports either `energy_*` (µWh) or `charge_*`
  (µAh); this laptop reports **only charge**, so an implementation reading `energy_now` — the one most
  examples show — displays nothing on it. Health (74% here) is a separate question from charge (97%).
- **MON-6** is a state machine, not an `if`. Hysteresis stops flapping, cooldown stops repetition, and a
  rule already firing stays silent — the criterion in as many words.
- **MON-7** decides the featured interface from the current reading. Activity alone is not enough: 29 of
  this machine's 31 interfaces report a carrier and an `up` state, so the test is physical *and* active.

Known gap, stated rather than hidden: **per-interface IPv4 addresses are absent.** There is no such
field anywhere in `sysfs` or `procfs`; obtaining one means `getifaddrs` or hand-built `rtnetlink`, so
either `unsafe` or a new dependency in a program that ships privileged code. IPv6 comes from
`/proc/net/if_inet6`, and what is missing is named.

---

### Phase 4 — Processes & services

| ID | Feature | Pri |
| --- | --- | --- |
| PRC-1 | Process table | P0 — done |
| PRC-2 | Process actions | P0 — done |
| PRC-3 | Process detail | P1 — done |
| PRC-4 | Process tree | P2 — done |
| SVC-1 | systemd unit inventory | P0 — done |
| SVC-2 | Unit actions | P0 — done |
| SVC-3 | Live unit state | P1 — done |
| SVC-4 | Timers & user units | P1 — done |
| SVC-5 | Unit logs | P2 — done |

**PRC-1 Process table.** Direct `/proc/<pid>` reads (§P4), **diff-updated** rather than
rebuilt each tick, with real instantaneous %CPU computed from `utime + stime` deltas — `ps`
reports an average since process start, which Stacer displayed as if it were live. Filter by
name, user, or state. *Accepts:* selection, scroll position and sort survive refreshes; column
choices persist.

**Measured against `ps` on this machine**, which is the point of computing it properly:

| Process | nix, instantaneous | `ps`, lifetime average |
| --- | --- | --- |
| python | **104%** | 75% |
| pycharm | **10%** | 31% |
| `kcompactd0` | **15%** | not listed at all |

`ps` would have a user believe their IDE is using three times what it is, and misses a kernel thread
that is busy right now. The figure is a percentage of **one core**, as `top` reports it, so a process
on four cores reads 400%.

Two things the reading has to get right. `/proc/<pid>/stat`'s second field is the executable name in
brackets and **may contain spaces and brackets** — this machine currently runs one called
`next-server (v1`, with a space and an unmatched bracket — so the line is split on the *last* bracket,
not the first and not on whitespace. And **a pid is not an identity**: they are reused, so delta state
is keyed on `(pid, start time)`, or a freshly started process would inherit its predecessor's counters
and show an enormous spike.

A complete pass costs about 43 ms for 655 processes, of which 16.7 ms is reading `stat` and is the data
itself. The table refreshes every two seconds while its view is open — `top` defaults to three — and
not at all when it is closed (§P9).

**PRC-2 Process actions.** Signal (TERM, then optional KILL escalation), renice, with
confirmation for non-own processes and a real result. *Accepts:* a failed signal reports errno;
no silent no-op.

**The specific no-op this is aimed at:** `kill(2)` **succeeds against a zombie**. The process has
already exited, the signal goes nowhere, and the call returns success — so a task manager reporting
"terminated" there is untrue about the one action a user most wants to be sure of. A zombie is refused
with an explanation before anything is sent, and every other failure carries the real `errno`, because
`EPERM` and `ESRCH` mean different things and a user can act on the difference.

`init` and `kthreadd` are never signalled, checked on both sides — here so nothing wrong is offered,
and again inside the helper so nothing wrong can be carried out. `TERM` to `systemd` as root begins a
shutdown, so "it would be harmless" is not true.

Renicing **downward** is privileged even for your own process: the kernel lets anyone be more polite
and nobody be less. That surprise is explained in the error rather than surfacing as a bare permission
failure. Both use `rustix`'s safe wrappers, so signalling needs no `unsafe` and no `libc`.

**PRC-3 Process detail.** Per-process CPU/memory/IO history, open files, threads, environment,
cgroup, and **disk footprint** — the link back to the storage model.

**PRC-4 Process tree.** Parent/child hierarchy with aggregated subtree resource use.

**Done.** The subtree figure is the point: a build system's cost is spread across dozens of short-lived
children and each one alone looks like nothing.

One defect found here, and found only because a test was wrong. The walk carried a cycle guard, and the
test written to prove it passed with the guard **deliberately removed** — because in a single-parent
graph no member of a cycle has a parent outside it, so a cycle is reachable from no root and the
recursion never started. Working out why exposed the real failure: those processes were reachable from
nothing, so they **vanished from the tree silently**. The walk now adopts anything still unvisited as a
root of its own, which makes "every process appears exactly once" hold whatever `ppid` claims — and
makes the guard load-bearing, verified by removing it and watching the stack overflow.

**PRC-3 Process detail.** Most of `/proc/<pid>` is unreadable for another user's process, measured
rather than assumed: `io`, `environ` and `fd/` are own-process only, while `cgroup` and `task/` are
readable for anything. So a detail panel for `systemd` shows its control group and thread count and
says, for each missing section, why.

nix deliberately does **not** offer to escalate for the environment. Another user's environment
routinely holds credentials, and a task manager that will show you any process's secrets for a password
is a credential-harvesting tool with a nice icon.

**SVC-1 systemd unit inventory.** Over D-Bus (`ListUnits`, `ListUnitFiles`, plus properties) —
one round trip instead of Stacer's `1 + 2N` subprocess spawns. Includes `static`, `masked` and
`generated` units, which Stacer's `--state=enabled,disabled` filter dropped, and template units,
which it discarded with a regex. *Accepts:* inventory of 400 units in under 500 ms.

*Delivered.* `ListUnits` returns this machine's 757 loaded units in **11.7 ms**, against the 1,515
subprocess spawns Stacer would have needed. Measured on Stacer's own filter for comparison: it would
have shown **183 of 491** unit files here, 37% — the omission was the larger of the two problems.

`ListUnitFiles` is the exception and does not belong in that round trip: it takes **2.2 seconds** for
491 files, because systemd walks the unit directories on disk rather than answering from loaded state.
It is a separate command the UI invokes only when the user asks for unit files, not part of the
inventory — folding it in would have made a 12 ms view a 2.2 s one.

**SVC-2 Unit actions.** Enable/disable, start/stop/restart via D-Bus, so polkit integrates
natively — one prompt, a real error, no custom helper. *Accepts:* a denied authorisation is
reported as denied, not as success.

**SVC-3 Live unit state.** Subscribe to unit change signals; no manual refresh needed.
Stacer loaded the list once per app run. *Accepts:* enabling a unit in a terminal updates the UI.

**SVC-4 Timers & user units.** `--user` units and `.timer` units with next/last elapse, both
absent from Stacer.

*Delivered.* Elapse times come from typed properties rather than parsed text, which is the whole point
of D10 — and they nearly didn't: systemd spells the property `NextElapseUSecRealtime`, zbus derives
`NextElapseUsecRealtime` from the method name, and the mismatched read failed silently through an
`.ok()` so every timer reported "never". The proxy now names all four properties explicitly.

**SVC-5 Unit logs.** Recent journal entries for the selected unit, with follow.

*Delivered.* `journalctl --no-pager -o json`, paged with `--after-cursor` so following does not re-read
what it has already shown. This is the one place in Phase 4 that shells out, and deliberately: the
journal's own read path is a C library with no stable D-Bus equivalent, and its JSON output *is* a
committed interface — unlike the human-facing table §D10 rejects. Unit names are validated against
`is_unit_name()` before reaching the argument list.

---

### Phase 5 — Software & system tools

*Phase 6 was folded in here once SYS-3 was cut (D7), leaving two system tools too small to stand
as their own phase.*

| ID | Feature | Pri |
| --- | --- | --- |
| PKG-1 | Installed software inventory | P0 — done |
| PKG-2 | Removal with cascade preview | P0 — done |
| PKG-3 | Multi-backend coverage | P1 |
| PKG-4 | Startup applications | P1 — done |
| PKG-5 | Repository management | P2 — done |
| SYS-1 | Hosts file editor | P1 — done |
| SYS-2 | File search | P2 |

**PKG-1 Installed software inventory.** Name, version, **installed size** (STO-10), summary,
install date, explicit-vs-dependency, sortable by size. Machine-readable queries only
(`dpkg-query -W -f=`, `rpm -qa --qf`, `pacman -Qi`), never display-string parsing — Stacer
round-tripped package names through a padded UI label.

Size is reported **twice, deliberately** (D2): the package database figure by default, plus a
per-row **Measure** action that walks the package's file list on disk. Both are shown and labelled
distinctly, and measured results are cached against the package version. *Accepts:* names are never
derived from rendered text; the two figures are never conflated or silently substituted.

*Delivered, with the second figure changed.* The plan named the pair "recorded" and "measured" and read
their difference as post-install growth. Measuring 40 real packages first showed that premise does not
hold — `Installed-Size` is a **build-time estimate**, rounded per file and counting directories, so file
contents sum to **0.80×** the recorded figure and four packages in five look like they *shrank*. The
pair now reported is content bytes and **committed** bytes, both measured from the same `stat`:

| | `flat-remix-gtk`, 30,547 files |
| --- | --- |
| Files contain | 76.1 MB |
| dpkg records | 96.3 MB |
| **Disk actually committed** | **181.3 MB** |

Only the last figure answers "what do I get back if I remove this", and it is 85 MB more than the
package manager's own number. Verified against `du` before anything was built on it. The recorded
figure is still shown and still sorts the list, and the difference is reported **signed** — an
overestimate and an exact match must not look alike.

Identity is arch-qualified (`libc6:amd64`), because 41 package names on this machine are installed for
two architectures at once with different sizes, and a bare name is not an identity. Install dates are
labelled **"last updated"**, since dpkg keeps no install date and the figure is the file list's mtime,
which an upgrade rewrites.

**PKG-2 Removal with cascade preview.** Show exactly what else goes (`apt-get -s remove`,
`dnf remove --assumeno`, `pacman -Rp`), require confirmation, report per-package results.
No unconditional empty invocations — Stacer ran `pkexec snap remove` with no arguments on every
uninstall. *Accepts:* the preview matches the actual outcome; a removal that would take out a
desktop environment is flagged prominently.

*Delivered.* The simulation is the authority on the cascade, and nix classifies the result itself —
which turned out to be the whole feature, because **`apt-get -s remove bash` exits zero**. On this
machine it plans to remove `bash`, `gdm3`, `ubuntu-desktop`, `ubuntu-desktop-minimal`, `aznfs` and
`mysql-apt-config`, and reports success. A tool that trusts the simulation's exit status offers its
user a button that destroys their system.

Danger is derived from dpkg's own metadata rather than a list of package names, with one exception that
had to be built because the metadata does not cover it:

| Signal | Source | Outcome |
| --- | --- | --- |
| `Essential: yes` | dpkg | **Refused** |
| `Priority: required` | dpkg | **Refused** |
| Part of the running kernel | `uname -r` | **Refused** |
| `Priority: important` | dpkg | Dangerous |
| Owns the configured display manager | `/etc/X11/default-display-manager` → `dpkg -S` | Dangerous |
| More leaves than was asked for | the simulation | Cascading |

The display-manager row exists because priority says nothing about it: `gdm3`, `gnome-shell` and
`ubuntu-desktop` are all `Priority: optional` here, so the metadata alone would have let "you will boot
to a text console" pass unmentioned. It is resolved from *this machine's* configuration rather than a
list of display-manager names, so it stays correct on a system running something the list would not
have known about.

**A refusal comes with the command to run by hand.** `Priority: required` is self-declared — `aznfs`, a
third-party NFS helper, declares it on this machine, and so does `libc6`, where removal destroys the
system. Nothing in the metadata separates those two cases, so nix refuses both; refusing an informed
user with no way through is how people end up guessing at commands, so the refusal names the exact one.

**The helper re-derives all of it.** `Op::RemoveSelected` cannot validate a user's selection against a
set it derives itself, so the guarantee is narrower and stated plainly on the op: every name must be an
installed package (which is what keeps flags and paths out of the argument list), and the helper runs
its **own** simulation and applies its own copy of the rules. A frontend that lies cannot make the
helper's simulation come out differently. `--allow-remove-essential` is never passed, so apt itself is
a last line of defence.

*Accepts:* the outcome is **measured** — the inventory is read before and after and diffed against the
preview, so a package that survived is reported as remaining and one that went unpreviewed is reported
as unexpected. apt exits once for the whole transaction and its status says nothing about individual
packages.

**Not yet exercised as root.** The refusal paths are fully tested unprivileged, because they all
complete before apt is invoked; the removal itself is on the isolated-VM list (§9.1 of `PLAN.md`), and
no test may reach it — a test that did would remove a package from whatever machine ran it.

**PKG-3 Multi-backend coverage.** apt, dnf, pacman, **zypper** (Stacer detected it and never
implemented it), snap, flatpak — each behind a common trait, each independently capability-probed.

**Per-manager implementations, not PackageKit** (D4). With the privileged helper landing in
Phase 0 (D1), PackageKit's main draw — free polkit integration for removal — is already paid for,
and what remains argues against it: it abstracts away exactly the detail this product needs
(installed size, orphan and superseded-kernel detection, cache locations, per-manager preview
semantics), two of our six backends bypass it entirely, and backend coverage is uneven across
distros. Trait surface: `list`, `installed_size`, `preview_removal`, `remove`, `clean_cache`,
`orphans`, `superseded`. A manager that cannot answer returns `Unsupported`, never a fabricated
value (§P7) — which keeps the zypper gap explicit rather than silent.

**PKG-4 Startup applications.** XDG autostart CRUD with **spec-correct defaults** — absence of
`Hidden` and `X-GNOME-Autostart-enabled` means *enabled*, which Stacer got backwards. Honour
`NoDisplay`, `OnlyShowIn`/`NotShowIn`, `TryExec`; show `Icon`; list `/etc/xdg/autostart`
read-only; atomic writes preserving unknown keys and comments. *Accepts:* distro-shipped entries
show their true state; an edit preserves every key nix does not manage.

*Delivered, and the Stacer claim verified in its source rather than assumed:*

```cpp
if (! hidden.isEmpty()) {
    enabled = (hidden != enabledStr);
} else {
    enabled = (gnomeEnabled == enabledStr);
}
```

With both keys absent, `gnomeEnabled` is `""`, `"" == "true"` is false, and the entry reads as
**disabled**. Measured against this machine: **neither key appears in any of the 44 entries**, so every
entry Stacer listed showed as disabled while actually running, and ticking one to "enable" wrote
`Hidden=false` into a file that was already starting. nix reports all 44 as enabled, which is the truth.

Stacer also read only `$XDG_CONFIG_HOME/autostart`, so the **42 entries in `/etc/xdg/autostart`** — the
ones a distribution ships, and the ones a user is most likely to want to stop — were invisible. nix
lists both, keyed on file name, with a user entry shadowing a system one.

**Turning off a system entry needs no privilege.** XDG already answers it: a file of the same name in
the user directory shadows the system file, so nix writes a copy carrying `Hidden=true` and never
touches `/etc`. This entire feature has no privileged code path.

`NoDisplay` is a label and a sort key, never a filter — 40 of the 42 system entries set it, so filtering
would leave the screen nearly empty while the thing the user came to stop is very likely among them.
`OnlyShowIn`/`NotShowIn` are matched case-insensitively against every name in `XDG_CURRENT_DESKTOP`
(`ubuntu:GNOME` here), with an exclusion beating an inclusion; a missing `TryExec` is reported, because
an entry that shows as on and does nothing is the most confusing state this screen can produce.

*Accepts:* every line keeps its original text and is re-emitted verbatim unless it is the one being
changed — which matters more than it looks, since the system entries here carry **338 localised keys**
(`Name[de]`, `Comment[fr]`, …) plus `X-GNOME-*`, `X-KDE-*` and `AutostartCondition` keys that an editor
rebuilding from known fields would silently delete. Asserted as a round trip over every real entry on
the machine. Section-aware, so a `[Desktop Action …]` block's own `Name` and `Exec` are never read as
the entry's, and an appended key lands in `[Desktop Entry]` rather than after the action section.
Enabling **removes** the key rather than writing `Hidden=false`, because absence is the specified
default.

**PKG-5 Repository management.** APT sources with **deb822 `.sources` support** — the format
current Debian/Ubuntu increasingly uses and which Stacer could not see at all — plus legacy
one-line entries. Entries tracked by file **and line number**, never located by substring match.
`signed-by` keyring fields are first-class. *Accepts:* a deb822-only system shows its real
sources; an edit never rewrites a different line.

*Delivered.* Measured against this machine: **47 repositories across 19 files**, 27 active, **2 in
deb822** with their `Signed-By` keyrings, 13 entries carrying a keyring path.

`/etc/apt/sources.list.d` holds **53 files and apt reads 18**. The other 35 are `.save` and
`.distUpgrade` copies left by release upgrades, several contradicting the live entries — so the
extension check is not tidiness, it is the difference between showing the machine's configuration and
showing its litter. Stacer's `entryInfoList({"*.list"}, …)` had the opposite problem: `.sources` matched
no glob it looked at, making both deb822 repositories invisible.

**On "an edit never rewrites a different line".** Stacer located the line like this:

```cpp
int _pos = sourceFileContent[i].indexOf(aptSource->source);
if (_pos != -1) { pos = i; break; }
```

A substring search from the top of the file, first hit wins, against an entry recording only its file
path. `deb http://x/ jammy main` is a substring of `deb http://x/ jammy main restricted`, so where a
narrower line follows a broader one, editing the narrower rewrites the broader.

Checked before repeating it as a claim: across all 46 source lines here, **no line's first substring
match is a different line**, so it misfires nowhere on this machine today. A latent hazard, not an
observed failure. Every entry nix produces carries its file and its **line number** — or stanza index,
for deb822 — and edits address that.

Two smaller things the one-line parser gets right that Stacer's did not: a line is only a repository if
its second field looks like a URI (`^\s*#*\s*deb` also matches `debug=1`), and toggling touches only
the leading `#` markers (`newSource.replace("#", "")` removes every `#` in the line, so a trailing
comment does not survive).

**The privileged write.** This is the only operation whose path crosses the boundary, so the helper
checks it against a set it **derives itself** — `apt_sources::source_files()`, the files apt actually
reads. `/etc/shadow` is not in it, and neither is `docker.list.save`. The content is then validated as a
well-formed file of that format, on both sides, by the same function. Same whole-file compare-and-swap
as `SYS-1`, and the same `replace_atomically` primitive.

**Not offered: adding a repository.** A repository without its signing key is useless, and fetching keys
on a user's behalf is not something this tool should do. Stated here rather than left as a gap.

---

**SYS-1 Hosts editor.** Table editing of `/etc/hosts` preserving comments and unparsed lines
(the one thing Stacer's editor did well), with IPv4/IPv6 validation, delete that actually
removes the line, external-change detection, and an **atomic privileged write preserving mode
and ownership** — never a fixed `/tmp` staging path (Stacer's created a symlink race).
*Accepts:* a concurrent external edit is detected and surfaced rather than overwritten.

*Delivered.* Every line carries its **original text**, and rendering emits that verbatim for any line
the user has not edited — so tab-versus-space alignment, unusual spacing and lines nix cannot parse all
survive untouched, and only edited lines are canonically formatted. That is stronger than "comments are
preserved", and it is a testable property rather than an intention: `render(parse(text)) == text`,
asserted against the real `/etc/hosts` as well as a captured copy. This machine's file uses tabs on its
first two lines and spaces on the rest, which is exactly what a reformatting editor destroys.

A commented-out entry is presented as a **disabled entry**, not as opaque comment text, because that is
what writing it that way meant. Address validation is what makes the inference safe: the line becomes an
entry only if what follows the `#` really parses as an IP, so the distribution's own
`# The following lines are desirable for IPv6 capable hosts` stays a comment. Addresses go through
`IpAddr`, which accepts exactly what the resolver accepts and normalises `0:0:0:0:0:0:0:1` to `::1` so
one address cannot appear twice under two spellings.

**The concurrent-edit check compares the whole file, not a digest.** The client sends the exact bytes it
read; the helper re-reads and refuses unless they still match. A hosts file is a few hundred bytes — 233
here — so there is nothing to save by hashing and no collision left to reason about. A hash would have
been the reflexive choice and strictly worse.

**The write, and the bug it exists not to have.** Stacer staged its hosts write at a predictable path
under world-writable `/tmp` and moved it into place as root, so any local user could plant a symlink
there and be written through. nix stages the replacement **beside the target in `/etc`**, with
`O_EXCL`: nothing unprivileged can create a file there to be raced, and `rename` is only atomic within
one filesystem — a `/tmp` on `tmpfs` turns the move into a copy, and a copy has a half-written window.
Mode, uid and gid are read from the original and applied before the rename, so the file is never visible
at the target path with the wrong permissions; a fresh file would be `0600 root:root`, and a `0600`
`/etc/hosts` breaks name resolution for every unprivileged process on the machine. A symlink or bind
mount at the target is refused with an explanation rather than replaced.

The content is validated **on both sides of the privilege boundary, by the same function**. In the app it
gives the user an actionable error before any password prompt; in the helper it is what stops the
operation being a way to write arbitrary content to a root-owned file that decides where name lookups
go. An unparsed line is tolerated on the way *in* — the user's existing file is not ours to reject — but
may never be introduced on the way out.

**SYS-2 File search.** A **filter over the storage index**, not a separate tool — the walker is
STO-2's walker, which is what makes this nearly free once Phase 1 exists, and it means results
carry size and category attribution for free. Streaming, no row cap. Stacer shelled out to
`find`, capped its table at 2000 rows, and emitted a `-invert` flag that isn't a real predicate,
so "invert" silently returned nothing. Filters: name/glob/regex, size, times, type, owner,
permissions. *Accepts:* results stream; the query is cancellable; every filter maps to real
behaviour.

---

### Phase 6 — Release readiness

*Runs partly in parallel with Phases 3–5; blocking for 1.0.*

| ID | Feature | Pri |
| --- | --- | --- |
| PLT-1 | Internationalisation | P1 |
| PLT-2 | Accessibility & keyboard | P0 |
| PLT-3 | Tray & background behaviour | P1 |
| PLT-4 | First-run experience | P2 |
| PLT-5 | Packaging & distribution | P0 |
| PLT-6 | Performance budget verification | P0 |
| PLT-7 | Documentation & in-app help | P1 |

**PLT-1 Internationalisation.** Harvest Stacer's 26 existing Qt `.ts` locale files into JSON
rather than restarting translation. Live language switching (Stacer required a restart), RTL
layout for Arabic. *Accepts:* no user-facing string is hardcoded; switching language needs no
restart.

**PLT-2 Accessibility & keyboard.** Full keyboard operation, visible focus, screen-reader
labels on every control, `prefers-reduced-motion` honoured, WCAG AA contrast in both themes.

**PLT-3 Tray & background behaviour.** Optional tray icon, quit-vs-minimise preference, and
`--hide` start — worth carrying over. Sampling stays paused while hidden unless an alert is
armed (§P9). *Accepts:* hidden in tray with no alerts armed, CPU is ~0.

**PLT-4 First-run experience.** Explain what nix will and won't touch, offer a first scan, set
up protected paths. Establishing trust before the first destructive action is the point.

**PLT-5 Packaging & distribution.** `.deb`, `.rpm`, AppImage, Flatpak, AUR. Desktop entry,
icon theme sizes, polkit policy file installed correctly per format. *Accepts:* each artefact
installs and runs on its Tier-1 target in CI.

**PLT-6 Performance budget verification.** The budgets in §7.3 asserted in CI on a fixed
fixture, failing the build on regression.

**PLT-7 Documentation & in-app help.** Per-category explanations of what reclaiming actually
does — inline, at the point of decision, not in a manual.

---

### Backlog — explicitly deferred

**Desktop tweaks** (ex-SYS-3, cut by D7) · **btrfs snapshot deletion** (from D3, needs its own
design review) · scheduled/automatic cleanup · filesystem-level dedupe (`duperemove`) ·
GPU monitoring · remote or multi-machine use · a plugin API · kernel/boot tuning ·
Wine/Proton and game-library storage · disk health (SMART) · benchmark tooling.

Deferred, not rejected. Each needs its own justification when it comes up.

---

## 7. Cross-cutting requirements

### 7.1 Privilege

One helper (FND-4) with a closed operation set. Prefer polkit-integrated APIs — systemd D-Bus
for units, PackageKit where viable — and write no helper code for them. No operation accepts
free-form argv. No `rm -rf` invocation is ever constructed from UI state. Root-destined file
writes are atomic and staged in a root-owned directory.

### 7.2 Failure semantics

Every operation reports success, partial success, or failure, with per-item detail for batch
operations. A cancelled authorisation is reported as cancelled — never as success, which was
Stacer's single most damaging behaviour. Exit status and stderr are captured for every
subprocess that survives §P4.

### 7.3 Performance budgets

| Budget | Target |
| --- | --- |
| Cold start to interactive | < 800 ms |
| Idle CPU, window open, monitoring view mounted | < 1% of one core |
| Idle CPU, hidden in tray, no alerts armed | ~0 (no sampling) |
| Resident memory, steady state | < 150 MB |
| Subprocess spawns in the steady-state monitoring loop | **0** (Stacer: ~2/second) |
| Space explorer, first useful paint | < 2 s on a 500 GB home |
| Full scan, 2 M files | < 60 s |
| Incremental rescan, unchanged tree | < 10% of initial |
| Service inventory, 400 units | < 500 ms (Stacer: seconds, 800+ spawns) |
| Cancellation latency, any streaming op | < 200 ms |

### 7.4 Security review checklist

Ahead of 1.0, each must be signed off: helper operation surface, TOCTOU on every privileged
write, path validation before any unlink, protected-path enforcement in both scanner and
executor, no shell interpolation anywhere, dependency audit, and packaged polkit policy
correctness.

---

## 8. Success criteria

nix 1.0 succeeds if:

1. A user with a full disk can answer "what's using my space?" **without leaving the first
   storage view**, and reclaim safely from the same place.
2. The three largest reclaim wins on a typical Ubuntu desktop — old kernels, snap revisions,
   caches — are all found and offered. Stacer found none of them.
3. **No operation ever fails silently.** Zero known silent-failure paths at release.
4. Reported reclaimed bytes match measured filesystem delta within 2%.
5. Steady-state idle cost is indistinguishable from an idle desktop.
6. Every Tier-1 platform passes the full feature matrix in CI.
7. No destructive action is reachable without a preview and a confirmation.

---

## 9. Resolved decisions

All decisions are settled. Recorded here with rationale, because the *reasons* constrain future work
more than the answers do.

| # | Question | Decision | Why |
| --- | --- | --- | --- |
| **D1** | Privileged helper in Phase 0, or Phase 1 read-only first? | **Phase 0.** | More work up front, but avoids building a throwaway escalation path — and it removes the main argument for PackageKit (D4). |
| **D2** | Installed package size — database, or on-disk measurement? | **Both, explicitly** — DB figure by default, per-row *Measure* action for the walk. Which two figures, **revised on measurement**: see below. | Never conflate or silently substitute them. The original reading of the gap as post-install growth was wrong, and the corrected pair is more useful. |
| **D3** | btrfs depth in v1 — report-only, or snapshot-aware reclaim? | **Report-aware, reclaim-naive, never dishonest.** STO-17 raised to **P0**. | Fedora is Tier 1 and btrfs by default, so `statvfs` numbers are already wrong there. Where exclusivity can't be proven, suppress the estimate instead of faking it. Snapshot deletion → backlog. |
| **D4** | PackageKit, or per-manager implementations? | **Per-manager, behind a common trait.** | It hides exactly the detail we need, two of six backends bypass it, coverage is uneven — and D1 already bought us polkit integration. |
| **D5** | Growth history — session timer, or systemd user timer? | **Opt-in systemd user timer**, with session-timer collection as the fallback tier. | On a session timer the series is one sample every few weeks, so the feature ships, works, and never produces a useful reading. A bounded periodic one-shot is not the resident daemon the non-goal protects against. Flatpak and non-systemd systems need the fallback regardless. |
| **D6** | Background indexing, or strictly on-demand? | **Cached-first, on-demand refresh, incremental.** | Gets ~95% of indexing's felt speed with no daemon. Safe because *you may browse stale data, you may never reclaim from it* — the executor re-stats before acting, which TOCTOU safety requires anyway. |
| **D7** | Keep desktop tweaks (SYS-3)? | **Cut.** Moved to backlog; Phase 6 folded into Phase 5. | The cost isn't implementation, it's a maintenance surface of every desktop × version × schema rename. Stacer's page rotted in a few years and then silently did nothing — worse than never shipping it. GNOME Tweaks and KDE System Settings already do this well. |
| **D8** | Keep the scaffolded React 19? | **Keep React 19**, with three binding architectural rules. | The treemap and charts must be canvas-rendered in *any* framework, and virtualised tables bound reconciliation cost — so the workloads where Solid or Svelte would win are canvas anyway. That leaves ecosystem and switching cost, both of which favour staying. |

### D8's binding rules

React was chosen on the assumption that these hold. If any is dropped, the decision should be
re-opened rather than quietly absorbed as a performance problem:

1. **Treemap and charts render to canvas.** A 100k-node treemap in the DOM is a non-starter.
2. **Every table is virtualised** — TanStack Table + TanStack Virtual — so only ~50 rows are live.
3. **Shared state lives in a small external store** (Zustand or similar), not context, so a 1 Hz
   metrics tick cannot re-render a subtree.

Re-open D8 if the team turns out to have no React familiarity; that is the one input that flips it.

### Consequential changes from these decisions

| Change | Source |
| --- | --- |
| STO-17 raised P1 → **P0** | D3 |
| STO-16 respecified: user timer, opt-in, bounded job, fallback tier | D5 |
| Scan **persistence** moved from STO-18 into STO-2 (Phase 1), so cached-first works without depending on a Phase 2 feature; STO-18 keeps the incremental-rescan optimisation | D6 |
| STO-18 superseded by a direct scan optimisation (2x, measured); its ratio criterion replaced by an absolute one; **STO-19 bounded scan memory** added at P0 | measurement, see STO-18 |
| systemd reached over **D-Bus**, not by running `systemctl`; the dependency feature-gated in `nix-core` rather than in a new crate | D10 |
| SYS-3 cut; Phase 6 (System tools) folded into Phase 5; Release readiness renumbered 7 → 6 | D7 |
| SYS-2 reframed as a filter over the storage index rather than a standalone search tool | D7 |
| Non-goal reworded from "always-on daemon" to "resident daemon or system service" | D5 |
| Total: 58 features / 8 phases → **57 features / 7 phases** | D7 |

---

### D10 — systemd over D-Bus, not by running `systemctl`

**Decided** for Phase 4, on measurements rather than on principle, because the two arguments usually
given for D-Bus both turned out to be false here.

**Not for speed.** Listing this machine's 764 units: `systemctl` 23 ms, D-Bus 26 ms — and the D-Bus
figure includes spawning `busctl`, so a native call is quicker still. Both are twenty times inside
`SVC-1`'s 500 ms budget. Speed does not distinguish them.

**Not for polkit either.** `systemctl` *is* a D-Bus client, so running it unprivileged hits the same
`org.freedesktop.systemd1.manage-units` action, with the same `auth_admin_keep`. Either approach means
nix writes **no privileged code for service management at all**, which is what §P6 actually asks for.
That win is shared.

What decides it is the two things left:

1. **No text to parse.** `ListUnits` returns typed data. `systemctl` returns a table built for humans
   that **truncates columns to terminal width** unless `--no-pager --plain --full` are all remembered.
   Parsing it is the same class of hazard as reading `/proc/meminfo` by line index, which is the
   defect §P8 exists because of. systemd guarantees its D-Bus interface is stable and explicitly does
   not guarantee CLI output.
2. **Signals instead of polling.** `SVC-3` is a *live* view. D-Bus emits `JobNew`, `JobRemoved` and
   `PropertiesChanged`; `systemctl` would mean a subprocess every second or two, which is precisely
   what §P4 forbids in a steady-state loop.

**The cost, stated.** `zbus` brings an async executor into a crate that is otherwise std threads
throughout, and needs the workspace MSRV moved from 1.85 to 1.87 — a one-line change, since the
toolchain is already 1.98.

**Where it lives, and why that needed checking.** `nix-helper` depends on `nix-core`, so anything added
to core risks linking an async runtime and a D-Bus stack into the binary that runs as root. The
dependency is optional behind a `dbus` feature that only `nix-app` enables.

Two different builds, with two different strengths of guarantee — worth separating, because an earlier
draft of this section ran the measurement in one and claimed the mechanism of the other:

- **`cargo build -p nix-helper`, how the helper is actually built and shipped.** nix-core compiles with
  `default` alone and `zbus` is **absent from the resolved dependency graph entirely** (`cargo tree`:
  zero matches). The isolation is structural — helper code that reached for zbus would not compile.
- **`cargo build --workspace`.** The resolver builds **one** nix-core rlib for the whole invocation and
  unifies the feature into it (`--message-format=json` reports `features = ["dbus", "default"]`, a single
  lib artifact), because nix-app asks for it. The helper links that rlib. Its **zero `zbus` symbols**,
  against 47,547 in the app, are therefore the linker discarding unreached code, not the feature gate.

So the shipped binary is safe by construction, but a whole-workspace build cannot *prove* it: zbus code
escaping its `#[cfg(feature = "dbus")]` gate would compile and test clean there. That is why CI runs
`cargo test -p nix-core` on its own, feature-off, as well as `--workspace` — the feature-on path is
already covered by `--workspace`, and the narrow run is the only thing that exercises the helper's
configuration.

So a feature gate is enough and there is no fourth crate — the rule that all system access lives in
`nix-core` survives intact, and the privileged binary's dependency tree stays at 52 crates against the
app's 293.

---

## 10. Deliberately not carried over from Stacer

| Dropped | Reason |
| --- | --- |
| Unity 7 / Compiz tweak surface | Targets deleted schemas; gated on a distro-name string match |
| Feedback form | Posted to a retired Heroku endpoint via a `curl` subprocess |
| Update check | Pointed at an abandoned repository, compared versions as strings |
| Splash screen | Only existed because all twelve pages were built eagerly at startup |
| `find`-form search UI | Query-builder-as-form; replaced by a native walker with live filters |
| Per-action `pkexec` prompting | Replaced by one audited helper and polkit-integrated APIs |
| The twelve-page sidebar structure | Replaced by a storage-first information architecture |
| Sidebar-tooltip page identity, translated setting keys | Fragile string-matching for navigation and persistence |

Carried over deliberately: the visual identity of the donut gauges and 60-second spline charts
with live readouts in the legend, the tray behaviour, the empty-state and loading affordances,
and the 26 translation files.
