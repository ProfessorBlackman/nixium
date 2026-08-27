# What generalises

Forty-nine entries is enough to see structure. This is the part worth re-reading.

---

## 1. One interface produced four silent failures, so the interface is the defect

Four defects came through `ts-rs` — `EntryId`'s number/string mismatch, the missing `Record<>` import,
bindings written to two directories, and two types named `Snapshot` overwriting each other. **Every one
compiled and typechecked.** Three of the four produced no error anywhere; they produced wrong data, or
no data, in a system where all the types agreed.

The reason is structural. Rust and TypeScript each had a complete, self-consistent view, and the
generator sat between them with no one checking that the two views described the same bytes. A
type-checked boundary between two type-checked systems feels safe and is not.

What worked was to stop trusting compilation and start asserting properties of the *generated output*:

| Guard | Catches |
| --- | --- |
| `no_two_exported_types_share_a_name` | one binding file clobbering another |
| Bindings committed, CI regenerates and diffs | a Rust type change not reflected downstream |
| `tsc --noEmit` over generated files in CI | an emitted name with no import |
| `serde_json` round-trip on hand-written impls | a declaration disagreeing with the wire format |

**Generalisation.** When a category of defect recurs through one seam, add a test that inspects the
seam's *output*, not more tests of either side. And verify the guard fires — the `Snapshot` test was
confirmed by reintroducing the collision.

---

## 2. Fixtures cannot find a wrong number, because a fixture contains what you expected

Six defects were wrong figures found by running against this machine. Not one was findable by fixture,
and the reason is not effort — it is epistemic. A fixture encodes the author's model. If the model is
wrong, the fixture is wrong in the same direction and the test passes.

The two worst numbers in the project came from exactly this:

- dpkg's `Installed-Size` for a residual package is what it occupied *when installed*. I assumed it
  described what remained. A fixture built from that assumption would have confirmed it. The real
  database disagreed by a factor of about 175,000.
- Snap blobs are hard-linked into snapd's cache, so 3.3 GiB of removable revisions would have freed
  nothing. A fixture would have had `nlink == 1`, because that is what I would have written.

**Generalisation.** For any figure derived from an external system's records, run it against the real
system and check the answer for plausibility before believing the code. Where the real system is not
available, say so in the code — `cow`'s parsers and `pkg::flatpak`'s runtime derivation both carry that
admission in their module documentation and in [README.md](README.md)'s open items, precisely because
they have not had this exposure.

---

## 3. Measure before designing, because the premise is the thing most likely to be wrong

STO-18 specified an incremental rescan, on the reasonable-sounding premise that a scan's cost is
filesystem access. Half a day of probing before writing any feature code established that the scan was
running at **4.3× its own syscall floor**, that two thirds of its time went on building tree nodes, and
that the feature's acceptance criterion was arithmetically unreachable for any correct implementation.

The feature was replaced by a change that made every scan twice as fast — including the first, which no
incremental scheme can help — and the probing also turned up a 4.2 GiB memory peak that had gone
unnoticed through three milestones of scanner work.

The probes were four throwaway test files, each about thirty lines, deleted afterwards. Nothing about
them was clever; they just measured the thing rather than reasoning about it.

**Generalisation.** Before building an optimisation, measure the cost you intend to remove and the
floor you cannot go below. If the gap between them is small, the optimisation is not worth building
whatever the plan says. Two of the three most valuable findings in this project came from measuring
something the plan had already decided.

The corollary, from the same episode, and it kept applying: **measure each attempted fix too.** Across
the scan work five implementations of correct diagnoses were themselves wrong — one gave 13% where 3×
was predicted, two made things *worse than the original*, and one silently orphaned 633,035 nodes.
None of those would have been noticed without a number, and in one case I read the number as noise for
two runs because the design "should" have been faster.

---

## 4. Testing components in isolation hid a whole milestone being inert

STO-11 was tested by calling `OldKernelCategory::candidates()` and checking the result: 1.2 GiB, both
kernels correct, running kernel excluded. Every one of those assertions was true. The feature did
nothing, because the preview stage refused every candidate for having a descriptive path and the
execution stage would have skipped them as already gone.

Each unit was correct. The composition was not, and no unit test can see that.

**Generalisation.** A feature is not done when its unit passes. It is done when the full pipeline has
been run end to end against real data and the output read by eye. That is a cheap check — the probe
that found this was about twenty lines and ran in three seconds — and it found the two most
consequential defects in the project.

---

## 5. A test can validate a weaker property than the one you are claiming

Five tests checked that reclaim figures were accurate to within 2%, and all five passed while nix was
reporting 9.8 GiB freed for a cache it had only moved to the trash. The tests compared against a
**directory-tree** measurement; the claim was about **free space**. Trashing satisfies the first and
not the second.

Nothing about the tests was sloppy. They measured carefully, to a stated tolerance, against an
independent implementation. They just measured the wrong noun — and being rigorous about the wrong
quantity is a very comfortable place to be, because everything passes.

The same suite made the same class of mistake a second way: every test that trashed something trashed
a plain **file**, while the only production category that trashes anything trashes **directories**. The
directory path reported the size of its own inode — four kilobytes for a 9.8 GiB cache — and no test
was shaped to notice.

**Generalisation.** For each test, write down the sentence it is supposed to prove, then ask whether a
plausible wrong implementation could pass it. Then ask a second question: does the fixture have the
same *shape* as production? A suite built entirely from files cannot speak about directories. Here, "the bytes left the directory" and "the user got
the space back" differ by exactly one design decision, and the harness never named which one it meant.
Naming it — the helper is now called `left_the_tree` — makes the gap visible in the source.

---

## 6. Show refusals, and the refusals will tell you what is broken

The inert-pipeline defect surfaced because M3 decided that candidates rejected by the protection rules
are **listed, not dropped**. That was a product decision about honesty: a user should be able to see
that nix declined to touch something.

It paid off as a diagnostic. The symptom was not "kernels are missing", which would have sent me to the
kernel code. It was twenty-one refusals whose reasons were all about relative paths, which named the
cause in the output.

**Generalisation.** Code that discards something should record what and why. A silent filter is a place
bugs live undisturbed, and the cost of surfacing them is usually one list in the UI.

---

## 7. Eliminate a caveat where you can, rather than reporting it

Snap revisions were initially reported as `AtMost { exclusive: None }` — honest, and useless: "we can
free somewhere between nothing and 3.3 GiB".

The better answer was to remove the cause. snapd's second link is a *download cache*; unlinking it
alongside the blob, matched by inode, makes the figure exact. Worst case if the reasoning about snapd's
intent is wrong: a re-download.

**Generalisation.** Honest hedging is the floor, not the goal. When a qualification exists because
something else holds a reference, ask whether that reference can be released before settling for
`AtMost`. Sometimes it cannot — flatpak's ostree repository stays qualified, and the orphaned-objects
case became an [advisory](../ARCHITECTURE.md) rather than a button.

---

## 8. A prefix is a set; an exact list is a list

The helper's read allow-list used directory prefixes, and `/etc` as a prefix admits `/etc/shadow`.
Separately, widening `fuse.` to `fuse` would have hidden every NTFS and exFAT volume, because
`fuseblk` is real storage.

Both are the same mistake: a prefix or substring test over a *delimited* namespace, where the delimiter
is exactly where the reasoning fails.

It has since come up twice more and been got right both times, because the pattern was known:
`protect.rs` matches paths **component-wise**, so `/usr` protects `/usr/bin` and not `/usrdata`; and
`pkg::flatpak::Ref::extends` matches dotted identifiers on the dot, so
`org.freedesktop.PlatformOther` is not an extension of `org.freedesktop.Platform`.

**Generalisation.** Never use `starts_with` on a delimited name. Split on the delimiter and compare
components, or compare the whole string. If the set of acceptable values can be enumerated, enumerate
it.

---

## 9. Make the dangerous option the one you have to ask for

`Elevation::default()` escalated to root. `Default` is the API's suggestion about what a caller
probably wants, and this one suggested "run as root". Two tests took it, on the reasonable assumption
that no helper would be installed during development — and one day one was.

Removing the `Default` impl fixed it in a way vigilance could not: there is now no way to obtain an
escalating `Elevation` without writing the word `production`, and the non-escalating constructor is
`cfg(test)`, so in a release build there is nothing to choose between at all.

**Generalisation.** For anything irreversible — root, deletion, network writes — the safe option gets
to be the default and the dangerous one gets a name. If a type can be constructed into a state that
does damage, make that construction say so at the call site, where a reader will see it. The compiler
enforcing this is worth more than any amount of care, because care is what runs out at 8pm on a
Thursday.

---

## 10. Encode the guard in the toolchain, not in memory

Ten of forty-nine entries were caught by a gate. Those cost minutes. The ones that cost hours were
caught by reasoning, and the two worst were nearly not caught at all.

Gates currently in force:

| Gate | Prevents |
| --- | --- |
| `unsafe_code = "deny"` | unsafe creeping in; earned its keep on day one by forcing a better design in `paths.rs` |
| `clippy::unwrap_used`, `dbg_macro`, `todo` | debugging residue and panics reaching a release |
| `clippy -D warnings` in CI | a warning being ignored long enough to become normal |
| Performance budgets asserted in CI | scanner regressions, which are otherwise noticed by someone waiting |
| Licence header hook **and** CI job | a file shipping without SPDX — the hook is bypassable, the CI job is not |
| Type-name uniqueness test | one generated binding overwriting another |
| `AppError` size assertion | `result_large_err` returning by accident |
| Helper protocol version handshake | a stale privileged binary accepting a request it misunderstands |
| Bindings committed and diffed in CI | generated types drifting from their source |

**Generalisation.** When a defect is found, the question is not only "how do I fix this" but "what would
have failed the build". If the answer is nothing, the fix is incomplete.

A failing assertion should also say what to do. `AppError`'s says *"Box the new field rather than
raising the ceiling"*, because a bare size assertion invites raising the number.

---

## 11. Do not filter your own build output

For most of this project I read compiler output through `grep -E "^error"`, because the interesting
line is usually an error and the rest is volume. The workspace was printing two ts-rs warnings on every
single compile, and I never saw them. It took the user running `pnpm tauri dev` and reading the log to
raise it.

The warnings turned out to be harmless — and redundant, which meant they were free to remove. That is
the worst possible outcome: months of a signal being trained out because it was never actionable, when
one minute would have made it actionable *and* silenced it.

**Generalisation.** A filter on build output is a decision to stop reading a channel. Check the
unfiltered output periodically, and treat a warning that cannot be acted on as a bug in its own right —
because the real cost is not the warning, it is that the next one will not be read either. Zero
warnings is the state that makes a warning meaningful.

---

## 12. Verify that the guard fires

Twice a guard was written and then confirmed by deliberately breaking the code:

- the type-name uniqueness test, by reintroducing the `Snapshot` collision;
- the helper's snap derivation tests, by making `derive_snap_revisions` return every revision instead
  of only the disabled ones — two tests failed with `bare revision 5 is active`, then the change was
  reverted.

A guard that has never failed is a guard that has never been tested. Two entries show what that looks
like when nobody checks: [07-1](07-tests-that-were-wrong.md), a test that scanned its own list of
banned words and so could not fail on a real occurrence, and
[05-9](05-concurrency-and-performance.md), a regression test whose fixture put its subject somewhere
the code under test never reached.

Checking is also how three of this project's tests were found to be wrong at all. It costs a minute.

**But not every guard can be tested by sabotage.** Disabling an escalation guard on a machine with a
live privileged helper installed is how [01-4](01-privilege-and-security.md) lost a kernel — and doing
it a second time, to verify the fix, escalated again. It was refused only because a *different* layer
held. Where breaking the guard is itself the hazard, assert the behaviour where it lives and say in the
test why sabotage was not used. Recognising which guards those are is part of using the technique.

**Generalisation.** After writing a test for a specific defect, break the code and watch it fail. It
takes a minute and it is the only evidence the test works.

---

## 13. When a test fails unexpectedly, read the code before changing the test

`candidates_are_ordered_largest_first` failed and looked like a comparator bug. It was not — the
category was reaching for a hardcoded path rather than the one it had been given, so under test it
sorted on sizes that were all zero. Fixing the test would have hidden a real defect.

**Generalisation.** A test failing for a reason you did not predict is information about the code.
Changing the test is the last step, not the first.
