//! `sot-capsule run <voyage_root> <voyage_id> [--no-echo] -- <cmd> [args...]`
//!
//! Runs one producer on a PTY under a capsule, recording its voyage
//! (ADR 0037/0039). Like `script(1)`, but the record is a Ship's Log voyage:
//! output you see on stdout has already been fsynced (the visibility
//! watermark), input is recorded redacted by default, and the voyage
//! verifies with `sot-log verify` afterward.

#[cfg(unix)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: sot-capsule run <voyage_root> <voyage_id> [--no-echo] -- <cmd> [args...]";
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

#[cfg(not(unix))]
fn main() {
    eprintln!("sot-capsule requires unix in v1 (ADR 0039 — Windows lands with P3)");
    std::process::exit(2);
}
