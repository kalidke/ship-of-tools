# julia/pluto: the daemon's Pluto server (sidecars)

One secret-gated Pluto server per daemon; the daemon runs start.jl with this folder as the project. Part of sidecars;
charter: rust/backend/src/sidecars/CLAUDE.md.

## Files
- `Project.toml`: the server's environment.
- `start.jl`: binds the port, configures Pluto, then serves the stdio protocol.
- `session_options.jl`: `configure_session!`, the daemon's Pluto options (loopback, the access secret, Distributed workers).
- `test/runtests.jl`: that a stranger's call to a notebook worker runs nothing and the notebook still evaluates for its owner (run directly: `julia --project=julia/pluto julia/pluto/test/runtests.jl`).

## Start here
`start.jl` for the protocol; `session_options.jl` for the security options.

## Rules
- Stdio protocol: `READY http://127.0.0.1:<port>` once bound; then one `URL <url>` or `ERR <msg>` per
  `OPEN <abspath>`.
- Every request needs the session secret, and every URL carries it before `id` (`edit_url`).
- Every notebook runs in a Distributed worker (`workspace_use_distributed_stdlib`), which checks the cluster cookie it read from its stdin on every connection; Pluto's default Malt worker accepts the first connection with no secret. On Windows Pluto cannot stop a running cell in this mode.
- The port is 1234 or ephemeral (`pick_port`), and the daemon learns it only from `READY`.

Record: ADR 0035.
