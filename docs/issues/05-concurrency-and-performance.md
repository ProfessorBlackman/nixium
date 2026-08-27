# Concurrency and performance

---

## 1. Scanner cancellation took 37 seconds

**M2** · **Serious** · **Found by** a test with a latency budget

Cancelling a scan should be near-instant; the budget is 300 ms. It took **37 seconds**. Three rounds of
diagnosis, two of which were wrong, and the order matters because each wrong answer was plausible.

**First guess: debug builds are slow.** Measured instead of assumed — a release scan of 93,620 files
takes 233 ms. Slowness was not the explanation for a 37-second cancellation.

**Second guess: this specific test is heavy.** Partly true and worth fixing, but not the cause.
Sibling tests were each building 93,000-file `Spec::perf()` fixtures, and several running in parallel
produced an I/O storm that made every timing in the suite unreliable. Right-sized to what each test
actually needs.

**The real cause: architecture.** The walker created a `rayon::scope` **per directory**. A scope blocks
its caller until its children finish. Nesting scopes therefore fills the thread pool with threads
waiting on other threads, and on a deep tree the pool deadlocks into near-serial progress — so
cancellation could not be observed until the structure unwound.

**Resolved** by replacing nested scopes with `par_iter().map().reduce()`, which is built on
`rayon::join` and nests correctly by design: work is stealable at every level and no thread blocks on a
child it could be running itself.

Result: **447,687 files/sec** in release, 2.23 µs per file against a 30 µs budget.

The test was also made deterministic. It had been racing — cancelling after a sleep and hoping work was
still in flight. It now cancels **from inside the progress callback**, which by construction only fires
while work is happening.

**Guard.** `SCAN_PER_FILE` and `CANCEL_LATENCY` are budgets in `budget.rs`, asserted by a CI job that
runs the release build:

```
cargo test -p nix-core --release --lib -- budget:: --nocapture
```

A regression fails the build rather than being noticed by someone waiting.

---

## 2. The scan spent two thirds of its time building tree nodes, not reading the filesystem

**M5 / STO-18** · **Serious** · **Found by** measuring before designing

STO-18 asked for an incremental rescan on the assumption that a scan's cost is filesystem access.
Probing first, on `/usr` (422,330 files, 45,488 directories, eight cores):

| | |
| --- | --- |
| parallel `readdir` + `stat`, accumulating only a byte total | **344 ms** |
| walk with the depth cap at 1, so almost no nodes are built | 480 ms |
| walk building 275,037 nodes | 1.0 s |
| the real scan, 454,129 nodes | **1.5 s** |

The scan was **4.3× its own syscall floor**. Two thirds of it was building tree nodes through a
shared `Mutex<SpaceTree>`, one acquisition per entry, at roughly 2 µs a node.

That inverted the premise of the feature being planned: no rescan can go below the syscall floor, so
the most an incremental pass could ever have saved was the 344 ms — while the 1.16 s of overhead was
sitting there available to *every* scan, including the first, with nothing to invalidate.

**Resolved** by having each directory accumulate its own nodes and hand them to a shared sink in one
append, then building the map once, pre-sized. Ids come from `EntryId::for_path`, a pure function of
the path, so there is no central allocator to serialise on and a node built in isolation has the id it
would have had in a shared tree. Only ids — eight bytes — travel up the recursion.

Result: **1.5 s → 759 ms**, 447,687 → 628,637 files/sec, with a byte-identical tree (454,129 nodes,
zero invariant violations).

**Guard.** `budget::SCAN_PER_FILE` tightened from 30 µs to 10 µs, asserted in CI on a generated
fixture. The 30 µs came from the throughput requirement in SPEC §8 and had three orders of magnitude
of slack; a budget that loose guards nothing. The new limit catches any regression worse than about 5×
while leaving room for a CI runner slower than a desktop.

---

## 3. Two wrong fixes for that, both slower than expected

**M5 / STO-18** · **Moderate** · **Found by** measuring each attempt instead of assuming it worked

Recorded because the diagnosis above was right and the first two implementations of it were not. Both
looked obviously correct.

**Attempt one: accumulate nodes per thread and merge them up the recursion.** No shared lock at all,
so by the reasoning above it should have approached the floor. It gave **1.3 s** — a 13% improvement
on 1.5 s, not the 3× predicted.

The reason is that a `SpaceEntry` is 200 bytes, and merging node vectors upward `memcpy`s every node
once per level of tree above it. A node twelve deep was copied twelve times. Removing the lock
removed one cost and added another of the same order. Fixed by sending nodes straight to a shared sink
and letting only ids travel up.

**Attempt two: write into the finished map under the lock**, to avoid a staging vector. This made it
**2.3 s — worse than the original 1.5 s.** With the map behind the lock, its rehashing happens with
every other thread waiting on it, and a map growing to 454,129 entries rehashes about nineteen times.

So the version that landed keeps a staging vector and builds the map once, pre-sized. The trade-off is
that the vector and the map are both alive during the transfer, which is the direct cause of
[entry 4](#4-scan-memory-scales-with-file-count-not-with-anything-a-user-can-see) being as bad as it
is. Both attempts and the reason each failed are recorded in the module documentation, so the next
person to look at that code does not re-run the experiment.

**Guard.** The budget above, and the numbers written down.

---

## 4. Scan memory scales with file count, not with anything a user can see

**M5 / STO-18** · **Critical** · **Found by** pointing the scanner at a real home directory

Every measurement above used `/usr`. Running the same scan against this machine's home directory:

| | |
| --- | --- |
| files | 5,406,062 |
| directories | 782,107 |
| tree nodes | 5,454,451 |
| time | 34.7 s |
| **peak resident memory** | **4.2 GiB** |

4.2 GiB is not a performance problem, it is an unusable product — and on a machine with less RAM it is
an out-of-memory kill during the tool's primary operation.

The arithmetic is unremarkable once seen: 5.45 million nodes at 200 bytes is 1.09 GiB of nodes before
anything else, the map's table rounds up to a power of two, the staging vector from
[entry 3](#3-two-wrong-fixes-for-that-both-slower-than-expected) is alive at the same time, and each
node owns a heap-allocated path and label besides.

The depth cap exists to bound exactly this and does not work: at depth 12 almost every file in a home
directory is still within it.

Worth being precise about what is *not* wrong. The byte accounting is correct — 307.2 GiB against
`du`'s 310.4 GiB, the difference being 427 permission errors and not crossing filesystem boundaries.
Nothing is miscounted. The model simply holds a node per file when what a user needs is to know where
the bytes are.

**Not resolved.** Raised as **STO-19** at P0 in `SPEC.md`, because it blocks pointing the explorer at
a home directory, which is the tool's main use. The likely shape is bounding the tree by
*significance* rather than depth — individual nodes for children that matter, one honest
"*n* smaller items" node for the rest, which is what the treemap already does at the pixel level
(decision D8) applied at the model level instead.

Recorded here rather than fixed silently because it was found while measuring something else, and the
measurement that found it is the argument for STO-19 existing.

---

## 5. Test fixtures caused an I/O storm

**M2** · **Friction** · **Found by** the diagnosis above

Several tests used `Spec::perf()` — 93,000 files — when they needed a few dozen. Under `cargo test`
these run in parallel, and building them concurrently made every timing measurement in the suite
noise.

**Resolved** by right-sizing each fixture to what its test actually asserts. `Spec::perf()` is now used
only by the performance tests that need that scale.

**Guard.** None mechanical. Convention, recorded here: a fixture's size is part of the test's cost, and
a correctness test does not need a performance fixture.

---

## 6. Fixture temporary directories collided between parallel tests

**Phase 0** · **Moderate** · **Found by** intermittent test failures

Fixture directories were named from the process id and the spec's seed. Two tests using the same seed
in the same process — the normal case, since the seed is chosen for reproducibility — got the **same
path**, and deleted each other's trees mid-run.

The symptom was intermittent failures in whichever test lost the race, with an error about a missing
file that had definitely been created.

**Resolved** with a process-wide `AtomicU64` counter appended to the name:

```rust
static COUNTER: AtomicU64 = AtomicU64::new(0);
let n = COUNTER.fetch_add(1, Ordering::Relaxed);
let root = std::env::temp_dir().join(format!("nix-fixture-{}-{}-{n}", process::id(), spec.seed));
```

**Guard.** The counter is inside `Fixture::create`, so every caller gets uniqueness without having to
know about the problem. The comment above it explains what went wrong, since the code's purpose is
otherwise unguessable.

The same mistake recurred in STO-12's flatpak tests, which used a bare `process::id()`. Fixed the same
way, which suggests the right long-term answer is one shared temp-directory helper rather than the
convention being re-derived per module.
