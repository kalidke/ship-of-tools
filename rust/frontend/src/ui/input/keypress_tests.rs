use super::*;

fn route(keep: Option<bool>, logical: Key, m: Modifiers, repeat: bool, synthetic: bool,
    pressed: bool, rebound: bool) -> (Option<QuitPromptStep>, usize, Vec<u8>) {
    route_q(keep, logical, m, repeat, synthetic, pressed, rebound, false)
}

fn route_q(keep: Option<bool>, logical: Key, m: Modifiers, repeat: bool, synthetic: bool,
    pressed: bool, rebound: bool, quit_again: bool) -> (Option<QuitPromptStep>, usize, Vec<u8>) {
    let mut bindings = KeyBindings::defaults();
    if rebound { bindings.merge_text("input.confirm = \"F11\""); }
    let mut prompts = FocusPrompts::new(PaneFocus::NavTree);
    if let Some(keep) = keep { prompts.ask_quit(keep); }
    let keep = prompts.quit_choice();
    let context = help::Context { prompt: keep.is_some(), ..Default::default() };
    let mut observed = (None, 0, Vec::new());
    input_continuation(&mut observed, &logical, repeat, synthetic, pressed, keep, quit_again,
        |_| (bindings.resolve(&logical, Some(&logical), m, context.consumes_text(), |a| context.allows(a)), ()),
        |_| false,
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

/// Keys follow what is drawn on top. The quit dialog takes a key before the reply bar is asked, so an Enter given to
/// the dialog is never the bar's; a bar that is in view takes a key before any binding or pane; and once the dialog
/// closes the bar's wait starts again, so an Enter right after it sends nothing.
#[test]
fn the_quit_dialog_takes_a_key_before_the_reply_bar_and_the_bar_before_the_panes() {
    use std::time::{Duration, Instant};
    struct Seen { bar: ReplyBar, at: Instant, panes: usize, asked: usize, sent: usize, prompt: Option<QuitPromptStep> }
    let t0 = Instant::now();
    let mut seen = Seen { bar: ReplyBar::default(), at: t0, panes: 0, asked: 0, sent: 0, prompt: None };
    seen.bar.offer(ReplyOffer {
        host: "<host-b>".into(), workspace_id: "id-7".into(), handle: "repo__box".into(), name: "project".into(),
        line: "[sot-page] approve".into(),
    }, Sight::Whole, t0).expect("the offer opens the bar");
    // Enter, as `keyboard_input` hands it: the bar reads the logical key, never a binding.
    let press = |seen: &mut Seen, quit: Option<bool>| {
        let enter = Key::Named(NamedKey::Enter);
        input_continuation(seen, &enter, false, false, true, quit, false, |_| (None, ()),
            |s| {
                s.asked += 1;
                match s.bar.key(bar_key_kind(&Key::Named(NamedKey::Enter), false, ModifiersState::empty()), s.at) {
                    BarKey::Pass => false,
                    BarKey::Send(_) => { s.sent += 1; true }
                    _ => true,
                }
            },
            |s, route| match route {
                InputRoute::Prompt(step) => s.prompt = Some(step),
                InputRoute::Next(..) => s.panes += 1,
            });
    };

    // The quit question opens at 5 s over an armed bar and takes the Enter; the bar is not asked.
    seen.bar.observe(Sight::Covered, t0 + Duration::from_secs(5));
    seen.at = t0 + Duration::from_secs(6);
    press(&mut seen, Some(false));
    assert_eq!((seen.prompt.as_ref(), seen.asked, seen.panes, seen.sent),
        (Some(&QuitPromptStep::Leave(LeaveIntent::Close)), 0, 0, 0));

    // It closes at 8 s. An Enter 0.3 s later is the bar's, and sends nothing; no pane sees it.
    seen.bar.observe(Sight::Whole, t0 + Duration::from_secs(8));
    seen.at = t0 + Duration::from_millis(8300);
    press(&mut seen, None);
    assert_eq!((seen.asked, seen.panes, seen.sent), (1, 0, 0));

    // A second after it came back, Enter sends once; the next Enter finds no bar and reaches the panes.
    seen.at = t0 + Duration::from_secs(9);
    press(&mut seen, None);
    assert_eq!((seen.asked, seen.panes, seen.sent), (2, 0, 1));
    seen.at = t0 + Duration::from_secs(10);
    press(&mut seen, None);
    assert_eq!((seen.asked, seen.panes, seen.sent), (3, 1, 1));
}
