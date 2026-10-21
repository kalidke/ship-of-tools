use super::*;
use crate::rows::run::end_run::EndRunOutcome as O;
use std::path::PathBuf;

#[tokio::test(start_paused = true)]
async fn end_with_retry_table() {
    // Kept twice (starting), then a confirmed end: ended.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut script = vec![O::NotEnded("starting".into()), O::NotEnded("starting".into()), O::RecordVerified].into_iter();
    let got = retry_until(deadline, || {
        let o = script.next().expect("no attempt after a confirmed end");
        async move { confirmed(crate::rows::run::end::capsule_destroy_outcome_of(o)) }
    })
    .await;
    assert_eq!(got, Ok(true));

    // Refused forever: tried once per second, then not ended at the deadline.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut tries = 0;
    let got = retry_until(deadline, || {
        tries += 1;
        async { confirmed(crate::rows::run::end::capsule_destroy_outcome_of(O::NotEnded("refused".into()))) }
    })
    .await;
    assert_eq!(got, Err("refused".to_string()));
    assert_eq!(tries, 10, "one attempt per second across the 10 s");
    assert!(Instant::now() + RETRY_EVERY >= deadline, "gave up before the deadline");

    // An attempt still running at the deadline: abandoned, not ended.
    let deadline = Instant::now() + Duration::from_secs(10);
    let got = retry_until(deadline, || std::future::pending::<Result<(), String>>()).await;
    assert!(got.is_err());
    assert_eq!(Instant::now(), deadline);
}

#[test]
fn drawer_is_an_end_target() {
    let root = tempfile::tempdir().expect("tempdir");
    assert!(!drawer_is_target(root.path()), "no pointer, no drawer to end");

    std::fs::write(sot_log::supervisor::journal::pointer::pointer_path(root.path()), "x").expect("pointer");
    assert!(drawer_is_target(root.path()), "a drawer pointer makes the drawer an end target");
}

#[tokio::test(start_paused = true)]
async fn forget_on_unremovable_registration() {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut tries = 0;
    let ended = forget_unless_removed("stuck".into(), deadline, || {
        tries += 1;
        false
    })
    .await;
    assert_eq!(ended, Ended::Forget("stuck".into()));
    assert_eq!(tries, 5, "retried until the deadline");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut tries = 0;
    let ended = forget_unless_removed("late".into(), deadline, || {
        tries += 1;
        tries == 3
    })
    .await;
    assert_eq!(ended, Ended::Row("late".into()), "removed on a retry is not forgotten");
}

#[tokio::test(start_paused = true)]
async fn end_every_returns_by_the_shared_deadline() {
    let deadline = Instant::now() + Duration::from_secs(5);
    let joins = vec![
        ("stuck".to_string(), tokio::spawn(std::future::pending::<Ended>())),
        ("done".to_string(), tokio::spawn(async { Ended::Row("done".into()) })),
    ];
    let report = tokio::time::timeout_at(deadline + Duration::from_secs(1), join_by(deadline, joins))
        .await
        .expect("end_every did not return by its deadline");
    assert_eq!(report, EndReport { ended: vec!["done".into()], not_ended: 1, forget: Vec::new() });
}

#[tokio::test]
async fn non_capsule_row_is_counted_not_ended() {
    let root = tempfile::tempdir().expect("tempdir");
    let reg = Workspaces::new();
    let mut row = Workspace::from_label("tm", PathBuf::from("/p/tm"), false, "none".into(), String::new(), String::new());
    row.runtime = "tmux".to_string();
    reg.insert(row);
    let (events, _rx) = broadcast::channel(4);
    let report = end_every(reg.list(), &reg, &events, root.path(), "window closed", Instant::now() + Duration::from_secs(3)).await;
    assert_eq!(report.not_ended, 1, "a row the shutdown cannot end was not counted");
}

#[tokio::test]
async fn already_removed_row_is_not_kept() {
    let reg = Workspaces::new();
    let mut row = Workspace::from_label("gone", PathBuf::from("/p/gone"), false, "none".into(), String::new(), String::new());
    row.runtime = "capsule".to_string();
    let row = reg.insert(row);
    // Another end removed it first.
    let _ = reg.remove_by_id(&row.workspace_id);
    let (events, _rx) = broadcast::channel(4);
    let started = Instant::now();
    let ended = end_row(&row, false, &reg, &events, "window closed", started + Duration::from_secs(3)).await;
    assert_eq!(ended, Ended::Gone);
    assert!(started.elapsed() < RETRY_EVERY, "an already-removed row was retried");
}
