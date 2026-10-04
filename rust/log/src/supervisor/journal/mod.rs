//! ADR 0041 step 6 U2: the supervisor's own durable operation journal,
//! under `<state_dir>/supervisor-journal/` — Lifecycle's "`operation_id`
//! is durable for MUTATING ops only" and "Recovery is part of the
//! transaction, and it runs FIRST".
//!
//! One immutable file PER STATE, per operation id, never rewritten:
//! `<key>.active` (published BEFORE the first irreversible act of an
//! `end_run`, `reset`, or `stop` — the ADR's own phrasing) and
//! `<key>.terminal` (published once the operation reaches the ONE
//! terminal state it will ever reach). `Accepted` is never a stored
//! value — it is the CALLER's own computation of "an `.active` record
//! exists with no matching `.terminal` one yet", so there is exactly one
//! place a state can be wrong: whichever of these two files is actually
//! on disk.
//!
//! `ActiveOp` is a TAGGED enum (Codex review round 1, simplicity audit):
//! an earlier version carried three independently-optional fields
//! (`intended_new_voyage`, `old_voyage`, `end_run_epoch`), which admitted
//! impossible combinations and let an `end_run` recorded with no known
//! epoch be silently misrecovered as a bare `stop` (recovery discriminated
//! on "which optional field is populated", and an absent `end_run_epoch`
//! looked identical to a `stop`'s own all-`None` shape). One tag, one
//! shape per op, no combination to get wrong.
//!
//! # `<key>` is `sha256_hex(operation_id)`, not the id itself (Codex
//! review round 2, finding M7)
//!
//! An earlier version filed records under the WIRE-VALIDATED operation
//! id directly (charset-restricted, ≤64 bytes, `.`/`..` rejected at
//! decode). That is not enough for an INJECTIVE Windows filename: case
//! folding aliases `op-X` and `op-x`, and a reserved device basename
//! (`CON`, `NUL`, `COM1`, ...) fails as a device path regardless of
//! suffix. Keying by the id's own SHA-256 hex digest sidesteps both —
//! the key is always exactly 64 lowercase hex characters, has no
//! platform-reserved shape, and two DIFFERENT ids collide only with
//! SHA-256-collision probability. [`ActiveRecord::operation_id`] stores
//! the real id so [`active_operations`] (the only function that must
//! recover an id from a bare filename it did not already know) can
//! report it; [`active_operations`] itself re-derives the key from that
//! stored id and refuses a mismatch as corruption, never a silent skip.
//! Wire-level validation is unchanged and unrelated — it protects the
//! PROTOCOL's own id shape, not this module's file-naming scheme.
//!
//! # No schema migration (Codex review round 2, finding M6)
//!
//! This journal format has never shipped before this branch merged — no
//! `state_dir` created by an earlier build can contain one of these
//! files at all, so there is nothing to migrate FROM. [`SCHEMA_VERSION`]
//! is nonetheless stamped on every record now, as the seam a REAL future
//! migration would need; a version mismatch is a loud, unrecoverable
//! `Err` (this module has no fallback interpretation for a version it
//! does not recognize), never a silent best-effort parse.
//!
//! Portable (no OS-specific code): reuses [`crate::fsutil::publish_noreplace`],
//! which already has both platform arms, like `pointer.rs`/`rollout.rs`.
//!
//! Single-writer by construction (Codex-anticipated simplification, named
//! so a reviewer does not go looking for arbitration logic that would
//! otherwise seem missing): every write here happens only while the
//! caller holds `supervisor.lock` — ADR 0041's "ONE AUTHORITY" — so two
//! processes never race a write to this journal. Durability against a
//! CRASH mid-write is the property this module provides; there is no
//! concurrent-writer race to arbitrate.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub mod fence;
pub mod pointer;
pub(super) mod end_run;
pub(super) mod recover;
pub(super) mod reset;

const JOURNAL_DIR_NAME: &str = "supervisor-journal";

/// The journal's own schema version — see the module doc's "No schema
/// migration" section. Bump this, and give `read_json` a real migration
/// path, the day a second shape is ever actually introduced.
pub const SCHEMA_VERSION: u32 = 1;

/// A journal record file is a small, fixed-shape JSON document — never
/// legitimately large. Bounds the read BEFORE parsing (Codex review
/// round 2, finding M5), so a corrupted or maliciously large file fails
/// fast on size alone rather than being loaded in full first.
const MAX_JOURNAL_RECORD_BYTES: u64 = 16 * 1024;

/// `sha256_hex(operation_id)` — see the module doc's own section on why
/// this, not the id itself, is the file key.
fn record_key(operation_id: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(operation_id.as_bytes());
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn journal_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(JOURNAL_DIR_NAME)
}

fn active_path(state_dir: &Path, operation_id: &str) -> PathBuf {
    journal_dir(state_dir).join(format!("{}.active", record_key(operation_id)))
}

fn terminal_path(state_dir: &Path, operation_id: &str) -> PathBuf {
    journal_dir(state_dir).join(format!("{}.terminal", record_key(operation_id)))
}

fn closed_path(state_dir: &Path, operation_id: &str) -> PathBuf {
    journal_dir(state_dir).join(format!("{}.closed", record_key(operation_id)))
}

/// What an `.active` record commits to (ADR 0041: "the id, a canonical
/// digest of the command, its state, and for `reset` the new voyage
/// identity it intends to publish"). `operation_id` is the real,
/// caller-chosen id (the file itself is keyed by its hash — see the
/// module doc); `digest` is the caller's own stable hex digest of the
/// WIRE command this id names — an id resubmitted with a DIFFERENT
/// digest is `refused {id_conflict}`, which the caller (not this module)
/// decides by comparing against [`read_active`]'s answer. `sot_log::wire`
/// owns the canonical BYTE encoding ([`wire::canonical_supervisor_op_bytes`]);
/// `supervisor.rs` SHA-256s those bytes into the hex string this module
/// only stores and compares (Codex review round 1, finding 6 — an
/// earlier version hashed `format!("{op:?}")`, Rust's `Debug` output,
/// which carries no stability guarantee across compiler or dependency
/// versions).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveRecord {
    pub operation_id: String,
    pub digest: String,
    pub op: ActiveOp,
}

impl ActiveRecord {
    /// Semantic validation beyond "parses as JSON" (Codex review round
    /// 2, finding M5): `digest` must be the exact shape a real SHA-256
    /// hex digest has; every voyage id must be the canonical
    /// lowercase-hyphenated UUID text [`crate::pointer::canonical_voyage_id`]
    /// requires everywhere else in this crate; `reset`'s `aside` must be
    /// a BARE basename — no path separator, no drive-letter colon, never
    /// `.`/`..`/absolute — so a corrupted record can never redirect
    /// recovery's own rename outside the state directory; `old_voyage`
    /// and `aside` must be present or absent TOGETHER (one names what
    /// the other renames aside; either alone is an impossible shape).
    fn validate(&self) -> Result<()> {
        if !is_sha256_hex(&self.digest) {
            return Err(Error::Schema(format!("invalid digest shape: {:?}", self.digest)));
        }
        match &self.op {
            ActiveOp::EndRun { voyage, .. } => {
                if crate::pointer::canonical_voyage_id(voyage).is_none() {
                    return Err(Error::Schema(format!("invalid voyage id shape: {voyage:?}")));
                }
            }
            ActiveOp::Reset { old_voyage, new_voyage, aside } => {
                if crate::pointer::canonical_voyage_id(new_voyage).is_none() {
                    return Err(Error::Schema(format!("invalid voyage id shape: {new_voyage:?}")));
                }
                if let Some(v) = old_voyage {
                    if crate::pointer::canonical_voyage_id(v).is_none() {
                        return Err(Error::Schema(format!("invalid voyage id shape: {v:?}")));
                    }
                }
                if let Some(a) = aside {
                    if !is_bare_basename(a) {
                        return Err(Error::Schema(format!("aside is not a bare basename: {a:?}")));
                    }
                }
                if old_voyage.is_none() != aside.is_none() {
                    return Err(Error::Schema(
                        "old_voyage and aside must be present or absent together".into(),
                    ));
                }
            }
            ActiveOp::Stop => {}
        }
        Ok(())
    }
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `true` iff `s` is a single path COMPONENT: nonempty, not `.`/`..`, and
/// containing none of `/`, `\`, or `:` — checked as plain bytes rather
/// than through `std::path::Path` (whose separator/prefix parsing is
/// PLATFORM-DEPENDENT and this validation must reject the exact same
/// shapes everywhere it runs, not whatever the host OS happens to parse).
fn is_bare_basename(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\', ':'])
}

/// One tagged shape per mutating op — every field an operation actually
/// needs, and no field it doesn't, so there is no impossible combination
/// for a reader to guess a discriminator from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ActiveOp {
    EndRun {
        /// The voyage this `end_run` targeted (voyage-fenced at
        /// admission).
        voyage: String,
        /// The leg epoch this operation targeted, if known at admission
        /// time — recovery's own "reads the DURABLE MARKER ... a marker
        /// in the leg's own epoch means ACCEPTED" needs this to know
        /// WHICH epoch to ask [`crate::verify::leg_carries_run_end_marker`]
        /// about (a marker "governs only its OWN epoch"). `None` is a
        /// real, recoverable case (recovery falls back to the CURRENT
        /// voyage's latest leg) — never confused with "this was actually
        /// a `stop`", which the tagged shape makes structurally
        /// impossible.
        #[serde(skip_serializing_if = "Option::is_none")]
        epoch: Option<u64>,
    },
    Reset {
        /// The voyage the caller OBSERVED (the pointer's value at
        /// admission) — recovery's "pointer still names the OLD voyage"
        /// case needs this to tell "the rename never took, safe to
        /// resume from the beginning" apart from "pointer names
        /// SOMETHING ELSE," a loud stop. `None` only when no pointer
        /// existed at admission (nothing to fence against).
        #[serde(skip_serializing_if = "Option::is_none")]
        old_voyage: Option<String>,
        /// The new voyage identity this operation intends to publish,
        /// recorded BEFORE the rename — recovery's "pointer names the
        /// INTENDED NEW voyage" case reconstructs `reset_done` from this
        /// without minting a second identity.
        new_voyage: String,
        /// The exact `drawer.voyage.reset-<nonce>` filename this
        /// operation will rename the OLD pointer to, chosen and recorded
        /// at ADMISSION time — recovery's "pointer ABSENT" row must
        /// VERIFY this evidence file actually exists before treating
        /// absence as "the rename already happened" rather than
        /// something worse (an interrupted move, a permission failure).
        /// A BARE BASENAME only ([`ActiveRecord::validate`]) — never a
        /// path a corrupted record could use to point outside the state
        /// directory. `None` only when `old_voyage` is `None` (nothing
        /// to rename aside).
        #[serde(skip_serializing_if = "Option::is_none")]
        aside: Option<String>,
    },
    Stop,
}

/// The terminal states `query` may report for a mutating operation (ADR
/// 0041 "one command family, one query family"): `record_verified`
/// (`end_run`), `reset_done` (`reset`), `stopping` (`stop`),
/// `failed {detail}`. `unknown_operation` is not a member here — it is
/// the absence of any `.active` record at all (see [`read_active`]).
/// `record_closed` is likewise not a member — it is the SEPARATE,
/// INTERMEDIATE [`mark_closed`]/[`is_closed`] milestone `end_run` alone
/// passes through on its way to `record_verified` or `failed`, never
/// itself an operation's LAST word (Codex review round 1, simplicity
/// audit: an earlier version's `RecordClosed` terminal variant duplicated
/// that milestone and was reachable only through an invalid
/// "pipe-absent, fabricate success" shortcut this crate no longer takes —
/// see `supervisor.rs`'s own EndRun reconciliation). `refused` is
/// likewise deleted: every wire-level refusal (`stale_voyage`,
/// `id_conflict`) is minted BEFORE the journal is ever touched (ADR:
/// "with NO MUTATION"), so no durable record has ever needed this shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TerminalRecord {
    RecordVerified,
    ResetDone { new_voyage: String },
    Stopping,
    Failed { detail: String },
}

/// Ensure the journal directory exists AND is durably anchored. Always
/// fsyncs the directory itself and its PARENT (`state_dir`) — restating
/// an already-durable anchor costs a little I/O and is always correct
/// (the same "restating is what the barrier is for" philosophy
/// `fsutil::finish_publication` already uses), where checking "did this
/// call create it for the first time" and fsyncing only then would leave
/// a residue directory that was never durably anchored by an EARLIER,
/// crashed first call. Without this, the very first `begin` under a
/// fresh state-dir can lose the entire journal directory across a crash
/// (Codex review finding 5) — undoing "`operation_id` is durable for
/// MUTATING ops only" at its own root.
pub fn ensure_dir(state_dir: &Path) -> Result<()> {
    let dir = journal_dir(state_dir);
    std::fs::create_dir_all(&dir)?;
    crate::fsutil::fsync_dir(&dir)?;
    crate::fsutil::fsync_dir(state_dir)?;
    Ok(())
}

/// Durably publish `operation_id`'s `.active` record BEFORE the first
/// irreversible act (ADR 0041: "publishes a journal record ... before
/// the first irreversible act"). Crash-durable: temp file, write, fsync,
/// no-clobber rename, directory fsync — [`crate::fsutil::publish_noreplace`]'s
/// own pinned order, not a second implementation of it.
///
/// `Err` wrapping [`std::io::ErrorKind::AlreadyExists`] means this id
/// already has an `.active` record (this call raced its own retry, or a
/// caller reused an id) — the caller reads it back via [`read_active`] to
/// decide `id_conflict` (different digest) vs "already active, return its
/// current state" (same digest). Panics (a caller bug, not a runtime
/// condition) if `record.operation_id != operation_id`.
pub fn begin(state_dir: &Path, operation_id: &str, record: &ActiveRecord) -> Result<()> {
    assert_eq!(
        record.operation_id, operation_id,
        "journal::begin: record.operation_id must match the operation_id parameter"
    );
    record
        .validate()
        .map_err(|e| Error::State(format!("journal::begin: refusing to write an invalid record: {e}")))?;
    ensure_dir(state_dir)?;
    publish_json(&journal_dir(state_dir), &active_path(state_dir, operation_id), record)
}

/// Durably publish `operation_id`'s `.terminal` record. A journal entry's
/// terminal fact is written EXACTLY ONCE and never rewritten — a second
/// `finish` call for the same id is tolerated only when it carries the
/// IDENTICAL record (a retried caller re-deriving the same terminal
/// state after its own crash); a different record is a caller bug and
/// errs loudly rather than silently overwriting the durable fact.
///
/// Calls [`ensure_dir`] first, exactly like [`begin`] and [`mark_closed`]
/// — every caller of `finish` in this crate happens to run after a prior
/// `begin` already created the journal directory, but `finish` had no
/// business trusting that: it is a public, independently callable
/// function, and a bare `std::fs::File::create` against a temp name under
/// a directory that does not yet exist fails PATH-not-FOUND on Windows
/// (the real cause of a CI failure once diagnosed as AV-transient — Codex
/// review round 1, CI finding (a) — not a retry-worthy timing window at
/// all, but a genuinely missing directory).
pub fn finish(state_dir: &Path, operation_id: &str, record: &TerminalRecord) -> Result<()> {
    ensure_dir(state_dir)?;
    let target = terminal_path(state_dir, operation_id);
    match publish_json(&journal_dir(state_dir), &target, record) {
        Ok(()) => Ok(()),
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = read_terminal(state_dir, operation_id)?.ok_or_else(|| {
                Error::State(format!(
                    "operation {operation_id}: terminal publish raced but no terminal record is readable"
                ))
            })?;
            if existing == *record {
                Ok(())
            } else {
                Err(Error::State(format!(
                    "operation {operation_id}: a DIFFERENT terminal record already exists \
                     ({existing:?} vs {record:?}) — a journal entry's terminal fact is never rewritten"
                )))
            }
        }
        Err(e) => Err(e),
    }
}

/// Durably mark `operation_id` RECORD CLOSED — the ADR 0041 INTERMEDIATE
/// state `end_run` alone passes through, between `accepted` and the
/// operation's true terminal fact (`record_verified` or `failed`): "the
/// COMMAND reply arrives at `record_closed`, and `record_verified`
/// follows through `query`." Unlike [`finish`], this is NOT the
/// operation's last word — a LATER [`finish`] call for the SAME id still
/// applies once verification (or its failure) actually concludes.
/// Idempotent: marking an already-closed id again is a no-op, not an
/// error (a retried caller reporting the same milestone twice).
pub fn mark_closed(state_dir: &Path, operation_id: &str) -> Result<()> {
    ensure_dir(state_dir)?;
    match publish_json(&journal_dir(state_dir), &closed_path(state_dir, operation_id), &ClosedMarker {}) {
        Ok(()) => Ok(()),
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Codex review round 3, N11/M5: `AlreadyExists` alone does
            // NOT prove a valid prior close -- a directory or corrupt
            // file at this path races `publish_noreplace` into this
            // SAME branch, and until now that made `mark_closed` treat
            // it as a successful idempotent no-op regardless. Validate
            // the pre-existing target exactly as `is_closed` does
            // (`read_json`'s own regular-file + size-cap + schema +
            // parse checks) before trusting this collision; a genuinely
            // corrupt target is loud, never silently accepted as
            // "already closed".
            if is_closed(state_dir, operation_id)? {
                Ok(())
            } else {
                Err(Error::Schema(format!(
                    "{operation_id}: a .closed marker publish collided with an existing target \
                     that is not itself a valid .closed marker"
                )))
            }
        }
        Err(e) => Err(e),
    }
}

/// The `.closed` file's own content — an empty, versioned record like
/// every other journal file, not a bare "file exists" convention (Codex
/// review round 2, finding M5: a directory, or any other filesystem
/// entry, used to count as closed because [`is_closed`] only ever
/// checked `metadata` — a regular, PARSEABLE file is now required).
#[derive(Serialize, Deserialize)]
struct ClosedMarker {}

/// `true` iff [`mark_closed`] has been called for `operation_id`. A
/// present-but-unparseable or non-file `.closed` entry is a loud `Err`,
/// never silently treated as "not yet closed" (Codex review round 2,
/// finding M5).
pub fn is_closed(state_dir: &Path, operation_id: &str) -> Result<bool> {
    Ok(read_json::<ClosedMarker>(&closed_path(state_dir, operation_id))?.is_some())
}

fn publish_json<T: Serialize>(dir: &Path, target: &Path, value: &T) -> Result<()> {
    let envelope = EnvelopeRef { schema_version: SCHEMA_VERSION, record: value };
    let bytes = serde_json::to_vec(&envelope)?;
    let mut nonce_bytes = [0u8; 8];
    getrandom::fill(&mut nonce_bytes).map_err(std::io::Error::from)?;
    let nonce = u64::from_le_bytes(nonce_bytes);
    let tmp = dir.join(format!(".tmp-{nonce:016x}"));
    {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    let result = crate::fsutil::publish_noreplace(&tmp, target);
    if result.is_err() {
        // A lost race (AlreadyExists) or any other publish failure: don't
        // leave this attempt's temp file behind as residue.
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Every journal file is wrapped in this envelope — see the module doc's
/// "No schema migration" section. `#[serde(flatten)]` merges the
/// record's own top-level fields alongside `schema_version` in the same
/// JSON object, so a human reading a journal file sees one flat
/// document, not a nested one.
#[derive(Serialize)]
struct EnvelopeRef<'a, T> {
    schema_version: u32,
    #[serde(flatten)]
    record: &'a T,
}

#[derive(Deserialize)]
struct EnvelopeOwned<T> {
    schema_version: u32,
    #[serde(flatten)]
    record: T,
}

/// Read `operation_id`'s `.active` record, if any, semantically
/// validated (see [`ActiveRecord::validate`]). `None` is
/// [`crate::verify`]-style ADR 0041 `unknown_operation`: "returned for a
/// MISSING journal entry and ONLY that; it is the one state meaning SAFE
/// TO RESUBMIT."
pub fn read_active(state_dir: &Path, operation_id: &str) -> Result<Option<ActiveRecord>> {
    let Some(record) = read_json::<ActiveRecord>(&active_path(state_dir, operation_id))? else {
        return Ok(None);
    };
    record.validate()?;
    Ok(Some(record))
}

/// Read `operation_id`'s `.terminal` record, if any. An `.active` record
/// with no `.terminal` one is `accepted` — the caller's own judgment
/// (this module stores neither), per the ADR: "the COMMAND reply arrives
/// at `record_closed`, and `record_verified` follows through `query`".
/// A PRESENT but unparseable terminal file is a loud `Err`, never
/// silently treated as absent (Codex review finding 5) — the same
/// "malformed journal → loud stop" rule [`active_operations`] itself
/// enforces.
pub fn read_terminal(state_dir: &Path, operation_id: &str) -> Result<Option<TerminalRecord>> {
    read_json(&terminal_path(state_dir, operation_id))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    // A non-regular-file entry (a directory, in practice) at a journal
    // path is corruption, not "absent" (Codex review round 2, finding
    // M5) -- `std::fs::read` would itself error opening a directory on
    // most platforms, but naming the actual condition is clearer than
    // whatever generic I/O message that produces.
    if !meta.is_file() {
        return Err(Error::Schema(format!("{}: not a regular file", path.display())));
    }
    if meta.len() > MAX_JOURNAL_RECORD_BYTES {
        return Err(Error::Schema(format!(
            "{}: {} bytes exceeds the {MAX_JOURNAL_RECORD_BYTES}-byte journal record cap",
            path.display(),
            meta.len()
        )));
    }
    let bytes = std::fs::read(path)?;
    let envelope: EnvelopeOwned<T> = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Schema(format!("{}: does not parse as a journal record: {e}", path.display())))?;
    if envelope.schema_version != SCHEMA_VERSION {
        return Err(Error::Schema(format!(
            "{}: journal schema version {} is not the {SCHEMA_VERSION} this build understands \
             (no migration exists — see journal.rs's own doc)",
            path.display(),
            envelope.schema_version
        )));
    }
    Ok(Some(envelope.record))
}

/// Every operation id with an `.active` record and NO VALID `.terminal`
/// one — ADR 0041's "reconciles every ACTIVE journal entry against the
/// world", which the authority runs FIRST: under `supervisor.lock`,
/// before pointer discovery, before start-mode authorization, and before
/// admitting any new command. "No valid terminal one" is deliberate
/// (Codex review finding 5): this reads and PARSES the terminal file via
/// [`read_terminal`] rather than a bare existence check, so a malformed
/// terminal file is this function's own loud `Err` — never silently
/// treated as either "terminal, skip it" or "no terminal yet, still
/// active", both of which would let a corrupted file quietly disable
/// recovery.
///
/// Recovers each id from the `.active` record's OWN `operation_id` field
/// (the filename is a hash — see the module doc), and refuses a filename
/// whose key does not match that field's own hash as corruption (Codex
/// review round 2, finding M7) — never silently trusted or silently
/// skipped.
pub fn active_operations(state_dir: &Path) -> Result<Vec<String>> {
    let dir = journal_dir(state_dir);
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(key) = name.strip_suffix(".active") else { continue };
        let Some(record) = read_json::<ActiveRecord>(&entry.path())? else { continue };
        record.validate()?;
        if record_key(&record.operation_id) != key {
            return Err(Error::Schema(format!(
                "{}: this file's own operation_id {:?} does not hash back to its filename — corrupt or tampered",
                entry.path().display(),
                record.operation_id
            )));
        }
        if read_terminal(state_dir, &record.operation_id)?.is_some() {
            continue;
        }
        out.push(record.operation_id);
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests;
