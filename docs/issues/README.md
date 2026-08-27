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
| [01-privilege-and-security.md](01-privilege-and-security.md) | the privileged helper's boundary | 3 |
| [02-rust-typescript-boundary.md](02-rust-typescript-boundary.md) | generated bindings and type drift | 6 |
| [03-reclaim-pipeline.md](03-reclaim-pipeline.md) | preview, guards and execution | 4 |
| [04-measurement-accuracy.md](04-measurement-accuracy.md) | sizes, and not overstating them | 7 |
| [05-concurrency-and-performance.md](05-concurrency-and-performance.md) | rayon, cancellation, scan cost and memory | 9 |
| [06-toolchain-and-lints.md](06-toolchain-and-lints.md) | the gates, and being caught by them | 6 |
| [07-tests-that-were-wrong.md](07-tests-that-were-wrong.md) | tests that passed for the wrong reason | 3 |
| [08-documentation-accuracy.md](08-documentation-accuracy.md) | claims about Stacer that were not true | 4 |
| [09-patterns.md](09-patterns.md) | what generalises, and what to do about it | — |

Forty-two entries: **3 critical, 12 serious, 16 moderate, 11 friction**.

## The tally by finding mechanism

| Found by | Count | Note |
| --- | --- | --- |
| A gate in the toolchain (lint, clippy, budget, CI, a hook) | 10 | Cheapest possible — catches before any reasoning is needed |
| Running against this machine rather than a fixture | 11 | Wrong numbers, and 4.2 GiB of memory nobody had measured |
| Reasoning about the code while changing something nearby | 7 | Includes the helper's read hole and one near-miss never shipped |
| Reading generated output rather than trusting it compiled | 5 | The entire Rust↔TypeScript cluster |
| Writing a test and finding it disagreed with the code | 4 | Two were the test's fault, two the code's |
| Verifying a documented claim against the source, or proofreading | 4 | Every Stacer claim re-checked turned out wrong |
| An external tool refusing what was written | 1 | The artifact skill's CSS rule |

Two rows are worth dwelling on.

**Running against this machine** found eleven defects, and a fixture could not have found any of them — a fixture contains what its author expected, so if the expectation is wrong
the fixture is wrong in the same direction and the test passes. dpkg's real database and this machine's
real snapd output both disagreed with what I expected. The starkest case came late: every scanner
measurement had used `/usr`, and pointing the same code at a real home directory showed it peaking at
4.2 GiB — a number no fixture in the suite was large enough to produce.

**Gates** found the most, and cost the least. The ten they caught took minutes each. The defects found
by reasoning took hours, and the two most serious — an entire milestone shipping inert — were nearly
not caught at all. That asymmetry is the argument for [09-patterns.md §8](09-patterns.md).

## Open items

Things known to be unresolved, kept here so they are not quietly forgotten. These are also recorded
in `PLAN.md` and `ARCHITECTURE.md`; this is the index.

| Item | Why it is open |
| --- | --- |
| `cow` parsers for btrfs, ZFS, LVM | This machine is ext4 with none of those tools; parsers are written to documented output formats and have never seen real output |
| `pkg::flatpak` runtime derivation | This machine has a flatpak installation with nothing installed in it |
| The `pkexec` path | Every helper test spawns the binary directly. The real polkit path has never been exercised, and the helper now removes packages and snap revisions as root |
| Dependency NOTICE attribution | Apache-2.0 §4(d); deferred to M9, recorded in `packaging/README.md` |
