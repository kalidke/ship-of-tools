# .github/: CI and the tag gate (distribution)

The workflows that verify a push to `main` and turn a `v*` tag into published release artifacts. CI is the tag gate, not
a pull-request gate: nothing here runs on a pull request except the secret scan. Part of distribution; charter:
scripts/CLAUDE.md.

## Files
- `dependabot.yml`: weekly version updates for the GitHub Actions and the Julia environments (`/`, `/docs`, `/test`).
- `workflows/`: the four workflows below.

## Workflows
- `workflows/rust.yml` ("Rust"): push to `main` (paths `rust/**`, `scripts/**`, `docs/tools/**`, `comm/**`, `agents/**` and the file
  itself) and dispatch. Jobs: `test` (build and test on windows and macos, after `npm ci` for the MathJax helper's modules, and on windows the
  PowerShell 5.1 parse and the `scripts/tests/` `.ps1` suites), `test-linux` (the ubuntu build and test, one job per
  shard, named "build+test (ubuntu-latest, <shard>)": its `SHARDS` table names the test targets of the shards `outage`,
  `capsule`, `wake` and `sotd`, each target whole or split once as `X` and `--skip X`; the shard `rest` runs every other
  test executable cargo builds, then the doc tests, so no row names `rest`; the matrix lists the table's shards in the
  order they first appear, then `rest`, and before it runs anything, every shard fails unless no row names `rest`,
  every split is such a pair and the matrix runs that many jobs, and a job fails unless its shard is the one at its
  place (`strategy.job-index`) in that list;
  every command selects `--workspace`; only `sotd` runs `npm ci`
  and the WGL depot step, and only `rest` saves the shared cache), `checks-linux` ("checks (ubuntu-latest)": the clippy
  gates and the allowance count, the selected-body proofs, the L3 run, the shell parse, "Check the layout"
  (`scripts/tests/check-layout.sh` with `check-layout.allow`), "Test the layout tools" (its two self-tests), the
  `scripts/tests/` bash suites, the comm hermetic suites and the agents CLI suites (the comm list names
  `test-stage-bin.sh`; among them `test-ccx-launch.sh`, which proves ccx's default handle)), `conpty-windows-2022`
  (ConPTY and capsule tests), `p2-e2e` (the SDK helper, offline), `fresh-install-smoke` (a `--be-only` install of the
  latest published tag into a clean container). The step "Test selected Rust bodies" runs the portable real-libtest
  shell proofs in `test` on windows and macos and in `checks-linux` on ubuntu; it invokes no daemon or peer suite. On
  ubuntu it also runs the candidate gate's finite selected-job and runtime-listing proofs; the full candidate gate is
  not invoked by that proof step. The step "Test L3 storage exhaustion" in `checks-linux` makes a private 64 MiB ext4
  loop volume, runs `fault_storage` on it with `--include-ignored`, then unmounts it, releases the loop device and
  removes its folder; sudo is used only in that step, and a failed setup or teardown fails the job.
  Every leg of `test` and every shard of `test-linux` installs Julia 1.13 (`julia-actions/setup-julia`); in `test`
  before "Test workspace", and in the shard `sotd`, the step "Prepare the Julia depot the WGL page test reads" adds
  WGLMakie 0.13, precompiled, to a depot in the runner's temporary folder that `JULIA_DEPOT_PATH` names for the rest of
  the job: the sidecar tests start that Julia, and the WGL page test adds WGLMakie from that depot offline. The step
  builds the depot over Julia's bundled depots and fails if the depot holds a compiled Pkg of its own, so the page
  test's add reuses its caches and compiles only the shim.
  The heartbeat context-deadline suite runs independently on Ubuntu, macOS and Windows Git Bash. Its per-behavior and sensitivity receipts distinguish fixture entry, actual release times, hook exit, both EOFs and positive lifetime cleanup; MSYS budget coverage remains separate from native Python P5 and its termination acceptance gate.
  window-close-windows and window-close-macos run the opt-in main-thread native window_close suite on hosted Windows and macOS; a missing body, native window or required observation is not a passing result. Ordinary native close must exit 0 before 2.5 seconds without the backstop; deliberate stalled teardown must end under the three-second backstop with the decided code.
  window-minimized-windows and window-minimized-macos run the native ten-minute minimized-window event-progress check on hosted Windows and macOS; a runner without a usable native window is not a passing result.
  pane-timing-windows and pane-timing-macos build sotd and sot-capsule, then run the opt-in native pane_timing suite on hosted Windows and macOS on its stand-in route: fifteen cold Ready attaches over an `ssh:<hub>/<host>` dial whose ssh is a `sotd stdio-bridge` stand-in, each within `CONNECT_BOUND` from the request to the presented frame, three absent-supervisor attaches recorded apart, and one frame failed before present that completes nothing. The real two-login relayed route runs locally on a Linux host with sshd. A runner without a usable native window is not a passing result.
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
  pins how many such allows `rust/` holds (not `rust/vt100`); removing one means lowering that number in the same commit. It also reads the feature-gated native targets (test-pane-timing, test-window-close).
- Disallowed methods are gated: the `rust.yml` step "Disallowed methods" runs clippy's `disallowed_methods` over every
  library, binary and build script on all three platforms. `rust/clippy.toml` holds one array in labelled groups, each
  opening with its rule. A sanctioned site carries `#[allow(clippy::disallowed_methods, reason = "...")]` on the one
  statement that calls the method; a new method joins its group, and a new rule is a new group in the same array. Like
  the other clippy steps it runs on main and on demand, not on every branch push, so a lane merge gate runs the same
  line (`cargo clippy
  --workspace --exclude vt100-ctt --lib --bins --locked -- -A clippy::all -D clippy::disallowed_methods`) on the merged
  tree before the merge.
- The daemon-lifetime harness (`rust/backend/tests/daemon_lifetime`) has its own jobs: "daemon lifetime harness (Linux)"
  runs every case with Julia 1.12 and Quarto 1.7.31 installed and the capsule built with its phase barriers, and "daemon
  lifetime premises (macOS)" runs the premises on real children; neither is part of the plain runs of `test` and `test-linux`, which
  build the harness without its fault feature. Before the harness, the Linux job installs and compiles Pluto's
  environment and Quarto's Julia runner into the runner's depot once (one notebook run, one document rendered), and
  hands the cases those environments through `SOT_L2_PLUTO_MANIFEST` and `QUARTO_JULIA_PROJECT`.
- The Windows containment tests that need julia, Git for Windows' bash or a job around the test process are `#[ignore]`
  in `cargo test`; the job "containment, ignored tests (windows-latest)" runs them with julia installed
  (rust/backend/src/lifecycle/contain.rs).
- The `paths:` filters decide which pushes run `rust.yml` and `CI.yml`; a new top-level code folder joins the filter of
  the workflow that tests it. Both workflows skip a commit whose message starts `release: v`.
- Release jobs pin every action by commit SHA, and only `publish` gets `contents: write`: the release is the updater's
  trust root (`workflows/release.yml`).
- `publish` needs `build`, the three smokes and `julia-check`; the Linux smoke boots the artifact in an ubuntu 20.04
  container to prove the old-glibc floor.
