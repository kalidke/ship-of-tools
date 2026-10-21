use super::*;

/// A writer whose liveness cannot be
/// disproven (here: `writer.lock` genuinely HELD, with no pipe
/// bound at all for this voyage — the same "pipe absent" state a
/// real post-teardown window produces) must be `PendingWriter`,
/// never `PreBarrierFailed` — the operation stays untouched, never
/// released for a respawn to run over a writer that might still be
/// alive.
#[test]
fn a_held_writer_lock_with_no_pipe_is_pending_writer_never_pre_barrier_failed() {
    let dir = tempfile::tempdir().unwrap();
    let voyage_id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
    let root = voyage_root_path(dir.path(), &voyage_id);
    let _held = host::lock_writer(&root.join("writer.lock")).unwrap();

    let (tx, _rx) = mpsc::channel();
    let result = finish_end_run_without_process(dir.path(), Some("op-1"), &voyage_id, None, Some(&tx));
    assert!(matches!(result, Ok(EndRunReconciliation::PendingWriter)), "expected PendingWriter, got {result:?}");
    // No journal entry was ever begun for "op-1" in this test, and
    // PendingWriter must not have created one either -- there is
    // nothing to release or respawn over.
    assert!(journal::read_active(dir.path(), "op-1").unwrap().is_none());
}
