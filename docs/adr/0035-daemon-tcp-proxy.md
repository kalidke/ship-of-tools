# ADR 0035: Daemon TCP proxy — any backend page through the one control tunnel

**Status:** Proposed (implements ADR 0024's deferred generalization / rejected-alt #4; supersedes the per-port `-L` sprawl as the primary reachability mechanism — the launcher forwards stay as legacy fallback)
**Date:** 2026-07-21

## Context

Every backend HTTP surface today is reachable from a remote FE only through a
**static per-port ssh `-L` forward** baked into the launchers: Pluto 1234,
video 1235, docs 1236, docs pool 1237–1240, WGLMakie/Bonito 1241
(`launch-sot.ps1:456-495`, ADR 0018/0024/0029/0032). The failure class is
proven and recurring: a box whose launcher predates a port serves dead pages
until a relaunch (the PR #11 forward-gap; the 2026-07-20 WGL box-B incident),
and every NEW backend port means touching every launcher + one relaunch per
box. The maintainer's ruling (2026-07-20): **"sot is designed to have
transparent FE switching or multiple FE"** — reachability must be transparent
*by construction*, not per-box-checklist.

ADR 0024 already named the route (`0024:159-162`): *"Proxy HTTP over the
existing daemon socket (tunnel request/response frames through the protocol —
no new port, no ssh). The most promising route for the general 'any port' case
and genuinely cross-OS... Deferred to the generalization phase."* Also
load-bearing from 0024: the FE must **not** spawn its own ssh, and
Win32-OpenSSH cannot add forwards dynamically — dynamic `-L` is a dead end.

Constraints discovered in the 2026-07-21 code map:

- The daemon's control connection is a single multiplexed NDJSON+blob select
  loop with load-bearing cancel-safety (`server.rs:708-767`); **no op may
  monopolize it as a raw pipe**, and tunnelling proxy bytes as multiplexed
  data frames through it would add head-of-line blocking against pty/repl
  traffic plus a bespoke backpressure design.
- The daemon happily accepts **many** connections on its Unix socket
  (`server.rs:667-704`); the FE's `-L 18743:<remote-socket>` control tunnel
  forwards **each** new TCP connection as a fresh Unix-socket connection.
- Bonito/WGLMakie needs the **WebSocket upgrade**; the existing hand-rolled
  HTTP servers are GET/HEAD-only, so any HTTP-level (request/response frame)
  proxy would have to reimplement WS. A TCP-level pipe carries it for free —
  exactly why the ssh `-L` path worked (ADR 0032:66-70).
- **The daemon does not know WGL's port**: `SOT_WGL_PORT` (default 1241) is
  read only inside the user's REPL child (`ShipToolsRepl.jl:67`); no backend
  Rust file references it. Any daemon-side allowlist must add it explicitly.

## Decision

### 1. TCP-level proxy on **dedicated** daemon-socket connections

A client opens a **new** connection to the daemon socket and sends, as its
**first frame**, `proxy.connect {port, token?}`. The daemon:

1. validates the port against the allowlist (§3) — reject ⇒ error frame + close;
2. dials `127.0.0.1:<port>` — refused ⇒ error frame + close;
3. replies `proxy.connect` res `{ok:true}`;
4. **hands the connection to `tokio::io::copy_bidirectional`** — from this
   moment the connection is a raw byte pipe until either side closes.

No multiplexing, no data frames, no chunking: backpressure is native TCP, the
WS upgrade is invisible payload, and the control connection is untouched. The
accept path grows one early-dispatch branch (first-frame op == `proxy.connect`
→ proxy path; anything else → the existing hello-gated loop). The per-frame
write-timeout reaper does not apply to piped connections (they are not frame
writers); an idle pipe dies with its TCP/Unix peer.

### 2. FE half: lazy loopback listeners through the control tunnel

The FE (remote transport only) gains a tiny `proxy_listen` facility:

- `ensure_proxy_listener(port)` lazily binds `127.0.0.1:<port>` locally. Each
  accepted browser connection dials the daemon endpoint (the SAME
  `127.0.0.1:18743` control-tunnel address it already uses), performs the
  `proxy.connect` handshake, then `copy_bidirectional`.
- Called from every URL-opening path just before `open_url_in_browser`:
  `DocsOpened`, `VideoOpened`, `ReplFrame::Browser` (WGL), and the
  `open_url` fe.command — parse the port out of any `127.0.0.1:<port>` URL.
- **`AddrInUse` ⇒ silently skip**: a legacy launcher's ssh `-L` already holds
  that port and serves it — the two mechanisms coexist; the proxy simply
  makes the launcher list unnecessary going forward.
- **Local (pipe-connected) FEs never bind**: the daemon's servers hold those
  loopback ports on the same box; direct reach already works.

Browser URLs are **unchanged** (`http://127.0.0.1:<port>/…` on both ends) —
no rewriting, no new user-visible surface.

### 3. Security: loopback-only + port allowlist

`proxy.connect` dials **loopback only** (hardcoded — the frame carries no
host). Allowed ports = the daemon's own served set, computed at request time:

- `1234` (Pluto sidecar), `http_serve::video_port()`,
  `site_serve::site_port()`, `site_serve::pool_ports()`,
- `SOT_WGL_PORT` (default **1241**) — now also read daemon-side, closing the
  "daemon doesn't know 1241" gap; config duplication is accepted (both sides
  default identically),
- `SOT_PROXY_EXTRA_PORTS` (comma-separated) — escape hatch for future
  REPL-served dashboards without a daemon release.

Anything else ⇒ `bad_port` error. Auth mirrors the existing op gate: on local
transport, Unix-socket fs perms are the trust boundary (as for every op,
`ops.rs:267-269`); the optional `token` field is honored when the daemon has
one configured. Note the proxied servers keep their own second factor (docs
pool secret+cookie, prefix nonce, video opaque tokens).

### 4. Capability gating

`HelloRes` gains `#[serde(default)] proxy: bool` (additive, back-compat both
directions). The FE arms `proxy_listen` only when the daemon advertised it;
otherwise it relies on the legacy forwards exactly as today.

## Consequences

- A new backend port becomes reachable from every FE by **adding it to the
  allowlist** — no launcher edits, no per-box relaunches, no ssh changes.
  The WGL/1241 class of incident is retired.
- The launchers' aux `-L` list becomes redundant but stays for one release
  cycle as a fallback (old daemon + new FE, or proxy bugs); removal is a
  follow-up once the proxy has field time.
- Each browser connection costs one extra Unix-socket connection + one
  `copy_bidirectional` task on the daemon — the same order as the ssh
  forward it replaces.
- kitt-native FEs are untouched (they never proxy).

## Rejected alternatives

1. **Multiplexed `proxy.data` frames over the control connection** — head-of-
   line blocking against pty/repl traffic; needs bespoke flow control; the
   blob codec's cancel-safety machinery makes surgery risky. Dedicated
   connections cost nothing the ssh forward didn't already cost.
2. **HTTP-level request/response proxying** — must reimplement WebSocket for
   Bonito; loses range requests/streaming subtleties the video server relies
   on; strictly more code for strictly less fidelity.
3. **Dynamic ssh forwards** — ruled out in ADR 0024 (FE must not spawn ssh;
   Win32-OpenSSH can't add forwards live).
4. **Teaching every launcher every port forever** — the status quo this ADR
   exists to end.

## Addendum (2026-07-23): ephemeral content-server ports — the multi-user collision

**Incident.** On a shared host (multiple users, shared `$HOME`), two users' daemons
both defaulted to the same content-server ports. The second daemon to start
lost every bind: its video server (1235) and the whole docs surface
(1236-1240) silently belonged to the *other user's* daemon. The losing
user's `o` opened a browser against the winner's video server — "no such
grant" — and `W` was dead. Worse than broken: the loser's grant token was
sent to another user's process. `127.0.0.1:<port>` is a single global
namespace per host; fixed defaults cannot be safe with two daemons on one
box.

**Decision.** Preferred-then-ephemeral binding for every daemon-owned
content server, with the *actual* bound port flowing everywhere the port
matters:

- `http_serve` (video), `site_serve` (docs shared), and the docs pool each
  try their preferred port (`SOT_VIDEO_PORT` 1235 / `SOT_DOCS_PORT` 1236 /
  pool 1237-1240) and fall back to an OS-assigned ephemeral port
  (`127.0.0.1:0`) when it is taken. The actual port is recorded
  (`bound_video_port()` / `bound_site_port()` / `BOUND_POOL`).
- The Pluto sidecar (`start.jl`) does the same probe-and-fallback in Julia;
  the daemon parses the actual port from the sidecar's `READY <url>` line
  (`bound_pluto_port()`), which it already consumed.
- `video.open` / `docs.open` URLs are built from the actual ports — never
  the preferred ones. If a server is genuinely down (even the ephemeral
  bind failed), the op returns a typed error (`video_server_down` /
  `site_server_down`) instead of a dead URL.
- The proxy allowlist is now **verified-bound**: it authorizes only ports
  this daemon (or its Pluto sidecar) actually bound, closing the previous
  "Residual (accepted)" note for video/docs/pluto — the proxy can no longer
  be pointed at a stranger's process squatting a preferred port. The one
  remaining configured-not-verified entry is `SOT_WGL_PORT` (1241): that
  server binds lazily inside the user's REPL child where the daemon cannot
  observe it. Per-child WGL port assignment is the tracked follow-up.

**Why the FE needs no change.** The FE arms proxy listeners *per URL*
(`ensure_proxy_for_url` parses the port out of the URL the backend hands
it), so an ephemeral port is proxied exactly like a fixed one. Launcher `-L`
forwards keep working on single-user hosts (the preferred port still wins
there); on a contended host the ephemeral port is reachable only through
the proxy — which is precisely the transport this ADR built.

**Consequence for static forwards.** A launcher-forwarded fixed port and an
ephemeral fallback port cannot both be "the video port" — the ephemeral
case *requires* the proxy path. This strengthens the case for retiring the
aux `-L` sprawl (this ADR's stated goal) rather than weakening it.
