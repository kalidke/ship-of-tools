use super::*;
use super::super::supervisor;
#[cfg(unix)]
use super::super::unix;

#[cfg(unix)]
fn work(rows: &[&str], pluto: bool, unsure: Option<&str>) -> Work {
    Work { rows: rows.iter().map(|r| (*r).to_string()).collect(), pluto, unsure: unsure.map(String::from) }
}

/// The Unix daemon question, run with stand-ins for the daemon, its children and the console: what it decided, and
/// whether it asked.
#[cfg(unix)]
fn ask(q: &unix::Question, live: Work, kids: Option<Vec<&'static str>>, answer: bool) -> (Result<(Work, bool), Stop>, bool) {
    let asked = std::cell::RefCell::new(false);
    let asks = Asks {
        live: &|| live.clone(),
        kids: &|| kids.clone(),
        ask: &|_| {
            *asked.borrow_mut() = true;
            answer
        },
        note: &|_| {},
    };
    let got = unix::question(q, &asks);
    (got, asked.into_inner())
}

#[cfg(unix)]
fn q(before: State, unit_runs: bool, restart_flag: bool) -> unix::Question<'static> {
    unix::Question { before, unit_runs, retire_stops: false, restart_flag, socket: "/s.sock", why: "no answer" }
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

/// A live daemon is asked before it ends. What it runs is its own answer: a daemon that runs and cannot be told its
/// version is never read as none; its live work is its REPL rows and its Pluto server, and an answer it could not give
/// in full says why; one that cannot say is judged by its own children, known by what they run; the question and the
/// steps name the work in one set of words. Unix: a unit this run would restart, on another version with a live REPL,
/// stops the run unless the console says yes or the person gave --restart-daemon; one a window holds, or one no unit
/// runs, is not restarted here and is not asked; a retire asks before it stops an active unit, whatever its version.
/// Windows: an update asks before any daemon it may stop or restart; it restarts one only through the supervisor's
/// converge, whose wait ends by the log or its bound and takes the sentinel, and otherwise leaves the update to the
/// person or applies it at the desk.
#[test]
fn a_live_daemon_is_asked_before_it_ends() {
    assert_eq!(state_of(Some(0), true), State::Current);
    assert_eq!(state_of(Some(3), false), State::Other);
    assert_eq!(state_of(Some(4), true), State::Held);
    for code in [Some(1), Some(2), None] {
        assert_eq!(state_of(code, true), State::Unknown);
        assert_eq!(state_of(code, false), State::None);
    }

    let w = work_of("repl alpha\nrepl beta\npluto server\n", "", Some(3));
    assert_eq!(w, Work { rows: vec!["alpha".into(), "beta".into()], pluto: true, unsure: None });
    assert_eq!(work_of("", "", Some(0)), Work::default());
    let w = work_of("repl alpha\n", "sotd update repls: the daemon's reply says nothing of its Pluto server\n", Some(1));
    assert_eq!((w.rows.len(), w.unsure.as_deref()), (1, Some("sotd update repls: the daemon's reply says nothing of its Pluto server")));
    assert_eq!(work_of("", "", None).unsure.as_deref(), Some("sotd update repls gave no reason (exit none)"));

    assert_eq!(child_kind("/j/julia --project=/c/julia/repl -e using ShipToolsRepl; ShipToolsRepl.run_shim()"), Some("repl"));
    assert_eq!(child_kind("/j/julia --project=/c/julia/pluto /c/julia/pluto/start.jl 1234"), Some("pluto"));
    assert_eq!(child_kind("\"C:\\j\\julia.exe\" \"C:\\c\\julia\\pluto\\start.jl\""), Some("pluto"));
    assert_eq!(child_kind("/j/julia --project=/c/julia/kernel -e serve()"), None);

    let rows = vec!["alpha".to_string()];
    assert_eq!(work_words(&rows, true, "restarting it would end"), "restarting it would end the REPLs of: alpha; the Pluto server is running: restarting the daemon ends every notebook open in it");
    assert_eq!(work_words(&[], false, "x"), "");
    assert_eq!(ask_words(&rows, false, false, false), "the REPLs of: alpha");
    assert_eq!(ask_words(&[], true, true, true), "a live REPL (the daemon could not name its row); the Pluto server and every notebook open in it, and any other the daemon could not list");
    assert_eq!(ask_words(&[], false, false, true), "any REPL or Pluto notebook that is live");
    assert_eq!(rows_note(&[], false, true), "its live REPLs and Pluto server could not be listed, so starting it again may end some");
    assert!(rows_note(&rows, false, true).ends_with("; it could not say the rest, so starting it again may end more"));

    #[cfg(unix)]
    {
        // A restart of the unit.
        let repl = work(&["alpha"], false, None);
        let (got, asked) = ask(&q(State::Other, true, false), repl.clone(), None, false);
        let stop = got.unwrap_err();
        assert!(asked && stop.code == 2 && stop.detail.contains("restarting it would end the REPLs of: alpha"), "{stop:?}");
        let (got, asked) = ask(&q(State::Other, true, false), repl.clone(), None, true);
        assert!(asked && got.is_ok());
        let (got, asked) = ask(&q(State::Other, true, true), repl.clone(), None, false);
        assert!(!asked && got.is_ok(), "--restart-daemon answers it");
        for (before, unit) in [(State::Held, true), (State::Other, false)] {
            let (got, asked) = ask(&q(before, unit, false), repl.clone(), None, false);
            assert!(!asked && got.unwrap().0.rows == ["alpha"], "{before:?} {unit}: the step names the rows");
        }
        let (got, asked) = ask(&q(State::Other, true, false), work(&[], false, None), None, false);
        assert!(!asked && got.is_ok(), "nothing live is nothing to ask");

        // The retire of a unit the role no longer runs here. Its daemon runs (the unit is active) even when the install's
        // own processes do not show it (another prefix's, taken over): one that cannot be told is never read as none.
        let dir = tempfile::tempdir().unwrap();
        let mute = dir.path().join("sotd");
        sot_log::test_exec::write_executable(&mute, "#!/bin/sh\necho 'sotd same-version: no hello within 5 s' >&2\nexit 1\n");
        assert_eq!(unix::discover(&mute, "unix:/s.sock", false, true, false, false).0, State::Unknown, "an active unit the retire stops runs a daemon");
        assert_eq!(unix::discover(&mute, "unix:/s.sock", false, false, false, true).0, State::None, "a role with no daemon here and nothing to retire asks nothing");
        let retire = |before| unix::Question { before, unit_runs: false, retire_stops: true, restart_flag: false, socket: "/s.sock", why: "no answer" };
        for before in [State::Current, State::Other, State::Held] {
            let (got, asked) = ask(&retire(before), work(&["alpha"], false, None), None, false);
            let stop = got.unwrap_err();
            assert!(asked && stop.code == 2 && stop.detail.contains("stopping it would end the REPLs of: alpha"), "{before:?}: {stop:?}");
        }
        let (got, asked) = ask(&retire(State::Current), work(&[], false, None), None, false);
        assert!(!asked && got.is_ok(), "nothing live: the retire goes on");
        let (got, asked) = ask(&unix::Question { restart_flag: true, ..retire(State::Current) }, work(&["alpha"], false, None), None, false);
        assert!(!asked && got.is_ok(), "--restart-daemon answers it");
        let (got, _) = ask(&retire(State::Unknown), work(&[], false, None), Some(vec!["repl"]), false);
        assert_eq!(got.unwrap_err().code, 2, "a daemon that cannot be told, with a REPL child, stops the run");

        // A daemon that cannot be told its version.
        let (got, _) = ask(&q(State::Unknown, false, true), work(&[], false, None), Some(vec![]), true);
        assert_eq!(got.unwrap_err().code, 2, "no unit to restart: nothing answers it");
        let (got, asked) = ask(&q(State::Unknown, true, false), work(&[], false, None), Some(vec![]), false);
        assert!(!asked && got.is_ok(), "no REPL or Pluto child: the restart ends nothing live");
        let (got, asked) = ask(&q(State::Unknown, true, false), work(&[], false, None), Some(vec!["repl"]), false);
        assert!(asked && got.unwrap_err().code == 2);
        let (got, asked) = ask(&q(State::Unknown, true, false), work(&[], false, None), None, true);
        assert!(asked && got.is_ok());

        // A daemon that cannot say all of its work.
        let unsure = work(&[], false, Some("no pluto field"));
        let (got, asked) = ask(&q(State::Other, true, false), unsure.clone(), Some(vec![]), false);
        assert!(!asked && !got.unwrap().1, "no REPL or Pluto child: nothing is unknown");
        let (got, asked) = ask(&q(State::Other, true, false), unsure.clone(), Some(vec!["pluto"]), false);
        let stop = got.unwrap_err();
        assert!(asked && stop.detail.starts_with("could not ask the daemon on /s.sock in full: it runs another version; no pluto field"), "{stop:?}");
        let (got, _) = ask(&q(State::Held, true, false), unsure, Some(vec!["repl"]), false);
        assert!(got.unwrap().1, "a daemon left running says its work is unknown in the step");
    }

    // Windows: the update's question.
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

    // Windows: which way the update applies.
    assert_eq!(supervisor::way(Some("AAA-BBB"), false, &|| Some(0)), supervisor::Way::Converge);
    assert_eq!(supervisor::way(Some(""), false, &|| Some(1)), supervisor::Way::Quit);
    assert_eq!(supervisor::way(None, true, &|| Some(0)), supervisor::Way::Quit);
    assert_eq!(supervisor::way(None, false, &|| Some(0)), supervisor::Way::Desk);
    assert_eq!(supervisor::way(None, false, &|| Some(2)), supervisor::Way::Here);
    assert_eq!(supervisor::way(None, false, &|| None), supervisor::Way::Here, "a session that cannot be read is not taken for ssh");

    // Windows: the converge's wait.
    let dir = tempfile::tempdir().unwrap();
    let (log, sentinel) = (dir.path().join("supervisor.log"), dir.path().join("relaunch.request"));
    let old = "sot-apply: APPLIED v1 (previous kept)\nstatus: DONE\n";
    let wait = |bound: Duration| {
        std::fs::write(&sentinel, "converge").unwrap();
        let t0 = Instant::now();
        (supervisor::wait_converge(&log, old.len() as u64, &sentinel, "v1", bound, &|_| {}), t0.elapsed())
    };
    std::fs::write(&log, old).unwrap();
    let (r, took) = wait(Duration::from_secs(1));
    assert!(r.unwrap_err().contains("did not report v1 applied within 1 s"));
    assert!(took >= Duration::from_secs(1) && took < Duration::from_millis(1500), "{took:?}");
    assert!(!sentinel.exists());

    std::fs::write(&log, old).unwrap();
    let writer = {
        let log = log.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let mut text = std::fs::read_to_string(&log).unwrap();
            text.push_str("sot-apply: APPLIED v1 (previous kept)\nstatus: Starting\nstatus: DONE\n");
            std::fs::write(&log, text).unwrap();
        })
    };
    let (r, took) = wait(Duration::from_secs(20));
    writer.join().unwrap();
    assert!(r.is_ok() && took < Duration::from_secs(5), "{r:?} {took:?}");
    assert!(!sentinel.exists());

    std::fs::write(&log, format!("{old}sot-apply: installing sotd failed - restoring previous binaries and pointers\n")).unwrap();
    let (r, _) = wait(Duration::from_secs(20));
    assert!(r.unwrap_err().contains("did not complete"));
    assert!(!sentinel.exists());

    // The daemon and its children, from the process table.
    {
        let all = procs::parse_ps("  10     1 sotd     /p/bin/sotd --project-root /h --label sot\n  11    10 julia    /j/julia -e x\nnot a line\n");
        assert_eq!(all.len(), 2);
        assert_eq!((all[0].pid, all[0].ppid, all[0].name.as_str(), all[0].exe.as_str()), (10, 1, "sotd", "/p/bin/sotd"));
        assert_eq!(all[1].args, "/j/julia -e x");
    }

    {
        let one = procs::parse_cim(r#"{"ProcessId":5,"ParentProcessId":4,"Name":"sotd.exe","ExecutablePath":"C:\\p\\bin\\sotd.exe","CommandLine":"x"}"#);
        assert_eq!((one.len(), one[0].pid, one[0].exe.as_str()), (1, 5, "C:\\p\\bin\\sotd.exe"));
        assert_eq!(procs::parse_cim(r#"[{"ProcessId":1},{"ProcessId":2,"ExecutablePath":null}]"#).len(), 2);
        assert!(procs::parse_cim("not json").is_empty());
    }

    // Unix paths: a Windows program path is never one.
    #[cfg(unix)]
    {
        let all = procs::parse_ps("10 1 sotd /p/bin/sotd --label sot\n11 1 sotd /q/bin/sotd\n12 1 bash bash -c /p/bin/sotd\n13 1 sot /p/bin/sot --socket s\n");
        assert_eq!(procs::daemon_pids(&all, Path::new("/p")), [10]);
        assert!(procs::window_runs(&all, Path::new("/p")) && !procs::window_runs(&all, Path::new("/q")));
    }

    // Windows: the supervisor's log.
    {
        assert_eq!(supervisor::converge_state("", "v1"), supervisor::Converge::Waiting);
        assert_eq!(supervisor::converge_state("status: DONE\nsot-apply: APPLIED v1 (previous kept)\n", "v1"), supervisor::Converge::Waiting);
        assert_eq!(supervisor::converge_state("sot-apply: APPLIED v1 (previous kept)\nstatus: Starting\nstatus: DONE\n", "v1"), supervisor::Converge::Done);
        assert_eq!(supervisor::converge_state("sot-apply: APPLIED v0 (x)\nstatus: DONE\n", "v1"), supervisor::Converge::Waiting);
        assert_eq!(supervisor::converge_state("sot-apply: installing sotd failed - restoring previous binaries and pointers\n", "v1"), supervisor::Converge::Restored);
        let log = "t  pid=7  supervisor start (relaunched=False) depth=0 ws=90MB code=AAA-BBB\nt  pid=9  supervisor start (relaunched=False) depth=0 ws=90MB code=CCC\nt  pid=7  supervisor start (relaunched=True) depth=1 ws=91MB code=DDD\n";
        assert_eq!(supervisor::start_code(log, 7), "DDD");
        assert_eq!(supervisor::start_code(log, 9), "CCC");
        assert_eq!(supervisor::start_code("t  pid=7  supervisor start (relaunched=False) depth=0 ws=90MB code=unknown\n", 7), "");
        assert_eq!(supervisor::start_code("t  pid=77  supervisor start code=X\n", 7), "");
    }

}

/// Bounded waits: a wait for the daemon keeps its bound by the clock: a daemon whose endpoint accepts and never answers
/// costs the bound, not the bound plus an ask's own reply bound, no ask starts after the deadline, and the last ask's
/// reason comes back with its state, so the check that reports it asks nothing more.
#[cfg(unix)]
#[test]
fn a_wait_for_the_daemon_keeps_its_bound() {
    let dir = tempfile::tempdir().unwrap();
    let sotd = dir.path().join("sotd");
    sot_log::test_exec::write_executable(&sotd, "#!/bin/sh\nsleep 10\nexit 3\n");
    let t0 = Instant::now();
    let (st, why) = wait_current(&sotd, "unix:/nowhere.sock", Duration::from_secs(1), false, true);
    assert!(t0.elapsed() < Duration::from_millis(1800), "the wait took {:?}", t0.elapsed());
    assert_eq!(st, State::Unknown, "a daemon that runs and did not answer in time is not taken for none");
    assert!(why.contains("did not finish within"), "{why}");
}
