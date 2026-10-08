# rust/backend/tests/daemon_lifetime/fixtures: the real inputs and process trees of the harness's cases (tests)

What the cases give the products they run, so that a case exercises the product's own route and not a stand-in, and the
process trees they watch for an end. Part of the daemon-lifetime harness; page: rust/backend/tests/daemon_lifetime/CLAUDE.md.

## Files
- `tree.rs`: `Tree`, a leader, a child and a grandchild that report their pids (the last two ignore TERM and HUP; the leader is started detached), and the forking tree; `session_members`
- `notebook.jl`: a Pluto notebook of one trivial cell; the case's worker is started by the startup expression the case's own copy of `session_options.jl` carries
- `render.qmd`: a Quarto document on the Julia engine whose one chunk reports the worker's pid and its parent's (the engine server), ignores TERM and HUP and starts a tree detached

## Rules
- A process a case holds comes from the fixture's own report (a pid file), from the product over its authenticated lane, from the
  daemon's control connection (`SO_PEERCRED`) or from the children list of a process the case holds; a parent reached by walking up
  is observed through a pidfd the case never signals (`Fixture::observe`).
