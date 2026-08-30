//! Voyage store: bootstrap, writer lock + epoch allocation, rotation, blob
//! CAS publication (ADR 0039). Kernel-semantics parts have Linux and
//! Windows arms (ADR 0041 §store port); the codec itself is portable.

use crate::envelope::{Digest, InputFactKind, Seq};
use crate::fsutil::{self, DirIdentity, PinnedDir, WriterLock};
use crate::recovery::{self, Reconciled};
use crate::segment::{
    HeaderBody, RetentionClass, SegmentIdentity, SegmentReader, SegmentState, SegmentWriter,
};
use crate::{Error, Result};
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// A 16-byte `idem_key`, parsed once from its `hex32` wire/JSON form (ADR
/// 0041 decision 5: "Parsed keys, not Strings") and used as the dedupe
/// index's key type from then on.
pub type IdemKey = [u8; 16];

/// One `idem_key`'s position in the ADR 0039 "Input WAL + dedupe" lattice,
/// as folded from the retained voyage. Four states, not the verifier's
/// five (`FactState` in `verify.rs` also tracks `Observed`): a
/// `producer_observed` fact — an adapter-only extension raw-terminal
/// capsules never emit (ADR 0041: "raw-terminal chains end at `forwarded`")
/// — folds into `Forwarded` here too, because both answer a duplicate
/// `idem_key` identically (`input_recorded`); this index exists to decide
/// that wire reply, not to record whether echo-confirmation ever happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupeState {
    Input,
    Intent,
    Forwarded,
    Refused,
}

/// One `idem_key`'s dedupe record. `input` is the ORIGINAL input frame's
/// seq — a `{input}`-only chain's retry-fold (ADR 0039: "chain = {input} =>
/// a same-key retry MUST re-attempt, new intent, same input identity")
/// needs it, not a freshly-minted one. `intent` is set once a
/// `forward_intent` fact has been folded in, so a later `forwarded`/
/// `refused_stale_epoch` fact (or a live re-attempt) can reference it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DedupeEntry {
    pub input: Seq,
    pub state: DedupeState,
    pub intent: Option<Seq>,
}

/// Parses a lowercase `hex32` `idem_key` into its 16 raw bytes. Shared by
/// the fold (below, the "writer" side) and `verify.rs` (finding 8: "add
/// lowercase-hex32 idem_key enforcement to BOTH the writer validation and
/// the verifier" — one format check, not two independently-drifting ones).
/// `pub(crate)` rather than a second copy: a malformed key is now a FOLD
/// ERROR (see `walk_segment`), not a silently-skipped frame.
pub(crate) fn parse_idem_key(s: &str) -> Option<IdemKey> {
    if s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Walks one segment's frames EXACTLY ONCE (finding 9: the previous version
/// traversed `reader.frames` a second time for this, even though there was
/// only ever one disk read/decode; `open_for_writing`'s max-take-epoch
/// tracking now folds into this SAME loop instead of its own), computing
/// the max committed `take_state.take_epoch` AND folding the ADR 0041
/// decision-5 input-WAL dedupe index together. `by_seq` is a
/// WALK-TIME-ONLY accessory (an `input_fact`'s `fact.input` names its input
/// frame by `Seq`, never by `idem_key` — the wire schema has no other way
/// to resolve it) threaded across every segment alongside `index`, then
/// dropped: a capsule's own LIVE updates after open always already hold the
/// idem_key of whatever they just wrote, so they never need it.
///
/// FAILS CLOSED (finding 8): a duplicate `idem_key` across two `input`
/// frames, an `input_fact` naming an unresolvable input, a fact-kind
/// illegal from its idem_key's CURRENT lattice state (mirroring
/// `verify.rs`'s own `FactState` machine exactly — `forward_intent` only
/// from `Input`, `forwarded`/`refused_stale_epoch` only from... see below),
/// an unrecognized fact-kind string, or a malformed (non-lowercase-hex32)
/// `idem_key` are all errors, not silently-skipped or last-writer-wins
/// frames. This index is read by a LIVE writer to decide whether to
/// re-forward a duplicate input — silently mis-indexing it here is not a
/// defect an optional, separate `sot-log verify` run can be trusted to
/// catch first.
fn walk_segment(
    index: &mut HashMap<IdemKey, DedupeEntry>,
    by_seq: &mut HashMap<Seq, IdemKey>,
    reader: &SegmentReader,
) -> Result<u64> {
    let mut max_take = 0u64;
    for f in &reader.frames {
        let Some(p) = f.payload.as_ref() else { continue };

        if p.get("kind").and_then(|v| v.as_str()) == Some("take_state") {
            if let Some(te) = p.get("take").and_then(|t| t.get("take_epoch")).and_then(|v| v.as_u64()) {
                max_take = max_take.max(te);
            }
        }

        if f.class == crate::envelope::Class::Input {
            let key_str = p.get("idem_key").and_then(|v| v.as_str()).ok_or_else(|| {
                Error::Schema(format!("frame {:?}: idem_key is missing or not a string", f.seq))
            })?;
            let key = parse_idem_key(key_str).ok_or_else(|| {
                Error::Schema(format!("frame {:?}: idem_key {key_str:?} is not lowercase hex32", f.seq))
            })?;
            if index.contains_key(&key) {
                return Err(Error::Schema(format!(
                    "frame {:?}: idem_key {key_str:?} reused from an earlier input frame",
                    f.seq
                )));
            }
            by_seq.insert(f.seq, key);
            index.insert(
                key,
                DedupeEntry {
                    input: f.seq,
                    state: DedupeState::Input,
                    intent: None,
                },
            );
            continue;
        }

        if f.class != crate::envelope::Class::Lifecycle
            || p.get("kind").and_then(|v| v.as_str()) != Some("input_fact")
        {
            continue;
        }
        // Round-2 review, finding 6: the fold goes fully TYPED and
        // fallible here -- `serde_json::from_value` into the same shape
        // `verify.rs`'s own `FactObj` deserializes (`input: Seq, fact:
        // InputFactKind, intent: Option<Seq>`), so a missing/malformed
        // `fact` object, a missing/non-object `input` seq, or an unknown
        // fact-kind string are ALL a single `Err` instead of three
        // separate silent `continue`s. `InputFactKind` is serde's closed
        // enum (`#[serde(rename_all = "snake_case")]`), so an
        // unrecognized string fails the deserialize itself -- no separate
        // catch-all arm needed anymore.
        #[derive(serde::Deserialize)]
        struct FactObj {
            input: Seq,
            fact: InputFactKind,
            #[serde(default)]
            intent: Option<Seq>,
        }
        let fact_value = p.get("fact").ok_or_else(|| {
            Error::Schema(format!("frame {:?}: input_fact is missing its fact object", f.seq))
        })?;
        let fact: FactObj = serde_json::from_value(fact_value.clone())
            .map_err(|e| Error::Schema(format!("frame {:?}: fact malformed: {e}", f.seq)))?;
        let key = *by_seq.get(&fact.input).ok_or_else(|| {
            Error::Schema(format!(
                "frame {:?}: input_fact names {:?}, which is not a committed input frame",
                f.seq, fact.input
            ))
        })?;
        let entry = index
            .get_mut(&key)
            .expect("by_seq and index are populated together, in the same branch above");
        match fact.fact {
            InputFactKind::ForwardIntent => {
                if entry.state != DedupeState::Input {
                    return Err(Error::Schema(format!(
                        "frame {:?}: forward_intent illegal from the current chain state for idem_key {key:02x?}",
                        f.seq
                    )));
                }
                // The `forward_intent` fact carries no separate `intent`
                // field (ADR 0039: only `forwarded`/`producer_observed`
                // do, naming a PRIOR frame) -- THIS frame's own seq IS the
                // intent record.
                entry.state = DedupeState::Intent;
                entry.intent = Some(f.seq);
            }
            InputFactKind::Forwarded => {
                if entry.state != DedupeState::Intent {
                    return Err(Error::Schema(format!(
                        "frame {:?}: forwarded illegal from the current chain state for idem_key {key:02x?}",
                        f.seq
                    )));
                }
                // Round-2 review, finding 6: `forwarded.intent` must name
                // the SAME `forward_intent` frame this idem_key's chain
                // actually recorded -- `entry.intent` is exactly that
                // (set, `Some`, the moment `ForwardIntent` fired above; a
                // mismatch or a missing `intent` field are equally wrong).
                if fact.intent != entry.intent {
                    return Err(Error::Schema(format!(
                        "frame {:?}: forwarded.intent {:?} does not match the recorded forward_intent {:?} for idem_key {key:02x?}",
                        f.seq, fact.intent, entry.intent
                    )));
                }
                entry.state = DedupeState::Forwarded;
            }
            InputFactKind::ProducerObserved => {
                if entry.state != DedupeState::Forwarded {
                    return Err(Error::Schema(format!(
                        "frame {:?}: producer_observed illegal from the current chain state for idem_key {key:02x?}",
                        f.seq
                    )));
                }
                if fact.intent != entry.intent {
                    return Err(Error::Schema(format!(
                        "frame {:?}: producer_observed.intent {:?} does not match the recorded forward_intent {:?} for idem_key {key:02x?}",
                        f.seq, fact.intent, entry.intent
                    )));
                }
                // Already the terminal `Forwarded` bucket (see
                // `DedupeState`'s doc) -- no state change.
            }
            InputFactKind::RefusedStaleEpoch => {
                if entry.state != DedupeState::Input {
                    return Err(Error::Schema(format!(
                        "frame {:?}: refused_stale_epoch illegal from the current chain state for idem_key {key:02x?}",
                        f.seq
                    )));
                }
                entry.state = DedupeState::Refused;
            }
        }
    }
    Ok(max_take)
}

pub struct VoyageStore {
    root: PathBuf,
    voyage_id: String,
    _lock: WriterLock,
    /// ADR 0041 Codex round-2b: held for the STORE's whole open lifetime,
    /// not merely through `open_prepared`'s own return — on Windows this
    /// is what keeps the root's rename/delete refused at the OS level for
    /// as long as the store stays open (see `PinnedDir`'s own doc); on
    /// every other platform it keeps the `/proc/self/fd` alias (or,
    /// where neither applies, simply the handle) alive.
    #[allow(dead_code)] // held for its Drop (releases the pin)
    _root_pin: PinnedDir,
    /// The epoch THIS writer allocated at open.
    pub epoch: u64,
    /// Chain state after reconciliation: last sealed digest + next index.
    pub prev_seal_digest: Option<Digest>,
    pub next_segment_index: u64,
    pub retention_class: RetentionClass,
    /// Highest committed `take_state.take_epoch` seen in sealed history —
    /// a resumed writer's revoke-first `take_state {holder: null}` must use
    /// a value strictly greater than this (ADR 0039 take predicate).
    pub last_take_epoch: u64,
    /// True when an `.open` segment survived reconciliation (the live tip a
    /// previous incarnation left; this writer must seal it before rotating —
    /// v1 keeps it simple: reconcile() recovers tears, and a clean survivor
    /// is sealed by `seal_survivor` before new writing).
    survivor_open: Option<SegmentIdentity>,
    /// ADR 0041 decision 5: the input-WAL dedupe index, folded once from
    /// this SAME open-time walk (never a second scan) over the whole
    /// retained voyage — keys never expire in v1, so this is O(retained
    /// inputs) memory, unbounded, stated here rather than hidden. A capsule
    /// keeps it live across the run: it already holds the idem_key of
    /// whatever it just wrote, so it updates this map directly rather than
    /// re-deriving anything from it.
    pub dedupe_index: HashMap<IdemKey, DedupeEntry>,
}

/// A resolved voyage root, captured together with its KERNEL identity —
/// not merely its canonical path text (ADR 0041 Codex round-2 discharge).
/// Produced by [`VoyageStore::prepare_root`]; consumed by
/// [`VoyageStore::open_prepared`], which verifies this identity against
/// what it ACTUALLY opens, under the fence, before trusting the path any
/// further. A canonicalized path STRING alone cannot make that promise: a
/// same-pathname directory-entry replacement (a rename-swap; see
/// `open_prepared`'s own doc) leaves the string unchanged while the
/// underlying object differs, and only the OS's own object identity
/// (`fsutil::DirIdentity`) tells the two apart.
#[derive(Debug, Clone)]
pub struct PreparedRoot {
    path: PathBuf,
    identity: DirIdentity,
}

impl PreparedRoot {
    /// The resolved, canonical path — still needed for every ordinary
    /// path-based operation `open_prepared` performs (the lock file,
    /// preflight, the segment directory, ...); `identity` is what actually
    /// authenticates it.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl VoyageStore {
    /// Bootstrap a new voyage: build under `<root>.creating/`, fsync
    /// bottom-up, publish by no-clobber rename (ADR 0039 §lifecycle 1).
    pub fn bootstrap(root: &Path, voyage_id: &str, retention: RetentionClass) -> Result<()> {
        // Resolve here rather than trust the caller went through
        // `ensure_container` first: `bootstrap` is a public entry point in
        // its own right (this module's own tests call it directly with a
        // raw path). Canonicalize the parent — which must already exist —
        // and reconstruct the not-yet-existing root by appending its raw
        // final component, the same pattern `ensure_container` uses and for
        // the same reason: a lexical-only `absolute` can leave `root` naming
        // its container through a symlink or a `..` alias, a different
        // identity than the one every operation below must agree on.
        let root_abs = std::path::absolute(root)?;
        let lexical_parent = root_abs
            .parent()
            .ok_or_else(|| Error::State("voyage root needs a parent dir".into()))?;
        let name = root_abs
            .file_name()
            .ok_or_else(|| Error::State("bad voyage root name".into()))?;
        // The container must PREEXIST: bootstrap will not create ancestor
        // levels, because it cannot durably anchor them (their entries in
        // THEIR parents are never flushed here — a "successful" bootstrap
        // into an implicitly created chain could vanish on power loss).
        // The container's durability is its creator's responsibility.
        let parent = std::fs::canonicalize(lexical_parent).map_err(|e| {
            Error::State(format!(
                "voyage container {lexical_parent:?} does not exist (bootstrap will not create it): {e}"
            ))
        })?;
        let root = parent.join(name);
        let root = root.as_path();
        let parent = parent.as_path();
        // Volume preflight BEFORE any `.creating` mutation (ADR 0041).
        fsutil::preflight_volume(parent)?;
        let name_str = name
            .to_str()
            .ok_or_else(|| Error::State("bad voyage root name".into()))?;
        // Staging is ATTEMPT-OWNED: `<name>.creating-<random>`, one fresh
        // directory per bootstrap attempt, protected from birth
        // (`create_dir_protected` — ADR 0041 §Security split, never
        // create-then-repair). A SHARED staging pathname — with or without a
        // remove-residue step — cannot be made safe: with removal, a
        // concurrent bootstrap can delete this attempt's populated staging
        // and substitute an empty one between our flushes and our rename
        // (publishing a directory nobody flushed, defeating
        // source-flush-before-rename); without removal, two attempts
        // interleave writes into one tree. A name nobody else knows
        // dissolves both — the same reasoning as `publish_blob`'s random
        // temp suffix — and `publish_noreplace` below already arbitrates
        // the winner. Everything under the staging root (seg/, blobs/,
        // writer.lock, ...) stays plain creation and INHERITS the
        // protection.
        let staging = {
            let mut r = [0u8; 4];
            getrandom::fill(&mut r).map_err(std::io::Error::from)?;
            parent.join(format!(
                "{name_str}.creating-{:02x}{:02x}{:02x}{:02x}",
                r[0], r[1], r[2], r[3]
            ))
        };
        // Any exit before the publish defuses this — error return or panic —
        // removes the attempt's staging: a loser or a failure never leaves
        // residue behind by any path that runs destructors. (A hard kill
        // does; the post-publish sweep below is what retires that.)
        struct StagingGuard(std::path::PathBuf, bool);
        impl Drop for StagingGuard {
            fn drop(&mut self) {
                if self.1 {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
        }
        let mut guard = StagingGuard(staging.clone(), true);
        fsutil::create_dir_protected(&staging)?;
        std::fs::create_dir_all(staging.join("seg"))?;
        std::fs::create_dir_all(staging.join("blobs").join(".tmp"))?;
        // sha256/ exists (and is flushed) from birth so the first CAS
        // publish only ever creates the SHARD level — whose entry its own
        // fsync_dir(sha256) pins. Created lazily instead, the sha256 entry
        // itself would never be anchored in blobs/.
        std::fs::create_dir_all(staging.join("blobs").join("sha256"))?;
        // The lock inode is persistent and never unlinked.
        let mut lockf = std::fs::File::create(staging.join("writer.lock"))?;
        lockf.write_all(b"{}")?;
        lockf.sync_all()?;
        // Windows refuses to rename a directory while any handle is open
        // beneath it (ERROR_ACCESS_DENIED) — the lock handle must close
        // before the publish rename below.
        drop(lockf);
        // Persist the voyage identity + retention where the genesis header
        // will restate it (bootstrap happens before any segment exists).
        fsutil::fsync_dir(&staging.join("blobs").join(".tmp"))?;
        fsutil::fsync_dir(&staging.join("blobs").join("sha256"))?;
        fsutil::fsync_dir(&staging.join("blobs"))?;
        fsutil::fsync_dir(&staging.join("seg"))?;
        fsutil::fsync_dir(&staging)?;
        fsutil::publish_noreplace(&staging, root)?;
        guard.1 = false; // published: the staging path IS the root now
        // Retire crash residue from attempts that never ran their guard (a
        // hard kill mid-bootstrap). Only after WINNING: the voyage exists,
        // so every `<name>.creating-*` sibling is either a dead attempt's
        // leavings or a live loser about to fail its own publish — removal
        // is correct for the first and merely hastens the second. Best
        // effort: a sweep failure is not a bootstrap failure.
        if let Ok(entries) = std::fs::read_dir(parent) {
            let residue_prefix = format!("{name_str}.creating-");
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().starts_with(&residue_prefix) {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        let _ = (voyage_id, retention); // identity/retention live in the genesis header
        Ok(())
    }

    /// Open for writing: take the kernel lock, reconcile every segment
    /// identity found, allocate this writer's epoch (max durable + 1), and
    /// compute the chain tip. No lease to check (see
    /// [`Self::open_for_writing_with_lease`] for the U2 supervisor's own
    /// entry point) — every in-tree caller today.
    pub fn open_for_writing(root: &Path, voyage_id: &str) -> Result<Self> {
        Self::open_for_writing_with_lease(root, voyage_id, None)
    }

    /// LAUNCHING-side preparation (ADR 0041 Codex round-1 Major 5 / round-2
    /// discharge): resolves `root` to its canonical, symlink-free absolute
    /// path AND captures its kernel identity — exactly the FIRST thing
    /// this store used to do internally on every open, now split out so a
    /// spawning authority (U2's supervisor) can perform this
    /// UNBOUNDED-COST resolution itself, BEFORE `CreateProcess`, and pass
    /// the already-prepared [`PreparedRoot`] token to the child. That is
    /// what actually bounds the child's own invisible window
    /// (`CreateProcess` -> fence acquisition) to "open + try_lock" alone —
    /// see [`Self::open_prepared`]'s own doc for the child-side half of
    /// this split, and [`PreparedRoot`]'s own doc for why the token
    /// carries identity, not merely a path. Requires `root` to already
    /// exist (every in-tree caller follows `bootstrap`, or its own
    /// exists-check).
    pub fn prepare_root(root: &Path) -> Result<PreparedRoot> {
        let path = std::fs::canonicalize(root)?;
        let identity = fsutil::dir_identity(&path)?;
        Ok(PreparedRoot { path, identity })
    }

    /// As [`Self::open_for_writing`], with the ADR 0041 Lifecycle
    /// "Discovery, and the two windows" parent-death lease check folded in:
    /// `lease_broken`, if supplied, is called EXACTLY ONCE, immediately
    /// after the writer fence is acquired and before any other pre-fence
    /// durability I/O or history traversal — "the child's first act after
    /// acquiring the fence is to check the parent-death lease it was
    /// spawned with... if the lease is broken it releases the fence and
    /// exits without binding." `None` (every in-tree caller today) skips
    /// the check entirely; U2's supervisor is the first real caller of
    /// `Some(_)`, passing an OS-checked, synchronizable handle to its own
    /// supervisor. `Err(Error::LeaseBroken)` means exactly that: the fence
    /// is already released by the time this returns, and the caller must
    /// exit without binding — never retry, never repair.
    ///
    /// A thin WRAPPER composing [`Self::prepare_root`] then
    /// [`Self::open_prepared`] in one call, for every single-process
    /// caller today — their end-to-end behavior and operation order are
    /// UNCHANGED by the split below (ADR 0041 Codex round-1, Major 5:
    /// "today's single-process callers keep working via a wrapper that
    /// prepares-then-opens").
    pub fn open_for_writing_with_lease(
        root: &Path,
        voyage_id: &str,
        lease_broken: Option<&dyn Fn() -> bool>,
    ) -> Result<Self> {
        let prepared = Self::prepare_root(root)?;
        Self::open_prepared(&prepared, voyage_id, lease_broken)
    }

    /// The CHILD-side entry point (ADR 0041 Codex round-1 Major 5 / round-2
    /// / round-2b discharge): `prepared` MUST be exactly what
    /// [`Self::prepare_root`] returned — the launching authority's own
    /// job, done BEFORE spawning this process. Performs, IN ORDER: open
    /// and PIN the prepared directory (before anything else — the pin
    /// itself is the first thing acquired) -> verify the pin's kernel
    /// identity against the token -> the writer-lock open + `try_lock`
    /// (the fence), taken THROUGH the pin -> the lease check -> volume
    /// preflight -> the two directory flushes -> history traversal
    /// (enumerate/reconcile every segment identity, fold the dedupe
    /// index), ALSO through the pin. [`Self::open_for_writing_with_lease`]
    /// is the WRAPPER every single-process caller uses today, composing
    /// [`Self::prepare_root`] with this in one call — their end-to-end
    /// order is unchanged.
    ///
    /// **Why the pin, and not merely a one-time identity check (Codex
    /// round-2b):** round-2's fix verified identity once, via a transient
    /// open, and then proceeded to re-open by PATH for everything after —
    /// the writer lock, preflight, both flushes, the segment directory
    /// enumeration. A live repro proved the gap that leaves: a tight
    /// `RENAME_EXCHANGE` loop against the prepared root let a store open
    /// with a REPLACEMENT's `retention_class`, because a swap landing
    /// AFTER the (correct, at the time) identity check but BEFORE (or
    /// between) those later path-based opens redirects every one of them
    /// exactly as easily as it would have redirected the check itself —
    /// checking the path and then TRUSTING it for everything afterward is
    /// still a check-then-use gap, merely a narrower one. Pinning closes
    /// it structurally: every operation below resolves through the SAME
    /// held object (`PinnedDir::pinned_path`), never by re-deriving a path
    /// from the original argument again, so there is no LATER re-open left
    /// for a swap to land in front of. See [`fsutil::PinnedDir`]'s own doc
    /// for the per-platform mechanism (Windows: the OS itself refuses the
    /// rename/exchange while the handle is held; Linux: the
    /// `/proc/self/fd` alias is immune to one regardless).
    ///
    /// The identity check itself is UNCHANGED in spirit from round 2 —
    /// [`DirIdentity`] still subsumes the symlink-retarget case a
    /// path-text comparison alone could not — only WHERE it reads from
    /// changes: off the pin's own handle, not a second transient open.
    ///
    /// `fsync_dir(parent)` is the one exception that stays on the REAL
    /// path: it flushes the root's PARENT directory (anchoring the root's
    /// own directory entry within it), a different object the pin was
    /// never opened on and has no fd-relative alias for — and it is not
    /// what the reported race targets (the swap replaces the ROOT's
    /// identity at a fixed pathname; the parent itself is untouched).
    pub fn open_prepared(
        prepared: &PreparedRoot,
        voyage_id: &str,
        lease_broken: Option<&dyn Fn() -> bool>,
    ) -> Result<Self> {
        // The pin: the FIRST thing acquired, before the fence, before the
        // lease check, before any other I/O -- everything below flows
        // from it (Codex round-2b).
        let pin = PinnedDir::open(&prepared.path)?;

        // Verify-on-the-handle, not re-stat-by-path -- `pin.identity()`
        // reads off the SAME handle the pin holds, so nothing can slip
        // between this check and the pin's own opening the way a second,
        // independent stat-by-path call could. Cheap (one metadata call,
        // not O(history)) and safely inside the pin's own protection, so
        // it costs nothing toward the invisible window this split exists
        // to bound.
        if pin.identity()? != prepared.identity {
            return Err(Error::State(format!(
                "voyage root {:?} does not match its prepared kernel identity -- \
                 the directory at this path was replaced (a rename-swap or a retarget) \
                 between preparation and fence acquisition",
                prepared.path
            )));
        }
        let root = pin.pinned_path();

        // U1a: the fence, ahead of preflight/fsync/history, and now taken
        // THROUGH the pin -- see this method's own doc for why.
        // `lock_writer` itself does no directory enumeration or unbounded
        // I/O: open-existing plus one bounded `try_lock` retry (ADR 0041
        // store port), exactly the primitive the spawned child's
        // INVISIBLE window is defined in terms of.
        let lock = fsutil::lock_writer(&root.join("writer.lock"))?;

        // The lease check: the fence's FIRST act, before any other durable
        // I/O or history traversal. `lock` (and the fence it holds) drops
        // here on the early return, releasing it before this function ever
        // touches the segment directory; `pin` drops too, releasing the
        // pin.
        if lease_broken.is_some_and(|f| f()) {
            return Err(Error::LeaseBroken);
        }

        // Re-run the volume preflight on the resolved voyage dir (ADR 0041):
        // a store bootstrapped elsewhere and moved to an unsuitable volume
        // must refuse — now checked with the fence already held, since nothing
        // about the refusal itself needs to precede it.
        fsutil::preflight_volume(root)?;
        // Restate the root's anchoring: bootstrap's publish rename may have
        // become visible while its container flush was lost to a crash — the
        // callers' bootstrap-if-absent check would then skip bootstrap
        // forever, leaving a store that acknowledges records from a root the
        // next power loss can remove. Idempotent, so every open re-anchors.
        // (This is also the Part 3 restatement for bootstrap's own publish:
        // no separate call is needed there — every open already re-flushes
        // root plus its parent, unconditionally, before anything else.)
        fsutil::fsync_dir(root)?;
        // The one exception that stays on the REAL path -- see this
        // method's own doc.
        if let Some(parent) = prepared.path.parent() {
            fsutil::fsync_dir(parent)?;
        }
        let seg_dir = root.join("seg");

        // Enumerate identities across ALL states.
        let mut idents: Vec<(u64, u64)> = Vec::new();
        for entry in std::fs::read_dir(&seg_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some((idx, ep, _state)) = SegmentIdentity::parse_file_name(name) {
                if !idents.contains(&(idx, ep)) {
                    idents.push((idx, ep));
                }
            }
        }
        idents.sort_unstable();

        let max_epoch = idents.iter().map(|(_, e)| *e).max().unwrap_or(0);
        let my_epoch = max_epoch + 1;

        // Reconcile in index order; walk the chain.
        let mut prev: Option<Digest> = None;
        let mut next_index = 0u64;
        let mut survivor: Option<SegmentIdentity> = None;
        let mut retention: Option<RetentionClass> = None;
        let mut last_take = 0u64;
        let mut dedupe_index: HashMap<IdemKey, DedupeEntry> = HashMap::new();
        let mut dedupe_by_seq: HashMap<Seq, IdemKey> = HashMap::new();
        for (idx, ep) in &idents {
            let id = SegmentIdentity {
                voyage_id: voyage_id.to_string(),
                segment_index: *idx,
                epoch: *ep,
            };
            match recovery::reconcile(&seg_dir, &id, my_epoch)? {
                Reconciled::ReinitializedOpen => continue, // identity never existed
                Reconciled::StillOpen => {
                    let r = SegmentReader::read(&id.path(&seg_dir, SegmentState::Open), false)?;
                    if r.header.voyage_id != voyage_id {
                        return Err(Error::State(format!(
                            "voyage_id mismatch: store holds {:?}, caller opened {:?}",
                            r.header.voyage_id, voyage_id
                        )));
                    }
                    last_take = last_take.max(walk_segment(&mut dedupe_index, &mut dedupe_by_seq, &r)?);
                    survivor = Some(id);
                    next_index = idx + 1;
                }
                _ => {
                    let sealed = id.path(&seg_dir, SegmentState::Sealed);
                    let r = SegmentReader::read(&sealed, true)?;
                    r.verify_seal()?;
                    // A writer opened under the wrong id must refuse HERE —
                    // not write mismatched headers for verify to find later
                    // (review finding 4).
                    if r.header.voyage_id != voyage_id {
                        return Err(Error::State(format!(
                            "voyage_id mismatch: store holds {:?}, caller opened {:?}",
                            r.header.voyage_id, voyage_id
                        )));
                    }
                    if r.header.prev_seal_digest.as_ref().map(|d| &d.value)
                        != prev.as_ref().map(|d| &d.value)
                    {
                        return Err(Error::Corrupt {
                            offset: 0,
                            what: format!("segment {idx} breaks the seal chain"),
                        });
                    }
                    if *idx == 0 {
                        retention = r.header.retention_class;
                    }
                    last_take = last_take.max(walk_segment(&mut dedupe_index, &mut dedupe_by_seq, &r)?);
                    prev = r.seal.as_ref().map(|s| s.digest.clone());
                    next_index = idx + 1;
                }
            }
        }

        // `root` (borrowed from `pin`) is done being read here -- capture
        // it as an owned path BEFORE moving `pin` itself into the
        // returned store, so the pin's own protection covers every LATER
        // self.root-based operation (open_segment, publish_blob, ...) for
        // the store's whole remaining lifetime, on the same terms as
        // everything open_prepared itself just did.
        let root_owned = root.to_path_buf();
        Ok(Self {
            root: root_owned,
            voyage_id: voyage_id.to_string(),
            _lock: lock,
            _root_pin: pin,
            epoch: my_epoch,
            prev_seal_digest: prev,
            next_segment_index: next_index,
            retention_class: retention.unwrap_or(RetentionClass::Archive),
            last_take_epoch: last_take,
            survivor_open: survivor,
            dedupe_index,
        })
    }

    /// The canonicalized root this store actually operates on — resolved
    /// ONCE at `open_for_writing` and never re-derived from a caller's
    /// possibly-symlinked path afterward. Crate-private: callers that need
    /// to scan the store's own files after opening it (ADR 0040's successor-
    /// closure scan, for one) must use THIS, not whatever path they
    /// originally passed to `open_for_writing` — that path can be a symlink
    /// retargeted after the lock was taken, in which case re-deriving from
    /// it scans whatever it points at NOW, not the store this writer is
    /// fenced to.
    pub(crate) fn resolved_root(&self) -> &Path {
        &self.root
    }

    /// A prior incarnation's clean `.open` tip: seal it under this writer's
    /// authority before opening a fresh segment (one open segment, only at
    /// the tip). Returns its digest for the chain.
    pub fn seal_survivor(&mut self) -> Result<()> {
        let Some(id) = self.survivor_open.take() else {
            return Ok(());
        };
        let seg_dir = self.root.join("seg");
        let open_path = id.path(&seg_dir, SegmentState::Open);
        let reader = SegmentReader::read(&open_path, false)?;
        if reader.tail_tear.is_some() {
            return Err(Error::State("survivor has a tear; reconcile first".into()));
        }
        // Chain check against the current tip.
        if reader.header.prev_seal_digest.as_ref().map(|d| &d.value)
            != self.prev_seal_digest.as_ref().map(|d| &d.value)
        {
            return Err(Error::Corrupt {
                offset: 0,
                what: "survivor breaks the seal chain".into(),
            });
        }
        // Rebuild-and-seal via the recovery staging path with zero
        // truncation (the survivor is clean; this writer stamps the seal).
        fsutil::publish_noreplace(&open_path, &id.path(&seg_dir, SegmentState::Recovering))?;
        recovery::reconcile(&seg_dir, &id, self.epoch)?;
        let sealed = SegmentReader::read(&id.path(&seg_dir, SegmentState::Sealed), true)?;
        sealed.verify_seal()?;
        self.prev_seal_digest = sealed.seal.as_ref().map(|s| s.digest.clone());
        Ok(())
    }

    /// Open the next segment for THIS writer's epoch.
    pub fn open_segment(&mut self, created_wall_ms: i64) -> Result<SegmentWriter> {
        self.open_segment_with_features(created_wall_ms, vec![])
    }

    /// As `open_segment`, declaring required features (ADR 0039 registry) —
    /// every segment an adapter writes under a feature must list it.
    pub fn open_segment_with_features(
        &mut self,
        created_wall_ms: i64,
        required_features: Vec<String>,
    ) -> Result<SegmentWriter> {
        if self.survivor_open.is_some() {
            return Err(Error::State("seal the survivor tip first".into()));
        }
        let index = self.next_segment_index;
        let header = HeaderBody {
            version: 1,
            required_features,
            voyage_id: self.voyage_id.clone(),
            segment_index: index,
            epoch: self.epoch,
            prev_seal_digest: self.prev_seal_digest.clone(),
            created_wall_ms,
            retention_class: (index == 0).then_some(self.retention_class),
        };
        let w = SegmentWriter::create(&self.root.join("seg"), header)?;
        self.next_segment_index += 1;
        Ok(w)
    }

    /// Record a seal digest as the new chain tip (caller sealed a writer).
    pub fn advance_chain(&mut self, digest: Digest) {
        self.prev_seal_digest = Some(digest);
    }

    /// Publish one blob into the CAS: temp → fsync → RENAME_NOREPLACE →
    /// fsync shard dir. EEXIST verifies digest AND length (idempotent
    /// success); mismatch is loud. Returns the digest hex.
    pub fn publish_blob(&self, content: &[u8]) -> Result<String> {
        use sha2::{Digest as _, Sha256};
        let digest = {
            let mut h = Sha256::new();
            h.update(content);
            let mut s = String::with_capacity(64);
            for b in h.finalize() {
                s.push_str(&format!("{:02x}", b));
            }
            s
        };
        let blobs = self.root.join("blobs");
        let shard = blobs.join("sha256").join(&digest[0..2]);
        std::fs::create_dir_all(&shard)?;
        // Anchor bottom-up. `sha256/` is created at bootstrap in stores made
        // since ADR 0041, but voyages bootstrapped by earlier builds lack it
        // (and it can be deleted) — then `create_dir_all` just made it here,
        // and ITS entry lives in `blobs/`. Flushing only `sha256` would let a
        // blob reference become durable while its namespace parent stays
        // losable (round-3 finding; also the migration path for old stores).
        fsutil::fsync_dir(&blobs.join("sha256"))?; // anchors the shard entry
        fsutil::fsync_dir(&blobs)?; // anchors the sha256 entry
        let dest = shard.join(&digest);
        if dest.exists() {
            let existing = std::fs::read(&dest)?;
            if existing.len() != content.len() || existing != content {
                return Err(Error::Corrupt {
                    offset: 0,
                    what: format!("CAS collision at {digest}: existing bytes differ"),
                });
            }
            // Found, not published by US: restate the SAME barrier a fresh
            // publish completes (Part 3 finding). A prior incarnation could
            // have renamed this blob into place and crashed before its own
            // renamed-file/parent flush ran, leaving it cache-visible but
            // not durable — `finish_publication` covers both halves; a bare
            // `fsync_dir(&shard)` covered only the parent.
            fsutil::finish_publication(&dest)?;
            return Ok(digest);
        }
        // Random suffix (not pid+digest): two same-process publishes of
        // identical content must not race each other's temp file.
        let nonce: u64 = {
            let mut b = [0u8; 8];
            getrandom::fill(&mut b).map_err(std::io::Error::from)?;
            u64::from_le_bytes(b)
        };
        let tmp = blobs.join(".tmp").join(format!("{:016x}-{}", nonce, &digest[0..16]));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(content)?;
            f.sync_all()?;
        }
        match fsutil::rename_noreplace_raw(&tmp, &dest) {
            Ok(()) => {}
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Raced an identical publish: verify + clean the temp. Same
                // reasoning as the `dest.exists()` branch above applies to
                // the flush below — this process didn't do the winning
                // rename, so it must not assume the winner finished
                // flushing before it crashed (if it did).
                let existing = std::fs::read(&dest)?;
                std::fs::remove_file(&tmp)?;
                if existing != content {
                    return Err(Error::Corrupt {
                        offset: 0,
                        what: format!("CAS collision at {digest}: existing bytes differ"),
                    });
                }
            }
            Err(e) => return Err(e),
        }
        fsutil::finish_publication(&dest)?;
        Ok(digest)
    }
}

#[cfg(all(test, any(target_os = "linux", windows)))]
mod tests {
    use super::*;
    use crate::envelope::{Actor, ActorKind, Class, Derivation, Emitter, Envelope, FrameRef, RefKind, Source};
    use crate::segment::{tests::test_env, Commit};

    /// A conforming standalone frame (lifecycle needs no attached_to).
    fn lc(epoch: u64, n: u64) -> crate::envelope::Envelope {
        let mut e = test_env(epoch, n);
        e.class = Class::Lifecycle;
        e.payload = Some(serde_json::json!({"kind": "producer_ready"}));
        e
    }

    /// A controller-actor frame (`Actor.kind=controller` requires
    /// `controller_id`+`take_epoch` — ADR 0039's cross-field matrix), the
    /// shape both `input` and `input_fact` frames use in `capsule_win.rs`'s
    /// real WAL.
    fn ctrl_env(epoch: u64, n: u64, class: Class, payload: serde_json::Value, refs: Vec<FrameRef>) -> Envelope {
        Envelope {
            seq: Seq { epoch, n },
            class,
            source: Source {
                emitter: Emitter::Capsule,
                actor: Actor {
                    kind: ActorKind::Controller,
                    controller_id: Some("ctrl".into()),
                    take_epoch: Some(2),
                },
                derivation: Derivation::Synthetic,
            },
            t_wall_ms: 1_756_000_000_000,
            t_mono_us: n * 1000,
            stream: None,
            transformed: None,
            refs,
            payload: Some(payload),
            payload_ref: None,
        }
    }

    /// A `take_state` lifecycle frame (revoke-first / grant), the preamble
    /// every real capsule commits before any producer-bound action.
    fn lc_take(epoch: u64, n: u64, take_epoch: u64, holder: Option<&str>) -> Envelope {
        let mut e = lc(epoch, n);
        e.payload = Some(serde_json::json!({"kind": "take_state", "take": {"take_epoch": take_epoch, "holder": holder}}));
        e
    }

    fn input_env(epoch: u64, n: u64, idem_key: &str) -> Envelope {
        ctrl_env(
            epoch,
            n,
            Class::Input,
            serde_json::json!({"idem_key": idem_key, "content": "redacted", "length": 3}),
            vec![],
        )
    }

    fn intent_env(epoch: u64, n: u64, input: Seq) -> Envelope {
        ctrl_env(
            epoch,
            n,
            Class::Lifecycle,
            serde_json::json!({"kind": "input_fact",
                "fact": {"input": {"epoch": input.epoch, "n": input.n}, "fact": "forward_intent"}}),
            vec![FrameRef { kind: RefKind::CausedBy, frame: input }],
        )
    }

    fn forwarded_env(epoch: u64, n: u64, input: Seq, intent: Seq) -> Envelope {
        ctrl_env(
            epoch,
            n,
            Class::Lifecycle,
            serde_json::json!({"kind": "input_fact",
                "fact": {"input": {"epoch": input.epoch, "n": input.n}, "fact": "forwarded",
                         "intent": {"epoch": intent.epoch, "n": intent.n}}}),
            vec![FrameRef { kind: RefKind::CausedBy, frame: input }],
        )
    }

    fn refused_env(epoch: u64, n: u64, input: Seq) -> Envelope {
        ctrl_env(
            epoch,
            n,
            Class::Lifecycle,
            serde_json::json!({"kind": "input_fact",
                "fact": {"input": {"epoch": input.epoch, "n": input.n}, "fact": "refused_stale_epoch"}}),
            vec![FrameRef { kind: RefKind::CausedBy, frame: input }],
        )
    }

    /// The dedupe index, from a voyage exercising every legal `idem_key`
    /// chain shape (ADR 0039's exact five, minus `{…,observed}` which
    /// `DedupeState` deliberately folds into `Forwarded` — see its doc),
    /// folded by `open_for_writing`'s OWN existing frame walk. "No second
    /// scan" is asserted by construction, not timing: `walk_segment` is
    /// called only from inside that walk (visible in this module's source,
    /// `open_for_writing` above), computing BOTH the max take epoch and the
    /// dedupe fold in one pass — there is no second call site anywhere in
    /// the crate. Reopening as a SUCCESSOR incarnation (a fresh
    /// `open_for_writing`, not the same store handle) proves the index
    /// survives an epoch boundary — the exact case decision 5 names: "a
    /// successor capsule starting with an empty index would let a
    /// pre-crash `forwarded` key re-forward".
    #[test]
    fn dedupe_index_folds_every_legal_chain_shape_across_a_capsule_restart() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voyd");
        VoyageStore::bootstrap(&root, "voyd", RetentionClass::Discard).unwrap();

        let key_input_only = "1".repeat(32);
        let key_intent_only = "2".repeat(32);
        let key_forwarded = "3".repeat(32);
        let key_refused = "4".repeat(32);

        {
            let mut store = VoyageStore::open_for_writing(&root, "voyd").unwrap();
            let mut w = store.open_segment(0).unwrap();
            // The take preamble every real capsule commits: revoke-first
            // (null holder), then grant -- required for the verifier's
            // take-matrix check (a controller frame's declared take_epoch
            // must match committed state).
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            // {input} only.
            w.append(&input_env(1, 3, &key_input_only), Commit::Immediate).unwrap();
            // {input, intent}.
            w.append(&input_env(1, 4, &key_intent_only), Commit::Immediate).unwrap();
            w.append(&intent_env(1, 5, Seq { epoch: 1, n: 4 }), Commit::Immediate).unwrap();
            // {input, intent, forwarded}.
            w.append(&input_env(1, 6, &key_forwarded), Commit::Immediate).unwrap();
            w.append(&intent_env(1, 7, Seq { epoch: 1, n: 6 }), Commit::Immediate).unwrap();
            w.append(
                &forwarded_env(1, 8, Seq { epoch: 1, n: 6 }, Seq { epoch: 1, n: 7 }),
                Commit::Immediate,
            )
            .unwrap();
            // {input, refused} -- refusal skips intent entirely (ADR 0039's
            // exact chain list has no {input,intent,refused} member).
            w.append(&input_env(1, 9, &key_refused), Commit::Immediate).unwrap();
            w.append(&refused_env(1, 10, Seq { epoch: 1, n: 9 }), Commit::Immediate).unwrap();
            let d = w.seal(None).unwrap();
            store.advance_chain(d);

            crate::verify::verify_voyage(&root, "voyd").unwrap();
        }

        // Reopen as a successor incarnation -- the index must be rebuilt
        // from the retained voyage, not start empty.
        let store = VoyageStore::open_for_writing(&root, "voyd").unwrap();
        let key = |s: &str| -> IdemKey { parse_idem_key(s).unwrap() };

        let e = store.dedupe_index[&key(&key_input_only)];
        assert_eq!(e.state, DedupeState::Input);
        assert_eq!(e.input, Seq { epoch: 1, n: 3 });
        assert_eq!(e.intent, None);

        let e = store.dedupe_index[&key(&key_intent_only)];
        assert_eq!(e.state, DedupeState::Intent);
        assert_eq!(e.input, Seq { epoch: 1, n: 4 });
        assert_eq!(e.intent, Some(Seq { epoch: 1, n: 5 }));

        let e = store.dedupe_index[&key(&key_forwarded)];
        assert_eq!(e.state, DedupeState::Forwarded);
        assert_eq!(e.input, Seq { epoch: 1, n: 6 });
        assert_eq!(e.intent, Some(Seq { epoch: 1, n: 7 }));

        let e = store.dedupe_index[&key(&key_refused)];
        assert_eq!(e.state, DedupeState::Refused);
        assert_eq!(e.input, Seq { epoch: 1, n: 9 });
        assert_eq!(e.intent, None);

        assert_eq!(store.dedupe_index.len(), 4, "no extra/spurious entries");
    }

    /// Finding 8: the fold FAILS CLOSED on retained history it cannot
    /// trust, rather than silently degrading the index. Four ways a
    /// segment can be untrustworthy, each its own test below: a duplicate
    /// `idem_key` across two `input` frames (previously last-writer-wins);
    /// an `input_fact` naming a `Seq` that was never a committed `input`
    /// frame (previously silently skipped); a fact-kind illegal from its
    /// idem_key's current lattice state, mirroring `verify.rs`'s own
    /// `FactState` machine (previously accepted unconditionally); and a
    /// malformed (non-lowercase-hex32) `idem_key` (previously silently
    /// skipped, which could verify green yet omit an identity from the
    /// index and re-forward it).
    #[test]
    fn dedupe_fold_rejects_a_duplicate_idem_key_across_two_input_frames() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voydup");
        VoyageStore::bootstrap(&root, "voydup", RetentionClass::Discard).unwrap();
        let dup_key = "5".repeat(32);
        {
            let mut store = VoyageStore::open_for_writing(&root, "voydup").unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            w.append(&input_env(1, 3, &dup_key), Commit::Immediate).unwrap();
            w.append(&input_env(1, 4, &dup_key), Commit::Immediate).unwrap(); // SAME key, second input
            w.seal(None).unwrap();
        }
        let Err(err) = VoyageStore::open_for_writing(&root, "voydup") else { panic!("expected open_for_writing to fail") };
        assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");
    }

    #[test]
    fn dedupe_fold_rejects_a_fact_naming_an_unresolvable_input() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voybadref");
        VoyageStore::bootstrap(&root, "voybadref", RetentionClass::Discard).unwrap();
        {
            let mut store = VoyageStore::open_for_writing(&root, "voybadref").unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            // A forward_intent fact naming a Seq that was never an input
            // frame.
            w.append(&intent_env(1, 3, Seq { epoch: 1, n: 99 }), Commit::Immediate).unwrap();
            w.seal(None).unwrap();
        }
        let Err(err) = VoyageStore::open_for_writing(&root, "voybadref") else { panic!("expected open_for_writing to fail") };
        assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");
    }

    #[test]
    fn dedupe_fold_rejects_an_illegal_state_transition() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voyillegal");
        VoyageStore::bootstrap(&root, "voyillegal", RetentionClass::Discard).unwrap();
        let key = "6".repeat(32);
        {
            let mut store = VoyageStore::open_for_writing(&root, "voyillegal").unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            w.append(&input_env(1, 3, &key), Commit::Immediate).unwrap();
            // `forwarded` directly from the `Input` state, skipping
            // `forward_intent` entirely -- illegal (mirrors verify.rs's
            // own FactState lattice).
            let input_seq = Seq { epoch: 1, n: 3 };
            w.append(&forwarded_env(1, 4, input_seq, input_seq), Commit::Immediate).unwrap();
            w.seal(None).unwrap();
        }
        let Err(err) = VoyageStore::open_for_writing(&root, "voyillegal") else { panic!("expected open_for_writing to fail") };
        assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");
    }

    #[test]
    fn dedupe_fold_rejects_a_malformed_idem_key() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voybadkey");
        VoyageStore::bootstrap(&root, "voybadkey", RetentionClass::Discard).unwrap();
        {
            let mut store = VoyageStore::open_for_writing(&root, "voybadkey").unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            // Uppercase hex: structurally a 32-char string, not lowercase
            // hex32.
            w.append(&input_env(1, 3, &"A".repeat(32)), Commit::Immediate).unwrap();
            w.seal(None).unwrap();
        }
        let Err(err) = VoyageStore::open_for_writing(&root, "voybadkey") else { panic!("expected open_for_writing to fail") };
        assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");
    }

    /// Round-2 review, finding 6: an `input` frame whose `idem_key` field
    /// is entirely ABSENT (not merely malformed) must fail closed too --
    /// previously a silent `continue` that dropped the frame out of the
    /// index without a trace.
    #[test]
    fn dedupe_fold_rejects_an_input_frame_missing_idem_key_entirely() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voynokey");
        VoyageStore::bootstrap(&root, "voynokey", RetentionClass::Discard).unwrap();
        {
            let mut store = VoyageStore::open_for_writing(&root, "voynokey").unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            let no_key = ctrl_env(1, 3, Class::Input, serde_json::json!({"content": "redacted", "length": 3}), vec![]);
            w.append(&no_key, Commit::Immediate).unwrap();
            w.seal(None).unwrap();
        }
        let Err(err) = VoyageStore::open_for_writing(&root, "voynokey") else { panic!("expected open_for_writing to fail") };
        assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");
    }

    /// Round-2 review, finding 6: an `input_fact` lifecycle frame with no
    /// `fact` object at all must fail closed -- previously a silent
    /// `continue`.
    #[test]
    fn dedupe_fold_rejects_an_input_fact_missing_its_fact_object() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voynofact");
        VoyageStore::bootstrap(&root, "voynofact", RetentionClass::Discard).unwrap();
        {
            let mut store = VoyageStore::open_for_writing(&root, "voynofact").unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            let key = "7".repeat(32);
            w.append(&input_env(1, 3, &key), Commit::Immediate).unwrap();
            let no_fact = ctrl_env(1, 4, Class::Lifecycle, serde_json::json!({"kind": "input_fact"}), vec![]);
            w.append(&no_fact, Commit::Immediate).unwrap();
            w.seal(None).unwrap();
        }
        let Err(err) = VoyageStore::open_for_writing(&root, "voynofact") else { panic!("expected open_for_writing to fail") };
        assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");
    }

    /// Round-2 review, finding 6 (last clause): a `forwarded` fact whose
    /// `intent` does not name the SAME `forward_intent` frame this
    /// idem_key's own chain recorded must fail closed -- previously
    /// unchecked entirely (the fold never even read `intent`).
    #[test]
    fn dedupe_fold_rejects_a_forwarded_intent_that_does_not_match_the_recorded_intent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voybadintent");
        VoyageStore::bootstrap(&root, "voybadintent", RetentionClass::Discard).unwrap();
        {
            let mut store = VoyageStore::open_for_writing(&root, "voybadintent").unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            let key = "8".repeat(32);
            let input_seq = Seq { epoch: 1, n: 3 };
            w.append(&input_env(1, 3, &key), Commit::Immediate).unwrap();
            w.append(&intent_env(1, 4, input_seq), Commit::Immediate).unwrap(); // the REAL forward_intent is seq (1,4)
            // `forwarded.intent` claims the INPUT frame's own seq instead
            // of the real forward_intent frame's seq -- wrong reference.
            w.append(&forwarded_env(1, 5, input_seq, input_seq), Commit::Immediate).unwrap();
            w.seal(None).unwrap();
        }
        let Err(err) = VoyageStore::open_for_writing(&root, "voybadintent") else { panic!("expected open_for_writing to fail") };
        assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");
    }

    /// The concurrent-bootstrap race a shared staging pathname allowed
    /// (review finding on the DACL unit): with one `.creating` path, attempt
    /// B could delete attempt A's populated, flushed staging and substitute
    /// an empty directory between A's flushes and A's rename — A then
    /// publishes a voyage NOBODY flushed. Attempt-owned random staging names
    /// dissolve the shared path entirely; `publish_noreplace` arbitrates.
    /// This drives both attempts through a start barrier and requires:
    /// exactly one winner, a verify-green published voyage (never an empty
    /// or hybrid one), and zero staging residue once both attempts and the
    /// winner's sweep are done.
    #[test]
    fn concurrent_bootstraps_publish_exactly_one_verifiable_voyage() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voyr");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let results: Vec<_> = (0..2)
            .map(|_| {
                let root = root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    VoyageStore::bootstrap(&root, "voyr", RetentionClass::Discard)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();

        let winners = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(winners, 1, "exactly one bootstrap must win: {results:?}");
        // The published voyage is complete and internally consistent — the
        // race's failure mode was an EMPTY root published as success.
        crate::verify::verify_voyage(&root, "voyr").unwrap();
        let store = VoyageStore::open_for_writing(&root, "voyr").unwrap();
        drop(store);
        // Loser's guard plus winner's sweep leave no attempt residue.
        let residue: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("voyr.creating"))
            .collect();
        assert!(residue.is_empty(), "staging residue: {residue:?}");
    }

    #[test]
    fn bootstrap_open_write_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voy1");
        VoyageStore::bootstrap(&root, "voy1", RetentionClass::Discard).unwrap();
        assert!(root.join("seg").is_dir());
        assert!(root.join("blobs").join(".tmp").is_dir());
        // No staging residue of any attempt survives a successful bootstrap.
        let residue: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("voy1.creating"))
            .collect();
        assert!(residue.is_empty(), "staging residue left behind: {residue:?}");

        // Incarnation 1: epoch 1, write + seal one segment.
        {
            let mut store = VoyageStore::open_for_writing(&root, "voy1").unwrap();
            assert_eq!(store.epoch, 1);
            let mut w = store.open_segment(0).unwrap();
            for n in 1..=2 {
                w.append(&lc(1, n), Commit::Immediate).unwrap();
            }
            let d = w.seal(None).unwrap();
            store.advance_chain(d);
            // Leave a second segment OPEN with one frame (the survivor).
            let mut w2 = store.open_segment(0).unwrap();
            w2.append(&lc(1, 3), Commit::Immediate).unwrap();
            // Dropped without sealing: simulates writer death.
        }

        // Incarnation 2: epoch = 2, survivor sealed, chain continues.
        let mut store = VoyageStore::open_for_writing(&root, "voy1").unwrap();
        assert_eq!(store.epoch, 2);
        store.seal_survivor().unwrap();
        let mut w = store.open_segment(0).unwrap();
        assert_eq!(w.identity().segment_index, 2);
        w.append(&lc(2, 1), Commit::Immediate).unwrap();
        let d = w.seal(None).unwrap();
        store.advance_chain(d);

        // Verify the whole voyage.
        crate::verify::verify_voyage(&root, "voy1").unwrap();
    }

    #[test]
    fn second_writer_is_fenced() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voy2");
        VoyageStore::bootstrap(&root, "voy2", RetentionClass::Discard).unwrap();
        let _first = VoyageStore::open_for_writing(&root, "voy2").unwrap();
        let second = VoyageStore::open_for_writing(&root, "voy2");
        assert!(matches!(second, Err(Error::State(_))));
    }

    /// U1a (ADR 0041 Lifecycle "Discovery, and the two windows"): a broken
    /// parent-death lease refuses with the dedicated typed error, and — the
    /// ADR's own "releases the fence and exits without binding" — the fence
    /// is provably free again immediately afterward: a fresh open right
    /// after the broken-lease attempt must succeed, not see a lock the
    /// failed attempt left held.
    #[test]
    fn lease_broken_releases_the_fence_and_exits_without_binding() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voylease");
        VoyageStore::bootstrap(&root, "voylease", RetentionClass::Discard).unwrap();

        let broken: &dyn Fn() -> bool = &|| true;
        let result = VoyageStore::open_for_writing_with_lease(&root, "voylease", Some(broken));
        assert!(matches!(result, Err(Error::LeaseBroken)), "{:?}", result.err());

        let reopened = VoyageStore::open_for_writing(&root, "voylease");
        assert!(
            reopened.is_ok(),
            "the fence must be released on a broken lease, not left held: {:?}",
            reopened.err()
        );
    }

    /// U1a Codex round-1, minor cluster: the EARLIER lease tests prove
    /// error ORDERING (lease-broken beats history corruption) and
    /// post-release availability, but neither actually proves the
    /// callback runs WHILE the fence is held — it could, in principle, run
    /// after `open_for_writing_with_lease` released it and still pass
    /// those tests. This one proves it directly: the callback ITSELF
    /// attempts a second concurrent open on the SAME root and must observe
    /// lock contention, then reports the lease as intact so the OUTER open
    /// still succeeds — proving both that the fence was genuinely held
    /// at the moment of the call, and that the overall function still
    /// works end to end once the callback returns.
    #[test]
    fn lease_callback_runs_while_the_fence_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voyleasefence");
        VoyageStore::bootstrap(&root, "voyleasefence", RetentionClass::Discard).unwrap();

        let root_for_probe = root.clone();
        let probe = move || {
            let second = VoyageStore::open_for_writing(&root_for_probe, "voyleasefence");
            assert!(
                matches!(second, Err(Error::State(_))),
                "the fence must still be held while the lease callback runs: {:?}",
                second.err()
            );
            false // lease intact -- let the outer open proceed
        };
        let store = VoyageStore::open_for_writing_with_lease(&root, "voyleasefence", Some(&probe));
        assert!(store.is_ok(), "{:?}", store.err());
    }

    /// A lease supplied but reporting itself intact is a pure no-op —
    /// `open_for_writing_with_lease(..., Some(&|| false))` must behave
    /// identically to `open_for_writing`'s own no-lease default.
    #[test]
    fn lease_intact_does_not_affect_an_ordinary_open() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voyleaseok");
        VoyageStore::bootstrap(&root, "voyleaseok", RetentionClass::Discard).unwrap();

        let intact: &dyn Fn() -> bool = &|| false;
        let store = VoyageStore::open_for_writing_with_lease(&root, "voyleaseok", Some(intact));
        assert!(store.is_ok(), "{:?}", store.err());
    }

    /// U1a: the fence and lease check run BEFORE history traversal, not
    /// after — proven by a voyage whose sealed history is genuinely
    /// corrupt (the same "bad forward-intent reference" fixture
    /// `dedupe_fold_rejects_a_fact_naming_an_unresolvable_input` uses,
    /// confirmed below to still fail the way that test expects for an
    /// ordinary open). A BROKEN lease against this same store must refuse
    /// with `Error::LeaseBroken`, never the history walk's own
    /// `Error::Schema` — if the walk ran first, the corruption would win
    /// the race and mask the lease check entirely.
    #[test]
    fn lease_check_precedes_history_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voyleasehist");
        VoyageStore::bootstrap(&root, "voyleasehist", RetentionClass::Discard).unwrap();
        {
            let mut store = VoyageStore::open_for_writing(&root, "voyleasehist").unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            // A forward_intent fact naming a Seq that was never an input
            // frame -- the exact fixture the existing dedupe-fold test uses,
            // reused here so the corruption is provably genuine, not a
            // stand-in.
            w.append(&intent_env(1, 3, Seq { epoch: 1, n: 99 }), Commit::Immediate).unwrap();
            w.seal(None).unwrap();
        }

        // Confirm the corruption is real and reachable via an ordinary,
        // no-lease open -- otherwise the test below would prove nothing.
        let Err(err) = VoyageStore::open_for_writing(&root, "voyleasehist") else {
            panic!("expected the corrupt history to fail an ordinary open")
        };
        assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");

        // The SAME store, with a broken lease: the lease check must win
        // the race against the corrupt history walk.
        let broken: &dyn Fn() -> bool = &|| true;
        let result = VoyageStore::open_for_writing_with_lease(&root, "voyleasehist", Some(broken));
        assert!(
            matches!(result, Err(Error::LeaseBroken)),
            "the lease check must run before history traversal ever sees the corruption: {:?}",
            result.err()
        );
    }

    #[test]
    fn blob_publish_idempotent_and_collision_loud() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voy3");
        VoyageStore::bootstrap(&root, "voy3", RetentionClass::Discard).unwrap();
        let store = VoyageStore::open_for_writing(&root, "voy3").unwrap();
        let d1 = store.publish_blob(b"hello").unwrap();
        let d2 = store.publish_blob(b"hello").unwrap();
        assert_eq!(d1, d2);
        let path = root.join("blobs").join("sha256").join(&d1[0..2]).join(&d1);
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        // Forged content under the same name: loud on the next publish.
        std::fs::write(&path, b"evil!").unwrap();
        assert!(store.publish_blob(b"hello").is_err());
    }

    /// Part 2 finding, reproduced: bootstrap two real stores A and B, open
    /// via a symlink pointed at A, retarget the symlink to B AFTER the fence
    /// is taken, then keep writing. A writer that re-resolved the unresolved
    /// alias at each later syscall would hold A's lock but write segments
    /// into B; canonicalizing once in `open_for_writing` and storing the
    /// result must keep every later operation on A regardless of where the
    /// alias points now.
    #[test]
    #[cfg(unix)]
    fn root_alias_cannot_escape_fence_after_open() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        VoyageStore::bootstrap(&a, "voy", RetentionClass::Discard).unwrap();
        VoyageStore::bootstrap(&b, "voy", RetentionClass::Discard).unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&a, &alias).unwrap();

        let mut store = VoyageStore::open_for_writing(&alias, "voy").unwrap();

        // Retarget AFTER the fence is taken.
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&b, &alias).unwrap();

        let mut w = store.open_segment(0).unwrap();
        w.append(&lc(1, 1), Commit::Immediate).unwrap();
        w.seal(None).unwrap();

        let has_sealed = |dir: &std::path::Path| {
            std::fs::read_dir(dir.join("seg"))
                .unwrap()
                .any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".sotseg"))
        };
        assert!(has_sealed(&a), "writer must operate on A, resolved at open time");
        assert!(!has_sealed(&b), "writer must NOT follow a post-open retarget into B");
    }

    /// ADR 0041 Codex round-1, Major 5 discharge: `prepare_root` +
    /// `open_prepared` composed manually (the SAME composition
    /// `open_for_writing` performs internally) reproduces an ordinary
    /// open end to end — proving the split itself changes nothing
    /// observable for a caller that performs both halves itself.
    #[test]
    fn prepare_root_then_open_prepared_reproduces_an_ordinary_open() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voysplit");
        VoyageStore::bootstrap(&root, "voysplit", RetentionClass::Discard).unwrap();

        let prepared = VoyageStore::prepare_root(&root).unwrap();
        assert_eq!(prepared.path(), std::fs::canonicalize(&root).unwrap());

        let mut store = VoyageStore::open_prepared(&prepared, "voysplit", None).unwrap();
        let mut w = store.open_segment(0).unwrap();
        w.append(&lc(1, 1), Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        crate::verify::verify_voyage(&root, "voysplit").unwrap();
    }

    /// ADR 0041 Codex round-2 discharge: `open_prepared`'s kernel-identity
    /// check catches the resolved path ITSELF being retargeted (its
    /// directory entry removed and replaced by a symlink) in the gap
    /// between `prepare_root` and this process acquiring the fence — the
    /// scenario the split's own safety reasoning depends on, distinct from
    /// `root_alias_cannot_escape_fence_after_open` above (which retargets
    /// an ALIAS TO the resolved path, never the resolved path itself, and
    /// so never triggers this check at all).
    #[test]
    #[cfg(unix)]
    fn open_prepared_refuses_when_the_canonical_root_itself_is_retargeted_before_the_fence() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        VoyageStore::bootstrap(&a, "voy", RetentionClass::Discard).unwrap();
        VoyageStore::bootstrap(&b, "voy", RetentionClass::Discard).unwrap();

        let prepared = VoyageStore::prepare_root(&a).unwrap();

        // Retarget the PREPARED PATH ITSELF (not merely an alias to it) --
        // A's own directory entry now resolves into B.
        std::fs::remove_dir_all(&a).unwrap();
        std::os::unix::fs::symlink(&b, &a).unwrap();

        let result = VoyageStore::open_prepared(&prepared, "voy", None);
        assert!(
            matches!(result, Err(Error::State(_))),
            "open_prepared must refuse when the prepared path's kernel identity no longer matches: {:?}",
            result.err()
        );
    }

    /// ADR 0041 Codex round-2 discharge: the reported gap, reproduced
    /// exactly. A live reproducer proved the ORIGINAL check (re-canonicalize
    /// and compare path TEXT) blind to this: bootstrap two stores, prepare
    /// ONE of them, rename it aside, then rename the OTHER into its exact
    /// former pathname -- `std::fs::canonicalize` on a plain directory
    /// (no symlink anywhere in the chain) returns the identical string
    /// before and after, so a path-text check sees nothing wrong and
    /// `open_prepared` would silently open the WRONG STORE. The
    /// kernel-identity check (`(st_dev, st_ino)`, read off the handle this
    /// function itself opens) catches it: the directory now at that path
    /// is a genuinely different object, identity differs, and the open is
    /// refused.
    #[test]
    #[cfg(unix)]
    fn open_prepared_refuses_a_same_pathname_directory_swap() {
        let dir = tempfile::tempdir().unwrap();
        let prepared_path = dir.path().join("a");
        let replacement_source = dir.path().join("b");
        let original_moved = dir.path().join("a-original");
        VoyageStore::bootstrap(&prepared_path, "voy", RetentionClass::Discard).unwrap();
        VoyageStore::bootstrap(&replacement_source, "voy", RetentionClass::Discard).unwrap();

        let prepared = VoyageStore::prepare_root(&prepared_path).unwrap();

        // The swap: move A aside, then move B into A's exact former
        // pathname. No symlink anywhere -- re-canonicalizing `prepared`'s
        // own path string would return the SAME string it always did.
        std::fs::rename(&prepared_path, &original_moved).unwrap();
        std::fs::rename(&replacement_source, &prepared_path).unwrap();

        let result = VoyageStore::open_prepared(&prepared, "voy", None);
        assert!(
            matches!(result, Err(Error::State(_))),
            "a same-pathname directory swap must be refused via kernel identity, not silently opened: {:?}",
            result.err()
        );
    }

    /// The happy path, unchanged: `prepare_root` then `open_prepared` with
    /// NOTHING disturbed in between still succeeds -- the identity check
    /// is a genuine verification, not a check that merely always fails or
    /// always passes regardless of what actually happened.
    #[test]
    fn open_prepared_succeeds_when_nothing_is_disturbed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voyintact");
        VoyageStore::bootstrap(&root, "voyintact", RetentionClass::Discard).unwrap();

        let prepared = VoyageStore::prepare_root(&root).unwrap();
        let store = VoyageStore::open_prepared(&prepared, "voyintact", None);
        assert!(store.is_ok(), "{:?}", store.err());
    }

    /// ADR 0041 Codex round-2b: the reported gap, reproduced. A live repro
    /// proved round-2's check-once-then-reopen-by-path design blind to an
    /// ONGOING `RENAME_EXCHANGE` storm: it verified identity once, via a
    /// transient open, then re-opened by PATH for the writer lock,
    /// preflight, both flushes, and the segment directory -- any of which
    /// a swap landing AFTER the check could still redirect. `seed_segments`
    /// gives A and B a RELIABLE discriminator (`next_segment_index`, a
    /// `pub` field on `VoyageStore`) -- NOT `retention_class`, which the
    /// original repro used: `VoyageStore::bootstrap`'s own `retention`
    /// parameter is not yet threaded into a fresh voyage's genesis segment
    /// (a separate, pre-existing gap, unrelated to this fix -- see that
    /// function's own `let _ = (voyage_id, retention)`), so EVERY freshly
    /// opened store defaults to `RetentionClass::Archive` regardless of
    /// what `bootstrap` was asked for, making that field unable to tell
    /// the two stores apart at all.
    ///
    /// Bounded runtime (a fixed WALL-CLOCK window, not a fixed iteration
    /// count like the original repro's `0..20_000`): the storm keeps
    /// racing for 1.5s. This value is empirically motivated, not a round
    /// guess: reverting this fix and running this exact test against the
    /// resulting (round-2) code, a 300ms window was NOT reliably long
    /// enough to observe the wrong-store open at all on this development
    /// machine, while 3s reliably was — 1.5s is a deliberately generous
    /// middle ground so this test would actually CATCH a reintroduced
    /// version of the bug, not merely fail to prove one that happens to
    /// dodge a too-short window.
    #[test]
    #[cfg(target_os = "linux")]
    fn open_prepared_refuses_under_a_sustained_rename_exchange_storm() {
        fn seed_segments(root: &Path, count: u64) {
            VoyageStore::bootstrap(root, "voy", RetentionClass::Discard).unwrap();
            let mut store = VoyageStore::open_for_writing(root, "voy").unwrap();
            for _ in 0..count {
                let w = store.open_segment(0).unwrap();
                let digest = w.seal(None).unwrap();
                store.advance_chain(digest);
            }
        }

        // `renameat2(RENAME_EXCHANGE)`: atomically swaps what `a` and `b`
        // each name, over and over -- the exact primitive the reported
        // repro used.
        fn exchange(a: &Path, b: &Path) -> std::io::Result<()> {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt;
            let a = CString::new(a.as_os_str().as_bytes()).unwrap();
            let b = CString::new(b.as_os_str().as_bytes()).unwrap();
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_renameat2,
                    libc::AT_FDCWD,
                    a.as_ptr(),
                    libc::AT_FDCWD,
                    b.as_ptr(),
                    libc::RENAME_EXCHANGE,
                )
            };
            if rc == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        seed_segments(&a, 1);
        seed_segments(&b, 3);
        let prepared = VoyageStore::prepare_root(&a).unwrap();

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_stop = std::sync::Arc::clone(&stop);
        let (thread_a, thread_b) = (a.clone(), b.clone());
        let swapper = std::thread::spawn(move || {
            while !thread_stop.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = exchange(&thread_a, &thread_b);
            }
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
        let mut attempts: u32 = 0;
        let mut refusals: u32 = 0;
        let mut wrong_store_opened = false;
        while std::time::Instant::now() < deadline {
            attempts += 1;
            match VoyageStore::open_prepared(&prepared, "voy", None) {
                Ok(store) if store.next_segment_index != 1 => {
                    wrong_store_opened = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => refusals += 1,
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        swapper.join().unwrap();

        assert!(
            !wrong_store_opened,
            "open_prepared must never succeed with the replacement store's history"
        );
        assert!(attempts > 0, "the storm loop never even ran once");
        assert!(
            refusals > 0,
            "the storm never actually raced the check within {attempts} attempts -- \
             widen the window or the loop is not exercising the race at all"
        );
    }

    /// Part 3 finding: the CAS `dest.exists()` replay path must restate the
    /// publication barrier over a blob this process didn't itself publish,
    /// not merely fsync its shard directory. Holding the existing blob open
    /// write-denied fails the renamed-target flush `finish_publication`
    /// performs — while the CAS byte-compare read, which the old code also
    /// performed, still succeeds — proving the flush is actually attempted.
    #[test]
    #[cfg(windows)]
    fn cas_replay_reflushes_existing_blob_on_windows() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voy4");
        VoyageStore::bootstrap(&root, "voy4", RetentionClass::Discard).unwrap();
        let store = VoyageStore::open_for_writing(&root, "voy4").unwrap();
        let d1 = store.publish_blob(b"hello").unwrap();
        let path = root.join("blobs").join("sha256").join(&d1[0..2]).join(&d1);

        // FILE_SHARE_READ, deny write: see `recovery.rs`'s `hold_with_share`
        // doc for why the hold must not block anything old code also does —
        // here that's only the CAS byte-compare read, so read-only sharing
        // is enough (unlike `recovering_alone_...` there, this path never
        // deletes `path`, so there is no deletion-denial trap to avoid).
        let _held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
            .open(&path)
            .unwrap();

        let e = store.publish_blob(b"hello").unwrap_err();
        assert!(matches!(e, Error::Io(_)), "{e}");
    }

    /// Independently derive this process's own token-user SID as a string —
    /// deliberately NOT calling into `fsutil`'s private
    /// `owner_protected_descriptor`, so a bug in THAT helper's SID lookup
    /// could not also hide from these tests.
    #[cfg(windows)]
    fn current_user_sid_string() -> String {
        use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
        use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
        use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            assert_ne!(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token), 0);
            let mut needed: u32 = 0;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
            assert!(needed > 0, "GetTokenInformation sizing call returned zero length");
            let words = (needed as usize).div_ceil(8); // u64-backed: TOKEN_USER holds a pointer field
            let mut buf: Vec<u64> = vec![0u64; words];
            let buf_ptr = buf.as_mut_ptr().cast::<u8>();
            assert_ne!(
                GetTokenInformation(token, TokenUser, buf_ptr.cast(), needed, &mut needed),
                0
            );
            let sid = (*buf_ptr.cast::<TOKEN_USER>()).User.Sid;
            let mut sid_str: *mut u16 = std::ptr::null_mut();
            assert_ne!(ConvertSidToStringSidW(sid, &mut sid_str), 0);
            let len = (0..).take_while(|&i| *sid_str.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(sid_str, len));
            LocalFree(sid_str as _);
            CloseHandle(token);
            s
        }
    }

    /// Round-trip `path`'s security descriptor to SDDL text via
    /// `GetNamedSecurityInfoW` + `ConvertSecurityDescriptorToStringSecurityDescriptorW`
    /// — far simpler and less error-prone in a test that cannot be compiled
    /// here than manually walking `ACL`/`ACE` binary structures with
    /// `GetAce`. Requests DACL + PROTECTED_DACL info only (no owner/group/
    /// sacl): the SDDL comes back as `D:P(...)` when protected, `D:(...)`
    /// when not, with each ACE's inherit/inherited flags spelled out as
    /// letters (`OICI` = object+container inherit, `ID` = inherited).
    #[cfg(windows)]
    fn security_descriptor_sddl(path: &std::path::Path) -> String {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW, SDDL_REVISION_1,
            SE_FILE_OBJECT,
        };
        // DACL_SECURITY_INFORMATION alone: the PROTECTED_ flag is SET-ONLY
        // (Microsoft's SECURITY_INFORMATION table marks its query right
        // "not available") — the P in the returned SDDL comes from the
        // descriptor's own control field, not from asking for it.
        use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        unsafe {
            let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            let rc = GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut psd,
            );
            assert_eq!(rc, 0, "GetNamedSecurityInfoW failed: {rc}");
            let mut sddl_ptr: *mut u16 = std::ptr::null_mut();
            let mut sddl_len: u32 = 0;
            let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                psd,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut sddl_ptr,
                &mut sddl_len,
            );
            assert_ne!(ok, 0, "ConvertSecurityDescriptorToStringSecurityDescriptorW failed");
            let len = (0..).take_while(|&i| *sddl_ptr.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(sddl_ptr, len));
            LocalFree(sddl_ptr as _);
            LocalFree(psd as _);
            s
        }
    }

    /// Round-trip an SDDL STRING through the converter pair (string -> SD ->
    /// string) to the converter's own canonical form. The first CI run on a
    /// real Windows machine taught why comparing raw SID strings to
    /// converter output is wrong: `ConvertSecurityDescriptorToString...`
    /// compresses well-known SIDs to their two-letter SDDL aliases — the
    /// runner's built-in Administrator account (RID 500) came back as `LA`,
    /// not `S-1-5-21-...-500` — while `ConvertSidToStringSidW` always emits
    /// the raw form. Pushing the EXPECTED string through the same converter
    /// makes both sides speak the converter's dialect, whatever account CI
    /// happens to run as.
    #[cfg(windows)]
    fn canonical_sddl(sddl: &str) -> String {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW,
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};

        let wide: Vec<u16> = std::ffi::OsStr::new(sddl)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            assert_ne!(
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide.as_ptr(),
                    SDDL_REVISION_1,
                    &mut psd,
                    std::ptr::null_mut(),
                ),
                0,
                "string->SD failed for {sddl}"
            );
            let mut out_ptr: *mut u16 = std::ptr::null_mut();
            let mut out_len: u32 = 0;
            let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                psd,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut out_ptr,
                &mut out_len,
            );
            assert_ne!(ok, 0, "SD->string failed for {sddl}");
            let len = (0..).take_while(|&i| *out_ptr.add(i) != 0).count();
            let out = String::from_utf16_lossy(std::slice::from_raw_parts(out_ptr, len));
            LocalFree(out_ptr as _);
            LocalFree(psd as _);
            out
        }
    }

    /// ADR 0041 DACL requirement, points 1 and 3 together: the published
    /// root (this IS the post-rename state — bootstrap exposes no way to
    /// inspect `.creating` before the rename, so this is simultaneously the
    /// proof that the rename preserved the descriptor) carries a DACL that
    /// is PRESENT and PROTECTED, with exactly one ACE granting the LIVE
    /// token-user SID full access, marked object+container inheritable.
    /// Fails against today's main: an un-DACL'd `create_dir_all` produces
    /// an unprotected, inherited-from-parent descriptor with no such ACE.
    #[test]
    #[cfg(windows)]
    fn bootstrap_voyage_root_gets_protected_dacl_for_token_user() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voy5");
        VoyageStore::bootstrap(&root, "voy5", RetentionClass::Discard).unwrap();

        // FULL equality against the canonicalized expected form — presence,
        // protected bit, exactly one ACE, flags, access, and trustee in a
        // single assertion, robust to the converter's well-known-SID
        // aliasing (see `canonical_sddl`).
        let sid = current_user_sid_string();
        let expected = canonical_sddl(&format!("D:P(A;OICI;FA;;;{sid})"));
        assert_eq!(security_descriptor_sddl(&root), expected);
    }

    /// ADR 0041 DACL requirement, point 2: `seg/` — created inside the
    /// staging root by `bootstrap`'s plain `create_dir_all`, with NO
    /// security attributes of its own — carries an INHERITED ACE (`ID` =
    /// INHERITED_ACE) for the same trustee, proving the tree propagates the
    /// protection without any per-file work. A DIRECTORY child specifically
    /// (rather than a leaf file the segment writer creates): Windows clears
    /// the OI/CI propagation flags when materializing an inherited ACE onto
    /// a FILE (they would have no meaning for something that can't have
    /// children of its own), but a CONTAINER child keeps them — asserting
    /// the exact `OICIID` flag combination is only reliable against another
    /// container, so this checks `seg/` rather than the `.open` file inside
    /// it. Fails against today's main for the same reason as the bootstrap
    /// test above: there is no protected, inheritable ACE anywhere in the
    /// tree to inherit FROM.
    #[test]
    #[cfg(windows)]
    fn seg_dir_inherits_the_protected_dacl() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voy6");
        VoyageStore::bootstrap(&root, "voy6", RetentionClass::Discard).unwrap();

        let sddl = security_descriptor_sddl(&root.join("seg"));
        // The expected inherited ACE is the ROOT's canonical ACE with the
        // INHERITED_ACE flag added: take the converter-canonical trustee
        // spelling (alias or raw, whatever this account canonicalizes to)
        // and splice ID into the flags we set — the flags are ours to know,
        // the trustee spelling is the converter's.
        let sid = current_user_sid_string();
        let canonical_root = canonical_sddl(&format!("D:P(A;OICI;FA;;;{sid})"));
        let ace = canonical_root
            .trim_start_matches("D:P")
            .replace("(A;OICI;", "(A;OICIID;");
        assert!(
            sddl.contains(&ace),
            "expected the inherited form {ace} of the root's ACE, got: {sddl}"
        );
    }

    // Unix is functionally unchanged by `create_dir_protected` — a plain
    // `create_dir`, strict about AlreadyExists, which the attempt-owned
    // random staging name makes unreachable in practice —
    // `bootstrap_open_write_reopen` above is the proof that the whole
    // bootstrap/write/reopen/verify path still holds on every unix/linux
    // CI run.
}
