use super::*;
use std::time::Instant;

const FIXTURE: &str = "install::procs::tests::listing_fixture";

/// A native stand-in for the listing program (the test binary itself, so it runs on every system).
fn listing(mode: &str) -> Command {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", FIXTURE, "--nocapture"]).env("SOT_LISTING_FIXTURE", mode);
    c
}

fn procs(list: &str) -> Vec<Proc> {
    parse_ps(list)
}

#[test]
fn a_listing_that_stalls_ends_at_its_bound_as_an_error_never_an_empty_list() {
    let start = Instant::now();
    let got = list(&mut listing("stall"), Duration::from_secs(2), parse_ps);
    let why = got.expect_err("a cut listing is not a list");
    assert!(why.contains("no answer") && why.contains("did not finish within"), "{why}");
    assert!(start.elapsed() < Duration::from_secs(10), "the cut took {:?}", start.elapsed());
}

#[test]
fn a_listing_that_ends_badly_or_reads_as_nothing_is_an_error() {
    let ended = list(&mut listing("good_then_exit_7"), Duration::from_secs(20), parse_ps).expect_err("its lines are discarded with its exit");
    assert!(ended.contains("exit 7"), "{ended}");
    let none = list(&mut listing("noise"), Duration::from_secs(20), parse_ps).expect_err("no process in the text is no list");
    assert!(none.contains("printed no process list"), "{none}");
    let missing = list(&mut Command::new("sot-no-such-listing-program"), Duration::from_secs(20), parse_ps).expect_err("a program that did not start lists nothing");
    assert!(missing.contains("no answer"), "{missing}");
}

#[test]
fn a_listing_that_ends_well_is_read() {
    let all = list(&mut listing("good"), Duration::from_secs(20), parse_ps).expect("a finished listing");
    assert!(all.iter().any(|p| p.pid == 4242 && p.ppid == 1 && p.name == "sotd"), "{all:?}");
}

#[test]
fn a_failed_listing_never_says_no_daemon_runs() {
    let prefix = Path::new("/p");
    let seen = Ok(procs("10 1 sotd /p/bin/sotd --label sot\n11 10 julia /j/julia -e x\n"));
    assert!(daemon_may_run(&seen, prefix));
    assert!(!daemon_may_run(&Ok(procs("12 1 bash bash -c /p/bin/sotd\n")), prefix), "a listing that shows none says so");
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
        "good" | "good_then_exit_7" => {
            println!("\n4242 1 sotd /p/bin/sotd --label sot");
            std::io::stdout().flush().unwrap();
            if mode == "good_then_exit_7" {
                std::process::exit(7);
            }
        }
        _ => {}
    }
}
