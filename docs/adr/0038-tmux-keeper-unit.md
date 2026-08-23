# ADR 0038: sot-tmux keeper — the tmux server leaves sotd's cgroup

**Status:** Accepted (2026-08-23). This is P0 of ADR 0037 (The Ship's Log): an
immediate, tmux-native fire extinguisher, independent of the substrate that
eventually replaces tmux.
**Date:** 2026-08-23

## Context

Nothing in the codebase ever runs `tmux start-server`: the server is created
implicitly by whichever client forks first — in practice `sotd` itself (the
default-workspace create at boot, or any `pty.open`/`workspace.create`). That
makes the tmux server a **child of `sotd.service`**, inside its cgroup, and the
unit's (default) `KillMode=control-group` means **every daemon restart or
upgrade SIGKILLs the server and every session on it** — all `sot-be-*` agent
sessions, plus anything else a sibling toolchain parked on the same private
socket. Root-caused in production 2026-08 (a `systemctl --user restart sotd`
killed the whole session fleet; a mid-upgrade toolchain was collateral). The
failure has zero trace: sessions simply vanish.

Facts that shape the fix:

- The daemon already targets a **private per-user socket**
  (`paths::tmux_socket_path()` → `$XDG_RUNTIME_DIR/sot/tmux.sock`, override
  `SOT_TMUX_SOCK`) on every server-touching invocation. The socket path is
  fine; the *parentage* is the bug.
- Fleet tmux versions span **3.0a to next-3.7**. `-D` (foreground server) and
  `-N` (never implicitly start a server) exist only on 3.4+.
- Observed: a query like `has-session` on a **missing** socket fails safely
  (no server is auto-started); the dangerous invocations are the
  session-creating ones (`new-session`), which do implicitly fork a server.
- `loginctl` linger is not guaranteed on every host; on a host reached only
  via transient ssh, `$XDG_RUNTIME_DIR` and user units exist only while a
  login session does. "Survives indefinitely" requires linger.

## Decision

### 1. A keeper unit owns the server: `deploy/sot-tmux.service`

```ini
[Service]
Type=forking
ExecStartPre=/bin/sh -c 'd=".../sot"; mkdir -p "$d" && chmod 700 "$d"'
ExecStart=/usr/bin/env tmux -S %t/sot/tmux.sock start-server ";" set -s exit-empty off
ExecStop=-/usr/bin/env tmux -S %t/sot/tmux.sock kill-server
Restart=on-failure
```

- **Same socket path** the daemon resolves (`%t` = the user runtime dir), so
  nothing else changes: every existing `-S` invocation, Rust and shell, lands
  on the keeper's server.
- **`start-server ";" set -s exit-empty off`** — validated on both fleet
  extremes (3.0a and next-3.7): the server starts with zero sessions and
  stays alive when the last session closes. No `-D`, so no version split and
  no per-host unit variants. The server daemonizes; `Type=forking` tracks it
  (exactly one process remains in the unit's cgroup, so main-PID guessing is
  deterministic in practice).
- **No `-f` override**: the keeper's server reads the user's normal
  tmux.conf, same as the implicit server it replaces.
- **KillMode stays `control-group` (default) on BOTH units.** The bug was
  cgroup *membership*, not KillMode; weakening KillMode would leave unmanaged
  orphans. Stopping the keeper deliberately kills the server — that is now an
  explicit, single-purpose act instead of a side effect of a daemon restart.
- No `PartOf=`/`Requires=` anywhere: no stop propagation in either direction.

`sotd.service` gains `Wants=sot-tmux.service` + `After=... sot-tmux.service`
(ordering + best-effort pull-in, not a hard dependency — sotd must still run
where the keeper is absent).

### 2. The daemon never implicitly starts a server again

`tmux::ensure_server_present(socket)` runs before every server-touching spawn
(`TmuxClient::run` — the single funnel for all generic ops — and
`pty::spawn_tmux_pair`):

1. Socket exists → proceed (`Present`).
2. Missing → `systemctl --user start sot-tmux.service` (on demand), then poll
   the socket briefly (≤5 s). This also gives boot "readiness retry" beyond
   `After=` ordering, which orders but does not prove readiness.
3. Still missing (unit not installed; no systemd — macOS, `--no-service`) →
   **warn once, loudly**, and fall back to the legacy implicit start rather
   than bricking session creation. On non-systemd hosts the cgroup-kill
   hazard doesn't exist; on systemd hosts the warning names the unit to
   install.

Belt-and-suspenders: when the server is `Present` and tmux is ≥ 3.4
(`tmux_supports_dash_n()`, probed once, fail-closed like the `-e` gate), the
global **`-N`** flag is added, so even the check-to-spawn race cannot resurrect
a captive server — the client errors instead. On 3.0a the primary guard alone
carries the protection.

`tmux -V` (the version probe) touches no server and is untouched.

### 3. Install / release plumbing

`scripts/install.sh` copies the (token-free) unit and runs
`enable --now sot-tmux.service` **before** enabling sotd; the release workflow
stages `sot-tmux.service` next to `sotd.service`. The updater needs no change
(non-archive release files are skipped by asset-name shape). Linger is already
enabled by the installer; hosts configured by hand MUST run
`loginctl enable-linger` or the keeper (and its sessions) dies with the last
login session.

## Cutover (per host, one-time)

tmux cannot transfer live sessions between servers, so the existing captive
server must be released once. **Drain, don't migrate:**

1. Schedule it — this final restart is the last massacre. Stop `sotd`
   (the captive server dies with it).
2. Drop any legacy `SOT_TMUX_SOCK` environment override so the daemon and the
   comm scripts resolve the default runtime-dir socket.
3. Install both units, `daemon-reload`, `enable --now sot-tmux`, then start
   `sotd`.
4. Recreate/resume sessions (`ccb`/`ccbe --continue`, FE workspace creates).

*Experimental alternative (zero-kill), to be validated on a scratch unit
before ever using it:* on cgroup-v2 hosts the live server PID can be migrated
into the keeper's cgroup by writing to its `cgroup.procs` (same-user delegated
subtree), after which sotd restarts no longer reach it. Not the documented
path; the drain is.

**Acceptance test** (per host): with a disposable session live,
`systemctl --user restart sotd` — the session must survive; `systemd-cgls
--user-unit sot-tmux.service` must show the tmux server; killing the keeper's
server and running any workspace op must show the daemon starting the keeper
on demand rather than adopting a new captive server.

## Consequences

- Daemon restarts and upgrades stop killing sessions — for this project's
  `sot-be-*` fleet and for any sibling toolchain on the same private socket.
- One new, tiny, single-purpose unit per host; stopping it is now the one
  deliberate way to kill the server.
- On non-systemd hosts nothing changes except a one-time warning.
- Known follow-ups (out of P0 scope): `comm-bootstrap.sh` still validates
  panes against the default socket (bare `tmux`, a pre-existing gap); a few
  skill docs teach bare-`tmux` one-liners; macOS keeper wiring arrives with
  the launchd work already on the roadmap.
