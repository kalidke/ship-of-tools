# ADR 0038: sot-tmux keeper — daemon restarts must stop killing sessions

**Status:** Accepted (2026-08-23). This is the first, immediately-shipped piece of
ADR 0037 — pure systemd plus a small guard in the daemon; none of the new
architecture is needed for it.
**Date:** 2026-08-23

## The bug, in plain words

tmux is a client/server program: the sessions live in one background *server*
process, and every `tmux ...` command is a short-lived client talking to it. If no
server is running, **the first client to run starts one** — as its own child.

In our setup that first client is almost always the daemon itself (it creates the
home workspace's session at boot). So the tmux server — carrying *every* session on
the machine — ends up a child of `sotd.service` in systemd's bookkeeping. And
systemd's default cleanup rule for a service is "when it stops, kill everything it
started." Net effect, root-caused in production 2026-08: **restarting the daemon
silently killed every session on the machine.** Sessions just vanished, with nothing
in any log to say why. The fleet had learned to fear daemon restarts without knowing
the reason.

Facts that shaped the fix:

- The daemon already keeps its sessions on a private per-user socket (that part is
  fine). The bug is purely *who is the server's parent*.
- The machines run tmux versions from 3.0a to next-3.7. The convenient new flags
  (`-D`, `-N`) only exist from 3.4, so nothing may depend on them.
- Some hosts are reached only over ssh; user services on those hosts need
  `loginctl enable-linger` or they die with the last login.

## The fix

**Give the tmux server its own tiny service, so it is nobody's accident.**

1. **A keeper unit, `sot-tmux.service`**, whose only job is to start the tmux server
   on the usual socket and own it. Its start command —
   `tmux -S <socket> start-server ";" set -s exit-empty off` — does two things:
   start the server with no sessions, and tell it to keep running when the last
   session closes (normally it would exit). This exact form was tested on both the
   oldest (3.0a) and newest (next-3.7) tmux in the fleet, so there is one unit for
   every host, no version variants. The server reads the user's normal tmux.conf,
   same as before. Stopping this unit is now the *one deliberate way* to kill the
   server — instead of a side effect of restarting something else.

2. **The daemon is ordered after the keeper** (`Wants=`/`After=` in
   `sotd.service`) — a soft preference, not a hard requirement, because the daemon
   must still run on machines with no systemd (macOS) or no keeper installed.

3. **The daemon never starts a server by accident again.** Before any tmux command
   that could start one, it now checks that the server's socket exists; if not, it
   asks systemd to start the keeper and waits briefly. Only if there is no keeper at
   all (not installed, no systemd) does it fall back to the old behavior — with a
   loud warning, once, naming the unit to install. On tmux 3.4+ it additionally
   passes the `-N` flag ("never start a server yourself"), so even a
   perfectly-timed race can only produce an error message, never a captive server.
   The version check for `-N` fails closed, exactly like the existing `-e` check —
   an unknown tmux is assumed old.

4. **Install and release plumbing:** the installer copies the unit and enables it
   *before* the daemon; the release tarball ships it; the auto-updater needs no
   change.

Deliberately *not* changed: systemd's kill behavior on either unit. The bug was the
server being in the wrong service's care, not the cleanup rule — weakening the rule
would just leave orphaned processes.

## Switching a machine over

tmux cannot hand live sessions from one server to another, so each machine needs
**one last scheduled restart** — the final one of its kind:

1. Pick a time; warn the sessions' owner (that's you). Stop the daemon — the old
   captive server dies with it, one last time.
2. Remove any leftover `SOT_TMUX_SOCK` override from the old migration, so
   everything resolves the standard socket.
3. Install both units, reload systemd, enable the keeper, start the daemon.
4. Resume sessions the normal way (`ccb` / `ccbe --continue`, frontend workspace
   creates).

(A zero-downtime alternative exists — moving the live server's process between
systemd's accounting groups by hand — but it is unproven; test it on a scratch unit
before ever trusting it. The scheduled restart is the documented path.)

## How to check it worked

On a host with the keeper installed, with a throwaway session open:

- `systemctl --user restart sotd` → **the session must survive.** This is the whole
  point, and the test.
- `systemd-cgls --user-unit sot-tmux.service` → the tmux server is listed under the
  keeper, not under the daemon.
- Kill the keeper's server, then do anything that touches tmux (open a workspace) →
  the daemon starts the keeper on demand rather than quietly adopting a new server
  of its own.

## Known loose ends (tracked, not blocking)

- One comm script still checks panes against tmux's *default* socket rather than
  ours (a pre-existing gap, unrelated to this change).
- A few skill documents show bare-`tmux` one-liners that assume the default socket.
- macOS gets its keeper when the planned launchd wiring lands; until then macOS
  simply keeps today's behavior (it has no cgroup-kill hazard).
