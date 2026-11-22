// Wire tests for the update's state: its lenient read on its carriers, its bounds, its words and its evt name.

use super::*;

fn state(phase: UpdatePhase, version: &str, detail: &str) -> UpdateState {
    UpdateState { version: version.into(), detail: detail.into(), ..UpdateState::of(phase) }
}

/// A hello reply that fails to parse is a window that cannot connect, so a value in `update` that is not an update
/// state reads as none on both carriers; a detail past its bound is cut on a character boundary, written and read; and
/// a phase added later reads as `other`.
#[test]
fn an_update_state_that_does_not_decode_never_refuses_its_hello() {
    let hello = |update: serde_json::Value| {
        serde_json::json!({"session_id": "s1", "revision": 3, "snapshot_pending": false, "protocol": 4, "update": update})
    };
    let version = |update: serde_json::Value| {
        serde_json::json!({"app_version": "0.7.4", "protocol": 4, "lane_build": "", "update": update})
    };
    let not_states = [
        serde_json::json!(7),
        serde_json::json!("ready"),
        serde_json::json!(null),
        serde_json::json!([]),
        serde_json::json!({}),
        serde_json::json!({"phase": 3}),
        serde_json::json!({"phase": "ready", "version": 5}),
        serde_json::json!({"phase": "none", "checked_s": -1}),
        serde_json::json!({"version": "0.7.4"}),
    ];
    for bad in not_states {
        let res: HelloRes = serde_json::from_value(hello(bad.clone())).unwrap_or_else(|e| panic!("{bad} refused the hello: {e}"));
        assert_eq!((res.update, res.protocol), (None, 4), "{bad}");
        let daemon: DaemonVersion = serde_json::from_value(version(bad.clone())).unwrap_or_else(|e| panic!("{bad} refused the answer: {e}"));
        assert_eq!((daemon.update, daemon.app_version.as_str()), (None, "0.7.4"), "{bad}");
    }

    let later = serde_json::json!({"phase": "rolling_back", "version": "0.7.5", "detail": "a later sotd's words", "later_field": 1});
    let res: HelloRes = serde_json::from_value(hello(later)).unwrap();
    assert_eq!(res.update, Some(state(UpdatePhase::Other, "0.7.5", "a later sotd's words")));
}

/// A string past its bound keeps its beginning and its end, with a mark where it was cut, written and read: a sentence
/// names what to run at its end and a cause chain names its fault there, so neither may lose its end, and a reader
/// must see that something is missing. A bound that falls inside a character moves off it.
#[test]
fn a_string_past_its_bound_keeps_both_ends_and_shows_the_cut() {
    let head = "install.json is missing at C:\\Users\\someone\\AppData\\Local\\sot\\install.json while bin\\sotd.exe is installed beside it";
    let run = "automatic updates stay off until `install.ps1 -Version <version>` writes it again";
    let long = format!("{head} ({}); {run}", "an installer replacing it leaves it so for an instant ".repeat(6));
    assert!(long.len() > MAX_UPDATE_DETAIL);
    let over = UpdateState { armed: "a".repeat(65), ..state(UpdatePhase::Blocked, &"v".repeat(80), &long) };
    let written = serde_json::to_value(&over).unwrap();
    let read: UpdateState = serde_json::from_value(serde_json::json!({"phase": "blocked", "version": over.version, "detail": long, "armed": over.armed})).unwrap();
    for detail in [written["detail"].as_str().unwrap(), read.detail.as_str(), over.line().as_str()] {
        assert!(detail.contains(&head[..60]) && detail.ends_with(run), "both ends are kept: {detail}");
        assert!(detail.contains(" \u{2026} "), "the cut is shown: {detail}");
    }
    assert_eq!((written["detail"].as_str().unwrap().len(), read.detail.len()), (MAX_UPDATE_DETAIL, MAX_UPDATE_DETAIL));
    assert_eq!((read.version.len(), read.armed.len(), written["version"].as_str().unwrap().len()), (MAX_UPDATE_VERSION, MAX_UPDATE_VERSION, MAX_UPDATE_VERSION));
    assert_eq!(serde_json::to_value(&read).unwrap(), written, "a bounded state is written again as it was read");
    // Two-byte characters across both cut points: the cuts move to character boundaries and stay inside the bound.
    let accented = serde_json::to_value(state(UpdatePhase::Off, "", &"\u{e9}".repeat(400))).unwrap();
    let kept = accented["detail"].as_str().unwrap();
    assert!(kept.len() <= MAX_UPDATE_DETAIL && kept.len() >= MAX_UPDATE_DETAIL - 2 && kept.contains(" \u{2026} "), "{}", kept.len());
    let short = state(UpdatePhase::Off, "0.7.4", "dev build");
    assert_eq!(serde_json::to_value(&short).unwrap(), serde_json::json!({"phase": "off", "version": "0.7.4", "detail": "dev build"}), "a string within its bound is as it was");
}

/// A reason quotes files and a child's last lines, and it is printed on a terminal of another computer: no control
/// character of it reaches a reader. Each becomes a space when the state is written, when it is read and when it is
/// worded, so a newline forges no row of a table and an escape sequence moves no cursor.
#[test]
fn no_control_character_of_a_state_reaches_a_reader() {
    let hostile = "armed over 9.9.7\nforged-host hub,daemon 9.9.9 up - up to date \u{1b}[2K\u{1b}[31mRED\r\u{9b}31m\u{7f}\u{0}; it no longer fits";
    let clean = "armed over 9.9.7 forged-host hub,daemon 9.9.9 up - up to date  [2K [31mRED  31m  ; it no longer fits";
    let told = UpdateState { armed: "0.7.3\u{1b}c".into(), ..state(UpdatePhase::NotApplied, "0.7.4\n", hostile) };
    let written = serde_json::to_value(&told).unwrap();
    assert_eq!(written, serde_json::json!({"phase": "not_applied", "version": "0.7.4 ", "detail": clean, "armed": "0.7.3 c"}));
    let read: UpdateState = serde_json::from_value(serde_json::json!({"phase": "not_applied", "version": "0.7.4\n", "detail": hostile, "armed": "0.7.3\u{1b}c"})).unwrap();
    assert_eq!(serde_json::to_value(&read).unwrap(), written);
    // The words clean a state that never crossed the wire, too.
    for text in [told.line(), told.mark().unwrap().long, told.mark().unwrap().short, read.line()] {
        assert!(!text.chars().any(char::is_control), "{text:?}");
    }
    assert_eq!(told.line(), format!("0.7.4  not applied: {clean}; 0.7.3 c is ready meanwhile"));
}

/// The words of every phase, in one table: the `sotd status` cell, and the mark's long and short forms where the phase
/// has a mark.
#[test]
fn every_phase_has_its_words_and_five_have_a_mark() {
    use UpdatePhase as P;
    let marked = |kind, long: &str, short: &str| Some(Mark { long: long.to_string(), short: short.to_string(), kind });
    let mark = |long: &str, short: &str| marked(MarkKind::Coming, long, short);
    let ready = |version: &str| marked(MarkKind::Ready, &format!("update {version} ready: quit keeping sessions, then start again"), &format!("{version} ready"));
    let warn = |long: &str, short: &str| marked(MarkKind::Warning, long, short);
    let checked = |phase, detail: &str, secs| UpdateState { checked_s: Some(secs), ..state(phase, "", detail) };
    let armed = |phase, version: &str, detail: &str, armed: &str| UpdateState { armed: armed.into(), ..state(phase, version, detail) };
    let unit = "applies when that computer's sotd.service next starts (a start of Ship of Tools there, or a restart of the service)";
    let table = [
        (state(P::Unchecked, "", ""), "not checked yet", None),
        (checked(P::None, "", 3 * 3600 + 5), "up to date (checked 3 h ago)", None),
        (checked(P::None, "", 20), "up to date (checked just now)", None),
        (checked(P::None, "", 125), "up to date (checked 2 min ago)", None),
        (checked(P::None, "", 3 * 86_400), "up to date (checked 3 d ago)", None),
        (state(P::None, "", ""), "up to date", None),
        (state(P::Found, "0.7.4", ""), "0.7.4 found", mark("update 0.7.4 found", "0.7.4 found")),
        (state(P::Found, "0.7.4", "no install record here to prepare for"), "0.7.4 found: no install record here to prepare for", mark("update 0.7.4 found", "0.7.4 found")),
        (state(P::Preparing, "0.7.4", ""), "0.7.4 preparing (can take many minutes)", mark("update 0.7.4 preparing", "0.7.4 preparing")),
        (armed(P::Preparing, "0.7.5", "", "0.7.4"), "0.7.5 preparing; 0.7.4 is ready meanwhile", mark("update 0.7.5 preparing", "0.7.5 preparing")),
        (armed(P::Preparing, "0.7.4", "", "0.7.4"), "0.7.4 preparing (can take many minutes)", mark("update 0.7.4 preparing", "0.7.4 preparing")),
        (state(P::Ready, "0.7.4", unit), "0.7.4 ready: applies when that computer's sotd.service next starts (a start of Ship of Tools there, or a restart of the service)", ready("0.7.4")),
        // A state the teller could not read from disk just now says so after its words, whatever its phase.
        (UpdateState { unread: true, ..checked(P::None, "", 3 * 3600) }, "up to date (checked 3 h ago); as last read, the disk did not answer in time", None),
        (UpdateState { unread: true, ..state(P::Ready, "0.7.4", "") }, "0.7.4 ready; as last read, the disk did not answer in time", ready("0.7.4")),
        (state(P::Ready, "0.7.4", ""), "0.7.4 ready", ready("0.7.4")),
        (state(P::NotApplied, "0.7.4", "preparing update failed: no julia"), "0.7.4 not applied: preparing update failed: no julia", warn("update 0.7.4 not applied", "0.7.4 not applied")),
        // A release not applied beside one a start applies now marks the one that is ready: an older one still armed,
        // and a newer one kept over the release this daemon was chasing. The line says both.
        (armed(P::NotApplied, "0.7.5", "staging update failed: disk full", "0.7.4"), "0.7.5 not applied: staging update failed: disk full; 0.7.4 is ready meanwhile", ready("0.7.4")),
        (armed(P::NotApplied, "0.7.4", "a newer release is already armed", "0.7.5"), "0.7.4 not applied: a newer release is already armed; 0.7.5 is ready meanwhile", ready("0.7.5")),
        (state(P::NotApplied, "", "unreadable pointer: EOF"), "an armed update cannot be read; the next start drops it (unreadable pointer: EOF)", None),
        (state(P::NotApplied, "", ""), "an armed update cannot be read; the next start drops it", None),
        (state(P::Blocked, "", "install.json cannot be read at /p/install.json"), "off: install.json cannot be read at /p/install.json", warn("updates off: install record unreadable", "updates off")),
        (state(P::Off, "", "dev build"), "off: dev build", None),
        (state(P::Off, "", "pinned at 0.7.3: automatic updates stay off for this install until a run that names no version"), "off: pinned at 0.7.3: automatic updates stay off for this install until a run that names no version", None),
        (checked(P::CheckFailed, "check unavailable: timed out", 2 * 3600), "check could not run 2 h ago: check unavailable: timed out", None),
        (state(P::CheckFailed, "", ""), "check could not run", None),
        (state(P::Other, "0.7.5", "a later sotd's words"), "a later sotd's words", None),
        (state(P::Other, "", ""), "state not known to this sotd", None),
    ];
    for (state, line, mark) in &table {
        assert_eq!(&state.line(), line, "{state:?}");
        assert_eq!(&state.mark(), mark, "{state:?}");
    }
    // No mark sends a person to a command, which would be wrong on a computer where that command says nothing of updates.
    for (state, _, _) in &table {
        for words in state.mark().iter().flat_map(|m| [&m.long, &m.short]) {
            assert!(!words.contains("sotd") && !words.contains("sot-fe") && !words.contains("see "), "{words}");
        }
    }
    let marked: std::collections::HashSet<UpdatePhase> = table.iter().filter(|(s, _, _)| s.mark().is_some()).map(|(s, _, _)| s.phase).collect();
    assert_eq!(marked, [P::Found, P::Preparing, P::Ready, P::NotApplied, P::Blocked].into_iter().collect(), "the five phases with a mark");
    // Every phase is in the table, so a phase added here without its words fails.
    let phases: std::collections::HashSet<UpdatePhase> = table.iter().map(|(s, _, _)| s.phase).collect();
    for phase in ["unchecked", "none", "found", "preparing", "ready", "not_applied", "blocked", "off", "check_failed", "other"] {
        let decoded: UpdatePhase = serde_json::from_value(serde_json::json!(phase)).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), serde_json::json!(phase), "the phase's wire name");
        assert!(phases.contains(&decoded), "{phase} has no row");
    }
}

#[test]
fn the_update_state_evt_is_declared_in_the_one_table() {
    assert_eq!(op::UPDATE_STATE, "update.state");
    assert!(op::ALL.contains(&op::UPDATE_STATE));
    // The evt's payload is the state itself, with nothing a state does not have written.
    let payload = serde_json::to_value(state(UpdatePhase::Preparing, "0.7.4", "")).unwrap();
    assert_eq!(payload, serde_json::json!({"phase": "preparing", "version": "0.7.4"}));
    let unread = serde_json::to_value(UpdateState { unread: true, ..UpdateState::of(UpdatePhase::Unchecked) }).unwrap();
    assert_eq!(unread, serde_json::json!({"phase": "unchecked", "unread": true}), "unread is written only when true");
}

/// The mark of a ready update says the way to apply it that keeps the sessions, and not that a start applies it: a
/// person who reads only of a next start closes the last window to get one, and a plain close of the last window ends
/// every session on that computer. The words fit every computer: where the sessions run elsewhere any quit keeps them.
#[test]
fn the_ready_mark_says_to_quit_keeping_sessions_and_never_only_a_next_start() {
    let kept_newer = UpdateState { armed: "0.7.5".into(), ..state(UpdatePhase::NotApplied, "0.7.4", "a newer release is already armed") };
    for (state, version) in [(state(UpdatePhase::Ready, "0.7.4", ""), "0.7.4"), (kept_newer, "0.7.5")] {
        let mark = state.mark().expect("a ready update has a mark");
        assert_eq!(mark.kind, MarkKind::Ready);
        assert_eq!(mark.long, format!("update {version} ready: quit keeping sessions, then start again"));
        assert_eq!(mark.short, format!("{version} ready"));
        for misread in ["next start", "close", "restart"] {
            assert!(!mark.long.contains(misread), "{}", mark.long);
        }
    }
}
