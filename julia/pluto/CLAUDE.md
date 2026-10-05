# julia/pluto: the daemon's Pluto server (sidecars)

One secret-gated Pluto server per daemon; the daemon runs start.jl with this folder as the project. Part of sidecars;
charter: rust/backend/src/sidecars/CLAUDE.md.

## Files
- `Project.toml`: the server's environment, with Pluto pinned to the version this folder's test passes on.
- `start.jl`: binds the port, configures Pluto, then serves the stdio protocol.
- `session_options.jl`: `configure_session!`, the daemon's Pluto options (loopback, the session secret, Distributed
  workers and their cookie).
- `test/runtests.jl`: a stranger's call to a notebook worker runs nothing and the notebook still evaluates for its
  owner; the session secret and the cluster cookie are drawn per session from the OS's generator and on no command
  line; start.jl answers nothing outside its secret path (on Windows, not a drive path either); and (Linux) start.jl with one open notebook listens on exactly
  its own port and the worker's (run directly: `julia --project=julia/pluto julia/pluto/test/runtests.jl`).

## Start here
`start.jl` for the protocol; `session_options.jl` for the security options.

## Rules
- Stdio protocol: `READY http://127.0.0.1:<port>` once bound; then one `URL <url>` or `ERR <msg>` per
  `OPEN <abspath>`.
- Pluto serves every route under a path that is its session secret (`base_url`), which `configure_session!` sets with
  the secret (32 hex characters from the OS's secure generator). A request without it gets 403 or 404, Pluto's own
  files and `/ping` included. A URL is `http://127.0.0.1:<port>/<secret>/edit?secret=<secret>&id=<id>`, the query's
  secret before `id` (`edit_url`).
- The path segment must stay the session secret. Under it Pluto serves a path that ends in an asset extension (`.js`,
  `.css`, `.json` and others) without the query secret, and its static route does not keep the file inside Pluto's
  own folder (on Windows a drive path there reads any such file). A separate or shareable path would open both.
- Every notebook runs in a Distributed worker (`workspace_use_distributed_stdlib`), which checks the cluster cookie it
  read from its stdin on every connection before it reads a message; `configure_session!` draws the cookie from the
  OS's secure generator. Pluto's default Malt worker accepts the first connection with no secret and is not used. On
  Windows Pluto cannot stop a running cell in this mode.
- Pluto is pinned exactly; a new Pluto version enters only with this folder's test passing on it.
- The port is 1234 or ephemeral (`pick_port`), and the daemon learns it only from `READY`.

Record: ADR 0035.
