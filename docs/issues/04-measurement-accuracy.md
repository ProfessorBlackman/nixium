# Measurement accuracy

The specification's fourth success criterion is that a reported figure must land within 2% of the
measured delta. Seven entries here; **five were found by running against this machine** rather than a
fixture, and a fixture could not have found any of those five, because a fixture contains what its
author expected.

---

## 1. Residual configuration overstated by a factor of about 175,000

**M5 / STO-11** · **Critical** · **Found by** running the category against the real dpkg database

A package removed without `--purge` leaves its configuration behind, and dpkg keeps a record marked
`rc`. The obvious size to report is dpkg's `Installed-Size` for that record, so that is what the first
version did.

`Installed-Size` for an `rc` package is what the package occupied **when it was installed** — not what
remains. dpkg keeps the field; it does not update it to reflect that only a config file is left.

The scale of the gap:

| Package | `Installed-Size` says | Actually remaining |
| --- | --- | --- |
| `zoom` | 640 MiB | a handful of config files |
| `bridge-utils` | 105 KiB | **124 bytes**, one file |

Across 241 residual packages the category claimed **36.1 GiB**. Measuring the conffiles that actually
exist gives **216 KiB across 22 packages** — the other 219 packages had nothing left at all.

Four orders of magnitude. This would have been the single most visible failure in the product: a user
told to expect 36 GiB back, receiving nothing, at which point every other figure the tool reports is
worthless too.

**Resolved** by measuring instead of asking. `parse_conffiles` reads dpkg's conffile list for the
package and `conffile_bytes` stats each path that still exists. Packages with nothing left produce no
candidate at all.

**Guard.** `crates/nix-core/tests/reclaim_accuracy.rs` checks the 2% criterion end to end rather than
leaving it asserted in a document. The category is also exercised against the live database on every
test run, which is what found it.

---

## 2. One kernel counted as two

**M5 / STO-11** · **Moderate** · **Found by** reading the output and recognising the version numbers

`linux-headers-5.15.0-190` and `linux-headers-5.15.0-190-generic` are two packages belonging to **one**
kernel. Grouping on the package's version suffix treated them as two separate removable kernels, which
made the list wrong in a way that undermines the safety rule: "never the newest" cannot be applied
correctly if the tool disagrees with the user about how many kernels there are.

**Resolved** with `KernelVersion::base()`, which takes the numeric core of a version by consuming
`-`-separated segments while they are digits and dots, stopping at the first that is not (`generic`,
`lowlatency`). Both packages resolve to `5.15.0-190` and group as one kernel.

**Guard.** Tests over real package-name shapes from this machine's database, including the flavour
variants. The safety rule has its own tests asserting that neither the running kernel nor the newest
installed one is ever offered — enforced in the category *and* re-derived independently in the
privileged helper.

---

## 3. APT reports in powers of ten

**M5 / STO-10** · **Moderate** · **Found by** comparing a parsed figure against `du`

`apt-get`'s removal simulation prints phrases like `After this operation, 1,234 MB disk space will be
freed`. Its `MB` is 10^6, not 2^20 — and APT is right to do so, since it says `MB` and not `MiB`.
Parsing it as binary overstates by about 5%, which alone is more than twice the 2% budget.

**Resolved** in `parse_size_phrase`, which uses powers of ten with a comment saying why so it does not
get "corrected" later.

**Guard.** A test over captured phrases at each unit, asserting the decimal interpretation.

---

## 4. A pseudo-filesystem was missing from the list

**M2** · **Moderate** · **Found by** listing this machine's mounts and reading the result

`fs::is_pseudo()` filters filesystems that hold no storage — `proc`, `sysfs`, `cgroup2` and so on — by
name, so the Explorer does not offer to scan them. `rpc_pipefs` was not in the list, so it appeared as
a mounted filesystem with a nonsense size.

The name list is the wrong shape of solution on its own: it is a denylist of an open set, and the next
kernel release can add to it.

**Resolved** by adding `rpc_pipefs`, and more importantly by backing the list with a **structural**
check. `holds_no_storage(total)` is `total == 0`, and `filesystems()` applies both — so a pseudo
filesystem nobody has named is still excluded, because it reports no capacity.

**Guard.** The structural check is the guard; the name list is now an optimisation rather than the
safety property.

---

## 5. Widening the pseudo-filesystem match would have hidden every NTFS and exFAT volume

**M2** · **Serious** · **Found by** re-reading a change before committing it — a near-miss, never shipped

While fixing entry 4 I widened a prefix test from `starts_with("fuse.")` to `starts_with("fuse")`, on
the reasoning that FUSE filesystems are not real storage.

`fuseblk` is how NTFS and exFAT mount. That change would have made every external drive and every
Windows partition invisible — in a storage tool, for the exact users most likely to have a full disk
they cannot account for.

Caught before committing, by reading the diff and asking which real filesystem types begin with
`fuse`.

**Resolved** by reverting to `fuse.`.

**Guard.** `fuseblk_is_real_storage_not_pseudo` asserts `!is_pseudo("fuseblk")`, with a comment naming
NTFS and exFAT so the reason survives without the story.

---

## 6. btrfs free space cannot be read from `statvfs`

**M2** · **Moderate** · **Found by** knowing btrfs does this, then confirming the documented behaviour

`statvfs` on btrfs reports free space that does not account for RAID profiles, metadata allocation or
unallocated chunks, and can be wrong by a large margin in either direction.

**Resolved** by shelling out to `btrfs filesystem usage --raw` when the filesystem type is btrfs, and
falling back to `statvfs` when the tool is absent.

**Guard.** Parser tests over documented output. Honestly labelled: this machine is ext4, so the btrfs
path has never run against a real btrfs filesystem. Recorded in the open items in
[README.md](README.md).

---

## 7. Four inherited defects in Stacer's trash implementation

**M3** · **Serious** · **Found by** reading the freedesktop trash specification alongside Stacer's code

These are Stacer's bugs, not nix's. They are logged because the documentation phase found them and the
implementation had to deliberately avoid each one, which is why `trash.rs` is longer than a naive
version.

1. **`Path=` was always absolute.** The spec requires it relative to the trash directory's volume for
   any trash other than the home one. An absolute path makes a restore to the wrong location, or
   fail.
2. **`Path=` was not percent-encoded.** A filename containing `%`, `=` or a newline produces a
   `.trashinfo` file that cannot be parsed — a newline ends the key/value line early.
3. **No per-volume trash.** Files were moved into the home trash regardless of origin, turning a
   rename within a filesystem into a copy-and-delete across one: slow, non-atomic, and able to fail
   half-way with the file in neither place.
4. **No collision handling.** Two files of the same name from different directories overwrite each
   other in the trash.

**Resolved** by implementing the specification: volume-relative `Path=` with percent-encoding,
per-volume trash discovery with the home trash as fallback, and collision-suffixed names.

**Guard.** Tests for each of the four, including a filename containing a newline and a `%`. The module
documentation lists Stacer's four errors at the top, so the tests read as deliberate rather than
arbitrary.

---

## 8. Every timer said "never", because systemd capitalises `USec` and zbus does not

**Phase 4** · **Serious** · **Found by** running against this machine

`SVC-4` shows each `.timer` unit's next and last elapse. On this machine every one of the 27 timers
reported **never scheduled**, including `apt-daily.timer`, which had visibly run that morning.

The proxy declared the properties by method name and let zbus derive the D-Bus name:

```rust
#[zbus(property)]
fn next_elapse_usec_realtime(&self) -> zbus::Result<u64>;
```

zbus derives `NextElapseUsecRealtime`. systemd spells it **`NextElapseUSecRealtime`** — `USec`, because
it is an abbreviation of microseconds, not a word. D-Bus property names are case-sensitive, so the read
failed with unknown-property.

What made it *serious* rather than a crash was the next line up: the result went through `.ok()`, on the
reasoning that a timer might legitimately have no next elapse. So a protocol-level failure and "this
timer is not scheduled" arrived at the display as the same value, and the view rendered the honest-looking
answer for all of them.

**Resolved** by naming all three `USec` properties explicitly rather than letting them be derived —
`NextElapseUSecRealtime`, `NextElapseUSecMonotonic` and `LastTriggerUSec`, every one of which zbus would
have got wrong in the same way:

```rust
#[zbus(property, name = "NextElapseUSecRealtime")]
fn next_elapse_usec_realtime(&self) -> zbus::Result<u64>;
```

**Guard.** `this_machines_timers_have_sane_schedules`, against this machine. It cannot know which timer
should be scheduled, so it asserts a weaker property with the right shape: no value is one of systemd's
sentinels (`0`, `u64::MAX`), and **if any timer has fired before, at least one must be scheduled to fire
again**. Verified by sabotage — restoring the derived spelling makes it fail with the diagnosis in the
message:

```
timers have fired before but none is scheduled to fire again — the property names are almost
certainly not being read: ["anacron.timer", "apport-autoreport.timer", "apt-daily.timer", ...]
```

Safe to sabotage, unlike the escalation guards of [01-privilege-and-security.md §5](01-privilege-and-security.md):
reading a D-Bus property changes nothing. The general lesson belongs with
[09-patterns.md §5](09-patterns.md): `.ok()` on a typed protocol read discards the distinction between
"the answer is nothing" and "the question was wrong", and only one of those is a fact about the machine.

This is the defect §D10 predicted in the other direction. Choosing D-Bus was partly about not parsing
text — and the failure still came from a string, just one in an attribute instead of in output.
