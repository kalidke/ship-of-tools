---
name: sot-be-session-start
description: Bootstrap or repair a Ship of Tools backend Codex session, including generic Codex comm setup, socket-only relay awareness, frontend reachability ping, and backlog handling. Use for BE session start, backend comm repair, or after restarting a backend Codex pane.
---

# sot-be-session-start

Run `sot-session-start` first. For Codex launched by `ccx`, that setup should
already be done; still run `comm-poll.sh` if there was downtime.

## Frontend Ping

Attached FEs receive daemon relay broadcasts even if no `win-fe-*` handle exists
in this backend host's `~/.sot-comm/registry.json`. Use `@win-fe` as an advisory
broadcast label:

```bash
~/.sot-comm/bin/comm-relay.sh send @win-fe "[$SOT_COMM_NAME] BE receive path armed; any FE please ack."
```

Do not block on `ask` during bootstrap. A later FE reply should arrive as a
directed `[relay] from win-fe-<host>:` line if your Codex wake path is armed.

## Socket-Only Facts

The backend normally listens on the private Unix socket from:

```bash
sotd session-socket-path ${SOT_BACKEND_LABEL:-sot}
```

The remote backend does not need to bind `127.0.0.1:18743`. That port exists only
on a frontend machine when its launcher opens an SSH tunnel to the remote Unix
socket. Browser helper ports `1234`-`1240` must also be forwarded for docs,
Pluto, and static page previews.

## Report State

Best-effort report:

- whether `comm-listen.sh --selftest` passed
- whether FE ack is pending or received
- any messages surfaced by `comm-poll.sh`
- whether you are blocked, waiting, or ready
