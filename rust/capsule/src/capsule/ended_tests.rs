use super::*;

#[test]
fn an_ending_round_trips_and_a_clear_removes_it() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read(dir.path()).unwrap(), None, "no run has ended in a fresh folder");

    let asked = Ended { requested: true, reason: Some("quit".into()), ..Ended::default() };
    write(dir.path(), &asked).unwrap();
    assert_eq!(read(dir.path()).unwrap(), Some(asked.clone()));

    let whole = Ended { finished: true, requested: true, uptime_ms: Some(1500), exit_code: Some(0), ..Ended::default() };
    write(dir.path(), &whole).unwrap();
    assert_eq!(read(dir.path()).unwrap(), Some(whole), "the whole file replaces the request's");

    clear(dir.path()).unwrap();
    assert_eq!(read(dir.path()).unwrap(), None);
    clear(dir.path()).unwrap();

    write(dir.path(), &asked).unwrap();
    publish_reset(dir.path()).unwrap();
    let published = read(dir.path()).unwrap().expect("a reset leaves a record");
    assert!(!published.requested, "the reset's record asks for no end");
}

#[test]
fn a_failed_write_leaves_no_ending_visible() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-folder");
    assert!(write(&missing, &Ended { requested: true, ..Ended::default() }).is_err());
    assert_eq!(read(&missing).unwrap(), None);
}
