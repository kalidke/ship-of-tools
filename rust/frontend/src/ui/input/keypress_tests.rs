use super::*;

fn route(keep: Option<bool>, logical: Key, m: Modifiers, repeat: bool, synthetic: bool,
    pressed: bool, rebound: bool) -> (Option<QuitPromptStep>, usize, Vec<u8>) {
    route_q(keep, logical, m, repeat, synthetic, pressed, rebound, false)
}

fn route_q(keep: Option<bool>, logical: Key, m: Modifiers, repeat: bool, synthetic: bool,
    pressed: bool, rebound: bool, quit_again: bool) -> (Option<QuitPromptStep>, usize, Vec<u8>) {
    let mut bindings = KeyBindings::defaults();
    if rebound { bindings.merge_text("input.confirm = \"F11\""); }
    let context = help::Context { prompt: keep.is_some(), ..Default::default() };
    let mut observed = (None, 0, Vec::new());
    input_continuation(&mut observed, &logical, repeat, synthetic, pressed, keep, quit_again,
        |_| (bindings.resolve(&logical, Some(&logical), m, context.consumes_text(), |a| context.allows(a)), ()),
        |seen, route| match route {
            InputRoute::Prompt(step) => seen.0 = Some(step),
            InputRoute::Next(_, ()) => {
                seen.1 += 1;
                if let Key::Character(c) = &logical { seen.2.extend_from_slice(c.as_bytes()); }
            }
        });
    observed
}

fn modifiers() -> Modifiers { Modifiers { ctrl: false, alt: false, shift: false, super_: false } }

#[test]
fn quit_other_keys_cancel_and_are_consumed() {
    for keep in [false, true] {
        for key in [Key::Character("typed-fixture-text".into()), Key::Named(NamedKey::F12), Key::Named(NamedKey::Escape)] {
            let (step, calls, bytes) = route(Some(keep), key, modifiers(), false, false, true, false);
            assert_eq!(step, Some(QuitPromptStep::Cancel), "other key must cancel");
            assert_eq!((calls, bytes), (0, vec![]), "trigger reached downstream input");
        }
    }
}

#[test]
fn quit_enter_and_tab_use_identity_with_modifiers_and_rebindings() {
    for keep in [false, true] {
        for rebound in [false, true] {
            for bits in 0..16 {
                let m = Modifiers { ctrl: bits & 1 != 0, alt: bits & 2 != 0,
                    shift: bits & 4 != 0, super_: bits & 8 != 0 };
                for (key, want) in [(NamedKey::Tab, QuitPromptStep::Stay { keep: !keep }),
                    (NamedKey::Enter, QuitPromptStep::Leave(if keep { LeaveIntent::Keep } else { LeaveIntent::Close }))] {
                    let (step, calls, bytes) = route(Some(keep), Key::Named(key), m, false, false, true, rebound);
                    assert_eq!(step, Some(want), "logical Tab/Enter must win");
                    assert_eq!((calls, bytes), (0, vec![]));
                }
            }
        }
    }
}

/// A second Ctrl+Q is a Ctrl press and then the chord: neither changes the open question, so it stays open
/// instead of cancelling at the Ctrl and reopening at the Q.
#[test]
fn a_second_quit_chord_leaves_the_question_open() {
    for keep in [false, true] {
        for key in [NamedKey::Control, NamedKey::Shift, NamedKey::Alt, NamedKey::Super, NamedKey::Meta, NamedKey::AltGraph] {
            let seen = route(Some(keep), Key::Named(key), modifiers(), false, false, true, false);
            assert_eq!(seen, (Some(QuitPromptStep::Ignore), 0, vec![]), "a modifier alone changes nothing");
        }
        let ctrl = Modifiers { ctrl: true, ..modifiers() };
        let seen = route_q(Some(keep), Key::Character("q".into()), ctrl, false, false, true, false, true);
        assert_eq!(seen, (Some(QuitPromptStep::Ignore), 0, vec![]), "the chord is consumed and changes nothing");
        let seen = route_q(Some(keep), Key::Character("q".into()), modifiers(), false, false, true, false, false);
        assert_eq!(seen.0, Some(QuitPromptStep::Cancel), "a bare q is any other key");
    }
}

#[test]
fn raw_admission_and_repeat_controls() {
    for keep in [false, true] {
        for key in [Key::Named(NamedKey::Enter), Key::Named(NamedKey::Control), Key::Character("text".into())] {
            assert_eq!(route(Some(keep), key.clone(), modifiers(), false, true, true, false), (None, 0, vec![]));
            assert_eq!(route(Some(keep), key.clone(), modifiers(), false, false, false, false), (None, 0, vec![]));
            // Modifier repeats are rejected before the prompt at the parent; every accepted repeat ignores.
            let seen = route(Some(keep), key, modifiers(), true, false, true, false);
            assert!(seen.0.is_none() || seen.0 == Some(QuitPromptStep::Ignore));
            assert_eq!((seen.1, seen.2), (0, vec![]));
        }
    }
    assert_eq!(route(None, Key::Named(NamedKey::Control), modifiers(), false, false, true, false), (None, 0, vec![]));
    assert_eq!(route(None, Key::Character("text".into()), modifiers(), false, false, true, false), (None, 1, b"text".to_vec()));
}
