---
name: release
description: Cut a Ship of Tools release — stamp the product version everywhere, tag, let CI build the GitHub Release as a draft, install the draft on a box and publish it on the maintainer's word (ADR 0030 Phase B). Activates for "cut a release", "release vX.Y.Z", "tag a release", "publish a release".
---

# release — cut a Ship of Tools release

One product version across all components (ADR 0030 §1): the release unit is
the whole ship, released as a git tag that CI turns into platform binaries +
a GitHub Release draft, which no updater lists until the maintainer publishes
it. The contract is `scripts/CLAUDE.md`, "Release path". Read
`docs/history/adr/0030-versioning-release-and-auto-update.md` §1–3 before your first
release from a fresh context.

Every tag and every release action (draft, publish, edit, delete) runs only on
the maintainer's word. The cut and the draft follow from the word to cut; the
publish waits for a second word. From the word to a candidate installed on the
maintainer's workstation takes about 23 minutes (measured 2026-10-20: the dry
run and the cut under a minute, release.yml 21 minutes to the draft, the
download and the archive install under a minute; the same download has also
run at 53 to 140 KB/s, about 3 to 7 minutes for the Linux archive).

## Before the word (report anything amiss instead of proceeding)

1. **The local gate passed at this commit**: `scripts/tests/rc-gate.sh` and
   `scripts/tests/user-manager-tests.sh`, each run in its test container (their
   headers have the commands), each `summary.txt` naming HEAD's tree and ending
   `ALLDONE`. The cut runs no tests and reads no hosted run; it relies on this.
2. **Tree clean + synced**: `git status --porcelain` empty, HEAD == `origin/main`
   (`release.sh` refuses otherwise). Coordinate over sot-comm if a session
   announced pending pushes.
3. **Pick the version**: semver, pre-1.0 (minor = anything may change). Between tags the line carries the next
   version with `-dev`; a release is the commit that drops it, so the version is normally HEAD's stamp minus `-dev`
   (`release.sh` refuses a cut that sorts below the stamp).
   Prereleases use `-rc.N` (auto-marked prerelease on the GitHub Release by the workflow).
   **The dot is load-bearing, and the form is fixed for a whole X.Y.Z line.**
   The updater compares semver, so a dotted `rc.N` sorts numerically and has
   no ceiling, while an undotted `rcN` is one alphanumeric identifier compared
   as text: `rc10` sorts BELOW `rc7`, and the updater would silently stop
   offering it. Two rules follow, and the second is the one that bites:
   - A new line starts at `-rc.1`. Never `-rc1`.
   - Never switch form mid-line. On a line already tagged `rcN`, a `rc.N+1` is
     LOWER than every `rcN` before it (`rc` alone sorts below `rc7`), so the
     switch would publish a release nobody can update to.
   - **A line on `rcN` does NOT stop at `rc9`.** What sorts above `rc9` is
     `rc9.1`: equal prefix, and `cmp_pre` breaks a common prefix on identifier
     count, so more identifiers wins. From there the trailing identifier is
     numeric on both sides, so `rc9.2 > rc9.1` and `rc9.10 > rc9.9`, with no
     ceiling. Read from `rust/updater/src/semver.rs`; do not restate this rule
     from memory, and do not "correct" it back to a ceiling. (A dotted `rc.10`
     does still sort BELOW `rc9` — that is the trap in the bullet above.)
   - The 0.6.6 line is undotted (`rc1`…`rc9`, then `rc9.1`, `rc9.2`, …) and
     stays that way. 0.6.7 opens at `-rc.1`.
4. **Handoff current** — the cutting session's handoff (`dev/output/handoff-<handle>.md`)
   is current, and every defect known to the line is on the candidate's list.
5. **The comm matrix has run, if this candidate carries any comm change**:
   `comm/tests/comm-matrix.sh --boxes <a,b,c> [--expect <version>]`. The
   hermetic suites gate a merge; the matrix gates the CANDIDATE, because a
   sender's receipt and an actual delivery are different facts.
6. **A candidate needs a named consumer** (decision 0013 as amended). Cut a
   candidate only when **a named box or person installs it the same day and a
   named test runs on it** — write both down before you cut, not after.
   - A defect found **before the candidate is tagged** rides into **that same
     candidate**: it lands on main before the cut, so the tag carries it. A
     defect found **after the tag** goes into the **next rc**; a published rc
     is never re-cut.
   - The one exception is a test that genuinely needs a **newer published
     version** — an updater/downgrade check, say — named as such when you cut.

## Cut it (on the word)

```bash
scripts/release.sh <X.Y.Z> --dry-run   # the checks' evidence and the stamp diff
scripts/release.sh <X.Y.Z>             # asks before the push; --yes when the word was given for this version
```

Run it from the clean main checkout; it needs cargo (the order check builds the
updater's `vercmp` example), git and origin, nothing else. Each check prints
what it found: HEAD and origin's main, the highest tag on the line, the stamp,
and the stamp after the tag. The script stamps `rust/Cargo.toml [workspace.package]` + every Julia
`Project.toml`, refreshes `Cargo.lock`, commits `release: vX.Y.Z`, tags, then stamps the line's next dev version in a commit
of its own (`version: <next> opens the line after vX.Y.Z [skip ci]`) and pushes main and the tag in one `git push --atomic`:
origin takes both or neither.

A cut that fails before or at the push leaves only its local commits and tag, and nothing on origin: `git tag -d vX.Y.Z`
and `git reset --hard origin/main` undo them, then run it again. A push alone that failed (origin moved on) is
re-run with `git push --atomic origin main vX.Y.Z` once main is synced.

## Watch, install, publish

1. `gh run watch <id> --exit-status` the `Release` run the tag started (about
   20 minutes; the Windows build and its install smokes are the long pole).
   **Every leg is blocking, macOS included**, and so are the install smokes
   (`install-linux`, `install-macos`, `install-windows`): the `draft` job needs
   them all, and each runs the installer against this run's artifacts, fresh
   and as an update from the release before (`scripts/tests/install-smoke.sh`,
   `install-smoke.ps1`), and an `update-implicit` leg (the release before
   installed by `--version`, then the entry with no arguments, which must end
   as a repair run at that release that clears the pin, and the run's release
   staged afterwards must read `newer` and arm). A leg that fails is an
   installer or artifact defect to fix, not a flake. The smokes do not prove
   the Windows converge (a resident supervisor and a window) or the systemd
   path (the unit, `ExecStartPre`, the restart check); those stay proofs on a
   lab box.
2. On success, `gh release view vX.Y.Z` lists `sot-<ver>-linux-x86_64.tar.gz`,
   `sot-<ver>-windows-x86_64.zip`, `sot-<ver>-macos-aarch64.tar.gz`, `COMMIT`
   and `SHA256SUMS`, as a draft (`--json isDraft` reads `true`).
3. **Install the draft on the named box** by an archive run of its own files,
   which pins the box at this tag (a draft has no public download URL, so
   `--version` cannot reach it):
   ```bash
   gh release download vX.Y.Z -p SHA256SUMS -p COMMIT -p 'sot-*-<platform>*' -D <folder>
   scripts/install.sh --archive <folder>      # linux, macos
   scripts/install.ps1 -Archive <folder>      # windows
   ```
4. **Publish — on the maintainer's word, run alone.** Until this step no
   updater sees the release. Publish in its own command, with nothing chained
   after it:
   ```bash
   gh release edit vX.Y.Z --draft=false
   ```
   The publish report names the boxes that moved by archive run and the word
   that published it. A draft left unpublished is reported, as an unclosed
   loop is.
5. Announce: a sot-comm broadcast so fleet sessions know a release landed.
6. **Close the loop on the named consumer**: the report says **who installed
   the tag and what the test showed**. A candidate that was cut and never
   installed is reported as exactly that, on the same day.

## Pipeline validation without a tag

`gh workflow run release.yml --ref <branch>` runs build, the smokes, julia-check
and the install smokes with a `0.0.0-ci.<sha>` artifact name (the binaries carry
their real `+<sha>` version, which the install smokes read) and drafts nothing —
use it after editing the workflow or before a first-of-its-kind release.

## Hard rules

- **Everything we know is wrong goes in the next rc or release.** The owner's
  standing rule, verbatim, 2026-09-26. A known defect is never parked in a
  later line to keep a candidate tidy: the next candidate carries every open
  defect, and the only legitimate reasons to hold one back are that two fixes
  contend for the same file, or that a fix rests on an unproven root cause and
  a cheaper diagnostic ships first. Say the split out loud with its reason per
  item, and put features — not defects — in the later candidate.
- **Every tag is cut from main, candidates and finals alike.** Main is the
  -dev line, as the Julia packages' main is: work merges to main, a cut is
  tagged from main, and the stamp after the tag moves main to the next version
  with `-dev`. Before any final, prove main actually carries what the last
  tested candidate carried — by content, not by ancestry.
- `scripts/release.sh` enforces the branch (`main` only), a clean tree,
  HEAD == `origin/main`, and a tag that sorts above every tag on its line and
  above the line's stamp. It runs no tests and reads no hosted run.
- **Never** delete/re-cut a published tag that anyone may have fetched — cut a
  patch release instead. A broken candidate may be deleted (tag and release) and
  re-cut under the same number, on the maintainer's word, only while its release
  is still a draft that no box has installed by archive run. Once it is
  published, or any box has installed it that way, it is never re-cut: the fix
  goes into the next rc.
- Publishing visibility and release availability are maintainer-gated. **Never
  treat a local tag or release procedure as permission to publish the repo.**
