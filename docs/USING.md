# Using Ship of Tools

Welcome — you've installed Ship of Tools and have the repo checked out locally.
This page is the entry point for what to do next.

## What this is

Ship of Tools is a keyboard-driven development environment for Julia that turns
coding into **directing Claude Code agents**: a four-pane window (navigation,
preview, orchestrator, REPL) lets you steer, watch, and review while the agent
writes and runs the code. It is opinionated by design — the layout isn't
configured, it's built around one workflow.

## Help while you work

The focused pane's border shows relevant shortcuts. Press **Ctrl+?** for a
five-second action overlay, then press it again for the persistent **Help drawer**.
**F1** opens Help directly. Search actions, inspect their current bindings, and
follow their manual links. Help shares the drawer with Julia, Terminal and Monitor;
Julia continues running when another drawer view is shown. Escape returns you to
where you were working. See `docs/src/ref/keybindings.md` for configuration.

## Ask the app, not just the docs

Your workspace's Claude Code agent — running in the **orchestrator pane**
(bottom-left), the agent associated with your workspace — is more than a
coding assistant; it's also your help system. (In the dev self-host setup
only, the Terminal drawer, `Ctrl+T`, also runs a Claude session; on a normal
install that drawer is a plain shell.) Ask the orchestrator plain questions
like:

- "How do I add a new preview file type?"
- "What does the concept layer do?"
- "How does reconnect work after my laptop sleeps?"

It answers from **this repo**, checked out locally right where you're reading
this. Every Ship of Tools workspace exports **`$SOT_MANUAL`**, pointing at this
checkout — the manual's root — so the agent reads straight from it: the docs
under `$SOT_MANUAL/docs/` (including this file, `$SOT_MANUAL/docs/USING.md`),
the design decisions in `$SOT_MANUAL/docs/adr/`, the scope document
`$SOT_MANUAL/requirements.md`, and the full user guide under
`$SOT_MANUAL/docs/src/guide/`. Nothing here is a duplicate — it's the same
corpus, just read directly instead of rendered to a website. If the published
site doesn't have the depth you need, the agent does.

## Getting started

If you haven't already, walk the [Quickstart](https://kalidke.github.io/ship-of-tools/dev/start/quickstart/)
in the published docs: shortest install, first launch, connecting, opening a
project, and the handful of keys that get you moving. (Locally, that's
`docs/src/start/quickstart.md`.)

## Where the daemon keeps its state

The backend daemon's state root is `${XDG_STATE_HOME:-~/.local/state}/sot` —
capsule session records (Ship's Log voyages) and the daemon's own `sotd.log`
live there. It must be a local, durable filesystem: the daemon refuses to
start a capsule row on a REMOTE filesystem (NFS, SMB/CIFS, 9p, an
unqualified FUSE mount) or a VOLATILE one (tmpfs, ramfs), answering
`state_root_unqualified` and naming the filesystem.

On a shared home (the same `$HOME` mounted on multiple backend hosts over
NFS), point the state root at each host's own local disk with one drop-in,
verbatim, per host:

```
~/.config/systemd/user/sotd.service.d/state.conf
```

```ini
[Service]
Environment=XDG_STATE_HOME=/scratch/<user>/state
```

then, on that host:

```
systemctl --user daemon-reload && systemctl --user restart sotd
```

The daemon's log moves with the root — a fresh `sotd.log` at the new
location; the old one stays where it was, as history.

**Relocation does not migrate rows.** Changing `XDG_STATE_HOME` points the
daemon at a different (and initially empty) state root — it does not move
anything there for you. Before changing it on a host that already has
capsule rows: end that host's capsule runs first; if any complete state
directories need to survive, move them yourself (their ids are the
directory names, and must be preserved); then verify the daemon's actual
environment after the change (`systemctl --user show sotd -p Environment`)
and that the destination has the retention you expect.

`SOT_STATE_HOST`, when set, must equal the short hostname sot-comm's own
registry stamps on that host's rows (case-insensitive) — the registry's
ownership check compares them.

## Windows: never launch from inside a capsule shell

Never launch the frontend or the daemon from a shell running inside a
capsule row — that shell's own job forbids breakaway, so a daemon started
there could never free the supervisors it spawns. A daemon that finds
itself inside a job that forbids breakaway refuses to create capsule rows
and says so, rather than spawning a supervisor that would silently die
with that job.

## Extending it

Ship of Tools is built to be extended with Julia multiple dispatch, not Rust
changes. Start with:

- `docs/src/extend/filetype.md` — writing a `FileType` plugin (the smallest,
  most common extension: recognize and preview a new kind of file).
- `examples/plugins/` — a complete worked example package (`HDF5Preview`)
  showing the pattern end to end, from outside core.

When in doubt, ask the orchestrator pane's Claude Code agent — it has read
both.
