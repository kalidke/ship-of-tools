---
name: release
description: Cut a Ship of Tools release — stamp the product version everywhere, tag, and let CI build+publish the GitHub Release (ADR 0030 Phase B). Activates for "cut a release", "release vX.Y.Z", "tag a release", "publish a release".
---

# release — cut a Ship of Tools release

One product version across all components (ADR 0030 §1): the release unit is
the whole ship, released as a git tag that CI turns into platform binaries +
a published GitHub Release (installs clone the repo at the tag — there is no
separate julia bundle). Read
`docs/adr/0030-versioning-release-and-auto-update.md` §1–3 before your first
release from a fresh context.

## Preflight (do all five, report anything amiss instead of proceeding)

1. **main is green**: latest `Rust` workflow run on main succeeded
   (`gh run list --workflow Rust --limit 1`); check the Julia `CI` workflow
   too — a red Julia CI should be investigated, though only the Rust gate
   hard-blocks a release.
2. **Tree clean + synced**: `git status --porcelain` empty, HEAD ==
   origin/main. Coordinate over sot-comm if an FE session announced pending
   pushes (sync-before-push convention).
3. **Pick the version**: semver, pre-1.0 (minor = anything may change).
   Check `git tag -l` for the last tag; if no public-track tags exist, use the
   current product version as the baseline. Prereleases use `-rc.N`
   (auto-marked prerelease on the GitHub Release by the workflow).
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
4. **Private ops handoff current** — `<ops>/STATUS.md` and `<ops>/TODO.md`
   should not be stale when cutting the release.
5. **A candidate needs a named consumer** (decision 0013 as amended, after the
   owner asked why we were cutting tags nobody used). Cut a candidate only when
   **a named box or person installs it the same day and a named test runs on
   it** — write both down before you cut, not after. A candidate nobody has
   installed is not a milestone; it is a tag that cost CI minutes and told us
   nothing.
   - A defect found **before the current candidate has been used** rides into
     **that same candidate on the branch**, not into a new tag. Amend the
     branch and cut once.
   - The one exception is a test that genuinely needs a **newer published
     version** — an updater/downgrade check, say — named as such when you cut.
   - This bounds the "everything we know is wrong goes in the next rc" rule in
     *Hard rules* below; it never licenses parking a defect. Same candidate,
     later cut — never a later line.

## Cut it

```bash
scripts/release.sh <X.Y.Z> --dry-run   # inspect the stamp diff first
scripts/release.sh <X.Y.Z> --yes       # stamp + test + commit + tag + push
```

The script stamps `rust/Cargo.toml [workspace.package]` + every Julia
`Project.toml`, refreshes `Cargo.lock`, regenerates `CHANGELOG.md` iff
git-cliff is installed (optional — CI generates the release notes), runs
`cargo test --workspace --locked`, commits `release: vX.Y.Z`, tags, pushes.

## Watch + verify (required — a tag that half-published is worse than none)

1. `gh run watch` the `Release` workflow (or poll
   `gh run list --workflow Release --limit 1` in a background monitor).
   **All three platform legs are blocking, macOS included** (`release.yml`
   sets `experimental: false` everywhere and `publish` requires
   `smoke-macos`) — a macOS failure fails the release; fix or revert, don't
   wave it through.
2. On success, verify the release assets:
   `gh release view vX.Y.Z` must list `sot-<ver>-linux-x86_64.tar.gz`,
   `sot-<ver>-windows-x86_64.zip`, `sot-<ver>-macos-aarch64.tar.gz`,
   `SHA256SUMS`. No julia bundle — installs clone the repo at the tag; the
   release-blocking `julia-check` job proves the envs resolve + load at this
   ref.
3. Announce: `/bus-note` + a sot-comm broadcast so fleet sessions know a
   release landed (they stay on dev builds; this is for awareness).
4. **Close the loop on the named consumer**: the publish report says **who
   installed the tag and what the test showed**. A candidate that was cut and
   never installed is reported as exactly that, on the same day — an unclosed
   loop is the finding, not an omission to leave quiet.

## Pipeline validation without a tag

`gh workflow run Release` (workflow_dispatch) runs build + julia-check with a
`0.0.0-ci.<sha>` version and SKIPS publish — use it after editing the workflow
or before a first-of-its-kind release.

## Hard rules

- **Everything we know is wrong goes in the next rc or release.** The owner's
  standing rule, verbatim, 2026-09-26. A known defect is never parked in a
  later line to keep a candidate tidy: the next candidate carries every open
  defect, and the only legitimate reasons to hold one back are that two fixes
  contend for the same file, or that a fix rests on an unproven root cause and
  a cheaper diagnostic ships first. "Small", "not a blocker", and "it can wait"
  are not reasons. Say the split out loud with its reason per item, and put
  features — not defects — in the later candidate.
- **Finals come from main; candidates come from the line's candidate branch.**
  A tag starts the next line's branch, candidates are tagged from that branch,
  and the final is cut from main after the branch merges in. The `main only`
  wording below applies to the final. Before any final, prove main actually
  carries what the last tested candidate carried — by content, not by ancestry.
- Releases are cut from **main only**; the script enforces clean-tree +
  HEAD==origin/main + tag-not-exists.
- **Never** delete/re-cut a published tag that anyone may have fetched — cut a
  patch release instead. A broken `-rc.N` may be deleted (tag + release) since
  rc consumers are just us.
- Publishing visibility and release availability are maintainer-gated. **Never
  treat a local tag or release procedure as permission to publish the repo.**
