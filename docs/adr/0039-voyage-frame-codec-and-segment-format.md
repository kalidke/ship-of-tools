# ADR 0039: Voyage frame codec and segment format (Ship's Log P1, v1)

**Status:** Accepted (2026-08-24). This is the permanent on-disk contract for
ADR 0037's voyages — the two artifacts everything reads forever. Implemented
by the `sot-log` crate; gated by golden fixtures (Rust writes / Julia reads)
and a crash/fault harness before anything builds on it.
**Date:** 2026-08-24

> How this was designed: a producer-semantics spike over three real producer
> surfaces (Claude Agent SDK, Codex app-server, our Julia REPL shim) distilled
> eighteen requirements; then four adversarial review rounds against this
> document's draft, each carrying BOTH pressures — find holes AND delete
> machinery ("as simple as possible, but no simpler"). The rounds removed
> roughly twenty mechanisms while the verdict moved to converged. v1
> deliberately does LESS than the first draft: it never deletes anything,
> emits no forks, has no packs, no compression, no encryption — each of those
> returns later through an explicit seam described here.

## Decision, in three sentences

A **voyage** is one linear chain of append-only **segment** files, each a run
of checksummed **records** (one header, frames, then a seal) whose seals form
a hash chain. A **frame** is a thin identity-complete envelope — sequence,
source, time, typed references — around either one of seven normalized
control classes or an opaque producer payload carried in its native form.
Everything else (catalog, viewers, search) is derived and rebuildable; the
segments are the only truth.

## What v1 guarantees (and what it doesn't)

- **Committed before seen**: input is durably logged before the producer
  sees it; an acknowledged input can never be lost.
- **Nothing observed can un-happen**: a frame reaches any watcher only after
  the fsync that covers it. Live readers never tail open files — they receive
  frames from the writing capsule; offline readers read sealed segments.
- **A crash costs at most the unfinished tail** of the open segment, and only
  a *provably* torn tail is ever discarded; every other defect is loud.
  Sealed history is never auto-repaired.
- **One writer**: a kernel-held lock fences capsules; a monotonic take-epoch
  fences controllers; a reconnecting controller is always a watcher.
- **Redact by default** is a checkable wire property: inline input bytes are
  illegal unless a capture opt-in frame precedes them in the voyage.
- **Never silently delete** is satisfied trivially: v1 deletes nothing — no
  GC, no retention deletion, no packing. Those arrive as designed features
  with their own ledger; until then absence of data = corruption, always.
- NOT claimed: tamper-*proofing* (the seal chain is consistency-checking —
  reordering/splicing/mid-edits are detectable; a signed anchor is future
  work), cross-voyage ordering (only causality via future fork pointers), and
  turn semantics for raw terminals (they get none, by design — we never
  guess turns from bytes).

## Identity

- **Voyage** = UUIDv7. One linear chain of segments. v1 headers have no
  parent field: every v1 voyage is a root. Fork emission returns later with a
  header version + required-feature; nothing in v1 bytes changes.
- **Epoch = leg = one producer-run attempt** by one capsule incarnation, with
  at most one admitted spawn. Producer death seals the segment and bumps the
  epoch. Zero-run epochs (crash before spawn) are legal. Live-producer
  handover is out of scope for v1. **"Producer" names a logical lifecycle
  role, not necessarily the innermost process** (amended 2026-08-24, ADR
  0040): an adapter-owned wrapper (e.g. the Claude SDK helper) may be the
  epoch's producer, with the processes it drives classified as internal
  query executions; lifecycle frames refer to the role-holder. Provenance is
  unaffected — wrapper-originated policy frames remain `emitter: adapter` /
  `synthetic`; the role does not launder machinery into producer-native.
- **Epoch allocation**: under the writer lock, `max(epoch across durable
  segment headers) + 1`. An epoch exists once its first segment header and
  directory entry are fsynced; reuse after a pre-publication crash is
  harmless (no frame identity existed). There is no separate counter file —
  the segments are the ledger, and nothing is ever deleted in v1.
- **Frame id** = `(voyage_id, epoch, n)`; `n` starts at 1 per epoch, is bound
  when the frame's bytes are first appended, and continues across segment
  rotation. **Retained frames are contiguous per epoch**: a torn tail frame
  never became an identity, so there are no sequence gaps.
- **Turn** = the frame id of its `turn_open`. Turn-scoped frames carry
  exactly one `caused_by` reference to it. Whether an opaque producer frame
  *ought* to be turn-scoped is an adapter-conformance question, not a generic
  verifier claim.

## Frame envelope and classes — normative schema

Grammar (CDDL-flavored; `?` optional). Absence means "not applicable"; `null`
appears only where written. Unknown JSON object members are ignorable;
duplicate keys are invalid.

```
Ref        = { epoch: u53, n: u53 }     ; same voyage; MUST resolve to an
                                        ; already-committed EARLIER frame
Actor      = { kind: "controller"/"producer"/"adapter_policy"/"foreign"/"unknown",
               controller_id?: str128, take_epoch?: u53 }
BlobRef    = { algo: "sha256", digest: hex64, length: u53, media_type: str }
Digest     = { algo: "sha256", value: hex64 }

Envelope = {
  seq:      { epoch: u53, n: u53 },
  class:    "input"/"turn_open"/"turn_close"/"control_exchange"/
            "artifact_ref"/"lifecycle"/"producer_attached"/"producer",
  source:   { emitter: "producer"/"adapter"/"capsule",
              actor: Actor, derivation: "native"/"synthetic" },
  t_wall_ms: i53,                       ; capsule wall clock; attribution only
  t_mono_us: u53,                       ; process-relative; epoch-comparable only
  stream?:  { cell: str, mode: "append"/"replace", complete: bool, prev?: Ref },
  transformed?: { ops: [ + { op: "redact_field"/"extract_blob",
                             path: str, note?: str } ] },
  refs:     [ * { kind: "responds_to"/"caused_by"/"revises"/
                        "duplicate_of"/"attached_to", frame: Ref } ],
  payload?:     ClassPayload / ProducerPayload,
  payload_ref?: BlobRef & { encoding: "bytes"/"json-utf8" }
}
; Exactly ONE of payload / payload_ref per frame. payload_ref is
; PRODUCER-CLASS ONLY (amended with the wiring PR): control-plane payloads
; carry cross-field obligations — the take matrix, the WAL lattice,
; locator-must-declare — that a spilled body would move out of the
; verifier's inline walk; producer bodies are the only payloads that grow.
; Writer validation and the verifier enforce this identically.
; ProducerPayload (class="producer"): any JSON value conforming to the
; encoding atoms below (null allowed). A native body that cannot conform
; (integers > 2^53-1, non-UTF-8, ...) rides payload_ref: "json-utf8" when it
; parses under these rules after dereference, else "bytes" (never parsed).

input      = { idem_key: hex32,         ; 128-bit random, voyage-scoped;
                                        ; the key IS the request identity
               content: "redacted" / { inline: str } / { blob: BlobRef },
               producer_echo_key?: str }
turn_open  = { admitted_by: str, native_marker?: str, run_label?: str }
turn_close = { reason: "producer_done"/"terminal_res"/"interrupted"/
                       "failed"/"synthesized_death" }
             ; the turn is the envelope's caused_by ref, not repeated here
control_exchange = { phase: "request"/"response"/"outcome",
               to?: Actor, kind_ns: str,
               scope?: str, target?: str, precondition?: str, body?: any }
artifact_ref = { blob: BlobRef, origin?: str }
lifecycle  = { kind: "producer_spawn"/"producer_ready"/"producer_dead"/
                     "take_state"/"capture_optin"/"input_fact",
               take?: { take_epoch: u53, holder: str128 / null },
               fact?: { input: Ref,
                        fact: "forward_intent"/"forwarded"/
                              "producer_observed"/"refused_stale_epoch",
                        intent?: Ref } }
producer_attached = { producer_kind: str, version: str, schema_hash?: hex64,
               profile_def: { id: str128, sha256: hex64, rules: object }
                            / { blob: BlobRef },
               native_session?: object }
```

**Cross-field matrix** (required ⊕ forbidden; verifier-decidable):

- `Actor.kind=controller` ⇒ `controller_id` and `take_epoch` required; both
  forbidden for every other kind.
- `lifecycle.kind=take_state` ⇒ `take` required, `fact` forbidden;
  `input_fact` ⇒ `fact` required, `take` forbidden; other kinds ⇒ both
  forbidden. `fact.intent` is required for `forwarded`/`producer_observed`
  (it names the forward_intent it completes).
- `control_exchange`: request ⇒ `to` required, `responds_to` forbidden;
  response ⇒ exactly one `responds_to`, `to`/`scope`/`target` forbidden;
  outcome ⇒ `scope` and `target` required, `responds_to` forbidden.
- Seal recovery metadata is all-or-none.
- A frame with `stream` or `transformed` must resolve an `attached_to`.
- Exactly one non-`duplicate_of` turn_close per turn; later closes carry
  `duplicate_of` → the winning close.
- Producer frames carry exactly one same-epoch `attached_to` → their
  `producer_attached` frame.

**Input WAL + dedupe** — order: `input` (fsync) → `input_fact:forward_intent`
(fsync) → forward syscall → `forwarded` → `producer_observed`
(echo-confirmed). Legal chains per idem_key, exactly: {input} ·
{input,intent} · {input,intent,forwarded} · {input,intent,forwarded,observed}
· {input,refused}. Anything else is verifier-loud. Deterministic retry: chain
= {input} ⇒ a same-key retry MUST re-attempt (new intent, same input
identity); chain ends at intent ⇒ delivery unknown, MUST NOT auto-retry;
otherwise return the recorded outcome. The authoritative corpus is the whole
retained voyage; keys never expire in v1.

**Capture opt-in** — initial state is capture-off; `capture_optin` is a
one-way enable; `{inline}`/`{blob}` input content is legal only after one is
committed. Redact-by-default is thereby machine-checkable.

**Streams (delta/snapshot/replace)** — `cell` is scoped to the frame's
`attached_to` (attachment ⊕ cell = state-cell identity). Every non-first
same-cell frame carries `prev` → its immediate cell predecessor. `append`
with `complete:true` is illegal; `replace`+`complete:true` is a full snapshot
superseding the chain behind it; `replace`+`complete:false` partially
overwrites the head.

**Take** — every `take_state` transition strictly increments `take_epoch`
(implicit initial state `{0, null}`). Every new writer epoch commits
`{holder: null, take_epoch > prior}` before any producer-bound action — a
reconnecting controller is always a watcher, on the wire. A
controller-originated producer-bound write must match both current take_epoch
and holder immediately before the forward syscall. `adapter_policy`/`capsule`
actors are exempt from take gating (machinery, not typing) but always
recorded.

## Encoding atoms (normative)

- `u53`/`i53`: JSON numbers, |v| ≤ 2^53−1, shortest decimal form, no
  exponent, no leading zeros, no `-0`. `hex32`/`hex64`: lowercase hex of
  exactly that length. `str128`: UTF-8 ≤ 128 bytes. UUIDs: RFC 4122 lowercase
  hyphenated. Strict UTF-8, no unpaired surrogates.
- Digests are computed over wire bytes as written — never over re-serialized
  JSON. There is no canonical-JSON machinery anywhere in this format.
- Seal-digest preimage: `"sotseg1.seal\x00"` ‖ header record bytes ‖ all
  frame record bytes ‖ the seal record bytes with two in-place,
  length-preserving substitutions: the digest value's 64 hex characters → 64
  ASCII `0`, and the seal record's `body_crc32c` prelude field → 0x00000000.
  The digest's wire form is 64 literal ASCII hex bytes, never JSON-escaped.

## Record wrapper

```
record := magic_u16 (0xA9 0x5F)
          wrapper_version_u8 (=1)   ; unknown => reader fails closed
          record_kind_u8 (1=header, 2=frame, 3=seal)  ; unknown => fail closed
          codec_id_u8 (1=JSON-UTF8) ; unknown => fail closed; the transform seam
          reserved_u8 (=0)          ; nonzero => fail closed
          len_u32_le (<= 16 MiB)
          prelude_crc32c_u32_le     ; over [version..len]
          body_crc32c_u32_le        ; over body
          body[len]
```

The fixed prelude is 18 bytes. CRC32C (Castagnoli); the prelude CRC validates
`len` independently of the body, which is what makes torn tails *provable*.

**Tail rule.** Tear classification applies only to the final record of an
`.open`/`.recovering` file, and only when no seal record precedes it (the
writer never appends after sealing — a post-seal suffix is loud in every
state). (a) fewer than 18 bytes remain ⇒ truncated prelude, discard;
(b) valid prelude + fewer than `len` body bytes ⇒ torn body, discard;
(c) anything else — a complete prelude failing any check, a complete body
with a bad CRC — is loud, operator-acknowledged only. In `.sotseg` every
defect is loud, and a valid seal must end exactly at EOF.

## Segment lifecycle

Files:
`<voyage>/seg/<index:hex8>-<epoch:hex14>.{open|recovering|recovering-out|sotseg}`
— fixed-width lowercase hex; index starts at 0 and increments per rotation;
consecutive segments may share an epoch (rotation within one run); filename
and header identity must agree. Record order per segment:
`header frame* seal?` (seal absent only while unsealed).

1. **Voyage bootstrap**: build `<voyage>.creating/` containing `seg/`,
   `blobs/`, `blobs/.tmp/`, `blobs/sha256/`, `writer.lock`; fsync created
   dirs bottom-up; RENAME_NOREPLACE to `<voyage>/`; fsync parent. The
   container must already exist — bootstrap never creates ancestor levels it
   cannot anchor. `blobs/sha256/` is created HERE so the first blob publish
   only ever creates the shard level (whose entry its own `sha256` flush
   pins); created lazily, the `sha256` entry itself would go unanchored in
   `blobs/`. Publication re-flushes `blobs/` anyway, which is also the
   migration path for voyages bootstrapped before this rule.
2. **Open**: O_EXCL-create `.open` → write header record → fsync file → fsync
   dir → only then may frames append or acks fire. A headerless or
   header-partial `.open` found under the lock is reinitialized in place.
3. **Seal**: append seal → fsync file → RENAME_NOREPLACE to `.sotseg` → fsync
   dir. Destination-exists is loud.
4. **Recovery** (provable tears only; loud conditions halt): rename `.open` →
   `.recovering` (no-clobber, fsync dir) → build `.recovering-out` = **the
   valid prefix copied byte-verbatim** + a seal → fsync it → RENAME_NOREPLACE
   to `.sotseg` (fsync dir) → verify → unlink `.recovering` → fsync dir.
   **Retained bytes are never decoded-and-re-encoded**: re-serialization
   would reorder JSON keys (changing committed frames' wire bytes and their
   digests) and strip unknown ignorable members — the format's
   forward-compat mechanism. Recovery may only ever *remove* the torn tail
   and *append* a seal. The recovery metadata group `{recovered: true,
   truncated_bytes, truncation_reason, recovered_by_epoch}` appears in the
   seal **iff bytes were actually discarded**; a clean unsealed tip that a
   successor epoch closes (writer died between output and seal, nothing
   torn) receives a **plain seal** — its content is exactly what the
   original writer wrote, and false recovery metadata would be a permanent
   lie in read-forever bytes. The original `.recovering` file is transaction
   scratch; every retained byte lives verbatim in the published file and the
   seal is the audit. Startup reconciliation,
   in order: headerless `.open` ⇒ reinitialize; `.open` with a valid seal at
   EOF ⇒ publish as-is; `.open` with a provable tear ⇒ recover; `.open` with
   a loud condition ⇒ halt; `.sotseg` alone ⇒ done; `.recovering`+`.sotseg`
   ⇒ verify, unlink scratch; `.recovering`+`.recovering-out` ⇒ delete
   staging, rebuild; `.recovering` alone ⇒ resume; `.recovering-out` alone ⇒
   invalid, loud; anything else ⇒ loud. Idempotent at every crash point.

**Header body**: `{version: 1, required_features: [], voyage_id,
segment_index: u53, epoch: u53, prev_seal_digest: Digest|null,
created_wall_ms: i53}`, plus genesis-only (index 0):
`retention_class: "archive"/"discard"/"distill"` — immutable voyage policy,
stated once, chain-derived thereafter.

**Seal body**: `{frame_count: u53, first_seq: Ref|null, last_seq: Ref|null,
recovered?: true, truncated_bytes?: u53, truncation_reason?: str,
recovered_by_epoch?: u53, digest: Digest}` (nulls ⇔ empty segment).
`prev_seal_digest` chains segments; the digest covers the header, all frames,
and the seal's own metadata via the preimage above. The chain claim is
consistency-checking, not tamper-proofing.

**Durability invariants (normative)**: input, input_fact, lifecycle,
control_exchange, turn frames, producer_attached, and anything acknowledged
or published to a watcher are committed (fsynced) before they are visible;
opaque producer output may group-commit behind the capsule-publication
watermark. Batching values and failure-signal transports are implementation
policy, not format. After an append/fsync failure the capsule writes nothing
further to the log — it stops reading producer output, refuses input, and
signals out of band; recovery repairs first.

## Writer fencing

`writer.lock` is a persistent lock file, never unlinked. The fence is **one
kernel-held exclusive lock per platform, pinned**: on unix,
`flock(LOCK_EX | LOCK_NB)` exactly, descriptor opened `O_CLOEXEC` and never
passed to the producer or any child; on Windows (specified and implemented
with phase P3's FE-local capsules), `LockFileEx` exclusive on the same file,
handle non-inheritable — the same semantics: held for the writer's lifetime,
released by the kernel on process death. Per-platform primitives create no
interop hazard because a voyage's live directory is local to one host — two
operating systems never contend for the same lock. The lock-file body
`{pid, boot_id, epoch}` is diagnostic only; the kernel lock is the truth
(PID reuse and probe-and-replace races are thereby irrelevant). Epoch
allocation happens while holding it.

**The format is OS-neutral; only the store's durability operations are
platform code.** Nothing in the wire or on-disk contract is unix-specific — a
voyage written on one OS reads on any other. The three platform touchpoints
and their Windows equivalents: directory flush (`FlushFileBuffers` on a
`FILE_FLAG_BACKUP_SEMANTICS` directory handle), no-clobber rename
(`MoveFileExW` WITHOUT `MOVEFILE_REPLACE_EXISTING` — std's rename clobbers),
and the lock above — all three implemented for Windows in P3 (ADR 0041
§store port), where a volume preflight additionally pins voyages to local
NTFS. The store still **fails closed** on the platforms without real arms:
non-Linux unix refuses the no-clobber rename (it requires `renameat2` —
hard-link-plus-unlink is not atomic, and a crash between the two syscalls
would manufacture exactly the invalid file-state combinations reconciliation
treats as loud). Silently weaker atomicity is the thing this store exists to
refuse. Live voyage directories live on a LOCAL filesystem on every OS;
sealed segments may later be packed to shared storage when the pack format
exists.

## The seams (how v1 grows without changing v1 bytes)

- **`codec_id`** is the per-record transform seam. A future registered codec
  defines its complete body layout when built; it must authenticate the
  immutable segment-header identity and declare bounded expansion; a segment
  rotation precedes first use. v1 registers codec 1 (JSON-UTF8) only.
- **`required_features`**: namespaced strings `sot.<area>.<name>`. Every
  segment using a feature lists it. A registry entry is required to extend
  any closed enum above or introduce an authority-changing field; readers
  refuse segments whose features they don't implement. Unknown plain JSON
  fields remain ignorable. **Registry (amended 2026-08-24, ADR 0040):**
  - `sot.producer.json-f64-v1` — producer-class payload numbers may be any
    finite IEEE-754 binary64 value in ordinary valid JSON spelling
    (exponents permitted); control and envelope fields remain u53/i53.
    Without this feature, producer-payload numbers obey the integer atoms.
  - `sot.capsule.cgroup-fence-v1` — `producer_spawn.detail` carries an
    authority-bearing kill-domain locator that successor epochs act on
    destructively. The locator is discriminated by scheme:
    `{"scheme": "cgroup", "path": ...}` bears authority, and the verifier
    REQUIRES its segment to declare this feature (locator-must-declare,
    enforced since the wiring PR); `{"scheme": "none"}` (an explicitly
    unfenced test rig) and an absent `kill_domain` (the P1 PTY capsule)
    claim no authority and need no feature; any other scheme fails closed.
- **Wrapper/header versions** cover everything else. Migration is only ever
  by derived copies or new linked segments — never in-place rewriting.

## Blob CAS

`<voyage>/blobs/sha256/<digest[0:2]>/<digest>`; algo registry v1 =
`{"sha256"}`. Publication: write to `blobs/.tmp/<random>` → fsync →
RENAME_NOREPLACE → fsync shard dir → then the referencing frame (a blob is
committed before anything may point at it). EEXIST ⇒ verify existing file by
digest AND length: match = idempotent success (remove the temp; still fsync
the shard dir before referencing); mismatch = loud. Blobs are immutable and
never removed in v1. Oversized producer payloads ride the same CAS via
`payload_ref`.

## Verifier and gates

`sot-log verify` checks: wrapper fail-closed fields; prelude+body CRCs; seal
digests + chain; filename⇔state consistency with (in quiescent state) at most
one non-`.sotseg` file, only at the chain tip; filename=header identity and
`segment_index` continuity; epoch nondecreasing, changing only at run
boundaries; frame `seq.epoch` = segment epoch; per-epoch `n` contiguous from
1; every structural ref resolves to an earlier committed frame; the
cross-field matrix; capture-before-inline-input; stream `prev`-chains
per-cell linear; blob presence + digest + length; take_epoch strictly
increasing with a committed null-holder state opening each writer epoch;
idem_key chains matching the lattice exactly; producer-payload numbers
conforming to the integer atoms unless the segment declares
`sot.producer.json-f64-v1`.

**Turn closure (amended 2026-08-24, ADR 0040):** every `turn_open` must have
exactly one winning `turn_close`. Two verification predicates exist —
**complete** (the default and the only CERTIFYING mode: zero unmatched
opens; required for archival, recovery acceptance, CI, and offline
verification) and **`--allow-open-tip`** (a non-certifying diagnostic for a
voyage under a live writer: tolerates at most one unmatched open, and only
in the currently open tip's epoch; a verifier cannot prove a writer is
live, so this mode certifies nothing). The mode is always an explicit
argument, never inferred from epoch maximality: a dead-but-latest epoch
fails complete mode loudly, which is correct — it needs successor closure,
not tolerance. Adapters that emit turns must perform successor closure
(synthesize a death close for each unmatched prior-epoch open, fsynced,
under the writer lock, before accepting input).

Merge gates for the crate: cross-language golden fixtures (Rust writes,
Julia reads — the fixtures are conformance tests for this ADR, not its
substitute) and a fault harness: kill -9 sweeps at randomized write points;
power-loss reordering below the fsync barrier; ENOSPC/EDQUOT/EIO/short
writes; directory-entry loss including voyage bootstrap; complete-record
corruption staying loud; corrupted length vs. truncation (the prelude CRC
distinguishes them); double stale-lock takeover; PID reuse; inherited-fd
lock leak; every reconciliation state entered by crash injection; blob
EEXIST identical and mismatched; dedupe folds across epoch restart; zero-run
epochs. Properties: verify green after every recovery; no acknowledged input
missing; nothing published to a watcher missing; retained frames contiguous.

## Adapter obligations (normative, outside the wire)

Voyage directories are owner-only (0700). Producer-owned stores (the Agent
SDK's transcripts, Codex's rollouts and history) remain plaintext regardless
of anything in this format — the redaction promise is the voyage's alone;
adapters isolate producer homes per capsule and tie their retention to the
voyage's. Each adapter declares: its native payload representation, its
input-derived alias list (what `transformed` strips), its turn admission
rule, and its producer version pin. Raw-terminal capsules declare no turns.

## Consequences

- The store is implementable from this document plus the encoding atoms;
  golden fixtures pin the bytes.
- Every deferred capability (forks, packs, GC, retention deletion,
  encryption, compression, signed anchors) has a named seam and costs v1
  nothing today.
- The design history (four adversarial rounds, the deletion log, and the
  producer-semantics spike that seeded the requirements) lives in the
  private working notes; this ADR is self-contained without them.
