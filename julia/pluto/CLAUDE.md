# julia/pluto: the daemon's Pluto server (sidecars)

One secret-gated Pluto server per daemon; the daemon runs start.jl with this folder as the project. Part of sidecars;
charter: rust/backend/src/sidecars/CLAUDE.md.

## Files
- `Project.toml`: the server's environment.
- `start.jl`: binds the port, configures Pluto, then serves the stdio protocol.

## Start here
`start.jl`, for any change to the protocol or the security options.

## Rules
- Stdio protocol: `READY http://127.0.0.1:<port>` once bound; then one `URL <url>` or `ERR <msg>` per
  `OPEN <abspath>`.
- Every request needs the session secret, and every URL carries it before `id` (`edit_url`).
- The port is 1234 or ephemeral (`pick_port`), and the daemon learns it only from `READY`.

Record: ADR 0035.
