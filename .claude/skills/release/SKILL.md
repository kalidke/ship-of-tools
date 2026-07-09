---
name: release
description: Cut a Ship of Tools release — stamp the product version everywhere, tag, and let CI build+publish the GitHub Release (ADR 0030 Phase B). Activates for "cut a release", "release vX.Y.Z", "tag a release", "publish a release", "new release" in the ship-of-tools repo.
---

# release — cut a Ship of Tools release

One product version across all components (ADR 0030 §1): the release unit is
the whole ship, released as a git tag that CI turns into platform binaries +
a published GitHub Release (installs clone the repo at the tag — the julia
bundle is retired, ADR 0030 amendment 2026-07-04). Read
`docs/adr/0030-versioning-release-and-auto-update.md` §1–3 before your first
release from a fresh context.

## Preflight (do all four, report anything amiss instead of proceeding)

1. **main is green**: latest `Rust` workflow run on main succeeded
   (`gh run list --workflow Rust --limit 1`), and check the Julia `CI`
   workflow too — green since 2026-07-02 (the docs-job fix); a red Julia CI
   is no longer expected and should be investigated, though only the Rust
   gate hard-blocks a release.
2. **Tree clean + synced**: `git status --porcelain` empty, HEAD ==
   origin/main. Coordinate over sot-comm if an FE session announced pending
   pushes (sync-before-push convention).
3. **Pick the version**: semver, pre-1.0 (minor = anything may change).
   Check `git tag -l` for the last tag; if no public-track tags exist, use the
   current product version as the baseline. Prereleases use `-rc.N`
   (auto-marked prerelease on the GitHub Release by the workflow).
4. **Private ops handoff current** — `<ops>/STATUS.md` and `<ops>/TODO.md`
   should not be stale when cutting the release.

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

## Pipeline validation without a tag

`gh workflow run Release` (workflow_dispatch) runs build + julia-check with a
`0.0.0-ci.<sha>` version and SKIPS publish — use it after editing the workflow
or before a first-of-its-kind release.

## Hard rules

- Releases are cut from **main only**; the script enforces clean-tree +
  HEAD==origin/main + tag-not-exists.
- **Never** delete/re-cut a published tag that anyone may have fetched — cut a
  patch release instead. A broken `-rc.N` may be deleted (tag + release) since
  rc consumers are just us.
- Publishing visibility and release availability are maintainer-gated. **Never
  treat a local tag or release procedure as permission to publish the repo.**
