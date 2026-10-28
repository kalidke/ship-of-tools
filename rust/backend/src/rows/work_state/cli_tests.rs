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
    let (e, h, wait, _) = parse(&args(&["prompt", "--handle", "h", "--origin", "user"]), no_text).expect("parses");
    assert_eq!((e, h.as_str(), wait), (Event::Prompt(Origin::User), "h", LOCK_WAIT));
    let (e, _, _, _) = parse(&args(&["prompt", "--handle", "h"]), no_text).expect("parses");
    assert_eq!(e, Event::Prompt(Origin::Machine), "a prompt's origin defaults to machine");
    let (e, _, wait, _) = parse(&args(&["tick", "--handle", "h", "--lock-wait", "0"]), no_text).expect("parses");
    assert_eq!((e, wait), (Event::Tick, Duration::ZERO));
    let (e, _, _, _) = parse(&args(&["blocked", "--handle", "h", "--text"]), || Ok("/a question?".into())).expect("parses");
    assert_eq!(e, Event::Declare(Declaration::Blocked, Some("/a question?".into())));
    let (e, _, _, _) = parse(&args(&["working", "--handle", "h"]), no_text).expect("parses");
    assert_eq!(e, Event::Declare(Declaration::Working, None), "no --text keeps the last line");
    let (e, _, _, _) = parse(&args(&["dialog", "--handle", "h", "--text"]), || Ok("codex permission request: Bash".into())).expect("parses");
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

/// A blocked stamp that owes a decision page is refused with its reason and the fix, unless the Stop hook has already
/// asked for the page (`--page-nudged`); a question that names its page, and every other verb, goes through.
#[test]
fn a_blocked_stamp_that_owes_a_page_is_refused_until_nudged() {
    let merge = "Merge it? Irreversible: main moves. Options: (a) merge; (b) wait. Recommend (a).";
    let (e, _, _, nudged) = parse(&args(&["blocked", "--handle", "h", "--text"]), || Ok(merge.into())).expect("parses");
    let line = page_refusal(&e, nudged).expect("an Irreversible question with no page is refused");
    assert!(line.contains("Irreversible") && line.contains("/decision-page") && line.contains("Page:"), "{line}");
    let (e, _, _, nudged) = parse(&args(&["blocked", "--handle", "h", "--text", "--page-nudged"]), || Ok(merge.into())).expect("parses");
    assert!(nudged && page_refusal(&e, nudged).is_none(), "the hook's stamp after its nudge goes through");
    let paged = format!("{merge} Page: /p/decision.html");
    assert!(page_refusal(&Event::Declare(Declaration::Blocked, Some(paged)), false).is_none());
    assert!(page_refusal(&Event::Declare(Declaration::Waiting, Some(merge.into())), false).is_none());
}
