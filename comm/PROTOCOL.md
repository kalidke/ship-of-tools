# sot-comm protocol — v1

Session-to-session messaging for Ship of Tools. A fork of the `agent-comm` user skill
with the single-session jail removed and a durable inbox added, so
sessions can address each other across capsule rows and across machines.

This file is the **contract**. Every client — the Claude skill today, a Codex or
Gemini adapter or the in-app Ship of Tools `Tool` plugin later — implements *this*, so
all clients are mutually addressable through the same registry and inboxes.

## Layout (runtime, under `$SOT_COMM_HOME`, default `~/.sot-comm`)

```
~/.sot-comm/
  bin/                     # installed scripts (the reference client)
  VERSION                  # the installer's commit stamp
  registry.json            # who is reachable + liveness  (source of truth for discovery)
  registry.json.tmp        # a registry write's temp file, renamed over registry.json
  registry.json.new.<pid>.<n>  # the skeleton ensure_home links into place when there is no registry
  .registry.lock           # the registry-write lock: a file naming its holder (below)
  .registry.lock.reclaim.<id>  # one marker per dead holder reclaimed; kept forever, but a daemon's own
  .registry.lock.tmp.<id>      # a take's temp file, removed by the take
  inbox-lock-manager       # the hub daemon's record of the inbox lock's mount, written at boot
  .inbox-lock-manager.<id>.<pid>.<n>  # that record's temp file
  gh-device-auth.json      # sot-gh-auth.sh's device-flow state
  inbox/<name>.jsonl       # durable per-recipient inbox (append-only)
  inbox/<name>.lock        # that inbox's lock file
  read/<name>.cursor       # per-recipient read cursor (`<count> <crc>-<len>`: lines shown, and a hash of the last)
  self/<host>__<pane>.txt  # this pane's declared agent name (identity recovery)
  state/                   # per-session scratch; the end-of-turn check keeps mail-<key>.tick,
                           # lock-fault-<handle>.<key>.tick and stop-feedback-<key>.jsonl here.
                           # Nothing removes a mail tick, so every session ever held on mail
                           # leaves one. Bounded per-session litter, and nothing sweeps it.
  probe/<handle>/          # a probe row's project root (comm-probe.sh)
```

Every entry above except `bin/` and `VERSION` (the installer's) is 0700 (a folder) or 0600 (a file), created so by every
writer; `ensure_home` closes an older folder's entries at the next join. Anything else in the folder keeps its mode behind
the 0700 folder. On Windows the folder inherits the profile's access list.

The registry and inboxes are **data at rest** — discovery and catch-up need a
shared place to publish, not a live broker. In an optional shared-home
deployment, one `~/.sot-comm` serves every host sharing that home.

## registry.json

```json
{
  "protocol_version": 1,
  "agents": {
    "<name>": {
      "host":       "myhost",                      // hostname -s; used for same-host delivery
      "workspace_id": "ws-…",                      // the row this session runs in (SOT_WORKSPACE_ID); "" in a bare shell
      "repo":       "Ship of Tools",
      "expertise":  ["files", "rust-backend"],
      "status":     "idle",                        // lifecycle: idle | spawning
      "joined":     "2026-05-29T18:00:00Z",
      "last_seen":  "2026-05-29T18:04:00Z",        // heartbeat, bumped on send/poll/join
      "state":      "working",                     // ADE state-nav WORK state: working | idle | blocked | waiting | done
      "summary":    "rebuilding backend + BE suite",// one-line current / just-finished work ("" when none)
      "status_at":  "2026-06-15T19:32:13Z"         // when state/summary were last written; nav ages a stale "working"
    }
  }
}
```

**Liveness** is heartbeat-based, not pane-based: an agent is *live* if
`now - last_seen <= SOT_COMM_STALE_SECS` (default 600). This is what lets a
session on one machine consider a session on another reachable. `host` +
`workspace_id` are the address a same-host daemon resolves.

**Work-state** (`state` + `summary`, stamped by `status_at`) powers the ADE
*state-nav* at-a-glance view, and is distinct from the lifecycle `status` above.
It is written by `comm-status.sh <state> ["summary"]` (merge-only; self-gating —
a silent no-op in any session that is not a joined comm agent). Two writers by
design: the model pre-announces long work (`comm-status.sh working "<one-liner>"`
when it judges a turn will run >~30s), and a global `Stop` hook floors each
turn-end to `idle`. The daemon joins these fields onto `workspace.list` (as
`agent_state` / `agent_summary` / `agent_status_at`, keyed by the workspace's
`agent_name`) so the frontend — which cannot read this registry directly —
renders `summary` as the per-session glance, colored by `state` and aged off
`status_at`.

**One writer, one reader.** Every script write goes through `registry_replace`
(`comm-lib-registry.sh`), under the registry lock (below). jq writes a tmp, and the tmp is
renamed over `registry.json` only if it is one JSON document with an object
`.agents` and its data has been flushed to the server; otherwise nothing is
written and the writer prints `FAILED: the registry could not be read or
updated, so nothing was written` (or `FAILED: the registry update could not be
flushed (…), so nothing was written`, or `… could not be renamed into place
(…) …`). The heartbeat hook calls the same `registry_replace`, under the same lock
with one try, and stays silent; the daemon flushes its tmp before its rename too. **perl is required for registry writes**: the flush is perl's
`sync`, and a host without perl fails closed with the "could not be flushed"
line. `ensure_home` creates the registry only when there is no file: it flushes a
skeleton tmp and publishes it by a hard link, which fails on an existing name,
so it never replaces, truncates or repairs a registry. Every read goes through
`sot_registry_read` (the hooks source the library in a subshell for it; only the Stop
hook's joined-agent test on a host without jq greps `sot_registry_bytes` instead), which has three
answers: present, absent (it parsed; no such row) and unreadable (missing,
empty, not JSON, not exactly one document, or no object `.agents`).
Unreadable is never absent. Every read, and every writer's read under the
lock, takes its bytes from `sot_registry_bytes` (every daemon read from its
Rust twin, `read_registry_fresh`, but its 1.5 s change poll, where a failed read
skips one tick), because an NFSv4 client can get ESTALE (stale file handle) on a
read after its open succeeded, when another host renames a new registry over
the file. Before the retry, the two-host test measured 16 such reads in about
2,000 on the first host, and 0 in about 9,000 on the peer. So a read that failed
(any error but a missing file, even after some bytes, which are discarded) or
that returned zero bytes revalidates the folder and opens the file by path
again, up to 3 times in about 200 ms; no good read by then is unreadable. A
missing file is told at the open, never by a stat. Zero bytes stays in the rule
because an empty file is never a valid registry, but no zero-byte read has ever
been observed. Non-empty bytes that do not parse are never retried. A send on
an unreadable registry prints
`FAILED -> @<to>: the registry could not be read, so identity @<me> is
unverified; nothing was sent`. leave, list, spawn and despawn print
`FAILED: the registry could not be read; nothing was <removed|listed|spawned|despawned>`,
status and worktree-sync end the same line with `stamp discarded` and `nobody
was reminded`, and a join prints the writer's FAILED line or `the registry
could not be read, so no handle was derived; nothing was written`. Each exits
1. **A registry that stays unreadable is not repaired
automatically**, because rewriting an unreadable file is the wipe. The fix is
by hand: move `registry.json` aside; the next script creates an empty one, and
each session's next send says `reclaim it with comm-join.sh --name <handle>`.

## The registry lock

Every write to `registry.json` (the scripts' `with_lock`, the daemon's
`with_comm_registry_lock`) holds `.registry.lock`, a regular file whose one line
names its holder: `name:machine:boot:pidns:pid:start` — the host name, Linux
`/etc/machine-id`, the kernel's `boot_id`, the pid namespace, the pid, and that
pid's start tick (field 22 of `/proc/<pid>/stat`). A field that cannot be read
is `-`, which never equals anything; off Linux the last four proof fields are
`-`. In file names ':' is written '.', because Windows reads a ':' in a name as
a stream; the record keeps its colons.

- **Take.** Write the record to a temp file and `link(2)` it to the lock (the
  command `link`, never `ln`, which links INTO an older peer's lock directory
  and "succeeds"). The lock never exists without its holder, and a link never
  replaces anything. A failed link after which the caller's fresh temp file is
  the lock was taken (a retransmitted NFSv3 LINK answers "exists" for the
  caller's own link). **Release** removes the file. The comm home must support
  hard links: every filesystem listed under Reclaim does, and so does NTFS; on
  FAT every write fails (the best-effort touches silently), and a ruled one
  FAILs naming "cannot hard-link in <dir>: <errno>".
- **Proof of death** (Linux only, and only from the holder's own machine): the
  same boot and pid namespace, with the pid gone or its start tick changed; or
  the same machine-id AND the same host name with a different boot. This
  assumes a machine-id is unique to one machine: a cloned image that shares one
  would read a live clone as rebooted. No timeout proves anything, so a frozen
  holder, one on another machine, or any unprovable one, is never forced.
- **Reclaim.** It runs on the FIRST failed take, before any sleep. A waiter that
  proves the holder D dead reads the mount, the one on top where mounts are
  stacked: it must be ext2, ext3, ext4, xfs, btrfs, zfs, f2fs or tmpfs, or nfs
  or nfs4 without `nocto`, and any other (overlayfs, say) proves nothing. It
  takes the marker `.registry.lock.reclaim.<D>` by the same link step,
  settles 1 s, reads the lock again fresh (open its folder, then open the
  record), and removes it only if it still names D. Only the marker's creator
  acts; if the creator died too, the next waiter proves that and takes
  `reclaim.<creator>`, which carries the same authority. A lock naming the
  daemon itself, seen by the daemon thread with the turn (below), was left by
  one of its threads, and is reclaimed like a dead holder's, through a marker
  naming the daemon. Markers are kept forever, but for that one: the daemon
  removes it when that reclaim removes the lock or finds it gone after the
  settle, and every record its walk passed is its own. An ID is the process's
  own only with its proof fields: `-` never equals anything, so a daemon or
  script with none never adopts a marker or reclaims a lock as its own. A
  marker naming a record the walk has already passed, other than the waiter's
  own, ends it: no reclaim can pass that marker, and FAILED says to remove the
  lock by hand. A removed lock is retaken at once, as part of the try that
  removed it, even past the deadline below.
- **Bounds** are deadlines, polled every 50 ms (`SOT_LOCK_WAIT_SECS` in the
  scripts): 10 s for join, leave, spawn, despawn, status and the daemon's
  workspace destroy; about 1 s for the best-effort `last_seen` touches of send,
  poll and spawn, and for the daemon's unread clear; 0 for the heartbeat, which
  still makes its one try and reclaims a dead holder. There is always one try,
  and with a clock its reclaim step (in the daemon, once the thread has its
  turn); after the first failed take the deadline is checked after every
  failed take, every sleep and every marker a step walks past its first, so no
  reclaim chains past the deadline and no try starts after it. One daemon
  thread at a time is inside the lock, and its wait for that turn counts
  inside the same deadline. The scripts' clock is bash 5's `EPOCHREALTIME`, or perl's `Time::HiRes` before bash 5
  (the mail tools check it there); both are wall clocks, so a clock jump can
  stretch or shorten one wait. The daemon's is monotonic.
- **FAILED** names the holder and the recovery, in the scripts and in the
  daemon's log alike: `registry lock <path> is held by <host> pid <pid> start
  <tick> (<age> old): <why>. If it is dead, run any comm command on <host>, or
  run comm-registry-lock-clear.sh.` A lock found released since the last read
  says instead `registry lock <path> was held by <host> pid <pid> start <tick>
  when last read (<why>); it may have been released since. Retry.` With no clock the scripts say `registry
  lock <path> is held, and there is no clock to wait by: <which>`; a daemon
  thread that never got its turn says `registry lock <path> was not tried:
  another thread of this daemon held its turn past the bound`. A lock no
  reclaim can clear (a record read whole that does not parse, an older
  version's directory, or a marker naming a record its walk already passed) says `registry lock <path> still
  held (<age> old): <why>. If its holder is dead, remove <path> by hand and
  retry.` A wait that ends with no holder read, the lock released or taken
  again just then or its record not read, says `registry lock <path> was not taken by the deadline:
  <why>. Retry.` `comm-registry-lock-clear.sh` takes the
  reclaim path above with a person's word in place of the holder's liveness
  proof, and nothing else: it never clears a holder this box proves alive, it
  never passes a marker whose creator this box cannot prove dead, and it is
  never a bare `rm`; either would let a pending reclaimer remove the next
  holder's lock. With no lock it says the lock is free and exits 0. When the
  record that blocks it has no proof fields, or no reclaim can clear the lock,
  its refusal says to remove the lock by hand.
- **Older peers.** An older version's lock is a holderless directory: it is
  never reclaimed, and FAILED says "held by an older version that records no
  holder". An older waiter's `mkdir` fails on the file and waits as before.
- **Residual.** A dead holder on a server where nothing comm-related runs again
  wedges registry writes fleet-wide until someone runs the clear command: every
  other box FAILs each ruled lock after 10 s, and every send or poll waits out
  the ~1 s touch bound. The 1 s settle is the only guard against a dead holder's
  late operations, its orphaned release `rm` and its in-flight calls; one that
  lands later than that is not covered. A clear killed during its settle, or
  whose re-read fails, on a box that proves no deaths (macOS, git-bash) leaves
  `reclaim.<D>` naming a record no box can prove dead: every later clear
  refuses, and the lock is removed by hand. A daemon S killed after taking its
  own marker and before removing the lock (the settle and the re-read
  included), or whose re-read fails and that dies before another of its writes
  adopts the marker, leaves `reclaim.<S>` naming S while the lock names S; so
  does one whose removal of that marker fails and that later leaves its lock
  and dies. Every walk then stops at that marker, and the lock is removed by
  hand. A clear on another box, run on a person's false word that a live S is
  dead, can remove S's live lock, so two processes hold it; S's ID is the same
  on every hold, so this was so before any daemon removed its own marker. That
  clear also leaves `reclaim.<S>` naming itself, which no walk on S's box can
  pass: a lock S later leaves is refused until a clear on that clear's box, or
  a person by hand, removes it. A removal of `reclaim.<S>` that failed for a
  live S but reaches the server later (NFS) can remove a later walker's
  `reclaim.<S>` after S has died, so two walkers each act for one dead holder,
  and one can remove the lock the other retook; this is very improbable. A lock
  whose record stays unreadable (a permission or I/O error) is never reclaimed,
  and its FAILED line says only that its record could not be read; it is
  removed by hand.

## Message frame (inbox JSONL, one object per line)

```json
{"from": "<name>", "to": "<name>|\"\"", "repo": "<repo>", "msg": "<text>", "ts": "2026-05-29T18:04:00Z"}
```

ISO-8601 UTC timestamps sort lexically, but they are NOT the read cursor: the
cursor is the NUMBER of inbox lines already shown. Stamps are second-resolution
and every comparison was strictly-greater, so a frame filed in the same second
as one already read was shown to nobody while its sender was told it had
landed. A count cannot lose a frame that way. `comm-poll.sh` is the only writer;
a legacy timestamp cursor is converted on first read (the count of lines at or
before it). The cursor is `<count> <crc>-<len>`: the count, then the `cksum` of
line `<count>` without its newline and NUL bytes — one hash, taken alike of the
file's line and of the reader's copy. If that line no longer hashes so, a
cut-back removed it, and the reader steps back one line and says so. A
count-only cursor still works. The daemon's wake reads every form exactly as
`sot_cursor_offset` does and never writes the cursor: a timestamp-form cursor
means "read up to that time" until the next poll that shows a line. Only
newline-terminated lines are ever counted.

`to` equal to the handle is directed mail and counts as unread; `""` is a
broadcast copy, filed and read at the next poll, never counted as unread. A
line with no `to` key is legacy (pre-stamp, before 2026-06-12) and reads as
directed.

## Delivery

This section is ADR 0049's design of record. The mechanism below lands in
stages; until each stage does, the two-mode delivery this
replaces stays in place. The per-session relay bridge is gone on every box
that shares the hub's comm folder: the hub files for every handle that folder
lists, and a box that holds its own link to the hub (a Windows host) files for its
own folder through its daemon, the frontend playing no part (decision 0031 B2).

**Words.** A *row* is a session running inside Ship of Tools. The *daemon*
is the background program on each box that runs that box's rows; it keeps
running when the window closes. The *hub* is the one daemon every box can
reach.

**Address.** A handle is a session's repository or worktree folder name plus
its box name; two boxes never share one. A row's session declares its
handle to its daemon when it first starts; the daemon keeps it through
restarts, compactions and clears, gives each handle to one row only, and a
newer declaration moves it. A box whose daemon runs no rows holds no
addresses.

**Inbox.** One file per handle, `inbox/<handle>.jsonl`, in the box's comm
folder. The read cursor is a line count and a hash of its last line, kept in
`read/<handle>.cursor`.
Unread mail is any line past it addressed to this handle by someone else.

**Sending** picks one of two routes, by whether the sender's own comm folder
lists the receiver:

1. It can reach the inbox (same box, or a box sharing the home): the line
   goes in under the inbox lock — the kernel's file lock (`flock`) on
   `inbox/<handle>.lock`, taken by the daemon's filer and the scripts alike.
   The inbox is opened inside the lock and closed before it is released.
   "Filed" means kept: the line is flushed to disk before the answer, an
   unterminated last line (a dead writer's partial) is cut back to the last
   newline first, and logged, so the new one stays whole, and a failed append
   is cut back to the length before it (after that cut), taken by a seek to the end of the
   descriptor opened under the lock, never from a path's cached size. A
   lock excludes only writers that share one lock manager, so the folder's
   hub records its own in `inbox-lock-manager`, beside `registry.json`, when
   it starts. Line 1 is the manager: `nfs4 <server>:<export>` (an NFS v4
   mount with `local_lock=none`), `local <machine-id>`, or, on NFSv3 or any
   other mount whose lock is unknown, `none@<machine-id>` — that one
   machine's own lock, so the record binds the folder to the machine that
   wrote it (bare `none`, with no machine id, never matches). A non-Linux
   daemon is `none@<its machine id>` on every mount. Line 2 is the writing
   machine's id (Linux `/etc/machine-id`, macOS its host UUID, Windows
   `MachineGuid`; never a hostname, which two machines can share); a record
   without it has an unknown writer. Every route compares line 1 only. The hub is
   the daemon with no topology, the topology's hub (an unreadable
   `hosts.toml` names no hub, so that daemon is a guest unless its folder is
   on its own disk), or a daemon whose comm
   folder is on its own disk, asked of the folder on each OS (Linux: `local
   <machine-id>`; macOS: its mount is `MNT_LOCAL`; Windows: a fixed drive, not a
   UNC path; anything else, or an error: not own disk); every
   other daemon is a guest on the hub's folder and never writes or deletes
   the record. The hub makes it by an exclusive create when it is absent
   (of two daemons starting at once only one writes it; the other reads the
   winner's), leaves it alone when line 1 already names the hub's own
   manager, and replaces it only when line 2 is the hub's own machine id
   (the hub after a remount). A record another machine wrote names a
   different lock manager: the hub leaves it untouched and logs it as an
   error, and only an explicit reset with no other daemon running replaces
   it. A hub with no machine id (bare `none`) never creates or replaces the
   record and refuses every filing, saying so: give the machine a machine id
   (`/etc/machine-id` on Linux), then restart the daemon. A v3 hub writes `none@<its machine>` and serves other hosts over the
   wire. A script
   adds the line itself only when `flock(1)` and `perl` exist (perl makes
   the append, its fsync and its cut-back on one descriptor), the box is Linux, and
   `findmnt -T` on the inbox names that same manager — under a `none@…`
   record, only a script on the record's own machine. Anything else (another
   machine's `none@…`, a mismatched export, another box mounting the daemon's local disk, no
   record, a bare `none`, no `flock(1)` or `perl`) hands the line to the daemon that owns the comm folder as
   `comm.file`: this box's own daemon, else the relay endpoint (the hub). A
   daemon that does not answer is `FAILED` and no second route is tried; one
   older than the record answers `unknown op: comm.file`. Every daemon
   rechecks its own manager against line 1 at each filing: a match appends
   (never on bare `none`); a guest forwards anything else to the
   hub, once, marked `forwarded`, and answers with the hub's own verdict;
   the hub refuses it as `file_failed` with the recovery named — restart
   the hub when its own machine wrote the record (a remount), else stop
   every daemon on the folder, delete the record and start the hub.
   On Windows, the daemon's first start moves the old frontend inbox's
   unread lines (`fe-inbox.jsonl`) into the inboxes; lines for handles not in
   the registry stay in `fe-inbox.jsonl.moved`.
2. It cannot: `comm-relay.sh send @h` (which `comm-send.sh` execs on a
   registry miss) writes ONE `comm.file` request to the relay endpoint, the
   hub, and reads ONE answer. The hub files for its own home: when its comm
   folder lists the handle and a session holds it, it adds the line under
   the same inbox lock and answers `ok`.

**The wait is chosen by lock kind, and bounded at 10 s (`SOT_INBOX_LOCK_WAIT_SECS`)
on both paths.** The Linux NFSv4 client retries a blocked lock with a backoff
that doubles from 100 ms, so a local writer re-takes the lock before a remote
waiter's next retry and a blocking waiter can sleep past a free lock. Under
`nfs4 …` the daemon's filer and the scripts therefore try the lock without
blocking every 15-25 ms (jittered) until the bound; under `local …` or
`none@…` (NLM on v3, one machine's own kernel lock) a blocked waiter is woken
on release, so they block, bounded. Past the bound nothing is appended and the
answer is `FAILED -> @<h>: the inbox lock for @<h> was held for 10s — nothing
was appended`. The reader's shared lock takes the same choice for its 3 s
bound. `test-inbox-lock-twohost.sh` counts a concurrency case as proof only
when its content checks pass and its two writers overlap; its unpaced liveness
case (shell here, shell on the peer and the daemon's filer, 200 each at once)
expects 0 `FAILED`, since a send that fails under ordinary two-host load is a
working-comms failure, and counts as proof only when its three writers overlap
too. `test-comm-e2e-readers.sh` runs the real readers (comm-poll, the Stop
hook and the daemon's wake) on three hosts against a scratch comm home on
the shared mount; like the two lock tests it needs real boxes, so it runs in no
workflow.

**Readers** have two guards against a line that a failed append then cuts
back. Where a writer would append locally (`flock(1)` and `perl`, Linux,
identity equal to line 1), the count-and-read runs under a shared lock on
`inbox/<handle>.lock`, bounded by `SOT_INBOX_READ_WAIT_SECS` (default 3). Every
inbox lock descriptor, a reader's or a writer's, is opened read-write: the
Linux NFS client refuses a shared lock on one without read access.
`comm-poll.sh` reads its batch once under the lock, lets go, shows the batch,
and then writes the cursor from the bytes of the last line it read, never from
the file again, so a slow display never holds off a writer. A timeout means
try again, never a skip: `comm-poll.sh` says the inbox is being written,
leaves the cursor and exits 75, the end-of-turn hook prints that the inbox was
busy and does not block the turn, and a wake reader checks again on its next
tick. Only a held lock is "try again": any other lock fault, a lock file that
cannot be opened or any flock error, is named (flock's code and its own error
text) where the session sees it (comm-poll's output, every block of the
end-of-turn hook) and the read runs unlocked, covered by the hashed cursor. On
every host the hashed cursor steps back one line when a cut-back removed the
last line read. The accepted residual: a reader on a
mismatched host may deliver a line whose sender was told `FAILED`, so a retry
can duplicate it; it can never lose one.

The daemon's filer checks liveness first: a row still runs a session with
that handle, or the session was active in the last ten minutes. A script's
own append in route 1 does not check it yet.

**The one result**, nothing else:

- `filed -> @h` (exit 0; a script's own append prints it indented).
- `FAILED -> @h: <reason>` (exit 1), the reason being the daemon's own
  sentence whenever there is one: `no box knows that handle: <h>`,
  `no live session holds @h`, `not a handle: …`, a failed append; the inbox
  lock held for 10 seconds (`the inbox lock for @h was held for 10s — nothing
  was appended`); a daemon older than the op (`unknown op: comm.file`); or no
  answer at all (`the daemon did not answer at <endpoint>`, followed by the
  transport's own stderr when it wrote any). A refusal means nothing was
  added; with no answer nothing is known to have been. Retrying or reporting
  is the sender's call.

**The answer decides, whatever the transport's exit status or stderr say**:
the transport's stderr is read only when no answer came, and then only
lengthens the reason. The read window is the lock wait plus 10 seconds, so a
hub that waited out the lock and then filed is not reported `FAILED`.

**Landing in stages: a handle the hub's folder does not list
(`not_here`) falls back to the older route** — `agent.send` offered to
everything attached to the hub, decided on a filer's receipt. There, a
receipt gives `filed -> @h (by <filer>, relay)` (exit 0); an ack with an empty roster
and no receipt within 5 seconds gives `FAILED -> @h: nobody filed it within 5s: no box knows that handle, or the daemon that holds it is stopped or not linked to the hub`; an ack naming
anyone with no receipt within 5 seconds gives `NOT CONFIRMED: sent for @h;
nobody claimed it within 5s. Attached: …`, the roster a diagnostic and never
a verdict — so a handle no box knows gives `NOT CONFIRMED` whenever anything
is attached; any other ack, or none, gives `FAILED -> @h: <reason>` when the
transport failed with one to give, else `FAILED -> @h: the daemon did not
answer at <endpoint>`. Every one of those but the receipt exits 1. A broadcast
(`send --all`) still goes this way and prints `relayed -> <all> (<n>
receiver(s)) via <endpoint>`. With no daemon found at all a wire send prints
`FAILED -> @h: no sotd daemon found; …` and exits 1.

Nothing is queued anywhere, and a failed send is not retried by another
route. To get an answer, send, end the turn, and be woken.

**Waking.** Every two seconds each daemon looks at every row it runs. If the
row's handle has unread mail and the row sits at a free prompt — the cursor
sitting at the start of the input line marked by the prompt glyph (`❯`,
or on Windows `❯` or `>`, the latter being Claude Code's fallback when its
unicode check fails), followed by a no-break space, between the input box's
two rules (on every OS; on Windows a bare glyph too, for now), so a grey
suggestion (drawn dim) or any other dim decoration does not count as a draft, while
a draft is not free wherever its cursor sits, and the screen down to the
input box's lower rule (the background-agent footer below it is not watched)
must then hold still for a second and a half before anything is typed, so a working
session is not typed into while its screen is still changing — the daemon
types one fixed line, `[sot-comm] you have mail: run
comm-poll.sh`, and Enter. The daemon does this, not the frontend or the
sender — several frontends can show one row and each would type, and a
closed window would leave the row deaf. It types a fixed notice, never the
message itself: a pasted message is never marked read, so it would show
again. One line per new batch of mail; one more if it is
still unread ten minutes later at a free prompt. A line it typed that did not
send is sent by a later tick, with Enter alone, once main's input box holds just
that line; within one daemon run it is never typed twice (a restart forgets it). A row whose registry entry
carries a `stop_at` under a minute old is running its Stop hook and is not typed
into: the hook stamps it as it starts, its `stop` deletes it when the turn ends,
and the hook reads the mail itself. A turn ended by Esc or an API error runs no
Stop, so a mark over a minute old holds nothing. A busy session needs no
typing: its end-of-turn check will not let a turn finish while unread mail
waits. That check reads the inbox before anything else, so a turn that closes
with a report marker is held too, and its row is stamped only at the turn end
that passes, from the last marker anywhere in the turn: the check's own
held-turn notices do not start a new turn. This is the only wake — no per-session watcher, listener, bridge or
Monitor exists.

**Cases.**

- A Claude or Codex row gets the same line and check; the free-prompt test
  knows each tool's prompt.
- A row that just restarted, compacted or cleared re-arms nothing: the
  handle stays with the row and the count is a file.
- A session outside any row (a bare terminal, or one on a box with no
  daemon) has a handle and an inbox, and its end-of-turn check reads new
  mail before a turn ends. While idle it sees mail only at its next turn,
  and after ten idle minutes sends to it fail as "no live session".
- A subagent that runs inside its parent's agent process is part of that
  session: it may send under the parent's handle and never reads the inbox.
  Another agent started inside a session (`codex exec`, `claude -p`), and
  anything it starts, has no comm identity: a process acts as a handle only if
  at most one agent (claude or codex) lies between it and its row's capsule, or
  the top of its process tree outside a row. A `node` process is an agent when
  any of its arguments names one or lies in its npm package, and it counts once
  with its native child only when that child is its direct child and its
  arguments after the binary equal the host's after its script (on Windows, as
  the Microsoft C runtime splits each command line, its backslash and `""`
  rules included). Agents are named, not launchers: the list is the agents comm
  ships an adapter for, and it grows in the commit that adds one. The check
  reads each ancestor's full command line, and an ancestry it cannot read in
  full (a walk past 64 processes, an unreadable /proc record, a parse that did
  not finish, a `sotd.exe` that fails or is too old) is refused, not trusted.
  On Windows the walk reads Cygwin's own /proc (the MSYS runtime Git for
  Windows ships) from the script's own process up through its MSYS parents, and
  `sotd.exe ancestors --from <pid>` above the first one whose parent is native.
  An MSYS program that execs another is a new Windows process whose Windows
  parent has exited, so where the native walk stops at an MSYS process, the
  walk goes on from that process's MSYS parent. An ancestor whose parent has
  exited with no MSYS parent to go on from, whose record cannot be opened, or
  whose creation time is after its child's, ends the walk as the top; for a
  process that names no row, only the 64 cap, an unreadable /proc record and
  node.exe's unreadable command line are refused (the row rule below refuses
  that early top for one that names a row). On Windows, and on macOS where
  `ps` prints `(name)` for arguments it cannot read, an unreadable command
  line is recorded as the program name with arguments unknown, which equals
  nothing: a native agent so recorded never counts once with its node host,
  though a standalone one is one layer, and a `node` so recorded is refused.
  On Windows every comm script that reads mail, sends, joins or
  stamps refuses until the installed `sotd.exe` has `ancestors --from` (an
  older one fails, "update sotd", or prints lines that carry no pid, which is
  an unreadable ancestry), so the scripts and `sotd.exe` ship together. A
  refused process may neither spawn nor despawn rows (`comm-probe.sh`'s
  included) nor run `comm-worktree-new.sh`, which refuses it before any git
  write, and no hook of it writes anything under the comm home. An agent
  that needs its own handle is started as its own row.
- A process whose identity names a row acts as that row's handle only if that
  row's capsule is one of its ancestors. The identity names row `<id>` when its
  pinned self file is named `<host>__<id>.txt` and `<id>` is not `nopane`. When
  no self file is pinned, it names the row `SOT_WORKSPACE_ID` gives. A capsule is
  that row's when one of its arguments before `--` (with `\` read as `/`) ends
  in `/workspaces/<id>` or contains `/workspaces/<id>/voyages/`. The process is
  refused, and nothing is read, sent or stamped, when its walk ends in any of
  three ways: at the top, at another row's capsule, or at a capsule whose
  command line cannot be read. So for a process that names a row, two kinds of
  early stop no longer pass: being reparented away from the row, and, on
  Windows, a walk that stops at an ancestor it cannot open (a sandboxed agent's
  child may be unable to open its parent even under the same account) or at a
  reused pid. Only a process that names no row may end its walk at the top,
  which is how a session outside any row passes, including one under another
  account's sshd. A private identity file for a process outside any row must not
  be named `<host>__<id>.txt`.
- What the check does not see: for a process that names no row, an agent above
  a reparenting, or on Windows above an ancestor that has exited with no MSYS
  parent to go on from (a process of another MSYS or Cygwin installation is not
  in this one's /proc), cannot be opened, or was created after its child; an
  agent whose name is not in the list; a macOS agent whose argv[0] contains a
  space; on macOS, an agent whose arguments `ps` cannot read and whose program
  file is not named after it; and a shim that starts `node` under the agent's
  own name (a
  volta-style shim), which may be refused instead. On macOS a capsule binary
  installed under a path that holds a space is not recognised (`ps` splits its
  argv[0]), and a state path holding a standalone `--` word ends the argument
  scan early; either way a process naming its row is refused there. An argument
  whose state root itself lies inside another row's voyages folder (a path
  holding `/workspaces/<id>/voyages/` before its own) matches that row too. On
  macOS the walk reads `ps`, which folds runs of white space, so a native agent
  started directly by its node host with arguments that differ only in white
  space counts as one layer with it; exact argv on macOS comes after 0.6.6. An
  argument that itself contains the record's marker bytes (\x1f, \x1e, \x1c,
  \x1b) reads as a separator, an unknown marker or an encoded newline or
  return. On Windows an unpaired UTF-16 surrogate in a command line reads as
  U+FFFD. On Linux a process whose whole command line is exactly `!end` ends
  the walk as if it were the top.

## Dependencies

The shell client needs `jq` (every path that reads or writes a frame) and, on
Linux, `flock` and `perl` (the inbox lock and the whole-line append). A session
whose tool is missing would never see its mail, so each path that reads mail
checks the tools it runs before first use and says so, naming the tool:
`comm-poll.sh` prints the line on stdout and exits 1; `comm-session-start.sh`
prints it and exits 1; the Stop hook blocks once per episode with it as the
reason, prefixes every later block with it, and clears it on the first turn end
with the tools present.

## Upgrading to 0.6.6

Once 0.6.6 is installed, bridge loops started by earlier versions stop working:
`comm-relay.sh bridge` is retired. A loop tied to a session ends with that
session, and an untied one sleeps idle until the next reboot. No action is
needed. The install also removes the comm scripts a release no longer ships
(`comm-listen.sh`, `bus.sh`, and any later retirement), by exact name, from
the list it recorded in `bin/.sot-comm-installed`. A commit that retires a
bin script first checks every shipped version for a running loop that
re-execs it; if one exists, the name stays shipped as a stub that logs once
and then sleeps, and only otherwise is the file deleted.

## Verbs (reference client = `bin/*.sh`)

| Verb        | Script           | Notes |
|-------------|------------------|-------|
| join        | `comm-join.sh`   | **Superseded by ADR 0049, removed in B6** — the handle is derived (folder plus box name), not chosen by flag. `--name <n>` `--expertise "a, b"`; writes registry + self file. Refuses (exit 3) when the self-file slot is already claimed for a DIFFERENT project — the slot is keyed by the workspace row in the environment while the identity comes from the shell's cwd, and a row that comes to name another project's session reads that session's mail. `--repin` is the deliberate override |
| audit slots | `comm-self-audit.sh` | compares each workspace-keyed slot's key against the `repo=` it carries; reports the ones naming a different project (exit 1), passes a suffixed or path-disambiguated name |
| send        | `comm-send.sh`   | `@name "msg"` or `--broadcast "msg"`; recipient is only the first positional `@arg`, so the message may itself begin with `@`. **Either verb routes**: a directed target this box's registry names is filed by route 1 of Delivery (`comm-relay.sh send` execs here), and one it cannot name goes to the hub as `comm.file` (this execs `comm-relay.sh send`). The triggers are mutually exclusive, so a session never has to know which verb reaches a peer |
| poll        | `comm-poll.sh`   | shows the inbox lines past the read cursor, then advances it; a busy inbox exits 75 (try again) |
| list        | `comm-list.sh`   | all agents + live/stale + (me) marker |
| leave       | `comm-leave.sh`  | removes self from registry; `--name <handle>` removes an orphan row (registry only — `comm-despawn.sh` is full teardown) |

## Naming — everything derives from the repo

One rule, enforced where possible: **names come from the repo, never from the
task**. A task-named anything is unfindable next to its repo-named siblings
(a spawn labeled `edge-classify` hid the MyPackage agent, 2026-06-12).

| Thing | Convention | Example |
|-------|-----------|---------|
| Durable BE peer handle | `<repo-lowercase>-<host>` | `myrepo-myhost` (Ship of Tools on the backend host), `lldevtools-myhost` |
| Spawned agent — repo checkout | `<repo-lowercase>` (bare, **no** descriptor) — **superseded by ADR 0049, removed in B6:** the handle also carries the box name | `myrepo` |
| Spawned agent — git **worktree** | `<repo>-wt-<shortname>` (the `-wt-` infix is reserved for worktrees and groups them next to the parent; `<shortname>` names the WORKTREE, never the task). Created via the `/worktree` skill — **superseded by ADR 0049, removed in B6:** the handle also carries the box name | `MyAnalysis-wt-rotation` (worktree `rotation`) |
| Frontend address | `fe@<host>` — the frontend PROCESS's declared hello `name`, the target `sot-fe --fe <host>` scopes a directed `fe.command`/`open-url` to (two frontends on one box differ by `instance`); the frontend is a client, never a comm peer, and no session derives or joins as this name | `fe@laptop` |
| Workspace label | repo basename (comm-spawn default; task-named labels are **rejected**) | `MyPackage` |
| Workspace slug (the row's name) | derived from the label by the daemon | `mypackage` |
| Second session on one repo | a git worktree (`comm-worktree-new.sh`); `comm-spawn.sh` refuses a root that already has a workspace | `MyPackage-wt-rotation` |

A bare `<repo-lowercase>` handle is the default for a normal repo checkout. For a
**git worktree**, use `<repo>-wt-<shortname>` — the `-wt-` infix is reserved for
worktrees (so they read as worktrees at a glance and the shared `<repo>-` prefix
groups them next to the parent in the sessions list), and `<shortname>` names the
worktree, never the task. Don't hand-roll it: the **`/worktree`** skill
(`comm-worktree-new.sh`) creates the worktree at
`<repo-parent>/worktrees/<repo>-wt-<shortname>` on branch `wt/<shortname>` and
spawns the session with that handle+label, so the slug groups it correctly. (Today the
worktree handle carries no box name and the parent is found by repo family —
**superseded by ADR 0049, removed in B6:** every handle carries the box name.) Never a suffix on a plain repo open, and never a task
name (`repo-fix` for a direct checkout was wrong on both counts: a
task-ish descriptor AND a shortened base; the right handle was
`myrepo`).

Task identity lives in `--task` / `--expertise` / the message body. Never reuse
a handle that has a registry row, even a stale-looking one — the owner may be
alive with a lagging heartbeat, and a collision makes two sessions execute the
same briefs in parallel.

## Conventions carried over from agent-comm (keep these)

- **Domain-expertise rule:** state your goal or the problem; suggest a fix if you
  have one, but don't dictate the other session's implementation.
- **Anti-groupthink tags:** `[design]` `[question]` `[breaks]` `[challenge]`
  `[consensus]`. No `[consensus]` without a prior `[breaks]` in the thread.

## Versioning

`protocol_version` is stamped into the registry on creation and checked on
`join`. A mismatch warns loudly (no quiet degradation) and means a machine needs
`ShipTools.update_comm()`. Bump this integer on any breaking change to the schema,
frame, or delivery semantics.
