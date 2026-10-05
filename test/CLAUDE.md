# test: the comm installer's tests (distribution)

The tests of the Julia installer package (`ShipTools`), one file per installer concept. Every test reaches the installer
as `ShipTools.<name>`. Part of distribution; charter: scripts/CLAUDE.md.

## Files
- `runtests.jl`: the entry: `in_home`, the suite's temporary home, then the six subject files and the home-variable scan inside one "Ship of Tools" testset
- `codex_tests.jl`: Codex hooks payload, marketplace registry and home profile export parsing (src/codex.jl)
- `publish_tests.jl`: `install_file` and `_install_files` publishing, staging names, marker reaping (src/publish.jl)
- `skills_tests.jl`: skill install, orphan sweep, retired session-start aliases, shipped project-local skills (src/skills.jl, src/launchers.jl)
- `install_tests.jl`: `update_comm` reporting, pruning of retired comm scripts, the one-file library and sot-fe (inlining, refusal, comm-lib.sh's functions and globals equal to the loader's), and install_comm's publish order and failures, driven through its own loop (src/install.jl, src/comm_bin.jl)
- `homes_tests.jl`: env-dir resolution (src/homes.jl)
- `claude_hooks_tests.jl`: Claude settings targets and hook merging, including `jq` calls (src/claude_hooks.jl)

## Start here
`runtests.jl` for the order of subjects; the subject file for the installer concept you change.

## Rules
- Run from the repo root: `julia --project=. -e 'using Pkg; Pkg.test()'`.
- The settings-merge tests call `jq` and read its raw output through `jq_lines` (claude_hooks_tests.jl), which takes the one CR off each line on Windows, where jq writes CRLF line endings.
- The suite runs inside a temporary home, and a test that needs a home of its own sets it only through `in_home(home)`
  (runtests.jl): HOME, USERPROFILE, HOMEDRIVE and HOMEPATH (Julia's `homedir()` reads USERPROFILE on Windows), the comm
  home and CODEX_HOME under it (the codex CLI the installer may start keeps its state in CODEX_HOME), CLAUDE_CONFIG_DIR
  unset, every other SOT_ variable unset (no comm script a test runs reaches the session's self file or daemon), and an
  error before the body if `homedir()` is not that home. The suite's last set fails a subject file that names a home
  variable as a string, and reads `in_home`'s contract back on the suite home. Every test writes only under
  `mktempdir()`. No hosted runner carries codex, so a real codex under these tests on Windows is untested.
- A test that needs Unix mode bits probes at run time, with the operation the code under test uses, and skips itself.
- The bash checks (sourcing a library, running an installed script) run only where `Sys.isunix()`; the rest runs everywhere.
- A subject file defines its own helpers, inside a testset or, when several of its testsets share one, once at its top; `in_home` (runtests.jl) is the one helper the files share.
- Comm paths come from ShipTools' constants (`ShipTools.CLAUDE_SKILL_SRCS`, `ShipTools._comm_bin_files()`), never a literal
  path.
