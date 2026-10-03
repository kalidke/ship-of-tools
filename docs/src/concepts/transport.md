# Transport

How a frontend reaches a daemon that is not on its own box.

## No loopback ports on the control plane

Reaching another box's daemon used to mean an `ssh -L` forward binding a
port on `127.0.0.1`, chosen from a series derived from a hash of the OS
user name. Any account on the box could connect to that port — the
control plane never checked who did. That whole class is gone: the
control plane is now an ssh child's stdin/stdout, or a Unix
socket/Windows pipe in a directory only the owning account can read.
There is nothing left on the control plane for a second account on the
same box to reach.

## The endpoint grammar

Every endpoint this version can dial has one of three schemes:

- `unix:<path>` / `pipe:<name>` — a local socket or named pipe, reached
  directly.
- `ssh:<target>` — that box's own daemon, reached by spawning
  `ssh <target> sotd stdio-bridge` and speaking the protocol over its
  stdin/stdout. `<target>` is a plain host name (`hosts.toml`'s own
  grammar): never a shell command, never anything with a leading `-`.
- `ssh:<target>/<host>` — a daemon `<target>` (always the hub) can reach
  on `<host>`'s behalf, through `sotd stdio-bridge --host <host>`. The
  `--host` argument names which of the hub's own relay sockets to bridge
  to; the box that owns that endpoint derives its path itself — an
  endpoint's shape is never carried across the wire.

`sotd stdio-bridge` takes no caller-supplied label. With no argument it
resolves this box's own daemon, at the label this box resolves for
itself — never one a caller names, which is exactly the mistake that let
a Windows box dial a pipe nothing listens on.

## What `sotd topology plan` gives out

`sotd topology plan --self <host>` answers, one fact per line: `self`,
`hub`, `relay-endpoint` (what this box uses to reach the relay — its own
socket on the hub, `ssh:<hub>` on a box that reaches the hub directly,
or the unchanged reverse-tunnel socket everywhere else), and one `dial`
line per daemon host (its own socket for itself, the hub's own relay
socket for a remote host when this box IS the hub, and `ssh:<hub>` /
`ssh:<hub>/<host>` everywhere else). There is no `tunnel` line and
nothing to forward: dialing a host now means spawning an ssh child, not
opening a port ahead of time.

**A box that has never declared a topology resolves its own daemon.** A
single-box install needs no `hosts.toml` at all: `sotd topology
relay-endpoint` answers this box's own endpoint the moment there is no
file, because with no file nothing has ever told the box of another box,
so its own daemon is the only daemon it could mean. A file that exists
but does not list this box is a different case and stays an error — that
file names a hub, so other boxes provably exist, and answering "myself"
there would be exactly the silent wrong-box failure this design deletes.

**An endpoint you name that this version cannot dial is refused, never
replaced by a local one.** A stale `SOT_RELAY_ENDPOINT` or
`SOT_FE_ENDPOINT` naming a form this version does not speak (a leftover
`tcp:` value, most often) is discarded with a line naming it; clear the
variable rather than expecting it to be silently reinterpreted.

## The second and third connections

A remote host's control connection is not the only one this frontend ever
opens to it: attaching a capsule pane and proxying one browser connection to
a backend-served page each spawn their OWN ssh child, through the exact
recipe the control connection already resolved for that host — never a
second, independent guess at how to reach it. Nothing about the wire
changes: the daemon accepts `lane.connect` and `proxy.connect` on any
connection, so these frames are byte-identical to what they always were.
The cost is one ssh login per attached pane and one per proxied browser
connection — an accepted cost, not a bug, so a page with several
subresources served by the same remote daemon pays once per resource, not
once per page. A port the daemon answers as not served (`bad_port`) is parked: its listener stays bound but closes each browser
connection at once, with no login and no log line, until a page on that port is opened again from the frontend. A
tab left open on a dead page therefore stops costing a login per retry after the daemon's first refusal. The daemon
logs a refused port once, until that port is served again. While the host's link is down, a browser connection
closes at once with no log line; the status line says the link is down.

**The link gate.** While a host's link is down — its control transport's
last attempt got no hello reply, or its session ended — the frontend starts
no ssh login to that host except the transport's own reconnect probe, and no
start site repeats a failed login sooner than a doubling backoff allows. The
gate is one flag per host. Only the transport writes it: up when any hello
reply arrives (a refusal proves the link too), down as soon as its session
ends for any reason but a refusal of the hello itself. Every other ssh start
site asks the gate first, and the one ungated spawn is the transport's probe,
so a new start site cannot skip it. The transport's probe backs off by
doubling from 200 ms, to a cap of 5 s on a local socket and 30 s on an ssh
dial. Once the wait reaches its cap, a down host costs the hub at most two
logins a minute per frontend; the first minute, while the wait doubles, costs
about nine. F5 retries at once. A browser connection to a proxied page is refused while
the gate is down.

**Pause and resume.** An attach worker whose lane dial finds the link down
stops dialing, shows "host offline, waiting for the link", and checks every
100 ms whether the link is up and its pane is the one on screen. The
transport's hello reply reopens the gate, so the viewed pane dials within a
tick of it; parked panes keep their last screen and dial when next viewed.
A resize read while paused is the size of the next attach. The pass bar is
therefore: at the link's return, the transport's one login plus two lane
logins (supervisor and voyage) per viewed row, and no login of any kind while
the link is down.

**Residual.** After a silent network drop the transport can take up to its
keepalive window (about 45 s) to notice, so a lane that dies first may still
dial once. Each such dial is bounded by the connect timeout and its backoff
and reaches no sshd, because the host is unreachable. Closing this would let
lane failures close a gate that only the transport can reopen, so it is
accepted.

A failed login or a dead `sotd` on the far end is the child exiting before
speaking the protocol; its last stderr line is the diagnosis, surfaced in
the pane's own status text or, for a proxied page, a log line — no
per-cause exit codes to learn.

## What this page does not yet cover

Known limits of the link gate:

- Backend ssh callers are outside this frontend invariant and pass an
  always-up gate.
- A gate starts up and stays so until the transport's first attempt.
- A partly queued input cut at the 8 KiB take queue, and a queue cleared
  on a lost pen or when the 30 s checkpoint-in-flight wait runs out, are
  reported by their own status line, not the discard count.

Later work in this same design (per-user isolation for the browser-facing
ports, the pipe/socket owner checks, and the daemon-side account guard)
lands in stages after this one and extends this page when it does.
