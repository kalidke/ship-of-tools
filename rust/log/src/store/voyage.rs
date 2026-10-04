//! Voyage store: bootstrap, writer lock + epoch allocation, rotation, blob
//! CAS publication (ADR 0039). Kernel-semantics parts have Linux and
//! Windows arms (ADR 0041 §store port); the codec itself is portable.

use crate::store::envelope::{Digest, Seq};
use crate::host::{self, DirIdentity, PinnedDir, WriterLock};
use crate::store::recovery::{self, Reconciled};
use crate::store::segment::{
    HeaderBody, RetentionClass, SegmentIdentity, SegmentReader, SegmentState, SegmentWriter,
};
use crate::{Error, Result};
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use super::dedupe::{DedupeEntry, IdemKey};
use super::dedupe::walk_segment;

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
/// (`host::DirIdentity`) tells the two apart.
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
        host::preflight_volume(parent)?;
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
        host::create_dir_protected(&staging)?;
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
        host::fsync_dir(&staging.join("blobs").join(".tmp"))?;
        host::fsync_dir(&staging.join("blobs").join("sha256"))?;
        host::fsync_dir(&staging.join("blobs"))?;
        host::fsync_dir(&staging.join("seg"))?;
        host::fsync_dir(&staging)?;
        host::publish_noreplace(&staging, root)?;
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
        let identity = host::dir_identity(&path)?;
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
    /// for a swap to land in front of. See [`host::PinnedDir`]'s own doc
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
        let lock = host::lock_writer(&root.join("writer.lock"))?;

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
        host::preflight_volume(root)?;
        // Restate the root's anchoring: bootstrap's publish rename may have
        // become visible while its container flush was lost to a crash — the
        // callers' bootstrap-if-absent check would then skip bootstrap
        // forever, leaving a store that acknowledges records from a root the
        // next power loss can remove. Idempotent, so every open re-anchors.
        // (This is also the Part 3 restatement for bootstrap's own publish:
        // no separate call is needed there — every open already re-flushes
        // root plus its parent, unconditionally, before anything else.)
        host::fsync_dir(root)?;
        // The one exception that stays on the REAL path -- see this
        // method's own doc.
        if let Some(parent) = prepared.path.parent() {
            host::fsync_dir(parent)?;
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
        host::publish_noreplace(&open_path, &id.path(&seg_dir, SegmentState::Recovering))?;
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
        host::fsync_dir(&blobs.join("sha256"))?; // anchors the shard entry
        host::fsync_dir(&blobs)?; // anchors the sha256 entry
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
            host::finish_publication(&dest)?;
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
        match host::rename_noreplace_raw(&tmp, &dest) {
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
        host::finish_publication(&dest)?;
        Ok(digest)
    }
}

#[cfg(test)]
#[cfg(any(target_os = "linux", windows))]
#[path = "voyage_tests.rs"]
mod tests;
