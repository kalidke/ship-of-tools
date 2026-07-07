# ADR 0028: Remote comm auto-connect — myhost-anchored reverse SSH tunnels under systemd --user

**Status:** Accepted (implemented + verified end-to-end; codex + win-fe reviewed the plan and converged, 2026-06-27)
**Date:** 2026-06-27

## Context

The sot-comm relay daemon `sotd` runs on **myhost** bound to `127.0.0.1:18743`
(loopback only — see ADR 0027 for the daemon's connection model). The Linux
cohort (myhost + the servers host-b / host-c / host-d) shares one `$HOME`
over NFS, so the *durable* comm layer (the registry, the per-handle inbox
`.jsonl` files, the `comm-*` scripts) is already visible on every box. But the
**live** relay — the instant-wake path that a session's `comm-listen.sh` bridge
and inbox Monitor depend on — needs a TCP connection to `sotd`, and a loopback
bind is unreachable from another machine (verified: `myhost:18743` UNREACHABLE
from host-b).

Result: a `ccb`/`ccbe` session launched on a server could only catch up on its
durable inbox on its next natural turn; it never woke on inbound. In practice the
servers ran **zero** sessions — the registry was 100% myhost — and "the monitors of
host-b are down" was the visible symptom.

The Windows FE already solved the same shape (no local daemon → tunnel `sotd`
from myhost + explicit `SOT_RELAY_ENDPOINT`); the Linux servers had simply never
been wired up.

## Decision

Anchor everything on myhost (the always-on hub where `sotd` lives) and use
**reverse** SSH tunnels, supervised by `systemd --user`.

### Transport — one reverse tunnel per remote
For each remote R, myhost holds:

    ssh -NT -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 \
        -o ServerAliveCountMax=3 -o BatchMode=yes -o StrictHostKeyChecking=accept-new \
        -R 127.0.0.1:18743:127.0.0.1:18743 R

This binds `127.0.0.1:18743` on R and forwards back through the SSH channel to
myhost's loopback `sotd`. **Reverse-from-myhost** (not forward-from-each-remote)
because myhost already holds agent-less outbound SSH keys to every box (verified),
so the credential + lifecycle stay centralized in one template unit and the
servers stay zero-config. The explicit `127.0.0.1` bind is loopback-only on the
remote regardless of its `GatewayPorts` (default `no`).

### Supervision — systemd --user + linger, on myhost
- `~/.config/systemd/user/sot-relay-tunnel@.service` — a **template** unit
  (`%i` = remote host), `Restart=always`, `RestartSec=5`,
  `StartLimitIntervalSec=0` (don't give up after a remote's transient outage),
  enabled for `host-b`, `host-c`, `host-d`.
- `~/.config/systemd/user/sotd.service` — `sotd` itself, `Restart=always`. This
  closes a gap codex + win-fe both flagged: before this, `sotd` was a detached
  `nohup` process (PPID 1) started by `scripts/launch-devenv.sh`, with **no
  supervisor** — a headless myhost reboot would leave the linger-restored tunnels
  forwarding to a *dead* daemon. `scripts/restart-backend.sh` was made
  systemd-aware (diverts to `systemctl --user restart sotd.service` when the unit
  is enabled, else keeps the legacy detached-nohup path) so it no longer races
  `Restart=always`.
- `loginctl enable-linger <user>` — both the daemon and the tunnels are restored
  on boot with no interactive login.

### Endpoint — one shared line
`export SOT_RELAY_ENDPOINT=tcp:127.0.0.1:18743` in `~/.bashrc`, placed **above
the interactive guard** so non-interactive ssh / daemon-spawned bridges see it
too. Because the reverse tunnel normalizes the endpoint to `localhost:18743` on
*every* Linux box (myhost = the real daemon; remotes = the tunnel), one line is
correct everywhere and lets `comm-relay.sh` skip pgrep auto-discovery. This is
the **shared-HOME Linux cluster only**; the Windows FEs have a separate `$HOME`
and keep setting `SOT_RELAY_ENDPOINT` inline in git-bash.

## Consequences

- A `ccb`/`ccbe` session on any Linux server now auto-joins the **live** relay
  with zero per-session config: `/sot-session-start` runs `comm-listen.sh`
  (bridge dials the tunnel) + arms its inbox Monitor, and instant wake works.
- An **idle** remote with no live session holds **zero** `sotd` connections: the
  `ssh -R` tunnel is just an idle SSH process on myhost until a session actually
  dials through it (`sotd` opens a connection on demand). The three always-on
  tunnels cost three myhost-side SSH procs, not three `sotd` conns, and don't
  interact with the ADR 0027 reaper (no idle-disconnect; healthy quiet bridges
  survive).
- Headless-reboot safe: linger restores `sotd.service`, then the three
  `sot-relay-tunnel@*` units, on boot.
- **Verified 2026-06-27:** relay frames sent *from host-b* (both inline-endpoint
  and zero-config-from-profile) reached myhost's `sotd` and woke the myhost session's
  inbox Monitor; the tunnels survived the `sotd` systemd cutover.

### Operational notes
- Add a remote: `systemctl --user enable --now sot-relay-tunnel@<host>` (host must
  be myhost-ssh-reachable with an agent-less key + a `known_hosts` entry, or rely on
  `accept-new` on first contact).
- Check: `systemctl --user status 'sot-relay-tunnel@*' sotd.service`.
- If a remote's `127.0.0.1:18743` is already bound (orphaned tunnel),
  `ExitOnForwardFailure` makes the unit fail+retry until the port frees — visible
  as a restart loop in `systemctl --user status`.
- `scripts/restart-backend.sh` now restarts the systemd `sotd` when the unit is
  enabled; the legacy `--check` staleness report is unchanged.
