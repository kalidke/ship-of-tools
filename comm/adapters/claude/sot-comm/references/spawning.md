# Spawning — the rarer knobs

`SKILL.md` covers the common case. This is for the rest.

## Endpoint + tuning

`comm-spawn.sh` auto-detects the daemon endpoint from explicit env, an old dev
`--socket` daemon flag, or `sotd session-socket-path
${SOT_BACKEND_LABEL:-sot}`. Override with `--endpoint unix:/path/to/sot.sock`
or `--endpoint ssh:target[/host]`. It waits up to
`SOT_COMM_SPAWN_CAPSULE_WAIT` seconds (default 90) for the new row to reach phase `ready`.

## A durable peer instead of a task agent

`comm-spawn.sh` is for delegation with a report-back-and-despawn lifecycle.
For a long-lived comm-aware backend session (survives a restart, which
starts a fresh conversation that re-bootstraps its own receive path), the path depends on who
is spawning:

- **You are a Claude session (or headless)**: still use `comm-spawn.sh` in
  workspace mode — the daemon + FE autostart give claude a clean env and a
  real attach.
- **A human at a shell**: create the row from the FE Sessions mode (or run
  `comm-spawn.sh`); the daemon no longer starts `ccb` — it runs the agent
  recipe directly via `sotd agent-exec` in the row's capsule. Never take a
  registry handle that already exists, even one that looks stale.

## A bash row

`comm-spawn.sh <repo-path> --agent none` makes a row that runs the daemon's
login shell (`$SHELL`, else `/bin/sh`; `cmd.exe` on Windows) and no agent.
Nothing there joins comm, so it takes no `--name` or `--task`, gets no
registry row or inbox, and the daemon refuses any `--account` but `default`
for it. The script waits for the row to reach `ready`, then prints the
`comm-despawn.sh` command that ends it, with the row's id and the endpoint
the spawn used. `comm-bootstrap.sh` refuses it.

## Git worktrees

Spawning into an **isolated full-repo checkout** (a parallel branch to
build/edit without disturbing the main tree) is the `/worktree` skill's job
end to end — it creates the worktree, picks the naming, and spawns the
session bound to it. Don't hand-roll `git worktree add` here; see
`/worktree` for `new` / `status` / `sync` / `clean`.

## Bootstrapping a session that hasn't joined yet

A session is only addressable by `@name` once it has joined — that's the
consent model. If another session has the skill installed but hasn't
joined, ask its owner to run `/sot-session-start` in that session, or type
the nudge yourself: `comm-bootstrap.sh <slug|label|workspace_id>
[suggested-name]` types a self-contained "run comm-join then reply to me"
message into that row through the daemon's `pty.input` — the only path
that bypasses the registry. Once the target joins it appears in
`comm-list.sh` and you exchange messages normally with `@name`.

## Seeing the new row in the FE

Every window connected to the daemon re-lists when a row is created or
destroyed, so the row appears in (or leaves) its session strip without a
refresh. A spawn moves no window off the row it is on; the user cycles to
the new row with Shift+ArrowRight / Shift+ArrowLeft.
