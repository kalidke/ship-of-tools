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

### Step 6 as specified (2026-08-28, pre-implementation review)

The build-order line for step 6 compresses seven mechanisms — probe/adopt,
EndRun on real quit, the attach-only FE, the `fe_down` marker,
`initial_command`, packaging, and the `shutdown-sot.ps1` rewrite — and it is
the first step whose callers live OUTSIDE this crate. A pre-code adversarial
review of the work split found that the compression hid one missing durable
fact, two races the pinned prose creates rather than prevents, and one
mechanism (the launcher) that cannot physically host the role the Lifecycle
section assigns it. The rulings, so implementation cannot re-litigate them:

- **The probe needs a NAME before it needs a state table.** The pipe is
  `\\.\pipe\sot-voyage-<id>` and `validate_voyage_id` requires a canonical
  UUID, so the state table presumes the supervisor already knows WHICH
  voyage to probe — and nothing durable names one. Step 6 adds exactly one
  new durable fact, `<state-dir>\drawer.json`, holding
  `{voyage: <uuid>, state: open | ended}`. Only the supervisor ever writes
  `voyage`; the `state` field is discussed two rulings down. It is published
  with the store's own pinned Windows order (temp → flush → rename → renamed
  flush → parent-directory flush) through a REPLACE variant of `fsutil`'s
  existing publish — replace, not no-replace, because a current-value
  pointer's whole purpose is to be overwritten and a no-replace publish
  would need a delete step with a window where no pointer exists. It lives
  in the state dir, NOT under `voyages\`, so the `SE_DACL_PROTECTED`
  descriptor keeps governing exactly the voyage tree. Deletion pressure
  applied and the alternative rejected: enumerating `\\.\pipe\*` answers
  "some capsule exists" in a global multi-user namespace, never "MY drawer's
  capsule", and would make adoption target selection a guess.
- **The session is the voyage; a respawn is a LEG, not a new voyage.** ADR
  0039 already pins epoch = leg = one producer-run attempt by one capsule
  incarnation, and `open_for_writing` allocates max-durable + 1 at every
  open. So the pointer changes only when there is no pointer at all; every
  respawn, adoption, and next-morning start reuses the same voyage id — and
  therefore the same pipe name, which is what lets the FE's reconnect target
  stay constant across a leg boundary it never predicted. Minting is the
  first-ever-start path and the operator's escape (delete the pointer); a
  pointer naming a store that no longer exists re-mints and ANNOUNCES it,
  because the data is already gone and a loud stop over lost data helps
  nobody.
- **The state table is amended: the pipe's own answer is the authority, and
  the lock is consulted only where there is no pipe to ask.** The Lifecycle
  section's four rows read as if `(pipe, writer.lock)` were sampled
  simultaneously. They cannot be — they are two syscalls, and step 5's
  teardown closes the pipe (`shutdown_all`, via the RAII guard) microseconds
  BEFORE the `VoyageStore` drop releases the lock. A supervisor that
  observes "pipe live", then "lock free", across an ordinary capsule exit
  would fire the table's "inconsistent: loud stop" on a completely healthy
  shutdown. As specified:

  | observation                    | required action                    |
  |--------------------------------|------------------------------------|
  | connect ok, challenge PASSES   | ADOPT — never touch the lock (a    |
  |                                | live server answering already      |
  |                                | proves a live writer holds it)     |
  | connect ok, challenge FAILS    | LOUD STOP — a squat or a foreign   |
  | (no reply in bound, malformed, | server; never spawn over it, never |
  | pid or creation-time mismatch) | touch the lock                     |
  | no pipe, lock HELD             | BOUNDED RETRY → visible wedge      |
  |                                | error; never spawn over it         |
  | no pipe, lock FREE             | release the probe lock, SPAWN,     |
  |                                | RE-PROBE to converge               |

  Only `ERROR_FILE_NOT_FOUND` means "pipe absent". `ERROR_PIPE_BUSY` means
  all instances are connected — a live capsule at its cap, which
  `connect_voyage_pipe` already absorbs for 2 s and which must never be read
  as absence. The lock test is `fsutil::lock_writer` itself (not a second
  implementation), acquired and DROPPED before any spawn, since the capsule's
  own `open_for_writing` must be able to take it. The bounded retry covers
  exactly two structural transients, both named so the bound can be chosen
  rather than guessed: a capsule between `open_for_writing` and
  `Transport::bind` (step 5 pinned `bind` as the first fallible step after
  the lock, so this window cannot grow), and `lock_writer`'s own 250 ms
  post-kill release lag. The retry's expiry is a loud stop, never a spawn:
  spawning over a held lock is the one thing a probe must never do. And the
  table is the supervisor loop's RE-ENTRY point, not only its start: an
  adopted capsule that ends puts the supervisor back at the top, where the
  ordinary answer is the fourth row and one new leg.
- **Cross-process adoption is proven by a supervisor that is a different
  PROCESS, which is the step-5 residual this closes.** The step-5 e2e
  asserted `status`-pid against its own pid in-process and named the real
  check as step 6's. The challenge itself is unchanged and its ORDER is
  load-bearing: read the server pid P via `GetNamedPipeServerProcessId` on
  the live connection; `OpenProcess(P, PROCESS_TERMINATE |
  PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE)`; THEN issue mgmt
  `status` on the same connection; proven iff reply-pid == P AND
  reply-`created` == `GetProcessTimes(handle)` on the exact `u64` FILETIME
  bits. Open-before-challenge IS the pid-reuse defense; a later tidy-up that
  reorders it has deleted the property. Step 5 made a `GetProcessTimes`
  failure a loud `Err` rather than a synthesized zero for exactly this
  comparison. What the proving test can and cannot claim, stated rather than
  blurred: it spawns `sot-capsule.exe` as a real child and asserts
  reply-pid ≠ the test's own pid, reply-pid == the server pid, and the
  creation-time match against an independently opened handle; it asserts the
  negatives (a same-named pipe stood up by a non-capsule helper fails; the
  comparison against a handle to a DIFFERENT live process fails). It does
  NOT simulate pid recycling, which no test can force. The property proven
  is substitutability — the challenge fails whenever the (handle,
  creation-time) pair does not match the answering server — and that is the
  whole of what the pid-reuse race needs. The termination-capable handle is
  RETAINED past the challenge: it costs one handle and it is the only thing
  that makes the Upgrade section's mgmt-lane-invalidated fallback
  executable, so it names its invariant and stays.
- **The supervisor holds ONE mgmt connection for the capsule's whole life;
  its EOF is the death signal.** No polling loop, no `WaitForExit` (which an
  adopting supervisor has no handle for anyway), no second mechanism. Step
  5's own admission control makes this legal by construction and the spec
  states the dependency so it cannot be silently removed: `Role::Mgmt`
  carries no deadline (only `Unclassified`/`PostHello` do), keepalive is
  driver-only, and the progress deadline only bites a connection with
  outstanding sends. The cost is one permanent slot of `NON_WATCHER_CAP`,
  which is the budget statement step 6 owes: supervisor 1, the teardown
  script's own EndRun connection 1 (it must never require a live
  supervisor), leaving 2 for pre-hello attaches. Consequently
  `MAX_PIPE_INSTANCES` stops being the harness's invented "single generous
  constant" and is DERIVED and exported from the protocol's own caps
  (`NON_WATCHER_CAP + SUBSCRIBER_CAP`), so the transport ceiling cannot
  drift from the admission control it exists to cover. Related deletion
  pressure, resolved honestly rather than by deleting: mgmt `probe` ends up
  with no caller at all — `status` is strictly more informative and the
  supervisor always needs identity — but the v0 lane is PERMANENTLY pinned,
  so `probe` stays, unused, and is recorded here as unused rather than
  quietly given a job to justify it.
- **The spawn-owner role is one, and it cannot live in the launcher.** The
  Lifecycle section assigns it to "the launcher loop that already outlives
  FE respawns and keeps the ssh tunnel" — a PowerShell process, which cannot
  do overlapped named-pipe I/O, binary framing, `OpenProcess`, or
  `GetProcessTimes`. The role moves to a `sot-capsule supervise` subcommand
  of the binary that already owns the voyage, the pipe, and the wire; the
  launcher's job shrinks to starting it and restarting it with the same
  backoff it already gives the tunnel. Legs are spawned as CHILD PROCESSES
  and deliberately NOT placed in the supervisor's job — the supervisor dying
  must be harmless, which is the nightly-composite row made structural
  rather than tested-for. The invariant that survives the move: exactly one
  process may spawn legs, and it is the one that can also probe.
- **Respawn is bounded; a fast-failing capsule stops the loop.** A
  supervisor that respawns unconditionally is a fork bomb the moment the
  store is unopenable — an omission in the existing prose, not a
  refinement of it. Consecutive legs that end within a short floor with no
  client ever attached increment a failure count; at a small threshold the
  loop STOPS with a visible error naming the last `producer_dead` detail. A
  leg that ran normally resets it. The supervisor's job is to keep ONE
  session alive, not to keep trying forever; an unspawnable voyage is a
  human's problem and must look like one.
- **EndRun ends the SESSION, so `state: ended` is what stops the respawn.**
  The pinned teardown order puts EndRun first, while the supervisor is still
  alive — and a supervisor whose entire job is to replace an ended leg will
  do exactly that, spawning a fresh headless capsule that the next two steps
  then orphan. The same race exists on the FE's own quit path. The
  permanently pinned mgmt lane cannot grow a "this end was requested" field,
  and making the supervisor parse the sealed voyage to learn why would couple
  it to the format for one bit. So the bit lives where the intent already
  does: every EndRun caller flips the pointer to `state: ended` BEFORE
  issuing mgmt `shutdown`, and the supervisor's leg-end handler re-reads it —
  `open` means respawn and announce "run ended — new leg", `ended` means stop
  and announce "session ended". Supervisor start sets `open` unconditionally
  before its first probe (a new launcher run is a new intent to have a
  session) and, finding `ended` with a capsule still live, adopts and
  finishes the interrupted teardown rather than leaving an orphan forever.
- **Step 6 adds exactly three EndRun callers, and the supervisor is not one
  of them.** (a) The FE's real-quit path — `Action::Quit` today sets
  `should_exit` and returns an implicit 0 with no teardown hook, so the
  mgmt round trip is inserted there, synchronous and bounded, before the
  event loop exits. Exit 75 never issues one. (b) `sot-capsule endrun`,
  which flips the pointer and issues `shutdown` in one command so the two
  steps cannot be ordered wrongly by a caller that is a shell script.
  (c) The incompatible-upgrade end-run, offered rather than automatic. The
  supervisor NEVER issues EndRun on its own initiative — not on FE exit,
  not on FE crash, not on tunnel loss, not on its own shutdown. That single
  rule is what keeps "one spawn owner" from becoming "one killer of runs",
  and it is what makes the operator-note inversion true. A quit whose ack
  never arrives fails LOUDLY and does not escalate to termination: a quit
  that cannot reach the capsule leaves a live session and says so, because
  silently promoting "quit" to "kill" exactly when the protocol is already
  misbehaving is the worst possible moment for it. The reason vocabulary is
  pinned so the record is greppable and the three callers are
  distinguishable in `producer_dead`'s detail: `fe-quit`, `shutdown-script`,
  `upgrade-incompatible`; the capsule's internal `transport-accept-failed`
  stays reserved.
- **The FE stops owning a process and becomes a client.** `LocalTerminal`
  keeps its parser, its reader thread and its dead flag, and loses the
  master, the child, the writer and `portable-pty`; `spawn` becomes
  `attach`. `respond_to_queries` is DELETED — the host-facing handshake is
  capsule-side from producer spawn, with zero clients attached, which is why
  step 7 can assert it was answered before anything ever connected. The
  first paint comes from `restore_screen` on the attach checkpoint, never
  from replaying bytes into a blank parser: checkpoint v1 exists precisely
  because reconstruction cannot encode the inactive grid or alternate-screen
  identity, and a `hello` refusal is a loud refusal offering EndRun, never a
  blank screen and never a silent replacement, at every version. The pen:
  attach arrives as a WATCHER always, and the FE takes on the FIRST
  KEYSTROKE, not on attach — one extra round trip, once, on a local pipe,
  and it means a second FE opened to LOOK at a session does not steal the
  pen from the one being typed at. (Auto-take-on-attach was the first
  draft's answer and is rejected for that reason; demote-on-take already
  makes the last taker win when someone actually types.) Resize is
  driver-only and reject-never-clamp, so a watcher renders the driver's
  geometry rather than fighting it — the alternative makes the recorded
  geometry a lie. Scrollback is stated, not papered over: the capsule keeps
  none, the FE accumulates its own from the live stream after attach, so a
  reattach shows a correct SCREEN and an empty history until frame replay
  lands in P4, and the drawer says so once rather than pretending.
- **The FE gains a `sot-log` dependency, and does not gain a new crate.**
  It needs `wire`, `pipe_win::PipeClient`, and the vt100 fork it already
  has; it does not need the store, the verifier, or recovery. A third crate
  to express that is machinery for a dependency edge, so the ruling is the
  plain dependency — with the escape named in advance rather than
  discovered: if the store's transitive deps prove unacceptable in the FE
  binary, the fix is a `client` cargo feature gating the store modules, not
  a crate split.
- **`fe_down` is written by the FE, on attach, and only when there is a
  down-window to report.** The FE is already the inbox's only writer, and
  keeping the marker there means its existence is evidence of exactly what
  it claims: an FE is present now and was not before. It is written
  immediately after the attach completes and before the drawer becomes
  interactive, through the SAME append path as a relayed message. `from` is
  not derivable from FE memory across a crash, so it is read back from the
  inbox — the timestamp of the last line this side wrote; with no such line
  (a first-ever attach) the marker is SKIPPED, because a synthetic zero
  would be a lie about a window that never existed. The line's shape is
  constrained by its readers, not by taste: existing monitors project
  `from`/`to`/`text` out of each line, so the marker is an ordinary
  `{"from","to","text","ts"}` object carrying an `fe_down` discriminator —
  a bare `{"fe_down":{…}}` line renders as nulls in every current reader.
  The invariant, stated because it is the marker's only reason to exist: the
  retired ritual's session-start catch-up was the sole mechanism that ever
  noticed a dropped relay message, and P3 must not make that failure
  quieter. A marker skipped when it was due, or written from the wrong side,
  silently reopens the hole.
- **`initial_command`'s firing rule is about the CAPSULE, so it needs no
  flag.** It fires when a capsule spawns the shell — the supervisor's spawn
  path, including every respawned leg, which is a genuinely new shell — and
  never on the adopt path, because there is no spawn to attach it to. It is
  passed as the capsule's shell launch ARGS (`-NoExit -Command` / `/K` /
  `-c "…; exec $SHELL"`, ADR 0017 §4's own mechanism), never as PTY stdin,
  so the prompt race stays solved and the command is part of the recorded
  spawn rather than a synthetic input frame. `resolve_shell` and the arg
  injection move out of the FE to the supervisor with it. `resume_command`
  is DELETED, not aliased: a stale key in an existing settings file is a
  loud startup warning naming its replacement, because silently honoring it
  would re-run `claude --continue` inside an ADOPTED session — the exact
  ritual P3 exists to retire, now doubled. `--relaunched` keeps only its
  ADR 0017 §1–§3 meaning (open into the drawer) and selects no command;
  today's arming condition additionally fires on a plain start, and collapses
  with the setting.
- **Adoption is announced on BOTH branches.** Two surfaces, both required
  because they fail independently: the supervisor's own append-only log, and
  an FE-visible drawer notice on attach. The start time is `status.created`
  rendered locally — no new field and no new frame, which is the right
  source precisely because the permanent lane cannot grow one. A SPAWN is
  announced too: the operator's failure mode is not knowing which of the two
  happened, and an announcement that fires on only one branch teaches
  nothing.
- **Packaging: ship the capsule, stage it after exit.** `sot-capsule.exe` is
  already built by the workspace release build and simply never copied into
  the stage directory; four sites hardcode the two-binary set (the release
  packaging step, the artifact smoke assertion, the updater's required-file
  list, and the applier's install/restore/rollback loops) and each must
  learn the third, which also means the capsule needs a `--version` line in
  the shape the smoke job asserts. Staging is **stage-after-exit**: the
  staged binary is replaced only after EndRun completes, a running capsule
  is never overwritten in place, and the condition "no live capsule" is
  answered by the probe the supervisor already runs rather than by a second
  mechanism. OUT: an installer, code signing, and capsule auto-update
  independent of the release.
- **The `shutdown-sot.ps1` rewrite is an INSERTION, not a redesign.** The
  script's existing order — supervisor first (a force kill runs no `finally`,
  which is what keeps it from respawning the FE or tearing the tunnel early),
  then the FE, then a bounded drain, then the tunnel — already achieves
  daemon-detach-before-tunnel and is kept verbatim. Step 6 prepends the
  EndRun step and extends "supervisor" to include the capsule supervisor
  process. The pinned order therefore reads: `EndRun → capsule ack + seal +
  lock release → supervisor stop → FE close → tunnel down`, with proof of
  completion being capsule ack + pipe closure + verify-green + lock release,
  never `WaitForExit` (the script has no child handle either). Every wait is
  bounded and every timeout is LOUD: a script that cannot reach the capsule
  STOPS and reports a live session rather than proceeding to kill the
  supervisor, which is the exact strand the reorder exists to prevent. Orphan
  stop is defined: no pointer, or pipe absent with the lock free, means skip
  EndRun and proceed — a fresh install and a post-crash cleanup must both
  work. The daemon-detach property is ASSERTED by the rewrite's own check,
  not inherited by assumption.

**Contradiction sweep** — where the existing prose and step-5 reality
disagree, named and amended rather than worked around:

1. The probe table's implied simultaneity, and "pipe live + lock free →
   inconsistent" as a SAMPLING rule. Amended above: as a sampling rule it
   fires on healthy shutdowns; as a SQUAT rule ("the pipe answers but fails
   the challenge") it is exactly right, and that is what it now says.
2. The Lifecycle section's teardown order races the supervisor it does not
   mention. Amended by `state: ended`, above — the order itself stands, and
   step 7's row keeps its wording because the flag is what makes "the run
   ends before any process dies" safe rather than merely stated.
3. "ONE spawn owner — the launcher loop." The launcher cannot perform the
   probe. The ROLE is one; its home moves into `sot-capsule supervise`.
4. Build-order line 1 is discharged for the frontend but not for the rule:
   the FE's `paths.rs` is now the single Windows `%LOCALAPPDATA%\sot` site,
   and step 6 must not add a fifth copy for the capsule — the rule is
   promoted into `sot-log` and the FE delegates. Two copies stay OUT of
   scope and are named so they are not mistaken for discharged: the
   backend's own state dir (deliberately different, Linux-side, daemon-
   private) and the FE's roaming `%APPDATA%\sot\state-<host>.toml`
   persistence path, which is still capable of the exact split line 1 was
   written to kill.
5. `MAX_PIPE_INSTANCES = 8` is carried in the harness bin as an admittedly
   invented constant whose real computation "step 6/7's supervisor owns".
   Step 6 owns it and derives it.
6. The Lifecycle section forbids `WaitForExit` while the Upgrade section
   requires a bounded `WaitForSingleObject`. Not a contradiction, and pinned
   so nobody resolves one against the other: the prohibition is on using
   process exit as the PROOF OF ENDRUN; the bounded wait applies after
   `TerminateProcess`, on a handle whose identity the challenge proved.
7. ADR 0017 §4 is marked retired in its own header; step 6 is the change
   that makes that true in code, by deleting `resume_command` rather than
   superseding it in prose.

**Units** (build order; each is one PR):

- **U0 — the capsule becomes a real, spawnable, findable program.** The
  Windows CLI grows `supervise` and `endrun` alongside `run`, plus
  `--version`; the state-dir rule is promoted into `sot-log` and the FE
  delegates; `<state-dir>\drawer.json` gets its publish/read/validate with
  the default voyage root `%LOCALAPPDATA%\sot\voyages` (the step-4 gate
  assigned this default to the step-6 spawn owner); `MAX_PIPE_INSTANCES` is
  derived from the protocol caps. Evidence: both Windows legs start the
  binary as a child process and complete an attach plus an mgmt round trip
  against it; pointer tests for absent, malformed (loud stop, never a
  guess), and stale-store (re-mint, announced). First because everything
  else needs an out-of-process capsule to test against.
- **U1 — the probe and the cross-process adoption challenge.** The amended
  state table, the challenge, the retained termination handle. No spawning.
  Evidence: the challenge matrix above on both legs, including both
  negatives; a loop that spawns and probes repeatedly under an output flood,
  asserting zero false wedges and zero double-spawns. This is the unit that
  discharges the step-5 residual, and it lands before anything depends on
  adoption.
- **U2 — `sot-capsule supervise`.** The loop, spawn with `initial_command`,
  adopt, the breakaway attempt that produces `Survival`, the held mgmt
  connection, announcement on both branches, `state`-gated respawn, and the
  anti-flap bound. Evidence: hard-kill a capsule and watch a new leg appear
  on the SAME voyage with a new epoch, both legs verify-green; two racing
  supervisors converge with the loser converging on the winner; a capsule
  spawned by a supervisor that has since exited is adopted by a fresh one.
- **U3 — `endrun`, the script rewrite, and the launcher hosting.** The
  reason vocabulary, bounded waits, loud failures, orphan stop, the
  preserved detach property, and the launcher restarting `supervise` the way
  it restarts the tunnel. Evidence: a scripted teardown that ends the run
  before the FE and tunnel go down, asserted on the voyage (sealed,
  verify-green); the negative — an unreachable capsule stops the script
  loudly and leaves the voyage open.
- **U4 — the attach-only drawer.** Client, checkpoint restore,
  take-on-first-keystroke, driver-only resize, the "run ended — new leg"
  UX in place of "process exited", the adoption/spawn notice, and EndRun on
  real quit. Evidence: attach fidelity against a real capsule through the
  FE's own parser (restore-oracle-verified), including alternate-screen and
  a resize between detach and reattach; the version-skew refusal path. The
  largest unit; if it must split, the seam is client-transport versus
  drawer-integration.
- **U5 — the `fe_down` marker and the settings migration.** Marker shape,
  `from` derivation, the skip rule; `initial_command` in, `resume_command`
  out with a loud warning on a stale key. Evidence: unit tests on the
  derivation and the skip; a settings-migration test.
- **U6 — packaging.** The four sites, the smoke assertion, and the
  stage-after-exit gate. Landable any time after U0.

Any new Windows test binary must be added to the `windows-2022` job
explicitly — the workspace run on `windows-latest` will execute it, but only
the older image exercises the blocking `ClosePseudoConsole` that steps 4 and
5 both learned their real lessons from.

**Explicitly OUT of step 6**, deferred where the ladder already puts them:
every row of step 7's real-machine matrix (step 6 builds the callers those
rows need — multi-user ACL, logout/reboot, AV, disk-full, forced-reboot
recovery, and the nightly composite are step 7's to RUN); frame replay and
pre-attach scrollback (P4); remote attach (P4 — the pipe stays local and
keeps rejecting remote clients); Sessions-mode rows for FE voyages and the
catalog; the Claude SDK adapter on Windows (the drawer stays a raw-terminal
voyage); the fe-inbox down-window itself, whose DETECTION is all that is
built here; any change to the merged Linux capsule path; multi-FE pen
arbitration beyond what demote-on-take already gives; voyage rotation
policy for a long-lived drawer voyage (measured here, decided in P4); and
broker workarounds for the breakaway-denied path, which stays DEGRADED,
surfaced, and unworked-around.

**Open risks, each with the test that settles it:**

1. *Cross-process adoption* — the step-5 residual. Settled by U1's matrix on
   both legs. The pid-RECYCLE race stays unsimulable; what is proven is
   substitutability, and the spec says exactly that rather than overclaiming.
2. *Probe transients on a loaded machine* — the spawn-to-`bind` window and
   the post-kill lock-release lag are both timing-dependent on two-core,
   contended runners that already forced step 5's suite to serialize.
   Settled by U1's repeated spawn/probe loop under flood, and by stating the
   retry bound as a number with the two transients it must dominate.
3. *The held mgmt connection as the death signal* — it depends on
   `Role::Mgmt` never acquiring a deadline. Settled by an explicit
   regression test that an idle mgmt connection outlives the pre-admission,
   keepalive, and progress deadlines combined, so a future timeout added to
   that role fails loudly here instead of silently blinding the supervisor.
4. *Leg startup is O(retained history)* — `open_for_writing` seeds the
   dedupe index from the whole retained voyage (step 5's own stated
   unbounded-memory note), and this voyage is meant to live for weeks across
   many legs. Settled by a measurement test that builds a voyage with a
   realistic input count and pins open time and index memory to a budget;
   exceeding it is a P4 rotation decision, not a step-6 build.
5. *A quit that cannot reach the capsule* — the no-escalation rule
   deliberately leaves a session alive rather than killing it under protocol
   failure. Settled by U3's negative test and then by dogfooding judgment;
   if it proves wrong, the fix is a second, explicitly operator-confirmed
   force path, never an automatic one.
6. *Checkpoint restore through the FE's own parser* — step 5 proved restore
   against a test oracle; U4 is the first time the live drawer, with its own
   geometry and resize history, is the restorer. Settled by U4's
   attach-fidelity test including alternate-screen and a resize across the
   detach.
7. *A new durable fact can be stale, corrupt, or absent* — settled by U0's
   three pointer tests, and by the ruling that a pointer naming a missing
   store re-mints with an announcement rather than wedging over data that is
   already gone.

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
