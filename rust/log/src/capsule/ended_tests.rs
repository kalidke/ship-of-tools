use super::*;

#[test]
fn an_ending_round_trips_and_a_clear_removes_it() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read(dir.path()).unwrap(), None, "no leg has ended in a fresh folder");

    let asked = Ended { requested: true, reason: Some("quit".into()), ..Ended::default() };
    write(dir.path(), &asked).unwrap();
    assert_eq!(read(dir.path()).unwrap(), Some(asked));

    let whole = Ended { finished: true, requested: true, uptime_ms: Some(1500), exit_code: Some(0), ..Ended::default() };
    write(dir.path(), &whole).unwrap();
    assert_eq!(read(dir.path()).unwrap(), Some(whole), "the whole file replaces the request's");

    clear(dir.path()).unwrap();
    assert_eq!(read(dir.path()).unwrap(), None);
    clear(dir.path()).unwrap();
}

#[test]
fn each_leg_gets_the_next_number() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(super::fence::read_leg_epoch(dir.path()), None);
    assert_eq!(super::fence::next_leg_epoch(dir.path()).unwrap(), 1);
    assert_eq!(super::fence::next_leg_epoch(dir.path()).unwrap(), 2);
    assert_eq!(super::fence::read_leg_epoch(dir.path()), Some(2));
}

#[test]
fn a_failed_write_leaves_no_ending_visible() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-folder");
    assert!(write(&missing, &Ended { requested: true, ..Ended::default() }).is_err());
    assert_eq!(read(&missing).unwrap(), None);
}
