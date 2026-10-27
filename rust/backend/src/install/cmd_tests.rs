use super::*;
use std::io::Write;
use std::path::PathBuf;

const FIXTURE: &str = "install::cmd::tests::command_fixture";

/// A native stand-in (the test binary itself, so on every system) in one of the fixture's modes.
pub(in crate::install) fn command(mode: &str, pulse: &Path) -> Command {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", FIXTURE, "--nocapture"]).env("SOT_BOUND_FIXTURE", mode).env("SOT_BOUND_PULSE", pulse);
    c
}

/// A native program that prints `text` exactly on stdout and exits 0 (the test binary's own harness prints lines of its
/// own): `sh` on Unix, Windows PowerShell on Windows.
fn printing(text: &str) -> Command {
    #[cfg(unix)]
    let mut c = {
        let mut c = Command::new("sh");
        c.args(["-c", "printf '%s' \"$SOT_BOUND_TEXT\""]);
        c
    };
    #[cfg(windows)]
    let mut c = {
        let mut c = Command::new("powershell.exe");
        c.args(["-NoProfile", "-NonInteractive", "-Command", "[Console]::Out.Write($env:SOT_BOUND_TEXT)"]);
        c
    };
    c.env("SOT_BOUND_TEXT", text);
    c
}

fn isolated(name: &str) -> bool {
    sot_platform::test_isolated::run_isolated_until(name, Instant::now() + Duration::from_secs(15)).unwrap()
}

#[test]
fn bounded_output_drains_both_large_streams() {
    if !isolated("install::cmd::tests::bounded_output_drains_both_large_streams") { return; }
    let d = tempfile::tempdir().unwrap();
    let ran = Steps::new().output_within(&mut command("flood", &d.path().join("pulse")), Duration::from_secs(5));
    assert!(ran.ok(), "{:?}: {}", ran.code, ran.err);
    assert!(ran.out.contains(&"a".repeat(2 * 1024 * 1024)));
    assert!(ran.err.contains(&"b".repeat(2 * 1024 * 1024)));
}

#[test]
fn bounded_output_keeps_deadline_after_leader_exit_and_stops_its_writer() {
    if !isolated("install::cmd::tests::bounded_output_keeps_deadline_after_leader_exit_and_stops_its_writer") { return; }
    let d = tempfile::tempdir().unwrap();
    let pulse = d.path().join("pulse");
    let bound = Duration::from_secs(4);
    let start = Instant::now();
    let ran = Steps::new().output_within(&mut command("leader", &pulse), bound);
    assert_eq!(ran.code, None, "a leader's success is not output completion: {}", ran.err);
    assert!(ran.err.contains("within"), "{}", ran.err);
    assert!(start.elapsed() < bound + Duration::from_secs(6));
    let before = std::fs::read(&pulse).expect("writer must have entered before the cut");
    assert!(!before.is_empty());
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(std::fs::read(&pulse).unwrap(), before, "an in-domain writer outlived cleanup");
}

#[test]
fn bounded_output_never_accepts_partial_output_before_a_timeout() {
    if !isolated("install::cmd::tests::bounded_output_never_accepts_partial_output_before_a_timeout") { return; }
    let d = tempfile::tempdir().unwrap();
    let pulse = d.path().join("pulse");
    let ran = Steps::new().output_within(&mut command("stall3", &pulse), Duration::from_secs(3));
    assert_eq!(ran.code, None, "a partial ready line must not turn a cut child into status 3");
    assert!(!ran.ok() && ran.err.contains("within"), "{}", ran.err);
    assert!(ran.out.is_empty(), "partial stdout must be discarded");
    assert!(!std::fs::read(&pulse).expect("stand-in must have printed before the cut").is_empty());
}

// Finite native stand-ins also run on Windows. PIDs printed here are witnesses only, never cleanup inputs.
#[test]
fn command_fixture() {
    let Ok(mode) = std::env::var("SOT_BOUND_FIXTURE") else { return };
    let pulse = PathBuf::from(std::env::var_os("SOT_BOUND_PULSE").unwrap());
    if mode == "flood" {
        std::io::stdout().write_all(&vec![b'a'; 2 * 1024 * 1024]).unwrap();
        std::io::stderr().write_all(&vec![b'b'; 2 * 1024 * 1024]).unwrap();
        return;
    }
    match mode.as_str() {
        "fail" => {
            eprintln!("the listing failed");
            std::process::exit(2);
        }
        _ => {}
    }
    if mode == "leader" {
        #[allow(clippy::disallowed_methods, reason = "finite fixture deliberately exits while its own child holds output; the bounded command owner owns their group/job")]
        let writer = command("writer", &pulse).spawn().unwrap();
        eprintln!("writer started: {}", writer.id());
        std::process::exit(0);
    }
    println!("partial ready line");
    std::io::stdout().flush().unwrap();
    let until = Instant::now() + Duration::from_secs(6);
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(pulse).unwrap();
    while Instant::now() < until {
        file.write_all(b".").unwrap();
        file.flush().unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    if mode == "stall3" { std::process::exit(3); }
}

/// A process listing is an answer or says why it is not one, through the real bounded runner: one that stalls past its
/// bound is cut and named so, one that exits nonzero, prints nothing or prints what is no process row is `Err` with its
/// reason, never an empty list.
#[test]
fn a_listing_with_no_answer_is_never_an_empty_list() {
    if !isolated("install::cmd::tests::a_listing_with_no_answer_is_never_an_empty_list") { return; }
    let d = tempfile::tempdir().unwrap();
    let pulse = d.path().join("pulse");
    let start = Instant::now();
    let cut = super::super::procs::listed(&mut command("stall", &pulse), Duration::from_secs(1), super::super::procs::parse_ps).unwrap_err();
    assert!(cut.contains("did not finish within 1000 ms"), "{cut}");
    assert!(start.elapsed() < Duration::from_secs(7), "the cut took {:?}", start.elapsed());
    let why = super::super::procs::listed(&mut command("fail", &pulse), Duration::from_secs(10), super::super::procs::parse_ps).unwrap_err();
    assert!(why.contains("ended with exit 2: the listing failed"), "{why}");
    for (text, says) in [("", "printed no process list"), ("not a process listing\n", "printed an unreadable process list: a row that is not `pid ppid name args`")] {
        let why = super::super::procs::listed(&mut printing(text), Duration::from_secs(20), super::super::procs::parse_ps).unwrap_err();
        assert!(why.contains(says), "{text:?}: {why}");
    }
    let missing = super::super::procs::listed(&mut Command::new("sot-no-such-listing-program"), Duration::from_secs(10), super::super::procs::parse_ps).unwrap_err();
    assert!(missing.contains("gave no answer"), "{missing}");
}

/// A listing reads only in the shape its program prints, through the real runner on every system: CIM's JSON of one
/// object or an array of objects with integer pids and string or null texts, and `ps` rows that each parse. `null`,
/// `{}`, `[null]`, a string, an object missing its pid, a text that is no string, and a `ps` row that does not parse are
/// each `Err` "unreadable" naming what was wrong; a healthy object, array and `ps` listing read.
#[test]
fn a_listing_reads_only_in_its_programs_shape() {
    if !isolated("install::cmd::tests::a_listing_reads_only_in_its_programs_shape") { return; }
    use super::super::procs::{listed, parse_cim, parse_ps, Proc};
    let printed = |text: &str, parse: fn(&str) -> Result<Vec<Proc>, String>| listed(&mut printing(text), Duration::from_secs(20), parse);
    for (text, wrong) in [
        ("null", "neither an object nor an array"),
        ("{}", "no integer ProcessId"),
        ("[null]", "an element that is no object"),
        ("\"a string\"", "neither an object nor an array"),
        (r#"{"ParentProcessId":1,"Name":"x.exe"}"#, "no integer ProcessId"),
        (r#"[{"ProcessId":1,"ParentProcessId":0},{"ProcessId":2}]"#, "no integer ParentProcessId"),
        (r#"{"ProcessId":1,"ParentProcessId":0,"Name":7}"#, "Name is no string"),
        ("not json", "not JSON"),
    ] {
        let why = printed(text, parse_cim).unwrap_err();
        assert!(why.contains("printed an unreadable process list") && why.contains(wrong), "{text}: {why}");
    }
    let one = printed(r#"{"ProcessId":5,"ParentProcessId":4,"Name":"sotd.exe","ExecutablePath":null,"CommandLine":"x"}"#, parse_cim).unwrap();
    assert_eq!((one.len(), one[0].pid, one[0].ppid, one[0].exe.as_str()), (1, 5, 4, ""));
    let two = printed(r#"[{"ProcessId":1,"ParentProcessId":0,"Name":"a.exe","ExecutablePath":"C:\\a.exe","CommandLine":"a"},{"ProcessId":2,"ParentProcessId":1,"Name":null,"ExecutablePath":null,"CommandLine":null}]"#, parse_cim).unwrap();
    assert_eq!(two.iter().map(|p| p.pid).collect::<Vec<_>>(), [1, 2]);
    for text in ["10 1 sotd /p/bin/sotd\nnot a row\n", "x 1 sotd\n", "10 1\n"] {
        let why = printed(text, parse_ps).unwrap_err();
        assert!(why.contains("printed an unreadable process list: a row that is not"), "{text:?}: {why}");
    }
    let ps = printed("  10     1 sotd     /p/bin/sotd --label sot\n  11    10 julia    /j/julia -e x\n", parse_ps).unwrap();
    assert_eq!(ps.iter().map(|p| (p.pid, p.ppid)).collect::<Vec<_>>(), [(10, 1), (11, 10)]);
}
