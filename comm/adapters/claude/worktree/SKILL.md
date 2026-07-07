---
name: worktree
description: Create a git worktree of the current repo and spawn a parallel sot-comm session bound to it (named <repo>-wt-<shortname> so it groups next to its parent in the sessions list); show worktree status (current/done/ready-to-clean + each session's work-state); remind a repo's parent + worktree sessions to compare progress and sync; and clean up a finished worktree (remove + delete branch + despawn). Use when making/spawning a worktree, checking worktree status, polling worktree sessions, syncing them, or cleaning one up. Activates for "new worktree", "create worktree", "spawn worktree", "worktree", "worktree status", "list worktrees", "what worktrees", "are worktrees done", "worktrees ready to clean", "poll worktree", "worktree sync", "sync worktrees", "remind worktrees", "clean worktree", "remove worktree", "wt".
---

# worktree

Two deterministic scripts in `~/.sot-comm/bin/` (installed from
`comm/core/scripts/` via `ShipTools.update_comm()`):

- `comm-worktree-new.sh` — create the worktree + spawn its session.
- `comm-worktree-status.sh` — show the family's state + cleanup-readiness.
- `comm-worktree-sync.sh` — remind the parent + worktree sessions to sync.
- `comm-worktree-clean.sh` — tear down a finished worktree (remove + branch + despawn).

## Convention (why this works with zero frontend change)

- Worktree session **handle** and **on-disk dir** = `<repo>-wt-<shortname>` (e.g.
  `MyAnalysis-wt-rotation`); the frontend **workspace label** = `<prefix>-wt-
  <shortname>`, where `<prefix>` defaults to the repo basename. The daemon derives
  the **slug** from the label (`paths::slug` → lowercased, dashes kept, `.`→`_`)
  and the sessions list sorts by slug — so the worktree lands next to its parent
  with no protocol/backend/frontend change. Grouping is purely **naming**; the
  handle/dir stay repo-based so `status`/`clean`/`sync` group by the real repo.
- **Display prefix override (durable):** to make a repo's worktrees display + sort
  somewhere specific, pin `<prefix>` via a committed `.sot/worktree.toml`
  (`display_prefix = "…"`) or a one-off `--display-prefix`. ship-of-tools uses
  `display_prefix = ".SoT"` → label `.SoT-wt-<short>` → slug `_sot-wt-<short>`
  (leading `_` sorts before any letter) → far left, by the pinned default `sot`
  row. Only the displayed label changes; the comm handle stays
  `ship-of-tools-wt-<short>`.
- Worktree directory: **`<repo-parent>/worktrees/<repo>-wt-<shortname>`** (never
  inside the repo).
- Branch: **`wt/<shortname>`** by default, off the current `HEAD` (override with
  `--base` / `--branch`).
- No host in the name — `$HOME` is shared across the cohort, so the machine name
  is noise. The parent is found by repo family, not by host.

## new — create a worktree + spawn its session

Run from inside the repo you want a worktree of:

```bash
comm-worktree-new.sh <shortname> [--base <ref>] [--branch <name>] \
                     [--task "what to do"] [--expertise "a, b"] [--no-spawn]
```

What it does (and the guards it enforces — it fails loudly, never `--force`s):
- validates `<shortname>` (`^[a-z0-9][a-z0-9-]*$`, ≤64) and the branch name
  (`git check-ref-format`), and resolves `--base` to a real commit;
- refuses if the branch already exists, the target dir exists, or the branch is
  checked out elsewhere — and points at `git worktree list`;
- `mkdir -p <parent>/worktrees`, then `git worktree add` (warns that uncommitted
  changes in the base checkout are NOT carried over);
- **replicates the source checkout's working-tree symlinks** into the worktree
  (e.g. `data/results → external storage`, `data/raw/* → external storage`) — these are gitignored and so
  NOT carried by `git worktree add`, and a worktree session needs them to reach
  external storage data / write results where the main repo does (`--no-symlinks` to skip);
- `comm-spawn.sh <repo>-wt-<shortname> <wtdir> --display-label <prefix>-wt-<shortname>`
  (`--display-label`, not `--label`: the display label may differ from the
  repo-based handle — e.g. `.SoT-wt-<short>` — which comm-spawn's repo-base
  `--label` guard would otherwise reject) with a brief telling the new session its
  **parent is `@<spawner-handle>`** and to coordinate progress/syncing with it and
  any sibling `@<repo>-wt-*`;
- notifies the parent that the worktree was spawned.

## sync — remind the family to compare progress + sync

Run from the parent **or** any worktree session:

```bash
comm-worktree-sync.sh [--message "extra note"]
```

It finds the family in the registry — the parent (`<base>` / `<base>-<host>`) and
every `<base>-wt-*` worktree (dash-guarded so `pkg` never matches
`pkg-analysis`) — and pings each (except the caller) with the roster, a
`git worktree list`, and an ahead/behind-vs-`main` summary, asking them to compare
progress and flag rebase/merge needs.

## status — is each worktree current / done / ready to clean up

Run from the parent **or** any worktree:

```bash
comm-worktree-status.sh
```

Prints, per worktree: branch, **behind/ahead** vs the base branch (main/master),
whether it's **MERGED** into base (removing it then loses nothing), and the owning
session's **work-state** from the registry (so this also serves as a passive
"poll" of each worktree session). **Ready to clean = MERGED=yes + session idle/done.**

## clean — tear down a finished worktree

The cleanup lifecycle is: `status` to confirm it's merged → **merge the branch to
main yourself** (a normal merge/PR — `clean` does NOT merge for you) → then:

```bash
comm-worktree-clean.sh <shortname> [--force] [--keep-session]
```

It removes the worktree, deletes its branch, and despawns its session. It
**refuses if the branch isn't merged** into the base branch (so you can't silently
drop unmerged commits); `--force` overrides (`git worktree remove --force` +
`git branch -D`). `--keep-session` leaves the session running.

## Notes

- A worktree session is just a normal workspace row (own project_root / kernel /
  panes); switch to it like any other.
- Edge: if the **parent repo is the daemon's default workspace**, `is_default`
  pins it to the top of the strip, so its worktrees group among the other rows
  rather than directly beneath it. Non-default repos group adjacently.
- Tear-down is the `clean` action above (or by hand: `git worktree remove <dir>` +
  `git branch -d wt/<shortname>` once merged + `comm-despawn.sh <repo>-wt-<shortname>`).
