//! ADR 0041 step 6 U2: the supervisor's own durable operation journal,
//! under `<state_dir>/supervisor-journal/` — Lifecycle's "`operation_id`
//! is durable for MUTATING ops only" and "Recovery is part of the
//! transaction, and it runs FIRST".
//!
//! One immutable file PER STATE, per operation id, never rewritten:
//! `<id>.active` (published BEFORE the first irreversible act of an
//! `end_run`, `reset`, or `stop` — the ADR's own phrasing) and
//! `<id>.terminal` (published once the operation reaches the ONE
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

const JOURNAL_DIR_NAME: &str = "supervisor-journal";

fn journal_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(JOURNAL_DIR_NAME)
}

fn active_path(state_dir: &Path, operation_id: &str) -> PathBuf {
    journal_dir(state_dir).join(format!("{operation_id}.active"))
}

fn terminal_path(state_dir: &Path, operation_id: &str) -> PathBuf {
    journal_dir(state_dir).join(format!("{operation_id}.terminal"))
}

fn closed_path(state_dir: &Path, operation_id: &str) -> PathBuf {
    journal_dir(state_dir).join(format!("{operation_id}.closed"))
}

/// What an `.active` record commits to (ADR 0041: "the id, a canonical
/// digest of the command, its state, and for `reset` the new voyage
/// identity it intends to publish"). `digest` is the caller's own stable
/// hex digest of the WIRE command this id names — an id resubmitted with
/// a DIFFERENT digest is `refused {id_conflict}`, which the caller (not
/// this module) decides by comparing against [`read_active`]'s answer.
/// `sot_log::wire` owns the canonical BYTE encoding
/// ([`wire::canonical_supervisor_op_bytes`]); `supervisor.rs` SHA-256s
/// those bytes into the hex string this module only stores and compares
/// (Codex review round 1, finding 6 — an earlier version hashed
/// `format!("{op:?}")`, Rust's `Debug` output, which carries no stability
/// guarantee across compiler or dependency versions).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveRecord {
    pub digest: String,
    pub op: ActiveOp,
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
        /// `None` only when `old_voyage` is `None` (nothing to rename
        /// aside).
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
/// current state" (same digest).
pub fn begin(state_dir: &Path, operation_id: &str, record: &ActiveRecord) -> Result<()> {
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
    match publish_json(&journal_dir(state_dir), &closed_path(state_dir, operation_id), &serde_json::json!({})) {
        Ok(()) => Ok(()),
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// `true` iff [`mark_closed`] has been called for `operation_id`.
pub fn is_closed(state_dir: &Path, operation_id: &str) -> Result<bool> {
    match std::fs::metadata(closed_path(state_dir, operation_id)) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn publish_json<T: Serialize>(dir: &Path, target: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
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

/// Read `operation_id`'s `.active` record, if any. `None` is
/// [`crate::verify`]-style ADR 0041 `unknown_operation`: "returned for a
/// MISSING journal entry and ONLY that; it is the one state meaning SAFE
/// TO RESUBMIT."
pub fn read_active(state_dir: &Path, operation_id: &str) -> Result<Option<ActiveRecord>> {
    read_json(&active_path(state_dir, operation_id))
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
    match std::fs::read(path) {
        Ok(bytes) => {
            let value = serde_json::from_slice(&bytes).map_err(|e| {
                Error::Schema(format!("{}: does not parse as a journal record: {e}", path.display()))
            })?;
            Ok(Some(value))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
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
        let Some(id) = name.strip_suffix(".active") else { continue };
        if read_terminal(state_dir, id)?.is_some() {
            continue;
        }
        out.push(id.to_string());
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn end_run(voyage: &str, epoch: Option<u64>) -> ActiveRecord {
        ActiveRecord { digest: "d".into(), op: ActiveOp::EndRun { voyage: voyage.into(), epoch } }
    }

    fn stop() -> ActiveRecord {
        ActiveRecord { digest: "d".into(), op: ActiveOp::Stop }
    }

    // macOS's `fsutil::rename_noreplace_raw` fails closed (see
    // `pointer.rs`'s own tests for the same gate) — every test here that
    // exercises `begin`/`finish` (both routed through `publish_noreplace`)
    // is gated identically.
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn begin_then_read_active_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let record = end_run("voy-1", Some(3));
        begin(dir.path(), "op-1", &record).unwrap();
        assert_eq!(read_active(dir.path(), "op-1").unwrap(), Some(record));
        assert_eq!(read_terminal(dir.path(), "op-1").unwrap(), None);
    }

    #[test]
    fn unknown_operation_reads_as_none_for_both_files() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_active(dir.path(), "nope").unwrap(), None);
        assert_eq!(read_terminal(dir.path(), "nope").unwrap(), None);
    }

    /// An `end_run` recorded with NO known epoch is still, structurally,
    /// an `end_run` — never recoverable as a bare `stop` the way three
    /// independently-optional fields used to allow (Codex review
    /// simplicity audit).
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn an_epoch_less_end_run_is_never_shaped_like_a_stop() {
        let dir = tempfile::tempdir().unwrap();
        begin(dir.path(), "op-1", &end_run("voy-1", None)).unwrap();
        match read_active(dir.path(), "op-1").unwrap().unwrap().op {
            ActiveOp::EndRun { voyage, epoch } => {
                assert_eq!(voyage, "voy-1");
                assert_eq!(epoch, None);
            }
            other => panic!("expected EndRun, got {other:?}"),
        }
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn a_second_begin_for_the_same_id_fails_write_once() {
        let dir = tempfile::tempdir().unwrap();
        let first = stop();
        let second = end_run("voy-1", None);
        begin(dir.path(), "op-1", &first).unwrap();
        let err = begin(dir.path(), "op-1", &second).unwrap_err();
        assert!(matches!(&err, Error::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists), "{err}");
        // The FIRST record is still what's on disk — the caller reads it
        // back to distinguish id_conflict from an idempotent resubmit.
        assert_eq!(read_active(dir.path(), "op-1").unwrap(), Some(first));
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn finish_then_read_terminal_round_trips_and_leaves_the_id_out_of_active_operations() {
        let dir = tempfile::tempdir().unwrap();
        begin(dir.path(), "op-1", &stop()).unwrap();
        assert_eq!(active_operations(dir.path()).unwrap(), vec!["op-1".to_string()]);
        finish(dir.path(), "op-1", &TerminalRecord::Stopping).unwrap();
        assert_eq!(read_terminal(dir.path(), "op-1").unwrap(), Some(TerminalRecord::Stopping));
        assert!(active_operations(dir.path()).unwrap().is_empty());
    }

    /// `finish` must not depend on a prior `begin` having already created
    /// the journal directory: a caller can legitimately reconstruct a
    /// terminal fact without ever journaling an `.active` record for the
    /// SAME id first (`supervisor.rs`'s own `reconcile_reset`, resuming a
    /// reset purely from the pointer's own on-disk state). Regression for
    /// a real bug (Codex review round 1, CI failure (a)): `finish` used to
    /// skip `ensure_dir`, so this exact call failed `PATH_NOT_FOUND` on
    /// Windows — misdiagnosed as an AV-scan transient before the missing
    /// `create_dir_all` was found.
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn finish_creates_the_journal_directory_with_no_prior_begin() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!journal_dir(dir.path()).exists());
        finish(dir.path(), "op-never-begun", &TerminalRecord::Stopping).unwrap();
        assert_eq!(read_terminal(dir.path(), "op-never-begun").unwrap(), Some(TerminalRecord::Stopping));
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn a_repeated_finish_with_the_identical_record_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        begin(dir.path(), "op-1", &stop()).unwrap();
        finish(dir.path(), "op-1", &TerminalRecord::Stopping).unwrap();
        finish(dir.path(), "op-1", &TerminalRecord::Stopping).unwrap(); // no error
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn a_repeated_finish_with_a_different_record_is_refused_loudly() {
        let dir = tempfile::tempdir().unwrap();
        begin(dir.path(), "op-1", &stop()).unwrap();
        finish(dir.path(), "op-1", &TerminalRecord::Stopping).unwrap();
        let err = finish(dir.path(), "op-1", &TerminalRecord::Failed { detail: "x".into() }).unwrap_err();
        assert!(format!("{err}").contains("never rewritten"), "{err}");
        // The FIRST terminal fact must still be what's readable.
        assert_eq!(read_terminal(dir.path(), "op-1").unwrap(), Some(TerminalRecord::Stopping));
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn active_operations_lists_only_ids_missing_a_terminal_record() {
        let dir = tempfile::tempdir().unwrap();
        begin(dir.path(), "still-active", &stop()).unwrap();
        begin(dir.path(), "done", &stop()).unwrap();
        finish(dir.path(), "done", &TerminalRecord::Stopping).unwrap();
        assert_eq!(active_operations(dir.path()).unwrap(), vec!["still-active".to_string()]);
    }

    #[test]
    fn active_operations_on_a_never_touched_state_dir_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(active_operations(dir.path()).unwrap().is_empty());
    }

    /// Codex review finding 5: a malformed `.terminal` file must be a
    /// loud stop for BOTH `read_terminal` directly and `active_operations`'
    /// own scan — never silently treated as "no terminal, still active"
    /// or "terminal, skip it".
    #[test]
    fn a_malformed_terminal_file_is_a_loud_stop_not_a_silent_skip() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(JOURNAL_DIR_NAME)).unwrap();
        std::fs::write(terminal_path(dir.path(), "op-1"), b"not json").unwrap();
        std::fs::write(active_path(dir.path(), "op-1"), serde_json::to_vec(&stop()).unwrap()).unwrap();
        assert!(read_terminal(dir.path(), "op-1").is_err());
        assert!(active_operations(dir.path()).is_err());
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn reset_records_carry_the_new_voyage_and_the_aside_pathname() {
        let dir = tempfile::tempdir().unwrap();
        let record = ActiveRecord {
            digest: "reset-digest".into(),
            op: ActiveOp::Reset {
                old_voyage: Some("old-voyage-id".into()),
                new_voyage: "new-voyage-id".into(),
                aside: Some("drawer.voyage.reset-deadbeefcafef00d".into()),
            },
        };
        begin(dir.path(), "op-reset", &record).unwrap();
        assert_eq!(read_active(dir.path(), "op-reset").unwrap(), Some(record));
        finish(
            dir.path(),
            "op-reset",
            &TerminalRecord::ResetDone { new_voyage: "new-voyage-id".into() },
        )
        .unwrap();
        assert_eq!(
            read_terminal(dir.path(), "op-reset").unwrap(),
            Some(TerminalRecord::ResetDone { new_voyage: "new-voyage-id".into() })
        );
    }

    // macOS's `fsutil::rename_noreplace_raw` fails closed by design (see
    // the module doc, and `pointer.rs`'s own tests for the same gate) --
    // `mark_closed` is routed through `publish_json`/`publish_noreplace`
    // exactly like `begin`/`finish`, so it needs the identical gate.
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn is_closed_is_false_before_mark_closed_and_true_after() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_closed(dir.path(), "op-1").unwrap());
        mark_closed(dir.path(), "op-1").unwrap();
        assert!(is_closed(dir.path(), "op-1").unwrap());
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn mark_closed_twice_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        mark_closed(dir.path(), "op-1").unwrap();
        mark_closed(dir.path(), "op-1").unwrap(); // must not error
        assert!(is_closed(dir.path(), "op-1").unwrap());
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn closed_then_finished_is_the_end_run_two_phase_shape() {
        // ADR 0041: "the COMMAND reply arrives at record_closed, and
        // record_verified follows through query" -- record_closed is an
        // INTERMEDIATE milestone (mark_closed), not itself the terminal
        // fact finish() guards; a later finish() for the same id still
        // applies once verification concludes.
        let dir = tempfile::tempdir().unwrap();
        begin(dir.path(), "op-endrun", &end_run("voy-1", Some(3))).unwrap();
        mark_closed(dir.path(), "op-endrun").unwrap();
        assert!(is_closed(dir.path(), "op-endrun").unwrap());
        assert_eq!(read_terminal(dir.path(), "op-endrun").unwrap(), None);
        finish(dir.path(), "op-endrun", &TerminalRecord::RecordVerified).unwrap();
        assert_eq!(read_terminal(dir.path(), "op-endrun").unwrap(), Some(TerminalRecord::RecordVerified));
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn ensure_dir_is_idempotent_and_safe_to_call_repeatedly() {
        let dir = tempfile::tempdir().unwrap();
        ensure_dir(dir.path()).unwrap();
        ensure_dir(dir.path()).unwrap();
        assert!(journal_dir(dir.path()).is_dir());
    }
}
