use sot_protocol::ops::ChatTarget;
use winit::keyboard::{Key, NamedKey};

use super::compose::{Compose, Edit};
use super::draft_edit;
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
    assert!(matches!(draft_edit(None, &ch("@"), (true, true, false)), Some(Edit::Typed("@"))));
    assert!(draft_edit(None, &ch("a"), (true, false, false)).is_none());
    assert!(draft_edit(None, &Key::Named(NamedKey::Space), (true, false, false)).is_none());
    assert!(matches!(draft_edit(None, &Key::Named(NamedKey::Space), (false, false, false)), Some(Edit::Space)));
    assert!(matches!(draft_edit(None, &Key::Named(NamedKey::Backspace), (false, false, false)), Some(Edit::Backspace)));
    assert!(matches!(draft_edit(Some(Action::ChatNewline), &Key::Named(NamedKey::Enter), (false, false, false)), Some(Edit::Newline)));
    assert!(draft_edit(Some(Action::ChatLatest), &Key::Named(NamedKey::End), (false, false, false)).is_none());
    assert!(draft_edit(None, &Key::Named(NamedKey::Enter), (false, false, false)).is_none(), "Enter posts at B5m, not before");

    // No channel to write to: nothing is begun.
    let (r, c1, c2) = (row("R"), channel("C1"), channel("C2"));
    let mut none = Compose::default();
    assert!(none.edit(&r, None, Edit::Typed("a")).is_err());

    // The bound counts the escaped form: 1,638 `&` fit and the 1,639th is refused.
    let mut c = Compose::default();
    for n in 1..=1638 {
        assert!(c.edit(&r, Some(&c1), Edit::Typed("&")).is_ok(), "the {n}th");
    }
    assert!(c.edit(&r, Some(&c1), Edit::Typed("&")).is_err(), "the 1,639th");
    assert!(c.edit(&r, Some(&c1), Edit::Space).is_ok() && c.edit(&r, Some(&c1), Edit::Backspace).is_ok());
    assert!(c.edit(&r, Some(&c1), Edit::Paste("&&")).is_err(), "a paste past it is refused whole");
    assert!(c.edit(&r, Some(&c1), Edit::Newline).is_ok(), "a byte that fits still goes in");
    assert!(c.edit(&r, Some(&c1), Edit::Paste("a\r\nb")).is_err());

    // Controls: CR LF and CR are LF, tab stays, every other C0 control goes.
    let mut c = Compose::default();
    c.edit(&r, Some(&c1), Edit::Paste("a\r\nb\0c\x07d")).unwrap();
    c.edit(&r, Some(&c1), Edit::Paste("\r\te\x1b")).unwrap();
    c.edit(&r, Some(&c1), Edit::Typed("\x08f")).unwrap();
    assert_eq!(held(&c, &r), Some(("a\nbcd\n\tef", "C1")), "LF between, tab kept, other controls gone");

    // The destination is fixed at the first character: the view moves to another channel and the draft stays.
    let mut c = Compose::default();
    c.edit(&r, Some(&c1), Edit::Typed("hi")).unwrap();
    c.edit(&r, Some(&c2), Edit::Typed("!")).unwrap();
    assert_eq!(held(&c, &r), Some(("hi!", "C1")));
    // Emptied, the draft is dropped and follows the view again.
    for _ in 0..3 {
        c.edit(&r, Some(&c2), Edit::Backspace).unwrap();
    }
    assert_eq!(held(&c, &r), None);
    c.edit(&r, Some(&c2), Edit::Typed("gone")).unwrap();
    assert_eq!(held(&c, &r), Some(("gone", "C2")));

    // A row a listing retires drops its draft, and no other.
    c.retire(&[row("S")]);
    assert_eq!(held(&c, &r), Some(("gone", "C2")));
    c.retire(&[r.clone()]);
    assert_eq!(held(&c, &r), None);
}
