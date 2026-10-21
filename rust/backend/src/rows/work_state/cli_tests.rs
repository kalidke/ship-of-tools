//! Tests of `sotd work-state`'s command line.

use super::*;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

fn no_text() -> std::io::Result<String> {
    panic!("the text was read without --text")
}

/// Every verb maps to its event; the text is read only with `--text`, and only from the reader (standard input).
#[test]
fn a_verb_names_its_event_and_the_text_comes_from_stdin() {
    let (e, h, wait) = parse(&args(&["prompt", "--handle", "h", "--origin", "user"]), no_text).expect("parses");
    assert_eq!((e, h.as_str(), wait), (Event::Prompt(Origin::User), "h", LOCK_WAIT));
    let (e, _, _) = parse(&args(&["prompt", "--handle", "h"]), no_text).expect("parses");
    assert_eq!(e, Event::Prompt(Origin::Machine), "a prompt's origin defaults to machine");
    let (e, _, wait) = parse(&args(&["tick", "--handle", "h", "--lock-wait", "0"]), no_text).expect("parses");
    assert_eq!((e, wait), (Event::Tick, Duration::ZERO));
    let (e, _, _) = parse(&args(&["blocked", "--handle", "h", "--text"]), || Ok("/a question?".into())).expect("parses");
    assert_eq!(e, Event::Declare(Declaration::Blocked, Some("/a question?".into())));
    let (e, _, _) = parse(&args(&["working", "--handle", "h"]), no_text).expect("parses");
    assert_eq!(e, Event::Declare(Declaration::Working, None), "no --text keeps the last line");
    let (e, _, _) = parse(&args(&["dialog", "--handle", "h", "--text"]), || Ok("codex permission request: Bash".into())).expect("parses");
    assert_eq!(e, Event::Dialog("codex permission request: Bash".into()));
}

/// A missing handle, an unknown verb or argument, a bad origin or wait is refused before anything is read or written.
#[test]
fn a_bad_command_line_is_refused() {
    for bad in [
        &["working"][..],
        &["working", "--handle", ""],
        &["read", "--handle", "h"],
        &["working", "--handle", "h", "--loud"],
        &["prompt", "--handle", "h", "--origin", "robot"],
        &["tick", "--handle", "h", "--lock-wait", "soon"],
        &[],
    ] {
        assert!(parse(&args(bad), no_text).is_err(), "{bad:?}");
    }
}
