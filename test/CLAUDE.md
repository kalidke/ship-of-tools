# test: the comm installer's tests (distribution)

The tests of the Julia installer package (`ShipTools`), one file per installer concept. Every test reaches the installer
as `ShipTools.<name>`. Part of distribution; charter: scripts/CLAUDE.md.

## Files
- `runtests.jl`: the entry; includes the six subject files inside one "Ship of Tools" testset
- `codex_tests.jl`: Codex hooks payload, marketplace registry and home profile export parsing (src/codex.jl)
- `publish_tests.jl`: `install_file` and `_install_files` publishing, staging names, marker reaping (src/publish.jl)
- `skills_tests.jl`: skill install, orphan sweep, retired session-start aliases, shipped project-local skills (src/skills.jl, src/launchers.jl)
- `install_tests.jl`: `update_comm` reporting, pruning of retired comm scripts, the one-file library and sot-fe (inlining, refusal, equivalence with the repo's form), and an install replayed step by step (src/install.jl, src/comm_bin.jl)
- `homes_tests.jl`: env-dir resolution (src/homes.jl)
- `claude_hooks_tests.jl`: Claude settings targets and hook merging, including `jq` calls (src/claude_hooks.jl)

## Start here
`runtests.jl` for the order of subjects; the subject file for the installer concept you change.

## Rules
- Run from the repo root: `julia --project=. -e 'using Pkg; Pkg.test()'`.
- The settings-merge tests call `jq`.
- Every test writes only under `mktempdir()`, with `HOME`, `SOT_COMM_HOME`, `CLAUDE_CONFIG_DIR` and `CODEX_HOME` set or
  unset by `withenv`.
- A test that needs Unix mode bits probes at run time and skips itself.
- The bash checks (sourcing a library, running an installed script) run only where `Sys.isunix()`; the rest runs everywhere.
- A subject file defines its own helpers inside its testsets.
- Comm paths come from ShipTools' constants (`ShipTools.CLAUDE_SKILL_SRCS`, `ShipTools._comm_bin_files()`), never a literal
  path.
