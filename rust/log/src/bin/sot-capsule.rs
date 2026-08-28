//! `sot-capsule run <voyage_root> <voyage_id> [--no-echo] -- <cmd> [args...]`
//!
//! Runs one producer on a PTY under a capsule, recording its voyage
//! (ADR 0037/0039). Like `script(1)`, but the record is a Ship's Log voyage:
//! output you see on stdout has already been fsynced (the visibility
//! watermark), input is recorded redacted by default, and the voyage
//! verifies with `sot-log verify` afterward.

#[cfg(target_os = "linux")]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: sot-capsule run <voyage_root> <voyage_id> [--no-echo] -- <cmd> [args...]\n       sot-capsule claude <voyage_root> <voyage_id> <helper-main.js> <expected-sdk-version>";
    if args.first().map(String::as_str) == Some("claude") {
        return run_claude(&args[1..], usage);
    }
    if args.len() < 5 || args[0] != "run" {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let voyage_root = std::path::PathBuf::from(&args[1]);
    let voyage_id = args[2].clone();
    let mut rest = &args[3..];
    let mut echo = true;
    if rest.first().map(String::as_str) == Some("--no-echo") {
        echo = false;
        rest = &rest[1..];
    }
    if rest.first().map(String::as_str) != Some("--") || rest.len() < 2 {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let argv: Vec<String> = rest[1..].to_vec();

    let config = sot_log::capsule::CapsuleConfig {
        voyage_root,
        voyage_id,
        retention: sot_log::segment::RetentionClass::Archive,
        producer_kind: "raw-terminal".into(),
        argv,
        echo,
    };
    match sot_log::capsule::run(config) {
        Ok(s) => {
            eprintln!(
                "sot-capsule: producer exited {:?}; {} frames, {} segments sealed",
                s.exit_code, s.frames_written, s.segments_sealed
            );
            std::process::exit(s.exit_code.unwrap_or(1));
        }
        Err(e) => {
            eprintln!("sot-capsule: {e}");
            std::process::exit(1);
        }
    }
}

/// The RAW total simultaneous pipe-instance ceiling this harness passes to
/// `PipeTransport::new` (ADR 0041: subscribers plus separately bounded
/// pre-hello/mgmt connections — the exact combined figure is a real
/// budget-table computation step 6/7's supervisor owns; this bin is a
/// manual-testing harness with no supervisor yet, so it states a single
/// generous constant rather than inventing that computation here).
#[cfg(windows)]
const MAX_PIPE_INSTANCES: u32 = 8;

/// Temporary harness for the Windows capsule runtime (ADR 0041 steps 4-5,
/// U2/U3). No stdin-forwarding thread (the wire lane replaces it — real
/// input/resize now arrive over the pipe, from whatever attaches to it)
/// and no `--echo` (pipe fan-out is the real subscriber path; a bare
/// stdout mirror duplicated that for no one). Ctrl+C still simply kills
/// this whole process — FE-loss, not EndRun (ADR 0041 Lifecycle) — until a
/// real supervisor exists.
#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: sot-capsule run <voyage_root> <voyage_id> [--cols <n>] [--rows <n>] -- <cmd> [args...]";
    if args.len() < 4 || args[0] != "run" {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let voyage_root = std::path::PathBuf::from(&args[1]);
    let voyage_id = args[2].clone();
    let mut rest = &args[3..];
    // Matches vt100_ctt::Parser's own Default (80x24) — a reasonable
    // harness default, not an ADR-pinned one.
    let mut cols: u16 = 80;
    let mut rows: u16 = 24;
    loop {
        match rest.first().map(String::as_str) {
            Some("--cols") if rest.len() > 1 => {
                cols = rest[1].parse().unwrap_or_else(|_| {
                    eprintln!("{usage}");
                    std::process::exit(2);
                });
                rest = &rest[2..];
            }
            Some("--rows") if rest.len() > 1 => {
                rows = rest[1].parse().unwrap_or_else(|_| {
                    eprintln!("{usage}");
                    std::process::exit(2);
                });
                rest = &rest[2..];
            }
            _ => break,
        }
    }
    if rest.first().map(String::as_str) != Some("--") || rest.len() < 2 {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let argv: Vec<String> = rest[1..].to_vec();

    let config = sot_log::capsule_win::CapsuleWinConfig {
        voyage_root,
        voyage_id,
        retention: sot_log::segment::RetentionClass::Archive,
        producer_kind: "raw-terminal-windows".into(),
        argv,
        cols,
        rows,
        // Step 6's breakaway attempt is the real source (ADR 0041 decision
        // 11) — this bin has none yet, so it states the honest default.
        survival: sot_log::wire::Survival::Normal,
    };
    // No command source yet (Ctrl+C kills the process instead — see the
    // doc above). The pipe IS real now (U3 round 2): `PipeTransport::bind`
    // (called by `run` itself, at the pipe-lifetime invariant's exact
    // point) creates `\\.\pipe\sot-voyage-<voyage_id>` for real
    // attach/mgmt clients to connect to.
    let (_cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (mut transport, transport_rx) = sot_log::pipe_transport::PipeTransport::new(MAX_PIPE_INSTANCES);
    match sot_log::capsule_win::run(config, cmd_rx, transport_rx, &mut transport) {
        Ok(s) => {
            eprintln!(
                "sot-capsule: producer exited {:?} ({:?}); {} frames, {} segments sealed \
                 (handshake_answered={}, handshake_suppressed={}, resize_os_calls={})",
                s.exit_code,
                s.exit_kind,
                s.frames_written,
                s.segments_sealed,
                s.handshake_answered,
                s.handshake_suppressed_matches,
                s.resize_os_calls
            );
            // Reinterpretation to a process exit code happens ONLY here,
            // at the actual OS process-exit boundary — everywhere else in
            // this crate the value stays a raw, unsigned DWORD (review
            // finding: an earlier version cast it to i32 well before this
            // point, which would have turned a high-bit NTSTATUS-shaped
            // code negative for no reason).
            std::process::exit(s.exit_code.map(|c| c as i32).unwrap_or(1));
        }
        Err(e) => {
            eprintln!("sot-capsule: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn main() {
    eprintln!("sot-capsule requires Linux or Windows (ADR 0039 P1 Linux; ADR 0041 P3 Windows; macOS has no capsule)");
    std::process::exit(2);
}

/// The ADR 0040 producer: operator commands ride stdin as JSON lines
/// ({"turn": text} | {"interrupt": true} | {"shutdown": true}).
#[cfg(target_os = "linux")]
fn run_claude(args: &[String], usage: &str) {
    use sot_log::claude::{run, ClaudeConfig, Fence, OperatorCmd};
    if args.len() != 4 {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let fence = match Fence::discover(&format!("{}", std::process::id())) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("sot-capsule: {e}");
            std::process::exit(1);
        }
    };
    let config = ClaudeConfig {
        voyage_root: std::path::PathBuf::from(&args[0]),
        voyage_id: args[1].clone(),
        retention: sot_log::segment::RetentionClass::Archive,
        helper_argv: vec!["node".into(), args[2].clone()],
        expected_sdk_version: args[3].clone(),
        fence,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            let cmd = if let Some(t) = v.get("turn").and_then(|t| t.as_str()) {
                OperatorCmd::Turn(t.to_string())
            } else if v.get("interrupt").is_some() {
                OperatorCmd::Interrupt
            } else if v.get("shutdown").is_some() {
                OperatorCmd::Shutdown
            } else {
                continue;
            };
            if tx.send(cmd).is_err() {
                break;
            }
        }
    });
    match run(config, rx) {
        Ok(s) => {
            eprintln!(
                "sot-capsule claude: {} turn(s), {} frames, terminal: {}",
                s.turns, s.frames_written, s.terminal_reason
            );
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("sot-capsule claude: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn run_claude(_args: &[String], _usage: &str) {
    eprintln!("the claude adapter requires Linux in v1 (ADR 0040 kill domain)");
    std::process::exit(2);
}
