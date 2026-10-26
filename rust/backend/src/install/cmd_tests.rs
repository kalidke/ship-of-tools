use super::*;
use std::io::Write;
use std::path::PathBuf;

const FIXTURE: &str = "install::cmd::tests::command_fixture";

/// A native stand-in (the test binary itself) that prints a partial line and then stalls for seconds, for the decisions
/// that read an observation: bound it to a second or two.
pub(in crate::install) fn command(mode: &str, pulse: &Path) -> Command {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", FIXTURE, "--nocapture"]).env("SOT_BOUND_FIXTURE", mode).env("SOT_BOUND_PULSE", pulse);
    c
}

fn isolated(name: &str) -> bool {
    sot_platform::test_isolated::run_isolated_until(name, Instant::now() + Duration::from_secs(15)).unwrap()
}

#[test]
fn bounded_output_drains_both_large_streams() {
    if !isolated("install::cmd::tests::bounded_output_drains_both_large_streams") { return; }
    let d = tempfile::tempdir().unwrap();
    let ran = Steps::new().output_within(&mut command("flood", &d.path().join("pulse")), Duration::from_secs(5)).expect("a finished program");
    assert_eq!(ran.exit, 0, "{}", ran.err);
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
    let why = Steps::new().output_within(&mut command("leader", &pulse), bound).expect_err("a leader's success is not output completion");
    assert_eq!(why, Why::Cut(bound));
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
    let why = Steps::new().output_within(&mut command("stall3", &pulse), Duration::from_secs(3)).expect_err("a partial ready line must not turn a cut child into status 3");
    assert_eq!(why, Why::Cut(Duration::from_secs(3)), "a cut program has not answered, and its partial stdout is discarded");
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
