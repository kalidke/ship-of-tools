//! Checked request ordering at the terminal boundary, main completion, and the documented terminal lexical pin.

use super::child_signal::{ContainedStd, Signal};
use super::{contain, shutdown};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(unix)]
use super::start_tests::unix::Watched;
#[cfg(windows)]
use super::start_tests::windows::Watched;

const FIXTURE: &str = "lifecycle::exit_tests::contained_fixture";
const ROLE: &str = "SOT_TEST_L2_EXIT_ROLE";

#[test]
fn contained_fixture() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    sot_log::test_isolated::enter(FIXTURE);
    let ready = PathBuf::from(std::env::var_os("SOT_TEST_L2_READY").expect("fixture ready path"));
    if role == "leader" {
        let (mut command, entered) = sot_log::test_isolated::test_command(FIXTURE);
        let mut descendant = command
            .env(ROLE, "descendant")
            .spawn()
            .expect("fixture descendant");
        wait_file(&ready);
        entered.assert_once(descendant.id());
        std::fs::write(
            ready.with_extension("leader"),
            std::process::id().to_string(),
        )
        .unwrap();
        let _ = descendant.wait();
    } else {
        assert_eq!(role, "descendant", "unexpected fixture role");
        std::fs::write(&ready, std::process::id().to_string()).unwrap();
        if let Some(cleanup) = std::env::var_os("SOT_TEST_L2_FIXTURE_CLEANUP") {
            let cleanup = PathBuf::from(cleanup);
            while !cleanup.exists() {
                std::thread::sleep(Duration::from_millis(20));
            }
        } else {
            std::thread::sleep(Duration::from_secs(120));
        }
    }
}

pub(crate) fn wait_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while std::fs::read_to_string(path).map_or(true, |s| s.is_empty()) {
        assert!(
            Instant::now() < deadline,
            "fixture readiness did not arrive: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) struct ReadyTree {
    pub(crate) child: ContainedStd,
    leader: Watched,
    descendant: Watched,
    _dir: tempfile::TempDir,
}

impl ReadyTree {
    pub(crate) fn start(signal: &'static Signal) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let (mut command, entered) = sot_log::test_isolated::test_command(FIXTURE);
        command.env(ROLE, "leader").env("SOT_TEST_L2_READY", &ready);
        let child = signal.spawn_std(&mut command).expect("contained fixture");
        wait_file(&ready.with_extension("leader"));
        entered.assert_once(child.id());
        let pid: u32 = std::fs::read_to_string(&ready)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        #[cfg(unix)]
        assert_eq!(
            unsafe { libc::getpgid(pid as i32) },
            child.id() as i32,
            "fixture group membership"
        );
        Self {
            leader: Watched::open(child.id()),
            descendant: Watched::open(pid),
            child,
            _dir: dir,
        }
    }

    pub(crate) fn descendant_pid(&self) -> u32 {
        std::fs::read_to_string(self._dir.path().join("ready"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    pub(crate) fn finish(mut self) {
        // Retain the owner until after the callback; its cleanup cannot substitute for terminal fire.
        self.child
            .kill()
            .expect("owned fixture cleanup and direct-child reap");
        assert!(self.leader.dead(), "terminal fixture leader did not die");
        assert!(
            self.descendant.dead(),
            "terminal fixture descendant did not die"
        );
    }
}

fn requests() -> Vec<&'static str> {
    contain::REQUEST_EVENTS.with(|events| events.borrow().clone())
}

fn clear_requests() {
    contain::REQUEST_EVENTS.with(|events| events.borrow_mut().clear());
}

fn expected_requests() -> &'static [&'static str] {
    #[cfg(unix)]
    {
        &["group", "leader"]
    }
    #[cfg(windows)]
    {
        &["job"]
    }
}

#[test]
fn update_exit_fires_before_termination() {
    let signal = Box::leak(Box::new(Signal::new()));
    let tree = ReadyTree::start(signal);
    let leases = super::lease::Leases::new(Some("boot".into()), None, None, false);
    clear_requests();
    let mut observation = None;
    crate::update::exit_for_update(&leases, |code| {
        shutdown::terminal(signal, code, |code| {
            observation = Some((code, signal.is_fired(), requests()))
        });
    });
    let before_cleanup = observation.unwrap();
    tree.finish();
    assert_eq!(before_cleanup.0, 75);
    assert!(
        before_cleanup.1,
        "update terminal callback preceded permanent fire"
    );
    assert_eq!(
        before_cleanup.2,
        expected_requests(),
        "update terminal callback preceded checked requests"
    );
}

#[test]
fn update_exit_yields_without_firing_or_terminating() {
    let signal = Box::leak(Box::new(Signal::new()));
    let leases = super::lease::Leases::new(Some("boot".into()), None, None, false);
    leases.begin_close();
    let mut called = false;
    crate::update::exit_for_update(&leases, |code| {
        shutdown::terminal(signal, code, |_| called = true)
    });
    assert!(
        !called && !signal.is_fired(),
        "a losing update fired or terminated"
    );
}

#[test]
fn every_terminal_code_checks_requests_first() {
    for code in [0, 1, 2, 75, 78, 101] {
        let signal = Box::leak(Box::new(Signal::new()));
        let tree = ReadyTree::start(signal);
        clear_requests();
        let before_cleanup =
            shutdown::terminal(signal, code, |c| (c, signal.is_fired(), requests()));
        tree.finish();
        assert_eq!(before_cleanup.0, code);
        assert!(
            before_cleanup.1,
            "terminal code {code} preceded permanent fire"
        );
        assert_eq!(
            before_cleanup.2,
            expected_requests(),
            "terminal code {code} preceded checked requests"
        );
        clear_requests();
        shutdown::terminal(signal, code, |_| ());
        assert!(
            requests().is_empty(),
            "idempotent fire signalled a reaped identity"
        );
    }
}

#[test]
fn request_failure_is_diagnosed_before_the_unchanged_code() {
    let signal = Box::leak(Box::new(Signal::new()));
    let tree = ReadyTree::start(signal);
    let log = sot_log::test_log::capture();
    contain::REQUEST_FAILURE.with(|failure| failure.set(if cfg!(unix) { 3 } else { 4 }));
    let (code, diagnostic) = shutdown::terminal(signal, 78, |code| (code, log.text()));
    contain::REQUEST_FAILURE.with(|failure| failure.set(0));
    tree.finish();
    assert_eq!(code, 78);
    assert!(
        diagnostic.contains("terminal child fire failed"),
        "terminal callback preceded cleanup diagnostic: {diagnostic}"
    );
    assert!(
        diagnostic.contains(if cfg!(unix) {
            "injected group request failure"
        } else {
            "injected job request failure"
        }),
        "concrete request reason missing: {diagnostic}"
    );
}

#[test]
fn main_completion_while_the_runtime_and_child_are_alive() {
    let name = "lifecycle::exit_tests::main_completion_while_the_runtime_and_child_are_alive";
    if !sot_log::test_isolated::run_isolated(name) {
        return;
    }
    #[cfg(unix)]
    super::child_signal::reset_child_signal();
    for (role, expected) in [("success", 0), ("error", 1), ("unwind", 101)] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let signal = Box::leak(Box::new(Signal::new()));
        let mut tree = ReadyTree::start(signal);
        clear_requests();
        let observed = crate::complete_main(
            &runtime,
            async {
                assert!(!tree.child.exited(false).unwrap());
                match role {
                    "success" => Ok(()),
                    "error" => anyhow::bail!("injected main returned error"),
                    _ => panic!("injected main future unwind"),
                }
            },
            |code| {
                shutdown::terminal(signal, code, |code| {
                    let runtime_alive = runtime.block_on(async {
                        tokio::task::yield_now().await;
                        true
                    });
                    (code, signal.is_fired(), requests(), runtime_alive)
                })
            },
        );
        tree.finish();
        assert_eq!(observed.0, expected);
        assert!(observed.3, "runtime dropped before terminal callback");
        assert!(
            observed.1,
            "main {role} terminal callback preceded permanent fire"
        );
        assert_eq!(
            observed.2,
            expected_requests(),
            "main {role} terminal callback preceded checked requests"
        );
    }
}

/// Tokenize enough Rust to ignore string/comment contents while retaining names, qualifications and aliases.
fn code_tokens(source: &str) -> Vec<(usize, String)> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
        } else if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'r' && matches!(bytes.get(i + 1), Some(b'#' | b'"')) {
            i += 1;
            let from = i;
            while bytes.get(i) == Some(&b'#') {
                i += 1;
            }
            let hashes = i - from;
            if bytes.get(i) == Some(&b'"') {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'"'
                        && bytes
                            .get(i + 1..i + 1 + hashes)
                            .is_some_and(|s| s.iter().all(|b| *b == b'#'))
                    {
                        i += 1 + hashes;
                        break;
                    }
                    i += 1;
                }
            }
        } else if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' {
            let at = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push((at, source[at..i].to_string()));
        } else if bytes[i..].starts_with(b"::") {
            out.push((i, "::".into()));
            i += 2;
        } else {
            out.push((i, (bytes[i] as char).to_string()));
            i += 1;
        }
    }
    out
}

fn raw_terminals(path: &str, source: &str) -> (usize, Vec<String>) {
    let tokens = code_tokens(source);
    let mut raw = 0;
    let mut forbidden = Vec::new();
    if source.contains("std::process::*") || source.contains("libc::*") {
        forbidden.push(format!("{path}: terminal namespace glob import"));
    }
    for (i, (at, name)) in tokens.iter().enumerate() {
        if !["exit", "abort", "_exit", "TerminateProcess", "ExitProcess"].contains(&name.as_str()) {
            continue;
        }
        let prev = i.checked_sub(1).map(|p| tokens[p].1.as_str());
        if name == "abort" && prev == Some(".") {
            continue;
        }
        if name == "exit"
            && (prev == Some("fn")
                || (prev == Some("::") && i >= 2 && tokens[i - 2].1 == "shutdown"))
        {
            continue;
        }
        let allowed = path == "rust/backend/src/lifecycle/shutdown.rs"
            && sot_log::test_scan::enclosing(source, *at) == "exit"
            && i >= 4
            && tokens[i - 4].1 == "std"
            && tokens[i - 2].1 == "process"
            && tokens.get(i + 1).is_some_and(|t| t.1 == "(");
        if allowed {
            raw += 1;
        } else {
            forbidden.push(format!(
                "{path}:{}: {name} in {} prev {:?}",
                source[..*at].lines().count(),
                sot_log::test_scan::enclosing(source, *at),
                prev
            ));
        }
    }
    (raw, forbidden)
}

/// Pins terminal-name tokens (except fn exit, .abort and shutdown::exit), two namespace globs, one raw exit and main-boundary text.
/// It misses namespace aliases named shutdown, renamed primitives whose imports lack those tokens, and other termination mechanisms.
#[test]
fn terminal_spellings_and_main_boundary_are_pinned() {
    let mut raw = 0;
    let mut forbidden = Vec::new();
    for (path, source) in sot_log::test_scan::production_sources() {
        if !path.starts_with("rust/backend/src/") {
            continue;
        }
        let (count, hits) = raw_terminals(&path, &source);
        raw += count;
        forbidden.extend(hits);
    }
    assert!(
        forbidden.is_empty(),
        "backend exits bypass terminal fire: {}",
        forbidden.join(", ")
    );
    assert_eq!(
        raw, 1,
        "the backend must have exactly one raw terminal primitive"
    );
    let main = sot_log::test_scan::without_test_modules(include_str!("../main.rs"));
    assert!(
        main.contains("complete_main(&runtime, daemon_main()"),
        "main bypasses the owned runtime completion boundary"
    );
}

/// Checks the listed qualified, unqualified and primitive-import examples against the terminal lexical pin.
/// It does not resolve namespace aliases, including std::process imported as shutdown, or discover other termination mechanisms.
#[test]
fn terminal_pin_rejects_the_listed_primitive_spellings() {
    for source in [
        "std::process::exit(1);",
        "use std::process::*;",
        "use std::process::exit as finish; finish(1);",
        "use std::process as p; p::abort();",
        "_exit(1);",
        "libc::_exit(1);",
        "use native::{TerminateProcess as stop};",
        "ExitProcess(1);",
        "exit(1);",
        "abort();",
    ] {
        assert!(
            !raw_terminals("rust/backend/src/new.rs", source)
                .1
                .is_empty(),
            "inventory missed {source}"
        );
    }
}
