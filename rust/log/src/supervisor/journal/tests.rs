use super::*;

fn digest_placeholder() -> String {
    "d".repeat(64)
}

fn end_run(id: &str, voyage: &str, epoch: Option<u64>) -> ActiveRecord {
    ActiveRecord {
        operation_id: id.into(),
        digest: digest_placeholder(),
        op: ActiveOp::EndRun { voyage: voyage.into(), epoch },
    }
}

fn stop(id: &str) -> ActiveRecord {
    ActiveRecord { operation_id: id.into(), digest: digest_placeholder(), op: ActiveOp::Stop }
}

fn a_voyage() -> String {
    uuid::Uuid::now_v7().to_string()
}

// macOS's `host::rename_noreplace_raw` fails closed (see
// `pointer.rs`'s own tests for the same gate) — every test here that
// exercises `begin`/`finish` (both routed through `publish_noreplace`)
// is gated identically.
#[cfg(any(target_os = "linux", windows))]
#[test]
fn begin_then_read_active_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let voyage = a_voyage();
    let record = end_run("op-1", &voyage, Some(3));
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
/// an `end_run` — never recoverable as a bare `stop`.
#[cfg(any(target_os = "linux", windows))]
#[test]
fn an_epoch_less_end_run_is_never_shaped_like_a_stop() {
    let dir = tempfile::tempdir().unwrap();
    let voyage = a_voyage();
    begin(dir.path(), "op-1", &end_run("op-1", &voyage, None)).unwrap();
    match read_active(dir.path(), "op-1").unwrap().unwrap().op {
        ActiveOp::EndRun { voyage: v, epoch } => {
            assert_eq!(v, voyage);
            assert_eq!(epoch, None);
        }
        other => panic!("expected EndRun, got {other:?}"),
    }
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn a_second_begin_for_the_same_id_fails_write_once() {
    let dir = tempfile::tempdir().unwrap();
    let first = stop("op-1");
    let second = end_run("op-1", &a_voyage(), None);
    begin(dir.path(), "op-1", &first).unwrap();
    let err = begin(dir.path(), "op-1", &second).unwrap_err();
    assert!(matches!(&err, Error::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists), "{err}");
    // The FIRST record is still what's on disk — the caller reads it
    // back to distinguish id_conflict from an idempotent resubmit.
    assert_eq!(read_active(dir.path(), "op-1").unwrap(), Some(first));
}

#[test]
#[should_panic(expected = "must match")]
fn begin_panics_if_the_record_names_a_different_operation_id() {
    let dir = tempfile::tempdir().unwrap();
    let _ = begin(dir.path(), "op-1", &stop("op-DIFFERENT"));
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn finish_then_read_terminal_round_trips_and_leaves_the_id_out_of_active_operations() {
    let dir = tempfile::tempdir().unwrap();
    begin(dir.path(), "op-1", &stop("op-1")).unwrap();
    assert_eq!(active_operations(dir.path()).unwrap(), vec!["op-1".to_string()]);
    finish(dir.path(), "op-1", &TerminalRecord::Stopping).unwrap();
    assert_eq!(read_terminal(dir.path(), "op-1").unwrap(), Some(TerminalRecord::Stopping));
    assert!(active_operations(dir.path()).unwrap().is_empty());
}

/// `finish` must not depend on a prior `begin` having already created
/// the journal directory: a caller can legitimately reconstruct a
/// terminal fact without ever journaling an `.active` record for the
/// SAME id first (`supervisor/journal/reset.rs`'s own `reconcile_reset`, resuming a
/// reset purely from the pointer's own on-disk state). Regression for
/// a real bug: `finish` used to skip `ensure_dir`, so this exact call
/// failed `PATH_NOT_FOUND` on Windows.
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
    begin(dir.path(), "op-1", &stop("op-1")).unwrap();
    finish(dir.path(), "op-1", &TerminalRecord::Stopping).unwrap();
    finish(dir.path(), "op-1", &TerminalRecord::Stopping).unwrap(); // no error
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn a_repeated_finish_with_a_different_record_is_refused_loudly() {
    let dir = tempfile::tempdir().unwrap();
    begin(dir.path(), "op-1", &stop("op-1")).unwrap();
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
    begin(dir.path(), "still-active", &stop("still-active")).unwrap();
    begin(dir.path(), "done", &stop("done")).unwrap();
    finish(dir.path(), "done", &TerminalRecord::Stopping).unwrap();
    assert_eq!(active_operations(dir.path()).unwrap(), vec!["still-active".to_string()]);
}

#[test]
fn active_operations_on_a_never_touched_state_dir_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(active_operations(dir.path()).unwrap().is_empty());
}

/// A malformed
/// `.terminal` file must be a loud stop for BOTH `read_terminal`
/// directly and `active_operations`' own scan — never silently
/// treated as "no terminal, still active" or "terminal, skip it".
#[test]
fn a_malformed_terminal_file_is_a_loud_stop_not_a_silent_skip() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(JOURNAL_DIR_NAME)).unwrap();
    let key = record_key("op-1");
    std::fs::write(dir.path().join(JOURNAL_DIR_NAME).join(format!("{key}.terminal")), b"not json").unwrap();
    std::fs::write(
        dir.path().join(JOURNAL_DIR_NAME).join(format!("{key}.active")),
        serde_json::to_vec(&EnvelopeRef { schema_version: SCHEMA_VERSION, record: &stop("op-1") }).unwrap(),
    )
    .unwrap();
    assert!(read_terminal(dir.path(), "op-1").is_err());
    assert!(active_operations(dir.path()).is_err());
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn reset_records_carry_the_new_voyage_and_the_aside_pathname() {
    let dir = tempfile::tempdir().unwrap();
    let new_voyage = a_voyage();
    let record = ActiveRecord {
        operation_id: "op-reset".into(),
        digest: digest_placeholder(),
        op: ActiveOp::Reset {
            old_voyage: Some(a_voyage()),
            new_voyage: new_voyage.clone(),
            aside: Some("drawer.voyage.reset-deadbeefcafef00d".into()),
        },
    };
    begin(dir.path(), "op-reset", &record).unwrap();
    assert_eq!(read_active(dir.path(), "op-reset").unwrap(), Some(record));
    finish(dir.path(), "op-reset", &TerminalRecord::ResetDone { new_voyage: new_voyage.clone() }).unwrap();
    assert_eq!(
        read_terminal(dir.path(), "op-reset").unwrap(),
        Some(TerminalRecord::ResetDone { new_voyage })
    );
}

// macOS's `host::rename_noreplace_raw` fails closed by design (see
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
    begin(dir.path(), "op-endrun", &end_run("op-endrun", &a_voyage(), Some(3))).unwrap();
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

// --- semantic validation ---

#[cfg(any(target_os = "linux", windows))]
#[test]
fn a_non_hex_digest_is_refused_at_begin_and_at_read() {
    let dir = tempfile::tempdir().unwrap();
    let bad = ActiveRecord { operation_id: "op-1".into(), digest: "not-hex".into(), op: ActiveOp::Stop };
    assert!(begin(dir.path(), "op-1", &bad).is_err());
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn a_non_canonical_voyage_id_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let bad = end_run("op-1", "not-a-uuid", None);
    assert!(begin(dir.path(), "op-1", &bad).is_err());
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn a_reset_aside_with_a_path_separator_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let bad = ActiveRecord {
        operation_id: "op-1".into(),
        digest: digest_placeholder(),
        op: ActiveOp::Reset {
            old_voyage: Some(a_voyage()),
            new_voyage: a_voyage(),
            aside: Some("../../etc/passwd".into()),
        },
    };
    assert!(begin(dir.path(), "op-1", &bad).is_err());
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn a_reset_with_old_voyage_but_no_aside_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let bad = ActiveRecord {
        operation_id: "op-1".into(),
        digest: digest_placeholder(),
        op: ActiveOp::Reset { old_voyage: Some(a_voyage()), new_voyage: a_voyage(), aside: None },
    };
    assert!(begin(dir.path(), "op-1", &bad).is_err());
}

/// A `.closed` marker must be a regular, parseable file — a
/// directory sitting at the same path (however that got there) must
/// never silently count as closed.
#[test]
fn a_directory_at_the_closed_path_is_a_loud_error_not_silently_closed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(closed_path(dir.path(), "op-1")).unwrap();
    assert!(is_closed(dir.path(), "op-1").is_err());
}

/// Injective, tamper-evident filenames: the file key is the operation id's own hash, not the id
/// itself, so Windows case-folding or reserved device names never
/// alias two distinct ids to the same journal object.
#[cfg(any(target_os = "linux", windows))]
#[test]
fn journal_file_keys_are_hashed_not_the_raw_operation_id() {
    let dir = tempfile::tempdir().unwrap();
    begin(dir.path(), "op-X", &stop("op-X")).unwrap();
    let key = record_key("op-X");
    assert!(dir.path().join(JOURNAL_DIR_NAME).join(format!("{key}.active")).exists());
    assert!(!dir.path().join(JOURNAL_DIR_NAME).join("op-X.active").exists());
    // A DIFFERENT id (differs only by case, which Windows folds) is
    // NOT the same file.
    assert_ne!(record_key("op-X"), record_key("op-x"));
}

#[test]
fn a_journal_file_with_an_unrecognized_schema_version_is_a_loud_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(JOURNAL_DIR_NAME)).unwrap();
    let key = record_key("op-1");
    let mut value = serde_json::to_value(EnvelopeRef { schema_version: SCHEMA_VERSION, record: &stop("op-1") }).unwrap();
    value["schema_version"] = serde_json::json!(SCHEMA_VERSION + 1);
    std::fs::write(
        dir.path().join(JOURNAL_DIR_NAME).join(format!("{key}.active")),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    assert!(read_active(dir.path(), "op-1").is_err());
}

/// `mark_closed`'s `AlreadyExists`
/// branch used to trust the collision blindly — a DIRECTORY at the
/// `.closed` path (which `publish_noreplace` also refuses with
/// `AlreadyExists`, same as a pre-existing file) would be silently
/// treated as "already closed, nothing to do" instead of the loud
/// corruption it actually is.
#[cfg(any(target_os = "linux", windows))]
#[test]
fn mark_closed_refuses_a_directory_masquerading_as_the_marker() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(JOURNAL_DIR_NAME)).unwrap();
    let key = record_key("op-1");
    std::fs::create_dir_all(dir.path().join(JOURNAL_DIR_NAME).join(format!("{key}.closed"))).unwrap();
    let err = mark_closed(dir.path(), "op-1").unwrap_err();
    // The AlreadyExists collision defers to `is_closed`'s own
    // validation, which itself already refuses a non-regular-file
    // target loudly (`read_json`'s own check) -- `mark_closed`'s own
    // "not itself a valid .closed marker" wrapper text is reached
    // only if `is_closed` somehow returned `Ok(false)` here, which a
    // directory never does (it always errs first).
    assert!(format!("{err}").contains("not a regular file"), "got: {err}");
}

/// The genuinely idempotent case still works once the target is
/// validated: a SECOND `mark_closed` for the same id, after a real
/// marker is already there, remains a no-op.
#[cfg(any(target_os = "linux", windows))]
#[test]
fn mark_closed_is_idempotent_over_a_genuine_prior_marker() {
    let dir = tempfile::tempdir().unwrap();
    mark_closed(dir.path(), "op-1").unwrap();
    mark_closed(dir.path(), "op-1").unwrap();
    assert!(is_closed(dir.path(), "op-1").unwrap());
}

#[test]
fn a_journal_file_over_the_size_cap_is_a_loud_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(JOURNAL_DIR_NAME)).unwrap();
    let key = record_key("op-1");
    let oversized = vec![b' '; (MAX_JOURNAL_RECORD_BYTES + 1) as usize];
    std::fs::write(dir.path().join(JOURNAL_DIR_NAME).join(format!("{key}.active")), oversized).unwrap();
    assert!(read_active(dir.path(), "op-1").is_err());
}
