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

/// `run`, plus (ADR 0041 step 6 U2) `supervise`/`endrun`/`reset` — see
/// each subcommand's own function for its usage line.
#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let top_usage = "usage: sot-capsule <run|supervise|endrun|reset|build-id> ...";
    match args.first().map(String::as_str) {
        // The lane build id this binary will answer the supervisor hello
        // with -- the daemon reads it before every spawn (`sot-backend`'s
        // `check_pair`) so a sotd/sot-capsule pair from two builds is
        // refused up front instead of failing every attach as `Foreign`.
        Some("build-id") => println!("{}", sot_log::exchange::SUPERVISOR_LANE_BUILD_ID),
        Some("run") => cmd_run(&args[1..]),
        Some("supervise") => cmd_supervise(&args[1..]),
        Some("endrun") => cmd_endrun(&args[1..]),
        Some("reset") => cmd_reset(&args[1..]),
        _ => {
            eprintln!("{top_usage}");
            std::process::exit(2);
        }
    }
}

/// Temporary harness for the Windows capsule runtime (ADR 0041 steps 4-5;
/// U2 adds `--parent-lease-name`, the flag ONLY a supervisor passes). No
/// stdin-forwarding thread (the wire lane replaces it — real input/resize
/// now arrive over the pipe, from whatever attaches to it) and no
/// `--echo` (pipe fan-out is the real subscriber path). Ctrl+C still
/// simply kills this whole process when run bare (no supervisor) — FE-loss,
/// not EndRun (ADR 0041 Lifecycle).
#[cfg(windows)]
fn cmd_run(args: &[String]) {
    let usage = "usage: sot-capsule run <voyage_root> <voyage_id> [--cols <n>] [--rows <n>] \
[--parent-lease-name <name>] [--survival <normal|degraded>] [--assume-no-rollback-target] -- <cmd> [args...]";
    if args.len() < 3 {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let voyage_root = std::path::PathBuf::from(&args[0]);
    let voyage_id = args[1].clone();
    let mut rest = &args[2..];
    // Matches vt100_ctt::Parser's own Default (80x24) — a reasonable
    // harness default, not an ADR-pinned one.
    let mut cols: u16 = 80;
    let mut rows: u16 = 24;
    // Codex round-2b Blocker 4: the ONE explicit, honestly-named operator
    // override that lets this manual-testing harness run at all before
    // U4's release-apply transaction exists — see the refusal message
    // below for what it actually asserts.
    let mut assume_no_rollback_target = false;
    // ADR 0041 step 6 U2: `Some(name)` only when a supervisor spawned
    // this process — see `CapsuleWinConfig::parent_lease_name`'s own doc.
    let mut parent_lease_name: Option<String> = None;
    // ADR 0042 slice L1a (Codex review finding 7): supplied by the
    // spawner (`--start`/`--resume`'s own supervisor, via
    // `build_run_command`'s `--survival`), never inferred — defaults to
    // `Normal` for a bare manual invocation, matching every existing
    // caller of this harness that predates the flag.
    let mut survival = sot_log::wire::Survival::Normal;
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
            Some("--parent-lease-name") if rest.len() > 1 => {
                parent_lease_name = Some(rest[1].clone());
                rest = &rest[2..];
            }
            Some("--survival") if rest.len() > 1 => {
                survival = match rest[1].as_str() {
                    "normal" => sot_log::wire::Survival::Normal,
                    "degraded" => sot_log::wire::Survival::Degraded,
                    _ => {
                        eprintln!("{usage}");
                        std::process::exit(2);
                    }
                };
                rest = &rest[2..];
            }
            Some("--assume-no-rollback-target") => {
                assume_no_rollback_target = true;
                rest = &rest[1..];
            }
            _ => break,
        }
    }
    if rest.first().map(String::as_str) != Some("--") || rest.len() < 2 {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let argv: Vec<String> = rest[1..].to_vec();

    // ADR 0041 "Upgrade and version skew" reader-first rollout gate
    // (Codex round-2b Blocker 4 discharge, superseding round-1 Major 9's
    // hardcoded default): this manual-testing harness has no supervisor/
    // release-apply transaction (U4) and therefore NO REAL evidence to
    // construct. The honest pre-U4 posture is FAIL CLOSED, naming U4 as
    // the reason — hardcoding `NoRollbackTarget` fabricated evidence and
    // recreated exactly the "missing means first install" default-through
    // Major 9 was supposed to remove, only under a typed name. Absent the
    // explicit override, this binary refuses before ever constructing a
    // config or opening a segment. `sot-capsule supervise` (U2) is in the
    // exact same "no real evidence" position and passes the SAME flag
    // down to every leg it spawns — see that subcommand's own doc.
    if !assume_no_rollback_target {
        eprintln!(
            "sot-capsule: no rollout evidence available -- this binary cannot open a \
             feature-bearing segment until U4's release-apply transaction supplies real \
             evidence (ADR 0041 \"Upgrade and version skew\"). Pass \
             --assume-no-rollback-target to override for manual testing -- that flag \
             ASSERTS, without proof, that there is no installed rollback target to \
             protect; a real supervisor must never pass it, and must construct real \
             evidence from its own transaction instead."
        );
        std::process::exit(2);
    }
    let rollout_evidence = sot_log::rollout::RolloutEvidence::NoRollbackTarget;

    let config = sot_log::capsule_win::CapsuleWinConfig {
        voyage_root,
        voyage_id,
        retention: sot_log::segment::RetentionClass::Archive,
        producer_kind: "raw-terminal-windows".into(),
        argv,
        cols,
        rows,
        // ADR 0042 slice L1a: supplied by `--survival` (a real spawner —
        // `build_run_command` — now sets it); a bare manual invocation
        // still defaults to the honest `Normal`.
        survival,
        rollout_evidence,
        parent_lease_name,
    };
    // No command source yet (Ctrl+C kills the process instead — see the
    // doc above). The pipe IS real now (U3 round 2): `PipeTransport::bind`
    // (called by `run` itself, at the pipe-lifetime invariant's exact
    // point) creates `\\.\pipe\sot-voyage-<voyage_id>` for real
    // attach/mgmt clients to connect to.
    let (_cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let mut transport = sot_log::pipe_transport::PipeTransport::new(MAX_PIPE_INSTANCES);
    match sot_log::capsule_win::run(config, cmd_rx, &mut transport) {
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

/// `sot-capsule supervise <state_dir> <--start|--resume> [--cols <n>] \
/// [--rows <n>] --assume-no-rollback-target -- <cmd> [args...]` (ADR
/// 0041 step 6 U2): the authority. `--assume-no-rollback-target` is
/// mandatory here for the exact reason `run`'s own copy of it is — see
/// `sot_log::supervisor`'s own module doc.
#[cfg(windows)]
fn cmd_supervise(args: &[String]) {
    let usage = "usage: sot-capsule supervise <state_dir> <--start|--resume> [--cols <n>] \
[--rows <n>] [--survival <normal|degraded>] --assume-no-rollback-target -- <cmd> [args...]";
    if args.len() < 3 {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let state_dir = std::path::PathBuf::from(&args[0]);
    let mode = match args[1].as_str() {
        "--start" => sot_log::supervisor::StartMode::Start,
        "--resume" => sot_log::supervisor::StartMode::Resume,
        _ => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    };
    let mut rest = &args[2..];
    let mut cols: u16 = 80;
    let mut rows: u16 = 24;
    let mut assume_no_rollback_target = false;
    // ADR 0042 slice L1a (Codex review finding 7): the spawner's own
    // breakaway outcome, threaded into every leg via `SuperviseConfig`
    // (see that field's own doc) — defaults to `Normal` for a bare
    // manual invocation, matching every existing caller of this CLI that
    // predates the flag.
    let mut survival = sot_log::wire::Survival::Normal;
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
            Some("--survival") if rest.len() > 1 => {
                survival = match rest[1].as_str() {
                    "normal" => sot_log::wire::Survival::Normal,
                    "degraded" => sot_log::wire::Survival::Degraded,
                    _ => {
                        eprintln!("{usage}");
                        std::process::exit(2);
                    }
                };
                rest = &rest[2..];
            }
            Some("--assume-no-rollback-target") => {
                assume_no_rollback_target = true;
                rest = &rest[1..];
            }
            _ => break,
        }
    }
    if rest.first().map(String::as_str) != Some("--") || rest.len() < 2 {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let producer_argv: Vec<String> = rest[1..].to_vec();
    let config = sot_log::supervisor::SuperviseConfig {
        state_dir,
        mode,
        producer_argv,
        cols,
        rows,
        assume_no_rollback_target,
        survival,
    };
    std::process::exit(sot_log::supervisor::supervise(config));
}

/// `sot-capsule endrun <state_dir> [--voyage <id>] [--reason <text>]`
/// (ADR 0041 step 6 U2): the no-supervisor path's own fence-acquiring
/// EndRun. Fails loudly (never terminates blind) if a real supervisor is
/// already the authority — see `sot_log::supervisor::endrun`'s own doc.
#[cfg(windows)]
fn cmd_endrun(args: &[String]) {
    let usage = "usage: sot-capsule endrun <state_dir> [--voyage <id>] [--reason <text>]";
    if args.is_empty() {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let state_dir = std::path::PathBuf::from(&args[0]);
    let mut rest = &args[1..];
    let mut voyage: Option<String> = None;
    let mut reason = String::new();
    loop {
        match rest.first().map(String::as_str) {
            Some("--voyage") if rest.len() > 1 => {
                voyage = Some(rest[1].clone());
                rest = &rest[2..];
            }
            Some("--reason") if rest.len() > 1 => {
                reason = rest[1].clone();
                rest = &rest[2..];
            }
            _ => break,
        }
    }
    if !rest.is_empty() {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    std::process::exit(sot_log::supervisor::endrun(&state_dir, voyage, reason));
}

/// `sot-capsule reset <state_dir> [--voyage <id>]` (ADR 0041 step 6 U2):
/// the no-supervisor path's own fence-acquiring reset — proceeds ONLY on
/// a classifier ABSENT taken while holding the fence.
#[cfg(windows)]
fn cmd_reset(args: &[String]) {
    let usage = "usage: sot-capsule reset <state_dir> [--voyage <id>]";
    if args.is_empty() {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let state_dir = std::path::PathBuf::from(&args[0]);
    let mut rest = &args[1..];
    let mut voyage: Option<String> = None;
    loop {
        match rest.first().map(String::as_str) {
            Some("--voyage") if rest.len() > 1 => {
                voyage = Some(rest[1].clone());
                rest = &rest[2..];
            }
            _ => break,
        }
    }
    if !rest.is_empty() {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    std::process::exit(sot_log::supervisor::reset(&state_dir, voyage));
}

#[cfg(not(any(target_os = "linux", windows)))]
fn main() {
    eprintln!("sot-capsule requires Linux or Windows (ADR 0039 P1 Linux; ADR 0041 P3 Windows; macOS has no capsule)");
    std::process::exit(2);
}

// ADR 0041 U0 round-1 blocker 3: `sot_log::fence::lock_supervisor` must be
// reachable from THIS binary crate -- Cargo treats `src/bin/sot-capsule.rs`
// as a SEPARATE crate from the package's own library even though they share
// one Cargo.toml, so a `pub fn` hidden inside a private library module (the
// ORIGINAL `fsutil::lock_supervisor`) was invisible here. This test is the
// actual proof: it calls the public facade from the real consumer Codex
// named, not merely from the library's own test suite.
#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn supervisor_lock_facade_is_reachable_and_works_from_this_binary_crate() {
        let dir = tempfile::tempdir().unwrap();
        let guard = sot_log::fence::lock_supervisor(dir.path()).unwrap();
        assert!(sot_log::fence::supervisor_lock_path(dir.path()).is_file());
        drop(guard);
    }
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
