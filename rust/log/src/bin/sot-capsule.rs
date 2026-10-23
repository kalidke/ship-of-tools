//! `sot-capsule supervise <state_dir> <--start|--resume> [--cols <n>] [--rows <n>] [--first-leg-only <token>]...
//! [--claim-fd <n> --takeover-fd <n>] -- <cmd> [args...]`: the one process of a row (`sot_log::supervisor`). It runs
//! the agent on a real terminal and serves the row until the agent ends or a `stop` arrives. `--survival` and
//! `--assume-no-rollback-target` are accepted and ignored.

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // The lane build id this binary answers the supervisor hello with: a diagnostic, since the lane gate is the
        // protocol integer.
        Some("build-id") => println!("{}", sot_log::BUILD_ID),
        Some("supervise") => cmd_supervise(&args[1..]),
        _ => {
            eprintln!("usage: sot-capsule <supervise|build-id> ...");
            std::process::exit(2);
        }
    }
}

/// A descriptor number flag's value, or the usage and exit 2.
fn fd_flag(value: &str, usage: &str) -> i32 {
    value.parse().unwrap_or_else(|_| {
        eprintln!("{usage}");
        std::process::exit(2);
    })
}

/// The claim flags of `supervise`: both or neither (usage and exit 2 otherwise), and Unix only.
#[cfg(unix)]
fn birth_from_flags(
    claim_fd: Option<i32>,
    takeover_fd: Option<i32>,
    usage: &str,
) -> Option<sot_log::supervisor::birth_claim::InheritedBirth> {
    if claim_fd.is_some() != takeover_fd.is_some() {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    claim_fd.zip(takeover_fd).map(|(claim_fd, takeover_fd)| {
        sot_log::supervisor::birth_claim::InheritedBirth {
            claim_fd,
            takeover_fd,
        }
    })
}

#[cfg(not(unix))]
fn birth_from_flags(claim_fd: Option<i32>, takeover_fd: Option<i32>, usage: &str) -> Option<()> {
    if claim_fd.is_some() || takeover_fd.is_some() {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    None
}

/// `sot-capsule supervise`: the argv grammar (the daemon's detach builds it; `--survival` and
/// `--assume-no-rollback-target` are accepted and ignored). `--first-leg-only` is repeatable and agent-agnostic: this
/// binary knows nothing about `claude` or `--resume`, only that the caller wants a given token on the first run and on
/// none after; see `SuperviseConfig::first_leg_only`.
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn cmd_supervise(args: &[String]) {
    let usage = "usage: sot-capsule supervise <state_dir> <--start|--resume> [--cols <n>] \
[--rows <n>] [--survival <normal|degraded>] [--first-leg-only <token>]... \
[--claim-fd <n> --takeover-fd <n>] --assume-no-rollback-target -- <cmd> [args...]";
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
    let mut first_leg_only: Vec<String> = Vec::new();
    // The claim a spawning parent took on the fence for this birth, and the channel to answer it on (Unix).
    let mut claim_fd: Option<i32> = None;
    let mut takeover_fd: Option<i32> = None;
    loop {
        match rest.first().map(String::as_str) {
            Some("--claim-fd") if rest.len() > 1 => {
                claim_fd = Some(fd_flag(&rest[1], usage));
                rest = &rest[2..];
            }
            Some("--takeover-fd") if rest.len() > 1 => {
                takeover_fd = Some(fd_flag(&rest[1], usage));
                rest = &rest[2..];
            }
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
            // Accepted and ignored: the spawner's breakaway outcome is not reported by this supervisor.
            Some("--survival") if rest.len() > 1 => {
                rest = &rest[2..];
            }
            Some("--first-leg-only") if rest.len() > 1 => {
                first_leg_only.push(rest[1].clone());
                rest = &rest[2..];
            }
            // Accepted and ignored: the rollout gate it answered is gone.
            Some("--assume-no-rollback-target") => {
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
    let birth = birth_from_flags(claim_fd, takeover_fd, usage);
    let config = sot_log::supervisor::SuperviseConfig {
        state_dir,
        mode,
        producer_argv,
        cols,
        rows,
        first_leg_only,
        #[cfg(unix)]
        birth,
    };
    #[cfg(not(unix))]
    let _ = birth;
    std::process::exit(sot_log::supervisor::supervise(config));
}

