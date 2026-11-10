use std::time::{Duration, Instant};

use winit::keyboard::{Key, NamedKey};

use super::compose::Edit;
use super::drawer::ChatDrawer;
use super::draft_edit;
use super::row_channel_tests::{answer, mapped};
use crate::ui::input::keybindings::Action;
use crate::ui::ResultRowIdentity;

fn row(id: &str) -> ResultRowIdentity {
    ResultRowIdentity { host: "a".to_string(), workspace_id: id.to_string() }
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

fn typed(d: &mut ChatDrawer, s: &str) -> Result<(), &'static str> {
    d.edit(Edit::Typed(s))
}

fn ch(s: &str) -> Key {
    Key::Character(s.into())
}

/// The wanted row's draft: its text and its channel.
fn held(d: &ChatDrawer) -> Option<(&str, &str)> {
    d.draft()
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
    let mut none = ChatDrawer::default();
    assert!(typed(&mut none, "a").is_err());

    // The bound counts the escaped form: 1,638 `&` fit and the 1,639th is refused.
    let (mut d, _) = watching();
    for n in 1..=1638 {
        assert!(typed(&mut d, "&").is_ok(), "the {n}th");
    }
    assert!(typed(&mut d, "&").is_err(), "the 1,639th");
    assert!(d.edit(Edit::Space).is_ok() && d.edit(Edit::Backspace).is_ok());
    assert!(d.edit(Edit::Paste("&&")).is_err(), "a paste past it is refused whole");
    assert!(d.edit(Edit::Newline).is_ok(), "a byte that fits still goes in");
    assert!(d.edit(Edit::Paste("a\r\nb")).is_err());

    // Controls: CR LF and CR are LF, tab stays, every other C0 control goes.
    let (mut d, _) = watching();
    d.edit(Edit::Paste("a\r\nb\0c\x07d")).unwrap();
    d.edit(Edit::Paste("\r\te\x1b")).unwrap();
    d.edit(Edit::Typed("\x08f")).unwrap();
    assert_eq!(held(&d), Some(("a\nbcd\n\tef", "C1")), "LF between, tab kept, other controls gone");

    // The destination is fixed at the first character: the view moves to another channel and the draft stays.
    let (mut d, at) = watching();
    typed(&mut d, "hi").unwrap();
    d.on_evt(&"a".to_string(), answer("R", serde_json::json!({ "state": "mapped", "workspace": "T1", "channel": "C2", "delivers": false })), at);
    typed(&mut d, "!").unwrap();
    assert_eq!(held(&d), Some(("hi!", "C1")));
    // Emptied, the draft is dropped and follows the view again.
    for _ in 0..3 {
        d.edit(Edit::Backspace).unwrap();
    }
    assert_eq!(held(&d), None);
    typed(&mut d, "gone").unwrap();
    assert_eq!(held(&d), Some(("gone", "C2")));

    // A row a listing retires drops its draft, and no other.
    d.retire(&[row("S")]);
    assert_eq!(held(&d), Some(("gone", "C2")));
    d.retire(&[row("R")]);
    assert_eq!(held(&d), None);
}
