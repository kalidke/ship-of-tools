# ADR 0041: FE-local capsules on Windows (Ship's Log P3)

**Status:** Accepted (2026-08-25). The contract for running voyage-recording
capsules on the Windows frontend machines, so the frontend's Terminal-drawer
session survives frontend restarts. Implements the ADR 0037 ladder's P3.
**Date:** 2026-08-25

> How this was designed: a three-track research spike (Windows store
> primitives with SQLite/LMDB/RocksDB/PostgreSQL evidence; Job Objects and
> the successor problem; the frontend's actual process tree and relaunch
> mechanics) followed by six adversarial review rounds with mandatory
> deletion pressure. Roughly thirty-five findings were absorbed — several
> from the reviewer reading crate and OS-API sources directly — and a long
> list of machinery was deleted on the way: named jobs and their locator
> feature, ReFS, the explicit detach op, capsule-side scrollback,
> synthesized closes for raw terminals, a sprawling exit matrix, and the
> `resume_command` mechanism itself. What remains is the smallest design
> that satisfies the ADR 0037/0039 invariants on Windows.

## Scope, in one paragraph

P3 makes the Windows frontend's drawer session a voyage-recorded,
capsule-held process that survives FE restarts, and thereby retires the
RESUME RITUAL — ADR 0017 §4's `resume_command`, `claude --continue`, and
the session-start re-arm dance. On an FE relaunch the FE attaches and runs
NOTHING; an optional `initial_command` (which replaces the
`resume_command` setting) fires only when a genuinely new capsule spawns
the shell, independent of `--relaunched`. Explicitly NOT in P3: the
relaunch mechanism itself (ADR 0017 §1–§3 stays); the fe-inbox
down-window (the FE process remains the inbox writer — relay traffic
while no FE is attached is still dropped; stated, not fixed — BUT its
DETECTION is preserved: the retired ritual's session-start catch-up was
the only mechanism that ever noticed a miss, so on every attach the FE
writes an `fe_down {from, to}` marker line into fe-inbox.jsonl; the
surviving drawer session's inbox Monitor wakes on it and can catch up.
The feature must not make the failure quieter); the Claude SDK
adapter on Windows (the drawer is recorded as a RAW TERMINAL voyage);
named jobs / a `winjob-fence-v1` feature / probe-successors; ReFS (local
NTFS only until ReFS passes the same suite); any change to the merged
Linux capsule path (the new socket module is platform-neutral code, wired
only on Windows in P3); Sessions-mode rows for FE voyages; the catalog;
remote attach (P4).

## The store port (fsutil's Windows arm)

- **Volume preflight** (NTFS, local, via `GetVolumeInformationByHandleW`
  on the parent directory) runs BEFORE any `.creating` mutation in
  bootstrap, and again on the resolved voyage dir at `open_for_writing`.
  SMB failing the call is usefully fail-closed; no fallback. This is the
  Windows mirror of "requires renameat2".
- **Publication order, pinned at every site** (segment seal, recovery,
  blob, bootstrap dir rename): source-file flush → no-replace rename
  (`MoveFileExW` flags 0 — one kernel op, no TOCTOU; bounded retry on
  `ERROR_SHARING_VIOLATION` and spurious `ACCESS_DENIED` from AV/indexer
  holders, a transient with no Linux analog; a persistent holder still
  fails at the deadline) → renamed-file flush (belt-and-braces: covers
  the doc-implied corner of the directory-flush contract) →
  parent-directory flush (`FlushFileBuffers` on a
  `GENERIC_WRITE + FILE_FLAG_BACKUP_SEMANTICS` directory handle). NTFS
  metadata journaling gives crash CONSISTENCY, not durability-at-return —
  the directory flush is required, and is strictly stronger than what
  SQLite, RocksDB, and PostgreSQL ship on Windows.
- **writer.lock**: open-existing only — never silently recreate a missing
  persistent fence; opened WITHOUT `FILE_SHARE_DELETE` (std shares
  delete/rename by default) so the locked path cannot be replaced to mint
  a second fence. The lock itself is `File::try_lock` (Rust ≥ 1.89 —
  `LockFileEx(EXCLUSIVE|FAIL_IMMEDIATELY)` on Windows, `flock` on Unix;
  both platform arms collapse into one std call), keeping the existing
  250 ms bounded retry (OS lock release after a hard kill has a
  documented timing transient).
- **Randomness**: both `/dev/urandom` sites (blob temp nonce, capsule
  idem keys) move to the `getrandom` crate — deletes a unix-ism.
- **Format invariance**: the byte-based tear classifier, goldens, and
  torn-tail semantics are platform-independent; CI proves goldens
  byte-identical across OSes.
- **Operational note**: the per-disk "turn off write-cache buffer
  flushing" checkbox makes `FlushFileBuffers` silently vacuous —
  undetectable from the application, the peer of Linux `barrier=off`,
  acceptable only on a UPS.

## Containment and the owned ConPTY layer

The capsule owns its small Windows spawn layer via `windows-sys` (the
same raw-API altitude the Linux side uses for `pre_exec`):
`CreatePseudoConsole` → a TWO-attribute `STARTUPINFOEXW`
(`PSEUDOCONSOLE` + `JOB_LIST`) → `CreateProcessW`. `portable-pty` is
deliberately not used for the spawn — its ConPTY path installs exactly
one attribute and exposes no job hook, so atomic containment is
impossible through it. The job is ANONYMOUS with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and no breakaway flags: capsule
death — however it dies — makes the kernel terminate the whole in-job
tree (the lease and the kill are one mechanism on Windows). "Reaps the
tree" is scoped to in-job descendants; broker-mediated spawning (WMI,
COM activation, schtasks, services) is outside the domain — the verbatim
analog of the Linux external-supervisor carve-out. NO `kill_domain` is
recorded: absence claims no authority (verifier-legal under the
locator-must-declare rule); the anonymous job and any outer-job coupling
are recorded as non-authority spawn-detail diagnostics.

- **Partial-spawn unwind, pinned**: each acquired resource (job, pipe
  server, pseudoconsole, attribute list, process, pump thread) unwinds
  in reverse on failure. A committed `producer_spawn` followed by
  `CreateProcessW` failure records `producer_dead {spawn_failed}` and
  seals — the voyage stays verify-green.
- **Termination sequence, one implementation**: revoke input →
  `TerminateJobObject` → poll `ActiveProcesses == 0` while draining
  output → call `ClosePseudoConsole` WITH THE READER STILL RUNNING
  (pre-24H2 Windows documents that the close can block forever unless
  output is concurrently drained); the reader drains THROUGH the close
  to EOF and is joined only after EOF — join-before-close is an
  implementation error, pinned here. Then commit final output →
  `producer_dead` → seal. "Drain" never means waiting for an interactive
  shell to exit on its own.
- **Breakaway-denied** (a jobbed launcher, e.g. an SSH session): startup
  proceeds with survival marked DEGRADED — recorded in spawn detail and
  surfaced to the FE ("session will not survive launcher death") — and
  the lifecycle rows conditional on it. No broker workaround in P3.

## Terminal state: the capsule is the rendering authority

- Attach delivers an EXACT STRUCTURED CHECKPOINT of the parser state.
  The vt100 fork this project owns gains checkpoint/restore covering
  both grids, alternate-screen identity, current and saved cursor, and
  origin/wrap/input modes — NO scrollback. Checkpoint completeness is a
  TESTED property (alternate-screen and saved-cursor roundtrip tests).
  `contents_formatted`-style reconstruction is explicitly not used — it
  does not encode the inactive grid or alternate-screen identity.
- The capsule keeps NO scrollback: scrollback is FE-side derived state
  accumulated from the live stream after attach; pre-attach history
  waits for frame replay (P4) — the voyage already records every byte.
  This deletion is what makes the resource budget provable.
- The ConPTY DSR responder runs from producer spawn with zero clients
  attached, with parser CARRY state across chunk boundaries (the
  frontend's current "queries don't straddle chunks" shortcut is not a
  protocol guarantee).
- Resize and DSR use request → act → outcome (the existing
  `control_exchange` phases): the request commits before the action, the
  outcome (success/failure) commits after, persisted geometry updates
  only on success, and the DSR reply frame `responds_to` its request —
  replay can distinguish "requested" from "performed".
- **The resource budget** (one table; arithmetic closes; a serializer
  test at max dimensions proves the snapshot bound):

  | resource            | bound                                        |
  |---------------------|----------------------------------------------|
  | max cols × rows     | 512 × 256                                    |
  | cell size (fork)    | 32 B → one grid's CELLS ≤ 4 MiB (see note)   |
  | checkpoint bound    | ≤ 2 grids + fixed header < 12 MiB, proven    |
  | snapshot transport  | chunked in ≤ 1 MiB frames (per-op cap holds) |
  | per-op message cap  | 1 MiB                                        |
  | producer channel    | 8 MiB bounded — when full the writer loop    |
  |                     | stops POLLING output (backpressure lands in  |
  |                     | ConPTY); control/liveness always serviced    |
  | per-watcher queue   | 4 MiB, overflow = eviction                   |
  | driver queue        | 4 MiB; committed driver-visible bytes are    |
  |                     | never dropped while the connection is live,  |
  |                     | but transport liveness is bounded (missed    |
  |                     | keepalive → disconnect; the bytes remain in  |
  |                     | the voyage) — a hung driver cannot wedge the |
  |                     | writer loop                                  |

### Step 3 as built (2026-08-26)

Three clarifications the implementation forced, recorded so the table above
is not read as more than it claims (a fourth decision, the geometry MINIMUM,
follows them):

- **"one grid ≤ 4 MiB" bounds the CELL PAYLOAD, not the allocation.**
  512 × 256 × 32 B is exactly 4 MiB of cells; the `Row` and `Vec` headers and
  the allocator's own overhead sit on top of that. The checkpoint bound below
  it is a bound on ENCODED LENGTH, which is the number that matters for the
  transport, and is unrelated to transient `Vec` capacity while encoding.

- **"wrap modes" means the per-row wrap flags and the pending-wrap cursor.**
  It cannot mean DEC autowrap (`?7`): the fork neither implements nor stores
  it, and a codec cannot serialize state the parser does not have. What the
  checkpoint carries is each row's `wrapped` flag plus the cursor's
  one-past-the-last-column position, which is the state a resumed session
  needs to decide whether the next glyph wraps or overwrites.

- **The checkpoint's own version is a local decoder defense, not a second
  negotiation axis.** Step 3 ships the version field, its rejection path, and
  a pinned byte-level golden for version 1. Binding a checkpoint version to
  an attach-lane `proto_version` — so an incompatible pair is refused during
  hello, before several MiB are generated and transferred, with the capsule
  left running and offered EndRun over the pinned v0 management lane — is
  step 5's, alongside that lane. Falling back to `contents_formatted`, a
  blank screen, or a silent replacement is excluded at every version.

The proven encoded bound is **8,651,327 bytes**, computed from the format and
asserted at compile time. A typical 200×50 screen encodes to about 22 KB,
because an empty cell with default attributes costs one byte.

**The budget table gains a floor: 2 × 2 (decided 2026-08-26).** The table
above caps geometry because a checkpoint has to fit a message. The opposite
end needed a rule for a different reason: below two rows or two columns the
parser had inputs with no correct answer and nowhere to put one — a
width-two glyph with no cell for its continuation half, a wrap with nothing
to scroll into — and both PANICKED. Two pre-existing crashes, inherited from
upstream, reachable from ordinary traffic (`Parser::new(1, 2, 0)
.process(b"abc")` was one repro in full), and neither fixable by patching the
underflow alone: the glyph still had nowhere to go one unwrap later.

So the geometry is refused rather than rendered. Constructors and `set_size`
raise anything smaller; a checkpoint announcing a smaller screen is REFUSED,
not clamped, because clamping would return a screen the payload does not
describe. Note the two bounds are different KINDS of rule and the asymmetry
is deliberate: the maximum is a format budget the parser does not enforce (a
300-row screen parses fine and simply cannot be checkpointed), while the
minimum is a parser invariant that the format then also enforces. Nothing
outside the fork lost a capability — both frontend paths that size a terminal
already guarded at 2 — and the two former panics are now pinned as tests.

**A minimum alone turned out not to be sufficient, which is worth recording.**
Fuzzing the checkpoint round trip across small geometries immediately produced
a THIRD panic at 2 x 2, and it was not a geometry bug: `unicode-width` reports
a few characters as three cells wide since 0.2, upstream's parser was written
against a table where "width() can only return 0, 1, or 2", and `cols - width`
underflows whenever a glyph is wider than the terminal. No geometry floor fixes
that in general — the floor would have to track a Unicode table, which is
exactly the dependency this project already refuses to take at restore time.
The fix is at the other end: a glyph's width is clamped to what the data
structure can hold (a lead plus one continuation), and `MIN_COLS >=
MAX_GLYPH_WIDTH` is asserted at compile time so the two rules cannot drift
apart. The lesson for steps 4-5: the inherited parser's stated invariants were
written against older dependencies, and the corpus is what finds where they
stopped being true.

### Step 4 as specified (2026-08-27, pre-implementation review)

The build-order line for step 4 compresses six mechanisms; a pre-code
adversarial review of the work split resolved the ambiguities it hid. The
rulings, so implementation cannot re-litigate them:

- **The host-side terminal-query model.** Child-emitted DSR/DA never
  reaches the hosting application — conhost's own output-side dispatcher
  answers those into the console input stream itself. What the capsule
  must answer is ConPTY's HOST-FACING handshake on `hOutput`: DA1
  (`ESC[c`), which current conhost emits at startup, and CPR (`ESC[6n`)
  only under `PSEUDOCONSOLE_INHERIT_CURSOR`. Step 4 creates the
  pseudoconsole with FLAGS 0 (no cursor inheritance — the supervisor
  context has no parent console worth inheriting from), answers DA1 with
  a fixed conservative VT identity (`ESC [ ? 1 ; 0 c`, subject only to
  the unit's ConPTY contract test), and keeps CPR support present but
  exercised only where inherit-cursor is enabled. There is NO scanning of
  producer output for child queries and no separate reusable DSR module —
  one consumer, one private carry-state machine, byte-ordered so a CPR
  sample reflects the cursor AT the query boundary, not after the chunk.
- **Resize rejects, never clamps.** An out-of-budget request (beyond
  512x256, below the vt100 fork's 2x2 floor) commits a FAILURE outcome;
  silently clamping would report a geometry nobody asked for. The
  exchange is an ordered-writer-loop command (request committed → one
  `ResizePseudoConsole` call → parser and persisted geometry updated only
  on success → outcome committed); step 5's attach lane becomes its first
  external caller and routes into the same command. Initial geometry is
  validated by the same rule. Frame legality follows ADR 0039's matrix
  exactly: query exchanges are request (`to`) / response (exactly one
  `responds_to`) / outcome (`scope` + `target`); resize is request +
  outcome only.
- **"Revoke input" means closing controller/driver ADMISSION** — the
  writer loop stops accepting new input and resize commands — never
  closing the ConPTY input handle: the host-facing handshake replies must
  stay writable through the drain (an unanswered handshake can deadlock
  the pre-24H2 close this sequence exists to survive).
- **Teardown has ONE orchestrator.** The capsule runtime (writer loop)
  owns the sequence; the ConPTY backend supplies primitives (terminate
  job, poll `ActiveProcesses`, close pty) but never buffers "final
  output" itself — output keeps committing through termination, and
  `producer_dead` + seal happen only after reader EOF.
- **The DEGRADED handoff is split across steps and pinned now.** Step 6's
  supervisor is what ATTEMPTS breakaway for the capsule and detects
  denial; it passes a typed diagnostic into the capsule's spawn config;
  step 4 RECORDS it in spawn detail; step 5 transports it (the mgmt
  `status` survival field above); step 6 renders the warning. A step-4
  capsule can also observe `IsProcessInJob` for diagnostics, but
  observation is not authority (the #119 locator-must-declare rule: no
  `kill_domain` is recorded either way).
- **Cumulative memory, stated honestly.** The budget table bounds parts;
  the whole is: two live grids at max geometry ~8 MiB cell payload + the
  8 MiB bounded output queue + step 5's ≤8.26 MiB transient encoded
  checkpoint ≈ 24.3 MiB lower-bound peak before allocator and transport
  overhead. Acceptable, and now a stated number rather than an implied
  one.
- **The store must be DACL-remediated BEFORE any real Windows voyage
  exists.** Bootstrap currently creates `.creating` without creation-time
  security attributes; the atomic `SE_DACL_PROTECTED` descriptor is
  store-side work (the step-2 column, not the capsule), and "never
  create-then-repair" makes it a prerequisite: step-4 tests use tempdirs,
  but no production voyage may be born un-DACLed. Default voyage-path
  selection (`%LOCALAPPDATA%\sot\voyages`) belongs to the step-6 spawn
  owner, not the store.
- **CI reality.** `windows-latest` now runs a build whose
  `ClosePseudoConsole` returns immediately — it cannot exercise the
  pre-24H2 blocking close this ADR pins the drain ordering against. The
  capsule's spawn/termination/drain tests get a focused `windows-2022`
  leg (an older build with the blocking behavior) alongside the full
  suite on `windows-latest`. Hosted runners may themselves run jobbed:
  tests probe and log `IsProcessInJob` rather than assuming, and nested
  jobs are the supported mechanism either way — an assignment failure is
  a loud spawn failure, never an unfenced fallback.

## The attach protocol

One local socket per voyage (a named pipe on Windows), created
`FIRST_PIPE_INSTANCE`, rejecting remote clients, all handles
non-inheritable. **Security split**: the persistent voyage TREE gets a
DACL for the stable account SID (token user — logon SIDs differ per
logon session and would strand voyages at reboot), installed ATOMICALLY
at `.creating` creation via security attributes (never
create-then-repair), with `SE_DACL_PROTECTED` set and owner ACEs marked
inheritable — a permissive parent directory cannot inject ACEs into the
tree. The ephemeral pipe uses the account SID too. The threat model is
other local users and anonymous access, not the owner.

Windows voyages live in their OWN protected subtree —
`%LOCALAPPDATA%\sot\voyages\<id>\` — so the `SE_DACL_PROTECTED`
descriptor governs exactly the voyage tree and can never end up owning
the staged binaries, logs, or the relaunch sentinel that the launcher
and relaunch script must keep writing under `%LOCALAPPDATA%\sot\`.

There is no `detach` op — ordered pipe EOF is detach, clean or crash.

- **mgmt lane** (PERMANENTLY PINNED v0 framing, never versioned):
  `probe` / `status` / `shutdown`. `status` replies carry the capsule's
  self-reported pid, process creation time, and its SURVIVAL state
  (`normal` | `degraded` — the breakaway-denied startup marker; pinned
  here 2026-08-27, before v0 ships, because a field the permanent lane
  lacks can never be added to it).
- **attach lane** (versioned via hello):
  - `hello {proto_version}` — both directions.
  - `attach {controller_id}` — arrives as a WATCHER, always (a frontend
    relaunch is precisely a reconnect, and reconnects arrive as
    watchers — ADR 0037's who-may-type).
  - `take {controller_id}` — a committed take-epoch transition with an
    acknowledgment barrier; installs an EPHEMERAL DRIVER CAPABILITY
    bound to THIS pipe connection (EOF clears it). The forward check
    requires the capability AND the durable holder/epoch — replaying
    identity fields without a `take` cannot type.
  - `input {controller_id, take_epoch, idem_key, bytes}` — the ack means
    exactly "input recorded"; duplicate `idem_key`s get deterministic
    answers; size-capped; the stale-epoch recheck happens immediately
    before the PTY write, and a stale refusal emits the WAL lattice's
    `{input, refused_stale_epoch}` fact — never a bare input frame
    (bare means must-retry) — with verifier-valid attribution (the
    envelope carries the committed epoch; staleness lives in the fact).
  - `resize {cols, rows}` — driver-only.

ALL ops enter the ONE ordered writer loop. Attach is serviced by that
loop with a WATERMARK BARRIER: force the pending output group-commit,
then establish checkpoint and subscription at that single watermark — a
snapshot can never show bytes the voyage could still lose.

## Lifecycle: one rule, one transition, one probe algorithm

ONE spawn owner — the supervisor (the launcher loop that already
outlives FE respawns and keeps the ssh tunnel). The FE is attach-only.

**The rule: a run is ENDED BY REQUEST only through an explicit
`EndRun` over the mgmt lane.** (Amended 2026-08-27 — the original
wording said a run ends ONLY by EndRun, which contradicted both the
shipped P1 capsule and step 7's own "run ended — new leg" row: a
producer that EXITS ends its run intrinsically — `producer_dead` with
the exit status, seal, verify-green — because the run's program ending
is not a teardown anyone requested. EndRun governs every EXTERNALLY
REQUESTED end; nothing else — no exit code, no FE event, no supervisor
inference — may request one.) Quit intent travels IN-BAND — the FE's real-quit path
issues EndRun itself before exiting, never leaving the supervisor to
infer intent from an exit code it may not even be alive to observe
(review from the target hardware found today's quit path produces only
an IMPLICIT exit 0, and the nightly cleanup script ends the FE with
`Stop-Process -Force`, which runs no exit path at all). Exit codes play
no role in run lifetime: any exit code, an FE crash, supervisor death —
all are FE loss; the capsule is untouched. Exit 75 keeps its ADR 0017
relaunch meaning and lands squarely in FE loss.

**The transition: `EndRun(reason)`** is the only teardown
implementation, invoked by the FE real-quit path, `shutdown-sot.ps1`,
and incompatible-upgrade end-run. EndRun = mgmt `shutdown` → the capsule
acks, drains, seals, exits. Proof of completion is capsule ack + pipe
closure + verify-green + lock release — never `WaitForExit` (an
adopting supervisor has no child handle). A raw-terminal EndRun writes
`producer_dead` + seal only — raw terminals emit no turns, so there are
no synthesized closes.

**The machine-teardown ORDER is pinned** (the nightly close): EndRun →
capsule ack + seal + lock release → supervisor stop → FE close → tunnel
down. `shutdown-sot.ps1` today force-kills the supervisor FIRST — under
P3 that order would strand the capsule headless behind a dead tunnel;
its rewrite is an explicit build-order deliverable. The
daemon-detach-before-tunnel property the current script achieves is
preserved by this order.

**Adoption is ANNOUNCED, never silent**: whenever the probe adopts a
live capsule, the supervisor and the FE surface "adopted a running
session (started <time>)" — a next-morning attach to yesterday's
session must be a visible event, not a silent substitution.

**The probe algorithm** — supervisor start is a pinned state table on
(pipe, writer.lock):

- pipe live + lock held → ADOPT (never touch the lock). Adoption
  captures TERMINATION AUTHORITY with a post-open liveness challenge
  that closes the pid-reuse race: (1) read the server pid P via
  `GetNamedPipeServerProcessId` on the live connection; (2)
  `OpenProcess(P, PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION
  | SYNCHRONIZE)`; (3) AFTER the open, issue a mgmt `status` challenge
  on the same connection — the reply carries the capsule's self-reported
  pid and creation time; (4) the handle is proven to be the server iff
  reply-pid == P AND reply creation time == `GetProcessTimes(handle)`.
  (A dead true server cannot answer the challenge; a live one means P is
  not recycled — pids are unique among live processes.) The (handle,
  creation-time) pair is the pinned identity; SYNCHRONIZE makes the
  post-termination wait executable.
- pipe absent + lock held → bounded retry → visible wedge error. Never
  spawn over it.
- pipe absent + lock free → release the probe lock, spawn, RE-PROBE to
  converge — two racing supervisors resolve with the loser converging on
  the winner, never a false fatal.
- pipe live + lock free, or identity mismatch → inconsistent: loud stop.

## Upgrade and version skew

`probe`/`status`/`shutdown` ride the pinned v0 mgmt framing — the
permanently compatible lane; attach-protocol versions negotiate via
`hello` above it. The staged capsule binary is replaced only after
EndRun completes (stage-after-exit) — a running capsule is never
overwritten in place; an FE upgrade mid-run keeps the old capsule and
offers end-run via the mgmt lane. If a security defect invalidates even
the mgmt lane, the honest fallback is hard termination + voyage
recovery, executable because adoption captured a termination-capable
handle bound to the live pipe server by the liveness challenge
(re-verified unchanged immediately before `TerminateProcess`, then a
BOUNDED `WaitForSingleObject` on the same handle — termination is
asynchronous and can await pending I/O; a timeout is a LOUD failure,
never an assumed death). Graceful shutdown is not promised
unconditionally. The release pipeline packages the capsule binary.

## Build order (each step lands green)

1. Promote the state-dir helper into a shared frontend `paths.rs`.
   This is a latent-bug fix, not cleanup: the three copies DISAGREE
   (one resolves XDG_STATE_HOME first, another uses LOCALAPPDATA only)
   — with XDG_STATE_HOME set, the FE reads state from one directory and
   drops the relaunch sentinel in another.
2. The store port: Windows fsutil + getrandom + volume preflight;
   UNGATE the deterministic suites on Windows (the reconciliation
   matrix, goldens, and verifier tests are compiled out by `cfg(unix)`
   today) + a `TerminateProcess` fault sweep as the kill-sweep analog;
   goldens byte-identical in CI.
3. vt100 fork checkpoint/restore, roundtrip-tested (prerequisite for 5).
4. Capsule ConPTY flavor: owned spawn + unwind ordering + job
   containment + DSR carry + request/outcome resize + the budget table.
5. The pipe protocol (mgmt v0 + attach lane) through the ordered writer
   loop; watermark attach; connection-scoped pen.
6. Supervisor probe/adopt/EndRun (adoption announced) + FE attach-only
   backend with in-band EndRun on real quit + the `fe_down` attach
   marker + `initial_command` + packaging + the `shutdown-sot.ps1`
   REWRITE to the pinned teardown order (EndRun first; orphan stop).
7. Acceptance on a real Windows machine, the full matrix: exit-75
   relaunch reattaches with screen restored and no ritual; FE hard
   crash; supervisor hard death then adoption; capsule hard kill →
   `ActiveProcesses == 0` → recovery → a visible "run ended — new leg"
   UX (never a silent blank swap); graceful quit → verify green; DSR
   answered before any client ever attaches; resize/reattach;
   stale-controller input refused per the lattice; slow-watcher
   eviction; hung-driver transport bounded; ACL denial for a second
   local user; logout/login and reboot ACL access; AV rename-retry;
   disk-full visible; forced-reboot recovery (the voyage survives: open
   tip recovered, all acknowledged input preserved, only a provable
   unpublished tail discarded, a new epoch, verify green); converging
   supervisor race; breakaway-denied degraded path; alternate-screen
   attach fidelity roundtrip; the NIGHTLY COMPOSITE (supervisor AND FE
   force-killed AND tunnel torn, no EndRun — the capsule survives
   headless, and the next supervisor start ADOPTS it with the visible
   announcement, never silently); rewritten `shutdown-sot.ps1` ends the
   run (EndRun → seal → verify green) before any process dies; the
   `fe_down` marker appears on attach after a respawn and wakes the
   drawer session's Monitor.

## Consequences

- The drawer session stops dying with the frontend; the resume ritual —
  the single most fragile piece of the FE session lifecycle — is
  deleted rather than repaired.
- OPERATOR NOTE — an intuition inversion: after P3, QUITTING (real quit
  = EndRun) is what ends the drawer session, while crashes and
  rebuild-relaunches are harmless to it. Today it is exactly backwards
  (quit is safe, crashes lose work). The visible adoption announcement
  and the "run ended — new leg" UX exist to keep this inversion honest
  at 6pm.
- The store becomes genuinely cross-platform with equal guarantees,
  which P4 (bridge) and every later phase inherit for free.
- The Windows kill domain is SIMPLER than Linux's in the common crash
  path: the kernel reaps on capsule death with no successor act.
- The ADR 0037 ladder's platform ordering is corrected: Windows first;
  macOS remains a recorded sketch (process groups, everything
  best-effort) until a macOS machine exists to dogfood it.
