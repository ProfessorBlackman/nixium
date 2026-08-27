# The reclaim pipeline

Preview, the guards between preview and execution, and execution itself. The first two entries are
the most consequential defects in the project so far: a whole milestone's output was correct as a
measurement and did nothing as a feature.

---

## 1. Every logical entry was refused at preview by the path protection rules

**M5 / STO-12** · **Serious** · **Found by** running the whole pipeline against this machine

Not every reclaimable thing is a file. A kernel, a snap revision, a package manager's cache are
*logical* objects, and the `Candidate` for one carries a descriptive path — `kernel
6.8.0-136-generic`, `snap chromium revision 3499` — that was never meant to exist on disk.

`preview()` asks the path protection rules about every candidate before offering it. Those rules
reject a relative path, correctly, with `rule=relative_path` and the reason "Only absolute paths can
be checked against the protection rules."

So every logical candidate was refused. On this machine that was both removable kernels, the
residual-config set and all eighteen snap revisions — about **4.5 GiB**, roughly a tenth of everything
the tool could find, none of which reached the user. STO-11's headline result was a correct
measurement attached to an inert feature.

It surfaced the moment the full pipeline ran against real data instead of each category being tested
in isolation. The refusal list printed twenty-one entries whose reasons were all about relative
paths:

```
"kernel 5.15.0-190-generic"  rule=relative_path  Only absolute paths can be checked…
"snap chromium revision 3499" rule=relative_path  Only absolute paths can be checked…
```

Worth stating plainly: what surfaced this was the decision, made in M3, that refusals are **shown**
rather than swallowed. Had refused candidates been dropped silently the symptom would have been
"kernels never appear", with nothing pointing at why.

**Resolved** by adding `ReclaimMethod::acts_on_path()`, which distinguishes methods where the path
*is* the target (`MoveToTrash`, `Unlink`, `SystemFile`, `TrashEmpty`) from those acting on a logical
object (`Packages`, `SnapRevision`, `FlatpakUnused`, `PackageManager`, `JournalVacuum`,
`ContainerPrune`). Path rules apply only to the first group.

This is not a loosening. Every logical method goes through the privileged helper, which **re-derives
its own eligible set at the moment it acts** — so a kernel that stopped qualifying between preview and
execution is refused by the process that would carry out the removal. That is a stronger guarantee
than a path check, not a weaker one.

**Guard.** `a_logical_candidate_is_not_refused_for_having_a_descriptive_path` builds a registry
holding one category shaped exactly like `OldKernelCategory` and asserts the preview refuses nothing
and offers the candidate.

Preview total on this machine: **39.4 GiB → 43.9 GiB**.

---

## 2. The time-of-check guard would have skipped every logical entry as "already gone"

**M5 / STO-12** · **Serious** · **Found by** writing the regression test for entry 1

The same root cause one stage later, and it would have survived the fix above on its own.

`reclaim_one` re-stats the item's path immediately before acting, comparing against a fingerprint
taken at preview time. This is the TOCTOU guard, and it is right for a file. For a logical entry
`fingerprint()` returns `0`, which the code reads as "the path does not exist" and reports as:

> It is already gone.

So even with entry 1 fixed, selecting both kernels and eighteen snap revisions would have produced a
report saying all twenty were already gone, having done nothing. A silent no-op wearing a success
message.

Found because the first regression test I wrote for entry 1 asserted at the wrong level — it checked
execution, failed, and the failure message was about relative paths rather than fingerprints, which
revealed *both* stages were broken.

**Resolved** with the same `acts_on_path()` predicate: the fingerprint comparison is skipped for
logical entries, whose guard is the helper's re-derivation.

**Guard.** Two tests. `a_logical_method_is_not_guarded_by_a_path_that_never_existed` asserts the
classification across all ten method variants in both directions.
`a_logical_item_reaches_its_method_rather_than_being_skipped_as_missing` asserts the behaviour: a
kernel-removal item must fail at *elevation* — proving it got as far as trying — and an outcome of
`Skipped` is an explicit test failure with the message spelled out.

---

## 3. Snap blobs are hard-linked, so removing a revision frees nothing

**M5 / STO-12** · **Serious** · **Found by** checking `nlink` before believing a 3.3 GiB figure

Eighteen superseded snap revisions on this machine came to 3.3 GiB, the largest figure the project
had produced. Before reporting it I checked the blobs' link counts, and fifteen of eighteen were `2`:

```
2 197312512 /var/lib/snapd/snaps/chromium_3499.snap
2 270237696 /var/lib/snapd/snaps/firefox_8736.snap
1 336678912 /var/lib/snapd/snaps/vlc_3721.snap
```

snapd hard-links every download into `/var/lib/snapd/cache` (mode `0700`, root-owned, which is why
the second link is not visible without privilege). Dropping the revision removes one reference; the
blocks stay allocated until snapd's own cache pruning gets to them, on its own schedule.

So "reclaim 3.3 GiB" would have been false, and the user's free space would not have moved.

This also invalidated an assumption behind `space::Reclaimable`, built in STO-17 for btrfs and ZFS
snapshots on the belief that shared extents were a copy-on-write concern. They are not — a hard link
does the same thing on plain ext4, and flatpak's ostree deployments make it three cases. The type
generalised correctly; the reasoning about *why* it was needed was too narrow.

**Resolved** twice over, in the right order.

First, honestly: a blob with more than one link reports `Reclaimable::AtMost { exclusive: None }`,
which promises **zero** and carries the reason.

Then, properly: the caveat was *eliminated* rather than merely reported. The helper unlinks snapd's
cache entry along with the blob, selected **by inode** — matched against the blob's inode recorded
before removal, never by name or pattern — so the only file it can touch is the one snapd was just
told to drop. The cost of being wrong is a re-download, because that directory is a download cache
and nothing else. Snap revisions are therefore `Exact`.

**Guard.** `a_hard_linked_blob_promises_nothing` asserts `promisable() == 0` for `nlink > 1`.
`measuring_reads_size_and_link_count_from_disk` builds a real hard link and checks it is detected.
`cache_links_are_matched_by_inode_and_not_by_name` puts a decoy of identical size and a similar name
beside the target and asserts the decoy survives.

---

## 4. Claiming the full size after removing two of three links

**M5 / STO-12** · **Moderate** · **Found by** re-reading the code I had just written

The first version of the cache-link removal was:

```rust
if links <= 1 { return Ok(bytes); }
if unlink_cache_link(dev, ino) { Ok(bytes) } else { Ok(0) }
```

Which is right for the two-link case and wrong above it. `snap remove` accounts for one link and the
cache unlink for one more; if the blob had three links, two are gone, one remains, **no space is
freed**, and the code reports the full size. Precisely the overstatement this project exists to avoid,
in the function written to prevent it.

**Resolved** by counting. `unlink_cache_links` removes *every* matching entry and returns how many,
and the bytes are claimed only when the links removed account for every link there was:

```rust
let removed = 1 + unlink_cache_links(Path::new(SNAPD_CACHE), dev, ino);
if removed >= links { Ok(bytes) } else { Ok(0) }
```

The `else` branch logs `links` and `removed` at info level rather than staying silent, because "the
revision went but the space did not" is something an operator should be able to find out.

**Guard.** `cache_links_are_matched_by_inode_and_not_by_name` creates three links to one inode and
asserts all three are counted.
