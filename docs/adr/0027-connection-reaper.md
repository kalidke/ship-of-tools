# ADR 0027: Half-open connection reaper — TCP keepalive + bounded write-timeout

**Status:** Accepted (diagnosed + implemented; FE/tunnel facts and timings confirmed, 2026-06-26)
**Date:** 2026-06-26

## Context

The daemon (`sotd`) accepts one connection per frontend, per comm bridge, and
per CLI probe, and serves each from an independent `tokio` task in
`server.rs::handle_connection`. Every task subscribes to the broadcast buses
(preview / workspace / agent-relay / fe-command / repl-frame / monitor) and, in
its `tokio::select!` loop, writes evt frames to its own socket via
`codec::write_frame(&mut tx, …).await`.

Over a session's life the daemon accumulated **leaked half-open connections**.
The trigger we observed: ~6 frontend relaunches left dead daemon-side sockets,
and eventually the comm relay **stalled** — a session's messages stopped
reaching others until the daemon was restarted. Forensics on the wedged daemon
showed an `ESTAB` loopback connection (`127.0.0.1:60770`) with **no client
process** and a backed-up `Recv-Q` (4618 bytes the daemon had never read).

### Why it happened

A peer that dies **without sending a FIN** — a frontend `kill -9`, a collapsed
SSH local-forward, a yanked network — leaves the daemon-side socket `ESTAB`
indefinitely. The OS never tells the daemon the peer is gone, so:

- If the connection is **idle** (no evts pending), its task sits parked in the
  `select!` read arm forever. The socket fd and the connection's `ClientGuard`
  (hence `clients.count()` / `clients_connected`) leak.
- If the connection is **active** (an evt arrives to forward), the task calls
  `write_frame(&mut tx, …).await`. The dead peer never drains, the socket's
  outbound buffer fills, and the write future **parks forever**. The task is now
  stuck in the write arm — it never returns to the read arm, so it also stops
  reading inbound frames on that socket (the `Recv-Q 4618` we saw). For a comm
  **bridge** connection — which both feeds `agent.send` *into* the daemon and
  receives broadcasts back over the same socket — a parked write therefore also
  blocks that bridge's inbound sends from ever being read and re-broadcast. That
  is the relay stall.

Note what is **not** the cause: the `tokio::broadcast` channels cannot block the
sender. A slow receiver gets `RecvError::Lagged` (handled in every `write_*`
helper — skip + warn, keep the connection), and the channel drops the oldest
buffered messages for it. So this is a **dead-connection** problem, not a
channel-capacity problem; the fix is to reap dead connections, not to tune the
buses.

The frontend and tunnel are **not** the leak source (confirmed by win-fe):

- The FE's exit-75 relaunch (ADR 0017) is `std::process::exit(75)` — abrupt, no
  Rust destructors — but the OS reclaims fds on exit, so a FIN **is** emitted on
  the daemon socket. A clean relaunch closes cleanly.
- The SSH tunnel runs with `ServerAliveInterval=30 ServerAliveCountMax=6
  ExitOnForwardFailure=yes` on the `-L 18743` forward, so a dead tunnel
  propagates closure too.

The leak is the genuinely-silent death (kill -9 / hard network drop) that
neither path can signal — exactly the case TCP keepalive exists for.

## Decision

Reap dead connections at two layers in `server.rs`. Both are needed; they cover
disjoint failure modes.

### 1. TCP keepalive on every accepted TCP socket (`run_tcp`)

After `set_nodelay`, set `SO_KEEPALIVE` plus a tight probe cadence via
`socket2::SockRef`:

| Tunable | Value | Maps to |
|---|---|---|
| `KEEPALIVE_IDLE` | 20 s | `TCP_KEEPIDLE` |
| `KEEPALIVE_INTERVAL` | 10 s | `TCP_KEEPINTVL` |
| `KEEPALIVE_RETRIES` | 3 | `TCP_KEEPCNT` |

→ a vanished peer is RST'd ~50 s after it dies; the daemon's read returns `Err`,
`handle_connection` unwinds, the socket closes, and the `ClientGuard` drops.
This reaps the **idle** half-open case (a parked read arm that no write-timeout
would ever fire on). It is also the **universal** net: it covers every write
path, including `stream_file_download`, which writes its own frames and is not
wrapped by layer 2.

`socket2` is already in the lockfile as a transitive `tokio` dependency, so the
direct dependency compiles nothing new. Keepalive does not apply to the local
(Unix-socket / Windows-pipe) transport — a same-host peer's death closes the
socket promptly anyway — so layer 1 is TCP-only; layer 2 backstops local.

### 2. Bounded write-timeout on every per-connection frame write

A `write_frame_to` wrapper sends each frame under
`tokio::time::timeout(WRITE_TIMEOUT, …)`; on elapse it returns `Err` so
`handle_connection` drops the connection. `WRITE_TIMEOUT = 10 s`.

→ this reaps the **active-but-not-draining** case — the witnessed `Recv-Q`
stall — fast and on **any** transport, without waiting on OS keepalive tuning.
A healthy client, even a remote one over the tunnel, drains a frame in well
under a second, so 10 s never false-trips on legitimate slowness; and a rare
false drop is cheap because the FE reconnects automatically (exponential backoff
200 ms → 5 s cap, plus F5 to force). Cancel-safety on the timeout path is moot:
we tear the whole socket down, so a partially-written frame doesn't matter.

All nine per-connection write sites (the pty-evt arm, the `pty.open` error reply,
the request-response loop, and the six broadcast `write_*` evt helpers) route
through `write_frame_to`. The core is split into a timeout-parameterized
`write_frame_within` so the drop-on-stuck-peer behavior is unit-tested in
milliseconds (a never-draining writer trips it; a `Vec` sink does not).

## Consequences

- A silently-dead peer is reaped in ≤ ~50 s (idle) or ≤ 10 s (mid-write); its
  fd and `ClientGuard` are released, so `clients_connected` stays truthful and
  the relay can no longer wedge behind one stuck bridge.
- The fix is daemon-local (`server.rs` + one `Cargo.toml` line); the FE needs no
  change — its existing clean-close + reconnect compose with reaper drops.
- `WRITE_TIMEOUT` and the keepalive cadence are the knobs if a future slow link
  ever false-trips; 10 s / ~50 s are deliberately conservative on the safe side.

## Known gap

`stream_file_download` writes its own frames inside `handlers.rs` and is **not**
wrapped by the write-timeout, so a peer that stalls *mid-download* is reaped only
by keepalive (≤ ~50 s), not the faster 10 s path. Acceptable: downloads are an
infrequent path and keepalive still bounds the leak. Wrapping it would mean
threading the timeout into the handler's write loop — deferred until it matters.

## Alternatives considered

- **Tune / bound the broadcast channels.** Wrong layer — `tokio::broadcast`
  already never blocks the sender (`Lagged`), so the buses were never the stall.
- **App-level heartbeat / ping-pong with an idle reaper.** More moving parts
  (a periodic sweep, last-activity bookkeeping per connection) for no coverage
  that keepalive + write-timeout don't already provide. Deferred unless the two
  TCP-level mechanisms prove insufficient.
- **Daemon-side dead-connection sweep task.** Same outcome as keepalive but
  reimplemented in userspace; the kernel already does this correctly via
  `SO_KEEPALIVE`.
