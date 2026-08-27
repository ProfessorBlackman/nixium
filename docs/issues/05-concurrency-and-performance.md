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
entry 4 being as bad as it
is. Both attempts and the reason each failed are recorded in the module documentation, so the next
person to look at that code does not re-run the experiment.

**Guard.** The budget above, and the numbers written down.

---

## 4. Scan memory scaled with file count, not with anything a user can see

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

**Resolved** as STO-19: a directory keeps nodes for children at or above a size threshold and folds the
rest into one aggregate node carrying exactly the bytes it replaced. Peak memory 4,211.8 MiB →
**94.9 MiB**, nodes 5,454,451 → **48,848**, and the scan got *faster* (34.7 s → 28.0 s) because most of
those nodes were never worth building. Totals unchanged, zero invariant violations.

Because a child cannot hold more than its parent, a folded directory has nothing significant inside it
either — so folding prunes whole subtrees, which is what bounds the directory count. That was the part
that mattered: 782,119 directories is why a per-directory rule like "keep the largest sixteen" would
not have worked.

**Guard.** Four tests: aggregate bytes equal what they replaced at every level, a smaller budget yields
a smaller tree with identical totals, an aggregate carries its count and claims no path, and the
reachability test in entry 7.

---

## 5. Counting the tree before building it doubled the cost of the scan it was meant to speed up

**M5 / STO-19** · **Moderate** · **Found by** measuring the first implementation

The threshold that decides which children earn a node is a share of the tree's total, and the total is
not knowable before walking. The obvious answer is to walk twice: count, then build.

Measured, that took the home-directory scan from 34.7 s to **61.2 s**. That tree is syscall-bound — 5.4
million `stat` calls against 307 GiB that cannot sit in page cache — so a second traversal simply
doubles the dominant cost. `/usr` had hidden this throughout development by being small enough to be
page-cache hot, where a second pass is nearly free.

**Resolved** by estimating the threshold from the filesystem's used bytes, which `statvfs` gives for
free, and walking once. The estimate is good precisely where it matters: a scan rooted at a home
directory covers most of what the filesystem holds.

A second walk still happens when the estimate is badly wrong — a small subtree of a large filesystem —
and that is the case where walking twice costs almost nothing. `Options::size_hint` lets a caller pass
the figure outright, the obvious source being the previous scan of the same root.

**Guard.** A test asserting a hint is honoured exactly, so the correction cannot creep back in for
callers who already know the answer.

---

## 6. Correcting the estimate on any overshoot retried nearly every scan

**M5 / STO-19** · **Moderate** · **Found by** the timings not improving as predicted

Having replaced the counting pass with an estimate, the correction fired whenever the estimate came out
higher than the truth. That is almost always: a scanned tree is almost always smaller than the
filesystem holding it.

So nearly every scan walked twice anyway and the fix had bought nothing. `/usr` went 759 ms → **2.4 s**,
and the home directory sat at 63.4 s — worse than the two-pass version it replaced.

Both numbers were in front of me and I read them as noise at first, because the design "should" have
been faster. What settled it was `aggregated_below` in the result: it reported the *corrected*
threshold, which is only possible if the correction had run.

**Resolved** with a tolerance: correct only when the estimate is more than 8x too coarse. A home
directory comes out around 1.4x and is left alone; `/usr`, at 3% of the filesystem, comes out 31x and is
worth the second walk.

**Guard.** None mechanical — the tolerance is a judgement, and the numbers behind it are in
`ARCHITECTURE.md`. What *is* guarded is that a `size_hint` skips the correction entirely.

---

## 7. An aggregate built by the wrong node orphaned 633,035 of 674,065 nodes

**M5 / STO-19** · **Serious** · **Found by** the node count being 3.4x its budget

The first version had each directory build its own aggregate node and push it to the shared sink. That
is the natural place for it — the directory is what the aggregate describes.

But whether a directory survives is its **parent's** decision, and the parent makes it later, once the
child's walk has returned its total. When the parent folded the directory, the directory's node was
never created and its child list was discarded — leaving the aggregate in the sink referenced by
nothing: unreachable from the root, still occupying memory and payload, and holding bytes already
counted inside its ancestor's own aggregate.

On a real home directory that was **633,035 orphans out of 674,065 nodes**, which is why the tree came
out at 3.4x its 200,000 budget instead of comfortably under it.

`check_invariants` passed throughout, because it only walks entries it can reach from the root. So did
every existing test. The signal was the node count, nothing else.

**Resolved** by having the parent create the aggregate at the moment it commits to keeping the child.
The root is the one case with no parent, so `scan` builds its aggregate itself — which was missed on the
first attempt and caught immediately by the structural test, as a root claiming 2,555,904 bytes whose
children held zero.

**Guard.** `a_folded_directory_leaves_no_orphaned_aggregate` walks from the root with a deliberately
tiny node budget, so most directories fold, and asserts every node is reachable exactly once.
**Verified to fire** by moving aggregate creation back into the child.

---

## 8. Category attribution reported 0.14 GiB of build artifacts on a machine holding 71

**M5 / STO-16** · **Serious** · **Found by** reading the first real sample

A growth sample is meant to carry category totals. A scan leaves every entry `Category::Unknown`,
because the scanner's job is to measure and attribution is somebody else's — so the first samples had a
single category called unknown, which is not a category total.

The obvious fix classifies each **leaf** by its path. Run against this machine it reported:

```
  user_file            173.74 GiB
  unknown               74.76 GiB
  package_cache         34.05 GiB
  app_cache             24.52 GiB
  build_artifact         0.14 GiB   <- STO-14 finds 71 GiB of these
```

Two structural reasons, both invisible until real data went through it:

- A `node_modules` directory is recognised by its own name plus a marker beside it. The files *inside*
  it have ordinary names and match nothing, so classifying leaves finds the directory's contents and
  attributes none of them.
- An aggregate entry has no path at all — it stands for a set of them — and after `STO-19` those
  entries hold most of the bytes of most directories. They could never be classified, which is the
  74.76 GiB.

**Resolved** by attributing **top-down**: descend from the root, and the first directory that
classifies as something specific claims its whole subtree. Bytes are counted once because an
attributed subtree is not descended into, and an entry with no path inherits the category of the
directory it was found in.

```
  user_file            154.15 GiB
  build_artifact        68.30 GiB
  package_cache         48.44 GiB
  app_cache             36.29 GiB
  trash                  0.07 GiB
  sum 307.24 GiB  ==  scan total 307.24 GiB
```

Nothing unattributed, and the figures independently agree with what the reclaim categories find —
68.3 against STO-14's 71.1, 48.4 against 48.1, 36.3 against 36.3. Two separately-written mechanisms
arriving at the same numbers is the most reassuring thing in this file.

**Guard.** `attribution_accounts_for_every_byte_exactly_once` asserts the sum equals the root's total,
which is the property that makes the list trustworthy at all. Two regression tests cover the specific
failures, both **verified to fire**.

---

## 9. A test for that passed with the bug reintroduced

**M5 / STO-16** · **Moderate** · **Found by** verifying the guard rather than trusting it

The regression test for the aggregate half of entry 8 passed after the fix — and passed again with the
fix removed.

Its fixture put the aggregate inside a `target/` directory, which is a *recognised* category, so
attribution claimed the whole subtree and never descended to the aggregate at all. The line under test
never ran. The test was asserting something true for a reason unrelated to what it claimed to check.

**Resolved** by putting the aggregate somewhere attribution actually descends into — a directory
classified `UserFile`, which is deliberately not decisive. It then fails without the fix, with
`{Unknown: 8000}`.

This is [09-patterns.md §10](09-patterns.md) earning its place for the third time: a guard that has
never failed is a guard that has never been tested. It is also the second instance in this project of a
test passing because its *fixture had the wrong shape* — the first put plain files where production
uses directories.

---

## 10. Test fixtures caused an I/O storm

**M2** · **Friction** · **Found by** the diagnosis above

Several tests used `Spec::perf()` — 93,000 files — when they needed a few dozen. Under `cargo test`
these run in parallel, and building them concurrently made every timing measurement in the suite
noise.

**Resolved** by right-sizing each fixture to what its test actually asserts. `Spec::perf()` is now used
only by the performance tests that need that scale.

**Guard.** None mechanical. Convention, recorded here: a fixture's size is part of the test's cost, and
a correctness test does not need a performance fixture.

---

## 11. Fixture temporary directories collided between parallel tests

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
