//! Tests of `state`: the boundary file and the filed set. The set is pure; the file is read and written in a scratch folder.

use std::path::PathBuf;

use sot_protocol::chat::Ts;

use crate::mail::Filed;
use crate::state::{decode, encode, path, write, State, StateError, MAX_FILED};

fn ts(s: &str) -> Ts {
    Ts::parse(s).unwrap()
}

fn filed(workspace: &str, channel: &str, at: &str) -> Filed {
    Filed { workspace: workspace.into(), channel: channel.into(), ts: ts(at) }
}

/// A scratch folder that removes itself.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("sot-chat-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn load(s: &Scratch, floor: &str) -> Result<State, StateError> {
    State::load(&s.0, "T0AAAA", "C0AAAA", &ts(floor))
}

#[test]
fn the_boundary_is_the_later_of_the_file_and_the_floor_and_a_commit_never_lowers_it() {
    let s = Scratch::new("later");
    write(&s.0, "T0AAAA", "C0AAAA", &ts("1700000500.000000")).unwrap();
    // The file is later than the floor: the file's; the floor is later: the floor's, numerically (9 before 10).
    assert_eq!(load(&s, "1700000100.000000").unwrap().boundary(), &ts("1700000500.000000"));
    assert_eq!(load(&s, "1700000900.000000").unwrap().boundary(), &ts("1700000900.000000"));
    write(&s.0, "T0AAAA", "C0AAAA", &ts("9.000000")).unwrap();
    assert_eq!(load(&s, "10.000000").unwrap().boundary(), &ts("10.000000"));
    let mut state = load(&s, "1700000100.000000").unwrap();
    state.commit(&ts("1700000300.000000"));
    assert_eq!(state.boundary(), &ts("1700000300.000000"));
    state.commit(&ts("1700000200.000000"));
    assert_eq!(state.boundary(), &ts("1700000300.000000"));
    // A commit drops the filings at or below the new boundary.
    state.insert(&ts("1700000400.000000"));
    state.insert(&ts("1700000500.000000"));
    state.commit(&ts("1700000400.000000"));
    assert!(!state.contains(&ts("1700000400.000000")) && state.contains(&ts("1700000500.000000")));
}

#[test]
fn an_unreadable_state_file_stops_its_channel() {
    let s = Scratch::new("unreadable");
    let file = path(&s.0, "T0AAAA", "C0AAAA").unwrap();
    for bad in ["not a ts", "1700000100.000000\n1700000200.000000\n", "1700000100.000000\n\n", "1700000100", "", "\n"] {
        std::fs::write(&file, bad).unwrap();
        assert_eq!(load(&s, "1700000000.000000").err(), Some(StateError::Unreadable), "{bad:?}");
    }
    std::fs::write(&file, vec![b'1'; 5000]).unwrap();
    assert_eq!(load(&s, "1700000000.000000").err(), Some(StateError::Unreadable));
    std::fs::write(&file, [0xff, 0xfe]).unwrap();
    assert_eq!(load(&s, "1700000000.000000").err(), Some(StateError::Unreadable));
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    assert_eq!(load(&s, "1700000000.000000").err(), Some(StateError::Unreadable));
    std::fs::remove_dir(&file).unwrap();
    // A FIFO is refused at once, not waited on.
    #[cfg(unix)]
    {
        sot_platform::test_wait::run_within(std::process::Command::new("mkfifo").arg(&file), std::time::Duration::from_secs(10)).ok();
        assert_eq!(load(&s, "1700000000.000000").err(), Some(StateError::Unreadable));
    }
}

#[test]
fn a_missing_file_is_the_floor() {
    let s = Scratch::new("missing");
    let mut state = load(&s, "1700000100.000000").unwrap();
    assert_eq!(state.boundary(), &ts("1700000100.000000"));
    // No filing moves the boundary: it is never taken from one.
    state.merge(&[filed("T0AAAA", "C0AAAA", "1700000900.000000")]);
    assert_eq!(state.boundary(), &ts("1700000100.000000"));
    assert!(state.contains(&ts("1700000900.000000")));
    // The text round-trips, with or without its line end, and takes nothing else.
    assert_eq!(decode(&encode(&ts("1700000100.000100"))), Ok(ts("1700000100.000100")));
    assert_eq!(decode("1700000100.000100"), Ok(ts("1700000100.000100")));
}

#[test]
fn the_merge_takes_only_this_channels_filings_above_the_boundary() {
    let s = Scratch::new("merge");
    let mut state = load(&s, "1700000100.000000").unwrap();
    let dropped = state.merge(&[
        filed("T0AAAA", "C0AAAA", "1700000200.000000"),
        filed("T0AAAA", "C0BBBB", "1700000300.000000"),
        filed("T0BBBB", "C0AAAA", "1700000400.000000"),
        filed("T0AAAA", "C0AAAA", "1700000100.000000"),
        filed("T0AAAA", "C0AAAA", "1700000050.000000"),
    ]);
    assert_eq!(dropped, 0);
    assert_eq!(state.count(), 1);
    assert!(state.contains(&ts("1700000200.000000")));
}

#[test]
fn the_filed_set_keeps_its_newest_100000() {
    let s = Scratch::new("bound");
    let mut state = load(&s, "1.000000").unwrap();
    let all: Vec<Filed> = (0..MAX_FILED as u64 + 7).map(|n| filed("T0AAAA", "C0AAAA", &format!("{}.000000", 1000 + n))).collect();
    assert_eq!(state.merge(&all), 7);
    assert_eq!(state.count(), MAX_FILED);
    assert!(!state.contains(&ts("1006.000000")) && state.contains(&ts("1007.000000")));
    assert!(state.contains(&ts(&format!("{}.000000", 1000 + MAX_FILED as u64 + 6))));
}

#[test]
fn the_file_is_named_from_validated_ids_alone() {
    let dir = PathBuf::from("state");
    assert_eq!(path(&dir, "T0AAAA", "C0AAAA"), Ok(dir.join("delivery-T0AAAA-C0AAAA")));
    for (w, c) in [("../x", "C0AAAA"), ("T0AAAA", "C0/AAAA"), ("t0aaaa", "C0AAAA"), ("T0AAAA", ""), ("T0AAAA", "C0AAAA.tmp"), ("T0AAAA", "C0AAAA\n")] {
        assert_eq!(path(&dir, w, c), Err(StateError::BadId), "{w:?} {c:?}");
        assert_eq!(State::load(&dir, w, c, &ts("1.000000")).err(), Some(StateError::BadId));
        assert_eq!(write(&dir, w, c, &ts("1.000000")), Err(StateError::BadId));
    }
}
