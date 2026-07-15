# ADR 0010: Transport, persistence, and reconnect

**Status:** Accepted
**Date:** 2026-05-07

## Context

The user's primary workflow is always remote: Windows local frontend → SSH → a Linux remote host where the Julia kernel and GPU live. Local-only operation is a fall-through, not the baseline. The session must survive SSH disconnects (the tmux property), allow reconnect from a fresh client, and not depend on host-specific port allocation.

ADR 0001 fixed the wire format (NDJSON + length-prefixed binary blobs); this ADR fixes how that wire is carried.

## Decision

**Backend** runs as a long-lived daemon on the remote, supervised by a named tmux session (`devenv-backend-<session_id>`). `systemd --user` is acceptable as a future cleaner alternative — kept open, not chosen now. The backend listens on a **per-session Unix socket** at `$XDG_RUNTIME_DIR/devenv/<session_id>.sock`, not a TCP port.

**Frontend** runs locally. On launch it spawns or attaches to an SSH connection and forwards the remote socket to a local socket via:

```
ssh -o ExitOnForwardFailure=yes \
    -o ServerAliveInterval=15 \
    -o StreamLocalBindUnlink=yes \
    -L "$LOCAL_SOCK:$REMOTE_SOCK" \
    <remote>
```

(Where Unix-socket forwarding isn't available — older Windows OpenSSH builds — fall back to a per-session local TCP port allocated at runtime, never fixed.)

**Authentication** is transport-specific. SSH itself authenticates the Unix user
for remote Unix-socket access, and the socket path is private to that user.
Direct TCP remains app-token-gated because localhost on a shared remote is
machine-scoped, not user-scoped.

**Reconnect protocol:** every connect carries `{session_id, client_id, last_seen_revision}`. Backend either replays missed events from a bounded ring (last N seconds / N events) or sends a snapshot if the client is too far behind. Heartbeats every ~5 s evict stale clients.

**Multi-client semantics** are deferred but not assumed away. The protocol carries `client_id` from day one. The day-one policy is "first client gets read+write; second client read-only follower"; final policy decision lives in a future ADR before M3.

## Consequences

- The backend, kernel, log files, and socket all live under `$XDG_RUNTIME_DIR/devenv/<session_id>/`. Sockets clean up via `StreamLocalBindUnlink`; logs rotate; PIDs are recoverable for `kill` / `tmux attach`.
- The frontend never touches kernel state directly — even when the user runs everything on one laptop. Local-only is "client connecting to localhost-bound backend," not a different code path.
- Per-session local port/socket allocation rules out fixed-port collisions when the user runs two DevEnv sessions on different remotes.
- The connect handshake's `last_seen_revision` is what makes reconnect feel like tmux. Without it, every disconnect loses the orchestrator session and any in-flight kernel work.
- App tokens get persisted in `~/.config/devenv/tokens.toml` (gitignored, 0600), keyed by `(remote_host, session_id)`. Frontend rotates them on user request.
- This ADR's transport is orthogonal to the line format. ADR 0001 still applies; this just says where the bytes flow.

## Update (2026-07-14, v0.4.0): daemon TCP listener removed

The direct-TCP transport — and the app-level auth token that existed to gate
it — is gone. `sotd --tcp <host:port>`, `--token`, `$SOT_TOKEN`, the
`~/.config/sot/token` file, and `--insecure-no-auth` were removed in 0.4.0;
a stale launcher passing any of them gets a pointed startup error naming
this ADR.

Why: by the time of removal, no deployment used it. Every canonical
topology rides the private per-user local socket (AF_UNIX / Windows named
pipe), with cross-machine access as an SSH local-forward *terminating at
the socket* — the "fall back to a local TCP port" note above describes the
frontend's LOCAL tunnel endpoint, which is unaffected (the FE still dials
`--tcp 127.0.0.1:<port>` into its own forward). The daemon listener's one
field appearance was the 2026-07-11 twin-daemon split-brain incident — an
escape hatch that only ever produced an incident.

Consequences: the socket's access control is purely OS ownership of its
private (0700) parent directory; the hello `token` wire field survives for
cross-version compat and is ignored (old FEs presenting tokens still
connect); the half-open reaper keeps only its write-timeout half
(SO_KEEPALIVE lived in the TCP listener; a dead SSH forward closes the
local stream with EOF). The release smoke tests boot via the Unix socket on
all platforms and assert `--tcp` fails loudly.
