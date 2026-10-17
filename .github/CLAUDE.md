# .github/: CI and the tag gate (distribution)

The workflows that verify a push to `main` and turn a `v*` tag into published release artifacts. CI is the tag gate, not
a pull-request gate: nothing here runs on a pull request except the secret scan. Part of distribution; charter:
scripts/CLAUDE.md.

## Files
- `dependabot.yml`: weekly version updates for the GitHub Actions and the Julia environments (`/`, `/docs`, `/test`).
- `wgl-depot.sh`: builds the Julia depot the WGL page test reads (rust.yml's `test` job and shard `sotd`); its
  comment says why it is built over Julia's bundled depots.
- `workflows/`: the four workflows below.

## Workflows
- `workflows/rust.yml` ("Rust"): push to `main` (paths `rust/**`, `scripts/**`, `docs/tools/**`, `comm/**`, `agents/**`, `.github/wgl-depot.sh` and the file
  itself) and dispatch. Jobs: `test` (build and test on windows and macos, after `npm ci` for the MathJax helper's modules; on windows the
  PowerShell 5.1 parse and the `scripts/tests/` `.ps1` suites; on macos "Comm and agents suites under /bin/bash 3.2",
  `scripts/tests/under-bash.sh /bin/bash` over every hermetic suite, run by Homebrew's bash, which the
  step installs: the macOS image's only bash is /bin/bash 3.2), `test-linux` (the ubuntu build and test, one job per
  shard, named "build+test (ubuntu-latest, <shard>)": its `SHARDS` table names the test targets of the shards `outage`,
  `capsule`, `wake` and `sotd`, each target whole or split once as `X` and `--skip X`; the shard `rest` runs every other
  test executable cargo builds, then the doc tests, so no row names `rest`; the matrix lists the table's shards in the
  order they first appear, then `rest`, and before it runs anything, every shard fails unless no row names `rest`,
  every split is such a pair and the matrix runs that many jobs, and a job fails unless its shard is the one at its
  place (`strategy.job-index`) in that list;
  every command selects `--workspace`; only `sotd` runs `npm ci`
  and the WGL depot step, and only `rest` saves the shared cache), `checks-linux` ("checks (ubuntu-latest)": the clippy
  gates, the selected-body proofs, the L3 run, the shell parse, "Check the layout"
  (`scripts/tests/check-layout.sh` with `check-layout.allow`), "Test the layout tools" (its two self-tests), the
  `scripts/tests/` bash suites, the comm hermetic suites and the agents CLI suites (the comm list names
  `test-stage-bin.sh`; among them `test-ccx-launch.sh`, which proves ccx's default handle)), `conpty-windows-2022`
  (ConPTY and capsule tests), `p2-e2e` (the SDK helper, offline), `fresh-install-smoke` (a `--be-only` install of the
  latest published tag into a clean container), `bash32-linux` ("scripts under bash 3.2.57 (ubuntu-latest)": builds GNU
  bash 3.2.57 from its tarball, sha256 pinned, and runs `scripts/tests/under-bash.sh` with it over every hermetic
  suite). The step "Test native harness helpers" in `checks-linux` runs the
  feature-gated sot binary's eight display-free native harness regression bodies through
  scripts/tests/run-filtered-tests.sh; native window acceptance remains in the dedicated hosted jobs.
  The step "Test selected Rust bodies" runs the portable real-libtest
  shell proofs in `test` on windows and macos and in `checks-linux` on ubuntu; it invokes no daemon or peer suite. On
  ubuntu it also runs the candidate gate's finite selected-job and runtime-listing proofs and test-filtered-tests.sh,
  the group runner's real-libtest controls and mutation table; the full candidate gate is not invoked by that proof
  step. The step "Test L3 storage exhaustion" in `checks-linux` makes a private 64 MiB ext4
  loop volume, runs `fault_storage` on it with `--include-ignored`, then unmounts it, releases the loop device and
  removes its folder; sudo is used only in that step, and a failed setup or teardown fails the job.
  Every leg of `test` and every shard of `test-linux` installs Julia 1.13 (`julia-actions/setup-julia`); in `test`
  before "Test workspace", and in the shard `sotd`, the step "Prepare the Julia depot the WGL page test reads" runs
  `wgl-depot.sh`: it adds WGLMakie 0.13, precompiled, to a depot in the runner's temporary folder, built over Julia's
  bundled depots, and names that same list in `JULIA_DEPOT_PATH` for the rest of the job: the sidecar tests start that
  Julia, and the WGL page test adds WGLMakie from that depot offline, compiling the shim and WGLMakie (whose precompile workload the test turns off, a setting its cache is keyed on). The Linux sidecar tests
  also start Xvfb, which the ubuntu-24.04 image ships; a host without it fails them as setup (`executable`).
  The heartbeat context-deadline suite runs independently on Ubuntu, macOS and Windows Git Bash. Its per-behavior and sensitivity receipts distinguish fixture entry, actual release times, hook exit, both EOFs and positive lifetime cleanup; MSYS budget coverage remains separate from native Python P5 and its termination acceptance gate.
  window-close-windows and window-close-macos run the opt-in main-thread native window_close suite on hosted Windows and macOS; a missing body, native window or required observation is not a passing result. Ordinary native close must exit 0 before 2.5 seconds without the backstop; deliberate stalled teardown must end under the three-second backstop with the decided code.
  window-minimized-windows and window-minimized-macos run the native ten-minute minimized-window event-progress check on hosted Windows and macOS; a runner without a usable native window is not a passing result. Its visible phase starts with the window's thread held for 600 ms. window-minimized-macos-mutation runs the same target with the feature `test-mac-redraw-mutation`, whose transport wakes the window by a `Window` call from its own thread, as before MAC-REDRAW; the job passes only if it prints `mac-redraw-mutation visible failures=` naming `workload_rate`, which is MAC-REDRAW's red shown in the same run. The minimized, mutation and close jobs each allow 25 minutes, their worst case with every case or child run.
  pane-timing-windows and pane-timing-macos build sotd and sot-capsule, then run the opt-in native pane_timing suite on hosted Windows and macOS on its stand-in route: fifteen cold Ready attaches over an `ssh:<hub>/<host>` dial whose ssh is a `sotd stdio-bridge` stand-in: the twelve made in a running window each within `CONNECT_BOUND` from the request to the presented frame, and each child's first, its window's first pane, recorded apart and held only to the driver's 30 s wait, three absent-supervisor attaches recorded apart, and one frame failed before present that completes nothing. The real two-login relayed route runs locally on a Linux host with sshd. A runner without a usable native window is not a passing result.
- `workflows/CI.yml` ("CI"): push to `main` (paths `core/**`, `julia/**`, `docs/**`, `src/**`, `test/**`, `Project.toml`,
  `Manifest.toml` and the file itself) and dispatch. Jobs: `test` (the root package on Julia 1.12 and pre-release),
  `julia-packages` (core, kernel, repl, the two preview plugins and SotLog), `docs` (builds the manual with
  `docs/make.jl`; a push to `main` deploys it).
- `workflows/release.yml` ("Release"): a `v*` tag or dispatch (`publish` runs only when the ref is a tag). Jobs: `build`
  (linux, windows and macos targets; the Linux `sotd` and `sot-capsule` are built musl static, with Ubuntu's
  `musl-tools` installed first as the C compiler for sot-log's native launcher), `julia-check`, the three smokes
  `smoke`, `smoke-windows` and `smoke-macos`, then `publish` (`SHA256SUMS`, `COMMIT`, git-cliff notes, the GitHub
  Release). The notes are cut at a whole line below 120000 bytes, with a link to every change since the previous
  tag, because GitHub rejects a release body over 125000 characters.
- `workflows/gitleaks.yml`: a full-history secret scan on push to `main` and on pull requests.

## Start here
`workflows/rust.yml` to add a suite (its steps are named); `workflows/release.yml` for what a release holds.

## Rules
- W1 trust preparation runs on hosted Windows and macOS against temporary files. P0/P1/P5 are proof limits closed by the human
  release done test; no Claude credential is provisioned in CI. Missing facilities or witnesses are not passes.
- CI is the tag gate: `scripts/release.sh` refuses a cut unless the latest non-skipped `rust.yml` and `CI.yml` runs on
  the branch being cut are green and at HEAD; dispatch them there first (`gh workflow run <workflow> --ref <branch>`),
  on a `fixes/*` or `rc/*` branch and on main too, since push runs are path-filtered and the cut's runs are dispatched ones.
- A new exception to the layout limits is a line in `scripts/tests/check-layout.allow` with its reason, in the commit that
  needs it.
- A suite runs only if a step names it: add a new suite to `rust.yml` (or `CI.yml` for Julia) by name, in the commit
  that adds it.
- Every job has a `timeout-minutes` of its own, a few times its usual length: without one, a hung step holds its runner
  for GitHub's default of 360 minutes.
- Function length is gated: the `rust.yml` step "Function length and test children" runs clippy's `too_many_lines` (more
  than 100 code lines) as an error, `vt100-ctt` excluded because it denies `clippy::all` in its own source. A function
  over the limit carries `#[allow(clippy::too_many_lines, reason = "...")]`. The step also reads the feature-gated
  native targets (test-pane-timing, test-window-close). The same run reads `rust/clippy-tests/clippy.toml`
  (`CLIPPY_CONF_DIR`), not `rust/clippy.toml`, and fails on a test that runs a child through `Command::output` or
  `status` or `Child::wait_with_output` (std or tokio) instead of `sot_log::test_wait`; on checks-linux it adds
  `daemon-lifetime-faults`, whose Linux modules hold such tests. "Check the layout" pins how many such allows `rust/`
  holds (not `rust/vt100`): the `too-many-lines` line of `scripts/tests/check-layout.allow`; a rise fails, and removing
  one means lowering that number in the same commit. It also fails on the text forms that loosen the lint without moving
  the count, exactly those `scripts/tests/CLAUDE.md` lists; a form not listed there is not caught.
- The build+test jobs' "Test workspace" and "Test shard" run every test binary through
  `scripts/tests/test-binary-bound.py` (`CARGO_TARGET_<triple>_RUNNER`): a binary still running at 900 s is killed, the
  tests still running are named, and `--no-fail-fast` runs the rest.
- Disallowed methods are gated: the `rust.yml` step "Disallowed methods" runs clippy's `disallowed_methods` over every
  library, binary and build script on all three platforms. `rust/clippy.toml` holds one array in labelled groups, each
  opening with its rule. A sanctioned site carries `#[allow(clippy::disallowed_methods, reason = "...")]` on the one
  statement that calls the method; a new method joins its group, and a new rule is a new group in the same array. Like
  the other clippy steps it runs on main and on demand, not on every branch push, so a lane merge gate runs the same
  line (`cargo clippy
  --workspace --exclude vt100-ctt --lib --bins --locked -- -A clippy::all -D clippy::disallowed_methods`) on the merged
  tree before the merge.
- The daemon-lifetime harness (`rust/backend/tests/daemon_lifetime`) has its own jobs: "daemon lifetime harness (Linux)"
  runs every case with Julia 1.12 and Quarto 1.9.38 installed and the capsule built with its phase barriers, and "daemon
  lifetime premises (macOS)" runs the premises on real children; neither is part of the plain runs of `test` and `test-linux`, which
  build the harness without its fault feature. Before the harness, the Linux job installs and compiles Pluto's
  environment and Quarto's Julia runner into the runner's depot once (one notebook run, one document rendered), and
  hands the cases those environments through `SOT_L2_PLUTO_MANIFEST` and `QUARTO_JULIA_PROJECT`.
- The Windows containment tests that need julia, Git for Windows' bash or a job around the test process are `#[ignore]`
  in `cargo test`; the job "containment, ignored tests (windows-latest)" runs exactly those (`--ignored`, filter
  `lifecycle::contain::tests::`) with julia installed, through scripts/tests/run-filtered-tests.sh with their count, so a
  selection that runs any other number of bodies fails. A commit that changes how many `#[ignore]` tests that module
  holds, or the module's path, changes the step's count or filter with it (rust/backend/src/lifecycle/contain.rs).
- The `paths:` filters decide which pushes run `rust.yml` and `CI.yml`; a new top-level code folder joins the filter of
  the workflow that tests it. Both workflows skip a commit whose message starts `release: v`.
- Release jobs pin every action by commit SHA, and only `publish` gets `contents: write`: the release is the updater's
  trust root (`workflows/release.yml`).
- `publish` needs `build`, the three smokes and `julia-check`; the Linux smoke boots the artifact in an ubuntu 20.04
  container to prove the old-glibc floor.
