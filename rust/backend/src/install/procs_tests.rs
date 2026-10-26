use super::*;
use std::time::Instant;

const FIXTURE: &str = "install::procs::tests::listing_fixture";

/// A native stand-in for the listing program (the test binary itself, so it runs on every system). Its rows carry a
/// `LIST ` lead, since the test harness prints its own lines on the same stream.
fn listing(mode: &str) -> Command {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", FIXTURE, "--nocapture"]).env("SOT_LISTING_FIXTURE", mode);
    c
}

/// The stand-in's rows read as `ps` lines: `None` when one of them is not.
fn fixture_rows(text: &str) -> Option<Vec<Proc>> {
    let rows: Vec<&str> = text.lines().filter_map(|l| l.split_once("LIST ").map(|(_, row)| row)).collect();
    parse_ps(&rows.join("\n"))
}

fn proc_of(prefix: &Path, bin: &str, pid: u32, ppid: u32) -> Proc {
    let name = super::super::pack::exe_name(bin);
    Proc { pid, ppid, exe: prefix.join("bin").join(&name).to_string_lossy().into_owned(), name, args: String::new() }
}

#[test]
fn a_listing_that_stalls_ends_at_its_bound_as_an_error_never_an_empty_list() {
    let start = Instant::now();
    let got = list(&mut listing("stall"), Duration::from_secs(2), fixture_rows);
    let why = got.expect_err("a cut listing is not a list");
    assert!(why.contains("no answer") && why.contains("did not finish within"), "{why}");
    assert!(start.elapsed() < Duration::from_secs(10), "the cut took {:?}", start.elapsed());
}

#[test]
fn a_listing_that_ends_badly_or_reads_as_nothing_is_an_error() {
    let ended = list(&mut listing("good_then_exit_7"), Duration::from_secs(20), fixture_rows).expect_err("its rows are discarded with its exit");
    assert!(ended.contains("exit 7"), "{ended}");
    let none = list(&mut listing("noise"), Duration::from_secs(20), fixture_rows).expect_err("no process in the text is no list");
    assert!(none.contains("no valid process list"), "{none}");
    let torn = list(&mut listing("good_then_garbage"), Duration::from_secs(20), fixture_rows).expect_err("a row that does not read makes the listing invalid, not shorter");
    assert!(torn.contains("no valid process list"), "{torn}");
    let missing = list(&mut Command::new("sot-no-such-listing-program"), Duration::from_secs(20), fixture_rows).expect_err("a program that did not start lists nothing");
    assert!(missing.contains("no answer"), "{missing}");
}

#[test]
fn a_listing_that_ends_well_is_read() {
    let all = list(&mut listing("good"), Duration::from_secs(20), fixture_rows).expect("a finished listing");
    assert!(all.iter().any(|p| p.pid == 4242 && p.ppid == 1 && p.name == "sotd"), "{all:?}");
}

#[test]
fn a_listing_must_be_structurally_one() {
    assert_eq!(parse_ps("10 1 sotd /p/bin/sotd\n\n11 10 julia\n").map(|all| all.len()), Some(2));
    assert!(parse_ps("10 1 sotd /p/bin/sotd\nnot a line\n").is_none(), "a row that does not read is not a shorter list");
    assert!(parse_ps("10 1\n").is_none());
    let one = parse_cim(r#"{"ProcessId":5,"ParentProcessId":4,"Name":"sotd.exe","ExecutablePath":"C:\\p\\bin\\sotd.exe","CommandLine":"x"}"#).unwrap();
    assert_eq!((one.len(), one[0].pid, one[0].exe.as_str()), (1, 5, "C:\\p\\bin\\sotd.exe"));
    assert_eq!(parse_cim(r#"[{"ProcessId":1,"ParentProcessId":0},{"ProcessId":2,"ParentProcessId":1,"ExecutablePath":null}]"#).map(|all| all.len()), Some(2));
    for bad in ["not json", "{}", "null", r#""text""#, "[{}]", "[1]", r#"[{"ProcessId":1}]"#, r#"{"ProcessId":"1","ParentProcessId":0}"#] {
        assert!(parse_cim(bad).is_none(), "{bad} is not a process listing");
    }
}

#[test]
fn a_failed_listing_never_says_no_daemon_runs() {
    let dir = tempfile::tempdir().unwrap();
    let prefix = dir.path();
    let seen = Ok(vec![proc_of(prefix, "sotd", 10, 1), Proc { pid: 11, ppid: 10, name: "julia".into(), exe: "julia".into(), args: "julia -e x".into() }]);
    assert!(daemon_may_run(&seen, prefix));
    assert!(!daemon_may_run(&Ok(vec![proc_of(Path::new("/elsewhere"), "sotd", 12, 1)]), prefix), "a listing that shows none says so");
    assert!(daemon_may_run(&Err("cut".into()), prefix), "a listing that could not be read presumes one");
    assert_eq!(super::super::daemon::children_in(&seen, prefix), Some(vec![]), "its children are those the listing shows");
    assert_eq!(super::super::daemon::children_in(&Err("cut".into()), prefix), None, "nothing can be ruled out");
}

// Finite stand-ins for the listing program; they run on every system.
#[test]
fn listing_fixture() {
    use std::io::Write as _;
    let Ok(mode) = std::env::var("SOT_LISTING_FIXTURE") else { return };
    match mode.as_str() {
        "stall" => std::thread::sleep(Duration::from_secs(30)),
        "good" | "good_then_exit_7" | "good_then_garbage" => {
            println!("\nLIST 4242 1 sotd /p/bin/sotd --label sot");
            if mode == "good_then_garbage" {
                println!("LIST not a row");
            }
            std::io::stdout().flush().unwrap();
            if mode == "good_then_exit_7" {
                std::process::exit(7);
            }
        }
        _ => {}
    }
}
