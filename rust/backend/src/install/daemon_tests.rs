use super::*;

/// A daemon that runs and cannot be told its version is never read as none.
#[test]
fn a_daemon_that_cannot_be_told_is_never_none() {
    assert_eq!(state_of(Some(0), true), State::Current);
    assert_eq!(state_of(Some(3), false), State::Other);
    assert_eq!(state_of(Some(4), true), State::Held);
    for code in [Some(1), Some(2), None] {
        assert_eq!(state_of(code, true), State::Unknown);
        assert_eq!(state_of(code, false), State::None);
    }
}

/// The live work is the daemon's own answer: its REPL rows and its Pluto server, and an answer it could not give in
/// full says why, whatever rows it did give.
#[test]
fn the_live_work_is_what_the_daemon_said_and_what_it_could_not() {
    let w = work_of("repl alpha\nrepl beta\npluto server\n", "", Some(3));
    assert_eq!(w, Work { rows: vec!["alpha".into(), "beta".into()], pluto: true, unsure: None });
    assert_eq!(work_of("", "", Some(0)), Work::default());
    let w = work_of("repl alpha\n", "sotd update repls: the daemon's reply says nothing of its Pluto server\n", Some(1));
    assert_eq!((w.rows.len(), w.unsure.as_deref()), (1, Some("sotd update repls: the daemon's reply says nothing of its Pluto server")));
    assert_eq!(work_of("", "", None).unsure.as_deref(), Some("sotd update repls gave no reason (exit none)"));
}

/// A daemon that cannot say is judged by its own children: the REPL shim and the Pluto server, by their command lines.
#[test]
fn a_daemons_children_are_known_by_what_they_run() {
    assert_eq!(child_kind("/j/julia --project=/c/julia/repl -e using ShipToolsRepl; ShipToolsRepl.run_shim()"), Some("repl"));
    assert_eq!(child_kind("/j/julia --project=/c/julia/pluto /c/julia/pluto/start.jl 1234"), Some("pluto"));
    assert_eq!(child_kind("\"C:\\j\\julia.exe\" \"C:\\c\\julia\\pluto\\start.jl\""), Some("pluto"));
    assert_eq!(child_kind("/j/julia --project=/c/julia/kernel -e serve()"), None);
}

/// The question and the steps name the work in one set of words.
#[test]
fn the_question_and_the_steps_name_the_work() {
    let rows = vec!["alpha".to_string()];
    assert_eq!(work_words(&rows, true, "restarting it would end"), "restarting it would end the REPLs of: alpha; the Pluto server is running: restarting the daemon ends every notebook open in it");
    assert_eq!(work_words(&[], false, "x"), "");
    assert_eq!(ask_words(&rows, false, false, false), "the REPLs of: alpha");
    assert_eq!(ask_words(&[], true, true, true), "a live REPL (the daemon could not name its row); the Pluto server and every notebook open in it, and any other the daemon could not list");
    assert_eq!(ask_words(&[], false, false, true), "any REPL or Pluto notebook that is live");
    assert_eq!(rows_note(&[], false, true), "its live REPLs and Pluto server could not be listed, so starting it again may end some");
    assert!(rows_note(&rows, false, true).ends_with("; it could not say the rest, so starting it again may end more"));
}

/// A wait for the daemon keeps its bound by the clock: a daemon whose endpoint accepts and never answers costs the
/// bound, not the bound plus an ask's own reply bound, and no ask starts after the deadline.
#[cfg(unix)]
#[test]
fn a_wait_for_the_daemon_keeps_its_bound() {
    let dir = tempfile::tempdir().unwrap();
    let sotd = dir.path().join("sotd");
    sot_log::test_exec::write_executable(&sotd, "#!/bin/sh\nsleep 10\nexit 3\n");
    let t0 = Instant::now();
    let st = wait_current(&sotd, "unix:/nowhere.sock", Duration::from_secs(1), false, true);
    assert!(t0.elapsed() < Duration::from_millis(1800), "the wait took {:?}", t0.elapsed());
    assert_eq!(st, State::Unknown, "a daemon that runs and did not answer in time is not taken for none");
}

/// The Windows update's question, run with stand-ins for the daemon, its children and the console: what it decided,
/// and what it asked.
fn ask_update(state: State, live: Work, kids: Option<Vec<&'static str>>, flag: bool, answer: bool) -> (Result<String, Stop>, Option<String>) {
    let asked = std::cell::RefCell::new(None);
    let asks = Asks {
        live: &|| live.clone(),
        kids: &|| kids.clone(),
        ask: &|words| {
            *asked.borrow_mut() = Some(words.to_string());
            answer
        },
        note: &|_| {},
    };
    let got = ask_before_update(state, "P", "no answer", flag, &asks);
    (got, asked.into_inner())
}

/// A Windows update asks before any daemon it may stop or restart, whatever its version: live work stops the run unless
/// the console says yes or the person gave -RestartDaemon; nothing live, or no daemon, goes on unasked; a daemon that
/// cannot be told or cannot say is judged by its own children.
#[test]
fn a_windows_update_asks_before_it_restarts_a_live_daemon() {
    let repl = Work { rows: vec!["a".into()], ..Work::default() };
    let (got, asked) = ask_update(State::Current, repl.clone(), None, false, false);
    assert_eq!(got.unwrap_err().code, 2);
    assert_eq!(asked.as_deref(), Some("the REPLs of: a"));
    let (got, _) = ask_update(State::Current, repl.clone(), None, false, true);
    assert_eq!(got.unwrap(), "the daemon on P is running, and restarting it would end the REPLs of: a");
    let (got, asked) = ask_update(State::Other, repl, None, true, false);
    assert_eq!((got.unwrap().as_str(), asked), ("the daemon on P runs another version, and restarting it would end the REPLs of: a", None));
    for state in [State::None, State::Other] {
        let (got, asked) = ask_update(state, Work::default(), None, false, false);
        assert_eq!((got.unwrap(), asked), (String::new(), None), "{state:?}");
    }
    let (got, asked) = ask_update(State::Unknown, Work::default(), Some(vec![]), false, false);
    assert_eq!((got.unwrap(), asked), (String::new(), None), "no REPL or Pluto child: nothing live");
    let (got, asked) = ask_update(State::Unknown, Work::default(), None, false, false);
    assert!(got.unwrap_err().detail.contains("could not be told its version (no answer)"));
    assert_eq!(asked.as_deref(), Some("any REPL or Pluto notebook that is live"));
    let unsure = Work { unsure: Some("timed out".into()), ..Work::default() };
    let (got, asked) = ask_update(State::Other, unsure, Some(vec!["repl"]), false, false);
    assert!(got.unwrap_err().detail.contains("could not ask the daemon on P in full"));
    assert_eq!(asked.as_deref(), Some("a live REPL (the daemon could not name its row), and any other the daemon could not list"));
}
