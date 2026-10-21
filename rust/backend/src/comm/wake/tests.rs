use super::*;

#[test]
fn batches() {
    let t0 = Instant::now();
    let later = t0 + Duration::from_secs(601);
    let w = Woken { line: 3, at: t0, enter_owed: false };
    let mail = |unread, fresh| Scan { total: 3, unread, fresh };
    // After a restart the map is empty: unread mail is a new batch.
    assert_eq!(decide(&mail(3, 3), None, t0), Decision::Wake);
    // One batch, one wake: nothing fresh, not yet ten minutes.
    assert_eq!(decide(&mail(3, 0), Some(&w), t0), Decision::Hold);
    // A new line after the wake is a new batch.
    assert_eq!(decide(&mail(4, 1), Some(&w), t0), Decision::Wake);
    // Still unread ten minutes on: one repeat.
    assert_eq!(decide(&mail(3, 0), Some(&w), later), Decision::Wake);
    // Read: forgotten.
    assert_eq!(decide(&mail(0, 0), Some(&w), later), Decision::Clear);
    // A typed line owed its Enter: only Enter alone, fresh mail or not, until REPEAT_AFTER; then a new line; read,
    // still only Enter alone (`an_owed_enter_outlives_the_mail`).
    let owed = Woken { enter_owed: true, ..w };
    assert_eq!(decide(&mail(3, 0), Some(&owed), t0), Decision::Complete);
    assert_eq!(decide(&mail(4, 1), Some(&owed), t0), Decision::Complete);
    assert_eq!(decide(&mail(4, 1), Some(&owed), later), Decision::Wake);
    assert_eq!(decide(&mail(0, 0), Some(&owed), t0), Decision::Complete);
}

/// A line typed and left unsent stays owed its Enter after its mail is read (by the Stop hook, or `comm-poll.sh` by
/// hand), at any age, so a later tick still finishes it. Fails while a read inbox forgets the debt (Clear), which
/// leaves the line in the prompt.
#[test]
fn an_owed_enter_outlives_the_mail() {
    let t0 = Instant::now();
    let owed = Woken { line: 3, at: t0, enter_owed: true };
    let read = Scan { total: 3, unread: 0, fresh: 0 };
    assert_eq!(decide(&read, Some(&owed), t0), Decision::Complete);
    assert_eq!(decide(&read, Some(&owed), t0 + REPEAT_AFTER * 3), Decision::Complete);
    assert_eq!(decide(&read, Some(&Woken { enter_owed: false, ..owed }), t0), Decision::Clear);
}

/// The debt ends on an empty box only with the mail read and REPEAT_AFTER past the typing; a box that holds the
/// line, or anything else, keeps it. Fails if an empty box ends it sooner (a slow echo would then be left in the
/// prompt) or never (an empty row would be attached to every tick for good).
#[test]
fn an_empty_box_ends_the_debt_only_with_the_mail_read_and_repeat_after_past() {
    let t0 = Instant::now();
    let later = t0 + REPEAT_AFTER;
    let owed = Woken { line: 3, at: t0, enter_owed: true };
    let (read, unread) = (Scan { total: 3, unread: 0, fresh: 0 }, Scan { total: 3, unread: 3, fresh: 0 });
    assert!(line_gone(&read, Some(&owed), Some(LINE_NOT_SHOWN), later));
    assert!(!line_gone(&read, Some(&owed), Some(LINE_NOT_SHOWN), later - Duration::from_secs(1)));
    assert!(!line_gone(&unread, Some(&owed), Some(LINE_NOT_SHOWN), later));
    for other in [None, Some("cursor not at the prompt"), Some("stop hook running")] {
        assert!(!line_gone(&read, Some(&owed), other, later), "{other:?}");
    }
    assert!(!line_gone(&read, Some(&Woken { enter_owed: false, ..owed }), Some(LINE_NOT_SHOWN), later));
    assert!(!line_gone(&read, None, Some(LINE_NOT_SHOWN), later));
}

#[test]
fn a_stop_mark_holds_only_inside_the_bound() {
    let now = 1_800_000_000;
    let iso = crate::comm::registry::registry::iso8601_utc_from_secs;
    let at = |off: i64| iso((now as i64 + off) as u64);
    let reg = |entry: &str| format!(r#"{{"agents":{{"h":{entry}}}}}"#).into_bytes();
    let mark = |off: i64| reg(&format!(r#"{{"floor":"user","stop_at":"{}"}}"#, at(off)));
    for off in [0, -60, 1] {
        assert!(stop_hook_running(&mark(off), "h", now), "offset {off}");
    }
    for off in [-61, 2, 60] {
        assert!(!stop_hook_running(&mark(off), "h", now), "offset {off}");
    }
    assert!(!stop_hook_running(&reg(r#"{"floor":"user"}"#), "h", now));
    assert!(!stop_hook_running(&reg(r#"{"stop_at":null}"#), "h", now));
    assert!(!stop_hook_running(&reg(r#"{"stop_at":5}"#), "h", now));
    assert!(!stop_hook_running(&mark(0), "other", now));
    assert!(!stop_hook_running(b"", "h", now));
    assert!(!stop_hook_running(b"{", "h", now));
}

#[test]
fn a_refusal_streak_logs_one_line_and_a_wake_or_a_read_ends_it() {
    let log = sot_log::test_log::capture();
    let text = || log.text();
    let count = || text().matches("keeps refusing the wake").count();
    let refused = || Step::Refused(Refusal { reason: "input not empty", border: "──── named-session ─".to_string(), detail: None, row: "col 2: a".to_string() });
    let (mut woken, mut streaks) = (HashMap::new(), HashMap::new());
    let t0 = Instant::now();
    let mut at = |d: Duration, step: Step| settle(&mut woken, &mut streaks, "h".to_string(), step, t0 + d);
    // Inside the bound: silent. Fails if the bound is ignored.
    at(Duration::ZERO, refused());
    at(REFUSED_FOR - Duration::from_secs(1), refused());
    assert_eq!(count(), 0, "inside the bound: silent");
    // One line per streak, with its fields. Fails if the info! is deleted, a field dropped, or a streak logs twice.
    at(REFUSED_FOR, refused());
    at(REFUSED_FOR * 5, refused());
    assert_eq!(count(), 1, "one line per streak");
    let t = text();
    assert!(t.contains("handle=h ") && t.contains("reason=\"input not empty\"") && t.contains("border=\"──── named-session ─\"") && t.contains("row=\"col 2: a\""), "{t}");
    // A wake ends the streak. Fails if it does not (the old streak is logged, so the count stays 1).
    at(REFUSED_FOR * 6, Step::Woke(Woken { line: 1, at: t0, enter_owed: false }));
    at(REFUSED_FOR * 7, refused());
    assert_eq!(count(), 1, "a new streak waits its own bound");
    at(REFUSED_FOR * 8, refused());
    assert_eq!(count(), 2);
    // A read inbox ends it too. Fails if Clear leaves the streak.
    at(REFUSED_FOR * 9, Step::Clear);
    at(REFUSED_FOR * 10, refused());
    at(REFUSED_FOR * 11, refused());
    assert_eq!(count(), 3);
}

#[test]
fn an_attach_failure_skips_the_row() {
    let out: Result<WakeOutcome, HeadlessError> = Err(HeadlessError { phase: "attach", detail: "no supervisor".into(), submitted: false });
    assert!(matches!(step_of("h", out, (None, String::new()), 1, Instant::now()), Step::Skip));
}

/// The one text refusal left: `send_and_wait_recorded`'s phase `input`, in the wake's `phase: detail` shape.
const STALE: &str = "input: input refused as stale (the take epoch changed); this op is never retried";

#[test]
fn a_typed_line_counts_as_the_wake_and_a_write_refused_as_stale_is_tried_again() {
    let now = Instant::now();
    let seen = || (None, "b".to_string());
    let typed_no_enter = WakeOutcome::TypedNoEnter { reason: "typed text not in main's input box", border: String::new(), row: String::new() };
    assert!(matches!(step_of("h", Ok(typed_no_enter), seen(), 7, now), Step::Woke(Woken { line: 7, enter_owed: true, .. })));
    // An Enter of unknown delivery may have been lost, leaving the line in the box: owed, so a later tick finishes it.
    let enter = WakeOutcome::Unconfirmed { step: "enter", detail: "record: input delivery unknown".into() };
    assert!(matches!(step_of("h", Ok(enter), seen(), 7, now), Step::Woke(Woken { line: 7, enter_owed: true, .. })));
    let text = WakeOutcome::Unconfirmed { step: "text", detail: STALE.into() };
    match step_of("h", Ok(text), seen(), 7, now) {
        Step::Refused(r) => {
            assert_eq!(r.reason, "text not confirmed");
            assert_eq!(r.detail.as_deref(), Some(STALE));
        }
        _ => panic!("a text write refused as stale is a refusal"),
    }
}

/// An Enter the leg refused as stale (phase `input`) wrote nothing (`send_and_wait_recorded`), so the typed line
/// still owes it: a later tick sends Enter alone once the box holds just the line. Fails while the stale Enter
/// is booked as possibly delivered (`enter_owed: false`), which leaves the line unsent until REPEAT_AFTER.
#[test]
fn an_enter_refused_as_stale_is_still_owed() {
    let stale = HeadlessError { phase: "input", detail: "input refused as stale (the take epoch changed); this op is never retried".into(), submitted: true };
    let out = attempt::wake_outcome(Err(stale));
    let step = step_of("h", Ok(out), (None, "b".to_string()), 7, Instant::now());
    assert!(matches!(step, Step::Woke(Woken { line: 7, enter_owed: true, .. })), "an Enter refused as stale must stay owed");
}

#[test]
fn a_text_write_of_unknown_delivery_owes_its_enter_and_is_not_typed_again() {
    let out = WakeOutcome::TextUnknown { detail: "record: input delivery unknown".into() };
    assert!(matches!(step_of("h", Ok(out), (None, "b".to_string()), 7, Instant::now()), Step::Woke(Woken { line: 7, enter_owed: true, .. })));
}

#[test]
fn an_unconfirmed_write_warns_once_per_streak() {
    let log = sot_log::test_log::capture();
    let count = || log.text().matches(&format!("text not confirmed ({STALE})")).count();
    let u = || Step::Refused(Refusal { reason: "text not confirmed", border: "b".to_string(), detail: Some(STALE.to_string()), row: String::new() });
    let (mut woken, mut streaks) = (HashMap::new(), HashMap::new());
    let t0 = Instant::now();
    let mut at = |secs: u64, step: Step| settle(&mut woken, &mut streaks, "h".to_string(), step, t0 + Duration::from_secs(secs));
    for secs in [0, 2, 4] {
        at(secs, u());
    }
    assert_eq!(count(), 1);
    at(6, Step::Woke(Woken { line: 1, at: t0, enter_owed: false }));
    at(8, u());
    assert_eq!(count(), 2);
}

#[test]
fn a_typed_line_refusal_logs_the_row_the_gate_read() {
    // The box holds the wake line and the tail after it is undimmed: a dim tail reads blank to the gate, so only
    // an undimmed one refuses on Linux (the control below). The NBSP after the glyph is the box's own mark.
    let log = sot_log::test_log::capture();
    let line = "[sot-comm] you have mail: run comm-poll.sh";
    let rule = "\u{2500}".repeat(80);
    let screen_of = |tail: &str| {
        let mut parser = vt100_ctt::Parser::new(3, 80, 0);
        parser.process(format!("{rule}\r\n\u{276f}\u{a0}{line}{tail}\r\n{rule}\x1b[2;{}H", 3 + line.len()).as_bytes());
        parser
    };
    let gate = |parser: &vt100_ctt::Parser| {
        let screen = parser.screen();
        let lines = super::screen::free_test_lines(screen);
        super::screen::typed_refusal(&lines, Some(screen.cursor_position()), "claude", false, line)
    };
    assert_eq!(gate(&screen_of("\x1b[2m try this\x1b[22m")), None, "the dimmed control reads blank");
    let undimmed = screen_of(" try this");
    let Some(reason) = gate(&undimmed) else { panic!("the undimmed tail must refuse") };
    let outcome = WakeOutcome::TypedNoEnter { reason, border: String::new(), row: super::screen::cursor_row_trace(undimmed.screen()) };
    assert!(matches!(step_of("h", Ok(outcome), (None, String::new()), 1, Instant::now()), Step::Woke(_)));
    // The row as the gate read it: the glyph, the NBSP, the wake line, then the tail, with the cursor after the line.
    let row = "col 44: U+276F U+00A0 [ s o t - c o m m ] U+0020 y o u U+0020 h a v e U+0020 m a i l : U+0020 r u n U+0020 c o m m - p o l l . s h U+0020 t r y U+0020 t h i s";
    assert!(log.text().contains(row), "{}", log.text());
}
