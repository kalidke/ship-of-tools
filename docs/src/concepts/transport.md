# Transport

How a frontend reaches a daemon that is not on its own box.

## No loopback ports on the control plane

Reaching another box's daemon used to mean an `ssh -L` forward binding a
port on `127.0.0.1`, chosen from a series derived from a hash of the OS
user name. Any account on the box could connect to that port — the
control plane never checked who did. That whole class is gone: the
control plane is now one ssh login's stdin/stdout (the window's link to
that computer), or a Unix socket/Windows pipe in a directory only the
owning account can read.
There is nothing left on the control plane for a second account on the
same box to reach.

## The endpoint grammar

Every endpoint this version can dial has one of three schemes:

- unix:<path> / pipe:<name> — a socket or named pipe reached directly. A generated hub relay socket is a route to a remote daemon: the window classifies its complete derived path once, does not lease that remote daemon, and runs the host's link over one connection to it. Other local endpoints retain their existing policy.
- `ssh:<target>` — that box's own daemon, reached over one login,
  `ssh <target> sotd stdio-bridge`, which carries the window's link to it. `<target>` is a plain host name (`hosts.toml`'s own
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

`--endpoint <unix:path|pipe:path>` is for a script on this same box that
already holds a local endpoint; the ssh forms above never carry one.

The no-argument and `--endpoint` forms read the caller's first line before
anything else: a `link.hello` makes the bridge the far end of a window's link
(below); any other line goes to the daemon unchanged, and the bridge copies
bytes both ways as it always has. `--host` never does: it copies from the
first byte, since each connection to a relay socket is a login of its own,
and the far computer's bridge is the one that routes.

## What `sotd topology plan` gives out

`sotd topology plan --self <host>` answers, one fact per line: `self`,
`hub`, `relay-endpoint` (what this box uses to reach the relay — its own
socket on the hub, `ssh:<hub>` on a box that reaches the hub directly,
or the unchanged reverse-tunnel socket everywhere else), and one `dial`
line per daemon host (its own socket for itself, the hub's own relay
socket for a remote host when this box IS the hub, and `ssh:<hub>` /
`ssh:<hub>/<host>` everywhere else). There is no `tunnel` line and
nothing to forward: dialing a host now means starting its link (one ssh
login), not opening a port ahead of time.

**A box that has never declared a topology resolves its own daemon.** A
single-box install needs no `hosts.toml` at all: `sotd topology
relay-endpoint` answers this box's own endpoint the moment there is no
file, because with no file nothing has ever told the box of another box,
so its own daemon is the only daemon it could mean. A file that exists
but does not list this box is a different case and stays an error — that
file names a hub, so other boxes provably exist, and answering "myself"
there would be exactly the silent wrong-box failure this design deletes.

**An endpoint you name that this version cannot dial is refused, never
replaced by a local one.** A stale `SOT_FE_ENDPOINT` or
`SOT_SPAWN_ENDPOINT` naming a form this version does not speak (a leftover
`tcp:` value, most often) is refused with a line naming it; clear the
variable rather than expecting it to be silently reinterpreted.
`SOT_RELAY_ENDPOINT` is no longer read: the relay endpoint is what `sotd
topology relay-endpoint` answers at each send, so a profile line that still
exports it changes nothing and can be deleted.

## One login per computer: the link

A window makes one login to each remote computer it dials, its link, and
nothing else it does starts one. The control connection is the link's
channel 0; attaching a capsule pane and proxying a browser connection to a
backend-served page each open a channel on the same link. The far
`sotd stdio-bridge` is the link's router: it connects each channel to its
daemon as a connection of its own, so the daemon admits each one exactly as
it admits any connection (this account at accept, then its `hello`), and it
reads no byte of any channel. Each channel's connection starts with a
`hello` whose role is `handoff` (or a control hello for channel 0) and, for a
pane or a page, carries its `lane.connect` or `proxy.connect` in the same
write, so the hello costs no extra round trip. On a relay route the window's
one connection to the hub's relay socket costs the hub one login to the far
computer, the relay service's; that is the window's whole cost there too.

A page served by a remote daemon pays once per browser connection, as one
channel, not once per file: the daemon's page servers keep a connection for
the next request, so a browser's few connections to a page carry all of its
files, and a connection with no whole request head within 30 s of its wait's
start is closed, as is one whose client takes more than 30 s to accept 64 KiB
of a reply. A port the daemon answers as not served (`bad_port`) is parked:
its listener stays bound but closes each browser connection at once, with no
channel and no log line, until a page on that port is opened again from the
frontend. The daemon logs a refused port once, until that port is served
again.

Each channel has its own flow control: each direction may have 256 KiB in
flight, returned as its reader reads, so a pane or page that stops reading
holds its own 256 KiB and nothing else, and the control connection and the
other channels keep moving. What one link carries shares ssh's and TCP's
buffers, so a large transfer on one channel can still delay a keystroke on
another by the bytes already in flight; on a slow network that is up to the
sum of the open channels' 256 KiB.

**While the link is down.** The link is up from its control session's
accepted `hello` until that session ends; a refused hello never puts it up.
While it is down, a browser connection to a proxied page closes at once with
no log line (the status line says the link is down), a pane's attach worker
stops dialing and shows "host offline, waiting for the link", and nothing
starts a login but the transport's own reconnect, which backs off by doubling
from 200 ms, to a cap of 5 s on a local socket and 30 s on an ssh or relay
dial. Once the wait reaches its cap, a down host costs the hub at most two
logins a minute per frontend; the first minute, while the wait doubles, costs
about nine. The wait starts over only after a session that lasted a minute
(`STABLE`), or at F5, so a daemon that answers the hello and then drops costs
no more than a down one; the daemon's hub link follows the same rule. F5
retries at once. When the link comes back, the viewed pane opens its channel
within a tick; parked panes keep their last screen and dial when next viewed.
The link's end ends every channel on it: a pane or page in the middle of its
handshake ends at once, not at its bound.

**When it ends.** Closing the window ends its link: its ssh login ends, the
far bridge reads the end of its input, closes every connection it made for
the link and exits, and the daemon sees each of those connections end as it
would any client's. Either end also ends a link that has carried no byte for
90 s; the control connection's 30 s ping keeps a live one inside that.

**Versions.** The link has its own number, beside the wire's protocol number
and the row's, and a far end that speaks another refuses the link by name. A
0.7 window that reaches a computer whose bridge predates the link sees that
backend refused: "the backend there predates the link (0.7.0); update it",
shown as a refused hello is. An older window still reaches a 0.7 daemon's
bridge, which forwards its plain hello; the daemon then refuses it for its
protocol, as before.

Another account can still fill one of these listeners' queues and slow your
own connections; each connection it opens is refused, or answered with
nothing, quickly. The guarantee is isolation, not availability.

## What this page does not yet cover

Known limits of the link:

- The daemon's own logins (its hub link, `sotd topology sync`, a comm
  forward) are not the window's and are not carried on it.
- A window that dials one computer under two names has two links.
- Bytes handed to ssh but not yet delivered when the link ends are lost with
  it; a keystroke in flight is reported as possibly undelivered.
- A partly queued input cut at the 8 KiB take queue, and a queue cleared
  on a lost pen or when the 30 s checkpoint-in-flight wait runs out, are
  reported by their own status line, not the discard count.

The window and the bridge speak only to an endpoint of their own OS account: on
Unix a socket whose listener ran as that account when it called `listen()`, on
Windows a pipe served by that account's process.

Later work in this same design (the daemon-side account guard) lands in stages after this one and extends this page
when it does.
