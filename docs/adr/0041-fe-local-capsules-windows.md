# ADR 0041: FE-local capsules on Windows (Ship's Log P3)

**Status:** Accepted (2026-08-25). The contract for running voyage-recording
capsules on the Windows frontend machines, so the frontend's Terminal-drawer
session survives frontend restarts. Implements the ADR 0037 ladder's P3.
**Date:** 2026-08-25

> How this was designed: a three-track research spike (Windows store
> primitives, Job Objects and the successor problem, the frontend's real
> process tree) followed by adversarial review rounds with mandatory
> deletion pressure, per step. Machinery deleted on the way: named jobs
> and their locator feature, ReFS, the explicit detach op, capsule-side
> scrollback, synthesized closes for raw terminals, a sprawling exit
> matrix, a second spawn fence, and the `resume_command` mechanism
> itself.

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

### Step 6 as specified (2026-08-28, pre-implementation review)

The contract is the "Lifecycle", "Upgrade and version skew" and "Build
order" sections BELOW, the last of which also carries the unit graph.
This section carries only what those do not: the FE's client behavior
and the acceptance matrix.

**The FE client, pinned where the protocol leaves a client free.**

- **One quit dispatcher, waiting for `record_closed`.** Every
  user-requested exit — the quit action, the window close request, the
  title-bar and Alt-F4 path that reaches `event_loop.exit()` directly —
  routes through one `request_quit(reason)`; exit 75, a crash and a
  `--capture` run are excluded by construction. The FE sends `end_run`
  on the supervisor lane and holds its window open in a visible "ending
  session" state until the lane's `end_run` reply — which arrives at
  `record_closed`, with `record_verified` following through `query`, on
  the FE QUIT cutoff. On expiry the window STAYS UP and says
  **"ending the session did not complete — outcome unknown"**: by then
  the job may already be terminated and the capsule mid-seal.
- **Take-on-first-input is a transaction.** Auto-take on attach stays
  rejected — ADR 0037 makes a reconnect a watcher and typing a
  deliberate act. On the first input while WATCHING: enter TAKING, hold
  the input in a bounded 8 KiB queue (encoded bytes, one wire input's
  cap; a larger paste splits there and the remainder is discarded
  visibly, never delivered minutes late into a context that no longer
  exists); send `take`; on `take_ok` send `resize` for the CURRENT
  viewport first — a watcher renders the driver's geometry and cannot
  correct it until it holds the pen — then flush in arrival order, one
  outstanding request at a time. `take_refused{not_attached}` re-attaches
  first; `take_refused{checkpoint_in_flight}` retries every 250 ms for up
  to 30 s, matching the connection's own write-progress allowance, since
  a legal 8.65 MiB checkpoint is entitled to that window, and at expiry
  the queue is DISCARDED visibly. `resize_refused{out_of_budget}` keeps
  the pen and reports the geometry unrepresentable; `{not_driver}` means
  the pen is gone.
- **Outstanding input survives reconnect, exactly once, WITHIN ONE
  VOYAGE.** The wire makes the CLIENT own the idem key and defines three
  terminal answers, so a connection dying between the capsule recording
  an input and the reply arriving is what decides whether a command is
  lost or run twice. The FE retains the exact
  `(voyage_uuid, idem_key, take_epoch, bytes)` — at most one outstanding
  — across reconnect and, after re-attaching and re-taking, RESENDS THE
  SAME KEY, which the durable dedupe index answers deterministically.
  `input_recorded` completes it. `input_delivery_unknown` is never
  auto-retried (the wire forbids it): the input is dropped and marked
  visibly unknown. `input_refused_stale` means the epoch moved, so it is
  re-sent under the new epoch with a NEW key — the old chain is closed
  by the refusal, so no double execution is possible. If the VOYAGE UUID
  has changed, the tuple is CANCELED and marked unknown, never replayed
  and never re-keyed: the dedupe index is per-voyage, so replaying into
  a reset session would type yesterday's bytes into a new one, which is
  neither loss prevention nor exactly once. A quit or cancel with a key
  outstanding reports it rather than dropping it silently. Resize
  carries no key and is simply re-sent after any successful take.
- **Reconnect is bounded, classified, and re-reads the pointer.**
  `disconnected → connecting → hello → attaching → watching/driving`,
  backoff 250 ms doubling to a 4 s cap. `drawer.voyage` is re-read and
  re-validated at the START OF EVERY EPISODE, so a `reset` is followed
  rather than looped against with a cached UUID. TERMINAL, with an
  actionable error offering retry and reset: a `hello` refusal, a
  FOREIGN or access-denied pipe, an absent or corrupt pointer, an
  operator cancel, and — the case a valid pointer would otherwise retry
  forever — a supervisor lane reporting phase ENDED-NO-RESPAWN or
  TERMINAL, or the voyage pipe absent while that lane is absent OR
  UNRESPONSIVE (its `status` unanswered within its budget, since
  `pipe_win` can keep a name alive over a dead accept path) for the
  HEALTH WINDOW. The phase makes a still-answering authority's terminal
  state visible immediately; the timeout covers the one that cannot
  answer at all. Everything else retries. Today's
  permanent dead-flag is deleted; each reattach replaces the screen from
  the new checkpoint, never a silent blank swap. The reader's unbounded
  channel becomes BYTE-ACCOUNTED and bounded at 4 MiB — bytes, not
  items, counting the frame being decoded, because the current channel
  carries byte vectors and an item count at the same number would permit
  unbounded memory. When it is full the FE STOPS READING THE PIPE, so a
  wedged FE is evicted by the capsule as step 5 designed.
- **The attach notice is bound to the leg it describes, and claims only
  what its teller can know.** The FE talks to the capsule; only the
  authority knows whether it spawned or adopted, and `status.created` is
  the LEG process's creation time, not the session's. So the notice is
  one truthful message on every attach — "attached to leg started
  `<time>`" — the spawn-versus-adopt distinction is logged by the
  authority, where that fact lives, and a next-morning attach to
  yesterday's session is still a visible event because the leg time is
  yesterday's, on screen, in the drawer. Mgmt and attach are separate
  lane-latched connections, so a leg dying between them would let the FE
  render leg A's start time over leg B's restored screen: the FE
  compares the pipe server's pid and creation time on BOTH connections
  and shows the notice only when they match; on mismatch it re-reads
  status. No new frame.
- **`fe_down` claims only what it can observe.** The inbox timestamp
  records when a message was relayed, not when a frontend was alive, so
  "no frontend attached from `<t0>` to `<t1>`" was false for any FE that
  ran a day without inbound traffic. The line is
  `{"from":"sot-fe","to":"<handle>","text":"possible relay gap: last
  inbox evidence <t0>, frontend reattached <t1>","ts":"<t1>",
  "kind":"fe_down","window":{"last_evidence":"<t0>"}}` — the
  envelope's `ts` IS the reattach time, and both values are ISO-8601
  STRINGS. `t0` is read at FE PROCESS START, before this run appends
  anything, or the current startup's own traffic becomes the baseline.
  No baseline, no marker. The append path gains a `Result` and a visible
  drawer error: a marker that exists so a failure is not quiet cannot
  fail quietly itself.

**Acceptance** — the races this design turns on, not only its happy
paths.

| what it settles | the test |
|---|---|
| the step-5 cross-process residual | a real child `sot-capsule run`: reply-pid ≠ the test's own pid, reply-pid == the pipe's server pid, creation time == an independently opened handle's |
| the classifier is total AND every row reachable | one case per row, including malformed frames, wrong opcodes, ACCESS_DENIED, `CreateProcess` failure, `WAIT_FAILED` and kill failure; the Stage-B deadline wedges every Stage-B PENDING row, while an A5 child at its readiness cutoff (A5 at t=0 → A3 at the cutoff) reaches KILL+WAIT — NO Stage-A terminal result may leave a live child |
| the marker is the acceptance barrier | an ack stalled after a durable marker still tears down; a failed append yields no ack, no marker, an unsealed exit, `failed {record_append}`, a released hold and a replaced leg; two concurrent callers get one marker and two acks; the frame is refused by the verifier in an undeclaring segment, and a declaring segment is refused by a pre-feature reader |
| a spawn failure is recovered, not obeyed | a missing shell and a transport fatal both respawn; only `run_end_requested` suppresses |
| start modes | `--resume` after a requested end exits 0; `--start` after the same end spawns; an ADOPTED leg ends correctly; a provable torn tail is REPAIRED and replaced while a complete corruption exits 69 without spawning |
| operation recovery is crash-total | a crash injected after journal durability, after the pointer rename, after pointer publication / marker commit, after terminal journal durability, and before the reply — each followed by restart and `query`: reset reconstructs `reset_done {new_voyage}` and never mints a second identity, EndRun reconstructs from the marker, a lost `reset` reply retried under the same operation id does not rename twice, and no entry is left ACTIVE forever |
| an ended authority still answers | after a requested end, `query`/`status`/`stop` are served before the start-mode exit; a restarted authority serves a recovered result before exiting 0 |
| the lease is not a sample | supervisor killed between `CreateProcess` and fence acquisition, and again during the history walk: the child exits or is visible, never both-invisible-then-binding |
| FE quit during spawn-pending | the request is admitted and latched DURING the spawn and ends the run properly, never leaving a capsule behind |
| reset under a live authority | mediated, never concurrent; no two-identity state |
| the flap bound | a store that never opens, and a shell that dies 1 ms after READY, each increment the ONE counter to its threshold |
| pointer durability | a crash between create and write leaves no unusable permanent pointer; reset preserves evidence under a unique name |
| input across reconnect and across reset | same key resent within a voyage, each of the three outcomes handled; a changed UUID cancels rather than replays |
| reconnect terminates | version skew, foreign pipe, corrupt pointer, reset, and a supervisor's terminal exit each reach a terminal state |
| status and attach cross a leg | a leg death in that window is detected, never rendered as the wrong start time |
| backpressure | a stalled FE is evicted rather than buffering without bound |
| the teardown order | the run ends and the record closes before the FE and tunnel die; an unreachable-but-visible capsule stops the script loudly |
| the release transaction is quiescent both ways | image replacement after both proofs never hits a sharing violation; a first-ever capsule install rolls back by DELETION; a supervisor-69 rollback with the new FE alive quiesces it first and never touches a locked image; the capsule is ended while the supervisor still holds its challenged handle; a pre-reader install cannot activate the writer at all |
| lifecycle commands are voyage-fenced | an `end_run` minted against voyage A and delivered after a reset to B is `refused {stale_voyage}` with no mutation; a `status` before the first leg answers with no `leg` and phase STARTING |
| the lane's own lifecycle | a squatted name loses first-instance bind and exits 69; a present-but-unanswering lane is treated as absent; a mismatched build is refused |
| the challenge authenticates the SERVER | on a real Windows machine, an OTHER-USER process pre-binds the supervisor name AND the voyage name with a permissive DACL and answers plausibly: every public client entry point classifies it FOREIGN before decoding or trusting a reply, and each target-token lookup failure lands PENDING, never READY or ADOPTED |
| the supported history envelope composes | a release-mode cold-history open at the named boundary publishes its measured time, and the gate fails unless `B >= measured × 3` and EVERY formula and consumer is recomputed with it in the same change; at that boundary a Stage-B observer of a fence-held/name-absent history walk does not expire before the child binds |
| a refusal is terminal | a step-0 feature-boundary refusal mutates no release or voyage state, arms `rollback_refused` durably, and a COLD launcher start then surfaces recovery without relaunching the failed release or retrying rollback |
| health is total | each health-table row decides as written, including READY+1 ms death and a lane that wedges after answering once; exit 75 at the deadline with no live FE ROLLS BACK; `just-applied` is never left ARMED — it is committed, rolled back, or terminal |
| teardown composes | worst-case worker fan-out completes inside the 20 s aggregate and the fence is free before the probe episode expires; a probe during the tail, and against four idle mgmt clients, never reaches WEDGED |
| `fe_down` evidence | baseline captured before this run appends; skipped on a first attach; an append failure is visible |

The `windows-2022` job runs `conpty` and `capsule_win` under one
10-minute cap. Step 6 makes it five serial commands with pinned budgets
— readiness, flap and grace cases use INJECTED CLOCKS, so no test
spends its cutoff in wall time:

| `cargo test -p sot-log --test …` | cap |
|---|---|
| `conpty` | 5 min |
| `capsule_win` | 10 min |
| `pipe_win` | 10 min |
| `e2e_pipe` | 10 min |
| `supervisor_win` | 15 min |
| job total (incl. ~15 min checkout and build) | 75 min |

`pipe_win` and `e2e_pipe` already exist and gain step-6 cases, which are
invisible on that image until their binary is named.

**Deferrals**, one line each: voyage rotation → P4, and until then the
honest ceiling on the readiness cutoff. A versioned pen-change
notification → P4 with remote attach, both needing the attach lane's
next version; until then a demoted FE learns it on its first refused
driver operation and the indicator is best-effort. Executable
attestation → excluded by the threat model, not deferred.

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

## Lifecycle: one authority, one rule, one transition, one probe

**ONE AUTHORITY.** Every act that starts, ends, adopts or resets a run is
performed by the process holding `<state-dir>\supervisor.lock` — a
`sot-capsule supervise` process the launcher starts, or, when none is
running, whichever caller acquires that fence for its own operation.
Nobody else touches a capsule. One authority behind one fence is what
makes "may I spawn?", "may I stop?" and "may I reset?" a single answer
instead of three that must agree: a second gate held by arbitrary
callers cannot linearize a spawn decision taken just before it is
acquired, cannot be acquired inside a legitimate long hold, and lets
`reset` run under a live supervisor.

The FE is attach-only and asks; it never acts. Requests reach the
authority over the **supervisor lane**, whose whole lifecycle is pinned
here because "reusing `pipe_win`" carries mechanics, not a contract:

- **Name and identity.** `\\.\pipe\sot-supervisor-<h>`, where `<h>` is a
  stable hash of the canonicalized state-dir path — the same thing that
  scopes the pointer and the fence, so two installs on one machine
  cannot collide and one install cannot address the wrong state.
- **Security is MUTUAL.** The server side is the posture the voyage pipe
  already proves: an explicit owner-account DACL,
  `PIPE_REJECT_REMOTE_CLIENTS`, non-inheritable handles, and
  `FILE_FLAG_FIRST_PIPE_INSTANCE`. The client side is THE CHALLENGE
  below, run before any reply is trusted — one procedure for both pipe
  families, because both have the same object-authentication problem and
  the voyage pipe's own clients have the hole today. A build identity is
  compatibility data, not a credential.
- **Lifetime, bracketed by the fence.** Bound only while holding
  `supervisor.lock`, AFTER the fence and BEFORE any adopt or spawn, and
  dropped BEFORE the fence releases. So the lane is present exactly when
  an authority exists, and a failed first-instance bind is terminal
  (exit 69) rather than a supervisor that runs unreachable.
- **Liveness, because a present name is not a living service.**
  `pipe_win` deliberately retains a dead instance and terminalizes
  acceptance while keeping the name alive, so absence is not the only
  failure. Every client's first act, after the identity check above, is
  a `status` with a 5 s budget; a lane that accepts but does not answer
  within it is treated exactly as an absent lane. On the server side, a
  terminal accept failure EXITS the supervisor (69) rather than leaving
  a misleading live name.
- **Build boundary.** The first frame of every connection carries the
  build identity; a mismatch is answered `refused {version_skew}` and
  closed. File replacement is not process replacement — the launcher is
  a long-lived process and an old FE can outlive an apply — so the
  upgrade transaction quiesces the old FE explicitly (below) AND the
  lane rejects the pair it did not. Identity is the belt; the build
  check is the version-skew braces.

**The lane's operations are one command family, one query family, and one
stateless request.**

- `command {op, operation_id}` where `op` ∈ { `end_run {reason, voyage}`,
  `reset {voyage?}`, `stop` }. Every op's success is a typed terminal
  result, not silence: `end_run` → `record_closed` then
  `record_verified`; `reset` → `reset_done {new_voyage}`; `stop` →
  `stopping` and then process exit, which the caller confirms by the
  handle and fence rather than by a further frame.
- **Lifecycle commands are VOYAGE-FENCED.** `end_run`, and a `reset`
  aimed at a live pointer, carry the voyage the caller OBSERVED, and it
  is part of the command digest. A mismatch is
  `refused {stale_voyage}` with no mutation. Without this, an FE
  attached to voyage A whose delayed quit arrives after someone else
  reset to B would be correctly linearized by the one authority — and
  would end B, a session it never saw. An `operation_id` cannot help: it
  identifies the request, not its target.
- `status` is a SEPARATE STATELESS REQUEST — not a `command`, so it
  carries no `operation_id` to be ignored, mutates nothing, and can
  never evict a destructive result from the journal. It answers
  `status_ok {voyage, leg?, phase}` — the connection's accepted
  handshake already carries the build. `leg` is optional and
  voyage-qualified, because the lane binds after the fence and BEFORE
  any adopt or spawn, so the mandatory first `status` of every client
  must be able to say "no leg yet". `phase` is total over the
  authority's own life: STARTING | READY | ENDING | ENDED-NO-RESPAWN |
  TERMINAL. CONTINUOUS READY IS THE OBSERVER'S OWN MEASUREMENT:
  uninterrupted identical observations, over a clock no server can
  compute wrongly.
- **`operation_id` is durable for MUTATING ops only.** Before the first
  irreversible act of an `end_run`, `reset` or `stop`, the authority
  PUBLISHES a journal record under the owner-protected state subtree —
  the id, a canonical digest of the command, its state, and for `reset`
  the new voyage identity it intends to publish — with the store's own
  crash-durable publication order, because NTFS metadata journaling is
  crash CONSISTENCY, not durability-at-return, and a record that can
  evaporate would let `unknown_operation` authorize a second destructive
  run. Retention is the durable journal, not a size-bounded cache. An id
  resubmitted with a DIFFERENT command digest is `refused {id_conflict}`;
  an id already ACTIVE returns its current state rather than executing
  twice.
- `query {operation_id}` returns `accepted` | `in_progress` |
  `record_closed` | `record_verified` | `reset_done {new_voyage}` |
  `stopping` | `failed {detail}` | `refused {reason}` |
  `unknown_operation`. `unknown_operation` is returned for a MISSING
  journal entry and ONLY that; it is the one state meaning SAFE TO
  RESUBMIT. A malformed or torn journal is not missing — it is LOUD, and
  the authority stops rather than guessing. Every other state means the
  operation exists and must not be reissued under a new id. This also
  resolves the `end_run` timing question: the COMMAND reply arrives at
  `record_closed`, and `record_verified` follows through `query` — so
  the FE never blocks on an O(history) walk, and a verification failure
  still has a place to be reported rather than vanishing.
- **Recovery is part of the transaction, and it runs FIRST.** Under
  `supervisor.lock`, before pointer discovery, before start-mode
  authorization and before admitting any new command, the authority
  reconciles every ACTIVE journal entry against the world. For `reset`,
  four states and no others: pointer still names the OLD voyage → the
  rename never took, so resume from the beginning; pointer ABSENT with
  the evidence rename present → resume from publication; pointer names
  the INTENDED NEW voyage → the operation succeeded, so reconstruct
  `reset_done {new_voyage}` and make it terminal; pointer names
  SOMETHING ELSE → loud stop, because a second identity has appeared and
  minting a third is the one thing at-most-once forbids. For `end_run`,
  recovery reads the DURABLE MARKER, never a capsule-local latch that no
  longer exists after process EOF: a marker in the leg's own epoch means
  ACCEPTED, so reconstruct `record_closed` and keep the no-respawn hold;
  a leg ended WITHOUT one is the pre-barrier failure, so terminalize
  `failed {record_append}` and release the hold. A terminal reply, a hold
  release, or process exit happens only after the terminal journal state
  is itself crash-durable OR deterministically reconstructible from the
  durable side effect that caused it. The journal reconciles TO the
  marker; it never becomes a second barrier.
- **An ended authority stays serviceable.** A supervisor whose run ended
  by request does not exit the instant it knows: it enters the
  ENDED-NO-RESPAWN phase and keeps serving `query`, `status` and an
  explicit `stop` — which is what lets a lost-reply client learn its
  outcome and what lets upgrade's acknowledged `stop` happen at all —
  and a RESTARTED authority holding a recovered operation serves that
  result before taking the requested-end start-mode exit.
- Every op has one budget: connect 2 s, request write 2 s, reply read
  5 s, and the client may re-`query` on any timeout. A client timeout
  ABANDONS THE CONNECTION, NEVER THE OPERATION — accepted intent is the
  authority's, not the caller's.
- **ACCEPTANCE IS THE MARKER COMMIT, and that is the only respawn
  barrier.** Before it, an `end_run` is `in_progress` and its
  no-respawn hold is PROVISIONAL: if the marker append fails, the
  operation terminalizes `failed {record_append}`, the hold is released,
  and the crashed leg is repaired and replaced — which is correct,
  because no durable evidence of a requested end exists and a later
  `--resume` would reach the same conclusion. At or after the commit the
  end is irrevocably accepted, the journal records it, and no new leg is
  legal whatever happens to the requester. One fact, one barrier: the
  marker IS the acceptance, so the two rules can no longer disagree.

**One authority means one linearized state machine, not one blocking
thread.** The authority admits, latches and answers on a lane-service
path that must stay responsive; slow work — an O(history) open, a kill
and wait, a seal, a `verify_voyage` — is delegated, and only its
COMPLETION re-enters the ordered state machine under the fence. Without
that split, `status` cannot distinguish a busy authority from a dead
one, and an accepted `end_run` could starve behind a whole readiness
cutoff of spawn.

**The no-supervisor path is the same TRANSITION, not the same
CAPABILITIES.** When no supervisor is running, `sot-capsule endrun` and
`sot-capsule reset` acquire the fence and run the same code — but they
do not inherit the challenged process handle a live supervisor holds,
and that handle is what authorizes the invalid-mgmt hard stop. So the
capability matrix is pinned rather than implied:

| the capsule's mgmt lane | what a no-supervisor caller may do |
|---|---|
| healthy | everything: challenge afresh, retain the handle, EndRun, and wait both proofs |
| proven ABSENT | `reset` only, on a classifier ABSENT taken while holding the fence |
| present but invalid | REFUSE LOUDLY, naming the recovery: start a supervisor, or run the explicit recovery procedure. It may never terminate an unauthenticated same-user process, and it may never destroy the pointer while that server lives |

Legs are spawned as CHILD PROCESSES and deliberately NOT placed in the
supervisor's job: the supervisor dying must be harmless to the run,
which is the whole reason adoption exists.

**Discovery, and the two windows a spawn passes through.**

- `<state-dir>\drawer.voyage` names the drawer's voyage: one canonical
  UUID, WRITE-ONCE. The voyage is the SESSION and every capsule run is a
  LEG (ADR 0039's epoch), so a respawn, an adoption and a next-morning
  start all reuse it, and therefore reuse the same pipe name — which is
  why the FE's reconnect target survives a leg boundary it never
  predicted. It is PUBLISHED, not merely created: temp file → write →
  file flush → NO-REPLACE rename → renamed-file flush → parent-directory
  flush, the store's own pinned Windows order, reusing that
  implementation rather than a second one. Publication completes BEFORE
  any spawn, so a pointer can never name a voyage nobody can find. A
  pointer that exists but is empty or not a canonical UUID is CORRUPT: a
  loud stop naming `reset`, never a silent re-mint.
- Absence of the store a pointer names is NOT a licence to re-mint. ADR
  0039 pins absence of data as corruption, always, and "missing" can
  equally be an access denial, a transient I/O failure or an interrupted
  move. `NotFound` is distinguished from every other I/O error and both
  are a LOUD STOP naming `reset`, which — like everything else — runs
  under the authority, proves no live server, and renames the pointer
  aside under a unique no-replace name with a directory barrier, so
  evidence is never overwritten and never reappears reordered.
- `<state-dir>\supervisor.lock` is the fence: the same `fsutil` kernel
  lock the writer fence uses, created `CREATE_NEW` when absent (the
  writer fence refuses to create because bootstrap owns it; this one has
  no bootstrap, and an atomic create stops two supervisors minting rival
  inodes), then opened existing and held for the operation's life.
  Kernel-released on any death, so it is never stale.
- A spawned child passes through exactly TWO windows, and only the first
  is invisible. INVISIBLE: `CreateProcess` → writer-fence acquisition —
  process start plus an open and a `try_lock`, bounded by neither
  history nor I/O volume. VISIBLE: fence held, pipe not yet bound —
  `open_for_writing` takes the fence FIRST and only then enumerates,
  reconciles and rebuilds the dedupe index over all retained history, so
  this window is O(retained history) but a probe can SEE it. The child's
  first act after acquiring the fence is to check the parent-death lease
  it was spawned with (an inherited, synchronizable handle to its
  supervisor); if the lease is broken it releases the fence and exits
  without binding. Because the check is INSIDE the fence, "a delayed
  child might take the fence later" is not a race to bound — the child
  either already holds it, and is visible, or — having found the lease
  broken — releases it and exits. This requires a REAL API SPLIT, not a
  check moved around a call: `open_for_writing` today canonicalizes,
  preflights the volume and flushes two directories BEFORE `lock_writer`,
  so the pre-fence path already performs durability I/O with no bound.
  U1a splits acquisition so the fence and the lease check both precede
  history traversal. What remains invisible is then genuinely small, but
  it is NOT proven small: `CreateProcess` returns before child
  initialization completes and Windows publishes no upper bound, so NO
  NUMBER OF ABSENT OBSERVATIONS proves process absence or
  `image_quiescent`. Any operation needing that proof holds or
  re-acquires a process handle and waits on it. The safety of the lease
  comes from the fence ordering.

**The rule: a run is ENDED BY REQUEST only through an explicit `EndRun`
over the mgmt lane.** A producer that EXITS ends its run intrinsically —
`producer_dead` with the exit status, seal, verify-green — because the
run's program ending is not a teardown anyone requested. EndRun governs
every EXTERNALLY REQUESTED end; nothing else — no exit code, no FE
event, no inference — may request one. Exit codes play no role in run
lifetime: any exit code, an FE crash, supervisor death — all are FE
loss; the capsule is untouched. Exit 75 keeps its ADR 0017 relaunch
meaning and lands squarely in FE loss.

**The transition: `EndRun(reason)`**, invoked by the authority on its own
behalf or on a `end_run` request. EndRun = mgmt `shutdown`, and its state
machine is ONE latch, because an ack is a courtesy and not a
prerequisite:

1. The capsule appends one `run_end_requested {reason}` lifecycle frame
   and fsyncs it. This is the ONLY requested-end discriminator; a leg
   ended by request has one and no other end does.
2. On successful commit the capsule IRREVOCABLY LATCHES EndRun, in the
   same writer-loop step, before the ack is queued. Ack completion only
   accelerates teardown; a stalled ack, a client that stops reading, a
   progress-deadline close or a lost connection cannot unlatch it — and
   because the ack cannot unlatch it, the ack cannot be what STARTS
   teardown either: the latch itself is what drives it, the instant it
   commits, so every one of those loss modes still reaches a torn-down,
   sealed leg rather than a shell running on under a record saying this
   run must never be replaced. The ack remains serviced normally
   throughout (still tracked for its own physical-write completion, the
   ack-grace window unchanged) — a courtesy attempted alongside
   teardown, never a precondition for it.
3. If the append FAILS there is NO REFUSAL FRAME, because the pinned v0
   lane has none to send: every reply tag means success, and inventing
   one would break the compatibility promise that lane exists to keep.
   An append failure is what it actually is — the store this capsule
   exists to write has stopped accepting records — so the capsule takes
   ADR 0039's crash shape: no ack, no marker, unsealed process exit. The
   mgmt client sees EOF; the lane operation terminalizes
   `failed {record_append}` per the acceptance barrier above, releasing
   its provisional hold, and the authority repairs and replaces the leg.
   ONE-FACT-ONE-BARRIER: this failure shape covers the write itself
   failing; if the write succeeds and only a LATER fsync report lies
   (the bytes are genuinely durable, the call merely returned an error),
   a VISIBLE marker is authoritative regardless of what the requester
   was told — reconciliation and the accessor both read the byte, not
   the stale report, and a pessimistic report is safe precisely because
   it can never contradict the request's own intent: the caller wanted
   this run to end.
4. Concurrent requests: the first commit wins and writes the only
   marker; every later `shutdown` is acked without a second marker. A
   request accepted in the final service poll has its ack queued and the
   pipe's disappearance is deferred until that ack completes or a 2 s
   grace expires.

**The marker is a registered ADR 0039 feature, not a new enum value
smuggled into an old segment.** `LifecycleKind` is a closed enum and the
verifier decodes every lifecycle frame through it, so an undeclared new
variant is exactly the authority-changing extension ADR 0039's registry
rule exists to catch. Step 6 registers **`sot.capsule.run-end-requested-v1`**
and enforces it bidirectionally, like the two entries already there:
every segment a step-6 capsule opens DECLARES it — unconditionally, at
segment creation, since a feature cannot be added to an immutable header
later and the marker's timing is not knowable in advance — and a
`run_end_requested` frame in a segment that does not declare it fails
closed. Rollout is two-phase — the READER lands one release before the
writer — but publication order alone is not the guarantee: the writer
may not open a feature-bearing segment until the INSTALLED rollback
target can read one, which "Upgrade and version skew" pins as the gate.

**Three proof terms, defined once and used everywhere below.**
`record_closed` = capsule ack (or latch, if the ack failed) + the pipe
NAME gone + writer fence released. `record_verified` = that plus a green
`verify_voyage` over the retained chain — an O(retained history) walk,
so never inside an interactive wait: the AUTHORITY performs it after a
leg ends, before reporting `record_verified` or exiting 0, and a
failure is terminal. `image_quiescent` = the capsule PROCESS has exited,
which `record_closed` does not imply because `sot-capsule` formats its
summary and exits after `run` returns; only upgrade needs it, and
whoever performed EndRun already holds a challenged handle to wait on.

**Respawn is gated by the typed marker, read from the LATEST LEG AFTER
RECONCILIATION.** `.open` and `.recovering` are states the store already
treats as authoritative — `open_for_writing` enumerates all three before
choosing the next epoch — so a leg hard-killed with its segment still
open is NEWER than the last sealed one and must not inherit that one's
verdict. The decision is taken under the fence, on the latest leg of ANY
state, and a marker governs only its OWN epoch.
`producer_dead.detail.reason` remains a free-form DIAGNOSTIC
and is never a discriminator: the shipped capsule writes it for spawn
failures and for `transport-accept-failed`, the two ends that must be
RECOVERED from.

**Startup authorization is a mode, not an identity.** The launcher knows
whether it is beginning a session or restarting a crashed supervisor, so
it says which; nothing is stamped into a leg, which is what makes this
work for an ADOPTED leg this launcher run never spawned and lets the
post-upgrade restart deliberately begin again.

| supervisor start | latest leg after reconciliation | action |
|------------------|----------------------------------|--------|
| `--start` (an operator launcher run, including post-upgrade and post-reboot) | anything | adopt if live, else spawn |
| `--resume` | a live capsule | adopt |
| `--resume` | open or recovering, no live capsule | RECOVER and spawn a new leg — an unfinished leg is never a requested end |
| `--resume` | sealed, carrying its own `run_end_requested` | exit 0; do not spawn |
| `--resume` | sealed any other way, or no leg at all | spawn a new leg |

A PROVABLE TORN TAIL takes the third row: repairing it is exactly what
`open_for_writing` already does. A COMPLETE CORRUPTION does not, and the
distinction is ADR 0039's, not a convenience — a complete frame with a
bad CRC or an invalid closed enum is loud and operator-acknowledged
only, `SegmentReader` marks nothing but `TornTail` repairable, and
`reconcile_open` propagates every other condition. Treating the two
alike would discard a complete record and seal a prefix, turning loud
corruption into apparently valid history, or spawn a successor over a
chain the store itself refused. So reconciliation aborting loudly is
terminal: the supervisor exits 69 WITHOUT spawning, and only an explicit
operator repair procedure may proceed. This table governs a SUPERVISOR
START only; the no-supervisor
`endrun` and `reset` commands are operator acts under the capability
matrix above and never consult it.

`--start` is the operator act. That is sound because the product is
launched from a desktop shortcut, not an auto-start service: a reboot
cannot authorize a start on its own, and if an automatic launcher
restart is ever added, this row is what must be revisited.

**Respawn is bounded by ONE counter.** `consecutive_unstable_legs`
increments for a leg that never reaches READY — holding the writer fence
AND answering the challenge — however long it took, killed-at-the-cutoff
included, and for a leg that reaches READY and ends inside the STABILITY
INTERVAL. Only a leg outliving that interval resets it; three stop the
loop. Which way a leg was unstable is DIAGNOSTIC — the sealed
`producer_dead` detail, or the child's exit code and stderr tail when the
store never opened — never a second counter.

**Supervisor exit codes are the launcher's contract.** `0` = clean end
(the run ended by request, or a stop was requested) — DO NOT restart.
`69` = terminal (three consecutive unstable legs, a foreign server, a
wedge, a failed `record_verified`) — DO NOT restart; surface it.
Anything else is a crash — restart with `--resume` on the launcher's shipped
1/3/7/15/30 s sequence, at most 5 restarts in 60 s, then stop and
report.

**The machine-teardown ORDER is pinned** (the nightly close): supervisor
stop → EndRun under the fence → `record_closed` → FE close → tunnel
down. The invariant: THE RUN IS ENDED BY REQUEST, AND ITS RECORD CLOSED,
BEFORE THE FE AND THE TUNNEL GO DOWN. "Supervisor stop" is the lane's
`stop` command, not a force kill — the launcher is told to stop first so
it cannot restart what the teardown is about to end, then the caller
waits for process exit and fence release on the fence-acquisition
budget, because a kernel lock's post-kill release has no documented OS
bound. Orphan stop: a classifier ABSENT while holding the fence means
skip EndRun and proceed — a fresh install and a post-crash cleanup must
both work. Every wait is bounded and every timeout is LOUD: a teardown
that cannot reach a capsule it can see STOPS and reports a live session
rather than tearing the tunnel out from under it. The
daemon-detach-before-tunnel property the current script achieves is
preserved by this order and asserted by the rewrite's own check.

**The probe.** One episode, one monotonic deadline, evaluated as a typed
transition table in TWO stages: the owned child is resolved FIRST AND
COMPLETELY, including its own challenge, and the episode deadline
governs only observations that leave nothing to clean up.

**Stage A — an owned spawn attempt exists** (evaluated before any
deadline, because every terminal answer here has a child to dispose of):

| # | observation | class |
|---|-------------|-------|
| A1 | `CreateProcess` failed | SPAWN-FAILED |
| A2 | the child has exited | LEG ENDED |
| A3 | its handle wait returned `WAIT_FAILED`, or the readiness cutoff expired | KILL+WAIT |
| A4 | alive, within cutoff → challenge on its pipe → well-formed `status_ok`, identity matches | READY |
| A5 | alive, within cutoff → any other challenge or connect outcome | PENDING |

**Stage B — no owned spawn attempt** (the deadline is checked first here,
where WEDGED disposes of nothing):

| # | observation | class |
|---|-------------|-------|
| B0 | episode deadline expired | WEDGED |
| B1 | connect ok → well-formed `status_ok`, identity matches | ADOPTED |
| B2 | connect ok → the server's token SID differs, or a well-formed `status_ok` whose pid/creation identity does not match | FOREIGN |
| B3 | connect ok → a complete well-formed frame that is not `status_ok` | FOREIGN |
| B4 | connect ok → undecodable bytes, or a frame over the wire cap | FOREIGN |
| B5 | connect ok → EOF, timeout, read/write error, or a failure of `GetNamedPipeServerProcessId` / `OpenProcess` / `OpenProcessToken` / `GetTokenInformation` / `GetProcessTimes` | PENDING |
| B6 | connect `ERROR_ACCESS_DENIED` | FOREIGN |
| B7 | connect `ERROR_FILE_NOT_FOUND` → writer fence FREE | ABSENT |
| B8 | connect `ERROR_FILE_NOT_FOUND` → fence held, or probing it errored | PENDING |
| B9 | connect `ERROR_PIPE_BUSY`, or any other connect error | PENDING |

Totality holds by construction: A1–A5 partition the owned-child state
including its challenge; B1–B5 partition every outcome of a successful
connect; B6–B9 partition every connect error, with `FILE_NOT_FOUND` the
only one consulting the fence, which keeps it disjoint from the
catch-all. REACHABILITY is a separate property and is now also true —
A4 is reachable because the challenge is attempted while the child is
alive, and A3 is reachable because no deadline precedes it — and it is
asserted by a model test that drives one case per row, not by reading
the list. READY and ADOPTED are the same evidence from different
provenance and both end the episode, as does FOREIGN. KILL+WAIT kills,
waits on its own bound, counts an unstable leg and re-enters; a kill
failure or expired wait is itself terminal, since a child able to bind
later is exactly what this row prevents. SPAWN-FAILED counts and
re-enters. Stage B's WEDGED is terminal with nothing outstanding.

Two capsule-side changes make the healthy window observable rather than
inflating the deadline to fit the code:

- **The pipe NAME disappears before any blocking join.** Today the
  transport is dropped at the very end of `run`, so between the last
  mgmt service point and the seal the pipe is live with nothing able to
  answer, and `PipeServer::drop` then joins an accept thread, a reaper
  and every connection's workers with no deadline. Teardown splits: the
  listener is disconnected and closed so the NAME is gone, which is all
  a prober observes; every worker is then cancelled and the joins share
  ONE 20 s absolute deadline, loud on expiry. The invariant gains a
  sibling: THE PIPE IS
  NEVER LIVE WHILE THE WRITER FENCE IS FREE, and NEVER LIVE WHILE
  NOTHING CAN ANSWER IT.
- **An admitted mgmt connection gets a 5 s idle deadline.** It has none
  today, so four idle clients can hold a healthy capsule at its
  non-watcher cap while every probe is closed without a reply. 5 s sits
  an order of magnitude inside the probe episode. Nothing legitimate
  holds an idle mgmt connection now that the death signal is the
  retained handle. ACTIVE occupancy — four owner clients each sending a
  valid request just inside the interval — is out of scope: they are the
  owner, whom the threat model excludes.

The numbers, pinned here so no implementation invents them. Most are
AVAILABILITY CUTOFFS, not derived success envelopes: Windows gives no
bound for post-kill lock release or for child initialization, and
`verify_voyage` and `open_for_writing` are both O(retained history), so
past these points the honest report is "outcome unknown".

| bound                | value                | role |
|----------------------|----------------------|-----------------------------|
| supported history `B` | MEASURED-GATED `>= measured_cold_boundary × 3`; 60 s today | the ONE free number; every DERIVED row is a formula over it |
| connect              | 2 s                  | `connect_voyage_pipe`'s existing deadline |
| challenge            | 2 s, clamped to the episode's remaining wall time | one `status` answered from memory; retryable |
| probe episode        | DERIVED `= B`, attempts 500 ms apart | the SAME visible O(history) window readiness covers; a shorter probe would wedge on a legitimate walk |
| readiness cutoff     | DERIVED `= B`, from spawn, then KILL + 10 s wait | availability cutoff over the O(history) walk |
| fence acquisition    | DERIVED `readiness + kill wait + 20 s` (90 s today) | must exceed a legitimate readiness hold plus kill and wait |
| anti-flap            | 3 consecutive unstable legs; stability interval DERIVED `= readiness cutoff` resets | a store that cannot open, and a shell that cannot stay up |
| launcher restart     | ≤ 5 in 60 s, on the shipped 1/3/7/15/30 s sequence | a crash-looping supervisor |
| FE quit              | DERIVED `fence acquisition` (90 s today) → "outcome unknown" | availability cutoff; a quit may have to wait out one legitimate fence hold |
| mgmt idle            | 5 s                  | pool squatting |
| teardown aggregate   | 20 s TOTAL after the name is gone, one absolute deadline shared by every join | loud on expiry |
| ack grace            | 2 s                  | a final-poll request still gets its ack |
| health window        | DERIVED `readiness + stability` (120 s today) | the first-boot decision, and the FE's terminal-lane timeout |

**One measured bound, and every other deadline is a formula over it.**
Let **B** be the SUPPORTED HISTORY BOUND — the time a legitimate
`open_for_writing` may take inside the envelope below. B is PROVISIONAL
UNTIL MEASURED and is 60 s today. The table above is the ONE derivation
graph: every DERIVED row is a formula over B, every pinned literal has
a named role, and no consumer restates either.

Probe and readiness are the same number because they bound the SAME
window from two sides: a supervisor that owns the child gives up at
readiness, and a replacement supervisor that owns nothing observes the
identical fence-held/name-absent walk through Stage B. Letting the probe
be the shorter of the two is how a healthy child gets declared a wedge
and then binds anyway, minutes after its authority exited.

Whether 60 s is generous or cruel depends on an `open_for_writing` cost
the ADR may not assert without evidence. So the SUPPORTED ENVELOPE is
named and gated. Claimed: **release build, ≤ 2 GiB retained across
≤ 64 segments, ≤ 100 000 retained input facts, on the two-core Windows
CI runner class, cold cache, with the default AV posture.** A
release-mode test at exactly that boundary is a CI EVIDENCE GATE — and a
COMPOSITION gate, not merely a number publisher, because every other
deadline above is a formula over B. The gate is green only if
`B >= measured × 3` AND every DERIVED row above and every consumer is
recomputed in the same change — the FE's cutoff, the classifier oracle,
the health window, the anti-flap interval, all of them. If the
measurement does not fit, the envelope shrinks or `open_for_writing`
gets faster — its quadratic identity collection is a known cost inside
the envelope and is fixed or measured, never assumed away — and beyond
the envelope the answer is rotation, not a larger constant. Cold cache
is ESTABLISHED, not hoped for: the test writes the boundary voyage,
drops it from the OS cache (a fresh process on a volume whose working
set has been evicted, asserted rather than assumed), and
only then measures the open, so it cannot accidentally time
just-generated hot files.

The teardown aggregate is one absolute deadline, not a per-thread one:
over an acceptor, a reaper, up to sixteen connection workers and the
capsule's own threads, per-thread bounds sum past both the probe episode
and the FE cutoff while the writer fence stays held, so a healthy
bounded teardown would be classified WEDGED. Cancellation is issued to
every worker FIRST; the joins then share ONE deadline, each wait taking
the remaining budget. 20 s sits inside both outer cutoffs with room for
the seal.

**The challenge** is ONE procedure used by every client of BOTH pipe
families, and its ORDER is load-bearing throughout: (1) read the server
pid P with `GetNamedPipeServerProcessId` on the live connection; (2)
`OpenProcess(P, PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION |
SYNCHRONIZE)`; (3) `OpenProcessToken` on that handle and compare its
TOKEN USER SID to this account's — the SID `fsutil` already computes for
the descriptor; (4) only then `status` on the SAME connection; (5)
proven iff the SID matched AND reply-pid == P AND reply creation time ==
`GetProcessTimes(handle)` on the exact FILETIME bits. Nothing in the
reply is trusted, decoded for meaning, or acted on before step 3
succeeds. A dead true server cannot answer; a live one means P is not
recycled; a wrong SID means the object was created by someone else.

The SID step is not decoration, and it is why steps 1–2 alone were never
enough. A DACL is DIRECTIONAL: it governs who may connect to the object
the honest server made, and says nothing about who made the object a
client found. `FILE_FLAG_FIRST_PIPE_INSTANCE` only makes the honest bind
FAIL; it does not evict the process that won. So an other-user process
that creates the predictable name first, and deliberately grants the
victim connect access, can answer a PID/creation-only challenge
perfectly. WHAT THE FULL CHALLENGE PROVES, narrowly: the answering
process is the server behind this connection, alive, with a stable
identity, running as THIS ACCOUNT. It still does not attest an
executable — a same-user process implementing the pinned v0 lane would
pass — and that residue is what the threat model excludes, since it
names other local users and anonymous access, not the owner.

Failure classification is total and no failure is permissive: a
successful token read whose SID DIFFERS is FOREIGN; a failure of
`OpenProcessToken` or `GetTokenInformation` — like a failure of
`GetNamedPipeServerProcessId`, `OpenProcess` or `GetProcessTimes` — is
PENDING, never READY and never ADOPTED. An unproven server is retried or
abandoned; it is never believed.

The retained handle is also the DEATH SIGNAL and the termination
authority for the invalid-mgmt fallback. `MAX_PIPE_INSTANCES` derives
from the protocol's caps (`NON_WATCHER_CAP + SUBSCRIBER_CAP`).

## Upgrade and version skew

`probe`/`status`/`shutdown` ride the pinned v0 mgmt framing — the
permanently compatible lane; attach-protocol versions negotiate via
`hello` above it. The supervisor lane carries a build identity per
connection and refuses a mismatched pair; file replacement is not
process replacement, so step 3 below quiesces the old FE rather than
assuming the transaction did.

**An upgrade is ONE atomic transaction.** The capsule image IS the
supervisor image, so no image can be deferred on its own: "no capsule is
live" does not make the file replaceable, and the applier's binaries,
manifest, junction and rollback are already one transaction in which a
single deferred image can activate a mixed release or roll back over a
running one. The order, and every step's reason:

1. The launcher asks the LIVE supervisor to end the run. It must still
   be alive: it is the process holding the challenged capsule handle,
   and the invalid-mgmt fallback below must be mediated before that
   handle is destroyed. Killing it first would leave a later `endrun`
   process facing a same-user pipe server it cannot authenticate — with
   the mgmt lane itself invalid there is no trusted baseline to
   "re-verify unchanged" against, so it could only terminate blind or
   refuse.
2. Wait for `record_verified` AND `image_quiescent`. The record can be
   closed while the image is still open, because `sot-capsule` formats
   its summary and exits after `run` returns.
3. Stop the supervisor (lane `stop`, acknowledged); wait for its process
   exit and fence release. QUIESCE THE OLD FE in the same step — it is a
   separate long-lived process that a file replacement does not touch,
   and an old FE reaching a new supervisor is exactly the mixed pair the
   lane's build check exists to catch rather than to permit.
4. Apply. 5. Start the supervisor with `--start`, which is what makes
   the fresh leg happen — the requested end just sealed would otherwise
   suppress it.

UPGRADING ENDS THE RUN, on purpose, once, at a moment the operator
chose (see Consequences).

Rollback is ABSENCE-AWARE, FEATURE-AWARE, and ITS OWN QUIESCENT
TRANSACTION. The applier today saves and restores two binaries and
leaves a file alone when it has no `.prev`, so the first release
introducing the capsule would, on a later failure, restore the two old
images and leave the new one — the mixed release this section forbids.
Transaction metadata records prior ABSENCE and restore DELETES new-only
files. But absence-awareness alone is not enough, because a health
rollback runs while the NEW release is live, and a forward apply is
allowed to assume nothing is. The steps are numbered because their order
is the safety:

0. **PREFLIGHT, before ANY release, voyage, process or health-arm
   mutation.** Validate the restore set, the required files, the
   prior-absence metadata and the installed-reader compatibility. Every
   reason to refuse is discovered here, while nothing has been touched —
   which is what makes a refusal costless rather than a half-rolled-back
   machine.
1. Latch no-respawn, then quiesce the new FE — its image is locked while
   it runs, and the forward path's "applier runs before anything is
   spawned" assumption does not hold here.
2. If a live supervisor exists, EndRun or safely hard-stop the CAPSULE
   first, WHILE that supervisor still holds its challenged handle; only
   then `stop` the supervisor, which destroys that capability. Prove
   process exit, a free fence, and every release image quiescent. This
   is the same ordering the forward transaction pins, for the same
   reason: the handle cannot be reacquired by a successor.
3. Only then restore, all-or-nothing, with required-file verification.

**A refusal is TERMINAL, not a retry.** ZERO MUTATION means zero RELEASE
and VOYAGE mutation; recording the decision is not mutation. So a typed
refusal at step 0 atomically transitions `just-applied` from ARMED to a
durable terminal `rollback_refused` state carrying its diagnostics. The launcher recognizes that state on a COLD launch, after
any restart, and does exactly two things: surfaces recovery, and stops.
It does not relaunch the failed release and it does not retry the
rollback. An operator decides what happens next.

The reader hop must also be REAL, not merely published: release
discovery jumps to the newest release, so a machine on a pre-reader
release can skip the reader release entirely and land on the writer,
whose first segment is unreadable by the only target it could roll back
to. So transaction metadata records the INSTALLED target's reader
feature set, and the writer's activation is GATED ON THAT INSTALLED
CAPABILITY: a capsule may not open a feature-bearing segment until the
recorded rollback target can read one. Publication adjacency is not
installation history.

First-boot health covers the supervisor as well as the FE — a
supervisor that exits terminal while the FE stays up would otherwise
never trip it — and it may not COMMIT at first READY, which the
anti-flap rule already counts unstable if the leg then dies. So the
window is the HEALTH WINDOW — `readiness + stability`, 120 s at today's
provisional values and RE-DERIVED WITH THEM — measured from ISSUANCE OF
THE SUPERVISOR `--start` (the transaction's step 5), since U4 starts
the FE only after READY and the stability interval therefore already
covers FE startup. Any rollback observation before commit WINS,
whenever it occurs:

| observation within the window        | decision       |
|--------------------------------------|----------------|
| FE exits abnormally                  | ROLL BACK      |
| supervisor exits 69, or crashes past its restart budget | ROLL BACK |
| supervisor never reaches READY (spawned or adopted) | ROLL BACK |
| FE exits 75                          | not a health signal; the window continues across the relaunch |
| FE alive AND an ACCEPTED, ANSWERING supervisor lane AND the leg READY AND STILL HEALTHY for the stability interval | COMMIT |
| the window EXPIRES with no COMMIT — including an exit-75 FE still relaunching at the deadline | ROLL BACK, subject to the typed refusal above |

The last row is what makes the window absolute rather than extensible: a
relaunch that has not produced a live FE by the deadline is
indistinguishable from one that never will, and leaving `just-applied`
armed forever is the failure mode a total table exists to prevent.

If a security defect invalidates even the mgmt lane, the honest fallback
is hard termination + voyage recovery, executable because adoption
captured a termination-capable handle bound to the live pipe server by
the liveness challenge (re-verified unchanged immediately before
`TerminateProcess`, then a BOUNDED `WaitForSingleObject` on the same
handle — termination is asynchronous and can await pending I/O; a
timeout is a LOUD failure, never an assumed death). Graceful shutdown is
not promised unconditionally. The release pipeline packages the capsule
binary.

## Build order (each step lands green)

1. Promote the state-dir helper. LANDED for the frontend; step 6 moves
   its OWNER to a public `sot-log` helper the frontend delegates to,
   since a frontend-private module cannot be called by the supervisor
   binary and the reverse dependency would invert the architecture.
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
6. The supervisor and the attach-only FE, which ACTIVATE TOGETHER —
   contract in "Lifecycle", "Upgrade and version skew" and this section,
   units below, FE behavior and acceptance in "Step 6 as specified".
7. Acceptance on a real Windows machine — step 6's matrix re-run where
   CI cannot go, plus the rows only a real machine has: multi-user ACL
   denial; logout/login and reboot ACL access; AV rename-retry;
   disk-full; forced-reboot recovery; exit-75 relaunch with the screen
   restored and no ritual; the breakaway-denied degraded path; and the
   NIGHTLY COMPOSITE (supervisor AND FE force-killed AND tunnel torn, no
   EndRun — the capsule survives headless and the next start ADOPTS it,
   never silently).

### Step 6 units

U0 and U1 carry mechanism the contract does not decide; U2 and U3 are
policy. U0 has NO BEHAVIOR; U1a changes ACTIVE step-5 plumbing, U1b
DURABLE compatibility. A supervisor spawning capsules while the FE still
owns its own PTY is two sessions, so U2 and U3 stay off until U4.

- **U0 — libraries, no behavior.** The state-dir rule as a public
  `sot-log` helper the frontend delegates to; `drawer.voyage`
  publication and validation; the same-connection challenge — including
  its target-process token-SID helper — and retained process-handle
  wrappers; the fence primitive with its `CREATE_NEW` bootstrap;
  classifier fault-injection scaffolding.
- **U1 — active capsule changes**, in two separately reviewable slices
  because one of them changes durable compatibility and the other does
  not. **U1a (plumbing):** enforcing the challenge in the shared
  `connect_voyage_pipe` constructor, which is active behavior for
  today's step-5 clients and therefore not U0's; idempotent
  `shutdown_all` so the RAII guard can be disarmed; the 5 s mgmt idle
  deadline; the ack grace; and the `open_for_writing` split that puts
  the writer fence and the lease check ahead of history traversal.
  **U1b (durable):** the `sot.capsule.run-end-requested-v1` registration
  with bidirectional verifier enforcement (amending ADR 0039's own
  displayed registry and lifecycle grammar in the same change, so the
  older normative document does not stay at two entries), the
  reader-first rollout with its installed-capability gate, the marker
  frame and its latch, and the aggregate teardown deadline. Every
  existing test stays; the new ones are the marker, teardown, idle-mgmt
  and feature-boundary rows of the acceptance matrix.
- **U2 — the authority.** `sot-capsule supervise` with its lane, the
  election fence, the classifier, spawn/adopt, the parent lease, start
  modes, the anti-flap counter, `record_verified`, the exit-code
  contract, and `endrun`/`reset` as fence-acquiring in-process callers.
- **U3 — the FE client**, behind the same off-by-default flag: the six
  FE rulings in "Step 6 as specified", checkpoint restore into the
  drawer's parser, and deletion of its DSR responder.
- **U4 — the cutover, one flag.** The launcher starts the supervisor
  with a start mode and the stop handshake, waits for pointer
  publication and READY before starting the FE, and stages the capsule
  on the dev path; the FE defaults to attach-only; the teardown script
  is rewritten; the upgrade transaction and absence-aware rollback land.

Archive membership — the capsule in the artifact, the manifest, the
required-file list, and a `--version` line in the shape the smoke job
asserts — may land before U4's upgrade transaction; apply policy may
not. The DEVELOPMENT build-and-stage path is bound by the same rule: a
dev loop that stages only the frontend runs yesterday's capsule against
today's protocol.

## Consequences

- The drawer session stops dying with the frontend; the resume ritual —
  the single most fragile piece of the FE session lifecycle — is
  deleted rather than repaired.
- OPERATOR NOTE — an intuition inversion: after P3, QUITTING (real quit
  = EndRun) and UPGRADING are what end the drawer's RUN — the voyage and
  session persist; a leg ends — while
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
