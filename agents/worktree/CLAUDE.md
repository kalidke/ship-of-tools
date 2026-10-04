# agents/worktree: a parallel session in its own git worktree (agents)

The scripts behind the /worktree skill: each makes, lists, reminds or removes a git worktree and the session row bound to
it. Each sources `comm-lib.sh` and runs its sibling scripts through its own folder, and the installer copies the folder
flat into `~/.sot-comm/bin`, so they run beside the library and the siblings at their installed names. Part of agents;
charter: agents/CLAUDE.md.

## Files
- `comm-worktree-new.sh`: makes the worktree and its branch `wt/<short>`, spawns the row (`comm-spawn.sh`) with a label built from `display_prefix` in `.sot/worktree.toml`, and tells the parent.
- `comm-worktree-status.sh`: the family of worktrees, from the registry and `git worktree list`, with cleanup readiness.
- `comm-worktree-sync.sh`: reminds every session of the family to share progress and sync.
- `comm-worktree-clean.sh`: removes a finished worktree, despawns its row and drops its registry entry.

## Start here
`comm-worktree-new.sh`, where the location (`WT`) and the handle (`HANDLE`) are built; the other three find the family
from the same name.

## Rules
- The worktree lands at `<repo-parent>/worktrees/<repo>-wt-<short>` (`HANDLE` and `WT` in `comm-worktree-new.sh`). The
  ruled location differs (the shared drive's worktrees folder, `<repo>--<name>`); a fix lane is open and this page
  states the code as it is.
- The family is found by stripping `-wt-*` from the repo name (`BASE` in `comm-worktree-status.sh`,
  `comm-worktree-sync.sh` and `comm-worktree-clean.sh`).
- `comm-worktree-clean.sh` measures "merged" against `main` or `master` only (`BASEBR`); a branch merged elsewhere
  reads unmerged until `--force`.
- `comm-worktree-new.sh` refuses a second agent inside a session (`sot_require_agent`), before any git write.
