# Using Ship of Tools

Welcome — you've installed Ship of Tools and have the repo checked out locally.
This page is the entry point for what to do next.

## What this is

Ship of Tools is a keyboard-driven development environment for Julia that turns
coding into **directing Claude Code agents**: a window of three columns
(navigation, preview, agent) and a bottom drawer (Julia REPL, Terminal, Help,
Monitor) lets you steer, watch, and review while the agent writes and runs the
code. The layout follows your monitor's shape and takes a few settings; see
`docs/src/ref/config.md`.

## Help while you work

The focused pane's border shows relevant shortcuts. Press **Ctrl+?** for a
five-second action overlay, then press it again for the persistent **Help drawer**.
**F1** opens Help directly. Search actions, inspect their current bindings, and
follow their manual links. Help shares the drawer with Julia, Terminal and Monitor;
Julia continues running when another drawer view is shown. Escape returns you to
where you were working. See `docs/src/ref/keybindings.md` for configuration.

## Ask the app, not just the docs

Your workspace's Claude Code agent — running in the **agent pane**
(the right column), the agent associated with your workspace — is more than a
coding assistant; it's also your help system. Ask it plain questions
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

If you haven't already, walk the [Quickstart](https://kalidke.github.io/ship-of-tools/dev/start/quickstart)
in the published docs: shortest install, first launch, connecting, opening a
project, and the handful of keys that get you moving. (Locally, that's
`docs/src/start/quickstart.md`.)

## State, survival and Windows launches

Where the daemon keeps its state, what a session survives and the Windows rule about launching from an agent's shell
are in the manual (published at <https://kalidke.github.io/ship-of-tools/dev/>; locally under `docs/src/`):

- [Sessions and persistence](https://kalidke.github.io/ship-of-tools/dev/concepts/sessions/) — "Capsules: what a
  session survives" and "Where the state lives" (`docs/src/concepts/sessions.md`).
- [Troubleshooting](https://kalidke.github.io/ship-of-tools/dev/guide/troubleshooting/) — "A session will not start on
  a shared home" (the `state_root_unqualified` refusal and the per-host `XDG_STATE_HOME` drop-in) and "Windows: never
  launch the daemon from an agent's shell" (`docs/src/guide/troubleshooting.md`).

`SOT_SELF_HOST` is the one declared-host override (ADR 0046): the daemon,
the frontend and the comm scripts all read it, so a value set for one of
them must be set for all of them on that box — it names the per-host state
dirs and the registry's ownership check compares it. systemd does not read it:
the host pins on `sotd.service` and on the hub's relay units compare the kernel
hostname, so a Linux box whose hosts.toml name differs from its hostname runs
`sotd` without the unit (`--no-service`).

## Extending it

A new file type is a `FileType` plugin written in Julia with multiple dispatch, no Rust changes. The kernel loads
plugins only from its own project, so in a source checkout you add the plugin package to the kernel's environment
(`julia/kernel/Project.toml`) and a `using` line in `julia/kernel/src/ShipToolsKernel.jl`; a package in your own
project is not picked up. Start with:

- `docs/src/extend/filetype.md` — writing a `FileType` plugin (the smallest,
  most common extension: recognize and preview a new kind of file).
- `examples/plugins/` — a complete worked example package (`HDF5Preview`)
  showing the pattern end to end, from outside core.

When in doubt, ask the agent pane's Claude Code agent — it has read
both.
