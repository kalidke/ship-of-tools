# rust/backend/src/comm: messaging (charter)

## Idea
A session sends one line to another by handle. The line is delivered when it is appended to the receiver's inbox file,
and the sender is told so, or told why not. An idle row with mail is woken by its own daemon, a busy session is held at
its turn's end until it reads, and a session reads its mail with `comm-poll.sh`. One implementation does all of it, in
Rust, in the `sotd` binary: the daemon runs it in process, and every session runs it as `sotd comm`, on any computer
that shares a home with a daemon host, a server with no daemon of its own included. The shell commands sessions type are
thin wrappers over `sotd comm`.

## Contract (pass 4)

The guarantees of record are the lab's messaging decision 0031; where this page and 0031 disagree, 0031 wins and the
difference is a defect here. The on-disk formats and the wire are `comm/PROTOCOL.md`.
Guarantees, each with the mechanism it rests on.

Address:
- A session that can receive has one handle, and two boxes never share one. A row's handle is rows' `bound_handle`: the
  handle its session declared with `agent.join`. A session's handle is the one its self file names (`self/<host>__<row
  id>.txt` for a row, which the daemon pins as `SOT_COMM_SELF_FILE`; `self/<host>__nopane.txt` outside any row) when
  its `root=` line is this project's root; else it is derived when it joins: the project folder's name and the box's
  name, `<folder>-<host>`, qualified by the parent folder's name and then by a hash of the root only when another project
  holds the shorter one (`identity::derive`). The derivation and the registry write are one step under the registry
  lock, so two joins never claim one handle. `--name` and `SOT_COMM_NAME` name a handle verbatim. This differs from
  0031 ("explicit `--name` handles are gone"): the agents' launchers and probe rows pass them (`ccx`, `comm-probe.sh`,
  `comm-worktree-clean.sh`), so they stay until the agents pass removes those callers.
- A handle belongs to one row at a time and survives restarts, compactions and clears: the row's self file and rows'
  declaration keep it, and a newer join moves it (rows' `set_agent_handle`). A process acts as a row's handle only from
  inside that row: the identity (`identity::context`) discards a handle whose registry entry binds another row, and a
  session outside any row never takes or reads a handle a row holds (the daemon's `agent.join` with an empty row id
  rules on each join from outside any row).
- Each handle has one inbox, `inbox/<h>.jsonl` in the comm folder of the box where its session runs (one folder for
  every computer that shares the home).

Sending:
- A send ends in `filed -> @h` (exit 0) or `FAILED -> @h: <reason>` (exit 1), and nothing else. `FAILED` appended
  nothing; with no answer, nothing is known to have been appended.
- `filed` is said only by the appender, after the line is on disk, for a live handle. The one append is
  `mail::inbox::file_frame`: the kernel's file lock on `inbox/<h>.lock`, an unterminated tail cut back first, the line
  written whole and synced, a failed write cut back. Live means the registry's `last_seen` for the handle is a
  `YYYY-MM-DDTHH:MM:SSZ` stamp under 600 s old, stamped by the session at each command and every minute by the daemon
  running its row. `sotd comm send` appends itself when its lock manager for the inbox folder equals line 1 of
  `inbox-lock-manager`, which only the folder's hub writes; otherwise it asks the daemon that owns the folder
  (`comm.file` to this box's daemon, else the hub), and a handle its folder does not list goes to the hub, which files
  it or offers it to the daemons linked to it (`mail::links`, `mail::hub_link`). The answer decides, never a transport's
  exit status.
- Silence is failure: a send waits 5 s for an answer (`cli::wire::FILE_WINDOW`), and a daemon gives a forward or an
  offer the inbox lock's wait plus 1 s (3 s at the default 2 s), inside the sender's 5 s. A daemon too old for the op answers `unknown op: comm.file`,
  a false failure, never a false success.
- Nothing is queued and no second route is tried. A sender never types into another session.
- A message reaches the send as an argument, from a file (`--file <path>`) or from standard input (`--file -`, or `-`
  as the message), so a sender who chooses a file or stdin never passes it through a shell's quoting.

Reading:
- Only `sotd comm poll` moves a read cursor. The cursor is `read/<h>.cursor`, `<count> <crc>-<len>`: the inbox lines
  shown and the POSIX `cksum` of the last one; a cursor whose last line no longer hashes so steps back one line, and the
  count-only and the old timestamp forms are read as before (`mail::cursor`). Every reader counts newline-terminated
  lines only, so a cut-back tail is never counted.
- A reader on a box that appends itself reads under a shared lock on `inbox/<h>.lock`, bounded at 3 s: a held lock is
  "try again" (poll exits 75 and moves nothing); any other lock fault is named and the read runs unlocked, covered by
  the cursor's hash.
- Unread mail is a line past the cursor that is a JSON object whose `to` is the handle and whose `from` is not; a
  broadcast copy (`to` empty) is shown at the next poll and never counted. One rule counts (`mail::cursor::counts`), for
  the wake, the end-of-turn check and `sotd comm unread`.
- The message is read only through `comm-poll.sh`, never pasted, so it is marked read once and never reads as the
  owner's own words.

Waking:
- An idle row with unread mail is woken within seconds by its own box's daemon, which types one fixed line,
  `[sot-comm] you have mail: run comm-poll.sh`, and Enter, at a free prompt (`wake`): every 2 s it looks, and it types
  one line per new batch and one more if the mail is still unread ten minutes later. The daemon does this, not the
  window, so a closed window leaves no row deaf and several windows do not type twice. A Windows box's daemon holds its
  own link to the hub, so its rows receive with the window closed.
- What a wake types into: only a Ready capsule row running Claude Code whose screen is a free prompt and holds still:
  the cursor at the start of the main input line, the prompt glyph and its no-break space with nothing but blanks
  after it (a dim cell reads blank: Claude Code draws its suggestion dim), that line alone between the input box's two
  rules, the agents panel (if any) on main, the line wide enough for the wake line, and every row through the box's
  lower rule unchanged for 1.5 s; and no `stop_at` mark under a minute old on its entry (its Stop hook is running).
- What a wake never touches: a dialog, a menu, a working session, and a person's unsent text. Text a person typed is not
  dim and is not free wherever the cursor sits, on one row or on several (a draft that wraps or holds a newline puts
  the cursor's row out of the box's one line), so the wake types nothing into it, appends nothing to it and sends no
  Enter for it. Enter goes only after a fresh read shows the wake line alone in main's input box; a box that holds
  anything else gets nothing. A line the wake typed that did not send is sent by a later tick, Enter alone, only once
  main's box holds just that line.
- A busy session needs no typing: its end-of-turn check (the Stop hook, through `sotd comm unread`) does not let a turn
  end while unread mail waits, once per batch.
- A session outside any row, every server session included, has a handle and an inbox and is never woken; it sees mail
  at its next turn, and after ten idle minutes a send to it fails as "no live session". No per-session watcher,
  listener, bridge or Monitor exists.

Isolation:
- Users on one computer never see each other. Everything messaging writes is its user's alone: the comm folder's
  entries are 0700 and 0600 (`sotd comm` runs under umask 077, and a join closes what an older release left open,
  refusing a folder that is the root, the home or a git checkout), and on Windows the profile's access list. Every
  connection `sotd comm` makes reaches only a daemon of its own OS account (`connect_own` for a socket or pipe, ssh for
  the hub) and says hello first; a refused hello is `FAILED` with the daemon's words.

Nested agents:
- A subagent uses no mail: on Claude Code a `PreToolUse` hook on Bash (`comm-subagent-guard.sh`, `sotd comm
  subagent-guard`) denies a command that runs an installed comm script when the hook input carries `agent_id`, which
  only a subagent's does. Codex has no such hook: there the rule is an instruction.
- Another agent started inside a session (`codex exec`, `claude -p`), and anything it starts, has no mail identity:
  every command that reads, sends, joins or stamps first walks the process's ancestors (`identity::layers`) and refuses
  when more than one agent lies between it and its row's capsule, or the top of its tree outside a row, or when the
  ancestry cannot be read in full. On Linux the walk reads `/proc`, on macOS `ps`, on Windows the native process
  snapshot joined to the Git Bash process table its wrapper passes (`SOT_COMM_MSYS`), since an MSYS program that execs
  another leaves no live Windows parent.

The registry:
- Every registry write is one read-decide-write under the registry lock on a fresh read (`registry::lock::acquire`: a
  file naming its holder, made by a hard link; a holder proved dead on its own machine is reclaimed through one marker;
  any other is never forced, and the bounded wait fails naming the holder and the recovery), written to a synced temp
  file and renamed. A read retries an NFS stale handle three times; an unreadable registry is never read as absent, and
  never repaired.

The install (0.7.0):
- The formats do not change: the inbox line, the cursor, the registry and its fields, the registry lock's record, the
  inbox lock and `inbox-lock-manager`, the self file and `comm.file` on the wire are what 0.6.6 reads and writes. So a
  session on the 0.6.6 scripts and one on `sotd comm` share one comm folder and send to each other, and a hub and a
  linked box on the two versions carry mail both ways. While the install publishes the new files (seconds), an old
  script that loads the new library, or the reverse, can fail; it prints `FAILED` or an error, never `filed`.

Owns: the comm folder (`~/.sot-comm`, or `$SOT_COMM_HOME`): `inbox/`, `read/`, `self/`, `registry.json` with its lock
and markers, `inbox-lock-manager`, and in `state/` the end-of-turn check's tick files; the address-book fields of a
registry entry (`host`, `workspace_id`, `repo`, `root`, `expertise`, `status`, `joined`, `last_seen`, `stop_at`); the
ops `comm.file` and `agent.send` (UI notifications, never mail); the wake; `sotd comm` and the wrappers in `comm/mail/`
and `comm/registry/`, the shell door `comm/lib/comm-lib-sotd.sh`, and `comm-subagent-guard.sh`; the messaging skills
in `comm/adapters/`.

Calls in: every session through the wrappers (`comm-send.sh`, `comm-poll.sh`, `comm-context.sh`, `comm-join.sh`,
`comm-leave.sh`, `comm-list.sh`, `comm-relay.sh`, `comm-session-start.sh`), whose names, arguments, output words and
exit codes are kept; rows' work-state scripts and the agents' CLIs through the shell door's functions, whose names and
answers are kept (`sot_require_agent`, `sot_registry_read`, `registry_put`, `registry_del`, `registry_touch`,
`registry_del_if_provisional`, `with_lock` (each write is one locked step), `claim_derived_handle`,
`sot_derive_handle`, `_sot_identity_routable`, `sot_require_routable_identity`, `_sot_self_slot`,
`sot_capsule_workspace_id`, `ensure_home`, `sot_sanitize_component`, `_sot_handle_host`, `sot_handoff_line`); the
Stop hook (`sotd comm unread`, `sotd comm stop-mark`); rows (`edit_entry`, `read_comm_agents`,
`remove_comm_agents_for_workspace`, `stamp_last_seen`, `last_joiner`, `dialog_of`); the server (`handle_comm_file`,
`handle_agent_send`, `Links`, `record_at_boot`, the wake's `run`, the hub link's `run`).

Calls out: rows (`bound_handle` and the headless attach client, for the wake; `agent.join` over the wire; `comm-status.sh
join` after a join, the work-state script's own verb); topology (the relay endpoint, `dial_and_call`, the ssh recipe);
platform (the declared host, the machine id, the kernel locks, `connect_own`); containment (`Signal` for every child).

Does not: decide a row's work-state (rows'); type anything but the wake line and its Enter; carry anything but mail on
`comm.file` (the window's notifications ride `agent.send`); queue or retry a send; read a subagent's mail.

Platforms: one implementation on all three. Linux reads the mount table for the lock manager (`nfs4 <export>` with
`local_lock=none`, `local <machine-id>` on a local disk) and `/proc` for the walk; macOS and Windows are always
`none@<machine id>`, one machine's own lock, and macOS walks with `ps`; Windows' wrappers run under Git Bash and pass the
MSYS table, and paths compare with `/c/...`, `C:/...` and `C:\...` as one spelling and without case. The wake's
no-break-space test is off on Windows until a Windows screen shows it survives ConPTY.

Tests: one unit test per guarantee, `cargo test -p sot-backend comm::` (each named in the pass's result); the
integration suites `comm_wake` (a real daemon and capsule row: a free prompt is woken, a draft, a dialog and a working
row are not), `comm_file`, `comm_send`, `hub_link` and `comm_cli` (the wrappers and `sotd comm` on a scratch home);
`comm/tests/interop-066.sh`, run by hand on a build host, for the 0.6.6 scripts and a 0.6.6 daemon beside this build,
and on two hosts at once for concurrent sends into one network folder.

Unbuilt: a sender's notice when a row keeps refusing the wake (queue item 55); compaction support (item 56, a feature).

## Folders
- `mail/`: delivery: the append and the lock manager (`inbox.rs`), the cursor and the unread rule (`cursor.rs`),
  `comm.file` (`filer.rs`), the hub's linked daemons (`links.rs`) and a box's link to the hub (`hub_link.rs`),
  `agent.send` (`relay.rs`, `bus.rs`).
- `registry/`: the registry's reads and writes (`registry.rs`), its lock (`lock.rs`), and the Windows ancestor walk
  (`ancestors.rs`, also `sotd ancestors`).
- `identity/`: who the calling process is: the context (`context.rs`), the self file, the derivation, the paths in one
  spelling, and the agent-layer walk (`layers.rs`).
- `cli/`: `sotd comm <verb>`: send, poll, join, leave, list, session start and the shell door's verbs.
- `wake/`: the wake tick, the screen reading and the one attach.

## Start here
A send: `cli/send.rs`, then `mail/inbox.rs` `file_frame`. A read: `cli/poll.rs` and `mail/cursor.rs`. Who am I:
`identity/context.rs`. The wake: `wake/mod.rs` `check_row`.
