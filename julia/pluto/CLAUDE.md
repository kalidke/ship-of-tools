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
  line; start.jl refuses a request without the secret; and (Linux) start.jl with one open notebook listens on exactly
  its own port and the worker's (run directly: `julia --project=julia/pluto julia/pluto/test/runtests.jl`).

## Start here
`start.jl` for the protocol; `session_options.jl` for the security options.

## Rules
- Stdio protocol: `READY http://127.0.0.1:<port>` once bound; then one `URL <url>` or `ERR <msg>` per
  `OPEN <abspath>`.
- Every request needs the session secret (32 hex characters from the OS's secure generator, set by
  `configure_session!`), except Pluto's own public script, style and font files and `/ping`; every URL carries the
  secret before `id` (`edit_url`).
- Every notebook runs in a Distributed worker (`workspace_use_distributed_stdlib`), which checks the cluster cookie it
  read from its stdin on every connection before it reads a message; `configure_session!` draws the cookie from the
  OS's secure generator. Pluto's default Malt worker accepts the first connection with no secret and is not used. On
  Windows Pluto cannot stop a running cell in this mode.
- Pluto is pinned exactly; a new Pluto version enters only with this folder's test passing on it.
- The port is 1234 or ephemeral (`pick_port`), and the daemon learns it only from `READY`.

Record: ADR 0035.
