# ADR 0037: The Ship's Log — session substrate (voyage / capsule / catalog / bridge)

**Status:** Accepted (2026-08-23). Phased delivery: P0 ships first as ADR 0038 (tmux
keeper unit — immediate fire extinguisher, zero new architecture); the substrate
itself (P1+) lands incrementally as a new crate in the existing Rust workspace.
**Date:** 2026-08-23

> Design provenance: converged over seven adversarial design rounds between the
> maintainer's Claude session and an external critic model (Codex), with the
> requirements deliberately held open. Three successive shapes were proposed and
> killed for cause — a central session daemon (blast radius: upgrading it kills every
> session, the exact failure class that motivated the work), then an "immortal
> per-session scribe" (a sole PTY holder cannot crash without ending interactivity,
> and frozen code preserves vulnerable parsers forever) — before the third
> decomposition below survived attack. Final verdict: "the decomposition and failure
> boundaries are sound."

## Context

A session today *is* a running process. Claude/agent sessions live in tmux panes on
the backend host; the Julia REPL is a child of `sotd`; a frontend-drawer session is a
child of the frontend. Consequences, all observed in production:

- A daemon restart can kill every tmux session on the host (root cause and immediate
  fix: ADR 0038). The fleet *behaves around* this fragility — daemon upgrades are
  avoided, session bootstrap requires a multi-step re-arm ritual after every restart.
- The REPL's heap (loaded packages, compiled state, workspace, GPU allocations) dies
  with every daemon bounce.
- Frontend relaunches kill the session driving them (the ADR 0017 sentinel dance
  exists to manage this).
- Session history is scrollback: partial, per-machine, and gone on restart. "What did
  that agent do last Tuesday" is unanswerable.

Meanwhile the ambition is larger than keep-alive: a **system of record** for a
person's entire agent fleet over years — every session permanently replayable,
forkable, and searchable; sessions that move between hosts; producers beyond
terminals (headless agents, REPLs, long simulations, instrument acquisitions); and
surfaces beyond the TUI (later: chat, voice, mobile viewports).

## Decision

### The axiom

**The log is the session; the voyage is immortal, the sailor is not.** Processes
crash, respawn, and move; the hash-linked record is the identity and the truth.

The system in three sentences: a **Voyage** is a stable session identity over
immutable, hash-linked segments and artifacts, while **Capsules** exclusively own
individual producer incarnations behind fenced local sockets. A rebuildable per-host
**Catalog** discovers and launches Capsules, and remote clients reach them only
through an SSH-executed **Bridge** carrying a bounded, versioned envelope. Non-leaf
failures never kill executions, leaf failures affect at most one incarnation, and
committed history survives every process crash.

Features that become corollaries rather than roadmap items: keep-alive (the log
persists), replay (read from a sequence number), hibernation (a voyage with no live
incarnation), migration/promotion (move a sealed head; start a new incarnation from
an adapter checkpoint — never a promise of generic live process migration), forking
(a ref recording a parent `(voyage, seq/hash)` — no copying), audit (the log *is* the
record), crash recovery (the state was never anywhere else).

### Invariants (the design test)

1. Every non-leaf service can crash or upgrade without killing executions.
2. A resource-owner failure affects at most one incarnation.
3. Committed history survives every process crash.
4. Planned leaf upgrades use drain/handoff where the adapter allows it, else an
   honest one-incarnation restart — a security fix may end an incarnation, never a
   voyage.

### Concepts

- **Voyage** — UUID + DAG of immutable, hash-linked segments + content-addressed
  artifacts + refs (`main` only in v1). Advancing a ref requires a durable generation
  and compare-and-swap on the previous head; with no branch machinery in v1 a
  competing writer **fails closed** (conflict-branches arrive with fork support).
- **Envelope / frames** — tagged typed records (`input | output | resize | state |
  exit`, rich kinds `turn | tool | artifact`); unknown kinds are skipped AND
  preserved; length + checksum + seq + explicit durability points; every allocation
  bounded. **Committed means:** input is logged durably before it reaches the
  producer; recovery truncates only to the last validated boundary; artifacts are
  hashed and durably published before anything references them. **Raw input payloads
  are redacted by default** (opt-in per voyage) — no-echo secrets must not become
  plaintext history. Disk-full stops input AND bounds producer output, visibly.
- **Capsule** — one process owning one incarnation: the producer resource, the
  current segment, and the takeover actor. Mutating frames carry
  `(voyage, incarnation, controller_epoch, input_seq)`; attach is **observe** or
  explicit **take** — takeover increments the epoch before acknowledgement and
  revokes the old controller; auto-reconnects re-observe, never steal; the writer
  exclusively owns resize. Input acks are `{op_id, seq}` = "sequenced", never
  "consumed"; reconnects resend idempotently. **Birth is transactional** (publish
  only after manifest, initial segment, endpoint, containment object, and producer
  readiness are durable; fsync/atomic-replace per OS). **Death is precise** (revoke
  input; kill and await the exact cgroup/job/process identity — the capsule owns its
  own containment object, since reaping must not depend on the crashable Catalog;
  seal the valid tail; tombstone; the voyage log is always retained). Producer
  adapters: PTY (`portable-pty` + derived VT checkpoint `{through_seq, grid}`) first;
  agent SDKs (Claude Agent SDK streaming/interrupt/resume-fork; Codex app-server when
  stable), the Julia REPL's existing typed-frame shim, telemetry samplers, and
  instrument acquisitions as peers. **Never infer turns from bytes.**
- **Catalog** (per host) — non-authoritative and rebuildable from the store:
  discovery (readdir + lifetime-lock liveness; never probe-timeout reaping), capsule
  launch, orphan sweep, search/location/checkpoint indexes, retention *execution*. It
  may never infer deletability from its own index; packs contain original segment
  bytes with a store-resident manifest published and verified before loose copies
  disappear. Its crash or upgrade is a non-event.
- **Bridge** — remote attach: `ssh <host> sot-bridge <id>`, no PTY, stdout reserved
  exclusively for frames, version negotiation, backpressure and half-close preserved,
  identifiers and local peer credentials validated, stale incarnation/epoch traffic
  fenced. Resolves the active endpoint without needing the Catalog alive.

### Identity-complete, semantics-small

The v1 record schema MUST carry: voyage/incarnation/segment identity, adapter and
host provenance, format/hash versions, predecessor and causal-parent identity, ref
name, record sequence + commit semantics, artifact hashes, ref CAS — because identity
is the one thing an archive cannot retrofit. v1 EXCLUDES: HLC timestamps (a future
*tagged* timestamp field beats a reserved slot), global indexing, branch UX,
timelines, cross-log ordering. Fleet history is a partial order, forever.

### Two tiers

**Rich tier** = cooperative producers driven through their real control APIs,
emitting turn/tool/artifact frames. **Dumb tier** = arbitrary TUI processes:
keep-alive, grid attach, byte-level record only. Turn semantics are never inferred
from the byte stream (a known tar pit).

### Topology and security

A **star, not a mesh**: each frontend host dials each backend host directly from
`hosts.toml`; zero backend-to-backend protocol and zero distributed state (a BE-BE
link is added only if the existing relay path is retired AND cross-host agent control
with all frontends offline is actually needed). **Local sessions are private by
topology** — frontend hosts accept no inbound dials; *promotion* (move a sealed head
to a backend) is what publishes work. Security is subtraction: SSH is the only
network boundary (no listening ports, no tokens, no PKI, nothing to rotate); locally,
owner-only Unix sockets / owner-DACL named pipes; state in an explicitly validated
owner-only, non-NFS per-host `state_root` (a shared `$HOME` does not make
"host-local" automatic); plain files a human can audit. Target platforms: Linux,
macOS, Windows 11.

### Retention (shape agreed; details open)

Per-voyage **retention class, declared at birth**, executed by the Catalog under the
never-silently-delete rule: **archive** (pack full-fidelity segments to bulk
storage), **discard** (ephemeral; birth-time declaration lets the capsule skip
durability costs entirely — also the sensitive-work tier), **distill** (an LLM reads
the sealed voyage and writes a summary; bytes age out, meaning survives; the summary
is an artifact frame whose causal parent is the sealed head, so distilled voyages
keep their place in the DAG). Open: default class per session kind; distillation
ownership; pack layout; interaction with input-redaction opt-in.

## Phases

- **P0** — stop the daemon-restart massacre with a tmux keeper unit (ADR 0038;
  zero new architecture, immediate value).
- **P1** — capsule + store (Linux, PTY adapter): hash-linked segments, transactional
  birth/death, observe/take with epochs, replay, checkpoints. **Gates:
  crash/fault-injection tests and a store verifier** — the frame codec and segment
  format are read forever, so they get the care first. Catalog = readdir + locks.
- **P2** — rich producer: Claude via the Agent SDK (headless legs need no PTY).
  Codex/Julia adapters follow as their upstreams stabilize.
- **P3 / P4** (order swappable) — FE-local capsules one OS at a time (macOS →
  Windows; detached spawn on first FE launch; contract: survives FE crash/relaunch,
  not logout/reboot; retires the ADR 0017 dance) / `sot-bridge` star attach (retires
  relaunch-to-switch-host, ADR 0015).
- **P5** — cutover and growth: `sotd`'s pty layer moves to capsules behind a rollout
  switch; **the tmux path is deleted only after a soak period with tested rollback**;
  Catalog search lands off every critical path; the machine monitor (ADR 0020)
  becomes a sampler capsule (history survives daemon restarts; no cross-host SSH
  sampler children).
- **Later, as DAG corollaries:** fork/branch UX, promotion, fleet timelines,
  inter-session comm riding the envelope, gateway + chat/voice/mobile viewports —
  each a client, each a separate decision.

## Consequences

- Restarts and upgrades of everything non-leaf become routine; the fleet stops
  behaving around fragility.
- The REPL keeps its heap across daemon restarts; frontend relaunches stop killing
  their own sessions; disconnected viewers lose nothing.
- Session history becomes queryable ground truth — provenance for outputs, an
  accountability layer for autonomous agents, and the substrate the Outputs/Agents
  modes were waiting for.
- Costs, named: the store engineering (fsync discipline, checksums, epoch fencing,
  fault harness) is real work that renders no pixels; tmux and capsules coexist for
  a transition period (the provenance glyph distinguishes them); archives now live on
  local disks and therefore need an explicit backup/pack tier; process count rises
  (N capsules + catalog vs one tmux server) and observability must keep up.
- Session data becomes a concentrated asset: `state_root` is treated like a vault for
  backup purposes; redaction-by-default and the discard class exist for exactly this.

## Deferred / research

Transactional turns; steering queues beyond synchronous ok/error; fork at older
turns (filesystem anchoring via worktrees/snapshots); fleet consistency epochs; live
job extraction; drain/handoff for capsule upgrades; conflict-branch creation.

## Naming

The nautical term and the technical term coincide — a ship's log is the durable
record of a voyage — and the project is Ship of Tools, steered from a wheel. Session
= **voyage**; incarnation = **leg**; the substrate is **the Ship's Log**. Component
binaries: `sot-bridge` (remote attach); capsule/catalog binary names are fixed at P1.
