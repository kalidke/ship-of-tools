// sotd
//
// Long-lived daemon that owns the per-session Ship of Tools state. Per ADR 0010 it
// runs on the remote as a user service and accepts frontend connections
// over a single transport:
//   --socket <path>   — local socket (AF_UNIX / Windows named pipe via
//                       interprocess::local_socket).
//
// The daemon TCP listener (and the app-level auth token that existed to
// gate it) was removed in 0.4.0: every deployment rides the private local
// socket, whose filesystem / named-pipe ownership under a private parent
// directory is the access control. Its one field use was the 2026-07-11
// twin-daemon split-brain. See ADR 0010's 0.4.0 update block.

mod agents;
mod clients;
mod comm;
mod daemon_log;
mod durable;
mod files;
mod install;
mod lifecycle;
mod pages;
mod paths;
mod rows;
mod server;
mod session;
mod sidecars;
mod topology;
mod update;

use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

/// Restrict default file-creation permissions to owner-only (security review): new files are `0600` and new dirs `0700`
/// whatever umask the launching shell had. Set ONCE, as the first thing this process does: `umask` is process-global, so
/// a scoped change would race every other file-creating task. A project file saved through `file.write`/`concept.write`
/// that was more permissive becomes `0600`. Unix-only; Windows has no umask (ACLs are out of scope).
#[cfg(unix)]
fn apply_umask() {
    // SAFETY: `umask` mutates only this process's file-creation mask and has
    // no aliasing/pointer preconditions; `0o077` is a valid mode_t constant.
    unsafe {
        libc::umask(0o077);
    }
}
#[cfg(not(unix))]
fn apply_umask() {}

/// `sotd --help`'s text; also the fallback for every command without a usage of its own.
const SOTD_HELP: &str = r#"Usage: sotd [OPTIONS]

  --socket <path>        listen on this unix socket / named pipe
                          (default: $SOT_SOCKET, else derived from --label)
  --project-root <path>  filesystem root the Files-mode tree exposes
                          (default: $SOT_PROJECT_ROOT, else the cwd)
  --label <name>         human-friendly backend label (Sessions mode
                          matches the running daemon to it)
  --adopt-legacy-registry  allow the Windows one-time legacy registry
                          adoption (installed launchers only)

Offline command (returns before daemon initialization):
  install --pack <folder> [options]
                          the body of scripts/install.sh and install.ps1,
                          which verify a pack and run its own sotd install
  trust declare <absolute-prefix>
                          declare folder trust in user-level settings
  update stage <folder> --prefix <prefix>
                          stage, prepare and arm a packed build for the
                          install at <prefix> (its converge applies it)
  update pointer --prefix <prefix> --target <target>
                          whether the pointer armed for <target> may apply
                          onto the install at <prefix> (exit 3: it may not)
  update direction <version> --prefix <prefix>
                          newer, same or older: the way the install at
                          <prefix> moves to <version>
  update repls --endpoint <unix:PATH|pipe:PATH>
                          the rows of the daemon there with a live REPL, one
                          label per line (exit 3 when there are any)

Pure queries (no startup side effects, answered before any of the above):
  --version, -V           print the version line and exit
  --help, -h              print usage and exit; every subcommand accepts it
                          (and then does nothing else)
  session-socket-path [label]
                          print the per-session socket path and exit
                          (no label: this box's own daemon)
  log-path                print the path of the daemon's log, sotd.log, and exit
  agent-exec <kind> [flags…] (Unix only)
                          resolve and exec the named agent's launch
                          recipe in place (ADR 0046 decision 4); only
                          "claude" has a recipe today
  ancestors [--from <pid>] (Windows only)
                          print the ancestors of <pid> (default: this
                          process), parent first: pid, exe, command line
  work-state <verb> --handle <h> [--origin user|machine] [--text]
                          one work-state write for a session's row, the
                          way comm-status.sh reaches it (text on stdin)
  topology <plan|status|relay-endpoint|sync|apply>
                          what this box derives from hosts.toml
                          (`sotd topology` alone prints the details)
  status [--json]        declared + LIVE: fans out to every reachable
                          daemon (topology plan §E; `sotd topology status`
                          stays the offline, declared-only view)
  same-version --endpoint <unix:PATH|pipe:PATH>
                          exit 0 if the daemon there runs this version,
                          3 (printing its version) if it runs another"#;

/// The usage to print when `--help`/`-h` is among `sotd`'s OWN arguments, else `None`.
/// `sotd`'s own arguments are all of `args` (argv without argv[0]) except for `agent-exec`,
/// where only the kind position counts: the words after it belong to the agent (`ccb --help`).
/// A help request must never write, dial or exec, so `main` asks this before anything else runs.
/// A `--help`/`-h` among those arguments is a help request even where it would be an option's
/// value (`--socket --help`): such a value is not supported. The parsers share no option table,
/// and a wrong guess only prints help, so there is one rule and no second option list.
fn help_for(args: &[String]) -> Option<&'static str> {
    let first = args.first()?;
    let owned = if first == "agent-exec" { &args[1..args.len().min(2)] } else { args };
    if !owned.iter().any(|a| a == "--help" || a == "-h") {
        return None;
    }
    Some(match first.as_str() {
        "trust" => agents::trust_declaration::USAGE,
        "topology" => topology::cli::USAGE,
        "status" => topology::status::USAGE,
        "stdio-bridge" => topology::stdio_bridge::USAGE,
        "same-version" => topology::same_version::USAGE,
        "ancestors" => comm::registry::ancestors::USAGE,
        "work-state" => rows::work_state::cli::USAGE,
        "update" => update::USAGE,
        "install" => install::USAGE,
        _ => SOTD_HELP,
    })
}

#[cfg(test)]
#[path = "main_help_tests.rs"]
mod help_tests;

/// The runtime `#[tokio::main]` built before this entry was synchronous: main builds it itself, after the serving
/// prologue, so that what must exist before any thread (the durable parent, on Linux the lifetime guard) can.
fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building the tokio runtime")
}

#[allow(
    clippy::too_many_lines,
    reason = "the daemon entry: startup checks, boot and the serve loop in one function; predates the 100-line limit"
)]
fn main() -> Result<()> {
    #[cfg(unix)]
    lifecycle::child_signal::reset_child_signal();
    // Pure query subcommands (security review): checked against raw argv
    // BEFORE any startup side effect (umask, private log file/state dir
    // creation, tracing init) below. Previously pure path/version queries were
    // handled inside `parse_args()`, which only runs
    // AFTER those side effects — so a shell script that just wants the
    // socket path was spinning up the daemon's log file/state dir as a
    // byproduct of a read-only query. Only recognised as the FIRST
    // argument (true subcommand position, matching how both are actually
    // invoked — `sotd session-socket-path sot`, `sotd --version`); this
    // replaces, rather than duplicates, the arms that used to live in
    // `parse_args()`.
    // Help comes first: see `help_for`. Read lossily: `env::args` panics on a value that is not
    // UTF-8, and such a value can never be `--help` or `-h` anyway.
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    if let Some(text) = help_for(&args) {
        println!("{}", sot_protocol::version_line("sotd"));
        println!("{text}");
        return Ok(());
    }
    if let Some(first) = std::env::args().nth(1) {
        match first.as_str() {
            "trust" => {
                apply_umask();
                let outcome = agents::trust_declaration::run(&args[1..])?;
                println!("{outcome:?}");
                return Ok(());
            }
            "session-socket-path" => {
                // No label means this box's own daemon — the only thing a
                // caller that omits it can mean, and the one place that
                // answers which label that is. It used to fall back to a
                // phantom `default` label no daemon ever binds.
                let label = std::env::args()
                    .nth(2)
                    .unwrap_or_else(|| paths::local_daemon_label().to_string());
                println!("{}", paths::session_socket_path(&label).display());
                return Ok(());
            }
            // The daemon's log of record: the launchers name it when a backend does not come up, because a daemon
            // whose stdout is a file writes its log only there. A pure query like the arm above.
            "log-path" => {
                println!("{}", paths::state_dir().join("sotd.log").display());
                return Ok(());
            }
            // `sotd ancestors [--from <pid>]` (Windows only): comm-lib.sh reads it to count
            // the agents above a comm script. A pure query like the arms around it.
            "ancestors" => lifecycle::shutdown::exit(comm::registry::ancestors::run(&std::env::args().skip(2).collect::<Vec<_>>())),
            "agent-exec" => agents::ops::agent_exec(),
            // `sotd work-state <verb> --handle <h> ...`: one work-state write for a session (rows/CLAUDE.md, work-state),
            // the way the hooks' and declarations' script reaches it. The comm folder is private: the umask first.
            "work-state" => {
                apply_umask();
                std::process::exit(rows::work_state::cli::run(&args[1..]));
            }
            // The capsule-only birth parent (`rows::spawn::durable`): the daemon starts it over a private channel it
            // holds as standard input. Not a user command, so it is in no usage text.
            #[cfg(unix)]
            "durable-parent" => {
                apply_umask();
                std::process::exit(rows::spawn::durable::parent::run(&args[1..]));
            }
            // The last inch of a cross-host dial: connect to THIS box's
            // own endpoint for a label and shuttle stdin/stdout. Sits in
            // this early block for the same reason the queries above do —
            // it must not create the daemon's log file or state dir as a
            // byproduct — and additionally because anything those steps
            // printed would land on the byte stream it owns.
            "stdio-bridge" => {
                let args: Vec<String> = std::env::args().skip(2).collect();
                lifecycle::shutdown::exit(topology::stdio_bridge::run(&args));
            }
            // Whether the daemon on a local endpoint runs this binary's version: the launch scripts' ensure asks it
            // before keeping a daemon it finds running. A pure query, answered here like the bridge above.
            "same-version" => {
                let args: Vec<String> = std::env::args().skip(2).collect();
                lifecycle::shutdown::exit(topology::same_version::run(&args));
            }
            // The declared topology (`hosts.toml` v2): what this box
            // derives from it — the launcher's tunnel/dial plan, the relay
            // endpoint, the declared table, a fetch of the hub's copy.
            "topology" => {
                let args: Vec<String> = std::env::args().skip(2).collect();
                lifecycle::shutdown::exit(topology::cli::run(&args));
            }
            // `sotd status` (topology plan §E): declared + LIVE, fanned out
            // to every reachable daemon concurrently — unlike `topology`
            // above this needs a runtime, so main builds one here and blocks on it
            // rather than calling a plain synchronous query.
            "status" => {
                let args: Vec<String> = std::env::args().skip(2).collect();
                lifecycle::shutdown::exit(runtime()?.block_on(topology::status::run(&args)));
            }
            // `sotd update stage <folder> --prefix <prefix>`: arm a packed build for an install's converge.
            // `sotd update pointer --prefix <prefix> --target <target>`: may the armed pointer apply onto the install?
            // `sotd update direction <version> --prefix <prefix>` and `sotd update repls --endpoint <endpoint>`: the
            // installer's two questions before it moves an install.
            "update" => {
                apply_umask();
                lifecycle::shutdown::exit(runtime()?.block_on(update::run(&args[1..])));
            }
            // `sotd install --pack <folder> [options]`: the installers' body (scripts/CLAUDE.md, Installers). A command a
            // person runs in the foreground; it starts no daemon, and its files take the person's own umask, as the
            // shell installer's did.
            "install" => {
                std::process::exit(install::run(&args[1..]));
            }
            "--version" | "-V" => {
                println!("{}", sot_protocol::version_line("sotd"));
                return Ok(());
            }
            // `--help`/`-h` is answered above by `help_for`, before this match.
            _ => {}
        }
    }

    apply_umask();

    // First reach of `rows::store::app_config_dir` is the registry load far
    // below; refuse here, before any state is created, rather than panic
    // mid-request (or fall back to a shared directory).
    if let Err(msg) = rows::store::check_config_dir() {
        eprintln!("sotd: {msg}");
        lifecycle::shutdown::exit(78);
    }
    // Defect fix (field-proven, Windows; see `sot_log::host::winhandle`'s module
    // doc): harden this process's own inherited stdio before anything is
    // spawned, so it can never leak into a supervisor/leg that outlives us.
    #[cfg(windows)]
    if let Err(e) = sot_log::host::winhandle::harden_own_stdio(true) {
        eprintln!("sotd: could not harden inherited stdio ({e}); continuing");
    }

    // Security review addendum (item 2b, v0.6.5 macOS field report): the
    // runtime dir and its sockets already get a symlink/ownership/mode
    // check before anything is trusted to live there; the STATE root
    // `XDG_STATE_HOME` selects did not. On a shared host that root can
    // sit under a world-writable sticky parent (e.g. `/scratch`), where
    // an attacker-precreated or symlinked directory would receive
    // session records. Checked here, at the very top of startup, before
    // `open_private_log_file` below (or anything else) ever touches it —
    // every daemon gets this, not only one that goes on to create a
    // capsule row (`rows::spawn::state_root::qualified_state_root` applies the
    // SAME check again per row create/attach, as defense in depth
    // against the directory being altered after this boot-time check).
    // A symlink or a foreign owner is refused outright. A mode that lets
    // group/other in on a directory this uid owns is repaired to 0700
    // first (what the previous `ensure_private_dir` always did), so a
    // hand-made `mkdir` under a 022 umask does not brick the daemon on
    // the next boot. Tracing isn't initialized yet at this point, so this
    // is stderr-only, same as the harden_own_stdio fallback right above.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = paths::state_dir();
        if let Ok(meta) = std::fs::symlink_metadata(&dir) {
            if meta.is_dir()
                && meta.uid() == paths::current_uid()
                && meta.permissions().mode() & 0o077 != 0
            {
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            }
        }
    }
    if let Err(e) = paths::secure_private_dir(&paths::state_dir()) {
        eprintln!(
            "sotd: state dir {} is not private ({e}) — refusing to start",
            paths::state_dir().display()
        );
        lifecycle::shutdown::exit(1);
    }

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    daemon_log::init(filter);

    let opts = parse_args().context("parsing command-line arguments")?;

    // A second daemon pointed at a live daemon's socket refuses here,
    // before any startup side effect (`server::listen::refuse_live_socket`).
    #[cfg(unix)]
    if let Some(path) = opts.socket.as_deref() {
        server::listen::refuse_live_socket(path)?;
    }

    // Finding 1, v0.6.5 macOS field report: refuse to serve a capsule row
    // sotd can never actually start, rather than booting cleanly and
    // leaving every pane blinking "supervisor lane not answering" while
    // the journal claims a start that produced no process. A pre-0.6
    // `sot-apply` (run by an old install's updater/systemd unit) only
    // knew to swap `sot` and `sotd`; `sot-capsule` (new in 0.6) is left
    // behind in the staged tarball. Plain check, no auto-repair — the
    // fix is a real reinstall (docs/INSTALL-AGENT.md), which
    // `sot-apply.sh` itself cannot retroactively become for a box already
    // running the old copy (see that script's own re-exec-the-staged-copy
    // comment for the case this DOES cover).
    if let Ok(exe) = std::env::current_exe() {
        if !rows::spawn::detach::capsule_sibling_present(&exe) {
            let msg = format!(
                "sotd: sot-capsule is missing next to sotd ({}); this install was upgraded by a pre-0.6 apply — re-run the installer: fetch docs/INSTALL-AGENT.md from main and follow it",
                exe.display()
            );
            eprintln!("{msg}");
            tracing::error!("{msg}");
            lifecycle::shutdown::exit(1);
        }
    }

    // Every session's SOTD_BIN is this start's own path, made absolute once, now (agents::env::own_sotd_bin).
    let _ = agents::env::own_sotd_bin();

    // The prologue's forks below (the durable parent's intermediate, then the guard) need a process with one thread: a
    // boot with a second one is refused before either fork.
    #[cfg(target_os = "linux")]
    {
        #[cfg(feature = "daemon-lifetime-faults")]
        if std::env::var_os("SOT_TEST_PROLOGUE_THREAD").is_some() {
            std::thread::spawn(|| std::thread::sleep(std::time::Duration::from_secs(3600)));
        }
        if let Err(e) = lifecycle::daemon_children::guard::require_one_thread() {
            eprintln!("sotd: {e} -- refusing to start");
            lifecycle::shutdown::exit(1);
        }
    }

    // The capsule-only birth parent, started once, here, before the runtime and before any thread: it is forked through
    // an intermediate that exits, so it descends from nothing this daemon starts later, and it is never started again.
    // A failure is logged and every capsule start then says so (`rows::spawn::durable::proxy`).
    #[cfg(unix)]
    if let Err(e) = rows::spawn::durable::proxy::start_before_runtime() {
        tracing::warn!(error = %e, "durable parent: not started; capsule rows cannot start until the daemon is restarted");
    }

    // On Linux this daemon becomes the child of a guard that ends everything the daemon starts when the daemon ends
    // (`lifecycle::daemon_children::guard`). It needs a process with one thread, so it comes after the durable parent
    // and before the runtime, the relay refresh and every other thread.
    #[cfg(target_os = "linux")]
    if let Err(e) = lifecycle::daemon_children::guard::install() {
        eprintln!("sotd: {e} -- refusing to start");
        lifecycle::shutdown::exit(1);
    }

    // INT and TERM take the fire every other controlled exit takes and then end the daemon by the signal itself, from a
    // thread of their own, so a stalled runtime or an inherited blocked mask does not hold them. After the guard, which
    // needs one thread.
    if let Err(error) = lifecycle::signal_exit::install() {
        eprintln!("sotd: signal watcher installation failed: {error}");
        lifecycle::shutdown::exit(1);
    }

    tracing::info!(
        socket = ?opts.socket,
        project_root = ?opts.project_root,
        label = ?opts.label,
        "sotd starting"
    );

    comm::mail::record_at_boot();

    #[cfg(target_os = "linux")]
    topology::relay_units::spawn_refresh_at_start();

    let runtime = runtime()?;
    complete_main(
        &runtime,
        async move {
            #[cfg(unix)]
            if let Err(e) = rows::spawn::durable::connect_parent() {
                tracing::warn!(error = %e, "durable parent: no answer; capsule rows cannot start until the daemon is restarted");
            }
            #[cfg(feature = "daemon-lifetime-faults")]
            return tokio::select! { served = server::run(opts) => served, injected = injected_outcome() => injected };
            #[cfg(not(feature = "daemon-lifetime-faults"))]
            server::run(opts).await
        },
        |code| lifecycle::shutdown::exit(code),
    )
}

/// The serving daemon's main result becomes a status while the runtime still exists, and takes the one terminal path: `Ok`
/// is 0, an error is printed and is 1, a panic of the main future is 101 (what an unwinding `main` would have given).
fn complete_main<T>(
    runtime: &tokio::runtime::Runtime,
    future: impl std::future::Future<Output = Result<()>>,
    terminate: impl FnOnce(i32) -> T,
) -> T {
    let code =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.block_on(future))) {
            Ok(Ok(())) => 0,
            Ok(Err(error)) => {
                eprintln!("Error: {error:?}");
                1
            }
            Err(_) => 101,
        };
    terminate(code)
}

/// Under the daemon-lifetime fault feature a case ends the main future with a result the product would reach only through a
/// defect: the file named by `SOT_TEST_MAIN_OUTCOME` holds `ok`, `err`, `panic`, `stall` (every runtime worker blocked) or
/// `raise:<signal>` (the daemon sends that signal to itself, as an outside sender's would arrive, so no case names the
/// daemon's pid) once the case is ready. The control selects an outcome and performs no cleanup.
#[cfg(feature = "daemon-lifetime-faults")]
async fn injected_outcome() -> Result<()> {
    let Some(path) = std::env::var_os("SOT_TEST_MAIN_OUTCOME") else {
        return std::future::pending().await;
    };
    loop {
        match std::fs::read_to_string(&path).as_deref().map(str::trim) {
            Ok("ok") => return Ok(()),
            Ok("err") => anyhow::bail!("injected main error"),
            Ok("panic") => panic!("injected main panic"),
            // A signal the daemon sends itself (Unix): the harness's stimulus for a death it does not deliver itself.
            #[cfg(unix)]
            Ok(raise) if raise.starts_with("raise:") => {
                let signal: i32 = raise["raise:".len()..].parse().expect("a signal number");
                // SAFETY: a signal to this process, sent by itself.
                unsafe { libc::kill(libc::getpid(), signal) };
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
            // Every runtime worker blocked at once: nothing the runtime runs makes progress for two minutes.
            Ok("stall") => {
                let workers = std::thread::available_parallelism().map_or(8, |n| n.get()) * 2;
                for _ in 0..workers {
                    tokio::spawn(async { std::thread::sleep(std::time::Duration::from_secs(120)) });
                }
                tokio::time::sleep(std::time::Duration::from_secs(120)).await;
            }
            _ => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Opts {
    /// The one transport: a private local socket (AF_UNIX / Windows named
    /// pipe). Clients present no app token — filesystem / named-pipe
    /// ownership under a private parent dir is the access control (the
    /// hello `token` wire field survives for cross-version compat and is
    /// ignored). The TCP listener + token machinery were removed in 0.4.0.
    pub socket: Option<PathBuf>,
    /// Filesystem root the Files-mode tree exposes. Defaults to the current
    /// working directory; `--project-root <path>` overrides.
    pub project_root: PathBuf,
    /// Optional human-friendly label for this backend. When set, `--socket`
    /// defaults to `paths::session_socket_path(label)` per ADR 0013.
    pub label: Option<String>,
    /// `--adopt-legacy-registry` (`rows::store::scan_disk`'s own gate):
    /// `false` unless passed, so a scratch/test daemon can never steal a
    /// box's pending legacy adoption (field-proven). Only
    /// `sot-local-daemon.ps1` passes it.
    pub adopt_legacy_registry: bool,
}

fn parse_args() -> Result<Opts> {
    let mut socket: Option<PathBuf> = None;
    let mut project_root_arg: Option<PathBuf> = None;
    let mut label: Option<String> = None;
    let mut adopt_legacy_registry = false;

    // `--version`/`-V` and `session-socket-path` are handled earlier, in
    // `main()`, before any startup side effect — see the comment there.
    // Not re-recognised here: if either slips through as a later/extra
    // argument in some invocation this fast path didn't catch, falling
    // into `other => bail!` below is correct (an actual server start with
    // a stray subcommand-shaped token is a usage error, not a silent
    // retry of the query).
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--socket" => {
                let p = args.next().context("--socket requires a path argument")?;
                socket = Some(PathBuf::from(p));
            }
            "--project-root" => {
                let p = args
                    .next()
                    .context("--project-root requires a path argument")?;
                project_root_arg = Some(PathBuf::from(p));
            }
            "--label" => {
                label = Some(args.next().context("--label requires a name")?);
            }
            "--adopt-legacy-registry" => {
                adopt_legacy_registry = true;
            }
            // Removed in 0.4.0 with the daemon TCP listener; named here so a
            // stale launcher gets a pointed error instead of "unrecognised".
            "--tcp" | "--token" | "--insecure-no-auth" => anyhow::bail!(
                "{a} was removed in 0.4.0 (the daemon listens only on its \
                 private local socket; SSH-forward to it for remote access — \
                 see docs/history/adr/0010-transport-and-persistence.md)"
            ),
            other => anyhow::bail!("unrecognised argument: {other}"),
        }
    }

    if socket.is_none() {
        if let Ok(p) = std::env::var("SOT_SOCKET") {
            socket = Some(PathBuf::from(p));
        }
    }
    let project_root = project_root_arg
        .or_else(|| std::env::var_os("SOT_PROJECT_ROOT").map(PathBuf::from))
        .unwrap_or_else(|| std::env::current_dir().expect("no current dir"));

    // `--label` auto-derives `--socket` when the latter isn't given.
    // Sessions mode (frontend) uses the same convention so spawn commands
    // can elide the explicit path: `sotd --label MyPkg --project …`.
    if socket.is_none() {
        if let Some(name) = label.as_deref() {
            socket = Some(paths::session_socket_path(name));
        }
    }

    if socket.is_none() {
        anyhow::bail!(
            "no transport configured: pass --socket <path>, --label <name>, or set SOT_SOCKET"
        );
    }

    Ok(Opts {
        socket,
        project_root,
        label,
        adopt_legacy_registry,
    })
}
