//! Existing log characterizations, driven through public owner operations.
use crate::ui::{DraftEdit, OutgoingReq, ReplFrame, ReplModel, ReplTarget, WsKey};

fn row() -> WsKey {
    ("host".into(), "row".into())
}
fn target() -> ReplTarget {
    ReplTarget {
        host: "host".into(),
        workspace_id: Some("row".into()),
        view_key: row(),
    }
}
fn submit(m: &mut ReplModel, code: &str, completed: bool) -> u64 {
    m.edit_draft(DraftEdit::Replace(code.into()));
    let p = m.begin_eval(target()).unwrap();
    let id = p.submission.eval_id();
    m.complete_submission(p.submission, Ok(()));
    if completed {
        m.apply_eval_completion("host".into(), id, &row(), 1, vec![]);
    }
    id
}
fn interrupt(m: &mut ReplModel) -> (Vec<u64>, bool) {
    let p = m.prepare_interrupt(target()).unwrap();
    let OutgoingReq::ReplInterrupt {
        eval_ids, force, ..
    } = p.request()
    else {
        panic!("interrupt")
    };
    m.complete_interrupt(Some(p), true);
    (eval_ids, force)
}

#[test]
fn a_second_ctrl_c_on_the_same_evals_forces() {
    let mut m = ReplModel::default();
    let a = submit(&mut m, "a", false);
    assert_eq!(interrupt(&mut m), (vec![a], false));
    assert_eq!(interrupt(&mut m), (vec![a], true));
    let b = submit(&mut m, "b", false);
    assert_eq!(interrupt(&mut m), (vec![a, b], false));
    assert_eq!(interrupt(&mut m), (vec![a, b], true));
}

#[test]
fn a_ctrl_c_names_every_eval_in_flight_and_no_other() {
    let mut m = ReplModel::default();
    submit(&mut m, "done", true);
    let b = submit(&mut m, "b", false);
    m.apply_frame(
        "host".into(),
        1 << 40,
        &row(),
        row(),
        ReplFrame::Started {
            run_id: "session-run".into(),
            origin: "session".into(),
            display: "run x.jl".into(),
        },
    );
    submit(&mut m, "also done", true);
    assert_eq!(interrupt(&mut m).0, [b, 1 << 40]);
}

#[test]
fn history_step_back_returns_none_on_empty_log() {
    let mut m = ReplModel::default();
    m.edit_draft(DraftEdit::Replace("draft".into()));
    assert!(!m.history_back());
    assert!(!m.history_forward());
    assert_eq!(m.input(), "draft");
}

#[test]
fn history_step_back_skips_in_flight_entries() {
    let mut m = ReplModel::default();
    submit(&mut m, "x = 1", true);
    submit(&mut m, "x = 2", false);
    m.edit_draft(DraftEdit::Replace("draft".into()));
    assert!(m.history_back());
    assert_eq!(m.input(), "x = 1");
    assert!(!m.history_back());
    assert!(m.history_forward());
    assert_eq!(m.input(), "draft");
}

#[test]
fn history_step_back_walks_oldest_to_newest_in_reverse() {
    let mut m = ReplModel::default();
    for code in ["first", "second", "third"] {
        submit(&mut m, code, true);
    }
    for code in ["third", "second", "first"] {
        assert!(m.history_back());
        assert_eq!(m.input(), code);
    }
    assert!(!m.history_back());
    assert_eq!(m.input(), "first");
}

#[test]
fn history_step_forward_no_op_when_not_walking() {
    let mut m = ReplModel::default();
    submit(&mut m, "a", true);
    assert!(!m.history_forward());
    assert_eq!(m.input(), "");
}

#[test]
fn history_step_forward_past_newest_restores_saved_buffer() {
    let mut m = ReplModel::default();
    submit(&mut m, "first", true);
    submit(&mut m, "second", true);
    m.edit_draft(DraftEdit::Replace("in-progress".into()));
    assert!(m.history_back());
    assert!(m.history_back());
    assert!(m.history_forward());
    assert_eq!(m.input(), "second");
    assert!(m.history_forward());
    assert_eq!(m.input(), "in-progress");
    assert!(!m.history_forward());
}
