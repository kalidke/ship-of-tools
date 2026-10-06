# .github/: CI and the tag gate (distribution)

The workflows that verify a push to `main` and turn a `v*` tag into published release artifacts. CI is the tag gate, not
a pull-request gate: nothing here runs on a pull request except the secret scan. Part of distribution; charter:
scripts/CLAUDE.md.

## Files
- `dependabot.yml`: weekly version updates for the GitHub Actions and the Julia environments (`/`, `/docs`, `/test`).
- `workflows/`: the four workflows below.

## Workflows
- `workflows/rust.yml` ("Rust"): push to `main` (paths `rust/**`, `scripts/**`, `docs/tools/**`, `comm/**`, `agents/**` and the file
  itself) and dispatch. Jobs: `test` (build and test on ubuntu, windows and macos, the PowerShell 5.1 parse and the
  `scripts/tests/` suites on their legs, the comm hermetic suites on ubuntu, and on ubuntu the steps "Check the layout"
  (`scripts/tests/check-layout.sh` with `check-layout.allow`) and "Test the layout tools" (its two self-tests)), `conpty-windows-2022` (ConPTY and capsule
  tests), `p2-e2e` (the SDK helper, offline), `fresh-install-smoke` (a `--be-only` install of the latest published tag
  into a clean container).
- `workflows/CI.yml` ("CI"): push to `main` (paths `core/**`, `julia/**`, `docs/**`, `src/**`, `test/**`, `Project.toml`,
  `Manifest.toml` and the file itself) and dispatch. Jobs: `test` (the root package on Julia 1.12 and pre-release),
  `julia-packages` (core, kernel, repl, the two preview plugins and SotLog), `docs` (builds the manual with
  `docs/make.jl`; a push to `main` deploys it).
- `workflows/release.yml` ("Release"): a `v*` tag or dispatch (`publish` runs only when the ref is a tag). Jobs: `build`
  (linux, windows and macos targets; the Linux `sotd` and `sot-capsule` are built musl static), `julia-check`, the three
  smokes `smoke`, `smoke-windows` and `smoke-macos`, then `publish` (`SHA256SUMS`, `COMMIT`, git-cliff notes, the GitHub
  Release).
- `workflows/gitleaks.yml`: a full-history secret scan on push to `main` and on pull requests.

## Start here
`workflows/rust.yml` to add a suite (its steps are named); `workflows/release.yml` for what a release holds.

## Rules
- W1 trust preparation runs on hosted Windows and macOS against temporary files. P0/P1/P5 are proof limits closed by the human
  release done test; no Claude credential is provisioned in CI. Missing facilities or witnesses are not passes.
- CI is the tag gate: `scripts/release.sh` refuses a cut unless the latest non-skipped `rust.yml` and `CI.yml` runs on
  the branch being cut are green and in HEAD's history; a `fixes/*` or `rc/*` branch has runs only if they are
  dispatched there (`gh workflow run <workflow> --ref <branch>`).
- A new exception to the layout limits is a line in `scripts/tests/check-layout.allow` with its reason, in the commit that
  needs it.
- A suite runs only if a step names it: add a new suite to `rust.yml` (or `CI.yml` for Julia) by name, in the commit
  that adds it.
- Function length is gated: the `rust.yml` step "Function length" runs clippy's `too_many_lines` (more than 100 code
  lines) as an error, `vt100-ctt` excluded because it denies `clippy::all` in its own source. A function over the limit
  carries `#[allow(clippy::too_many_lines, reason = "...")]`, and the step "Function length allowances can only fall"
  pins how many such allows `rust/` holds (not `rust/vt100`); removing one means lowering that number in the same commit.
- Disallowed methods are gated: the `rust.yml` step "Disallowed methods" runs clippy's `disallowed_methods` over every
  library, binary and build script on all three legs. `rust/clippy.toml` holds one array in labelled groups, each
  opening with its rule. A sanctioned site carries `#[allow(clippy::disallowed_methods, reason = "...")]` on the one
  statement that calls the method; a new method joins its group, and a new rule is a new group in the same array. Like
  the other clippy steps it runs on main and on demand, not on every branch push, so a lane merge gate runs the same
  line (`cargo clippy
  --workspace --exclude vt100-ctt --lib --bins --locked -- -A clippy::all -D clippy::disallowed_methods`) on the merged
  tree before the merge.
- The Windows containment tests that need julia, Git for Windows' bash or a job around the test process are `#[ignore]`
  in `cargo test`; the job "containment, ignored tests (windows-latest)" runs them with julia installed
  (rust/backend/src/lifecycle/contain.rs).
- The `paths:` filters decide which pushes run `rust.yml` and `CI.yml`; a new top-level code folder joins the filter of
  the workflow that tests it. Both workflows skip a commit whose message starts `release: v`.
- Release jobs pin every action by commit SHA, and only `publish` gets `contents: write`: the release is the updater's
  trust root (`workflows/release.yml`).
- `publish` needs `build`, the three smokes and `julia-check`; the Linux smoke boots the artifact in an ubuntu 20.04
  container to prove the old-glibc floor.
