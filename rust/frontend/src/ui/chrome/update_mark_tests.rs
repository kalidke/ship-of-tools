use super::*;
use sot_protocol::{MarkKind, UpdatePhase};

fn state(phase: UpdatePhase, version: &str) -> UpdateState {
    UpdateState { version: version.into(), ..UpdateState::of(phase) }
}

fn long(owner: &UpdateMark) -> Option<String> {
    owner.mark(None).map(|mark| mark.long)
}

/// The mark is what this computer's own daemon last said on its live connection, and no other computer's: a remote
/// daemon's state is never shown, a new connection begins with nothing said, a disconnect takes the mark away, and
/// the daemon's word after it is back brings the mark back.
#[test]
fn the_mark_is_the_local_daemons_word_and_goes_with_its_connection() {
    let (here, hub) = ("<here>".to_string(), "<hub>".to_string());
    let mut owner = UpdateMark::new(false);
    assert_eq!(owner.mark(None), None, "nothing connected");

    // A state said before any connection is known to be the local one is nobody's.
    owner.told(&here, state(UpdatePhase::Ready, "0.7.4"));
    assert_eq!(owner.mark(None), None);

    owner.connected(&hub, false);
    owner.told(&hub, state(UpdatePhase::Ready, "0.7.5"));
    assert_eq!(owner.mark(None), None, "another computer's daemon said ready");

    owner.connected(&here, true);
    assert_eq!(owner.mark(None), None, "a connection begins with nothing said");
    owner.told(&here, state(UpdatePhase::Preparing, "0.7.4"));
    assert_eq!(long(&owner).as_deref(), Some("update 0.7.4 preparing"));
    owner.told(&hub, state(UpdatePhase::NotApplied, "0.7.5"));
    assert_eq!(long(&owner).as_deref(), Some("update 0.7.4 preparing"), "the other computer's word changed the mark");
    owner.told(&here, state(UpdatePhase::Ready, "0.7.4"));
    let mark = owner.mark(None).expect("the local daemon said ready");
    assert_eq!((mark.long.as_str(), mark.short.as_str(), mark.kind), ("update 0.7.4 ready: quit keeping sessions, then start again", "0.7.4 ready", MarkKind::Ready));
    // A state with no mark takes the mark down.
    owner.told(&here, state(UpdatePhase::None, ""));
    assert_eq!(owner.mark(None), None);
    owner.told(&here, state(UpdatePhase::Ready, "0.7.4"));

    // The other computer's link going away changes nothing; the local one's takes the mark with it.
    owner.disconnected(&hub);
    assert!(owner.mark(None).is_some());
    owner.disconnected(&here);
    assert_eq!(owner.mark(None), None, "the mark outlived its connection");
    // Back: nothing until the daemon speaks again on the new connection.
    owner.connected(&here, true);
    assert_eq!(owner.mark(None), None, "a reconnect showed what the old connection said");
    owner.told(&here, state(UpdatePhase::Found, "0.7.5"));
    assert_eq!(long(&owner).as_deref(), Some("update 0.7.5 found"));
    // A new connection of the same daemon with no end of the old one seen begins with nothing said too.
    owner.connected(&here, true);
    assert_eq!(owner.mark(None), None, "a second connection showed what the first said");
    owner.told(&here, state(UpdatePhase::Found, "0.7.5"));

    // The same dial host reached another way is no longer this computer's own daemon.
    owner.connected(&here, false);
    owner.told(&here, state(UpdatePhase::Ready, "0.7.5"));
    assert_eq!(owner.mark(None), None);
}

/// A harness instance draws no mark, whatever its daemon says.
#[test]
fn a_harness_instance_draws_no_mark() {
    let here = "<here>".to_string();
    let mut owner = UpdateMark::new(true);
    owner.connected(&here, true);
    owner.told(&here, state(UpdatePhase::Ready, "0.7.4"));
    assert_eq!(owner.mark(None), None);
}

/// The state of the window's own update check is the mark only where no daemon on this computer has spoken: with no
/// local connection, and on a local connection that has said nothing (a daemon that predates the state). Once the
/// local daemon speaks its word is the mark, a state of its own with no mark included; another computer's daemon
/// changes nothing; and a harness instance draws none.
#[test]
fn the_windows_own_state_shows_only_where_no_local_daemon_spoke() {
    let (here, hub) = ("<here>".to_string(), "<hub>".to_string());
    let own = || Some(state(UpdatePhase::Ready, "0.7.4"));
    let words = |owner: &UpdateMark, own: Option<UpdateState>| owner.mark(own).map(|mark| mark.long);
    let mut owner = UpdateMark::new(false);
    assert_eq!(words(&owner, own()).as_deref(), Some("update 0.7.4 ready: quit keeping sessions, then start again"), "a window-only computer");
    assert_eq!(words(&owner, None), None);

    owner.connected(&hub, false);
    owner.told(&hub, state(UpdatePhase::Preparing, "0.7.5"));
    assert_eq!(words(&owner, own()).as_deref(), Some("update 0.7.4 ready: quit keeping sessions, then start again"), "the hub's word took the mark");

    owner.connected(&here, true);
    assert_eq!(words(&owner, own()).as_deref(), Some("update 0.7.4 ready: quit keeping sessions, then start again"), "a local daemon that has said nothing");
    owner.told(&here, state(UpdatePhase::Preparing, "0.7.5"));
    assert_eq!(words(&owner, own()).as_deref(), Some("update 0.7.5 preparing"), "the local daemon spoke");
    owner.told(&here, state(UpdatePhase::None, ""));
    assert_eq!(words(&owner, own()), None, "the local daemon says up to date, so the window's own state is not shown");
    owner.disconnected(&here);
    assert_eq!(words(&owner, own()).as_deref(), Some("update 0.7.4 ready: quit keeping sessions, then start again"), "the local daemon is gone");

    assert_eq!(UpdateMark::new(true).mark(own()), None, "a harness instance");
}
