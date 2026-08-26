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

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("sot-capsule requires Linux in v1 (ADR 0039 — Windows lands with P3; macOS has no capsule)");
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
