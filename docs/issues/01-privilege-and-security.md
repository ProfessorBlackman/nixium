# Privilege and security

The helper runs as root. Everything here is a defect in the boundary that decides what it will do.

---

## 1. The helper's read allow-list matched directory prefixes

**Phase 0** · **Critical** · **Found by** writing tests for the thing I had just written

`Op::ReadTextFile` exists so the unprivileged app can read files it cannot open — `/proc/self/mountinfo`
and similar. The first version constrained it to an allow-list of **directory prefixes**:

```
/proc  /sys  /etc  /var/log
```

Which reads, in practice, as "the helper will read you any file under `/etc` as root". That includes
`/etc/shadow`. `/proc` is worse, because `/proc/<pid>/environ` belongs to whichever user owns that
process and routinely contains credentials passed through the environment.

The tests I wrote to demonstrate the allow-list working are what showed it. Asking "what is the worst
path that satisfies this predicate?" produced `/etc/shadow` immediately.

**Resolved** by replacing prefixes with an **exact-path** allow-list — four entries, compared whole:

```rust
const READABLE_FILES: &[&str] = &[
    "/etc/fstab",
    "/etc/os-release",
    "/proc/self/mountinfo",
    "/proc/sys/kernel/osrelease",
];
```

**Guard.** The comparison is exact string equality against a `const` list, so there is no
normalisation step to get wrong and no way to widen it by accident. Adding an entry is a visible
one-line diff in a file whose module documentation says every addition must be reviewed on its own.

The general lesson is in [09-patterns.md](09-patterns.md): a prefix is a *set*, and allow-listing a
set requires knowing every member. Exact paths are a list, and a list can be read.

---

## 2. `Path::components()` silently normalises `.` away, so the traversal check never fired

**Phase 0** · **Moderate** · **Found by** reasoning about the code while fixing entry 1

The prefix version above also tried to reject path traversal by walking `Path::components()` and
refusing `Component::CurDir` and `Component::ParentDir`. The `ParentDir` arm worked. The `CurDir` arm
was dead code: `Path::components()` normalises `.` out during iteration, so `/etc/./shadow` yields
`/`, `etc`, `shadow` and the check sees nothing to reject.

Harmless in isolation — dropping `.` is the correct normalisation — but it meant a guard that looked
like two checks was one, and I had no way to tell from reading it which half was load-bearing.

**Resolved** by deleting the whole approach along with entry 1. Exact-match comparison has no
normalisation, so there is nothing for a traversal sequence to exploit and no dead arm to misread.

**Guard.** None needed: the class of bug is gone with the code that had it. Recorded because the
lesson generalises — a validation arm that can never fire is worse than no arm, since it reads as
coverage.

---

## 3. Snap revision strings reach a root command line

**M5 / STO-12** · **Friction** · **Found by** designing the operation

Not a defect — an obstacle worth recording because it shaped the design. `RemoveSnapRevision` has to
put a revision into `snap remove --revision=N`, and that is an argument on a command line run as
root.

The primary defence is that both fields are matched against snapd's own output before either is used:
the package must be one snapd reports installed, and the revision must be one snapd reports
`disabled` for that package. So the values are not caller-supplied in any meaningful sense — they are
snapd's own strings, echoed back.

That is sufficient, but it makes the safety of the operation depend on a parser. So there is a second,
cheaper check that does not: `is_revision_shaped` requires digits, optionally prefixed with `x` for a
locally installed revision, and at most sixteen characters.

**Guard.** A table test covering `3499`, `x1` and `27710` as acceptable, and `--purge`,
`3499; rm -rf /`, `../../etc` and `3499 4000` as rejected. Plus two tests, run against live snapd
output, asserting the helper refuses an active revision and an invented snap with
`ErrorCode::HelperRejected` — verified to fire by deliberately breaking the derivation and watching
them fail.
