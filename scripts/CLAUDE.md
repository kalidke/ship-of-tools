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
  tree it ran and ending `ALLDONE` or `ALLDONE FAILED`. The maintainer's word is given on a commit both passed.
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

Owns: `release.sh`; `rust/updater/examples/vercmp.rs`; `.github/` (release.yml; rust.yml and CI.yml, the hosted gate,
dispatched at the end of a pass and at the release gate; gitleaks.yml; dependabot); the local gate (`tests/rc-gate.sh`,
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
windows-latest and macos-14 and smokes and installs on all three.

Tests: `tests/test-release-cut.sh` (the cut in throwaway repos with a scratch origin, and the workflows' release
writes), `tests/test-release-before.sh`, `tests/test-rc-gate-*.sh`, the order in `rust/updater/src/semver_tests.rs`, and
a dispatch of release.yml for the pipeline. rust.yml's checks job runs the shell suites.

## Installers (contract; pass 1b)
A box runs one tagged release, laid under `$PREFIX` and recorded in `$PREFIX/install.json`. Install puts a release
there and says in one line whether the box is whole; update stages and arms a newer one; apply swaps it in offline;
launch starts a window and never stops on an update. The entry is `install.sh` (Linux, macOS) or `install.ps1`
(Windows), a bootstrap that fetches and verifies a pack and runs its `sotd install` (`rust/backend/src/install/`),
where every rule of install lives once for all three systems.

Guarantees, each with the mechanism it rests on. Install:
- One verdict. Every run ends in exactly one line, `PASS: Ship of Tools <v> at <short> on this computer`, `ONE STEP
  LEFT: <steps>` (exit 0: the install is whole) or `FAIL: <check>: <detail>` (exit 1; 2 for a refusal), on standard
  output after every other line. `sotd install` prints it from one place (`verdict.rs`), on a signal too (then it exits
  128 plus the signal); the bootstrap prints one only before `sotd install` runs or when it ended without one.
- Verified before it runs. The bootstrap runs nothing from a pack until every file it downloaded or copied matches
  `SHA256SUMS` (a file the list does not name is refused) and the unpacked `sotd` runs here. `sotd install` then reads
  the pack's identity (the one archive `SHA256SUMS` names for this system, `COMMIT`, each binary's `--version`) and
  refuses a pack whose parts disagree. This is why the bootstrap is a script: it must verify before anything runs.
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
- The pin. A run that names its target pins the install (`"pinned": true`); one that names none removes the pin; the
  automatic writers never arm over a pin or over a record that cannot be read (`InstalledBuild::admits`). Every
  verdict line of a pinning run says so once the pin is written.
- A repair run moves nothing. A named run of the installed version and commit, with no daemon on another version,
  writes the pin, drops any pointer armed over the install, and stops, restarts, stages and applies nothing; its other
  steps lay again what is there (the same binaries and checkout, the environments' own manifests kept), so a repeat
  finishes a half-done install, and a readable record of that build gets its pin and no other change.
- The record last, whole, in its old shape. `install.json` is written last, beside itself and renamed in (no BOM), one
  key per line with schema 1's keys, the shape every earlier release's readers take (line readers in the scripts,
  `InstallManifest`). A repeat keeps the rollback: a `.prev` copy is made only of a binary that differs, and a prune
  keeps the previous version folder and names and keeps any folder with local changes.
- A live daemon is asked before it ends. A daemon is restarted only when it runs another version (`sotd
  same-version`), only where this run owns it (Unix: an active `sotd.service` no window holds; Windows: the window's
  supervisor, by its converge), and only once its live work (`sotd update repls`: REPL rows and the Pluto server; a
  daemon that cannot say is judged by its own child processes, by parent id) is answered: yes at a console, or
  `--restart-daemon`/`-RestartDaemon`; else the run stops changing nothing. A restart must answer as the target within
  its bound by the clock. A daemon that runs and cannot be told its version is never read as none.
- The role is declared (Unix). hosts.toml's entry for this host decides (a frontend runs its own daemon); else the
  role flag; else what install.json records; else a question at the console; else FAIL 2. A host hosts.toml runs no
  sotd on never writes over an install at the prefix, a run never replaces another install's unit, wrapper or desktop
  entry, and under a declared topology a run never records less than install.json does (`--force-role-change` waives
  the three; an unrecognized file is never overwritten). The unit is pinned to its hosts before any reload.
- The Julia the daemon will run meets the kernel's floor (`[compat] julia` of `julia/kernel/Project.toml`): the
  daemon's (`SOT_JULIA_BIN`, else juliaup's default read from `juliaup.json`, else PATH) and the comm refresh's (PATH).
  Below it the run ends ONE STEP LEFT naming the fix; a PATH julia below it ends the run before a Julia step. Julia
  steps run with `TMPDIR` inside the run's scratch folder, which the run removes.
- What it left is checked. The verdict reads the disk and the processes: the binaries' versions, the record,
  `repo/current`, no pointer armed again, the comm stamp, the Julia manifests of a daemon role, the daemon's answer;
  a step only a person takes (a window on the old version, a daemon a window holds) is ONE STEP LEFT.
- Bounded waits. curl runs with `--connect-timeout 15 --speed-limit 1000 --speed-time 60`, every git call with
  `http.lowSpeedLimit=1000` and `http.lowSpeedTime=60`, ssh with the tree's `SSH_OPTS` under a 30 s bound; a converge
  waits at most `SOT_INSTALL_CONVERGE_WAIT_S` (1800 s), a restarted daemon `SOT_INSTALL_RESTART_WAIT_S` (Unix, 30 s) or
  `SOT_INSTALL_DAEMON_WAIT_S` (Windows, 60 s).

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
- A release runs its own apply at every later update: the installed apply hands off once to the staged release's
  (`SOT_APPLY_REEXECED`; Unix: the archive's executable `sot-apply`, which reads the prefix from where the stage sits).

Launch (`launch-sot.sh`, the rendered `sot-launch`, `lib/sot-daemon.sh`, `launch-sot.ps1` and the files it sources):
- It fails open: every update or freshness step logs and the window still starts.
- It keeps this computer's daemon only when it runs the launch's version, unless a window holds it (`sotd
  same-version` exit 4); one launcher per user; on Windows no window spawns without a lease held across the gap.
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

Calls out: `sotd update direction|repls|stage|pointer`, `sotd same-version`, `sotd topology status|plan|sync|pin`,
`sotd trust declare`, `sotd session-socket-path`, `sotd stdio-bridge`, run as programs (the command line is the seam:
`sotd install` names no other subsystem's module but platform's `host_name` and `kernel_host_name` and topology's
`SSH_OPTS`); git, curl, tar, julia and juliaup, npm, systemctl and loginctl,
ps; on Windows powershell.exe for the shortcut, the converge (`relaunch-sot.ps1 -Converge`), the stop of a daemon
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
(`Get-SotLauncherCodeId`) hashes every file `launch-sot.ps1` sources; the bounds the launcher and the daemon share live
in `rust/protocol/src/ops/lease.rs` (`launcher_bounds_match_ops`).

Tests: one Rust unit test per install guarantee in `rust/backend/src/install/*_tests.rs` (`cargo test -p
sot-backend install::`); the install smokes (`tests/install-smoke.sh`, `tests/install-smoke.ps1`, the jobs
`install-linux`, `install-macos`, `install-windows` that release.yml's `draft` needs): a fresh install, an update from
the release before (`tests/release-before.sh`, by version order), update-apply (the release before's installed apply
hands off to the staged release's), update-implicit (no arguments against the real repository: a repair run at the
release before that clears its pin, then the run's release stages as newer) and a repair run, each judged by its
verdict and the state it left; the apply, library and wrapper suites (`tests/installer-*.sh`, `tests/test-*.ps1`); the
integration tests `shell_dial`, `unit_pin`, `same_version`, `window_start`.
