---
name: worktree
description: Create a git worktree with a parallel sot-comm session bound to it; show worktree status; remind sessions to sync; clean up a finished worktree. Activates for "worktree", "new worktree", "worktree status", "sync worktrees", "clean worktree", "wt".
---

# worktree

Four deterministic scripts in `~/.sot-comm/bin/` (installed from
`comm/core/scripts/` via `ShipTools.update_comm()`):

- `comm-worktree-new.sh` — create the worktree + spawn its session.
- `comm-worktree-status.sh` — show the family's state + cleanup-readiness.
- `comm-worktree-sync.sh` — remind the parent + worktree sessions to sync.
- `comm-worktree-clean.sh` — tear down a finished worktree.

## Naming

Worktree session **handle** and **on-disk dir** are `<repo>-wt-<shortname>`;
the frontend **workspace label** groups it next to the parent row (a
committed `.sot/worktree.toml` `display_prefix`, or `--display-prefix`, can
override the label's prefix for a repo whose row name differs from its
handle — the comm handle itself always stays repo-based, so
`status`/`clean`/`sync` group by the real repo regardless). Worktree
directory: `<repo-parent>/worktrees/<repo>-wt-<shortname>` (never inside the
repo). Branch: `wt/<shortname>` off the current `HEAD` by default (override
with `--base`/`--branch`). No host in the name — the parent is found by repo
family, not host.

## new — create a worktree + spawn its session

Run from inside the repo you want a worktree of:

```bash
comm-worktree-new.sh <shortname> [--base <ref>] [--branch <name>] \
                     [--task "what to do"] [--expertise "a, b"] [--no-spawn]
```

Refuses (loudly, never `--force`s): an invalid `<shortname>`
(`^[a-z0-9][a-z0-9-]*$`) or branch name, a branch that already exists or is
checked out elsewhere, or a target dir that exists. Replicates the source
checkout's gitignored working-tree symlinks (e.g. `data/results` → external
storage) into the worktree so it can reach the same data (`--no-symlinks` to
skip), then spawns `<repo>-wt-<shortname>` via `comm-spawn.sh` with a brief
naming its parent and sibling worktrees, and notifies the parent.

## status — is each worktree current / done / ready to clean up

```bash
comm-worktree-status.sh   # run from the parent or any worktree
```

Prints, per worktree: branch, ahead/behind the base branch, whether it's
**MERGED** (removing it then loses nothing), and the owning session's
work-state. **Ready to clean = MERGED=yes + session idle/done.**

## sync — remind the family to compare progress

```bash
comm-worktree-sync.sh [--message "extra note"]   # from the parent or a worktree
```

Finds the family in the registry (the parent + every `<base>-wt-*`,
dash-guarded) and pings each with the roster, a `git worktree list`, and an
ahead/behind-vs-base summary.

## clean — tear down a finished worktree

`status` to confirm it's merged → **merge the branch to main yourself**
(`clean` does not merge for you) → then:

```bash
comm-worktree-clean.sh <shortname> [--force] [--keep-session]
```

Removes the worktree, deletes its branch, despawns its session. **Refuses if
the branch isn't merged** into the base (so you can't silently drop unmerged
commits) — `--force` overrides; `--keep-session` leaves the session running.

## Notes

- A worktree session is a normal workspace row (own project_root/kernel/
  panes) — switch to it like any other.
- Hand alternative to `clean`: `git worktree remove <dir>` + `git branch -d
  wt/<shortname>` once merged + `comm-despawn.sh <repo>-wt-<shortname>`.
