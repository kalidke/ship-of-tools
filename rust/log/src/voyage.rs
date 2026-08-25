//! Voyage store: bootstrap, writer lock + epoch allocation, rotation, blob
//! CAS publication (ADR 0039). Kernel-semantics parts have Linux and
//! Windows arms (ADR 0041 §store port); the codec itself is portable.

use crate::envelope::Digest;
use crate::fsutil::{self, WriterLock};
use crate::recovery::{self, Reconciled};
use crate::segment::{
    HeaderBody, RetentionClass, SegmentIdentity, SegmentReader, SegmentState, SegmentWriter,
};
use crate::{Error, Result};
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub struct VoyageStore {
    root: PathBuf,
    voyage_id: String,
    _lock: WriterLock,
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
}

/// Max take_epoch across a segment's committed `take_state` frames.
fn max_take_epoch(reader: &SegmentReader) -> u64 {
    reader
        .frames
        .iter()
        .filter_map(|f| {
            let p = f.payload.as_ref()?;
            (p.get("kind")?.as_str()? == "take_state")
                .then(|| p.get("take")?.get("take_epoch")?.as_u64())
                .flatten()
        })
        .max()
        .unwrap_or(0)
}

impl VoyageStore {
    /// Bootstrap a new voyage: build under `<root>.creating/`, fsync
    /// bottom-up, publish by no-clobber rename (ADR 0039 §lifecycle 1).
    pub fn bootstrap(root: &Path, voyage_id: &str, retention: RetentionClass) -> Result<()> {
        // Absolutize first: a relative root would (a) make the parent of a
        // bare name the empty path and (b) leave every later operation
        // raceable against set_current_dir elsewhere in the process.
        let root = std::path::absolute(root)?;
        let root = root.as_path();
        let parent = root
            .parent()
            .ok_or_else(|| Error::State("voyage root needs a parent dir".into()))?;
        // The container must PREEXIST: bootstrap will not create ancestor
        // levels, because it cannot durably anchor them (their entries in
        // THEIR parents are never flushed here — a "successful" bootstrap
        // into an implicitly created chain could vanish on power loss).
        // The container's durability is its creator's responsibility.
        if !parent.is_dir() {
            return Err(Error::State(format!(
                "voyage container {parent:?} does not exist (bootstrap will not create it)"
            )));
        }
        // Volume preflight BEFORE any `.creating` mutation (ADR 0041).
        fsutil::preflight_volume(parent)?;
        let staging = parent.join(format!(
            "{}.creating",
            root.file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| Error::State("bad voyage root name".into()))?
        ));
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
        fsutil::rename_noreplace(&staging, root)?;
        fsutil::fsync_dir(parent)?;
        let _ = (voyage_id, retention); // identity/retention live in the genesis header
        Ok(())
    }

    /// Open for writing: take the kernel lock, reconcile every segment
    /// identity found, allocate this writer's epoch (max durable + 1), and
    /// compute the chain tip.
    pub fn open_for_writing(root: &Path, voyage_id: &str) -> Result<Self> {
        // Absolutize for the same reasons as bootstrap: the stored root must
        // not be re-resolvable against a moved CWD while the lock is held.
        // (Ancestor-junction retargeting by another PRINCIPAL is out of the
        // fence's threat model: the voyage container's ancestors are
        // owner-controlled, and the ADR's DACL step protects the subtree.)
        let root = std::path::absolute(root)?;
        let root = root.as_path();
        // Re-run the volume preflight on the resolved voyage dir (ADR 0041):
        // a store bootstrapped elsewhere and moved to an unsuitable volume
        // must refuse before the fence is even touched.
        fsutil::preflight_volume(root)?;
        // Restate the root's anchoring: bootstrap's publish rename may have
        // become visible while its container flush was lost to a crash — the
        // callers' bootstrap-if-absent check would then skip bootstrap
        // forever, leaving a store that acknowledges records from a root the
        // next power loss can remove. Idempotent, so every open re-anchors.
        fsutil::fsync_dir(root)?;
        if let Some(parent) = root.parent() {
            fsutil::fsync_dir(parent)?;
        }
        let lock = fsutil::lock_writer(&root.join("writer.lock"))?;
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
                    last_take = last_take.max(max_take_epoch(&r));
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
                    last_take = last_take.max(max_take_epoch(&r));
                    prev = r.seal.as_ref().map(|s| s.digest.clone());
                    next_index = idx + 1;
                }
            }
        }

        Ok(Self {
            root: root.to_path_buf(),
            voyage_id: voyage_id.to_string(),
            _lock: lock,
            epoch: my_epoch,
            prev_seal_digest: prev,
            next_segment_index: next_index,
            retention_class: retention.unwrap_or(RetentionClass::Archive),
            last_take_epoch: last_take,
            survivor_open: survivor,
        })
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
        fsutil::rename_noreplace(&open_path, &id.path(&seg_dir, SegmentState::Recovering))?;
        fsutil::fsync_dir(&seg_dir)?;
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
            fsutil::fsync_dir(&shard)?;
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
        match fsutil::rename_noreplace(&tmp, &dest) {
            Ok(()) => {}
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Raced an identical publish: verify + clean the temp.
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
        fsutil::fsync_dir(&shard)?;
        Ok(digest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::Class;
    use crate::segment::{tests::test_env, Commit};

    /// A conforming standalone frame (lifecycle needs no attached_to).
    fn lc(epoch: u64, n: u64) -> crate::envelope::Envelope {
        let mut e = test_env(epoch, n);
        e.class = Class::Lifecycle;
        e.payload = Some(serde_json::json!({"kind": "producer_ready"}));
        e
    }

    #[test]
    fn bootstrap_open_write_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voy1");
        VoyageStore::bootstrap(&root, "voy1", RetentionClass::Discard).unwrap();
        assert!(root.join("seg").is_dir());
        assert!(root.join("blobs").join(".tmp").is_dir());
        assert!(!dir.path().join("voy1.creating").exists());

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
}
