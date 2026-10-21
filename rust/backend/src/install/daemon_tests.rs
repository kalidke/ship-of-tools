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
