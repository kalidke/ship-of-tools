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

### Step 4 as built (2026-08-27, PRs #130 and #132)

Every spec-gate ruling shipped as specified. What the paper design could
not know, learned by running it — recorded because steps 5-7 build on
these facts, not on the citations that predicted otherwise:

- **The two spawn attributes use OPPOSITE `lpValue` conventions.**
  `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` takes the HPCON *value itself*
  (Microsoft's sample passes `hPC`, not `&hPC`); `JOB_LIST` stores a
  *pointer* to a handle array that must live at a move-stable (heap)
  address until the attribute list is destroyed. Passing the wrong shape
  fails `CreateProcessW` with `ERROR_INVALID_HANDLE` — instantly, on
  every image.
- **Std-handle cloning is the trap the walkthrough cannot see.** Windows
  copies the parent's std handles into a console child even with
  `bInheritHandles = FALSE` when `STARTF_USESTDHANDLES` is absent. From
  any redirected context — a test harness, CI, a supervisor — the child
  then writes to the parent's pipe while its console (the pty it DID
  attach to; its title propagates) renders nothing. Microsoft's sample
  only works from a real console, where the cloned handles are console
  handles that remap. The fix is Windows Terminal's own:
  `STARTF_USESTDHANDLES` with all three std handles null. Every capsule
  spawn context is redirected; this is load-bearing, not defensive.
- **The close must never pause the drain.** A teardown that stops
  consuming output to make the (pre-24H2, possibly-blocking)
  `ClosePseudoConsole` call deadlocks precisely when the reader is
  blocked in the output budget — with the drain timeout's clock unable
  to start. As built, teardown is a PHASE of the ordered writer loop:
  the reap-poll services output every iteration, the close runs on a
  dedicated closer thread, and the drain-timeout clock starts at
  closer-spawn. The windows-2022 CI leg (the image that still blocks)
  is the standing referee.
- **The contract recording, flags 0:** neither image emits a DA1 query
  on `hOutput` (windows-latest opens with `?9001h ?1004h`;
  windows-2022 goes straight to the first paint). The handshake answer
  machine ships anyway — pure bytes, once-per-run answer policy so a
  hostile producer gets no frame-amplification channel, an addition ON
  TOP of the gate's ruling — but step 5's design should treat "conhost
  asks at startup" as NOT OBSERVED under these flags.
- **Exit codes are raw `u32` end-to-end.** The Unix-style `i32` cast
  turned NTSTATUS values negative, and `GetExitCodeProcess`'s 259 is a
  real exit value once `wait()` has proven termination — the primitive
  is `exit_code_after_confirmed_exit`, precondition in the name.
- **The command lane is the step-5 seam.** `run(config, commands)` with
  one cancellable Input/Resize/Kill lane and no stdin ownership: the
  attach lane plugs into it; teardown revokes admission by ceasing to
  poll it.

Step-7 acceptance rows ALREADY PROVEN by CI on both windows images:
owned spawn; tree containment via kill-on-job-close (child + plainly
spawned grandchild); the pinned termination sequence
terminate → drain-to-zero → close-with-reader-running → EOF; repeated
partial-spawn unwind; spawn-failure compensation sealing verify-green;
natural exit recording the true exit code (259 and high-bit NTSTATUS
pinned); resize request/outcome with reject-never-clamp and exactly one
OS call per accepted request; the 8 MiB budget engaging under a 20 MiB
flood; DACL descriptor structure, protection, and inheritance. Remaining
for the real machine (step 7 unchanged): multi-user ACL denial,
logout/reboot ACL access, AV rename-retry, disk-full visibility,
forced-reboot recovery, and every FE-facing row — those need steps 5-6
first.

Accepted residuals, named in the landing commits rather than here being
resold as fixed: a full pty input pipe can still block the writer thread
in `write_all` (narrow; unmitigated by choice, not oversight), and a
test timeout detaches a genuinely hung OS thread because safe Rust
cannot kill one.

### Step 5 as specified (2026-08-28, pre-implementation review)

The build-order line for step 5 compresses the whole §attach-protocol
section plus four budget rows that only exist once clients exist. The
pre-code adversarial review of the work split overturned eight of its
first-draft decisions; the rulings, so implementation cannot re-litigate
them:

- **Connections are lane-typed, not frame-multiplexed.** The first
  frame's magic binds the connection (mgmt or attach); every later frame
  must match; violation closes it. Mgmt is lockstep — one request
  outstanding, no request IDs. A refused attach `hello` closes that
  connection; the ADR's "mgmt remains available" is satisfied by a fresh
  mgmt connection, and step 6's adoption challenge runs on a mgmt-typed
  connection whose pipe handle is exactly what
  `GetNamedPipeServerProcessId` verifies.
- **Both lanes carry fixed BINARY bodies; no JSON on the wire.** Outer
  framing `magic(4) + len:u32-LE + body`, 1 MiB cap enforced before
  parse. The pinned mgmt v0 shapes: request opcode byte (`probe`,
  `status`, `shutdown {reason: len u8 + UTF-8 ≤128 B}`), reply opcode
  meaning success, `status` carrying `pid:u32-LE`, `created:u64-LE` (raw
  FILETIME bits — exact `u64` bits cannot ride permanently-pinned JSON;
  above 2^53 exactness is parser-dependent, and the adoption challenge
  compares exact bits), and `survival:u8`. No version field anywhere in
  the lane, and no attach-version advertisement in it either — that
  would couple the permanent lane to the versioned one. Input, output,
  and checkpoint bytes ride raw (no base64), so the chunk arithmetic is
  exact. Every frame layout — requests, replies, refusals — is specified
  with byte-level goldens; attach proto v1 binds checkpoint format v1,
  decided before the goldens exist.
- **Attach is GROUND-GATED.** Checkpoint v1's contract (stated in the
  fork's `checkpoint.rs`) requires the post-checkpoint stream to start
  at a VTE ground-state boundary; ConPTY reads can end mid-CSI/OSC/DCS/
  UTF-8, so attaching at an arbitrary commit boundary is wrong. An
  attach PENDS — output keeps draining and committing — until the first
  commit boundary where the parser reports ground; then, in one writer-
  loop step: force the group-commit, publish those bytes to existing
  subscribers, encode the checkpoint, subscribe the new connection at
  that watermark. A bounded pend (ground recurs constantly in any sane
  stream) ends in a loud typed refusal, retryable. This requires
  `is_ground()` from the vt100 fork — VT state AND the UTF-8 decoder —
  which requires owning the state machine: no released `vte` (through
  0.15.0) exposes parser state, so the fork vendors vte's core, the same
  provenance move the fork itself is. Attach-fidelity tests must cut the
  stream inside CSI, OSC, DCS, and multibyte UTF-8.
- **The checkpoint never rides the live queue.** The maximum checkpoint
  (8,651,327 B) exceeds the 4 MiB per-watcher queue; enqueuing it there
  would evict every maximum-size attach. One global snapshot-transfer
  slot (a second attach waits — preserving the budget's single ~8.26 MiB
  transient term); the checkpoint is a writer work item outside the
  queue; live post-watermark output queues behind it; overflow before
  completion evicts.
- **The pen: capability-only EOF, demote-on-take, no local grant.** The
  capsule preamble stops at the null-holder state — the step-4 `"local"`
  grant is deleted; the first driver ever is a pipe `take`. `take`
  commits the strict-increment `take_state`, fsyncs, then acks, and
  DEMOTES the previous driver connection's capability (load-bearing for
  `resize`, which carries no identity fields). Connection EOF clears the
  ephemeral capability ONLY — no durable `{holder:null}` transition on
  EOF (a stale durable holder cannot type without a capability, and the
  EOF-vs-newer-take race disappears). The stale-epoch recheck stays
  immediately before the PTY write.
- **Dedupe is seeded, exactly once, in the walk that already exists.**
  Keys never expire and the corpus is the whole retained voyage: a
  successor capsule starting with an empty index would let a pre-crash
  `forwarded` key re-forward into the replacement shell — the exact
  double-forward dedupe exists to prevent. The index (16-byte key →
  input Seq, lattice state, intent Seq) is folded into
  `open_for_writing`'s existing full-frame walk, never a second scan;
  its memory is O(retained inputs), unbounded in v1, stated here rather
  than hidden. Retry folds are exact: `{input}` → new intent for the
  ORIGINAL input; `{input, intent}` → wire reply `delivery_unknown`,
  append NOTHING (the lattice has no intent→refused edge); later chains
  → replay the recorded outcome. The client supplies the idem_key; no
  content hash (an equality oracle over redacted input). Wire inputs are
  capped small (8 KiB) so step 4's accepted blocking-`write_all`
  residual stays narrow. `producer_observed` is not emitted — echo
  confirmation is the adapter's (ADR 0040); raw-terminal chains end at
  `forwarded`.
- **Liveness deadlines run on physical writes, driver-only.** One
  `keepalive {nonce}` echo frame (no ping/pong pair); its deadline clock
  starts when the transport reports the frame physically written (an
  enqueued ping behind a backlog must not kill a healthy reader);
  suspended during that connection's checkpoint transfer; `take` is
  refused until the taker's final checkpoint chunk is physically
  written; independently, a nonempty writer queue must make write
  progress within its deadline. Watchers get no keepalive — eviction
  bounds them. Transport cancellation is overlapped I/O + `CancelIoEx`,
  pinned (dedicated threads alone do not make blocked I/O cancellable);
  `ERROR_PIPE_CONNECTED` is success; `FIRST_PIPE_INSTANCE` on the first
  instance only — a rival create WITH the flag failing is the squat
  detection. The budget closes over a TOTAL subscriber cap (driver
  included) plus separately bounded pre-hello/mgmt connections, a
  pre-hello timeout, and lockstep inbound (one outstanding request per
  connection).
- **The pipe is never live while the writer lock is free.** Created only
  after `open_for_writing` holds the lock, closed before release —
  step 6's probe table depends on that implication. The pipe DACL is a
  variant of the store descriptor WITHOUT directory inheritance flags
  (pipe DACLs gate both ends; OI/CI is directory semantics). The voyage
  id is validated as a UUID before pipe-name interpolation. The shutdown
  ack is physically written before teardown closes its connection.
- **Survival is supplied, never inferred.** The capsule config gains a
  typed `Survival` the SPAWNER provides (step 6's breakaway attempt is
  the real source); mgmt `status` transports it. Deriving it from
  `IsProcessInJob` observation would cross the ADR's
  observation-is-not-authority line; the observation stays diagnostics.
- **Deleted from the production path** once the pipe lane is live: the
  local take grant, the raw internal input command and Windows stdin
  harness, Windows echo/stdout mirroring, capsule-generated idem keys,
  and every proposed protocol counter on the exit summary — tests
  observe checkpoint receipt, EOF, and voyage facts directly.

### Step 5 as built (2026-08-28, PRs #135, #136, #138, #139, #140)

Every spec-gate ruling shipped as specified — lane-typed connections,
binary bodies both lanes, the ground-gated watermark barrier, the
checkpoint outside the live queue, capability-only EOF with
demote-on-take and no local grant, dedupe seeding in the existing
`open_for_writing` walk, physical-write liveness clocks, the pipe never
live while the writer lock is free, supplied-not-inferred survival, and
the production-path deletions. What the paper design could not know,
learned by running it against the real OS and a real transport —
recorded because step 6 builds on these facts:

- **Attaching to an IDLE producer is the normal attach, and the gate
  must not wait for output to prove it.** As first wired, the ground
  evaluation ran only at fresh-output commit boundaries; a silent
  producer (a shell at its prompt) never produces one, so the attach
  rode its full pend into a GroundTimeout refusal. The rule as built:
  with no uncommitted bytes pending, the current position IS a commit
  boundary — the gate is evaluated the iteration an attach is admitted
  and on every tick while one pends. The masking behavior (residual
  conhost rendering after the producer finished) differed between CI
  images, which is why the bug surfaced late.
- **A real transport reorders `Sent` against the reply's own
  consequences.** The reply's overlapped write completes, the client
  reads it and sends its next request, and the server's reader thread
  can publish those bytes before the writer thread publishes `Sent` —
  so strict lockstep would close a fully compliant client. As built,
  lockstep HOLDS exactly one frame that arrives while its
  predecessor's reply is queued-but-unconfirmed and replays it on the
  completion — against the transfer's COMPLETED state (a one-chunk
  checkpoint's first chunk is its last; marking `Done` after the
  replay falsely refused a racing `take`). A second held frame, or a
  frame with no reply in flight, remains a violation.
- **The budget table's driver row is two clauses, together.** A first
  reading exempted the driver from the 4 MiB bound outright
  ("never dropped while live"); the review's correction stands as
  built: the bound STAYS, and overflow CLOSES the driver — bytes are
  never silently dropped while it lives, because ending liveness is
  the resolution; the record retains everything.
- **conhost delivers its rendered writes sequence-atomically** on both
  CI images — across 1000 script repeats, zero reader-chunk boundaries
  landed inside a CSI/OSC/DCS/UTF-8 sequence. The mid-sequence carry
  property therefore keeps its deterministic pins where cuts can be
  forced (the fork's ground/checkpoint suites, the wire splitter's
  every-byte-boundary fuzz); the e2e reports how much fragmentation a
  run actually exercised instead of gating on conhost internals.
- **Producer lifetime is a test invariant, never an emission-speed
  assumption.** Two distinct mechanisms produced one identical timeout
  symptom: a producer exiting mid-test moved the capsule into teardown
  (where admission is correctly refused), and a lingering-but-silent
  producer starved waits that needed live output. The helper's
  `--linger` and `--drip` modes make the intended lifetime explicit at
  every call site; the diagnostic trail (three falsified scheduling
  theories before the timeline reconstruction) is preserved in the
  landing commits as method, not embarrassment.
- **The transport seam is event-polling, not event-pushing.** The
  first bridge wrapped the pipe server in an actor thread with an
  unbounded forward channel — machinery that existed only because
  `run` took a separate event receiver, and which silently defeated
  the transport's own bounded inbound channel. As built, `Transport`
  exposes `try_recv_event`, the capsule polls under a per-pass quota
  (a flood interleaves with output/tick, never starves them), and the
  pipe transport owns its server directly. A terminal accept failure
  is not a mute-but-live service: it routes into the same orderly end
  as an external EndRun (reason `transport-accept-failed`), so the
  next leg re-binds a fresh pipe.
- **The real API referees what paper review blesses.** The pipe SDDL
  shipped with five ACE fields — both the author and the adversarial
  review read `(A;FA;;;sid)` as correct — and every bind failed with
  error 87 on the first real run; an ACE is six fields, and the empty
  flags field must be present: `D:P(A;;FA;;;<sid>)`. The descriptor
  builder now takes flags and rights separately so the malformed shape
  cannot be reconstructed.
- **The fork owns its state machine.** No released `vte` (through
  0.15.0) exposes parser state, so `is_ground()` required vendoring
  vte's core into the vt100 fork — where review then verified the
  pinned transition table against upstream's macro across all 4,096
  cells, and where vte's DEFAULT features turning out to be the
  `no_std`/arrayvec arm meant the OSC accumulation cap was the
  actually-shipping behavior and is kept unconditionally.

Step-7 acceptance rows now CI-PROVEN on top of step 4's: attach with
the screen restored exactly (checkpoint transfer over the REAL pipe,
restore-oracle-verified, mid-stream and idle); stale-controller input
refused per the lattice with the WAL chains proven across a capsule
restart; slow-watcher eviction while the driver stays live; the
hung-driver bound (keepalive + whole-queue progress deadlines, unit
level); mgmt probe/status/shutdown over the real pipe with the
shutdown ack physically sent before EOF; the pipe DACL on the wire.
Remaining for the real machine, unchanged: everything FE-facing,
multi-user ACL, reboot/logout, AV, disk-full, forced-reboot recovery,
supervisor adoption and the nightly composite — step 6 builds the
callers those rows need.

Accepted residuals, named in the landing commits: the e2e's
cross-process pid identity is asserted in-process only (the true
cross-process challenge is step 6's adoption test); keepalive timing
is unit-proven under synthetic clocks (the e2e's drip keeps the driver
perpetually active, which is fidelity to real use, not a gap); and the
capsule integration suite runs serialized because two real ConPTY
floods on a two-core runner starve each other — additive, not
adversarial, by design.

### Step 6 as specified (2026-08-28, pre-implementation review, round 2)

The contract for step 6 is the amended Lifecycle, Upgrade and Build-order
sections above — round 1 of this review put it in a section of its own,
which gave the ADR two incompatible normative readings, so it was moved
rather than restated. What remains here is what those sections do not
cover: the FE's own client behavior, the unit graph, and the acceptance
matrix that closes step 5's cross-process residual.

**The FE client, pinned where the protocol leaves a client free.**

- **One quit dispatcher, and it waits for the whole proof.** Every
  user-requested exit — the quit action, the window's close request, the
  title-bar and Alt-F4 path that reaches `event_loop.exit()` directly —
  routes through one `request_quit(reason)`; exit 75, a crash and a
  `--capture` run are excluded by construction, not by a check. The
  choice between waiting for full proof and defining an accepted
  asynchronous quit goes to FULL PROOF, for one reason: `shutdown_ok` is
  physically written BEFORE the drain, the process wait, the
  `producer_dead` write and the seal, so a quit that returns on the ack
  has proved only that the request was heard, and the failures worth
  knowing about all happen after it. The FE therefore holds its window
  open in a visible "ending session" state until ack + pipe closure +
  verify-green + lock release, bounded at 30 s (the teardown reap bound
  plus the process wait plus a seal, with margin). On timeout the window
  STAYS UP with a loud error and the session stays live: a quit that
  cannot prove the record is sealed must not look like a successful one.
- **Take-on-first-input is a transaction, not a hope.** Auto-take on
  attach stays rejected — ADR 0037 makes a reconnect a watcher and
  typing a deliberate act — but the first keystroke needs a pinned
  sequence or it is dropped, sent at a stale epoch, or overtaken. On the
  first input while WATCHING: enter TAKING, hold the input in a bounded
  pending queue (8 KiB, the wire's own input cap); send `take`; on
  `take_ok` send `resize` for the CURRENT viewport first — the watcher
  was rendering the driver's geometry and cannot correct it until it
  holds the pen — then flush the queue in arrival order, one outstanding
  request at a time. `take_refused{not_attached}` re-attaches first;
  `take_refused{checkpoint_in_flight}` retries at 250 ms up to 10 times
  and then fails visibly. Queue overflow DISCARDS with a visible
  indicator rather than delivering a keystroke minutes later into a
  context that no longer exists. The drawer shows which of
  watching / taking / driving it is in, and returns visibly to watching
  whenever the pen is lost.
- **Reconnect is a bounded state machine; backpressure is the FE's job
  to accept.** `disconnected → connecting → hello → attaching →
  watching/driving`, with reconnect backoff 250 ms doubling to a 4 s cap
  and NO attempt limit — the supervisor may legitimately be up to the
  spawn-ready bound away — and the drawer showing "reconnecting". Today's
  permanent dead-flag is deleted: a drawer is never permanently dead
  while a pointer names a voyage. Each reattach replaces the screen from
  the new checkpoint and shows the leg notice, never a silent blank
  swap. Today's reader drains into an unbounded channel, which would let
  a stalled GUI buffer without limit while still draining the pipe — the
  capsule would see send progress and step 5's slow-watcher eviction
  would never fire. The channel becomes bounded at the protocol's own
  watcher budget, and when it is full the FE STOPS READING THE PIPE, so
  a wedged FE is evicted as designed instead of growing.
- **`fe_down` has a schema, a baseline captured before it can be
  polluted, and a loud failure.** The line reuses the inbox envelope so
  existing readers stay honest — `from` remains a sender identity, `ts`
  remains the line's own time — and carries the window in its own field:
  `{"from":"sot-fe","to":"<handle>","text":"fe_down: no frontend
  attached from <t0> to <t1>","ts":<t1>,"kind":"fe_down","window":
  {"from":<t0>,"to":<t1>}}`. `t0` is read from the inbox's last line AT
  FE PROCESS START, before this run appends anything — reading it after
  attach would let messages received during this very startup become the
  baseline and hide most of the window it exists to report. No baseline
  (a first-ever attach) means no marker: a synthetic zero is a lie about
  a window that never existed. The append path today is best-effort and
  reports failure only as a trace warning, which contradicts the
  marker's entire purpose; it gains a `Result` and a visible drawer
  error.

**Units.** Five, ordered, with the constraint that made round 1's graph
unbuildable: a supervisor that spawns capsules while the FE still owns
its own PTY is two sessions, so those two activate ATOMICALLY.

- **U0 — dormant primitives.** In `sot-log`: the state-dir rule promoted
  (the FE delegates rather than keeping a copy); `drawer.voyage` and
  `supervisor.lock` with their create/validate/reset semantics; the
  probe classifier and the challenge as a library; the derived
  `MAX_PIPE_INSTANCES`. No new subcommands, no behavior change.
- **U1 — spawn inputs, still dormant.** `initial_command` replaces
  `resume_command` (a stale key is a loud startup warning naming its
  replacement — silently honoring it would re-run the retired ritual
  inside an adopted session), and shell resolution moves somewhere the
  supervisor can call. It lands BEFORE anything spawns and is exercised
  immediately, because the still-PTY-owning FE uses the same resolution
  for its own shell.
- **U2 — `sot-capsule supervise` / `endrun` / `reset`, not yet started
  by the launcher.** Election, the probe loop, spawn and adopt, the
  handle wait, reason-gated respawn, anti-flap, exit codes, the
  supervisor's own log.
- **U3 — the FE client, behind the same off-by-default flag.** Everything
  in the four rulings above, plus checkpoint restore into the drawer's
  own parser and the deletion of its DSR responder (the capsule has
  answered the host handshake since producer spawn, with no client
  attached).
- **U4 — the cutover, and it is one flag.** The launcher starts the
  supervisor and stages the capsule on the development path too; the FE
  defaults to attach-only; the teardown script is rewritten to the pinned
  order; the upgrade transaction lands. Archive membership may ride U0;
  apply policy rides here.

**Acceptance.** Per unit, and per open risk — the same table, because a
risk with no test is not tracked and a test with no risk is not
evidence.

| what it settles | the test |
|---|---|
| the step-5 cross-process residual | a real child `sot-capsule run`: reply-pid ≠ the test's own pid, reply-pid == the pipe's server pid, creation time == an independently opened handle's |
| the classifier, not just the happy path | a well-formed reply with a mismatched pid or creation time is FOREIGN and stops; EOF-before-reply, BUSY expiry and lock-held are PENDING and retry; retry expiry is WEDGED |
| probe transients under real contention | spawn/probe in a loop under an output flood: zero false wedges, zero double spawns |
| the election | two supervisors, one lock: the loser exits 0, never a second spawner |
| reason-gated respawn | a hard-killed capsule yields a new leg on the same voyage with a new epoch, both legs verify-green; an EndRun yields none, proven from the sealed reason |
| the anti-flap bound and its outer loop | a voyage that cannot open: three fast fails, terminal exit, no launcher restart |
| spawn-ready is a real bound | `open_for_writing` measured against a realistic retained voyage, failing as it approaches 60 s |
| the teardown order | the script ends the run and seals verify-green before the FE and tunnel die; the negative — a capsule it can see but cannot reach stops it loudly, voyage still open |
| the quit dispatcher | every user-requested exit path issues EndRun and waits for the whole proof; exit 75 issues none |
| FE fidelity across a leg boundary | attach, alternate-screen, and a resize between detach and reattach, restore-oracle-verified through the FE's own parser; a version-skew `hello` refusal is loud, never a blank screen |
| backpressure | a stalled FE is evicted by the capsule rather than buffering without bound |
| the marker | baseline captured before this run appends; skipped on a first attach; an append failure is visible |

On `windows-2022` the job currently runs `conpty` and `capsule_win`
only. Step 6 makes it, explicitly and by name:
`cargo test -p sot-log --test conpty`, `--test capsule_win`,
`--test pipe_win`, `--test e2e_pipe`, `--test supervisor_win`. The
middle two already exist and GAIN step-6 cases; a case added to an
existing binary is invisible on that image until its binary is named
here.

**New deferrals** (the ADR's scope paragraph and the step-7/P4 ladder
already carry the rest): voyage rotation policy for a long-lived drawer
voyage — measured in U2, decided in P4; multi-FE pen arbitration beyond
what demote-on-take already gives; and executable attestation for the
adoption challenge, which the DACL-bounded threat model excludes.

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

## Lifecycle: one owner, one rule, one transition, one probe

ONE spawn owner — the SUPERVISOR, a `sot-capsule supervise` process the
launcher starts and keeps started. (Amended 2026-08-28: the original
wording put the role in the launcher loop itself, which cannot perform
the probe — a PowerShell process can do neither overlapped named-pipe
I/O nor `OpenProcess`/`GetProcessTimes`. The ROLE is still one; its home
moves to the binary that already owns the voyage, the pipe and the wire.
The launcher's job shrinks to starting it, restarting it under the
exit-code contract below, and keeping the ssh tunnel.) The FE is
attach-only. Legs are spawned as CHILD PROCESSES and deliberately NOT
placed in the supervisor's job: the supervisor dying must be harmless to
the run, which is the whole reason adoption exists.

**Discovery, election and exclusion — two fences and a lease, each named
to the invariant it serves.** A supervisor cannot probe a voyage it
cannot name; two supervisors must not both spawn; and a spawn already in
flight must not escape a teardown.

- `<state-dir>\drawer.voyage` names the drawer's voyage: one canonical
  UUID, WRITE-ONCE. The voyage is the SESSION and every capsule run is a
  LEG (ADR 0039's epoch), so a respawn, an adoption and a next-morning
  start all reuse it, and therefore reuse the same pipe name — which is
  why the FE's reconnect target survives a leg boundary it never
  predicted. It is PUBLISHED, not merely created: temp file → write →
  file flush → NO-REPLACE rename → renamed-file flush → parent-directory
  flush, the store's own pinned Windows order, reusing that
  implementation rather than a second one. A bare `CREATE_NEW`-then-write
  can crash between the two and leave a permanent empty pointer that no
  later supervisor can win. Publication completes BEFORE any spawn is
  authorized, so a pointer can never name a voyage nobody can find. A
  pointer that exists but is empty or not a canonical UUID is CORRUPT: a
  loud stop naming `reset`, never a silent re-mint.
- Absence of the store a pointer names is NOT a licence to re-mint. ADR
  0039 pins absence of data as corruption, always, and "missing" can
  equally be an access denial, a transient I/O failure or an interrupted
  move. `NotFound` is distinguished from every other I/O error and both
  are a LOUD STOP naming the explicit `reset` operation, which takes the
  spawn gate, proves no live server, and renames the pointer aside under
  a unique no-replace name with a directory barrier — evidence is never
  overwritten and never reappears reordered after a crash.
- `<state-dir>\supervisor.lock` is the ELECTION fence: the same `fsutil`
  kernel lock the writer fence uses, created `CREATE_NEW` when absent
  (the writer fence refuses to create because bootstrap owns it; this one
  has no bootstrap, and an atomic create is what stops two supervisors
  minting rival inodes), then opened existing and held for the
  supervisor's whole life. Kernel-released on any death, so it is never
  stale. Acquiring it means you are the sole spawner; failing means a
  supervisor is already live and the loser EXITS 0.
- `<state-dir>\spawn.lock` is the SPAWN GATE, and it is a different
  fence because it answers a different question. (Amended 2026-08-28: one
  lifetime fence could not serve both. A teardown caller cannot take a
  lock the live supervisor holds for life, which is why an earlier
  revision exempted the FE's quit path — and that exemption was exactly
  the hole: an FE quitting during a spawn had no mgmt server to reach and
  no way to stop the capsule that was about to appear.) The supervisor
  holds it ONLY across a spawn-to-ready critical section; `endrun` and
  `reset` — the FE's quit path INCLUDED, with no exemption — hold it for
  their whole operation. So a teardown that arrives mid-spawn simply
  WAITS for the child to become ready and then ends it properly, and two
  EndRun callers serialize instead of racing. Acquisition is bounded
  separately from the probe (below): the probe cannot absorb a failure
  that happens before the caller is allowed to probe at all.
- A pre-ready child is held by a PARENT-DEATH LEASE. A capsule is
  deliberately not in the supervisor's job, so between `CreateProcess`
  and readiness it would otherwise outlive a supervisor that died, take
  the writer fence after a teardown had already concluded nothing was
  running, and reappear headless. The supervisor therefore hands the
  child an inherited, synchronizable handle to itself; the child checks
  it immediately before acquiring the writer fence and again immediately
  before binding its pipe, and EXITS without doing either if the lease is
  broken. The residual window is not zero — the supervisor can die
  between the last check and the bind — so it is bounded rather than
  claimed away: after taking the spawn gate, `endrun` and `reset` must
  observe ABSENT TWICE, separated by the bind window, before concluding
  that nothing is running. The lease shrinks the window; the double
  observation covers what is left.

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
implementation, invoked by the FE real-quit path, `sot-capsule endrun`
(which the teardown script calls) and the incompatible-upgrade end-run.
EndRun = mgmt `shutdown` → the capsule acks, drains, seals, exits. Proof
of completion is capsule ack + pipe closure + verify-green + lock
release — never `WaitForExit` (an adopting supervisor has no child
handle). The ACK ALONE IS NOT PROOF: it is physically written before the
drain, the process wait, the `producer_dead` write and the seal, so a
caller that exits on the ack hides a failure in every step that
actually matters. The `reason` (≤128 B) is recorded in `producer_dead`'s
detail, and its presence there is the durable, verified, single-writer
signal that the end was REQUESTED. A raw-terminal EndRun writes
`producer_dead` + seal only — raw terminals emit no turns, so there are
no synthesized closes.

**Respawn is gated by a TYPED marker the capsule writes, not by a
diagnostic string.** (Amended 2026-08-28: an earlier revision gated on
the presence of `producer_dead.detail.reason`, which the shipped capsule
also writes for a spawn failure and for `transport-accept-failed` — the
two ends it must RECOVER from. Gating on it would have made a missing
shell look like an operator's decision.) On receiving mgmt `shutdown`
the capsule appends one `run_end_requested {reason}` lifecycle frame,
committed IMMEDIATELY and BEFORE the `shutdown_ok` ack is queued, so the
evidence is durable before any caller can observe success. It is written
by the capsule — the single writer of its own store — inside the leg's
own epoch, and it inherits that leg's seal, which is what makes it
single-writer, generation-scoped and durable at once.
`producer_dead.detail.reason` stays a free-form DIAGNOSTIC and is never
read as a discriminator. The mapping is total: a spawn failure and a
transport fatal write no `run_end_requested`, so both are recovered;
only an externally requested end has one. A second `shutdown` while one
is in flight is idempotent — acked, no second frame, the first reason
stands.

**Start authorization distinguishes a crash-restart from a new
session.** The gate above is read for exactly one epoch — the leg the
supervisor itself spawned or adopted — never "the latest record",
because a leg that died before writing its own tail must not inherit an
older leg's verdict. On STARTUP the supervisor reads the highest sealed
epoch instead, and needs one more bit: a launcher that restarts a
crashed supervisor must not resurrect a session the operator ended,
while a launcher STARTED FRESH tomorrow must be able to. The launcher
therefore mints one START TOKEN per launcher run and passes the same
value to every supervisor it starts within that run; the supervisor
passes it into the capsule's spawn detail, where it is sealed with the
leg. Startup spawns unless the highest sealed epoch carries BOTH a
`run_end_requested` AND this same start token — that combination, and
only it, means "this launcher run already ended its session". A new
launcher run carries a new token and starts normally. No file, no
second writer.

**Respawn is bounded, and the criterion is READINESS, not age.**
(Amended 2026-08-28: a raw 10 s floor let a store that fails to open
after 11 s reset the counter forever and spawn without end.) A child
that never reaches READY — the point at which it holds the writer fence
AND its pipe answers the challenge — is a STARTUP FAILURE however long
it took, including one killed at the readiness deadline. Three
consecutive startup failures stop the loop. Only a leg that actually
reached READY resets the count; the time it then ran is irrelevant.
Diagnosis is whatever exists: the sealed `producer_dead` detail when a
segment sealed, the child's exit code and stderr tail when the store
never opened at all.

**Supervisor exit codes are the launcher's contract**, so the outer loop
can neither defeat the bound nor resurrect an ended session. `0` = clean
end (the run ended by request, or the supervisor was asked to stop) —
DO NOT restart. `69` = terminal (anti-flap threshold, a foreign server,
a wedge) — DO NOT restart; surface the error. Anything else is a
supervisor crash — restart with the backoff the launcher already gives
the tunnel, at most 5 restarts in 60 s, then stop and report. Only a new
launcher run, an operator act, starts a session that ended cleanly.

**The machine-teardown ORDER is pinned** (the nightly close): supervisor
stop → EndRun, holding the spawn gate → capsule ack + seal + lock
release → FE close → tunnel down. (Amended 2026-08-28: the original put
EndRun first, so that the script's opening force-kill could not strand a
run. But the supervisor's entire job is to replace an ended leg, so
EndRun while it lives races it into spawning a fresh headless capsule
that the following steps then orphan. Stopping the spawner first costs
nothing — it holds no handle the capsule depends on — and makes the
EndRun step's own observation unfalsifiable, because holding the spawn
gate means nothing can spawn. The invariant is unchanged: THE RUN IS ENDED
BY REQUEST, AND ITS RECORD SEALED AND VERIFY-GREEN, BEFORE THE FE AND
THE TUNNEL GO DOWN.) "Supervisor stop" is a defined handshake, not a
force kill: the launcher — the restart owner — is told to stop first so
it cannot restart what the teardown is about to kill; the supervisor is
asked to stop, acknowledges, and the caller waits for its PROCESS EXIT
and then for the election fence to release, on a bound of its own,
because a kernel lock's post-kill release is documented as having no OS
bound. Orphan stop: spawn gate held and ABSENT observed twice means skip
EndRun and proceed — a fresh install and a post-crash cleanup
must both work. Every wait is bounded and every timeout is LOUD: a
teardown that cannot reach a capsule it can see STOPS and reports a live
session rather than tearing the tunnel out from under it. The
daemon-detach-before-tunnel property the current script achieves is
preserved by this order and asserted by the rewrite's own check.

**Adoption is never silent, and the notice never claims what its teller
cannot know.** The FE talks to the capsule; only the supervisor knows
whether it spawned or adopted, and `status.created` is the LEG process's
creation time, not the session's. So the FE shows one truthful message
on every attach — "attached to leg started `<time>`" — and the
spawn-versus-adopt distinction is logged by the supervisor, which is
where that fact actually lives. A next-morning attach to yesterday's
session is still a visible event: the leg time is yesterday's, on
screen, in the drawer.

**The probe algorithm** — one classification, run at supervisor start
and at every loop re-entry. (Amended 2026-08-28: the original was a
table over `(pipe, writer.lock)` read as if the two were sampled at
once. They are two syscalls, and step 5's teardown closes the pipe
microseconds before it releases the lock, so an ordinary healthy
shutdown could be classified fatal. The pipe's own ANSWER is the
authority; the lock is consulted only where there is no pipe to ask;
and the only fatal classification is a WELL-FORMED WRONG ANSWER.)

| class   | evidence                                          | action    |
|---------|---------------------------------------------------|-----------|
| ADOPTED | connect ok, challenge passes                      | adopt; never touch the lock |
| FOREIGN | connect ok, well-formed mgmt reply, identity does not match | LOUD STOP |
| PENDING | connect ok but EOF or timeout before a reply; `ERROR_PIPE_BUSY` past its own retry; any other connect error; no pipe with the lock HELD; a spawned child alive but not yet holding the lock | retry |
| ABSENT  | no pipe, lock free, no spawn outstanding          | spawn     |
| WEDGED  | the retry budget expired in PENDING               | LOUD STOP |

PENDING is ONE class because its members are indistinguishable from
outside and converge on the same two answers. It covers four REAL
healthy states, each named so its bound can be chosen rather than
guessed: a capsule that has bound its pipe but not yet built its
protocol machine (bind is deliberately the first fallible step after the
lock, ahead of `seal_survivor`, the first segment and the preamble); a
capsule in its shutdown TAIL, whose pipe stays live through the reap
poll, the process wait, the `producer_dead` write and the seal; a live
capsule momentarily at its instance cap; and SPAWN-PENDING — a child
that exists but has not yet acquired the writer lock, because
`open_for_writing` enumerates, reconciles and rebuilds the dedupe index
over all retained history before it returns. That last window is
O(retained history), so it gets its own, much larger bound.

The numbers, pinned here so that no implementation invents them:

| bound            | value               | what it must dominate            |
|------------------|---------------------|----------------------------------|
| connect          | 2 s                 | `connect_voyage_pipe`'s existing retry |
| challenge        | 2 s                 | one `status` answered from memory by a loop whose per-iteration budget is one group-commit window |
| probe retry      | 10 × 500 ms = 5 s   | startup-before-protocol, the shutdown tail (worst case the 5 s process wait plus a seal), and BUSY |
| spawn-ready      | 60 s, polled 500 ms | O(retained history) `open_for_writing`; a child exit short-circuits it |
| lock retry       | `lock_writer`'s existing 250 ms | nothing more: Windows' unbounded post-kill release lag is absorbed by the probe retry above, never by a second lock bound |
| anti-flap        | floor 10 s, 3 consecutive | a leg that cannot start    |
| launcher restart | ≤ 5 in 60 s         | a crash-looping supervisor       |

The 60 s spawn-ready bound is DEFENDED, not assumed: the acceptance
suite measures `open_for_writing` against a realistic retained voyage
and fails if it approaches the bound. If it grows there, the answer is
voyage rotation, not a larger constant.

**The challenge**, unchanged, with its ORDER load-bearing: (1) read the
server pid P via `GetNamedPipeServerProcessId` on the live connection;
(2) `OpenProcess(P, PROCESS_TERMINATE |
PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE)`; (3) AFTER the open,
issue a mgmt `status` challenge on the SAME connection; (4) the handle
is proven to be the server iff reply-pid == P AND reply creation time ==
`GetProcessTimes(handle)`, compared on the exact FILETIME bits. (A dead
true server cannot answer; a live one means P is not recycled — pids are
unique among live processes.) The (handle, creation-time) pair is the
pinned identity; SYNCHRONIZE makes the post-termination wait executable.
WHAT THIS PROVES, narrowly: the answering process IS the server behind
this connection, and it is alive with a stable identity. It does not
attest an executable — a same-user process implementing the pinned v0
lane would pass. The bound on that is the DACL: the pipe admits only the
owner account and rejects remote clients, and this ADR's threat model is
other local users and anonymous access, NOT the owner. No attestation is
added for a threat the model excludes.

That retained handle is the DEATH SIGNAL as well as the termination
authority: the supervisor WAITS on it rather than holding a management
connection open for the capsule's whole life, which leaves the
protocol's non-watcher pool free for transient callers. There is no
reserved management slot and none is needed — a probe refused because
the pool is momentarily full closes without a reply, which is PENDING,
which retries. `MAX_PIPE_INSTANCES` is derived from the protocol's own
caps (`NON_WATCHER_CAP + SUBSCRIBER_CAP`), never an invented constant.
Waking on the handle is not proof of a COMPLETED EndRun — that stays ack
+ pipe closure + verify-green + lock release; it is only a wake-up.

## Upgrade and version skew

`probe`/`status`/`shutdown` ride the pinned v0 mgmt framing — the
permanently compatible lane; attach-protocol versions negotiate via
`hello` above it.

**An upgrade is ONE atomic transaction that stops the session first.**
(Amended 2026-08-28: "stage-after-exit" was written as though the
capsule image could be deferred on its own. It cannot — the same
executable is the supervisor, so "no capsule is live" does not make the
file replaceable, and the applier's binaries, manifest, junction and
rollback are already one transaction, in which deferring a single image
can activate a mixed release or roll back over a running one.) The
launcher stops the supervisor, acquires the election fence, ends any
live run through the incompatible-upgrade EndRun, and only then does the
applier replace anything; the supervisor restarts afterwards and spawns
a fresh leg. The consequence is stated rather than hidden: UPGRADING
ENDS THE DRAWER SESSION, on purpose, once, at a moment the operator
chose. Archive membership — the capsule in the artifact, the manifest,
the required-file list, and a `--version` line in the shape the smoke
job asserts — may land before that transaction; apply policy may not.
The DEVELOPMENT build-and-stage path is bound by the same rule: a dev
loop that stages only the frontend runs yesterday's capsule against
today's protocol. If a security defect invalidates even
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
6. The supervisor and the attach-only FE, which ACTIVATE TOGETHER:
   discovery pointer + election fence + probe/adopt/challenge;
   reason-gated respawn with the anti-flap bound and the launcher
   exit-code contract; `sot-capsule supervise`/`endrun`/`reset`; the FE
   as a pipe client (bounded reconnect, bounded backpressure, the
   take-on-first-input transaction, checkpoint restore, ONE quit
   dispatcher issuing EndRun); the `fe_down` attach marker;
   `initial_command`; the teardown-script rewrite to the pinned order;
   the upgrade transaction. Half of this does not ship: an FE that still
   owns its own PTY plus a supervisor that spawns capsules is two
   sessions, so the cutover is one flag.
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
   unpublished tail discarded, a new epoch, verify green); the supervisor
   election (the loser exits 0, never a second spawner); an upgrade that
   ends the run and comes back clean; breakaway-denied degraded path;
   alternate-screen
   attach fidelity roundtrip; the NIGHTLY COMPOSITE (supervisor AND FE
   force-killed AND tunnel torn, no EndRun — the capsule survives
   headless, and the next supervisor start ADOPTS it with the visible
   attach notice, never silently); the rewritten teardown script ends
   the run (EndRun → seal → verify green) before the FE and the tunnel
   die; the
   `fe_down` marker appears on attach after a respawn and wakes the
   drawer session's Monitor.

## Consequences

- The drawer session stops dying with the frontend; the resume ritual —
  the single most fragile piece of the FE session lifecycle — is
  deleted rather than repaired.
- OPERATOR NOTE — an intuition inversion: after P3, QUITTING (real quit
  = EndRun) and UPGRADING are what end the drawer session, while
  crashes and rebuild-relaunches are harmless to it. Today it is exactly
  backwards (quit is safe, crashes lose work). The attach notice's leg
  time and the "run ended — new leg" UX exist to keep this inversion
  honest at 6pm.
- The store becomes genuinely cross-platform with equal guarantees,
  which P4 (bridge) and every later phase inherit for free.
- The Windows kill domain is SIMPLER than Linux's in the common crash
  path: the kernel reaps on capsule death with no successor act.
- The ADR 0037 ladder's platform ordering is corrected: Windows first;
  macOS remains a recorded sketch (process groups, everything
  best-effort) until a macOS machine exists to dogfood it.
