# scripts/tests/: suites for install, apply and launch (distribution)

Hermetic suites for the scripts in scripts/, and the local candidate gate. Each suite builds its prefix, home or stub
`sotd` under a temp folder and touches no live daemon. Part of distribution; charter: scripts/CLAUDE.md.

## Files
- `installer-state.sh`: install.sh's decisions, the rendered unit and wrapper, `sot_daemon_ensure`, the log pruner,
  and the pinned bounds and copies. Runs in the `rust.yml` step "Test installer state (bash)" (ubuntu leg) and in
  `rc-gate.sh`.
- `installer-apply.sh`: `sot-apply.sh` apply and rollback, the one-copy helper and the network refusal. Runs in the
  `rust.yml` step "Test installer apply (bash)" (ubuntu leg) and in `rc-gate.sh`.
- `installer-support.sh`: the setup both installer suites source: install.sh and lib/sot-daemon.sh, `check`,
  `starts_with`, `case_start`, the sandboxed tool dir (`mk_tools`) and the recording stubs (`mk_stubs`).
- `check-layout.sh`: the layout check (file and folder sizes, pages, `## Files` lists); see Tools below.
- `check-layout.allow`: the layout exceptions CI passes to `check-layout.sh`, each with its reason.
- `exempt.txt`: the standing exemptions `check-layout.sh` reads from beside itself.
- `moved-check.sh`: compares the lines a commit range added and removed, to show a move left no residual.
- `test-check-layout.sh`: self-test of `check-layout.sh` over throwaway repos.
- `test-moved-check.sh`: self-test of `moved-check.sh` over throwaway repos.
- `rc-gate.sh`: the local candidate gate: the Rust workspace tests, doc tests, windows-gnu and darwin cross checks,
  every Julia suite and the shell suites, as concurrent jobs under one cap. Linux only, run by hand.
- `test-install-layout.ps1`: `Test-SotPinnedCheckout`, `Get-SotLauncherTarget`, `Get-SotLauncherCodeId` and
  `Set-SotFolderTrust` (scripts/sot-install-layout.ps1). Runs in the `rust.yml` step "Test install layout
  (pinned-checkout predicate)".
- `test-local-daemon.ps1`: scripts/sot-local-daemon.ps1 start, stop and wait behaviour (sections 0-8 and 12-15: the
  refusal, the pipe name, a late bind, `-Stop`, log retention, `Get-StopWaitMs`). Runs in the `rust.yml` step "Test
  local daemon launcher".
- `test-launcher-leases.ps1`: launch-sot.ps1's ensure and lease order in the supervisor loop and the converge lease
  (sections 9-11 and 16 of the old suite), read as syntax trees and run against the fake `sotd.exe`. Runs in the
  `rust.yml` step "Test launcher leases".
- `test-local-daemon-fake.ps1`: dot-sourced by both local-daemon suites: compiles the fake `sotd.exe` and defines
  `Clear-FakeEnv`, `New-FakePrefix` and `Stop-FakeOn`.
- `test-local-daemon-support.ps1`: dot-sourced by both local-daemon suites: `Check`, the fixture and pipe helpers, the
  test root and `Complete-LocalDaemonTest`, their cleanup.
- `test-sot-apply.ps1`: scripts/sot-apply.ps1 against a synthetic staged update: apply, damaged stage, rollback,
  already applied, wrong target, lock held. Runs in the `rust.yml` step "Test sot-apply.ps1".
- `test-topology-plan.ps1`: `Get-SotTopologyPlan` and `Invoke-SotTopologySync` (scripts/sot-hosts.ps1) against a fake
  `sotd`. Runs in the `rust.yml` step "Test topology plan".
- `test-topology-plan.sh`: `sot_topology_plan` (scripts/lib/sot-hosts.sh) against a fake `sotd`. Runs in the `rust.yml` step
  "Test topology plan (bash)" (ubuntu leg) and in `rc-gate.sh`.

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
- `named-path`: `VIOLATION named-path <page> <path>`. For each tracked page in the list `NAMED_PATH_FILES` (at first
  `docs/ownership.md`; the list is in the tool, and later pages join it), every inline-backticked token outside fenced
  blocks is a candidate. A trailing `:<n>` or `:<n>-<m>` and a trailing `::<item>` are stripped and one `{a,b,...}` group
  is expanded. A token containing `<`, `>`, `*`, `$`, `~` or a space is skipped. A token whose first segment is not a
  tracked top-level folder or root file is skipped. Any other must name a tracked file or a folder holding tracked
  files, else it is a violation. A folder-argument run checks the listed pages too.

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

When a listed page is tracked, the line before the last is `named-path: n tokens checked in f files`.
Last line: `violations: n, allowed: m, exempt: e, unused-allow: u, folders checked: k`. Exit 0 none, 1 any, 2 usage error.

`--report`: per folder, source files as `code N test M name`, largest code first.

### Self-tests

`bash scripts/tests/test-moved-check.sh`, `bash scripts/tests/test-check-layout.sh`: build throwaway repos under
`mktemp -d`; exit 0 when all assertions pass.

## Start here
`installer-state.sh` for a change to install.sh, sot-apply.sh or lib/sot-daemon.sh: each `case_start` names the
behaviour it pins. For a Windows script change, the `.ps1` suite named for it above.

## Rules
- The five `.ps1` suites run only on the windows-latest leg of `rust.yml`, under Windows PowerShell 5.1; `rc-gate.sh`
  and a Linux box never run them. The step "Parse PowerShell scripts" globs `scripts/*.ps1` without recursion, so each
  `.ps1` suite parses itself and its siblings in its section 0.
- A suite runs only if a step of `.github/workflows/rust.yml` or a job of `rc-gate.sh` names it; a new suite is added
  to the step list in the commit that adds it. `rc-gate.sh` lists only the three shell suites here by name
  (`SHELL_ALL` in `producer`).
- `installer-support.sh` sources `install.sh` with `SOT_INSTALL_SOURCE_ONLY=1` and `lib/sot-daemon.sh` for both installer
  suites; `installer-state.sh` reads `rust/protocol/src/ops/lease.rs` for the pinned bounds (`launcher_bounds_match_ops`), so a
  rename there breaks it.
- The suites stub `sotd` (and `systemctl`, `nc` in `installer-support.sh`); none needs a network. `test-local-daemon.ps1`
  sections 3 to 6 need a real `sotd.exe`: a failure on CI when absent, a skip elsewhere.
- `rc-gate.sh` needs `CARGO_TARGET_DIR` to itself while it runs; its verdict ends `<logdir>/summary.txt` as `ALLDONE` or
  `ALLDONE FAILED`.
