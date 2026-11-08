# scripts/: distribution (charter)

## Idea
A box runs exactly one tagged release. CI turns a tag into verified artifacts; install, update-apply and launch place,
swap and start that release as whole files under `$PREFIX` (`~/.local/share/sot`, or `%LOCALAPPDATA%\sot` on Windows),
and the launch path fails open: no update step can stop a window from starting.

## Release path (contract; pass 1a)
The maintainer's word starts and ends every release. The cut tags a commit that already passed the local gate; CI turns
the tag into verified artifacts and a draft; the maintainer installs the draft on a box and publishes it. Nothing in the
repository tags, drafts, publishes, edits or deletes a release on its own.

Guarantees, each with the mechanism it rests on:
- Nothing tags or releases on its own. A tag reaches origin only from `release.sh`, after its prompt or a `--yes` the
  maintainer gave. A release is created only as a draft, only by release.yml's `draft` job, which runs only on a tag ref
  and is the only job of release.yml with `contents: write`. Publishing, editing and deleting a release are the
  maintainer's own `gh release` commands. `tests/test-release-cut.sh` scans the workflows: their one release write is that
  draft, and none pushes a tag.
- A tag sorts strictly above every tag of its line, local and origin's, and above the line's stamp, and the stamp after
  it sorts above the tag: `release.sh` asks the updater's own `compare_versions` (through `rust/updater/examples/vercmp.rs`),
  the comparison every installed box makes, before it stamps anything.
- A tag is cut from main, at origin's main, from a clean tree, so the tagged commit's parent is a commit origin already
  holds; the release commit changes only the stamped version files (the workspace version and its members in
  `Cargo.lock`, the version of every Julia `Project.toml`).
- The tag and the line's next `-dev` stamp (`version: <next> opens the line after vX.Y.Z [skip ci]`, decision 0033) reach
  origin in one `git push --atomic`, or neither does. Until the commit a failure restores the stamped files; `--dry-run`
  stamps, shows the diff and restores.
- The cut refuses nothing that only repeats the local gate: it runs no tests and reads no hosted run. Each of its checks
  prints what it found (HEAD and origin's main, the highest tag compared, the stamp, the stamp after the tag).
- The local gate is `tests/rc-gate.sh` (every suite, in a test container that reaches no user manager) and
  `tests/user-manager-tests.sh` (the five tests that need a user manager, each by name with
  `SOT_TEST_REQUIRE_USER_MANAGER=1`, in a test container that keeps it). Each writes a `summary.txt` naming the head and
  tree it ran and ending `ALLDONE` or `ALLDONE FAILED`. The maintainer's word is given on a commit both passed. The
  local gate needs `jq`, a yacc (bash32.sh's build), a git of 2.28 or later (`init -b`) and `Xvfb`; a host without them runs `tests/gate-tools.sh`, which
  puts a checked jq, a built byacc and a built git in `/scratch/<user>/gate-tools/bin` (`GATE_TOOLS_DIR` names another), and the gate
  puts that folder first on every PATH it uses when it exists. The
  local gate holds to five minutes on descent (`under-bash` aside, which leaves with the shell twins under messaging's
  decision D1): the sotd unit binary runs in the cuts `.github/test-shards.txt` gives the hosted shards, and the tests
  that sleep on the clock (the lifetime harness, `window_start`, `window_lease`, the `lane_bridge` outage cases,
  `fe_client`'s health window) and the page test that needs a prepared Julia depot run only in release-gate.yml, or here
  with `RCG_FULL` set.
- A scoped run (`RCG_SCOPE=<base commit-ish>`, given as `env RCG_SCOPE=<base> bash rc-gate.sh ...` inside the container, and
  to `user-manager-tests.sh` the same way) is the gate for a change inside a few pieces. The scope comes from one rule,
  `tests/lib-gate-scope.sh`: the paths that differ between the base and HEAD each map to a piece (`rust <crate dir>`,
  `julia <package>`, `shell comm|agents|scripts`, `docs`; a Rust path a shell or Julia suite proves through the `sotd`
  binary adds that piece: `backend/src/comm` and `rows` add `shell comm` and `julia root`, `agents` adds `shell agents`,
  `install`, `update`, `topology`, `rust/updater` and `rust/topology` add `shell scripts` and `julia root`, and the
  daemon's entry and shared command files add all three shell groups), and a path no piece owns widens the run to the whole, never to
  less: `rust/Cargo.toml`, the lock, the clippy configs, a directory under `rust/` that is not a crate, the gate's own
  scripts (`tests/` outside the shell suites, `setup-publish-guard.sh`, `hooks/`, `lib/`), `.github/`, `.claude/`,
  `julia/pluto`, `examples/`, `dev/`, and an empty diff. Docs alone (`docs/`, markdown outside the pieces) select no test.
  The workspace build, all three clippy lines and both cross checks run in every scope; only test jobs narrow: a touched
  crate's tests, and its doc tests when it has a library target (a binary-only crate has no doc job and no doc result) but not its dependents' tests, which the hosted run on main still runs; the Julia packages
  touched, the shell suites of the touched group, and the
  installer-publish jobs when `scripts` or any crate is touched. `under-bash` is two kinds of job, by the files that differ
  from the base (`gate_scope_ub`): its parse and scan checks, one job `under-bash-parse`, when any file under `comm/`,
  `agents/` or `scripts/` differs (markdown included: the bare in-place `sed` scan reads every tracked file there), and
  each hermetic suite of the selected shell groups under bash 3.2 as its own job `under-bash-<suite key>` when one of
  those files other than markdown differs; with neither, no `under-bash` job (a daemon change that selects shell groups
  through the backend path rule runs those suites under the default bash only). A whole run emits the scans and every
  suite's job, and a whole run given a line keeps its skip when the three folders equal it. `under-bash.sh` with no
  option, as release-gate.yml calls it, does everything one after another; `--parse-only`, `--no-parse` and `--list`
  serve the gate's jobs. Markdown is skipped for the suites because no suite's verdict on a markdown input depends on
  which bash runs it. When the decision selects any bash 3.2 job the run builds bash 3.2 and runs every selected job, or
  fails naming the build (and its path, when one was built), also when the bash built reports another version; it never
  ends `ALLDONE` with a selected job missing. The pipeline starts its jobs in the order of their last
  measured wall time, longest first; a job with no measurement starts before every measured one; every job still starts
  exactly once. The times are kept per checkout in `<CARGO_TARGET_DIR>/rc-gate-times.tsv`, keyed by a name unique to the
  job (a test binary by its target, a shell suite by its path) and written at the run's end from the jobs that ran (a
  failed or timed-out job's time counts; a job that did not run keeps its old time). `user-manager-tests.sh` runs when the scope selects `backend`, `capsule`,
  `platform` or `containment`, since its five tests are the backend's over those. A job the scope
  does not select has no result file; one it selects starts `unrun`, and a selected package or shell group the tree no
  longer has fails (101). A scoped summary says so: `scoped base <sha>` and one `scope <piece>` line each (a widened
  scope says `scope whole <path>`), and it ends `ALLDONE SCOPED`, never `ALLDONE` (`ALLDONE FAILED` as always). A
  release needs a whole run: `.claude/skills/release/SKILL.md` asks for a summary ending exactly `ALLDONE` with no
  `scoped` line, so a scoped run, widened to everything or not, cannot pass there. `RCG_SCOPE` with `RCG_FULL` is
  refused. `tests/test-rc-gate-scope.sh` holds the path table with its widening cases and runs the real pipeline over
  stand-ins, and drives the verdict, summary and refusal functions the entries call and the two entries' refusals; a whole
  run's own container checks are the one part it does not reach.
- Every gate run, scoped or not, also runs `tests/release-features-check.sh --prove` in the cargo chain: no shipped
  binary may resolve sot-chat's `test-support` feature (its fake Slack's committed server key would let anyone impersonate
  Slack's hosts to it; sot-chat's lib.rs also refuses it as a compile error in a build without debug assertions, and `--prove` runs a release-profile check of sot-chat with the feature that must fail with that error; a release-profile test build is refused by design). It reads
  `cargo tree --workspace -e normal,build` for the release targets; `tests/test-rc-gate-scope.sh` pins that it runs with
  `--prove` over the checkout's rust folder. A depot missing the release targets' packages needs `cargo fetch --locked` once.
- Every gate run, scoped or not, also runs `tests/test-defects-check.sh`, which runs `tests/defects-check.sh` on the
  checkout: it fails with the id when a landing record of `docs/history/passes.md` names an item that is not in
  `docs/defects.md`, and when a tracked `CLAUDE.md` cites an item id there that is not one. The rule that tells an id from
  other capitals is `docs/CLAUDE.md`'s.
- The Linux local gate builds `sotd`, `sot` and `sot-capsule` before its jobs, then runs
  `installer-publish-guard.py` for `fresh` and `repair`, each with its own log, result and named summary line.
  `tests/test-rc-gate-installer.sh` proves both jobs execute and retain their own status through the real pipeline.
  The `install-smoke` job runs `install-smoke.sh fresh-local` (a fresh install, then a repair run, each judged by its
  verdict line, so a `ONE STEP LEFT` fails it) on a pack the job makes from the gate's own build of the checkout: the
  three binaries, the unit and `sot-apply`, under the built `sotd`'s version. The smoke installs into a scratch home in a
  folder of the job's own (its `TMPDIR`), removed when the job ends. Its git fetch of the pack's commit is rewritten to
  the gate's checkout, which may be detached at a fetched commit. The job gets the Julia the gate found and the Julia
  depot the gate was started with (`JULIA_DEPOT_PATH`, passed on, never derived), so the installer's Julia environments
  come from that depot; the installer may still reach the network for a package the depot lacks. A whole run does the
  job. A scoped run does it when the scope selects the `shell scripts` piece, which `lib-gate-scope.sh` gives every
  `scripts/` path (the gate's own scripts widen the run to whole) and the Rust paths the installer is built from
  (`install`, `update`, `topology`, `updater`, `main.rs` and the files beside it); a change only to the pack's own
  inputs, such as `rust/capsule` or `rust/platform`, does not trigger it. Measured on a warm depot: 2 minutes of its own,
  run beside the other jobs. Its limit: it is a debug build on the gate host's libc, not the release archive, and
  `fresh-be-only`, the update scenarios and the Windows smoke stay in release.yml.
- The hosted runs are two. rust.yml, on main after each batch of merges (since 2026-10-26, the maintainer's decision; a
  red run is fixed forward in the next landing or its landing reverted), is the build and the unit tests sharded on all three systems by
  `.github/test-shards.txt`; its `release SYSTEM shard...` lines defer selected shards to that system's release gate.
  Each job is bound by `timeout-minutes`, with a check that every test of a named target runs in
  exactly one shard (`tests/shards.py check`), in five to seven minutes. In the build+test job every step has its own
  bound and the job's is their sum plus two minutes, so a slow runner fails in the step it slowed, named, never as a
  job ceiling with every step green (seen 2026-11-03 on a macOS runner at a quarter speed). CI.yml's preview tools (ghostscript, poppler, ffmpeg) come from a cache keyed by the runner
  image and the package list, so a landing does not wait on a package mirror. release-gate.yml, dispatched by hand at the
  release gate (pass 11 and before each final release), never on a tag, holds the rest: the `gate-*` shards (the tests
  that sleep, the WGL page test with its depot), the native window proofs (minimized, closed, pane timing), the daemon
  lifetime harness, the hermetic comm and agents suites, the gate's own parity tests, Windows' slow installer publish
  proofs (`inst` plus fresh/repair orchestration), the launcher's suites on Windows, the Windows ignored-containment
  and ConPTY runs, and the shell suites under bash 3.2
  (Linux and macOS; they leave with the shell twins under D1). No cut reads either.
- rust.yml's MathJax `npm ci` step has its own three-minute bound, so a registry stall fails under that step's name.
- A release's artifacts are CI's build of its tag. release.yml sets `SOT_BUILD_ORIGIN=ci` exactly when the ref is a tag,
  so only a tag run's binaries carry the bare version (`is_release_build`); a branch dispatch's and a local pack's carry
  a `+`. The smokes check that each updater-bearing binary says the run's version, that the Linux `sotd` and `sot-capsule`
  (musl static) boot on glibc 2.31, and that `sotd` answers a hello.
- A draft exists only after every artifact smoke, `julia-check` and every install smoke of its run passed (`draft` needs
  them all): a release is installed before it is published. It carries the three archives, `COMMIT` (the tag's commit)
  and `SHA256SUMS` over all of them, the names the updater and the installers read.
- A draft becomes public only by the maintainer's `gh release edit vX.Y.Z --draft=false`, run alone. Before that a box
  takes the candidate only by an archive run of the draft's own files (`install.sh --archive`, `install.ps1 -Archive`),
  which pins it.
- A dispatch of release.yml on a branch runs the same build and smokes, names its artifacts `0.0.0-ci.<sha>` and drafts
  nothing.
- A local pack (`pack-build.sh`, `pack-build.ps1`) is never a release artifact: it unsets `SOT_BUILD_ORIGIN`, so its
  version carries `+`, and it packs only a clean HEAD that a remote branch holds.

Owns: `release.sh`; `rust/updater/examples/vercmp.rs`; `.github/` (release.yml; rust.yml, the per-landing run, and
release-gate.yml, the release gate's hosted run, with `test-shards.txt` their one table of test shards, and CI.yml;
gitleaks.yml; dependabot); the local gate (`tests/rc-gate.sh`,
`tests/user-manager-tests.sh`); `tests/release-before.sh`; `pack-build.sh`, `pack-build.ps1`; `cliff.toml`; the release
skill (`.claude/skills/release/SKILL.md`). On GitHub: the `v*` tags and the releases with their assets.

Calls in: an installed box's updater lists published releases only, compares versions with `compare_versions`, and reads
`sot-<version>-<target>.tar.gz` (`.zip` on Windows), `SHA256SUMS` and `COMMIT`; `install.sh` and `install.ps1` fetch
their own copy at their tag, so every tag holds them; every build of main carries the line's stamp, which the cut
moves to the next `-dev` version.

Calls out: `compare_versions` (through `vercmp`); git and origin; `cargo update --workspace` for the lock. release.yml
runs the install smokes (`tests/install-smoke.sh`, `tests/install-smoke.ps1`, the installers' half below) on the run's
artifacts, `release-before.sh` (which asks the run's `sotd update direction`), git-cliff for the notes, and GitHub's
API for the draft.

Does not: run tests or read hosted runs at the cut; publish, edit or delete a release; build a release's artifacts
outside CI; decide what a box installs or how (install, update, apply and launch, below, are the installers' half).

Platforms: `release.sh`, the local gate and `pack-build.sh` run on Linux (the gate's two containers are systemd user
services, `tests/in-container.sh`); `pack-build.ps1` on Windows PowerShell 5.1. release.yml builds on ubuntu-22.04,
windows-latest and macos-15 (GitHub retires the macos-14 image on 2026-11-02) and smokes and installs on all three.

Both Linux gate entries acquire the same nonblocking `flock` on `<CARGO_TARGET_DIR>/gate.lock` inside their
container, before setup or builds, and retain its descriptor through the run. A second gate on that folder
fails immediately naming the folder; a different target can run concurrently. The lock file is never unlinked.
`tests/test-gate-target-lock.py` exercises both entry points against each other, target aliases, separate targets
and lock release using recording setup tools, with no build or user manager.

Tests: `tests/test-release-cut.sh` (the cut in throwaway repos with a scratch origin, and the workflows' release
writes), `tests/test-release-before.sh`, `tests/test-rc-gate-*.sh`, the order in `rust/updater/src/semver_tests.rs`, and
a dispatch of release.yml for the pipeline. rust.yml's checks job runs the gate scripts' own tests and the installer suites, and its Linux rest-0 build+test job runs the window's State fixtures alone (the `window_minimized` target's `child fixtures`) under `xvfb-run` with Mesa's lavapipe, its log naming the adapter;
release-gate.yml's slow checks job runs the comm and agents suites.

rust.yml's Julia setup step has a five-minute bound. Windows build/test jobs allow 17 minutes, preserving their
original 12-minute budget after setup; a stalled extraction fails in the named setup step.

## Installers (contract; pass 1b)
A box runs one tagged release, laid under `$PREFIX` and recorded in `$PREFIX/install.json`. Install puts a release
there and says in one line whether the box is whole; update stages and arms a newer one; apply swaps it in offline;
launch starts a window and never stops on an update. The entry is `install.sh` (Linux, macOS) or `install.ps1`
(Windows), a bootstrap that fetches and verifies a pack and runs its `sotd install` (`rust/backend/src/install/`),
where every rule of install lives once for all three systems.

Guarantees, each with the mechanism it rests on. Install:
- One verdict. Every run ends in exactly one line, `PASS: Ship of Tools <v> at <short> on this computer`, `ONE STEP
  LEFT: <steps>` (exit 0: the install is whole) or `FAIL: <check>: <detail>` (exit 1; 2 for a refusal), on standard
  output after every other line. A Unix run that reaches its checks prints `Checklist:` immediately before that last
  line: an `ok` or `made` line for each check that passed, in order (binaries at the version, install record,
  checkout, comm and skills at the commit, Julia environments and the state folder and Claude Code for a daemon role,
  the daemon at the version, each clone the publish guard found ready), then each step left as `todo`, then the
  failed check as `FAIL`, so it holds everything the verdict names. A stop before the checks prints no checklist, and
  Windows records no items yet, so it prints the verdict alone. `sotd install` prints it from one place
  (`verdict.rs`), on a signal too: the running step and every process it started end first, then the line names the
  step, and it exits 128 plus the signal. The
  bootstrap passes a signal it gets on to `sotd install` (as TERM), and prints a verdict only before `sotd install`
  runs or when it ended without one.
- Verified before it runs. The bootstrap runs nothing from a pack until every file it downloaded or copied matches
  `SHA256SUMS` (a file the list does not name is refused) and the unpacked `sotd` runs here. `sotd install` then reads
  the pack's identity (the one archive `SHA256SUMS` names for this system, `COMMIT`, and the `--version` of each
  binary this run's role starts: `sot` only for a role that runs the window, so a backend-only box whose libc the
  window cannot load still installs) and refuses a pack whose parts disagree. This is why the bootstrap is a script:
  it must verify before anything runs.
- A tag's own code. A run that names no pack runs its tag's own installer: the prelude resolves the tag (named, or
  the channel below), fetches that tag's `scripts/install.sh` (`install.ps1`) and runs it; that body runs the tag's own
  `sotd install`. `--archive`/`-Archive` runs the bootstrap given with the pack's own `sotd`. A release whose `sotd` has
  no `install` (0.6.6 and earlier) is installed only by its own installer.
- Nothing written before every gate. A run refused by its options, the prefix's or home's characters (Unix: none of
  `% \ ' " $ ` & | ;`, space, tab, newline, which a unit, launcher or shell line would read as syntax), `jq` missing
  (comm needs it), the direction, the role and ownership gates or the daemon question writes nothing under the prefix,
  `~/.local/bin`, the unit folder or the comm home: `sotd install` decides from read-only facts first, then writes.
- Never backwards unasked. A run that names no target follows the channel install.json records (no install or a
  stable release: GitHub's latest stable; a pre-release or a `+` build: the newest published release) and refuses a
  target `compare_versions` calls older, or one it cannot compare while the installed version is readable (the
  record's, else the installed `sotd --version`'s). A `+` build is told by ancestry (its commit against the pack's
  `COMMIT`, in `repo/base`) which way the newest release moves it, and stops. `--version`/`--archive` moves either way.
- The pin. Only a run's own version or archive argument (`--version`/`--archive`, `-Version`/`-Archive`) names its
  target and pins the install (`"pinned": true`); one that names none removes the pin and is implicit for the
  never-backwards rule too. `SOT_INSTALL_TAG` selects the release's body and never pins it, including when an older
  prelude passes it without `SOT_INSTALL_IMPLICIT`. Current preludes keep sending `SOT_INSTALL_IMPLICIT` for older
  bodies that read it; this body needs no marker. The automatic writers never arm over a pin or over a record that
  cannot be read (`InstalledBuild::admits`). Every
  verdict line of a pinning run says so once the pin is written.
- A repair run moves nothing. A named run of the installed version and commit that would change nothing else the
  readable record says (the role, the hub, the prefix; `record::keeps`), with no daemon on another version, writes the
  pin, drops any pointer armed over the install, and stops, restarts, stages and applies nothing except the Unix
  state-folder correction below; its other steps lay
  again what is there (the same binaries and checkout, the environments' own manifests kept), so a repeat finishes a
  half-done install, and the record gets its pin and no other change. A run of the same build that changes the role
  or the hub is an install over it, and writes the record whole. Apart from that correction, it stops and restarts
  nothing by construction: on Unix a repair is a run for which `ends_daemon`, the predicate the version restart and
  the retire act on, is false; on Windows the repair returns before the stage.
- The record last, whole, in its old shape. `install.json` is written last, beside itself and renamed in (no BOM), one
  key per line with schema 1's keys, the shape every earlier release's readers take (line readers in the scripts,
  `InstallManifest`). A repeat keeps the rollback: a `.prev` copy is made only of a binary that differs, and a prune
  keeps the previous version folder and names and keeps any folder with local changes. Version cleanup removes only
  the exact available worktree through Git; a refused removal keeps its folder, and unavailable worktree registrations
  are never pruned (the common repository may serve checkouts on another computer).
- A live daemon is asked before it ends. A daemon is restarted only when it runs another version (`sotd same-version`),
  only where this run owns it (Unix: an active `sotd.service` no window holds; Windows: the window's supervisor, by its
  converge), and stopped, whatever its version, only where the run's role has no daemon here (Unix: the retire of an
  active `sotd.service`) or an update applies at the desk (Windows); each only once its live work (`sotd update repls`:
  REPL rows and the Pluto server; a daemon that cannot say is judged by its own child processes, by parent id) is
  answered: yes at a console, or `--restart-daemon`/`-RestartDaemon`; else the run stops changing nothing. A restart
  must answer as the target within its bound by the clock. A daemon that runs and cannot be told its version is never
  read as none. The state-folder correction below is the one additional restart: rows outlive it, and no row could
  have started on the unqualified root, so it asks nothing.
- The role is declared (Unix). hosts.toml's entry for this host decides (a frontend runs its own daemon); else the
  role flag; else what install.json records; else a question at the console; else FAIL 2. A host hosts.toml runs no
  sotd on never writes over an install at the prefix, a run never replaces another install's unit, wrapper or desktop
  entry, and under a declared topology a run never records less than install.json does (`--force-role-change` waives
  the three; an unrecognized file is never overwritten).
  When a declared frontend's previous record says it had no daemon, adding its own daemon is named as a checked
  detail in the verdict, with no step for the person.
- The unit is pinned before any reload (Unix). `sotd topology pin` (`--claim` for this host with no hosts.toml) writes
  the unit's host pin before the unit is written, reloaded or started; a pin that fails stops the run with no unit
  written, and a retire's pin that fails stops it with nothing stopped or reloaded. Boot start is the drop-in, never `enable` (on a shared home an enable links the unit for every host): wired
  for a unit the run finds new, kept over an existing one only where a link or the drop-in was. A retire never
  disables the shared unit or removes its file, and drops the boot start only where the pin names this host alone.
  The pin command the retire tells a person to run is the install's own arguments (`pin_args`), by construction.
- A managed Linux daemon survives logout through user linger. The installer checks `loginctl show-user` first and
  runs `loginctl enable-linger` only when linger is not already reported enabled. Each call is bounded at 30 s through
  the installer's bounded runner. A failed or unrunnable enable does not stop install: it adds ONE STEP LEFT with
  the exact `loginctl enable-linger` command for this account, also printed as a `todo` in the checklist.
- The state folder qualifies by the row launcher's own `qualify_state_root`, never a second filesystem list. Unix
  resolves it with `sot_state_dir_in` from the installer's environment, where an exported `XDG_STATE_HOME` decides
  even when empty; with none exported, a managed unit's own `XDG_STATE_HOME` (a person's drop-in), read after the
  pin and a daemon-reload, by that one name, from `systemctl --user show sotd.service -p Environment --value`, an
  entry systemd prints quoted skipped whole; else `$HOME/.local/state`; joined with `sot`. A nonempty value of
  either is an explicit choice. An unqualified explicit choice is left alone and ends ONE STEP LEFT,
  naming the folder, the qualification's reason and the export to change (the unit sources `~/.bashrc`, whose
  export wins over its drop-in). On Linux with a managed daemon unit and no explicit choice, the installer tries
  `/scratch/<user>/sot-state/sot` only when `/scratch` is a directory and its account folder is absent or a real
  directory owned by this uid. It creates and qualifies the private root, then durably publishes
  `sotd.service.d/state.conf` setting `XDG_STATE_HOME=/scratch/<user>/sot-state`, after the host pin and before
  the daemon-reload that precedes the start. An absent drop-in is published without replacing anything; identical
  bytes are kept; every other entry is the person's and is left alone with ONE STEP LEFT, read before anything is
  made under `/scratch`. A newly written drop-in gets `try-restart`, even on a
  repair. A shared home carries the same drop-in to each computer, whose own local folder is created at its first
  daemon start. No usable candidate gives ONE STEP LEFT with commands to make a local folder, write the drop-in,
  reload and restart. A daemon role without a managed unit (macOS, `--no-service`) gets the same qualification, and
  a failure gives ONE STEP LEFT naming the folder, the reason and the export to change. A computer that runs no
  daemon has no state folder to check.
- The Julia the daemon will run meets the kernel's floor (`[compat] julia` of `julia/kernel/Project.toml`): the
  daemon's (`SOT_JULIA_BIN`, else juliaup's default read from `juliaup.json`, else PATH) and the comm refresh's (PATH).
  Below it the run ends ONE STEP LEFT naming the fix; a PATH julia below it ends the run before a Julia step. Julia
  steps run with `TMPDIR` inside the run's scratch folder, which the run removes. The comm step runs with
  `--compiled-modules=no`, so a ShipTools cache compiled through `repo\current` cannot fail an elevated run on a
  Windows 11 computer (`the_comm_step_reads_no_precompile_cache`).
- What it left is checked. The verdict reads the disk and the processes: the binaries' versions, the record,
  `repo/current`, no pointer armed again, the comm stamp, the Julia manifests of a daemon role, the state folder,
  Claude Code through agents' executable resolver with the installer's PATH and home, and the daemon's answer;
  missing Claude Code gives ONE STEP LEFT to install it on this computer;
  a step only a person takes (a window on the old version, a daemon a window holds) is ONE STEP LEFT.
  On every completed install or repair, Unix and Windows use one shared check to ask this computer's own daemon
  once through `version.query`, within 3 s. A connected `fe` whose declared host is this computer and whose
  nonempty `app_version` differs from the installed version adds a restart step naming both versions, regardless
  of whether the install record changed. No daemon, no reply or an unreadable reply adds nothing and fails nothing.
  On Unix this adds to the step a window seen before the run earns when the record moved, since such a window may
  not have reconnected yet.
- Windows files belong to the running account. After staging, before reporting the update armed or applying it, an
  elevated run corrects any different owners recursively under its prefix and config folder, skipping reparse
  points; an unelevated run cannot change an owner and walks nothing, so an old file an elevated run left does not
  fail it. Every run then reads the checkout's owner back. The correction and read-back share a 300 s bounded process.
  Fresh installs and repairs receive the same check, repeated after the final files are written. A failure drops
  the staged pointer (a failed drop is named), reports FAIL with the owner found or that it was unreadable, and
  tells the person to run the installer again unelevated as the account that uses Ship of Tools; it never reports
  the update armed on that path.
- Bounded waits. curl runs with `--connect-timeout 15 --speed-limit 1000 --speed-time 60`, every git call with
  `http.lowSpeedLimit=1000` and `http.lowSpeedTime=60`, ssh with the tree's `SSH_OPTS` under a 30 s bound; a converge
  waits at most `SOT_INSTALL_CONVERGE_WAIT_S` (1800 s), a restarted daemon `SOT_INSTALL_RESTART_WAIT_S` (Unix, 30 s) or
  `SOT_INSTALL_DAEMON_WAIT_S` (Windows, 60 s). Publish enrollment's child commands have the stronger wall-clock and
  output-completion bound below. The installer's own process listing (`procs::snapshot`: `ps` on Unix, CIM's
  `Win32_Process` on Windows) runs through the same bounded runner at 60 s, and so do the Windows end-state check's
  `--version` asks of the installed binaries. A listing that was cut, did not start, was ended by a signal, exited
  nonzero, printed no process, or printed what its program never prints is never an empty list: `ps` must give a
  `pid ppid name args` row on every nonblank line, and CIM one object or an array of objects, each with an integer
  `ProcessId` and `ParentProcessId` and a `Name`, `ExecutablePath` and `CommandLine` each a string or null, so `null`,
  `{}`, `[null]`, a string or an object missing a pid is unreadable. Whether a daemon of the install runs is then
  presumed yes, so it is asked about (a daemon's children it cannot list rule nothing out); Unix decide stops before any write
  (FAIL, exit 1, naming why); a Windows update after its stage stops nothing and stays armed (ONE STEP LEFT naming
  why); and the final checks are FAIL with the reason. A `--version` that was cut or gave no answer is FAIL with its
  reason. Every other observation keeps its own wait. The tests read each listing shape through the real runner on
  every system, and each caller's meaning through its own function; tests that drive decide, the update and the
  checks with a cut listing wait for 0.7.1's fake-machine seam. Whole-install completion on Windows in an isolated profile
  remains unproven until the release-gate witness completes.

Update (`rust/updater`, `rust/backend/src/update*`, `rust/frontend/src/selfupdate.rs`):
- An automatic arm goes only strictly above install.json's version, never into a dev build, a pinned or unreadable
  install, or an install on a host the topology names with neither key. `sotd update stage` is the operator's and may
  arm at or below. A stage verifies the sums and digests and prepares the checkout at the tag's commit, clean, under
  the stage lock, before it arms.

Apply (`sot-apply.sh`, `sot-apply.ps1`):
- It verifies the whole stage before it mutates, restores on a failure after mutation and exits 0 (fail-open); a
  rollback marks the bad tag so it is never armed again; comm follows every apply (fail-open, bounded by `timeout`).
- A pointer applies only onto the install it was armed over: the installed `sotd update pointer` answers above the
  hand-off, and a refused pointer is dropped under the lock (`--drop-only`/`-DropOnly` apply nothing).
- When the prepared checkout's HEAD cannot be read or disagrees with the pinned commit, apply keeps Git's output
  and exit code on the one log line that drops the pointer. It still leaves the install and staged files alone.
- A release runs its own apply at every later update: the installed apply hands off once to the staged release's
  (`SOT_APPLY_REEXECED`; Unix: the archive's executable `sot-apply`, which reads the prefix from where the stage sits).

Launch (`launch-sot.sh`, the rendered `sot-launch`, `lib/sot-daemon.sh`, `launch-sot.ps1` and the files it sources):
- It fails open: every update or freshness step logs and the window still starts. A launcher run from a clone of main
  says first, before any pull, that a box should run a release and how to install one
  (`agents/tests/test-launcher-main-notice.sh`; the PowerShell launcher has the same note in its prelude, and no test
  executes it, locally or hosted), and carries on.
- It keeps this computer's daemon only when it runs the launch's version, unless a window holds it (`sotd
  same-version` exit 4); one launcher per user; on Windows no window spawns without a lease held across the gap.
- On Windows, before every window start (the first, -Local and each relaunch), nonempty `frontend.stdout.log` and
  `frontend.stderr.log` move to `frontend.stdout.prev.log` and `frontend.stderr.prev.log`, replacing older backups.
  Empty logs leave the backups alone; a failed move is logged in `supervisor.log` and the window still starts.
  `supervisor.log` names both current output files on an exit other than 0, 75 (relaunch) or 76 (converge).
  `tests/test-launcher-leases.ps1` case 17 executes both spawn paths and the relaunch loop with a stand-in window:
  exit 101's stderr survives the next start, empty output preserves it, and 75 and 76 rotate before respawning.
  Linux and macOS log rotation is out of scope: the window's output goes to the desktop session's log.
- A script reaches a daemon only through `sotd stdio-bridge --endpoint` and finds it by its socket or pipe owner,
  never by a pattern over command lines; socket and pipe paths come from `sotd session-socket-path`.

What moved and what stayed. Moved into `sotd install`: both entry bodies (options, the pack, direction, pin, repair,
role and gates, the daemon question, layout, checkout, Julia, comm, trust, unit and launcher wiring, record, checks,
verdict). Stayed in a script, and why: the bootstraps (they verify before anything runs); the renders of the unit,
the wrappers and the boot drop-in in `lib/sot-daemon.sh` (the apply and the rendered wrapper run them too, each from
the checkout it applies); apply (every installed release execs the staged `sot-apply` or `sot-apply.ps1`; moving it
is its own change, with the update-apply legs as its test); launch (what the shortcut and the unit run and what
restarts the window, ADR 0017; a move needs window boxes this pass may not touch); the Windows shortcut, its AUMID
and the hand-install record (`install-shortcut.ps1`, `set-shortcut-aumid.ps1`, `install-manifest.ps1`: COM, and the
launcher's first start runs them).

Owns: `install.sh`, `install.ps1`, `rust/backend/src/install/` (`sotd install`), `sot-apply.sh`, `sot-apply.ps1`,
`lib/` (`sot-daemon.sh`, `sot-hosts.sh`), `launch-sot.sh`, `launch-sot.ps1` and the files it sources (`sot-hosts.ps1`,
`sot-install-layout.ps1`, `sot-freshness.ps1`, `sot-lease.ps1`), `launch-splash.ps1`, `sot-local-daemon.ps1`,
`relaunch-sot.ps1`, `shutdown-sot.ps1`, `install-shortcut.ps1`, `install-manifest.ps1`, `set-shortcut-aumid.ps1`,
`restart-backend.sh`, `deploy/sotd.service`, `sot.svg`, `four-numbers.jl`, `rust/updater`, `rust/backend/src/update*`,
`rust/frontend/src/selfupdate.rs`, `rust/frontend/src/relaunch.rs`, the comm installer (`src/`, `test/`:
`ShipTools.install_comm`, `update_comm`) and the install smokes. On disk: `$PREFIX` (`bin/` with `.prev` copies,
`repo/{base,versions/<tag>,current}`, `install.json`, `updates/`, `logs/`), `~/.local/bin/sot-launch`, the desktop
entry or app bundle, `~/.config/systemd/user/sotd.service` with its pin and boot drop-in, the Windows shortcut.

Calls in: a person or an agent runs the one-liner, "update sot from the repo" (the entry from `repo/current`), or
`--archive`; release.yml's install smokes run the entry of the run's checkout on its artifacts; the daemon's
`update.apply` and the unit's `ExecStartPre=-` run the installed apply; a window starts through `sot-launch` or the
shortcut. The interfaces that do not move: the entry paths and the option names (`install.sh`: `--version`,
`--archive`, `--local`, `--backend`, `--be-only`, `--prefix`, `--hub`, `--no-service`, `--force-role-change`,
`--restart-daemon`; `install.ps1`: `-Version`, `-Archive`, `-Hub`, `-Prefix`, `-RestartDaemon`; a new option is added,
never renamed), the verdict words, `$PREFIX`'s layout, `install.json`, the unit names, `SOT_INSTALL_REPO`,
`SOT_INSTALL_TAG`.

Calls out: rows' `qualify_state_root`, agents' `resolve_claude`, platform's state resolver and host facts, and topology's
`SSH_OPTS`; `sotd update direction|repls|stage|pointer`, `sotd same-version`, `sotd topology status|plan|sync|pin`,
`sotd trust declare`, `sotd session-socket-path`, `sotd stdio-bridge`, run as programs; git, curl, tar, julia and juliaup,
npm, systemctl, loginctl and ps; on Windows powershell.exe for the shortcut, the converge (`relaunch-sot.ps1 -Converge`), the stop of a daemon
(`sot-local-daemon.ps1 -Stop`) and the apply.

Does not: write hosts.toml (the hub's daemon does); set or derive a Julia depot; install a package a box lacks (git,
curl, tar and jq are named with their install command, never installed; Julia is installed through juliaup on Unix
only); tag or publish; end a row.

Platforms: Linux runs the unit (systemd user manager, linger) and a desktop entry; macOS has no service manager wiring
(the wrapper starts the daemon) and an app bundle; Windows runs a junction for `repo\current`, a supervisor
(`launch-sot.ps1`), the shortcut, stages an update with `sotd update stage` and applies it by what runs: the converge,
the start at the desk, or the apply here in a desktop session. Scripts: `sot-apply.sh` is POSIX sh plus `local`;
every other `.sh` runs under bash 3.2 (`tests/under-bash.sh`), makes temporary files from a template in
`${TMPDIR:-/tmp}`, and runs a utility under `set -e` through `env`, never the `command` builtin; `.ps1` files are
Windows PowerShell 5.1 with ASCII-only string literals and call native commands in a `Continue` scope
(`Invoke-SotNative`), parsed by rust.yml's "Parse PowerShell scripts" step. The launcher code id
(`Get-SotLauncherCodeId`) hashes every file `launch-sot.ps1` sources, and `sotd install` runs that function of the
checkout rather than keeping a copy; the bounds the launcher and the daemon share live
in `rust/protocol/src/ops/lease.rs` (`launcher_bounds_match_ops`).

Tests: `install::tests::only_a_runs_arguments_pin_it_even_with_a_bootstrap_tag` checks the record's pin, verdict and
downgrade guard with a bootstrap tag and no implicit marker, and with either marker value, on all systems.
One test per guarantee: a Rust unit test in `rust/backend/src/install/*_tests.rs` (`cargo test -p sot-backend
install::`), named for its guarantee, for what `sotd install` decides; a case of `tests/installer-entry.sh` for the
bootstrap's share and for what needs the real `sotd install` as a process (the binaries a backend-only run starts, a
signal); the install smokes (`tests/install-smoke.sh`, `tests/install-smoke.ps1`, the jobs
`install-linux`, `install-macos`, `install-windows` that release.yml's `draft` needs): a fresh install, an update from
the release before (`tests/release-before.sh`, by version order), update-apply (the release before's installed apply
hands off to the staged release's), update-implicit (no arguments against the real repository: a repair run at the
release before that clears its pin, then the run's release stages as newer) and a repair run, each judged by its
verdict and the state it left; the apply, library and wrapper suites (`tests/installer-*.sh`, `tests/test-*.ps1`); the
integration tests `shell_dial`, `unit_pin`, `same_version`, `window_start`.
The HEAD diagnostic regression (`tests/installer-head-error.py`) runs native apply with real Git on Linux/macOS
and Windows PowerShell 5.1. `install::window`'s regression uses a private Unix socket on Linux/macOS to exercise
the shared version query and verdict. `install::role`'s regression exercises the declared-role notice and status
rendering on Unix. Windows' ignored ownership regression needs an elevated Windows PowerShell 5.1 account to
seed Administrators-owned fixture files; it runs the production ownership correction and reads the owners back. A
hosted Windows runner is elevated, so `rust.yml`'s Windows `sotd-c` job runs it by its exact name at every landing and
fails unless exactly that one test passed.

## Publish guard (landings A and B; B execution evidence pending)

Distribution owns `hooks/pre-push` (the versioned scanner), `hooks/pre-push-shim` (the common-directory entry),
`hooks/publish-guard-lib.sh` (policy-location and encoding validation), `hooks/publish-guard-scan.awk` (batch framing,
raw graph traversal and matching), and `setup-publish-guard.sh` (the only enrollment owner).
The private list and the private operations archive are never public repository assets.

Guarantees and boundaries:

- A supported development entry (`tests/rc-gate.sh` with its checkout argument, `release.sh`, `pack-build.sh`, or
  `pack-build.ps1`) enrolls its clone before builds, stamps, tags or publication. Setup installs an executable thin shim
  in the physical common Git directory's default `hooks/pre-push` slot. Linked worktrees share that shim, which runs
  the pushing worktree's `scripts/hooks/pre-push` and refuses if that implementation is missing. Source setup follows
  [the manual](../docs/src/start/install.md#publish-guard-setup) before raw Cargo commands. Landing B's installer
  integration is implemented here but remains pending validation: no passing install or baseline-red evidence is
  claimed by these edits. Its native owner (`install/publish.rs`) captures caller facts read-only before install
  gates; after the verified target checkout exists it enrolls the installed base and the caller when its origin has
  this repository's exact HTTPS/SSH identity (or the existing local test origin). Physical common directories
  deduplicate linked worktrees. An unrelated caller is named and kept; a caller outside Git adds no clone. Absolute
  installer invocation from outside a source clone cannot identify that source clone.
  The shell pack entry runs read-only platform, libc and tree preflight before enrollment; output creation and builds
  follow enrollment. Inherited Git repository selectors cannot redirect setup away from its named checkout.
- For a normal push from an enrolled checkout, the guard scans exact selected commit snapshots: raw commit metadata
  and messages, full unquoted paths, blob bytes including binary and symlink targets, branch/tag destination names,
  raw annotated tag objects (including tagger identity and embedded name), nested tags and tree/blob targets. Gitlinks
  contribute their path and OID, not a fetched submodule repository. Objects are deduplicated; paths retain their tree
  context. Policy files must be valid UTF-8 without BOM or NUL; CR, LF and TAB are allowed. UTF-16 in either byte order,
  a UTF-8 BOM, NUL and invalid UTF-8 refuse at readiness and push, including errors in comment lines. Non-ASCII UTF-8
  comments are allowed. Entries match as whole words, bounded by any character that is not a letter or digit
  (ASCII `[A-Za-z0-9]`), or by the start or end of the text. This applies to every entry, including dotted names:
  paths, host labels, `@`, hyphens and punctuation supply boundaries; longer words do not. Matching remains literal
  bytes under `LC_ALL=C`, case-insensitive for ASCII letters only: a non-ASCII entry matches identical UTF-8 bytes,
  without Unicode case folding or normalization. Blank/comment lines are ignored; `!span` erases literal permitted
  spans before matching. CRLF lists and an unterminated last line keep original line numbers.
- Commit history is bounded by the push's old OIDs plus what the destination advertises at push time: one
  `git ls-remote` per destination URL (git calls the hook once per URL), which fetches no object. The hook first
  requires `git ls-remote --get-url -- <url>` to print exactly the URL git handed the hook, because ls-remote applies
  `insteadOf` again and reads a configured remote named like a bare-word URL; any difference refuses
  (`destination advertisement would come from another URL`), as does any ls-remote error. A fetch-through-a-mirror,
  push-upstream configuration therefore refuses every push from an enrolled clone. A destination that needs
  `remote.<name>.uploadpack` also refuses, since the option is not honored, and the read has no deadline beyond git's
  own. An empty advertisement from an empty remote is valid and excludes nothing. The advertisement is trusted, within
  "client hooks are an owner's protection, not a server security boundary". The bound excludes only commits and tags
  available locally and their locally available commit ancestry. Unrelated remote-tracking refs never bound the scan,
  and no tracking ref is read. A new branch or tag from an enrolled clone scans only history that this destination does
  not advertise; new snapshots, ref names and tag objects still receive their normal scans. A retained denied file in a
  new snapshot still refuses after an ancestor was published. Unavailable old OIDs can conservatively recheck history.
  Raw parent traversal refuses missing shallow/promisor objects; local object enumeration precedes reads, so the
  hook never fetches to complete a scan. In a nonshallow repository, locally available
  old/advertised tags are peeled in raw batches; their commit targets and the locally available old/advertised commits
  seed one `rev-list` traversal.
  Replacements stay off, `core.commitGraph=false` disables cached parents, and an owned empty `GIT_GRAFT_FILE` disables
  grafts, with its selection checked through `rev-parse --git-path`. `--missing=print` disables implicit fetching.
  `--ignore-missing` leaves unreadable starting revisions to the raw audit. One batch audits the enumerated local
  commits and all seeds' raw parent headers; any remaining raw edges use the original walk. A nonzero `rev-list` exit
  or an invalid ancestry list discards the traversal output and uses the raw walk from the peeled seeds, without a
  Git version gate. Shallow repositories and a graft override that Git does not select use that raw walk throughout;
  other Git/read/scanner failures refuse.
  The new commit/tag walk reads only its selected frontier, one batch per generation; tree walks read one selected
  depth level per batch, preserving every path context. Each round checks catalog membership before reading; the
  catalog supplies types, never additional requests. Distinct selected blobs use one
  final batch, including published blobs retained in new snapshots. Raw tree mode/name/NUL/binary-OID records retain
  unquoted path bytes. No diff, textconv or replacement object changes its inspected bytes. NUL bytes delimit
  single-line matching. A CR in a blob reaches the matcher as `\001`, another byte that is no letter or digit, so a
  match is unchanged; Git for Windows' awk drops a CR before a line's end, which miscounted a CRLF blob against its
  size and refused every push that carried one (red on Windows from 2026-10-26 16:20 to this fix).
- On every push, one raw common-clone-local `sot.publishList` value must name an absolute readable regular file outside
  every repository and Git metadata. There is no global, system or command-config fallback. Missing, multiple,
  relative, linked, inside-repository or unresolvable policy paths refuse, including deletion-only pushes. Directory
  aliases are resolved physically on Unix; Windows reparse aliases refuse. The hook and readiness check explicitly
  require a readable, successfully loaded validator with its required functions and successful validator calls.
  Validation and Git/read/scanner producer failures refuse. The guard's own policy-read diagnostics suppress utility
  and shell error text, including a policy path lost between validation and reading. Guard-generated refusal lines omit
  policy paths, private entries, offending files and denied ref names. Content refusals identify
  an object OID and list line; ref refusals identify namespace, update ordinal, source OID and list line.
- The provisioning location is `publish-list.txt` in platform's config folder: `$XDG_CONFIG_HOME/sot` or
  `$HOME/.config/sot` on Linux/macOS, `%LOCALAPPDATA%\sot\config` on Windows. A shared Linux home shares that file.
  Setup can record an explicitly supplied absolute `--list` path and check `--require-ready`; it never creates policy
  entries. Development enrollment never invents a location or an empty replacement. Placement without provisioning is
  reported explicitly and pushes remain refused. An absent or invalid list is a checked detail in the installer's
  verdict, never a step: each clone's pushes are refused until a valid list exists on this computer. Preserved hook
  choices that need migration still give `ONE STEP LEFT`. A Windows update left armed uses the verified staged setup
  when available to enroll the known clones, and checks only the state it actually placed; it does not claim the release was applied.
  If that setup is unavailable, the verdict names base/caller enrollment as pending until apply and a repeated install.
  Every ordinary fresh/repair run enrolls again and checks readiness without repairing during the final check.
- Setup's installer interface is `--installer --list ABSOLUTE_PATH`: exit 0 means placed and policy ready, 3 means
  placed but policy needs provisioning/repair, 4 means preserved hook configuration/occupied slot needs its owner,
  and 1 means an operational failure. Existing development invocation keeps its nonzero refusal behavior.
  `--check-only` performs no enrollment/config writes: it verifies the shipped shim, executable selection, ownership
  hash, exact recorded location and policy. A previously placed hook that changes before the final check is FAIL.
  Unix uses Bash on PATH; Windows derives Bash from Git for Windows, never WSL's Bash. Native paths reach Bash as
  forward-slash absolute paths; setup remains the only hook-placement and policy-validation owner.
- Each single Git read started by `install/publish.rs` (caller discovery/origin/common-directory reads and source
  HEAD/diff verification) has 60 s wall clock through exit and complete stdout/stderr. The setup script has 300 s
  per enrollment or read-only check: its many Git/Bash processes need more time on Windows, including concurrent
  installs/tests. Tests pass short bounds explicitly through the discovery parameter; no environment variable can
  shorten the product's bounds. `install/cmd.rs` drains both streams while waiting, observes the leader without
  releasing its owned group/job, and allows at most 5 s more for termination, reap and output cleanup. It ends only
  its own containment domain. A timeout, signal,
  failed read or incomplete cleanup is an operational failure: discovery/enrollment return `Stop`, the final check
  records `Found::fail`; partial output never becomes ready or setup status 3/4. This bound does not extend to the
  separate installer observation calls noted above.
- The last `PASS` or `ONE STEP LEFT` line retains the guard's checked results independently of the ordinary install
  summary. It names the checked base and recognized caller (including a caller sharing the base's common directory),
  readiness/provisioning or preserved conflict, and the limited discovery scope: installed base and recognized caller
  only, other clones not discovered. Pending enrollment names its clones and remains pending until actually checked.
  Empty parts are omitted when joining the verdict. A failed check takes precedence over these results and human steps.
- Setup never changes `core.hooksPath`. Any effective explicit setting, including empty/default or worktree/global
  overrides, is a named conflict with the clone-local migration command. An unknown/edited/nonregular/linked slot is
  preserved. Replacement requires an exact shipped shim or a matching recorded installed hash; publication is whole
  files with a common-directory ownership record. Other default hooks remain selected. A person's optional
  `hooks/pre-push.local` runs after acceptance with the original arguments, complete stdin, streams and exit status;
  denial never reaches it and chain cycles refuse. An externally configured direct guard chains to a distinct default
  `hooks/pre-push` for compatibility. Chained hooks own the content of their forwarded streams. Setup never moves a
  foreign hook automatically.
- Limits: a direct push before enrollment, a direct build that skips setup, `--no-verify`, a disabled/removed hook,
  command/worktree configuration overriding hook selection, and `gh` text (PR, issue or release bodies) are outside
  this guarantee. Client hooks are an owner's protection, not a server security boundary. No discovery of arbitrary
  clones, live rows, or home directories occurs. Operations retirement and live migration remain the operator's work.

Platforms: Bash 3.2 and ordinary Git/awk/od/tr/sed on Linux/macOS and Git for Windows Bash; Windows path validation also
uses Windows PowerShell without profiles. Hook and shim check out at LF even with autocrlf. The Linux CI matrix builds
bash 3.2 and rewrites only scratch hook shebangs; macOS uses `/bin/bash`; Windows retains shipped shebangs and actual
Git-launched hooks. The matrix needs no product binary or secret and runs in `rust.yml` at landings and branch dispatch.

Hosted Linux publish guards and the release gate cache the built Bash 3.2.57 executable by version, architecture
and runner image. A cache miss downloads from GNU with a kernel.org mirror fallback, with bounded curl attempts
and the pinned SHA-256 checked before extraction (`tests/test-bash32.sh` covers reuse, fallback and corrupt bytes).

Windows fixture limits are reported with named `skip:` lines, never counted as passing coverage:

- Unreadable-validator push/readiness uses an `icacls` native Read deny, with its original ACL saved and restored by
  the fixture's exit cleanup. The test first observes that Bash cannot source the file. A Windows volume/token that
  cannot save/apply the ACL, or an MSYS runtime that still reads it, skips this condition; failed/incomplete libraries
  and failed validator calls remain separate executing cases. Linux/macOS retain their permission-denial assertions.
- Filesystem symlink cases (policy file/directory, hook slot and ownership record) require verified native links on
  Windows; Developer Mode or symlink privilege and filesystem support are prerequisites. No copied file substitutes
  for a link. The self-chain hardlink case requires observed file identity, and the Windows junction case requires a
  real reparse directory reaching its policy. Unsupported Windows conditions skip by name.
- The nonexecutable occupied hook-slot condition uses POSIX execute permissions on Linux/macOS and skips on Windows,
  where that mode-bit condition is not portable. Positive script cases observe Git-launched hooks and recording
  helpers; Windows does not use `chmod` as evidence of execution. Diff/textconv and upload-pack absence checks have
  positive native-Git execution controls. Symlink blob and unusual-name scans use raw Git objects, not filesystem
  symlinks or Windows-invalid filenames; numeric tree/index modes describe Git objects, not NTFS permissions.
- Native Git reads real empty scratch configuration files; `/dev/null` is used only for Bash redirection. These suites
  use no process substitution, signal/kill probes or filename pairs distinguished only by case. Exit cleanup restores
  any active read deny before removing scratch files. The Linux cgroup gate and shell pack routes print Windows skips;
  Windows runs its PowerShell pack route.

Tests (all observe execution, not source spellings):

| Entry | Guarantee |
|---|---|
| `tests/test-pre-push.sh` | Full object/ref scope, matching and allowances, advertised-old exclusions for new branches and tags pushed by path, URL and remote name, a nested remote namespace, a force-replaced destination, a mirror rewrite, a bare-word URL and a push URL holding none of the history excluding nothing unadvertised, missing shallow/promisor objects without fetch, witnessed producer/read failures (including the URL resolution and the advertisement read, path-bearing stderr and disappearing policy), complete stderr audit and deletion behavior |
| `tests/test-publish-guard-setup.sh` | Policy placement/UTF-8 encoding at readiness and push, non-ASCII comments and literal entry matching with ASCII folding only, unreadable/failed/incomplete validators, hostile Git selectors, preserved configuration/hooks, repeated setup, common-worktree shim/version selection, fresh-clone limit, exact chaining and veto/cycles, default commit hooks, LF/native Windows paths/interpreter |
| `tests/test-publish-guard-setup.sh` (installer cases) | Distinct ready/provisioning/conflict/write-failure statuses and read-only final checks, including a missing hook that stays missing |
| `install::cmd::tests::*` | Concurrent large-stream draining, deadline through output completion after a leader exits, owned writer cessation, and partial output never supplying a successful/provisioning result; native stand-ins run on all three systems |
| `install::publish::tests::*` | Distinct base/caller push behavior, platform location without policy creation, repair, preserved/edited hooks, read-only caller discovery, exact origin identity, common-worktree deduplication, native Bash/path execution, failed writes and pending-apply honesty; healthy and Julia-only verdicts retain checked names/scope; stalled Unix Git/Bash commands produce operational failures, including the read-only check |
| `tests/installer-publish-guard.py --scenario fresh\|repair` | Real `sotd install` orchestration in isolated profiles, both clones' clean/denied pushes and destination refs, refused options without caller writes, hook migration in the verdict and missing-policy provisioning; Unix's later role refusal witnesses recognized discovery before refusal and unchanged clone config/hooks and installed state. Unix records Julia steps, Windows records the shortcut and checks Julia's unavailable verdict step. Windows uses a unique scratch USERNAME and checks its pipe name and absence. No daemon, window or Julia instantiation is exercised on Windows |
| `tests/install-smoke.sh`, `tests/install-smoke.ps1` | Existing release smokes provision only synthetic policy, exclusively create it in their scratch config and invoke the absolute bootstrap outside the workflow checkout, so the smoke enrolls only its installed clone; the Unix smoke also creates a stand-in `claude` in its scratch home, so a daemon role's Claude Code check can pass |
| `tests/test-publish-guard-entries.sh` | Actual release push refusal; gate checkout enrollment at container boundary; shell/PowerShell pack enrollment at recording build boundary; entry conflicts before builds |
| `tests/installer-apply.sh` (including `installer-handoff.sh`) | Git-less libc/tree preflight retains its cause; a real scratch clone enrolls before recording builds and packs an executable sot-apply |
| `tests/test-rc-gate-scope.sh` | The path-to-piece table with its widening cases, and the real pipeline over stand-ins: a scoped run selects only its pieces, a selected job the tree lacks fails |
| `tests/release-features-check.sh --prove` | The real tree and the feature on a dev-dependency alone pass; the feature on a normal dependency of the backend, on the frontend, in sot-chat's default, in a target table, or on a member only `--workspace` builds fails and the message names the feature |
| `tests/test-defects-check.sh` (and `tests/defects-check.sh`) | A landing record naming an unknown item id fails and the output names it; known open and closed ids pass; capitals that are not ids never hit; a duplicate id and an open item with no mark fail; the real tree passes |
| `tests/test-release-cut.sh`, `tests/test-rc-gate-target.sh`, `tests/test-rc-gate-parity.sh`, `tests/test-native-stderr.ps1` | Existing entry contracts retain their intended executing fixtures after enrollment |
| `install::unix::tests::version_cleanup_keeps_unavailable_registrations_and_refused_worktrees` | Version cleanup preserves unavailable registrations and dirty/locked checkouts while removing available clean ones |

The installer process fixture preserves Windows machine installation/data roots, system drive and computer name, and
sets PSModulePath to Windows PowerShell's system modules alone. Profile/config/temp folders, PATH and the daemon
identity stay isolated; the runner's user module paths are excluded. Its `installer_process.py` helper gives each
command a Windows job (joined before the real command starts, kill on close, no breakaway) or a Unix process group.
Output goes to an owned scratch file with a separate reader, so Windows progress is available while the child lives.
At the command's timeout (600 s on Windows, 180 s on Unix, unless a probe names a shorter bound), it first prints
captured progress and a descendant snapshot with PID, parent, elapsed time and full command line. The listing command uses the runner's full
environment with no PowerShell profile and has its own 10 s bound; a failed/timed-out listing is reported explicitly.
Only then does cleanup close the Windows job or send TERM to its own Unix group, allowing 15 s on Unix before KILL
and a final 5 s reap bound. Unix cleanup after an abnormal installer exit is not guaranteed: installer steps take
their own groups and can survive the harness's group. The enclosing lane container is the cleanup boundary where
available; macOS containment remains unproven. Windows allows 5 s to drain/reap;
if the Python trampoline never joined the job, its own spawn handle permits termination and a final 5 s reap.
Snapshot PIDs are never used for termination. `capture` owns the file and process lifetime directly; there is no
separate `capture_to_file` helper.

Hosted Windows measured a cold CIM process query at about 26 s; the install's process listing is bounded at 60 s
(Installers, "Bounded waits").

The three publish suites are listed in `rc-gate.sh` and `under-bash.sh`, and run as real local pushes in rust.yml's
three-system `publish-guard` job. No passing evidence is implied by this contract; the landing records the actual runs.
Linux/macOS run the native publish units (`inst`) and fresh/repair process witness at every landing; the witness
follows the Rust tests in the job containing `sotd-c`. Windows runs both only at the release gate, in
`build+test, release gate (windows-latest, inst)`: real Git Bash enrollments take minutes each (the nine native units
take about 9 to 11 minutes each on hosted Windows), while the three guard shell suites retain per-landing enforcement
proofs there. The Windows gate builds all three binaries before the units (45-minute step, 44-minute test-binary
bound) and witness (75-minute step, 600 s per command), within a 140-minute job. Landing B lands with both proven green
on Windows: release-gate run 40189400665 (the nine units in 31 minutes, fresh and repair in 46.5). That run's conclusion is
red from one step outside the guard, `Test sot-comm disambiguation (hermetic)` in its slow checks, which named comm
suites pass 4 deleted (`test-join-disambiguation.sh: No such file or directory`, exit 127); the step is gone, since
`rc-gate.sh` runs every comm suite that remains at each landing. The command-owner unit proofs remain
in the per-landing sotd shard on all three systems. The table's rows and coverage check still require every test
in exactly one row; the planner/check also requires every shard in exactly one landing or release job per system.
Native Windows/macOS success and signal behavior require execution evidence; the configured matrix alone does not establish them.
