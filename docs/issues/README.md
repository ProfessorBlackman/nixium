# Defect and obstacle log

Every bug, near-miss and piece of friction hit while building nix, with what caused it, how it was
resolved, and what now stops it coming back.

## Why keep this

Two reasons, and neither is ceremony.

The first is that a defect log is the only honest record of how much of a codebase's correctness is
*earned* rather than assumed. Reading the commit history of this project suggests a fairly smooth
run. Reading this file shows that the residual-config category overstated its findings by a factor of
about 175,000 before anyone measured it, that the entire old-kernel feature shipped inert, and that
the privileged helper briefly had a hole wide enough to read `/etc/shadow` through. That is the more
useful picture.

The second is that the *finding mechanism* matters more than the individual bugs. Each entry records
how it surfaced, and those cluster hard — see [09-patterns.md](09-patterns.md). Four separate
defects came through the Rust↔TypeScript boundary and every one of them typechecked. Two came from
running code against this machine rather than a fixture. Knowing which practices pay lets us spend
more on those and less elsewhere.

## How entries are written

Each has a heading, a one-line status strip, and prose in this order: what happened, why it was
wrong, how it was resolved, and the guard that prevents recurrence. Where there is no guard the entry
says so plainly rather than implying one.

**Severity** means consequence if shipped, not effort to fix:

| Severity | Meaning |
| --- | --- |
| **Critical** | A security hole, data loss, or a number wrong enough to destroy trust in the tool |
| **Serious** | A feature that silently does nothing, or a materially wrong figure |
| **Moderate** | Wrong behaviour with a visible symptom, or a correct thing reported badly |
| **Friction** | Cost paid, nothing shipped wrong — tooling fights, environment gaps, process trips |

**Found by** is the practice that surfaced it, and is the column worth reading down.

## Contents

| File | Covers | Entries |
| --- | --- | --- |
| [01-privilege-and-security.md](01-privilege-and-security.md) | the privileged helper's boundary | 8 |
| [02-rust-typescript-boundary.md](02-rust-typescript-boundary.md) | generated bindings and type drift | 8 |
| [03-reclaim-pipeline.md](03-reclaim-pipeline.md) | preview, guards and execution | 6 |
| [04-measurement-accuracy.md](04-measurement-accuracy.md) | sizes, and not overstating them | 10 |
| [05-concurrency-and-performance.md](05-concurrency-and-performance.md) | rayon, cancellation, scan cost, memory, sampling | 16 |
| [06-toolchain-and-lints.md](06-toolchain-and-lints.md) | the gates, and being caught by them | 12 |
| [07-tests-that-were-wrong.md](07-tests-that-were-wrong.md) | tests that passed for the wrong reason | 6 |
| [08-documentation-accuracy.md](08-documentation-accuracy.md) | claims about Stacer that were not true | 5 |
| [09-patterns.md](09-patterns.md) | what generalises, and what to do about it | — |

Seventy-one entries: **8 critical, 21 serious, 27 moderate, 15 friction**.

## The tally by finding mechanism

| Found by | Count | Note |
| --- | --- | --- |
| A gate in the toolchain (lint, clippy, budget, CI, a hook) | 17 | Cheapest possible — catches before any reasoning is needed |
| Running against this machine rather than a fixture | 17 | Wrong numbers, and 4.2 GiB of memory nobody had measured |
| Reasoning about the code while changing something nearby | 10 | Includes the helper's read hole and one near-miss never shipped |
| Reading generated output rather than trusting it compiled | 7 | The Rust↔TypeScript cluster, and a test count I misread |
| Writing a test and finding it disagreed with the code | 8 | Three were the test's fault, three the code's |
| Measuring before building, and finding the specification wrong | 1 | `PKG-1`'s premise, falsified in ten minutes — as `STO-18`'s was, under its own row |
| Verifying a documented claim against the source, or proofreading | 7 | Every Stacer claim re-checked turned out wrong — and one claim about nix's own build |
| An external tool refusing what was written | 1 | The artifact skill's CSS rule |
| The user reading output I had been filtering | 1 | A build warning on every compile for months |
| The machine doing the thing | 1 | A test escalated silently and removed a kernel |
| A user installing it and it not working | 1 | A package that could not run on a Tier-1 target, past a check written to catch that |

Two rows are worth dwelling on.

**Running against this machine** found sixteen defects, and a fixture could not have found any of
them — a fixture contains what its author expected, so if the expectation is wrong
the fixture is wrong in the same direction and the test passes. dpkg's real database and this machine's
real snapd output both disagreed with what I expected. The starkest case came late: every scanner
measurement had used `/usr`, and pointing the same code at a real home directory showed it peaking at
4.2 GiB — a number no fixture in the suite was large enough to produce.

**Gates** found the most, and cost the least. The thirteen they caught took minutes each. The defects found
by reasoning took hours, and the two most serious — an entire milestone shipping inert — were nearly
not caught at all. That asymmetry is the argument for [09-patterns.md §10](09-patterns.md).

## Open items

Things known to be unresolved, kept here so they are not quietly forgotten. These are also recorded
in `PLAN.md` and `ARCHITECTURE.md`; this is the index.

**How these get closed.** Most of the rows below share one cause: they need a machine that can be
broken. They are therefore validated together, in a **throwaway VM, once the first version is
feature-complete** — see [`PLAN.md` §9.1](../PLAN.md). Not on the developer's machine, and not
incrementally: §5 of [01-privilege-and-security.md](01-privilege-and-security.md) is what incremental
verification on a live machine already cost, and the guards added after it protect against the mistake
that was already made rather than the next one. A snapshot-and-restore loop also makes each operation
repeatable, which is the thing this machine cannot offer at any level of care.

So a row marked *unexercised* below is not a gap someone forgot. It is deferred deliberately, with a
named place to be closed, and the standing rule holds until then: no destructive privileged path ships
having never been exercised — anything still unexercised at the end ships as an advisory instead, the
way the ostree prune did.

| Item | Why it is open |
| --- | --- |
| `cow` parsers for btrfs, ZFS, LVM | This machine is ext4 with none of those tools; parsers are written to documented output formats and have never seen real output |
| `pkg::rpm` — the dnf and zypper backends | `rpm` is installed here with an **empty database**, so the query format is verified (`rpm --querytags`, checked by sabotage) and the results are not. Golden-file tested against documented output |
| `pkg::pacman` | No pacman and no Arch machine. Golden-file tested against documented output only — the least verified module in the project |
| `pkg::flatpak` runtime derivation | This machine has a flatpak installation with nothing installed in it |
| The `pkexec` path — **read operations verified** | Run for real on 2026-08-27 via `make install-helper && make helper-smoke`: helper starts as `uid=0`, protocol 4 handshake, an allow-listed read succeeds, `/etc/shadow` is refused, and three operations took **one** password prompt — `auth_admin_keep` working, which is the Stacer defect it exists to fix. The audit log landed at the root-owned `/var/log/nix/helper-audit.log`, world-readable so a user can inspect it and root-owned so they cannot edit it |
| The helper's **destructive** operations | Still unexercised as root. `WriteHostsFile`'s mechanics *are* tested — `replace_atomically` takes a path, so the compare-and-swap, mode preservation, staging cleanup and symlink refusal all run against a temporary file; what is unexercised is that path being `/etc/hosts` under real `pkexec`. `PackageManagerClean`, `JournalVacuum`, `ReclaimFile`, `RemovePackages`, `RemoveSelected` and `RemoveSnapRevision` have only ever run against a directly-spawned helper in tests. `RemoveSelected`'s **refusal** paths *are* fully tested unprivileged, since they all complete before apt is invoked — it is the success path that is unreachable, and must stay so: a test that got to `apt-get remove` would remove a package from whatever machine ran it. The safest first real test is `apt-get clean` — 985 MiB here, entirely regenerable |
| Installing the growth-history systemd units | Would enable a daily job on the developer's own machine, which is not a side effect to create unasked. The unit text, orphan detection and the `nix snapshot` subcommand are all exercised; `systemctl enable` is not |
| Dependency NOTICE attribution | Apache-2.0 §4(d); deferred to M9, recorded in `packaging/README.md` |
