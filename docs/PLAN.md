# nix — Development Plan

**Status:** v0.1 · **Companion to:** [SPEC.md](SPEC.md) (draft v0.2, all decisions resolved) ·
**Last updated:** 2026-08-26

The spec says *what* and *why*. This says *in what order, and how we know it works*. What actually
went wrong along the way — thirty-six defects and obstacles, with resolutions and the guards that
followed — is in [issues/](issues/README.md).

## Progress

| Milestone | State |
| --- | --- |
| **M0** Walking skeleton | **complete** |
| **M1** Foundation complete (Phase 0, tasks 0.1–0.11) | **complete** |
| **M2** "Where did my disk go?" (tasks 1.1–1.6, 1.15) | **complete** |
| **M3** First safe reclaim (tasks 1.7–1.10) | **complete** |
| **M4** Storage core complete (tasks 1.11–1.14) — **Phase 1 done** | **complete** |
| **M5** Storage depth (Phase 2) — **Phase 2 done** | **complete** |
| **M6** Monitoring (Phase 3) — **Phase 3 done** | **complete** |
| M7 Processes & services (Phase 4) | **in progress** — PRC-1 to PRC-4 done; SVC-1 to SVC-5 remain |
| M8 – M9 | not started |

Phase 0 shipped all eleven tasks: the three-crate workspace and quality gates, the error taxonomy,
the IPC contract with its progress/cancellation primitive, the app shell with lazily-mounted views,
the notification centre, the settings store, capability probing, structured logging, the privileged
helper with its exact-path allow-list and audit log, packaging for four formats, and the fixture and
budget harness. 84 tests, all four gates green in CI.

Two things changed during implementation and are worth carrying forward:

- **The helper's read allow-list was rewritten** from directory prefixes to exact paths, after the
  tests showed prefixes would have exposed `/etc/shadow` and other users' `/proc/<pid>/environ`.
- **Task 0.11's fixture had a real API bug** — naming temp directories by seed alone made
  parallel tests delete each other's trees. Fixed with a per-process counter.

M2 shipped the space model with property-tested invariants, filesystem enumeration with
btrfs-correct accounting, the streaming scanner at **447,000 files/sec** (2.23 µs per file against a
30 µs budget), the canvas treemap and virtualised table, cached-first opening, and inotify staleness
watching. 169 tests. The explorer is read-only by design, so it can be used and trusted before any
reclaim code exists.

Four bugs found by the tests, each worth remembering:

- The helper's read allow-list used directory *prefixes*, which would have exposed `/etc/shadow` and
  other users' `/proc/<pid>/environ`. Now exact paths.
- `rpc_pipefs` was missing from the pseudo-filesystem name list, so `is_pseudo` is now backed by a
  structural check — no capacity means not storage. While fixing it I widened the fuse rule and
  would have hidden NTFS volumes, which mount as `fuseblk`.
- The scanner created a `rayon::scope` per directory. Nested blocking scopes starve the shared pool;
  it looked fine in isolation and collapsed to fifteen seconds under concurrency.
- `EntryId` was `#[serde(transparent)]` over `u64` while declaring `string` to TypeScript — the same
  id crossed the wire as a string when used as a map key and a number when used as a value. It
  typechecked.

M3 built the safety machinery: protected paths, the preview → confirm → execute → report pipeline,
the category registry, and a spec-compliant trash implementation. **Trash is the only registered
category**, deliberately — the pipeline is proven against the one destructive operation whose
consequences the user has already accepted before anything irreversible is wired in. 228 tests.

The gate that matters is enforced by the type system rather than by convention: `execute` requires a
`Ticket` that only `preview` can mint, tied to the exact item set it described. A caller cannot
construct one, reuse a superseded one, or widen the selection afterwards.

M4 completed Phase 1. Five categories are registered — trash, application caches, rotated logs, the
journal and package caches — and three of them go through the privileged helper. 289 tests, and CI
green on all four jobs including the bundle.

Growing the privileged surface was the substantial part, and the design decision worth carrying
forward is that **an operation carries its category, and the helper re-derives which roots that
category owns**. A caller cannot claim `/etc/shadow` is a rotated log, because `/etc` is not a root
of any category. That is specification invariant 4 enforced on the privileged side rather than
trusted from the unprivileged one. The reclaim methods were retyped at the same time: a manager is
an enum and a vacuum limit is a number, so no caller-supplied text can reach a root command line.

Task 1.14's harness now checks the specification's fourth success criterion — reported bytes within
2% of the measured delta — end to end, rather than the claim being asserted in a document and never
tested.

M5 is under way. §6 orders Phase 2 by reclaim value, and the first two are done — with one swap:
**STO-10 landed before STO-11**, because deciding which kernels are removable needs a package-query
layer underneath it. On this development machine the result is **1.2 GiB of removable kernels**,
correctly excluding the running one.

STO-17 followed, completing the P0 items. Its acceptance criterion — no reclaim estimate for extents
nix cannot prove are exclusive — is enforced by `space::Reclaimable`, and a `Preview` now carries
both a stated total and a *promisable* one. The parsers for btrfs, ZFS and LVM are the weak link and
are labelled as such: the development machine has none of those filesystems, so they are tested
against documented formats rather than captured output.

**STO-12** followed, and produced the largest single figure the project has found: **3.3 GiB of
superseded snap revisions** across eighteen blobs — more than every removable kernel put together,
and completely invisible in Stacer, which listed snaps by name with neither revisions nor sizes.

Two things came out of it that were not planned for.

The first is that `space::Reclaimable`, built in STO-17 for copy-on-write filesystems, turns out not
to be about copy-on-write at all. Fifteen of those eighteen snap blobs have a link count above one,
because snapd hard-links every download into its own cache — so the sharing problem exists on plain
ext4. It appears a third time in flatpak, whose ostree deployments are hard links into a repository
that outlives them. One type covers all three.

For snaps that qualification was then *eliminated* rather than merely reported: the helper removes
snapd's cache link along with the blob, selected by inode so the only file it can touch is the one
snapd was just told to drop. That turns "up to 3.3 GiB, we cannot say how much" into an exact figure.

The second was a defect, found by running the whole pipeline against this machine rather than each
category in isolation. Every **logical** entry — one whose path is a description like
`kernel 6.8.0-136-generic` rather than a file — was being refused at preview by the *path* protection
rules, with the reason "only absolute paths can be checked". Both kernels, the residual-config set
and all eighteen snap revisions, about 4.5 GiB, never reached the user; and had they reached
execution, the time-of-check guard would have skipped them all as "already gone". So STO-11's headline
result was real as a measurement and inert as a feature.

`ReclaimMethod::acts_on_path` now separates the two cases: path rules and fingerprints apply to
paths, while a logical entry is guarded by the helper re-deriving its own eligible set at the moment
it acts — a stronger check, not a weaker one. Regression tests cover the classification, the
execution path and the preview stage. What surfaced this was the refusal list being *shown* rather
than swallowed, which is the argument for that design in one sentence.

STO-12 also added `space::Advisory`, for space nix can measure but should not act on. The case that
forced it: **699 MiB of unreferenced objects** in this machine's flatpak ostree repository, with no
`ostree` binary present to prune them. Hiding those bytes would be the failure this project exists
to avoid; shipping an automated privileged prune never once executed would be worse. An advisory
reports the size, the reason and the command, and is deliberately excluded from both preview totals.

**STO-18 was superseded rather than built**, on the evidence of measuring it first.

The plan called for an incremental rescan keyed by (path, mtime, size), accepting at "under 10% of
the initial scan time". Probing before designing showed that criterion is unreachable for any
*correct* rescan: a directory's mtime does not move when a file inside it grows in place, so nothing
stat-based can prove a subtree unchanged without walking it; `readdir` alone is 47% of a full scan;
and complete inotify coverage of this machine's home directory would need 782,060 watches against a
kernel ceiling of 524,228.

The same probes found something better. On `/usr` — 422,330 files, 45,488 directories, eight cores —
the parallel syscall floor is **344 ms** while the scan took **1.5 s**. The scan was never
filesystem-bound; it was spending two thirds of its time building 454,129 tree nodes, one mutex
acquisition at a time. Fixing that helps the *first* scan as much as the tenth, and needs no cache,
no staleness tracking and no second code path to keep correct.

Two wrong turns on the way, both measured rather than assumed — the first "fix" moved nodes up the
recursion and copied 200 bytes per node per level (1.5 s → 1.3 s only); a later attempt wrote
straight into the map under the lock and made it *worse* than the original at 2.3 s, because the
map's rehashing then blocked every thread. The version that landed batches one append per directory
and builds the map once, pre-sized:

| | before | after |
| --- | --- | --- |
| `/usr` scan | 1.5 s | **759 ms** |
| throughput | 447,687 files/sec | **628,637 files/sec** |
| per file | 3.55 µs | **1.59 µs** |
| tree nodes | 454,129 | 454,129 (identical, 0 invariant violations) |

`budget::SCAN_PER_FILE` was tightened from 30 µs to 10 µs so the gain cannot silently regress. The
30 µs figure came from the throughput requirement in SPEC §8 and had three orders of magnitude of
slack, which guards nothing.

**STO-19 was added at P0 as a result.** Scanning a real home directory — 5,406,062 files, 782,107
directories — takes 34.7 s and peaks at **4.2 GiB resident**, because it materialises 5,454,451 nodes
of 200 bytes each. The depth cap was supposed to bound this and does not, since depth 12 still reaches
almost every file. Its byte accounting is *correct* (307.2 GiB against `du`'s 310.4 GiB, the gap being
427 permission errors and not crossing filesystems) — the problem is purely that the model holds a
node per file. That has to be bounded by significance before the explorer can be pointed at a home
directory.

**STO-19 is done.** Scan memory no longer scales with file count. On the same home directory —
5,409,614 files, 782,119 directories:

| | before | after |
| --- | --- | --- |
| peak resident memory | 4,211.8 MiB | **94.9 MiB** |
| tree nodes | 5,454,451 | **48,848** |
| time | 34.7 s | **28.0 s** |

Children too small to list fold into one aggregate node per directory, holding exactly the bytes they
replaced. Totals are untouched: the root still reports 307.3 GiB against `du`'s 310.4 GiB, and every
directory still equals the sum of its children including its aggregate.

Three things went wrong on the way, and all three were found by measuring rather than reasoning:

1. **Counting first, then building** — the obvious way to learn the total before choosing a threshold —
   took the home-directory scan from 34.7 s to **61.2 s**. That tree is syscall-bound, so a second
   traversal simply doubles the dominant cost; `/usr` had hidden this by being page-cache hot. The
   threshold is now estimated from the filesystem's used bytes and corrected only when badly wrong.
2. **Correcting on any overshoot retried essentially every scan**, since a scanned tree is almost
   always smaller than its filesystem. `/usr` went 759 ms → 2.4 s. Now it corrects only past 8x.
3. **The aggregate node was built by the directory it summarised**, but whether that directory
   survives is its parent's decision, made later — so 633,035 of 674,065 nodes were orphans,
   unreachable from the root yet still holding bytes counted in an ancestor. `check_invariants` did not
   catch it because it only walks what it can reach. Aggregates are now built by the parent. A
   reachability test guards it, verified by reintroducing the bug.

**STO-14 is done**, and it is the largest category in the tool: **71.1 GiB** across 805 project
artifact directories and **48.1 GiB** of package-manager stores outside `~/.cache`. The whole preview
went from 43.9 GiB to **161.0 GiB**.

Detection is by marker, never by name — the specification's criterion, and not a formality, since
`build`, `dist`, `venv` and `target` are ordinary words. Candidates come from the cached scan rather
than a walk, because traversal costs 33 s here even while pruning; `STO-19`'s bounded tree already
holds every directory big enough to be worth reclaiming.

Designing it turned up **two accounting defects in shipped code**, both about the same thing:

1. **Trashing was reported as freeing.** The trash must sit on the same filesystem as its contents,
   because the move is a rename — so trashing frees nothing until the trash is emptied. `Report::freed`
   counted trashed bytes anyway, so clearing 9.8 GiB of cache reported "Freed 9.8 GiB" with the user's
   free space unmoved. Five accuracy tests missed it because they compared against a *directory-tree*
   measurement, which trashing satisfies. Fixed separately in `c051a69`.
2. **A trashed directory reported the size of its own inode**, about four kilobytes, rather than its
   contents — and `AppCacheCategory`, the only category that trashed anything, trashes directories
   exclusively. Every accuracy test trashed plain files.

Both are in [issues §03](issues/03-reclaim-pipeline.md). The second is the more embarrassing: it means
the reported figure for the one production path that existed was wrong by three orders of magnitude,
and had been since M3.

The preview also got 20x faster along the way — **27.8 s to 1.4 s** — by taking directory sizes from
the cached scan instead of walking `~/.cache` and the package stores, which are 85 GiB between them.
The first attempt at that made it *slower* (7.5 s to 11 s) by reloading and deserialising the whole
cached tree once per candidate instead of once per category.

**STO-13 is done.** 17.5 GB of reclaimable images, 3.04 GB of build cache and 1.49 GB of unused
volumes on the development machine. Volumes are the one `Risky` rating in the storage half of nix and
get one candidate each — a volume holds the only copy of what is in it, so there is no "prune volumes"
button to press by accident.

Docker's sizes are powers of ten, verified against `docker image inspect` rather than assumed. That is
the second tool to do this after APT, which suggests treating any human-formatted size from an external
tool as decimal-until-proven.

**STO-15 is done.** A `Find` view answers two questions without a form: what are the biggest files,
and what is here twice.

The interesting constraint was "never a false positive", which rules out finishing on a hash. Detection
stages size, then the first 4 KiB, then whole content, then a **byte-for-byte comparison** — the hash is
a filter and never the verdict, so it needs no cryptographic strength and no new dependency. Hard links
are excluded because two names for one inode share their blocks; verified by removing the guard, which
made the tool claim 2 MiB recoverable from a link that frees nothing.

**STO-16 is done, and Phase 2 with it.** A `Trends` view answers "what grew" rather than "what is
big", collected by an opt-in systemd user timer that is off by default.

Two things the specification did not anticipate:

1. **The job is a full scan, not an incremental refresh.** That requirement came from STO-18, which
   measurement retired. A full walk of this machine's home directory takes 28 s, and at `Nice=19` with
   idle I/O once a day that buys a correct answer for less than a second code path and a staleness
   model would cost. The criterion "completes in seconds" is met literally — 33 s — but not in the
   spirit the incremental design implied, and PLAN says so rather than rounding it favourably.
2. **Category totals needed attribution that did not exist.** A scan leaves everything
   `Category::Unknown`, so the first samples had a single category. `history::attribute` walks the tree
   top-down using the reclaim categories' own signals; its output sums exactly to the scan total and
   independently matches what those categories find, which is a useful cross-check on both.

Two of my own attempts at that attribution were wrong and measured wrong: classifying leaves reported
**0.14 GiB** of build artifacts on a machine holding 71 (a file inside `node_modules` has an ordinary
name), and aggregates having no path put **31 GiB** into "unattributed" that was really small files in
known places. The test for the second passed with the bug reintroduced, because its fixture put the
aggregate inside a directory whose whole subtree was already claimed — so the aggregate was never
reached. Fixed, and verified to fire.

**M5 complete. Phase 2 (Storage depth) is finished**: STO-10 through STO-19, one superseded, one added
at P0 mid-phase.

Note also that §4's parallelisation advice no longer applies: this is a solo project, which is why
**task 0.9 was completed within Phase 0 rather than deferred** — see §5.2 for why M2 is nonetheless
not gated on it.

---

## 1. How to read this, and what it assumes

- **Feature IDs** (`FND-1`, `STO-4`, …) refer to [SPEC.md §6](SPEC.md). **Principles** (`P1`–`P9`)
  refer to §3. **Decisions** (`D1`–`D8`) refer to §9. This plan does not restate them.
- **Task sizes** are relative and reliable: **S** ≤ 1 day · **M** 2–4 days · **L** 1–2 weeks ·
  **XL** > 2 weeks.
- **Milestone durations assume one experienced full-time Rust + TypeScript engineer.** That is my
  assumption, not a given — rescale for real team size. Sizes are the durable part of this
  document; the week ranges are the soft part.
- Phases 0 and 1 are broken down to task level because that is where the priority is. Phase 2 is
  at feature level. Phases 3–6 are at milestone level, deliberately: planning them in detail now
  would be fiction.

---

## 2. Sequencing strategy

Four rules decide the order. Each exists because violating it is a known, expensive mistake.

### 2.1 Mechanisms before features

The error model, IPC contract and helper protocol are load-bearing for all 57 features. Retrofitting
an error taxonomy after fifty call sites exist is the single most expensive refactor available to
us, and Stacer is the cautionary tale for what happens when you never do it at all — it shipped
with no error surface whatsoever because there was never a natural moment to add one.

So Phase 0 ships **no user-facing features**, and that is correct.

### 2.2 Vertical slices, not horizontal layers

Never "all backend, then all frontend". Every task lands a working path from kernel interface →
typed command → view. The first such slice is the walking skeleton (M0), and it uses the smallest
real feature available — enumerating mounted filesystems — precisely because it exercises the IPC
contract, the error model, the capability probe and a view without needing anything else.

### 2.3 Read-only before destructive

**M2 is a complete, useful, shippable product that cannot delete anything.** The space explorer
answers "where did my disk go?" with zero destructive surface. That means we can dogfood and alpha
it with no risk of data loss, and we get real scanner performance data from real machines before
any reclaim code exists.

This is the highest-leverage decision in the plan. Take it.

### 2.4 Safety machinery before the tempting categories

The preview → confirm → execute → report pipeline (P2) gets built against **one trivial category**
— trash — before any category worth reclaiming exists. If the pipeline arrives after package caches
and `/var/log`, there will be pressure to bypass it "just for this one", and that is exactly how
Stacer ended up with a bare `pkexec rm -rf` over a UI-built argument list.

Corollary: **protected paths (STO-9) land before the executor**, not after.

---

## 3. Milestone map

| M | Name | Contains | Outcome you can demo | Gate to pass |
| --- | --- | --- | --- | --- |
| **M0** | Walking skeleton | 0.1 – 0.5 | A typed command round-trips; a deliberately failed call shows a specific error in the UI | An induced failure at every layer surfaces a distinct, actionable message |
| **M1** | Foundation complete | 0.6 – 0.11 | Helper performs one real privileged operation; all four packages build in CI | Helper rejects an out-of-enum operation; perf harness runs in CI |
| **M2** | **"Where did my disk go?"** | 1.1 – 1.6, 1.15 | Treemap + tree table over a real home directory, streaming, cancellable, cached | Scan budgets met on the fixture; **zero destructive code paths exist** |
| **M3** | First safe reclaim | 1.7 – 1.10 | Trash and app caches reclaimed through the full pipeline | Freed bytes match measured delta within 2%; no path bypasses preview |
| **M4** | **Storage core complete** (Phase 1) | 1.11 – 1.14 | Logs, journal and package caches; honest totals | All Phase 1 acceptance criteria in SPEC §6 pass on Tier 1 |
| **M5** | Storage depth (Phase 2) | STO-10 – STO-18 | Kernels, snaps, containers, build artifacts, btrfs honesty | The three largest real-world reclaims are found and offered |
| **M6** | Monitoring (Phase 3) | MON-1 – MON-7 | Dashboard and history charts | Zero subprocesses in the steady-state loop; idle CPU < 1% |
| **M7** | Processes & services (Phase 4) | PRC + SVC | Process table and unit management | 400 units inventoried < 500 ms |
| **M8** | Software & tools (Phase 5) | PKG + SYS | Sized inventory, cascade removal, hosts, search | Removal preview matches actual outcome |
| **M9** | 1.0 (Phase 6) | PLT-1 – PLT-7 | Shippable release | Every §8 success criterion met |

**Rough sizing, one engineer:** M0–M1 ≈ 6–8 weeks · M2 ≈ 5–7 weeks · M3 ≈ 3–4 weeks ·
M4 ≈ 4–5 weeks. So **storage core (through M4) is a ~4–6 month single-engineer effort**, of which
the first third is foundation that never gets thrown away.

---

## 4. Phase 0 — Foundation (task level)

| # | Task | Feature | Size | Blocks |
| --- | --- | --- | --- | --- |
| 0.1 | Workspace layout, clippy/fmt/test gates, pre-commit hooks | FND-9 | M | everything |
| 0.2 | Error model: `AppError` taxonomy, cause chain, remedy field | FND-3 | M | everything |
| 0.3 | IPC contract: command/event conventions, TS type generation, reusable progress + cancellation primitive | FND-2 | L | all views |
| 0.4 | App shell: routing, lazy view mounting, theme tokens, layout | FND-1, FND-6 | L | all views |
| 0.5 | Error surface: notification centre, per-operation error rendering | FND-3 | M | — |
| 0.6 | Settings store: versioned schema, atomic write, migration path | FND-5 | M | — |
| 0.7 | Capability probe registry with explicit invalidation | FND-7 | M | 1.2, all backends |
| 0.8 | Structured logging + diagnostics export | FND-8 | S | — |
| 0.9 | **Privileged helper**: threat model, socket protocol, operation enum, polkit policy, audit log, one real operation end-to-end | FND-4 | **XL** | 1.11, 1.12, 1.13 |
| 0.10 | Packaging: `.deb`, `.rpm`, AppImage, Flatpak from CI, each with desktop entry, icons, polkit policy placement | FND-9 | L | M9 |
| 0.11 | Perf harness: reproducible filesystem fixture + budget assertions in CI | PLT-6 | M | 1.3 |

### Notes on the hard ones

**0.2 first, and non-negotiably.** Write the error taxonomy before the second command exists.
Minimum shape: a stable code, a plain-language message, the cause (exit status / stderr / errno /
D-Bus error), and an optional remedy. Every layer wraps rather than swallows.

**0.3 — the progress + cancellation primitive is the reusable asset**, not the individual commands.
Get one generic "long operation" pattern right (handle, event stream, cancel token, cleanup on drop)
and every scan, search and package query inherits it. Getting this wrong means writing cancellation
five times.

**0.9 — do not enumerate every helper operation up front.** Build the protocol, the polkit
integration, the audit log and the rejection path, then add exactly **one** operation (a privileged
read is enough) to prove the loop. The operation enum grows per feature, and each addition is a
small, reviewable diff. Threat-model review happens *before* the protocol is written, not after.

**0.11 — build the fixture early or the budgets are decoration.** A reproducible directory tree
with known file counts and sizes, generated by script, checked into CI. Without it, §7.3's numbers
are aspirations nobody can fail.

### Parallelisation

With two engineers: 0.9 (helper, XL) runs independently of 0.3–0.6 for its whole duration, and
0.10 is independent of everything. With three, 0.11 splits off too. The helper is the long pole —
start it as soon as 0.2 exists, since it needs the error model and nothing else.

---

## 5. Phase 1 — Storage core (task level)

| # | Task | Feature | Size | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | Space model types + property tests for the five §5.5 invariants | — | M | The model is the spine; test it as such |
| 1.2 | Filesystem enumeration; **btrfs-correct free space** | STO-1, STO-17 (slice) | M | See §5.1 below |
| 1.3 | Scanner: parallel streaming walker, apparent vs allocated, cancellation, error-per-entry | STO-2 | L | Budget-tested against 0.11's fixture |
| 1.4 | Scan cache: persist, reload, age labelling, invalidation | STO-2 (per D6) | M | Enables cached-first open |
| 1.5 | Treemap on canvas, with sub-pixel aggregation | STO-2 | L | D8 rule 1 |
| 1.6 | Tree table, virtualised, sortable, drill-down | STO-2 | M | D8 rule 2 |
| 1.7 | Protected paths + user exclusions, consulted by scanner **and** executor | STO-9 | M | **Before** any executor |
| 1.8 | Executor pipeline: preview, confirm, execute, per-item report, re-stat before act | STO-4 | L | Trash only at first |
| 1.9 | Reclaim scan framework + category registry | STO-3 | M | Categories become plugins |
| 1.10 | Trash category, freedesktop-spec compliant | STO-7 | M | First real category |
| 1.11 | App cache category + application attribution | STO-5 | L | Needs helper for system paths |
| 1.12 | Logs + journald category, open-handle detection | STO-6 | L | Needs helper |
| 1.13 | Package manager cache category + first backend trait impls | STO-8 | L | Needs helper; per-manager (D4) |
| 1.14 | Freed-bytes verification harness | §8 criterion 4 | M | Asserts the 2% claim |
| 1.15 | inotify staleness watching, top-N directories | STO-2 (per D6) | M | Watch limits ⇒ top-N only |

### 5.1 Pull the btrfs free-space slice forward

STO-17 is a Phase 2 feature, but **STO-1 reports wrong numbers on btrfs without it**, and Fedora
Workstation is Tier 1 and btrfs by default. So task 1.2 includes the free-space-accounting slice of
STO-17 now; subvolume inventory and exclusive/shared extent reporting stay in Phase 2.

Shipping M2 with confidently wrong free space on a Tier-1 default filesystem would violate P8 on
day one.

### 5.2 M2 does not need the helper

Everything through 1.6 operates on paths the user already owns. So **M2 can land before task 0.9
completes**, which takes the helper off M2's critical path entirely. Scope M2's first release to
the home directory, and add system-path scanning when the helper arrives.

This is worth exploiting: it means the most valuable, most demoable milestone is not gated on the
longest, riskiest task.

### 5.3 Category registry shape

Task 1.9 is the difference between nine categories and nine special cases. Each category declares:
its roots, how it enumerates, how it computes safety, its reclaim method, and its cost description.
Categories are then independently shippable and independently testable — which is what makes
Phase 2's nine additions tractable.

---

## 6. Phase 2 — Storage depth (feature level)

Ordered by reclaim value per unit of effort, which is roughly the reverse of the spec's ID order:

1. **STO-11 removable system packages** — old kernels and orphans are usually the single largest
   win on Ubuntu. Highest value, moderate effort. Do it first.
2. **STO-10 package storage attribution** — feeds STO-11 and PKG-1, and turns directories into
   named owners.
3. **STO-17 btrfs, LVM, ZFS** (remainder) — P0. Without it the numbers are wrong on Fedora.
4. **STO-12 snap and flatpak revisions** — done. Turned out to be the *largest* Ubuntu win, not
   the second: 3.3 GiB of snap revisions on the development machine.
5. **STO-18 incremental rescan** — unlocks STO-16's daily job being cheap enough to exist.
6. **STO-14 developer build artifacts** — large wins on developer machines specifically.
7. **STO-13 container storage** — large wins, narrower audience.
8. **STO-15 large files and duplicates** — mostly a projection of work already done.
9. **STO-16 growth history** — depends on 5; P2, and last for a reason.

Each is independently shippable behind a flag. That is the containment strategy for Phase 2 scope
creep: a category that turns out harder than expected gets deferred without blocking the others.

---

## 7. Phases 3–6 (milestone level)

Deliberately coarse. Detail these when M4 lands, not now.

**M6 Monitoring.** Build MON-1 (the pipeline) as one task; the other six are views over it. The
risk here is not difficulty, it is the idle-cost budget — assert it in CI from the first commit,
because a monitoring feature that costs 2% CPU at idle is a regression against Stacer's own
worst behaviour.

**M7 Processes & services.** Two independent tracks. The systemd D-Bus work (SVC-1) is the
unfamiliar part; spike it early. Nothing here blocks anything else.

**M8 Software & system tools.** PKG-1/2/3 depend on Phase 2's package work being done, so this
sequencing is already implied. SYS-2 is cheap because it is a projection of STO-2 (D7); do not let
it grow into a separate search subsystem.

**M9 Release.** PLT-1 (i18n) and PLT-2 (a11y) must not start here — see §8. What genuinely belongs
in M9 is packaging polish, the security sign-off (§7.4), first-run (PLT-4) and docs (PLT-7).

---

## 8. Continuous workstreams

Four things must run from the first week, because each is dramatically more expensive to retrofit
than to maintain:

| Workstream | Discipline from day one | Retrofit cost if skipped |
| --- | --- | --- |
| **i18n keys** (PLT-1) | Every user-facing string goes through the translation layer immediately, even while English-only | Auditing hundreds of components later, and missing some |
| **Accessibility** (PLT-2) | Keyboard path and labels with each component | A full a11y pass across every view |
| **Perf budgets** (PLT-6) | Budgets asserted in CI from 0.11 onward | You discover the regression at M9, with no idea which commit caused it |
| **Security review** (§7.4) | Threat model before the helper protocol; every new helper operation reviewed as a small diff | One enormous unreviewable audit before 1.0 |

---

## 9. Testing and verification

| Layer | Approach | Anchored to |
| --- | --- | --- |
| Parsers | **Golden-file tests** per format, with fixtures captured from every Tier-1 distro | P8 — the meminfo positional-index bug is exactly what this catches |
| Space model | **Property tests** for all five §5.5 invariants | §5.5 |
| Scanner | Fixture tree with known counts and sizes; correctness *and* budget assertions | 0.11, §7.3 |
| Reclaim | **Throwaway-container harness**: snapshot filesystem usage, reclaim, measure delta, assert within 2% | §8 criterion 4 |
| Package backends | Container per distro, integration tests against real package databases | Tier 1 matrix |
| Helper | Rejection tests, fuzzed payloads, audit-log assertions | FND-4 acceptance |
| Privileged ops | TOCTOU tests: mutate the path between preview and execute, assert skip-and-report | STO-4, §7.4 |
| Frontend | Component tests plus a virtualisation smoke test at 100k rows | D8 rules |

The reclaim harness (row 4) is the one that makes the whole product credible. Build it at 1.14, not
at M9.

---

## 10. Risk register

| Risk | Likelihood | Impact | Mitigation |
| --- | --- | --- | --- |
| Scanner too slow on large or network trees | High | High | Benchmark from 0.11; parallel walker; stream-and-cancel so slowness degrades to partial results rather than a hang; exclude network mounts by default |
| Helper security design flaw | Medium | **Severe** | Threat model *before* protocol; closed operation enum; every addition reviewed as a small diff; fuzzing in CI |
| Reclaim under-delivers vs the estimate (CoW, snapshots, hardlinks) | High | High | This is exactly why D3 says suppress the estimate rather than guess; the 1.14 harness catches regressions |
| Treemap performance at 100k+ nodes | Medium | Medium | Canvas (D8 rule 1) + aggregate anything below a pixel threshold; never render what can't be seen |
| Distro CI matrix cost and flakiness | High | Medium | Containers for package backends; VMs only where a container genuinely can't work |
| Phase 2 scope creep across nine categories | High | Medium | Each category independently shippable behind a flag; defer, don't block |
| Unfamiliarity with systemd D-Bus | Medium | Low | Spike SVC-1 during Phase 1 downtime; it blocks nothing |
| Rust/Tauri ramp-up unknown | Unknown | High | **Flagged, not estimated** — tell me the team's Rust experience and I will rescale §3 and revisit D8 |

---

## 11. Definition of done

**Task.** Compiles clean under clippy; unit-tested; no new `unwrap()` in an operation path; errors
typed and surfaced; strings translated; keyboard-reachable if it has UI.

**Feature.** Every acceptance criterion in SPEC §6 demonstrably passes; relevant §7.3 budget
asserted in CI; works or degrades explicitly on all Tier-1 platforms; if destructive, has preview +
confirm + per-item report and a TOCTOU test.

**Milestone.** Gate in §3 passes; a fresh install from each packaged artefact runs on its Tier-1
target; no known silent-failure path introduced.

---

## 12. The first week, concretely

If work starts Monday:

1. **Day 1** — Workspace layout and CI gates (0.1). Decide the crate split now: `nix-core`
   (samplers, scanners, model), `nix-helper` (privileged), `nix-app` (Tauri). This mirrors
   Stacer's one genuinely good architectural decision — its GUI-free core library — and it is what
   makes the helper a separate binary rather than an afterthought.
2. **Day 2** — Error taxonomy (0.2). Write it before the second command exists.
3. **Day 3–4** — IPC contract and the progress/cancellation primitive (0.3). One long-running
   fake operation, cancellable end to end.
4. **Day 5** — App shell with one lazy view and the theme tokens (0.4), rendering a real
   filesystem list from a real typed command.

End of week one, M0 is in sight: a typed command round-trips, a failure at any layer produces a
specific message, and the mechanism for every future long operation exists.

Start the helper threat model in parallel if there is a second engineer. It is the long pole, and
it needs only the error model to begin.
