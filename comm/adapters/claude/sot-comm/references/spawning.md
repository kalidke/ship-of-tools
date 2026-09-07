# Spawning — the rarer knobs

`SKILL.md` covers the common case. This is for the rest.

## Endpoint + tuning

`comm-spawn.sh` auto-detects the daemon endpoint from explicit env, old dev
`--tcp`/`--socket` daemon flags, or `sotd session-socket-path
${SOT_BACKEND_LABEL:-sot}`. Override with `--endpoint unix:/path/to/sot.sock`
or `--endpoint tcp:HOST:PORT`. Tune boot wait with `SOT_COMM_SPAWN_WAIT`
(default 6s). `--no-workspace` skips the daemon entirely and just makes a
plain tmux session — headless, won't appear in the FE strip.

## A durable peer instead of a task agent

`comm-spawn.sh` is for delegation with a report-back-and-despawn lifecycle.
For a long-lived comm-aware backend session (survives `--continue`,
re-bootstraps its own receive path every restart), the path depends on who
is spawning:

- **You are a Claude session (or headless)**: still use `comm-spawn.sh` in
  workspace mode — the daemon + FE autostart give claude a clean env and a
  real attach.
- **A human at a shell**: `tmux -S "$SOCK" new-session -s <name> -c <repo>
  ~/.local/bin/ccb` (`ccbe` for a Ship of Tools backend) — no `-d`, create
  and attach in one step. Never take a registry handle that already exists,
  even one that looks stale.

## Git worktrees

Spawning into an **isolated full-repo checkout** (a parallel branch to
build/edit without disturbing the main tree) is the `/worktree` skill's job
end to end — it creates the worktree, picks the naming, and spawns the
session bound to it. Don't hand-roll `git worktree add` here; see
`/worktree` for `new` / `status` / `sync` / `clean`.

## Bootstrapping a session that hasn't joined yet

A session is only addressable by `@name` once it has joined — that's the
consent model. If another session has the skill installed but hasn't
joined, find its tmux target and nudge it:

```bash
source ~/.sot-comm/bin/comm-lib.sh
SOCK="$(sot_tmux_socket)" || { echo "cannot resolve the sot tmux socket" >&2; exit 1; }
tmux -S "$SOCK" list-panes -a -F '#{session_name}:#{window_index}.#{pane_index}  #{pane_id}'
comm-bootstrap.sh sot-be-lab-guide:1.1 lab-guide
```

`comm-bootstrap.sh` pastes a self-contained "run comm-join then reply to me"
message into the target's prompt (via `comm-send.sh --force-target`, the
only path that bypasses the registry). Once the target joins it appears in
`comm-list.sh` and you exchange messages normally with `@name`. Use
`--force-target` directly only for a raw one-off delivery; prefer
`comm-bootstrap.sh` for enrollment.

## Seeing the new row in the FE

The frontend learns about new workspaces on its next `workspace.list` poll —
refresh the session list (enter Sessions mode) after spawning and the new
row appears, then Ctrl+PageDown to switch to it.
