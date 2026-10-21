# sot-comm protocol — v1

Session-to-session messaging for Ship of Tools: the files in the comm folder and the requests on the wire. This page
is the formats; what they guarantee, and the mechanism behind each guarantee, is messaging's contract
(`rust/backend/src/comm/CLAUDE.md`). Every client reads and writes exactly these formats: `sotd comm` (the commands
sessions type, `comm-send.sh` and its siblings, are wrappers over it), the daemon's own mail and wake, and the 0.6.6
shell scripts during an install, so a box on either version shares one comm folder and one hub.

## Layout (under `$SOT_COMM_HOME`, default `~/.sot-comm`)

```
~/.sot-comm/
  bin/                     # installed scripts: the wrappers, the shell library, the hooks
  VERSION                  # the installer's commit stamp
  registry.json            # who is reachable, and liveness (below)
  registry.json.tmp        # a registry write's temp file, renamed over registry.json
  registry.json.new.<pid>.<n>  # the skeleton a first command links into place when there is no registry
  .registry.lock           # the registry-write lock: a file naming its holder (below)
  .registry.lock.reclaim.<id>  # one marker per holder a waiter proved dead
  .registry.lock.tmp.<id>      # a take's temp file, removed by the take
  inbox-lock-manager       # the hub daemon's record of the inbox lock's manager, written at its start
  .inbox-lock-manager.<id>.<pid>.<n>  # that record's temp file
  gh-device-auth.json      # sot-gh-auth.sh's device-flow state (agents')
  inbox/<name>.jsonl       # durable per-recipient inbox (append-only)
  inbox/<name>.lock        # that inbox's lock file
  read/<name>.cursor       # per-recipient read cursor
  self/<host>__<row>.txt   # a session's handle between commands (below)
  state/                   # per-session scratch of the hooks: the end-of-turn check's tick files and
                           # feedback record, rows' turn marks. Bounded per-session litter; nothing sweeps it.
  probe/<handle>/          # a probe row's project root (comm-probe.sh)
```

Every entry above except `bin/` and `VERSION` is 0700 (a folder) or 0600 (a file): `sotd comm` runs under umask 077,
and a join closes what an older release left open (never a folder that is the root, the home or a git checkout). On
Windows the folder inherits the profile's access list.

## registry.json

```json
{
  "protocol_version": 1,
  "agents": {
    "<name>": {
      "host":       "<host>",          // the box's declared host
      "workspace_id": "ws-…",          // the row whose session joined; "" for a session outside any row
      "repo":       "Ship of Tools",   // the project folder's name
      "root":       "/home/u/repo",    // its canonical root
      "expertise":  ["files"],
      "status":     "idle",            // idle | spawning (a spawn's provisional entry, with its "nonce")
      "joined":     "2026-05-29T18:00:00Z",
      "last_seen":  "2026-05-29T18:04:00Z",
      "stop_at":    "…",               // the Stop hook's mark while it runs (the wake holds off)
      "state": "working", "summary": "…", "status_at": "…"   // rows' work-state, with its facts
    }
  }
}
```

The address-book fields are messaging's; `state`, `summary`, `status_at` and the facts they reduce (`floor`,
`question`, `waiting`, `done`, `dialog`, `note`) are rows' work-state (rust/backend/src/rows/CLAUDE.md). A join writes
the whole entry; every other write changes only its own fields.

**Liveness** is one fact, `last_seen`: a handle is live when its `last_seen` is a `YYYY-MM-DDTHH:MM:SSZ` stamp under
600 seconds old. The session stamps it at each command (join, send, poll, its work-state events), and the daemon that
runs a Starting or Ready row stamps that row's handle every minute, so an idle row stays live while it runs and for
ten minutes after. An age is read against the reader's clock.

**One writer path, one read.** Every write is one read-decide-write under the registry lock on a fresh read, written
to `registry.json.tmp`, synced, and renamed over `registry.json`; a failed step writes nothing and says `FAILED: the
registry …, so nothing was written`. A registry is created only where there is no file, by a hard link of a synced
skeleton, so nothing ever replaces, truncates or repairs one. A read that fails (an NFS stale handle after another
host's rename, or zero bytes) opens the folder and reads again, up to 3 times in about 200 ms; no good read by then is
unreadable, and unreadable is never absent. A registry that stays unreadable is moved aside by hand; the next command
creates an empty one, and each session's next join takes its handle again.

## The registry lock

The lock is the file `.registry.lock`, whose one line names its holder: `name:machine:boot:pidns:pid:start` (the
declared host, Linux `/etc/machine-id`, the kernel's `boot_id`, the pid namespace, the pid, and its start tick; on
macOS the host UUID, the boot session UUID, `darwin`, the pid and its start in seconds). A field that cannot be read is
`-`, which never equals anything; where the comm folder's mount proves no fresh read (anything but a local file system,
tmpfs, or NFS without `nocto`; on macOS anything but the Mac's own disk) the four proof fields are `-`. In file names
`:` is written `.`.

- **Take**: the record goes into a temp file, which is hard-linked to the lock; a link never replaces anything, and the
  lock never exists without its holder. **Release** removes the file.
- **Proof of death**, only from the holder's own machine: the same boot and pid namespace with the pid gone or its start
  changed, or the same machine and host with a different boot. Nothing else, and no timeout, proves a death: a frozen
  holder, one on another machine, or one with no proof fields is never forced.
- **Reclaim**, on the first failed take: a waiter that proves the holder D dead takes the marker
  `.registry.lock.reclaim.<D>` the same way, settles 1 s, reads the lock again fresh, and removes it only if it still
  names D. Only the marker's creator acts; if it died too, the next waiter proves that and takes its marker. A marker
  naming a record the walk already passed ends the walk, and the lock is removed by hand.
- **Bounds**: `SOT_LOCK_WAIT_SECS`, 10 s, for a join, a leave, a spawn and a work-state declaration; 1 s for the
  best-effort `last_seen` touch of a send or a poll and for the Stop hook's mark; 0 for the heartbeat's one try.
- **FAILED** names the holder and the recovery: `registry lock <path> is held by <host> pid <pid> start <tick> (<age>
  old): <why>. If it is dead, run any comm command on <host>.`, with the by-hand clause where no box can prove it dead.

## The inbox

One line per message, JSON, LF-terminated:

```json
{"from": "<name>", "to": "<name>", "repo": "<the sender's project>", "msg": "<text>", "ts": "2026-05-29T18:04:00Z"}
```

`to` equal to the handle is directed mail; `""` is a broadcast copy, shown at the next poll and never counted as unread.
A line whose `to` is missing, not a string or another handle's, or whose `from` is this handle, is shown and never
counted. A line a daemon files for another sender carries `"repo": "daemon"`. Only newline-terminated lines are ever
counted or read.

**The append** takes the kernel's file lock on `inbox/<name>.lock` (opened read-write), opens the inbox under it, cuts
an unterminated tail back to the last newline (a dead writer's partial; logged), writes the line whole, syncs it, and on
any failure cuts the file back to its length before the line. The wait is bounded at `SOT_INBOX_LOCK_WAIT_SECS`, 2 s:
under an `nfs4` manager a non-blocking try every 15 to 25 ms, under any other a blocking wait; past the bound nothing is
appended and the answer is `the inbox lock for @<name> was held for 2s — nothing was appended`.

**The lock manager.** A lock excludes only writers that share one lock manager, so the folder's hub records its own
in `inbox-lock-manager` at its start: line 1 `nfs4 <server>:<export>` (an NFS v4 mount with `local_lock=none`), `local
<machine-id>` (a local file system), or `none@<machine-id>` (any other mount, and every mount on macOS and Windows:
that one machine's own lock); line 2 the writing machine's id. A process appends itself only when the manager it
computes for `inbox/` equals line 1 (never on bare `none`, a machine with no id); everything else is handed to the
daemon that owns the folder as `comm.file`. The hub is the daemon with no topology, the topology's hub, or one whose
comm folder is on its own disk; it creates the record by an exclusive create, keeps a record line 1 already names it
by, replaces only one its own machine wrote, and refuses every filing while a record names another machine's manager,
naming the recovery. A guest daemon never writes the record and forwards what it cannot file to the hub.

**The read cursor**, `read/<name>.cursor`, is `<count> <crc>-<len>`: how many inbox lines its reader has been shown,
and the POSIX `cksum` of the last one without its newline and NUL bytes. A cursor whose last line no longer hashes so
(a failed append cut it back after a reader counted it) steps back one line. A count past the end is 0, but a hashed
count one past it is the end. A count without a hash, and the old timestamp form (the lines before the first one
stamped later), are read as before. Only `sotd comm poll` writes a cursor, after it has shown the batch, from the bytes
it read; a reader on a box that appends itself counts and reads under a shared lock, bounded at
`SOT_INBOX_READ_WAIT_SECS`, 3 s (a held lock is "try again", exit 75).

## The self file

`self/<host>__<row id>.txt` for a row (the daemon pins it as `SOT_COMM_SELF_FILE`; `<row id>` with its bytes outside
`A-Za-z0-9._-` made `_`), `self/<host>__nopane.txt` for every session outside a row on the box (shared,
last-writer-wins), or any private file a launcher pins. Three lines:

```
<handle>
repo=<the project folder's name>
root=<the canonical project root>
```

It names a handle only for a process in that root; a file without a `root=` line names nobody. A join refuses to put an
identity for one project into a row's slot that names another's, unless asked to (`--repin`).

## Handles

A handle is 1 to 64 bytes of `A-Za-z0-9._-`, with no `..` and no leading `-`. Derived, it is
`<folder>-<host>`: the project folder's name and the box's raw host name, each with every other byte made `-`, runs of
`-` collapsed and clamped (the folder to 20, the host to 12, with `-<the first six hex digits of the host's SHA-256>`
when the clamp changed it). Only when another project's root holds that name is it qualified, by the parent folder's
name (`<folder>-<parent>-<host>`) and then by the root's hash (`<folder>-<hash6>-<host>`). Names come from the folder,
never the task: a worktree's handle is derived from its folder like any other (the `/worktree` skill names the folder
`<repo>-wt-<short>`).

## The wire

Every request opens with the `hello` every client sends, and a refused hello is the answer.

- **`comm.file`** `{from, to, text, broadcast, forwarded}`: file one line. The daemon appends it under the rules above,
  forwards it to the hub (a guest), or, as the hub, offers a handle its folder does not list to every daemon linked to
  it (a `link`-role connection), each given the lock wait plus 1 s; the first `ok` wins, else the listing box's own
  refusal, else a box that did not answer, else `no box knows that handle: <h>` (naming the boxes that should hold a
  link and hold none). The answer is `{ok: true}` or `{error, code}` (`not_here`, `no_live_session`, `bad_handle`,
  `file_failed`). A daemon older than the op answers `unknown op: comm.file`.
- **`agent.join`** `{workspace_id, handle}`: a row's session declares its handle (the row's from then on); with an empty
  `workspace_id` it is a question from a session outside any row, answered `ok` or `held_by_row`.
- **`agent.send`**: the window's UI notifications, never mail; one that names a handle is refused
  (`mail_requires_comm_file`).

**The one result** of a send is `filed -> @h` (exit 0; indented, `  filed -> @h`, when this box's folder took it) or
`FAILED -> @h: <reason>` (exit 1), the reason the daemon's own words where it gave any. The answer decides, never the
transport's exit status; no answer within the lock wait plus 3 s (5 s) is `FAILED`. Nothing is queued, and no second
route is tried.

## Waking

The daemon running a row types one fixed line, `[sot-comm] you have mail: run comm-poll.sh`, and Enter, into a free
prompt of an idle Claude Code row with unread mail, and never anywhere else; the contract says what a free prompt is
and what a wake never touches. A Codex row has no wake (no Codex screen has been read). The Stop hook holds a busy turn
until its mail is read, once per batch, through `sotd comm unread <h>`.

## Upgrading to 0.7.0

Nothing on disk changes: a 0.6.6 script and `sotd comm` read and write the same files, and a hub and a linked box on
the two versions carry each other's mail. The install publishes the shell library first and the wrappers after it; for
the seconds between, an old script that loads the new library, or the reverse, fails with `FAILED` or an error, never
`filed`. The scripts 0.7.0 no longer ships (`comm-self-audit.sh`, `comm-registry-lock-clear.sh`) are removed by name from
the list the last install recorded; `comm-relay.sh bridge`, which pre-0.6.6 loops re-run, stays a sleeping stub.

## Versioning

`protocol_version` is stamped into a new registry and checked at a join: a mismatch warns loudly. It rises with any
breaking change to the schema, the line, the cursor or delivery, and with it the contract's install step.

## Conventions carried over from agent-comm

- **Domain-expertise rule:** state your goal or the problem; suggest a fix if you have one, but don't dictate the
  other session's implementation.
- **Anti-groupthink tags:** `[design]` `[question]` `[breaks]` `[challenge]` `[consensus]`. No `[consensus]` without a
  prior `[breaks]` in the thread.
