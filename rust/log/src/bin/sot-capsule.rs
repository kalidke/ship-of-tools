//! `sot-capsule run <voyage_root> <voyage_id> [--screen <path>] [--cols <n>] [--rows <n>]
//! [--parent-lease-name <name> | --parent-lease-fd <n>] [--survival
//! <normal|degraded>] [--assume-no-rollback-target] -- <cmd> [args...]`
//!
//! Runs one producer on a real terminal under a capsule (ADR 0043 "Decisions for LU2": ONE `cmd_run` drives both
//! platforms' own producer + transport, below). The leg keeps no record of the terminal: its latest screen goes to
//! the `--screen` file, and what it tells its supervisor about its ending goes to `<voyage_root>/ended`.

/// The RAW total simultaneous connection ceiling this harness passes to
/// each platform's own transport constructor (ADR 0041: subscribers plus
/// separately bounded pre-hello/mgmt connections — the exact combined
/// figure is a real budget-table computation a real supervisor owns; this
/// bin is a manual-testing harness with no supervisor yet, so it states a
/// single generous constant rather than inventing that computation here).
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
const MAX_TRANSPORT_CONNECTIONS: u32 = 8;

/// `run`, plus (ADR 0041 step 6 U2) `supervise`/`endrun`/`reset` — see
/// each subcommand's own function for its usage line. A supervisor spawns its legs through the `run` arm.
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let top_usage = "usage: sot-capsule <run|supervise|endrun|reset|build-id> ...";
    match args.first().map(String::as_str) {
        // The lane build id this binary will answer the supervisor hello
        // with -- a diagnostic only since ADR 0045 decision 7 (the lane
        // gate is the protocol integer, not this value).
        Some("build-id") => println!("{}", sot_log::lane::exchange::SUPERVISOR_LANE_BUILD_ID),
        #[cfg(windows)]
        Some("run") => cmd_run::<sot_log::capsule::producer::conpty::producer::ConptyProducer, _, _>(&args[1..], |n| {
            sot_log::lane::platform_transport::PlatformTransport::new(n)
        }),
        #[cfg(unix)]
        Some("run") => cmd_run::<sot_log::capsule::producer::pty::PtyProducer, _, _>(&args[1..], |n| {
            sot_log::lane::platform_transport::PlatformTransport::new(n)
        }),
        Some("supervise") => cmd_supervise(&args[1..]),
        Some("endrun") => cmd_endrun(&args[1..]),
        Some("reset") => cmd_reset(&args[1..]),
        _ => {
            eprintln!("{top_usage}");
            std::process::exit(2);
        }
    }
}

/// One `run` arm over both platforms' own producer and transport (ADR
/// 0043 "Decisions for LU2" LU2b): `P` is the platform's own
/// [`sot_log::capsule::producer::Producer`] (`ConptyProducer` on Windows,
/// `PtyProducer` on Linux), `make_transport` builds its
/// [`sot_log::lane::transport::Transport`] from the connection ceiling above
/// (`PlatformTransport::new`). Everything else — the
/// flag grammar, the rollout-evidence gate, the exit-code mapping — is
/// genuinely one shared implementation now; only the lease flag's own
/// name and `--survival`'s own meaning still differ per platform, both
/// enforced right here. No stdin-forwarding thread (the wire lane
/// replaces it — real input/resize now arrive over the transport, from
/// whatever attaches to it) and no `--echo` (fan-out is the real
/// subscriber path). Ctrl+C still simply kills this whole process when
/// run bare (no supervisor) — FE-loss, not EndRun (ADR 0041 Lifecycle).
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[allow(clippy::too_many_lines, reason = "the capsule's run command: builds the transport and runs the producer; predates the 100-line limit")]
fn cmd_run<P, T, F>(args: &[String], make_transport: F)
where
    P: sot_log::capsule::producer::Producer,
    T: sot_log::lane::transport::Transport,
    F: FnOnce(u32) -> T,
{
    let usage = "usage: sot-capsule run <voyage_root> <voyage_id> [--screen <path>] [--cols <n>] [--rows <n>] \
[--parent-lease-name <name> | --parent-lease-fd <n>] [--survival <normal|degraded>] \
[--assume-no-rollback-target] -- <cmd> [args...]";
    if args.len() < 3 {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let voyage_root = std::path::PathBuf::from(&args[0]);
    // ADR 0043 decision 25: this leg's stderr is now whatever its parent
    // (`sot-capsule supervise`, spawned by the daemon) is writing to —
    // the daemon's own shared `sotd.log` when supervised, inherited
    // straight through with no redirection of its own. A tag naming the
    // voyage root is the ONLY identifying context this process alone can
    // add to a line in a file every OTHER workspace's own leg writes into
    // too; `run_note` issues each diagnostic as ONE complete write, same
    // reasoning as `supervisor/mod.rs`'s own `note` (a piecewise `eprintln!`
    // has no atomicity guarantee once two processes append to the same
    // `O_APPEND` file).
    let run_note_prefix = format!(
        "sot-capsule run[{}]",
        voyage_root.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
    );
    let run_note = |args: std::fmt::Arguments<'_>| {
        let line = format!("{run_note_prefix}: {args}\n");
        let _ = std::io::Write::write_all(&mut std::io::stderr(), line.as_bytes());
    };
    let voyage_id = args[1].clone();
    let mut rest = &args[2..];
    // Matches vt100_ctt::Parser's own Default (80x24) — a reasonable
    // harness default, not an ADR-pinned one.
    let mut cols: u16 = 80;
    let mut rows: u16 = 24;
    // Where the leg keeps its screen; none unless a supervisor (or a caller) names one.
    let mut screen_path: Option<std::path::PathBuf> = None;
    // ADR 0041 step 6 U2 / ADR 0043 decision 15: `Some(_)` only when a
    // supervisor spawned this process — see
    // `capsule::CapsuleConfig::parent_lease`'s own doc. Held here as the
    // raw CLI value; wrapped into the per-platform `ParentLease` variant
    // below. Exactly one of the two is ever meaningful on a given
    // platform (checked below) — the OTHER flag is a loud refusal, never
    // silently ignored.
    #[cfg(windows)]
    let mut parent_lease_name: Option<String> = None;
    #[cfg(unix)]
    let mut parent_lease_fd: Option<std::os::fd::RawFd> = None;
    // ADR 0042 slice L1a: supplied by the spawner (`--start`/`--resume`'s
    // own supervisor, via `build_run_command`'s `--survival`), defaulting
    // to `Normal` for a bare manual invocation. ADR 0043 decision 32:
    // honored verbatim on both platforms now -- no clamp -- the value itself is all downstream
    // code ever needs.
    let mut survival = sot_log::lane::wire::Survival::Normal;
    loop {
        match rest.first().map(String::as_str) {
            Some("--screen") if rest.len() > 1 => {
                screen_path = Some(std::path::PathBuf::from(&rest[1]));
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
            Some("--parent-lease-name") if rest.len() > 1 => {
                // The two inner blocks are individually exhaustive per
                // platform (never both present at once): the diverging
                // "wrong platform" arm ends in `process::exit`, so
                // nothing follows it in the SAME match arm on that
                // build -- the one that DOES continue advances `rest`
                // itself, rather than a shared trailing statement no
                // platform's own diverging branch could ever reach.
                #[cfg(windows)]
                {
                    parent_lease_name = Some(rest[1].clone());
                    rest = &rest[2..];
                }
                #[cfg(unix)]
                {
                    eprintln!(
                        "sot-capsule: --parent-lease-name is Windows-only; this platform's \
                         supervisor passes --parent-lease-fd <n> instead (ADR 0043 decision 15)"
                    );
                    std::process::exit(2);
                }
            }
            Some("--parent-lease-fd") if rest.len() > 1 => {
                #[cfg(unix)]
                {
                    parent_lease_fd = Some(rest[1].parse().unwrap_or_else(|_| {
                        eprintln!("{usage}");
                        std::process::exit(2);
                    }));
                    rest = &rest[2..];
                }
                #[cfg(windows)]
                {
                    eprintln!(
                        "sot-capsule: --parent-lease-fd is Unix-only; this platform's supervisor \
                         passes --parent-lease-name <name> instead (ADR 0043 decision 15)"
                    );
                    std::process::exit(2);
                }
            }
            Some("--survival") if rest.len() > 1 => {
                survival = match rest[1].as_str() {
                    "normal" => sot_log::lane::wire::Survival::Normal,
                    "degraded" => sot_log::lane::wire::Survival::Degraded,
                    _ => {
                        eprintln!("{usage}");
                        std::process::exit(2);
                    }
                };
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
    let argv: Vec<String> = rest[1..].to_vec();

    #[cfg(windows)]
    let parent_lease = parent_lease_name.map(sot_log::capsule::producer::ParentLease::NamedMutex);
    #[cfg(unix)]
    let parent_lease = parent_lease_fd.map(sot_log::capsule::producer::ParentLease::InheritedFd);

    let config = sot_log::capsule::CapsuleConfig {
        voyage_root,
        voyage_id,
        screen_path,
        argv,
        cols,
        rows,
        survival,
        parent_lease,
    };
    // No command source yet (Ctrl+C kills the process instead — see the
    // doc above). The transport IS real now: `Transport::bind` (called by
    // `run` itself, at the transport-lifetime invariant's exact point)
    // creates `\\.\pipe\sot-voyage-<voyage_id>` (Windows) or
    // `<runtime_dir>/voyage-<voyage_id>.sock` (Unix) for real attach/mgmt
    // clients to connect to.
    let (_cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let mut transport = make_transport(MAX_TRANSPORT_CONNECTIONS);
    match sot_log::capsule::run::<P>(config, cmd_rx, &mut transport) {
        Ok(s) => {
            run_note(format_args!(
                "producer exited {:?} ({:?}) (handshake_answered={}, \
                 handshake_suppressed={}, resize_os_calls={})",
                s.exit_code,
                s.exit_kind,
                s.handshake_answered,
                s.handshake_suppressed_matches,
                s.resize_os_calls
            ));
            // Reinterpretation to a process exit code happens ONLY here,
            // at the actual OS process-exit boundary — everywhere else in
            // this crate the value stays a raw, unsigned DWORD on Windows. ADR 0043
            // decision 13: `Signal` is the real Unix shape for a signal
            // death, mapped `128 + n` (the POSIX shell convention); on
            // Windows it stays unreachable but mapped the same honest way.
            std::process::exit(leg_exit_code(&Ok(s)));
        }
        Err(e) => {
            run_note(format_args!("{e}"));
            std::process::exit(leg_exit_code(&Err(e)));
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

/// The leg's process exit code: the producer's own exit code (`128 + n` for a signal death), 1 for a run that failed
/// before or without one.
fn leg_exit_code(result: &sot_log::Result<sot_log::capsule::ExitSummary>) -> i32 {
    use sot_log::capsule::producer::ExitStatus;
    match result {
        Err(_) => 1,
        Ok(s) => match s.exit_code {
            Some(ExitStatus::Code(c)) => c as i32,
            Some(ExitStatus::Signal(n)) => 128 + n,
            None => 1,
        },
    }
}

/// `sot-capsule supervise <state_dir> <--start|--resume> [--cols <n>] \
/// [--rows <n>] [--first-leg-only <token>]... [--claim-fd <n> --takeover-fd <n>]
/// --assume-no-rollback-target -- <cmd> [args...]` (ADR 0041 step 6 U2): the authority. `--assume-no-rollback-target` is
/// accepted and ignored: the rollout gate it answered is gone. `--first-leg-only` is
/// repeatable and agent-agnostic: this binary knows nothing about `claude`
/// or `--resume`, only that the caller wants a given token on the legs
/// before any producer has run in this process and on none after — see
/// `SuperviseConfig::first_leg_only`.
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
    // ADR 0042 slice L1a: the spawner's own
    // breakaway outcome, threaded into every leg via `SuperviseConfig`
    // (see that field's own doc) — defaults to `Normal` for a bare
    // manual invocation, matching every existing caller of this CLI that
    // predates the flag.
    let mut survival = sot_log::lane::wire::Survival::Normal;
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
            Some("--survival") if rest.len() > 1 => {
                survival = match rest[1].as_str() {
                    "normal" => sot_log::lane::wire::Survival::Normal,
                    "degraded" => sot_log::lane::wire::Survival::Degraded,
                    _ => {
                        eprintln!("{usage}");
                        std::process::exit(2);
                    }
                };
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
        survival,
        first_leg_only,
        #[cfg(unix)]
        birth,
    };
    #[cfg(not(unix))]
    let _ = birth;
    std::process::exit(sot_log::supervisor::supervise(config));
}

/// `sot-capsule endrun <state_dir> [--voyage <id>] [--reason <text>]`
/// (ADR 0041 step 6 U2): the no-supervisor path's own fence-acquiring
/// EndRun. Fails loudly (never terminates blind) if a real supervisor is
/// already the authority — see `sot_log::supervisor::endrun`'s own doc.
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
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
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
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

// `sot_log::supervisor::journal::fence::lock_supervisor` must be
// reachable from THIS binary crate -- Cargo treats `src/bin/sot-capsule.rs`
// as a SEPARATE crate from the package's own library even though they share
// one Cargo.toml. This test is the
// actual proof: it calls the public facade from the real consumer,
// not merely from the library's own test suite.
#[cfg(test)]
#[path = "support/sot_capsule_tests.rs"]
mod tests;
