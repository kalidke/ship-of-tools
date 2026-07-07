# Using Ship of Tools

Welcome — you've installed Ship of Tools and have the repo checked out locally.
This page is the entry point for what to do next.

## What this is

Ship of Tools is a keyboard-driven development environment for Julia that turns
coding into **directing Claude Code agents**: a four-pane window (navigation,
preview, orchestrator, REPL) lets you steer, watch, and review while the agent
writes and runs the code. It is opinionated by design — the layout isn't
configured, it's built around one workflow.

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

If you haven't already, walk the [Quickstart](https://kalidke.github.io/ship-of-tools/start/quickstart/)
in the published docs: shortest install, first launch, connecting, opening a
project, and the handful of keys that get you moving. (Locally, that's
`docs/src/start/quickstart.md`.)

## Extending it

Ship of Tools is built to be extended with Julia multiple dispatch, not Rust
changes. Start with:

- `docs/src/extend/filetype.md` — writing a `FileType` plugin (the smallest,
  most common extension: recognize and preview a new kind of file).
- `examples/plugins/` — a complete worked example package (`HDF5Preview`)
  showing the pattern end to end, from outside core.

When in doubt, ask the orchestrator pane's Claude Code agent — it has read
both.
