use super::*;

/// A crashed authority's own
/// admitted-but-unfinished `Stop` is FINISHED as terminal `Stopping`
/// by the next restart's recovery pass — loud on failure, via the
/// bare `?` `reconcile_journal_on_startup` already propagates.
#[test]
fn recovery_finishes_a_crashed_stop_as_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let record = journal::ActiveRecord {
        operation_id: "stop-1".into(),
        digest: digest_of(&SupervisorOp::Stop).unwrap(),
        op: journal::ActiveOp::Stop,
    };
    journal::begin(dir.path(), "stop-1", &record).unwrap();
    reconcile_journal_on_startup(dir.path()).unwrap();
    assert_eq!(journal::read_terminal(dir.path(), "stop-1").unwrap(), Some(journal::TerminalRecord::Stopping));
}
