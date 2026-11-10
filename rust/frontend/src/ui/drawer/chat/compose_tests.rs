use std::time::{Duration, Instant};

use sot_protocol::ops::ChatTarget;
use winit::keyboard::{Key, NamedKey};

use super::compose::{Compose, Edit};
use super::drawer::ChatDrawer;
use super::draft_edit;
use super::row_channel_tests::{answer, mapped};
use crate::ui::input::keybindings::Action;
use crate::ui::ResultRowIdentity;

fn row(id: &str) -> ResultRowIdentity {
    ResultRowIdentity { host: "a".to_string(), workspace_id: id.to_string() }
}

fn channel(c: &str) -> ChatTarget {
    serde_json::from_value(serde_json::json!({ "row": "R", "workspace": "T1", "channel": c })).unwrap()
}

fn ch(s: &str) -> Key {
    Key::Character(s.into())
}

/// Row `R`'s draft: its text and the channel it goes to.
fn held<'a>(c: &'a Compose, r: &ResultRowIdentity) -> Option<(&'a str, &'a str)> {
    c.draft(r).map(|d| (d.text.as_str(), d.dest.channel.as_str()))
}

#[test]
fn the_draft_keeps_its_destination_and_its_escaped_bound() {
    // A chord edits the draft only where it types: AltGr does, Ctrl+a does not, and an action's key edits nothing.
    assert!(matches!(draft_edit(None, &ch("@"), (true, true, false)), Some(Edit::Text("@"))));
    assert!(draft_edit(None, &ch("a"), (true, false, false)).is_none());
    assert!(draft_edit(None, &Key::Named(NamedKey::Space), (true, false, false)).is_none());
    assert!(matches!(draft_edit(None, &Key::Named(NamedKey::Space), (false, false, false)), Some(Edit::Space)));
    assert!(matches!(draft_edit(None, &Key::Named(NamedKey::Backspace), (false, false, false)), Some(Edit::Backspace)));
    assert!(matches!(draft_edit(None, &Key::Named(NamedKey::Backspace), (true, false, false)), Some(Edit::Backspace)), "Ctrl+Backspace is Backspace");
    assert!(draft_edit(Some(Action::Paste), &ch("v"), (false, false, true)).is_none(), "a key that resolved to an action types nothing");
    assert!(draft_edit(Some(Action::ChatLatest), &Key::Named(NamedKey::End), (false, false, false)).is_none());
    assert!(draft_edit(None, &Key::Named(NamedKey::Enter), (false, false, false)).is_none(), "Enter posts at B5m, not before");

    // No channel to write to: nothing is begun.
    let (r, c1, c2) = (row("R"), channel("C1"), channel("C2"));
    let mut none = Compose::default();
    assert!(none.edit(&r, None, Edit::Text("a")).is_err());

    // The bound counts the escaped form: 1,638 `&` fit and the 1,639th is refused.
    let mut c = Compose::default();
    for n in 1..=1638 {
        assert!(c.edit(&r, Some(&c1), Edit::Text("&")).is_ok(), "the {n}th");
    }
    assert!(c.edit(&r, Some(&c1), Edit::Text("&")).is_err(), "the 1,639th");
    assert!(c.edit(&r, Some(&c1), Edit::Space).is_ok() && c.edit(&r, Some(&c1), Edit::Backspace).is_ok());
    assert!(c.edit(&r, Some(&c1), Edit::Text("&&")).is_err(), "a paste past it is refused whole");
    assert!(c.edit(&r, Some(&c1), Edit::Newline).is_ok(), "a byte that fits still goes in");
    assert!(c.edit(&r, Some(&c1), Edit::Text("a\r\nb")).is_err());
    assert_eq!(held(&c, &r).map(|(t, _)| t.to_string()), Some(format!("{}\n", "&".repeat(1638))), "a refused paste adds none of its part that would fit");
    // A first paste past the bound begins no draft, so the next channel is the one it goes to.
    let mut big = Compose::default();
    assert!(big.edit(&r, Some(&c1), Edit::Text(&"&".repeat(1639))).is_err());
    assert!(held(&big, &r).is_none());
    // Backspace takes one character, with or without a chord.
    assert!(c.edit(&r, Some(&c1), Edit::Backspace).is_ok());
    assert_eq!(held(&c, &r).map(|(t, _)| t.len()), Some(1638));

    // Controls: CR LF and CR are LF, tab stays, every other C0 control goes.
    let mut c = Compose::default();
    c.edit(&r, Some(&c1), Edit::Text("a\r\nb\0c\x07d")).unwrap();
    c.edit(&r, Some(&c1), Edit::Text("\r\te\x1b")).unwrap();
    c.edit(&r, Some(&c1), Edit::Text("\x08f")).unwrap();
    assert_eq!(held(&c, &r), Some(("a\nbcd\n\tef", "C1")), "LF between, tab kept, other controls gone");
    c.edit(&r, Some(&c1), Edit::Text("\x7fg\u{85}\u{9b}h")).unwrap();
    assert_eq!(held(&c, &r), Some(("a\nbcd\n\tefgh", "C1")), "DEL and C1 controls go too");

    // The destination is fixed at the first character: the view moves to another channel and the draft stays.
    let mut c = Compose::default();
    c.edit(&r, Some(&c1), Edit::Text("hi")).unwrap();
    c.edit(&r, Some(&c2), Edit::Text("!")).unwrap();
    assert_eq!(held(&c, &r), Some(("hi!", "C1")));
    // Emptied, the draft is dropped and follows the view again.
    for _ in 0..3 {
        c.edit(&r, Some(&c2), Edit::Backspace).unwrap();
    }
    assert_eq!(held(&c, &r), None);
    c.edit(&r, Some(&c2), Edit::Text("gone")).unwrap();
    assert_eq!(held(&c, &r), Some(("gone", "C2")));

    // A row a listing retires drops its draft, and no other.
    c.retire(&[row("S")]);
    assert_eq!(held(&c, &r), Some(("gone", "C2")));
    c.retire(&[r.clone()]);
    assert_eq!(held(&c, &r), None);
}

/// A connected drawer on row `R`, its channel `C1` resolved and watched.
fn watching() -> (ChatDrawer, Instant) {
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.on_connection(&"a".to_string(), true, t0);
    d.tick(Some(row("R")), t0);
    let at = t0 + Duration::from_millis(200);
    d.tick(Some(row("R")), at);
    d.on_evt(&"a".to_string(), mapped("R"), at);
    (d, at)
}

#[test]
fn the_wired_draft_is_the_shown_rows_and_begins_in_its_own_watch() {
    // chat.newline is an edit; plain Enter is not (it posts at B5m).
    assert!(matches!(draft_edit(Some(Action::ChatNewline), &Key::Named(NamedKey::Enter), (false, false, false)), Some(Edit::Newline)));

    // No shown row, no draft.
    let (mut d, at) = watching();
    assert_eq!(d.edit(None, Edit::Text("a")), Err("no row to write for"));

    // The row is the one given at the key: `S` has no watch yet (the frame has not moved it), so nothing begins in `R`'s
    // channel for it, and `R`'s own draft is untouched.
    d.edit(Some(&row("R")), Edit::Text("hi")).unwrap();
    assert_eq!(d.edit(Some(&row("S")), Edit::Text("x")), Err("no channel to write to"));
    assert_eq!(d.draft(), Some(("hi", "C1")));

    // The destination is fixed at the first character: the view's channel moves, the draft stays.
    d.on_evt(&"a".to_string(), answer("R", serde_json::json!({ "state": "mapped", "workspace": "T1", "channel": "C2", "delivers": false })), at);
    d.edit(Some(&row("R")), Edit::Text("!")).unwrap();
    assert_eq!(d.draft(), Some(("hi!", "C1")));

    // A row a listing retires drops its draft.
    d.retire(&[row("R")]);
    assert_eq!(d.draft(), None);
    d.edit(Some(&row("R")), Edit::Text("again")).unwrap();
    assert_eq!(d.draft(), Some(("again", "C2")), "a new draft begins in the channel now watched");
}
