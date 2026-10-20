# scripts/tests/: suites for install, apply and launch (distribution)

Hermetic suites for the scripts in scripts/, and the local candidate gate. Each suite builds its prefix, home or stub
`sotd` under a temp folder and touches no live daemon. Part of distribution; charter: scripts/CLAUDE.md.

## Files
- `lib-test-body.sh`: the shared completion verdict for one selected Rust test; ordered, unambiguous capture closure precedes the outer result; missing, ignored, mismatched and failed bodies cannot pass.
- `test-test-body.sh`: real-libtest and scratch-witness proofs of the verdict and its shell owners, including combined captured-summary/truncation and scratch-path controls; accepts a compiled fixture executable by absolute path and validates its shell scratch root before cleanup.
- `run-filtered-tests.sh`: shared runner for a filtered libtest group. A usage error exits 2. It makes the log's
  folder before it runs the command, and exits 1 if it cannot; it tees the command's output to a fresh log, and a
  failed command or capture exits 1 with `command or capture failed`. Otherwise it accepts only a log holding exactly
  one libtest summary line that starts with the expected positive pass count, zero failed and zero ignored, and on
  rejection prints each summary line it found (or `no summary line`). Used by the `rust.yml` steps "Test native
  harness helpers" (job `checks-linux`) and "Test the ignored containment tests" (job `windows-ignored-containment`).
- `test-filtered-tests.sh`: real-libtest controls of the group runner and its mutation table. Each case pins its log's
  summary lines, the fixture bodies that ran, the runner's exit status and every line the runner prints, and a failing
  case prints its saved status, output and libtest log. `MUTANTS` is one table in the runner's order: its rows remove
  each of the runner's checks by one exact edit of a temporary copy, and its strings are the code no row's edit text
  holds. The suite fails if the case a row names is not the first to fail on that copy, or if the runner does not read
  as the table. It receives the existing compiled fixture by absolute path and uses its own temporary roots. Runs in
  the `rust.yml` step "Test selected Rust bodies" (job `checks-linux`).
- `installer-state.sh`: install.sh's decisions, the rendered unit and wrapper, `sot_daemon_ensure`, the log pruner,
  and `restart-backend.sh`'s choice of the daemon it judges, and the pinned bounds and copies. Runs in the `rust.yml`
  step "Test installer state (bash)" (job `checks-linux`, after the step that builds `sotd`, with `SOT_TEST_SOTD` naming
  the binary so the real-sotd cases run there instead of skipping) and in `rc-gate.sh`.
- `installer-unit-limits.sh`: sourced by `installer-state.sh`, never run: a start's log prune counts the log it is about to create (`SOT_LOG_KEEP-1` old ones stay), and the sotd unit and the launcher render each refuse, before their write and naming what and the character, a prefix or home holding anything in the library's one list (`sot_path_refused`), whose install.sh and sot-apply.sh copies are pinned byte-equal; the library's lines run under dash, which reads a backslash in an `echo`.
- `installer-path-refusal.sh`: sourced by `installer-entry.sh`, never run: install.sh run end to end with a home holding `%` or `$`, on the systemd path and with `--no-service`, is refused before any write (the prefix tree is the same bytes), the line naming the prefix or home and the character.
- `installer-boot-line.sh`: sourced by `installer-entry.sh`, never run: an install over a unit with neither the old link nor the drop-in prints one boot-off line with the command that turns it on, before the verdict; a fresh install prints none.
- `installer-apply-boot.sh`: sourced by `installer-apply.sh`, never run: the background update prints the installer's boot-off line once after APPLIED for an owned unit with neither link nor drop-in, and none with the drop-in, the old link or no owned unit.
- `installer-entry-julia.sh`: sourced by `installer-entry.sh`, never run: a Julia environment never instantiated fails naming it, and a Julia below the kernel's compat floor stops the install before any step runs on it, naming the version.
- `installer-entry-live.sh`: sourced by `installer-entry.sh`, never run: the restart ask judges "live" from the daemon's own process tree (a stand-in `ps` lists it from `ps-tree`): an old daemon that cannot report Pluto, or be told its version, with no REPL or Pluto child installs with no flag; a REPL or Pluto child with no console is the one-line stop, and with a console (`SOT_INSTALL_TTY`, a file) `y` goes on and `n` stops.
- `installer-entry-exits.sh`: sourced by `installer-entry.sh`, never run: every exit of the installer ends in one verdict line (an option error, a command that fails under `set -e`, an interrupt mid-run), and an interrupt takes its scratch folder with it, the file a Julia step left there included.
- `installer-topology-pipe.sh`: sourced by `installer-state.sh`, never run: `installer_topology_role` answers with status 0 on a table far larger than a pipe, where awk stops at the match while the writer still writes.
- `sigpipe-scan.awk`: the scan `under-bash.sh` runs over every shipped non-test script for a pipeline whose reader can stop early (`head`, `grep -q`/`-m`, `awk` with `exit`, `sed q`, `read`), which fails at random under pipefail.
- `test-sigpipe-scan.sh`: proves what `sigpipe-scan.awk` flags and what it passes.
- `installer-build-direction.sh`: sourced by `installer-entry.sh`, never run: a build of the line with no `--version` is told by ancestry (the release's COMMIT against the build's commit in the base clone) whether the newest release moves it forward or back, or why that cannot be told; it also asks the rule directly for a named tag and for a record's full commit, and pins that an `--archive` install on a build of the line makes no curl call and prints no ancestry line. It also checks the whole of install.sh for its one `git` function (the stall bound; defined before the first git call, and no call names git another way).
- `installer-ensure-timeout.sh`: sourced by `installer-state.sh`, never run: the daemon's own log named in `sot_daemon_ensure`'s timeout message (a stub `sotd log-path` answers).
- `installer-list-limit.sh`: sourced by `installer-entry.sh`, never run: the prelude's release listing under GitHub's anonymous rate limit sends `GITHUB_TOKEN` (or `GH_TOKEN`) only when one is set, and a 403 or 429 ends in a FAIL line that says GitHub refused the request, when to retry and that `--version <tag>` needs no listing.
- `installer-entry.sh`: sourced by `installer-state.sh`, never run: install.sh run end to end in a scratch HOME from a
  fixture release (a local git origin, an archive of stand-in binaries, and stand-in `curl`, `sotd`, `systemctl`,
  `julia` and `ps` driven by files), as the entry of an update: the channel the prelude follows from `install.json`, an
  implicit run that would move back stopping before anything changes, the role recovered from the record, the one
  verdict line on every exit (option errors, a failed command, a TERM, which leaves no file a Julia step made in its
  temporary folder) and each check that fails it, the daemon a restart
  or a start of the unit must bring up (never PASS while it does not answer), the stop-and-ask before a restart that
  would end a live REPL or a running Pluto server (also when the daemon cannot be asked, each reason in its own FAIL), a running daemon that cannot be told its version (a unit or a process at the prefix; a stale socket file alone is none), the step that names the rows of a held or
  launcher-started daemon, a window still on the old version, `--archive` offline, a run interrupted and repeated, the
  rollback a repair run keeps, a hub that omits this computer, an old version folder with local changes kept, and the
  launcher an enrolled frontend gets (this computer's own socket beside the plan's dials); with a built `sotd`, the real `update direction` answers. It SKIPs off Linux (a
  systemd unit).
- `installer-wrapper.sh`: the rendered `sot-launch` under the sandboxed PATH: the bridge probe before every spawn, the
  crash-loop rollback, the exit-75 respawn, the service operations by unit ownership, the re-exec after an apply, and
  `apply_pending`'s refused pointer (a stand-in `sotd` answers `update pointer`: no stop, no restart, one drop-only
  sot-apply), the exit-76 converge respawn, and the daemon finders (`sot_daemon_pid`, `installer_process_at`) naming the
  daemon, never an impostor whose argv[0] is sotd, on the box's own ps (an exported COLUMNS too) and, on Linux, on
  `bsd_ps`.
  Runs in the `rust.yml` step "Test installer wrapper (bash)" (job `checks-linux`) and in `rc-gate.sh`.
- `installer-apply.sh`: `sot-apply.sh` apply and rollback, the one-copy helper, the network refusal, the dev launcher's
  git stall bound, the ensure's replacement of a daemon an update left on the old binary (`sot_daemon_current`), and the
  refusal of a pointer that no longer fits the install (a stand-in `sotd` answers `update pointer`; `SOT_TEST_SOTD`,
  by default this tree's `debug/sotd`, names a built `sotd` for the cases that judge from the pointer's own content,
  else they print SKIP; `rc-gate.sh` sets it).
  Runs in the `rust.yml` step "Test installer apply (bash)" (job `checks-linux`) and in `rc-gate.sh`.
- `installer-shared-home.sh`: a home several hosts share holds one install. Every path that writes, re-renders or
  retires `sotd.service` (`installer_enable_local_service`, `installer_retire_local_service`, `sot_rerender_owned`) pins
  it with `sotd topology pin` before any reload, and none disables the shared unit under a declared topology; with no
  hosts.toml, two hosts on one home: the install claims the pin, and only the host it names alone removes the boot drop-in (a retire that keeps it moves an old enable link onto it); a real `systemctl --root --global` enable or disable by hand changes no file, read by `systemd --test` on scratch folders with its control;
  one rule for `install.sh` and an update: a fresh install wires boot start, an existing unit only where its old link or the drop-in is; a
  hosts.toml sotd cannot read is still a declared topology; the tables of `installer_record_decision` and
  `installer_host_decision`; and `install.sh`, run end to end from a stub release on a host listed with neither key,
  on an unlisted host asking for a daemon, and with an unpacked sotd that cannot run, refuses and leaves every entry
  under the prefix and `~/.local` as it was; run without jq, it stops with the one line naming the command that installs
  jq on this OS, before any curl call or change. It also reads the argv of the stand-in curl and git of
  a run that reaches the base clone, the tag fetch and the Julia installer: each carries its stall bound; the same run drops
  the pointer armed over the install it replaced and keeps another platform's. install.sh's
  PATH is the stand-ins and a tool dir of its own (`mk_tools` plus tar, gzip, tr and jq), never the host's /usr/bin, where a
  Julia (the hosted ubuntu-24.04 image links one there) would skip the Julia installer. Runs in the
  `rust.yml` step "Test installer shared home (bash)"
  (job `checks-linux`) and in `rc-gate.sh`.
- `installer-apply-fit.sh`: sourced by `installer-apply.sh` inside its real-sotd branch, never run: the applier checks the prepare against this host's role, so an env-less prepare is refused where a backend runs (no topology, a `daemon` entry, a `frontend` entry whose install records a daemon, a guest of the home over a daemon install) and applies for a `frontend` entry whose install records none, and a stage with no `prepared.json` is refused.
- `installer-handoff.sh`: sourced by `installer-apply.sh`, never run: a real staged `sot-apply` (a copy made by `cp`, the
  packer's own, and an installed v0.6.5 one when the tag is in the checkout) applying onto the install that re-executed
  it, `pack-build.sh` with stand-in cargo and git listing `sot-apply` executable in its archive, and the comm refresh
  an apply makes (bounded when julia stalls, julia's stderr shown), a rollback and drop-only through a hand-off, the staged copy's second
  ask of the installed `sotd` under the lock, and the unit's `ExecStartPre` reading `.bashrc`.
- `installer-julia-step.sh`: sourced by `installer-wrapper.sh`, never run: install.sh's Julia steps (`julia_pick_chan`,
  `julia_run`) under a stand-in `julia`: the channel is chosen once, by one probe, each step runs once on that julia (the
  logged argv is asserted, with and without the channel) with its stderr shown, and a failing step shows its own error
  once and keeps its exit status.
- `installer-julia-floor.sh`: sourced by `installer-wrapper.sh`, never run: install.sh's `julia_floor_check` under stand-in
  `julia` and `juliaup.json` (a scratch `JULIAUP_DEPOT_PATH`): a default and a PATH julia at or above the kernel's compat floor
  leave nothing; a default below it (the 1.12 channel installed or not), a PATH julia below it (which stops the run), the
  refresh's Julia alone below it, a `SOT_JULIA_BIN` below, relative or removed, and a PATH julia that cannot be asked each
  end with the version and the fixing command. Each printed command is also run against a scratch juliaup in
  `installer-wrapper.sh`.
- `installer-support.sh`: the setup the installer suites source: install.sh and lib/sot-daemon.sh, `check`,
  `starts_with`, `case_start`, the sandboxed tool dir (`mk_tools`), the recording stubs (`mk_stubs`), and BSD stand-ins
  for Linux: `bsd_stat_date`, and `bsd_ps`, macOS's ps modeled from /proc (comm is argv[0] cut to 16 characters).
- `install-smoke.sh`: `install-smoke.sh SCENARIO ARTIFACTS CHECKOUT [BEFORE-TAG]` (Linux, macOS; bash 3.2): `install.sh`
  against the artifacts a release run built. It makes the `--archive` folder from the run's tarball (SHA256SUMS, COMMIT,
  the tarball repacked under the binaries' own `+` version when a branch run named it `0.0.0-ci.<sha>`), refuses an
  archive whose `sot-apply` is not executable, and judges each run's one verdict line (PASS; a ONE STEP LEFT fails).
  Every leg ends in `state_is_new`: `install.json` (version, commit, daemon and frontend bits of the role), the checkout,
  the binaries, `bin/sot-apply` equal to the release's own, comm, and the wrapper equal to the new `render_sot_launch`
  (a backend-only install has none). Scenarios: `fresh-local`, `fresh-be-only` (Linux, `--no-service`: no user manager in a
  container), `update-entry` (BEFORE-TAG by its own installer through the prelude, then the entry) and `update-apply`
  (BEFORE-TAG's installed `sot-apply` run as `ExecStartPre` runs it, over a release staged by the new `sotd`: the
  staged apply must hand off and run), and `update-implicit` (BEFORE-TAG installed, then the entry with no arguments
  against the real repository: a repair run at BEFORE-TAG that clears the pin, then the new `sotd` stages the run's pack
  and must say `newer` and arm; its unauthenticated release-list calls share the runner's rate limit); the others end with a repair run. Runs in `release.yml`'s `install-linux` and `install-macos` jobs, and off
  CI the same way (a scratch `$HOME`; the caller's `SOT_` and XDG variables are unset).
- `install-smoke.ps1`: the same for `install.ps1` (Windows PowerShell 5.1): scenarios `fresh`, `update-entry`,
  `update-apply` and `update-implicit` (it says SKIPPED and exits 0 while the release before carries no `install.ps1`), one scratch profile and a `USERNAME` of its own. The release before is installed by its own
  `install.ps1` when its tag carries one, else by that tag's section 2b (`Install-BeforeByHand`, to delete when the
  release before has `install.ps1`). In a session with no desk (session 0) an update ends in the entry's "start Ship of
  Tools at the desk" step and the script applies it with the installed `sot-apply.ps1`, as the start does. A fresh `+`
  pack fails if the shortcut step printed "no manifest written". Each leg's log starts with the runner's session id.
  Runs in `release.yml`'s `install-windows` job.
- What the smokes do not cover: the systemd unit path (the default Linux install; the legs take no service), Windows
  path (c) when the runner is session 0 (the update stays armed and the script applies it), paths (a) and (b) (a running
  window), and the by-prelude Windows release before (the first branch of Install-Before) until a published release
  carries install.ps1.
- `release-before.sh`: `release-before.sh SOTD VERSION [OWNER/NAME]` prints the newest published release older than
  VERSION, by `sotd update direction` (the one version comparison; GitHub's "latest" and any written tag are never
  used), and fails naming a missing platform asset instead of taking an older release. Runs in `release.yml`'s
  `release-before` job.
- `test-release-before.sh`: `release-before.sh` over fixture release lists (`SOT_SMOKE_RELEASES_JSON`) with the sotd of
  this tree (`SOT_TEST_SOTD`): the pick for a dev build, an rc, a final, the first rc of a new minor and a version with
  nothing older, and a release missing an asset naming it instead of an older one. Runs in the `rust.yml` step "Test
  release-before (bash)" (job `checks-linux`, after the sotd build).
- `test-release-branch.sh`: `release.sh`'s branch guard in a throwaway repo with no origin: a cut from `main` gets past the
  guard (its first refusal is the missing origin), and a cut from `fixes/*` or `rc/*` is refused by it. A second fixture
  (origin, stubbed cargo, gh and systemctl) pins the test step's setup refusals before the stamp (MathJax modules, user
  manager) and that a forced test failure leaves the tree clean. Runs in the
  `rust.yml` step "Test release.sh branch guard (bash)" (job `checks-linux`).
- `check-layout.sh`: the layout check (file and folder sizes, pages, `## Files` lists); see Tools below.
- `check-layout.allow`: the layout exceptions CI passes to `check-layout.sh`, each with its reason.
- `exempt.txt`: the standing exemptions `check-layout.sh` reads from beside itself.
- `moved-check.sh`: compares the lines a commit range added and removed, to show a move left no residual.
- `test-check-layout.sh`: self-test of `check-layout.sh` over throwaway repos.
- `test-moved-check.sh`: self-test of `moved-check.sh` over throwaway repos.
- `test-rc-gate-target.sh`: `rc-gate.sh` names its checkout in its target and refuses the target to any other checkout.
- `test-rc-gate-basename.sh`: `rc-gate.sh` names a shell suite's result and log by its checkout path, so two suites with one basename keep their own verdicts, and `emit` refuses a result file emitted twice before any job runs.
- `test-rc-gate-parity.sh`: `rc-gate.sh`'s three clippy jobs (`lint-lib-bins`, `lint-all-targets`, `lint-windows-tests`) go red over a stand-in workspace holding a test-children violation, an un-allowed `std::fs::copy` or a violation only `cfg(windows)` test code holds, and its `under-bash` job goes red on a script that parses only under bash 4. The jobs run as the gate runs them (`--job`) with the real cargo and a real bash 3.2.57 (`bash32.sh`); the windows-gnu cases print SKIP where that target is not installed. The `--pipeline` run over a stub cargo shows each clippy line emitted with its arguments and `CLIPPY_CONF_DIR`.
- `bash32.sh`: `bash32.sh [DIR]` prints the path of a GNU bash 3.2.57 built in DIR (default `/scratch/<user>/bash-3.2.57`) from its tarball (sha256 pinned, no sudo), once: a built DIR is reused, and a build moves into place whole. A build needs bison, yacc or byacc on PATH, because make runs yacc on the tarball's `parse.y` (newer than its `y.tab.c`); with none, it exits 2 before the download, and a DIR that already holds a built bash needs none of them. `rc-gate.sh` and the `bash32-linux` job both call it, so the recipe lives here.
- `test-bash32.sh`: `bash32.sh` with no yacc on PATH exits 2 with one line naming bison, yacc or byacc and never calls curl (a stub records the call); a DIR holding a built bash is reused with no yacc; and each of the three yacc names lets the build go on to its download. Hermetic: a PATH of links to the tools the script needs, no download.
- `test-rc-gate-logs.sh`: no command an `rc-gate.sh` job runs has its standard output or error in the logs folder; each
  job kind but `under-bash` (it runs the real suites) runs through `rc-gate.sh --job` on stand-ins that name their own
  streams and exit 7, and the job's log gets their lines and its result the 7. Linux.
- `on-host.sh`: `on-host.sh HOST DIR -- CMD [ARG...]` runs CMD in DIR on a second host through `ssh HOST bash -s`, with
  the host's SOT_ variables, XDG_STATE_HOME, JULIA_LOAD_PATH and JULIA_PROJECT unset first; prints nothing of its own.
- `under-bash.sh`: `under-bash.sh BASH [-- SUITE...]` checks the shell scripts under BASH, the bash 3.2 that
  comm/CLAUDE.md and scripts/CLAUDE.md promise. Every tracked shell script outside a tests folder under `comm/`,
  `agents/` and `scripts/` must parse under BASH and run no utility by the `command` builtin outside a pipeline (it ends a `set -e` shell under 3.2 when the utility fails), and no tracked file under those folders, tests included, may hold an in-place `sed` with no suffix (macOS's `sed` reads the expression as the suffix); then each SUITE runs from its own folder with a folder holding
  `bash -> BASH` first on PATH, so each script it starts through `env bash`, `bash x.sh` or `bash -c` gets BASH (a
  `#!/bin/bash` script only where BASH is /bin/bash). A suite here runs on BASH itself (it sources the scripts it
  tests); any other on the bash running under-bash.sh (4 or later). With no SUITE it runs every hermetic suite: each
  `test-*.sh` in `comm/tests` and `agents/tests` but the five that need peer hosts, and the four suites here that
  `checks-linux` runs. Prints `under-bash: PASS|FAIL` lines and exits with the count of FAIL lines, 2 on a wrong setup.
  Runs in the `rust.yml` job `bash32-linux` (GNU bash 3.2.57, every hermetic suite) and in `test` on macOS
  (`/bin/bash` under the scripts, every hermetic suite, run by Homebrew's bash, which the step installs).
- `in-container.sh`: `in-container.sh [--user-manager] [--tmpdir-empty] UNIT -- CMD [ARG...]` runs CMD as the main process of the transient
  `systemd --user` service UNIT.service, the test container (Linux): a private empty `XDG_RUNTIME_DIR` and no
  `DBUS_SESSION_BUS_ADDRESS`, so every capsule row stays in its control group, and every process left in it is SIGKILLed
  when CMD ends. `--user-manager` keeps the manager's address. `--tmpdir-empty` (opt-in; TMPDIR must be set to the job's fresh folder) fails
  the job with exit 3 and names what CMD left in TMPDIR, unless CMD already failed. CMD runs as written (`$` is doubled for systemd, which expands
  `$VAR` and `${VAR}`); the exit status is CMD's when CMD exits, and a CMD killed by a signal gives 255; a failed start
  leaves no `/tmp/sot-job-*`.
- `test-on-host.sh`: `on-host.sh` against a stub `ssh` that runs `bash -s` locally. Run by hand.
- `scratch-guard.ps1`: the first lines of every Windows suite that can start or touch sot state (`test-sot-apply`,
  `test-install-manifest`, `test-install-layout` with its entry cases, `test-sot-within`, `test-local-daemon` with its
  parts, `test-launcher-leases`, `test-topology-plan`, `install-smoke`; four lines, the file's header gives them). The
  suite never runs in the caller's profile: the guard makes its own home `<account's real temp folder>\sot-scratch-<id>`
  with a marker, runs the suite again as a child PowerShell there (USERNAME `sot-scratch-<id>`, USERPROFILE, HOME,
  APPDATA, LOCALAPPDATA, TEMP, TMP and SOT_COMM_HOME inside it, other SOT_ variables cleared, same arguments), removes
  exactly that home by its path and hands back the child's exit code. In the child it refuses (exit 2, one line) unless
  every one of those lies inside the home, the home sits directly under the real temp folder, and USERNAME differs from
  `[Environment]::UserName`; the real temp folder comes from the process token's profile (`GetUserProfileDirectoryW`; `GetFolderPath` follows a moved USERPROFILE and would name the scratch home). The library switch `$sotScratchLibOnly` counts only in the dot-sourcing scope itself. The child also loses the caller's `XDG_*` and `CLAUDE_CONFIG_DIR`, and gets a `JULIA_DEPOT_PATH` inside its home unless the caller's lies inside the real temp folder. The guard prints a line when it makes the home and one when it removes it; the child starts in the caller's location. No switch turns it off. Out of
  scope, with the reason: `in-job.ps1` (a job runner, starts only the command it is given), `pipe-request.ps1` (a client
  of the pipe its caller names), and the dot-sourced parts, which run inside a guarded suite. A suite removes only
  folders it made, by path; `test-local-daemon-support.ps1` ends only daemons it recorded (pid and creation time) on pipes this run named.
- `test-scratch-guard.ps1`: the guard's rule on a made-up account, and the entry end to end on a probe suite (arguments,
  environment, the home gone at the end, a sibling kept, two concurrent runs, a refused child, the four lines in each
  guarded suite). Not itself guarded; it removes only its own folder.
- `in-job.ps1`: `in-job.ps1 CMD [ARG...]` runs CMD in a Windows job object, the test job (Windows): every process CMD
  starts, at any depth, ends when the script does: when CMD exits, when the script is ended from outside, or when
  the ssh session it runs under ends (its nearest sshd ancestor exits; exit 129). A CMD with no path is looked up on PATH, as the calling shell would (CreateProcess alone finds System32's WSL `bash` first). Exit status CMD's, 2 on a usage error; 8 when CMD exits 0 with processes of the job still running: then a stderr line names up to 16 of them (image and pid, and how many more), the job ends them (`TerminateJobObject`), and the script exits 8. A CMD that exits non-zero keeps its own status. Run by hand and by the Windows box's runner; the `rust.yml` step "Parse PowerShell scripts" parses it. Limits: the job's `CREATE_BREAKAWAY_FROM_JOB` always fails (the capsule spawn takes its degraded path, as on hosted CI), so the box does not exercise the breakaway path; words such as `--` and `-x:y` pass through `powershell.exe -File` parsing first and are untested, so a runner passes one script.
- `rc-gate.sh`: the Linux candidate gate, run as the main process of its own test container (`in-container.sh`); a selected Rust job succeeds only when its exact body completed, and runtime-listed ignored tests are reported as skipped rather than submitted as ordinary jobs. Whole-binary jobs retain their existing status policy. It runs the daemon lifetime harness as rust.yml's Linux job does (`job_lifetime`), with `SOT_LIFETIME_KEEP_DIR` at `lifetime-keep` in its result folder, where a failed case keeps its daemon logs, answers and process lists.
- `test-install-layout.ps1`: `Test-SotPinnedCheckout`, `Get-SotLauncherTarget`, `Get-SotLauncherCodeId` and
  `Initialize-InstallLayout`'s trust delegation (scripts/sot-install-layout.ps1), and `Invoke-FreshnessPass`'s comm
  decision by the comm stamp (scripts/sot-freshness.ps1, section 5), and `Invoke-PendingApply`'s daemon stop (section 6:
  a pointer the real `sotd` refuses stops no daemon). Runs in the `rust.yml` step "Test
  install layout (pinned-checkout predicate)".
- `test-install-entry-direction.ps1`: the cases of `test-install-entry.ps1` for a build of the line run with no `-Version` (dot-sourced there, same scope): the stop says by ancestry, against a base clone with three commits and a stand-in COMMIT asset, whether the newest release moves it forward or back, or why it cannot tell; install.ps1's syntax tree, whole file, has no command naming git or git.exe outside its one `Invoke-Git`, which carries the stall bound.
- `test-install-entry-live.ps1`: the same cases for `install.ps1` (dot-sourced by `test-install-entry.ps1`, same scope): `Select-DaemonChildren` on synthetic process lists, and the entry with `Get-ProcessSnapshot`, `Test-Console` and `Read-Answer` replaced: no child, a REPL child with and without a console, answers `y` and `n`.
- `test-install-entry-julia.ps1`: the Julia-floor cases of `test-install-entry.ps1` (dot-sourced there, same scope): a stand-in
  PATH `julia` and a scratch `juliaup.json` at, above and below the kernel's compat floor, no juliaup, a juliaup stand-in the case owns, `SOT_JULIA_BIN`, and a julia that cannot
  be asked, each ending PASS, FAIL (the PATH julia below the floor stops the run) or ONE STEP LEFT with its line.
- `test-install-entry-repair.ps1`: the repair cases of `test-install-entry.ps1` (dot-sourced there, same scope): `install.ps1` over an
  `install.json` that is missing, holds a pin and no version, or is cut short, beside a running stand-in `sotd.exe`, with and without a
  version named, and the pin edit's rename with an open handle
- `test-install-entry.ps1`: section 7 of `test-install-layout.ps1` (dot-sourced there, so it runs in the same step), the
  Windows entry `scripts/install.ps1` against stand-ins in a scratch profile: a compiled sot/sotd/sot-capsule per version
  (the daemon questions answered from `SOT_FAKE_*`, every other `sotd` verb handed to the real one), a git origin at one
  tag per version, a julia (a compiled one at `SOT_JULIA_BIN` for the stage's prepare, a script for the comm step) and a
  jq, the network replaced by copies from pack folders. It pins the URLs the entry builds (as whole strings) and a
  prelude run end to end (the child stopped at its first network call by a dead proxy), a fresh install and an update
  from that layout (nothing running: applied here), the repair run (the pointer dropped, the pin written, nothing
  downloaded when it names the installed version), the never-backwards default, the `+` stop, option errors and an
  interrupt each ending in one verdict line, the live-work ask (rows, the Pluto server, a daemon that cannot be asked, a
  daemon that cannot be told by pipe or by process), the path choice (a supervisor with and without `code=`, the
  window's launcher code, a desk session or not), the converge to its end with the daemon answering as the target by the
  clock, the enroll refusal, a box with no Julia, and the four launcher log lines the entry reads. `Write-EntryLine` is
  the output seam, and `Get-EntrySessionId` the desk-session seam.
- `test-sot-within.ps1`: the PowerShell scripts' network bounds. `Invoke-SotWithin` (scripts/sot-hosts.ps1, through
  sot-lease.ps1's `Start-SotRedirected` and `Stop-SotTree`): a stalled command ends at its bound with code 124, with
  every process it started; so does one whose descendant holds its output after it exits; a finished one returns its
  output and code; a hidden PowerShell whose console input encoding has a BOM writes only its caller's bytes; a command
  that cannot start is code 127. Each stand-in goes
  first on a child PowerShell's PATH, never the test's: `shutdown-sot.ps1`'s `Get-DaemonDisconnect`, read as a syntax
  tree and run against a stand-in `ssh.exe`, passes the tree's ssh options and the command on stdin, and names a hung
  ssh; `sot-install-layout.ps1`'s tag fetch, against a stand-in `git.exe`, carries the stall bound; and
  `launch-sot.ps1`'s self-update pull, read as a syntax tree, carries it too and reads git's stall error as offline.
  Runs in the `rust.yml` step "Test Invoke-SotWithin".
- `test-local-daemon.ps1`: scripts/sot-local-daemon.ps1 start, stop and wait behaviour (sections 0-8 and 12-15: the
  refusal, the pipe name, a late bind, `-Stop`, log retention, `Get-StopWaitMs`). Runs in the `rust.yml` step "Test
  local daemon launcher".
- `test-launcher-leases.ps1`: launch-sot.ps1's ensure and lease order in the supervisor loop and the converge lease
  (sections 9-11 and 16 of the old suite), read as syntax trees and run against the fake `sotd.exe`. Runs in the
  `rust.yml` step "Test launcher leases", a bridge that ends before the lease is written (11f), and a request the
  daemon answers late, kept open with its bridge through a collection and its handover behind it (11g).
- `test-local-daemon-fake.ps1`: dot-sourced by both local-daemon suites: compiles the fake `sotd.exe` and defines
  `Clear-FakeEnv`, `New-FakePrefix` and `Stop-FakeOn`.
- `test-local-daemon-own.ps1`: dot-sourced by test-local-daemon.ps1 after section 5b, in its scope: the bridge's
  own-account pipe probe (4b), the daemon finder's account filter and its process controls (4c), and the real
  daemon's bridge-held lease handover (5b2).
- `test-local-daemon-decoy.ps1`: dot-sourced by test-local-daemon.ps1 after the binary file, in its scope, when the fake
  compiled: `-Stop` ends the daemon its pipe's owner is and leaves a `sotd.exe` decoy whose command line names that pipe,
  and a second daemon on another pipe, running (4d).
- `test-local-daemon-pipe.ps1`: dot-sourced by test-local-daemon.ps1 inside section 5c, in its scope: the session pipe
  under load (cases (iii)-(viii): large requests behind an accepted and a refused hello, the inbound buffer's memory, a
  peer the daemon gives up on, a session that ends on a frame the daemon will not read).
- `test-local-daemon-binary.ps1`: dot-sourced by test-local-daemon.ps1 right after the fake daemon's compilation is attempted, in its
  scope: the local daemon's one binary, shared by the daemon, `-Stop` and the launcher's query and lease (2c), the
  bytes the lease's and the probe's bridges read in a hidden console, which are only their callers' (7c), and the
  ensure's replacement of a running daemon of another version (7d), and the daemon's own log named when a daemon that never binds is left running (7e, which waits the launcher's whole 160 s; 7c to 7e only when the fake compiled).
- `pipe-request.ps1`: the local-daemon suites' raw pipe client (5c, 5d): a hello and one request into a named pipe,
  the reply whose op matches printed. Test code; it checks no account.
- `test-local-daemon-support.ps1`: dot-sourced by both local-daemon suites: `Check`, the fixture and pipe helpers, the
  test root and `Complete-LocalDaemonTest`, their cleanup.
- `test-sot-apply.ps1`: scripts/sot-apply.ps1 against a synthetic staged update: apply, damaged stage, rollback,
  already applied, wrong target, lock held, and (section 9, with a built `sotd.exe`, else SKIP) a pointer that no
  longer fits the install or whose stage has no record of a prepare. Runs in the `rust.yml` step "Test sot-apply.ps1".
- `test-native-stderr.ps1`: a script under `$ErrorActionPreference = 'Stop'` does not depend on how its caller redirects: `Invoke-SotNative` (stderr never ends the script, the exit code is the answer, a command that cannot start is 127), and `pack-build.ps1` and `relaunch-sot.ps1` run with `*>&1` and `2>&1` against a stand-in cargo that warns on stderr (a zero exit goes on, a failed build stops with the script's own message). Windows PowerShell 5.1. Runs in the `rust.yml` step "Test native stderr under Stop".
- `test-install-manifest.ps1`: scripts/install-manifest.ps1's pointer rule and its log line for a build of the line (kept record or none): a changed version or commit drops the armed
  pointer, a same-version rewrite keeps it, no pointer breaks nothing. A stand-in sot.exe built with csc.exe and a scratch
  git repo; runs under Windows PowerShell 5.1 and pwsh. Runs in the `rust.yml` step "Test install-manifest.ps1".
- `test-topology-plan.ps1`: `Get-SotTopologyPlan` and `Invoke-SotTopologySync` (scripts/sot-hosts.ps1) against a fake
  `sotd`. Runs in the `rust.yml` step "Test topology plan".
- `test-topology-plan.sh`: `sot_topology_plan` (scripts/lib/sot-hosts.sh) against a fake `sotd`. Runs in the `rust.yml` step
  "Test topology plan (bash)" (job `checks-linux`) and in `rc-gate.sh`.
- `test-binary-bound.py`: cargo's test runner in the build+test jobs: each test binary ends at its bound (900 s in CI), naming the tests libtest reported running that never ended
- `test-test-binary-bound.sh`: its check, on stand-in binaries

## Tools

Bash wrappers around python3 (standard library only). Run from any directory with `--repo <worktree>`; they work in
detached worktrees and change nothing.

### moved-check.sh

`scripts/tests/moved-check.sh [--repo DIR] [--allow-file FILE] <base>..<head>`

Takes `git diff -U0 --no-renames` over the range, normalises every added and removed line (trim whitespace, drop one
leading `pub `, `pub(crate) `, `pub(super) `, `pub(in ...) `, drop empty lines) and compares the two multisets. Lines
present on both sides are MOVED. The rest is residual.

Output, in order: per-file table (path, added, removed); `MOVED: n`; `SCAFFOLD: n`; `ALLOWED-BY-FILE: n` (only with
`--allow-file`); `RESIDUAL-ADDED: n` then `+ file:newline: text`; `RESIDUAL-REMOVED: n` then `- file:oldline: text`.

Exit: 0 both residuals empty, 1 otherwise, 2 usage or git error.

Allowed scaffolding (removed from the residual, counted in SCAFFOLD):
- any line in a file named `CLAUDE.md` (never enters the multiset)
- Rust (`.rs`): `mod x;` / `pub mod x;`, `#[cfg(...)]`, `#[path = "..."]`, `use ...;` and `pub use ...;` including
  multi-line `use ... {` blocks up to the line with `;`, `impl Name {`, `impl<..> Name {`, `impl Trait for Name {`, lines
  of only `{` or `}`, `//!` lines, `#[cfg(test)]`, `#![...]`
- Shell (`.sh`, extensionless): `source path`, `. path`, shebangs, comment lines that name a file path
- Julia (`.jl`): `include("...")`, `module X`, `end`
- any file: shebang lines

`--allow-file`: one regular expression per line; a residual line whose text, or `file:text`, matches is counted under
ALLOWED-BY-FILE, not residual.

### check-layout.sh

`scripts/tests/check-layout.sh [--repo DIR] [--allow FILE] [--exempt FILE] [<folder> ...]`
`scripts/tests/check-layout.sh [--repo DIR] --report [<folder> ...]`

Source files: tracked `.rs .jl .sh .ps1` plus tracked extensionless files starting with `#!`. Folders are repo-relative;
with none, every folder that directly contains tracked source, or holds a CLAUDE.md, is checked (so the `## Files` list
of a source-free folder such as `.github/` is checked too).

Each violation is `VIOLATION <kind> <path> <detail>`:
- `file-size`: more than 800 code lines. Rust test lines are the span of each column-0 `mod` item whose stacked
  attributes include a cfg that requires `test` (`cfg(test)`, or `cfg(all(..))` with `test` among its operands; `any(..)`
  and `not(test)` do not count), from the first attribute to the module's closing column-0 `}` (or its `mod x;` line);
  code lines are the rest, so a test module in the middle of a file is counted only for its own span. `*_tests.rs`,
  `tests.rs`, `tests_*.rs`, `*_tests_*.rs`, `test_support.rs` and anything under `tests/` are test files: 800 total lines.
- `no-page`: folder directly holds source but has no CLAUDE.md.
- `file-list`: the `## Files` section (ends at the next `## `) must list exactly the folder's tracked files and
  subfolders with tracked files, minus CLAUDE.md; a name is the first backticked token of a `- ` line, with or without a
  colon after it. A subfolder is written `name/` and a file `name`; the slash must match the kind. Reports `missing:`
  and `extra:`.
- `folder-size`: direct non-test source over 3,000 code lines or 12 files.
- `named-path`: `VIOLATION named-path <page> <path>`. For each tracked page in the tool's list `NAMED_PATH_FILES`,
  every inline-backticked token outside fenced blocks is a candidate. A trailing `:<n>` or `:<n>-<m>` and a trailing
  `::<item>` are stripped and one `{a,b,...}` group is expanded. A token containing `<`, `>`, `*`, `$`, `~` or a space
  is skipped. A token whose first segment is not a tracked top-level folder or root file is skipped. Any other must
  name a tracked file or a folder holding tracked files, else it is a violation. A folder-argument run checks the
  listed pages too.
- `too-many-lines`: `VIOLATION too-many-lines rust <n> allowances, pinned <p>: the count rose|fell...`. The lines of
  tracked `.rs` files under `rust/` (not `rust/vt100`, the vendored crate the clippy steps exclude) that name
  `clippy::too_many_lines` must number exactly the pin, the allow file's line `too-many-lines rust <n> <reason>` (no
  line pins 0; a pin that does not start with a number is a usage error). The count can only fall: a rise says to
  split the function, a fall says to lower the pin in the same commit. Every run checks it, folder arguments or not.
  These text forms, which loosen the gate without moving the count, each fail as `VIOLATION too-many-lines <file>
  <why>`. In a `.rs` under `rust/` (not `rust/vt100`): an inner `#![allow(..)]` or `#![expect(..)]`, at crate or module
  level, naming `clippy::too_many_lines`; a `cfg_attr(.., allow(..))` or `expect` naming it, at any level; an `allow` or
  `expect` naming `clippy::pedantic`, at any level. In a Cargo.toml there: `too_many_lines = "allow"` or `"warn"`, plain
  or as `{ level = .. }`. In a clippy.toml there: `too-many-lines-threshold`. In a workflow: `-A`, `-W`, `--allow` or
  `--warn` before `clippy::too_many_lines` (spaced, with `=` or attached), and any `--cap-lints`. A form not listed here
  is not caught: removing the step's `-D clippy::too_many_lines`, or a source-level `warn` of the lint or of
  `clippy::pedantic`, which also overrides the command line's `-D`.
- `timing-bounds`: `VIOLATION timing-bounds rust <n> literal waits in test code, pinned <p>: the count rose|fell...`. The
  occurrences of `thread::sleep(`, `recv_timeout(`, `Duration::from_secs(` and `from_millis(` on non-comment lines of
  Rust test code under `rust/` (a test file by the tool's file rule, or a `#[cfg(test)]` mod; not `rust/vt100`, and not
  `rust/log/src/test_*.rs`, the harness that owns the layers' hang guards) must number exactly the pin, the allow file's
  line `timing-bounds rust <n> <reason>` (no line pins 0). The count can only fall: a new literal bound fails the build,
  and each existing one is a deletion target, replaced by the product's own constant or the layer's hang guard. A
  fall says to lower the pin in the same commit. Every run checks it.

Covered folders: a crate root (a tracked Cargo.toml with `[package]`) covers its `src/`; a Julia package root (a tracked
Project.toml) covers `src/`, `test/` and `ext/`. A covered folder needs no CLAUDE.md of its own (a covered folder that
has its own page is an ordinary folder). The root's page is required (`no-page` at the root path) and its `## Files`
lists the root's own entries minus the covered folders, plus every entry of each covered folder with its prefix
(`src/lib.rs`, `src/ops/`, `test/runtests.jl`), one level only. `file-size` and `folder-size` still apply to covered
folders.

`exempt.txt` (beside the script; override with `--exempt FILE`; an explicit file that is missing is a usage error):
standing rules for everyone, lines `<kind> <path-glob> <reason...>`, `#` comments, kinds `no-page`, `file-size`,
`folder-size`, `file-list`. Globs: `**` crosses folders, `*` and `?` stay inside one name, a trailing `/**` also matches
the folder. An exempt match is silent and counted as `exempt: n`.

`--allow` file (`scripts/tests/check-layout.allow` in CI): lines `<kind> <path> <reason...>`, `#` comments. Path is the
file for file-size, the CLAUDE.md for file-list, the folder otherwise. Matches print as `ALLOWED ...` and do not fail.
An allow line that matches no violation prints `UNUSED-ALLOW <line>` and makes the exit status 1.

When a listed page is tracked, `named-path: n tokens checked in f files` follows the violations; then always
`too-many-lines: n allowances in rust (pinned: p)` and `timing-bounds: n literal waits in rust test code (pinned: p)`.
Last line: `violations: n, allowed: m, exempt: e, unused-allow: u, folders checked: k`. Exit 0 none, 1 any, 2 usage error.

`--report`: per folder, source files as `code N test M name`, largest code first.

### Self-tests

`bash scripts/tests/test-moved-check.sh`, `bash scripts/tests/test-check-layout.sh`: build throwaway repos under
`mktemp -d`; exit 0 when all assertions pass.

## Start here
`installer-state.sh` for a change to install.sh, sot-apply.sh or lib/sot-daemon.sh: each `case_start` names the
behaviour it pins. For a Windows script change, the `.ps1` suite named for it above.

## Rules
- Each check run-filtered-tests.sh makes is a row of test-filtered-tests.sh's `MUTANTS`, with the case that fails first
  without it. `MUTANTS` is one table in the runner's order: each row where its edit text starts, and each fragment of
  code no row's text holds as a string. The suite reads the runner's bytes, fails on a NUL byte (bash drops it), and
  fails unless each row's text occurs once and the runner reads as exactly that table, apart from comment lines and the
  spaces, tabs and line breaks at a fragment's edges; a comment that holds a row's text, or a removed comment that a
  row's text ends in, still fails. So code added to, removed from, moved within or changed in the runner fails the suite
  until the table changes with it; a new check adds its row and, if no case fails without it, its case. Review still
  catches what the suite cannot see: a deleted row whose text other rows' texts cover (one row's, or two together, as
  argument-order's); a check's code written into a fragment instead of a row; a deleted case that is no row's first
  killer (`ignored`), or a weakened case that still fails first on its rows' edits; tee-status's fail-open form (a stale
  log tee cannot open; root can open any file, so no case can pin it); and spaces, tabs and line breaks at a fragment's
  edge.
- A `rust.yml` step that filters libtest bodies to run them as tests runs through run-filtered-tests.sh with its verified
  positive count ("Test native harness helpers" in `checks-linux`, "Test the ignored containment tests" in
  `windows-ignored-containment`), so a filter that runs no body, or another number, fails. Two filter steps keep cargo's
  exit status, because a step of the same run executes every body they select: "Folder trust premises (Windows and
  macOS)" filters `trust_declaration` and `agents::folder_trust` in `test`, whose "Test workspace" runs every
  non-ignored sotd body; the `test-linux` row `outage test lane_bridge outage::` is paired with `capsule test
  lane_bridge --skip outage::`, which runs every lane_bridge body the first does not select. The fixture selections of
  "Test selected Rust bodies" are inputs, not tests: test-test-body.sh's `cargo_parity` case judges their logs. Single
  selected-body owners retain lib-test-body.sh and its stronger identity verdict.
- The checker accepts the outer result only after complete, unambiguous captured-output boundaries; a captured summary or truncated capture cannot supply that result.
- The proof binds its shell scratch root to a validated absolute directory before installing cleanup; behavior controls observe the driver and cleanup paths under relative TMPDIR.
- A shell owner that selects one Rust body consumes the completion verdict from lib-test-body.sh; a listed name, child exit zero or a zero-test summary alone is not execution proof.
- The five `.ps1` suites run only on the windows-latest leg of `rust.yml`, under Windows PowerShell 5.1; `rc-gate.sh`
  and a Linux box never run them. The step "Parse PowerShell scripts" globs `scripts/*.ps1` without recursion (and names `tests/in-job.ps1` and `tests/install-smoke.ps1`), so each
  `.ps1` suite parses itself and its siblings in its section 0.
- A suite runs only if a step of `.github/workflows/rust.yml` or a job of `rc-gate.sh` names it; a new suite is added
  to the step list in the commit that adds it. The exception is `install-smoke.sh`, `install-smoke.ps1` and
  `release-before.sh`: they need a release run's artifacts and the published releases, so `release.yml` runs them
  (jobs `install-*` and `release-before`), and `publish` needs those jobs. `rc-gate.sh` lists the five installer/topology shell suites and the finite selected-body proof here by name
  (`SHELL_ALL` in `producer`).
- `installer-state.sh` (and `installer-entry.sh` it sources), `installer-wrapper.sh`, `installer-apply.sh`, `installer-shared-home.sh` and `test-topology-plan.sh` run wholly under
  bash 3.2 in `under-bash.sh`'s run, since they source the scripts they test, so they hold under 3.2 themselves: inside
  `$( )` a `case` pattern opens with `(`, because 3.2 ends the substitution at a bare pattern's `)`.
- `installer-support.sh` sources `install.sh` with `SOT_INSTALL_SOURCE_ONLY=1` and `lib/sot-daemon.sh` for both installer
  suites; `installer-state.sh` reads `rust/protocol/src/ops/lease.rs` for the pinned bounds (`launcher_bounds_match_ops`), so a
  rename there breaks it.
- The suites stub `sotd`, its `stdio-bridge` arm included (and `systemctl` in `installer-support.sh`); none needs a
  network. `test-local-daemon.ps1` sections 3 to 6 need a real `sotd.exe`: a failure on CI when absent, a skip elsewhere.
  Section 6 derives its pipe as a launch does (`sotd session-socket-path local`, no `-PipeName`), under a `USERNAME` of
  its own (`New-TestPipeName`), and starts nothing unless that pipe is neither this box's own daemon's nor one a
  `sotd.exe` already serves.
- A fixture never shares a process object with the code under test: a suite that ends a child keeps a `Process` of its
  own for it, its handle taken while the child runs (`GetProcessById`), since production disposes its own. A timed fake
  starts its timer at the test's own origin, the arm file the test writes as it starts measuring
  (`FAKE_SOTD_EXIT_ARM_FILE`), never at its own start.
- A test job on a Windows box runs in a test job (`in-job.ps1`), started over ssh as `powershell.exe -NoProfile
  -NonInteractive -ExecutionPolicy Bypass -File scripts/tests/in-job.ps1 CMD...` (through `on-host.sh` or a runner's
  start), and a runner's bound ends in-job.ps1's process, never a parent-id walk (`taskkill /T`, `Stop-SotTree`): Git
  Bash's exec leaves the new process a creator that has exited, so the walk stops above the suite, and Windows
  OpenSSH ends no process of a session whose client is gone (its sshd exits, which in-job.ps1 watches).
- A test job runs in a test container (`in-container.sh`). The five tests that need a user manager run under
  `--user-manager` with `SOT_TEST_REQUIRE_USER_MANAGER=1`, by name (`rust/backend/tests/CLAUDE.md`).
- `rc-gate.sh` runs as the main process of its own test container (`in-container.sh UNIT -- bash rc-gate.sh ...`, no
  `--user-manager`) and exits 2 otherwise: `DBUS_SESSION_BUS_ADDRESS` unset, a private `XDG_RUNTIME_DIR` holding neither
  `bus` nor `systemd`, and a `.service` control group are all required, so its jobs reach no user manager. It lists every
  process left in the container, nested groups included, at the end of a clean run as a leftover, and the container's end
  kills them. Its verdict ends `<logdir>/summary.txt` as `ALLDONE` or `ALLDONE FAILED`; a container stopped from outside
  SIGKILLs rc-gate, so a summary without `ALLDONE` is a failed run.
- No command an `rc-gate.sh` job runs writes into `<logdir>`: `logged` gives it files in a folder of its own in the
  container's private `XDG_RUNTIME_DIR`, on the host's own disk, and appends them to the job's log when the command ends
  (until then the log holds only its header line). `<logdir>` is wherever the caller reads it, often a network home,
  where each close of a written file waits for the file server to commit it, and a child closes the standard streams it
  inherited as it sets up its own, so a test's bound on that setup (a native launch's ready, 10 s) would time the file
  server.
- `CARGO_TARGET_DIR` is the gate's alone while it runs: its jobs run the binaries in it (the tests.tsv paths, `sotd`,
  `sot-capsule`), and summary.txt's first line names one head and tree; a build of another checkout into it would replace
  them mid-run. A target belongs to one checkout for good: the first run names it in `<target>/rc-gate-checkout` and a run
  for any other checkout exits 2 before anything is built (cargo can run another tree's binaries from a shared target).
- `rc-gate.sh` builds the daemon lifetime harness as rust.yml's Linux job does (`sot-capsule` with `native-barrier`, the
  harness with `daemon-lifetime-faults`) in `<target>/daemon-lifetime`, a target of its own, since those features would
  replace the `sotd` and `sot-capsule` the other jobs run; `job_lifetime` runs every case with `--include-ignored
  --test-threads=1`, the Julia cases on the gate's `julia` (`SOT_JULIA_BIN`), after preparing Pluto's environment in the
  gate's depot as rust.yml's job does (`fixtures/warm_pluto.jl`, its log `<logdir>/lifetime-warm.log`; a failure there
  is the job's result) and handing the cases its manifest (`SOT_L2_PLUTO_MANIFEST`); the harness's end is 3600 s, not a
  job's 1200, since it took 963 s inside a whole gate and 1035 s alone, on a shared test host. Two parts go unchecked
  there: no user manager answers in the gate's container, so `a_service_manager_counts_int_and_term_as_a_clean_stop`
  prints `SKIPPED` and passes (it runs by name under `--user-manager`, `rust/backend/tests/CLAUDE.md`), and where the
  jobs' PATH (cargo, julia, node, /usr/bin, /bin) holds no `quarto`, the done test's Quarto half prints NOT CHECKED (the hosted
  job checks it). A failed build leaves the job's result `unrun`, a failure.
- `rc-gate.sh` needs `node` on its PATH, as it needs `julia`, and gives its jobs node alone, through a link in `<logdir>/bin` (julia likewise, in `<logdir>/julia-bin`, so no other binary of its folder shadows one), and `npm ci` done in `rust/backend/sidecars/mathjax` (the
  sotd contract tests and test-agent-layers use them); it refuses, exit 2, rather than install into the checkout it
  judges. It needs `Xvfb` in `/usr/bin` or `/bin`, its jobs' PATH, since the sotd sidecar tests start it, and refuses,
  exit 2, without it. A Rust test binary's stderr goes to `<key>.stderr` beside its log, so the body check reads stdout only.
- `rc-gate.sh` runs rust.yml's three clippy lines as jobs of their own (`lint-lib-bins`; `lint-all-targets` under `CLIPPY_CONF_DIR=<checkout>/rust/clippy-tests`; `lint-windows-tests`, that line for `x86_64-pc-windows-gnu` over sot-log and sot-backend, which finds Windows-only test code from Linux; the frontend's tree-sitter needs a cross C compiler, so the hosted leg keeps that crate), each with a result and log (`lint-<line>`), and `under-bash.sh` under the bash 3.2.57 `bash32.sh` builds once per gate host (`under-bash`, 2400 s). Its optional fourth argument is the line: when `scripts/`, `comm/` and `agents/` equal it at HEAD the job is skipped and summary.txt says so; with no line it always runs (about 10 minutes on a hosted runner, concurrent with the other jobs).
- `rc-gate.sh` gives every job a HOME of its own, `<logdir>/home`, with `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `CLAUDE_CONFIG_DIR` under it and `CARGO_HOME` and `RUSTUP_HOME` named as the runner has them, so a test that depends on the runner's real home fails in the gate as on a hosted runner, and none reaches the fleet's live `~/.sot-comm`, `~/.config/sot` or `~/.claude`. The juliaup resolution, which needs the real home, runs before the jobs.
- `rc-gate.sh`'s julia is the binary the runner's `julia` starts, asked once under the runner's HOME, so a juliaup
  launcher names its installation (on a job's PATH a launcher would install a Julia under each HOME a test makes);
  summary.txt's third line names it (the sotd unit tests resolve juliaup's default channel under the real HOME before the PATH, so on a host whose `julia` is not that default the gate runs two Julias), and the gate refuses, exit 2, when `julia` starts none. The Julia suites and the
  lifetime harness run it, and the Rust jobs get its folder on their PATH, as rust.yml's test jobs get setup-julia's:
  a test daemon is handed it (`support::julia_binary`), so `daemon_boot`'s environment test and `support_checks` check
  that Julia, and `preview_order`'s daemon starts its kernel on it.

- Trust tests execute the real declaration owner and installer entry; emitted-byte assertions and native exit-status
  observations establish behavior, never source membership or statement offsets.
