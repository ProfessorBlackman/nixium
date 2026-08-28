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

## 3. The polkit action's path could drift from the client's, silently

**Phase 0, guarded in M5** · **Moderate** · **Found by** reading `pkaction --verbose` after the first real prompt

polkit matches an action to a program by the absolute path in its
`org.freedesktop.policykit.exec.path` annotation. The client's `default_helper_path()` and the policy's
annotation are two separate declarations of the same fact, in two files, in two languages.

If they drift, `pkexec` finds no matching action and falls back to `org.freedesktop.policykit.exec` —
which **still authenticates, still runs the helper, and still works**. What is lost is invisible from
the code:

- The prompt stops saying *"Authentication is required to inspect and reclaim system storage"* and says
  *"run a program as another user"* instead.
- `auth_admin_keep` stops applying, so a user is asked for a password **once per operation** rather than
  once per batch — reintroducing exactly the Stacer behaviour the single-session helper exists to avoid.

A failure that leaves the feature working is a failure nobody reports.

**Resolved** — nothing was broken, so this is a guard rather than a fix. Two tests read the policy file
and assert it against the compiled-in path and against `auth_admin_keep` for the active session.
**Verified to fire** by changing the annotation to `/usr/lib/nix/nix-helper`, which produces the
message naming both paths and what would be lost.

Also corrected while there: `pkaction --verbose` showed the description as *"Manage system storage and
services"*, and the helper has no service operation — Phase 4 is not built. A privilege statement that
over-claims is worse than a vague one, and widening it later should be a visible diff that forces a
reinstall rather than something granted in advance.

---

## 4. A unit test removed a real kernel from a real machine

**M5** · **Critical** · **Found by** the user's machine doing it

The worst thing that has happened in this project, and entirely self-inflicted.

`Elevation` derived `Default`, and `Elevation::default()` escalated through polkit on first need. Two
unit tests called it, expecting elevation to fail because no helper is installed during development —
which had always been true.

Then the helper *was* installed, at the shipped path, to exercise the `pkexec` path manually. That
worked. And `auth_admin_keep` — the policy setting that makes one prompt cover a whole batch, and the
specific fix for Stacer prompting per action — had cached the authorisation from that prompt.

So the next `make check` escalated **silently**. The test's fixture named
`linux-image-6.8.0-136-generic`, copied from the machine's own kernel list when the test was written.
The helper's derivation was asked whether that was a removable old kernel, correctly answered yes, and
ran `apt-get remove --purge -y`.

```
DESTRUCTIVE id=2 op=remove_packages detail=RemovePackages { kind: OldKernel,
             packages: ["linux-image-6.8.0-136-generic"] }
```

**Every safety rule held.** The helper refused nothing it should have allowed and allowed nothing
outside its own derivation; the running kernel and the newest were protected; the second attempt was
correctly denied once the package was gone. The machine kept its running kernel and a fallback, and
stayed bootable. The audit log recorded it precisely, which is how it was found — within minutes, and
by reading the trail rather than by noticing a symptom.

None of that makes it acceptable. Something was removed from someone's computer without their consent.

**Three causes, and each got its own fix:**

1. **Reaching root took no deliberate act.** `Elevation` no longer implements `Default`. There are two
   constructors, `production()` and `never()`, and `never()` is `cfg(test)` — so in a release build the
   escalating one is the *only* one that exists, and in a test build a caller has to write the word
   `production` to reach polkit. Compile-time, not vigilance.
2. **A fixture named a package that existed.** Every test package name is now obviously synthetic —
   `nix-test-not-a-real-kernel`. This layer was then proven to work by accident: while verifying the
   first fix I disabled the guard on the same machine, and the run escalated again — and was refused,
   because the name was fake. Defence in depth is not a slogan.
3. **The suite was not safe to run on a machine with the helper installed.** `make test` now sets
   `NIX_HELPER_PATH` to a path that cannot exist, so even a test that asked to escalate finds nothing
   to launch. Belt and braces: if that line is what saves you, two guards have already failed.

**Guard.** `no_privileged_operation_can_execute_under_test_elevation` runs every helper-backed method
through `reclaim_one` with `Elevation::never()` and requires each to fail at *elevation* specifically,
not somewhere further along. `refusing_elevation_never_opens_a_session` asserts no child process is
started even when a caller retries. The full suite now runs clean on this machine with the helper still
installed and starts **zero** helper sessions.

Deliberately **not** verified by breaking the guard and watching a test fail, which is this project's
usual practice — see [09-patterns.md §12](09-patterns.md). Disabling an escalation guard on a machine
with a live helper is how the kernel was lost, and repeating it to prove a point would be
indefensible. The behaviour is asserted where it lives instead. Some guards cannot be tested by
sabotage, and recognising which is part of using the technique.

---

## 5. Snap revision strings reach a root command line

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

---

## 6. `apt-get -s remove bash` exits zero

**Phase 5** · **Critical** · **Found by** running against this machine

`PKG-2` shows what a removal would do before doing it, and the plan was to trust the package manager's
own simulation — which is right, since guessing at dependency resolution would be both wrong and
dangerous. What was not checked until it was run is what the simulation *says* about danger.

```
$ apt-get -s remove bash; echo "exit=$?"
The following packages will be REMOVED:
  aznfs bash gdm3 mysql-apt-config ubuntu-desktop ubuntu-desktop-minimal
WARNING: The following essential packages will be removed.
This should NOT be done unless you know exactly what you are doing!
  bash
0 upgraded, 0 newly installed, 6 to remove and 19 not upgraded.
exit=0
```

Six packages including the shell, the display manager and the desktop, and it **succeeds**. The warning
is prose on stdout; the exit status is zero. A tool that renders the removal list and enables its button
on a successful simulation has just offered to destroy the machine, and it will look like it is working
correctly right up to the moment someone clicks.

(A real `apt-get remove -y bash` would refuse, because removing an essential package needs
`--allow-remove-essential` or a typed confirmation phrase. That is a backstop for the *execution*. It
does nothing about the interface, which is where the user makes the decision.)

**Resolved** by classifying the cascade in nix rather than reading apt's prose. The signals come from
dpkg's own metadata — `Essential`, `Priority` — batched into one query over whatever the simulation says
will be removed. Essential, `Priority: required`, and the running kernel are **refused**; `important` is
allowed behind a deliberate confirmation.

Metadata alone was not enough for the criterion the specification actually names — "a removal that would
take out a desktop environment is flagged prominently":

```
gdm3            optional  no
gnome-shell     optional  no
ubuntu-desktop  optional  no
xserver-xorg    optional  no
```

Every one of them `optional`. Priority describes what the base system needs, not what the user will
miss. So there is one extra signal, and it is resolved from the machine rather than guessed: the package
owning the binary named in `/etc/X11/default-display-manager`, via `dpkg -S`. On this machine that is
`gdm3`, and it stays right on a system running a display manager nobody thought to put in a list.

**And the same rules again, in the helper.** `Op::RemoveSelected` is the first destructive operation
whose validation cannot be "re-derive the eligible set and refuse anything outside it" — the eligible
set *is* the user's choice, and validating input against itself is not validation. What it does instead
is stated on the op rather than implied: every name must be an installed package, which is what keeps
`--allow-remove-essential`, `-y`, `../../etc/passwd` and `bash; rm -rf /` out of the argument list; and
the helper runs its **own** simulation and applies its **own** copy of the classification, so the
refusal does not depend on the preview the user was shown being honest.

**Guard.** `this_machine_refuses_a_removal_that_would_take_the_system_with_it` runs the real thing
against the real database and asserts `Refused`, and `this_machine_allows_removing_something_harmless`
asserts the opposite for a font package chosen from the inventory — a classifier that refuses everything
would pass the first test alone. Both were checked with a printout to confirm they actually reached
their assertions rather than returning early, since both begin with a `let Ok(…) else { return }`.

On the helper side, four tests drive `remove_selected` with hostile names and with `bash`, and assert
the refusal each time; the `bash` one asserts the message says *essential*, because a refusal from the
installed-package lookup would have meant the lookup was broken and the test was passing for the wrong
reason.

**Deliberately not guarded:** the success path. It is unreachable from any test here, and must stay
that way — a test that got as far as `apt-get remove` would remove a package from whatever machine ran
it, which this project has already done once (§5). It is on the isolated-VM list in
[`PLAN.md` §9.1](../PLAN.md).
