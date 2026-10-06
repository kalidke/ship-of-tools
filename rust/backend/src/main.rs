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
mod durable;
mod files;
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
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use sot_log::secret::RedactingWriter;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Restrict default file-creation permissions to owner-only (security
/// review): without this, every file sotd creates — its own log, the
/// workspace registry toml, `.sot-tmp` write-then-rename siblings, trash,
/// sockets, … — inherits whatever umask the launching shell happened to
/// have, often `0022` (world-readable). `0o077` makes new files `0600` and
/// new dirs `0700` by default. Set ONCE, permanently, as the very first
/// thing this process does — NOT scoped/restored around individual
/// operations: `umask` is process-global, not per-thread, and sotd is a
/// multi-threaded async runtime, so a temporarily-scoped change would race
/// every other concurrently-running file-creating task. Note the one real
/// side effect: `file.write`/`concept.write` saves go through the same
/// write-then-rename path, so a project file that was previously more
/// permissive (e.g. group-readable) becomes `0600` the next time Ship of
/// Tools saves it — acceptable for a single-user dev tool, but worth knowing.
/// Unix-only; Windows has no umask concept (ACLs are separate, out of scope).
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

/// Writer for `tracing_subscriber::fmt`: mirrors every log line to BOTH
/// stdout (unchanged — a launcher that redirects it keeps working)
/// AND a private file under `paths::state_dir()` (security review: sotd now
/// owns a real, `0600` copy of its own log regardless of how it's launched,
/// rather than depending entirely on the launcher's redirect target). The
/// file half is best-effort: `None` (couldn't create the state dir / open
/// the file) or a write failure there never breaks the stdout path.
struct TeeWriter {
    file: Option<Arc<Mutex<std::fs::File>>>,
}

impl std::io::Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = std::io::Write::write(&mut std::io::stdout(), buf)?;
        if let Some(f) = &self.file {
            if let Ok(mut f) = f.lock() {
                let _ = f.write_all(buf);
            }
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut std::io::stdout())?;
        if let Some(f) = &self.file {
            if let Ok(mut f) = f.lock() {
                let _ = f.flush();
            }
        }
        Ok(())
    }
}

/// Open (create/append) the private log file, enforcing `0600` unconditionally
/// (not just on creation — a file left over from before this fix, or opened
/// under a looser umask, won't self-correct otherwise). `None` on any
/// failure; the caller falls back to stdout-only logging rather than
/// treating this as fatal — a log-file problem shouldn't block the daemon
/// from starting.
fn open_private_log_file() -> Option<Arc<Mutex<std::fs::File>>> {
    let dir = paths::state_dir();
    if let Err(e) = paths::ensure_private_dir(&dir) {
        eprintln!(
            "sotd: could not create state dir {}: {e} (logging to stdout only)",
            dir.display()
        );
        return None;
    }
    let path = dir.join("sotd.log");
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "sotd: could not open log file {}: {e} (logging to stdout only)",
                path.display()
            );
            return None;
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = file.set_permissions(std::fs::Permissions::from_mode(0o600)) {
            eprintln!("sotd: could not chmod log file {}: {e}", path.display());
        }
    }
    Some(Arc::new(Mutex::new(file)))
}

/// The daemon's log: events at the level `filter` passes (`main` gives the `RUST_LOG` level, default `info`), each masked of page secrets
/// (`sot_log::secret`) and written, without colour codes, through `TeeWriter` to stdout and the private file.
fn log_subscriber(
    filter: EnvFilter,
    file: Option<Arc<Mutex<std::fs::File>>>,
) -> impl tracing::Subscriber + Send + Sync {
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(move || RedactingWriter(TeeWriter { file: file.clone() }))
        .finish()
}

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

Pure queries (no startup side effects, answered before any of the above):
  --version, -V           print the version line and exit
  --help, -h              print usage and exit; every subcommand accepts it
                          (and then does nothing else)
  session-socket-path [label]
                          print the per-session socket path and exit
                          (no label: this box's own daemon)
  agent-exec <kind> [flags…] (Unix only)
                          resolve and exec the named agent's launch
                          recipe in place (ADR 0046 decision 4); only
                          "claude" has a recipe today
  ancestors [--from <pid>] (Windows only)
                          print the ancestors of <pid> (default: this
                          process), parent first: pid, exe, command line
  topology <plan|status|relay-endpoint|sync|apply>
                          what this box derives from hosts.toml
                          (`sotd topology` alone prints the details)
  status [--json]        declared + LIVE: fans out to every reachable
                          daemon (topology plan §E; `sotd topology status`
                          stays the offline, declared-only view)"#;

/// The usage to print when `--help`/`-h` is among `sotd`'s OWN arguments, else `None`.
/// `sotd`'s own arguments are all of `args` (argv without argv[0]) except for `agent-exec`,
/// where only the kind position counts: the words after it belong to the agent (`ccb --help`).
/// A help request must never write, dial or exec, so `main` asks this before anything else runs.
/// A `--help`/`-h` among those arguments is a help request even where it would be an option's
/// value (`--socket --help`): such a value is not supported. The parsers share no option table,
/// and a wrong guess only prints help, so there is one rule and no second option list.
fn help_for(args: &[String]) -> Option<&'static str> {
    let first = args.first()?;
    let owned = if first == "agent-exec" {
        &args[1..args.len().min(2)]
    } else {
        args
    };
    if !owned.iter().any(|a| a == "--help" || a == "-h") {
        return None;
    }
    Some(match first.as_str() {
        "topology" => topology::cli::USAGE,
        "status" => topology::status::USAGE,
        "stdio-bridge" => topology::stdio_bridge::USAGE,
        "ancestors" => comm::registry::ancestors::USAGE,
        _ => SOTD_HELP,
    })
}

#[cfg(test)]
mod help_tests {
    use super::*;

    fn v(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn help_for_table() {
        if !sot_log::test_isolated::run_isolated("help_tests::help_for_table") {
            return;
        }
        let rows: &[(&str, &str)] = &[
            ("--help", SOTD_HELP),
            ("--version --help", SOTD_HELP),
            ("--label x --help", SOTD_HELP),
            ("--socket --help", SOTD_HELP),
            ("session-socket-path --help", SOTD_HELP),
            ("agent-exec --help", SOTD_HELP),
            ("ancestors --help", comm::registry::ancestors::USAGE),
            (
                "ancestors --from 1 --help",
                comm::registry::ancestors::USAGE,
            ),
            ("stdio-bridge --help", topology::stdio_bridge::USAGE),
            (
                "stdio-bridge --host a --help",
                topology::stdio_bridge::USAGE,
            ),
            ("status --help", topology::status::USAGE),
            ("topology --help", topology::cli::USAGE),
            ("topology plan --help", topology::cli::USAGE),
            ("topology status --help", topology::cli::USAGE),
            ("topology relay-endpoint --help", topology::cli::USAGE),
            ("topology relay-sockets --help", topology::cli::USAGE),
            ("topology sync --help", topology::cli::USAGE),
            ("topology apply --help", topology::cli::USAGE),
            ("topology apply --yes --help", topology::cli::USAGE),
            ("topology apply --help --yes", topology::cli::USAGE),
            ("topology set --help", topology::cli::USAGE),
            ("topology set add --help", topology::cli::USAGE),
            ("topology set add h --help", topology::cli::USAGE),
            ("topology set remove --help", topology::cli::USAGE),
        ];
        for (argv, want) in rows {
            for flag in ["--help", "-h"] {
                let mut args = v(argv);
                for a in args.iter_mut().filter(|a| *a == "--help") {
                    *a = flag.to_string();
                }
                assert_eq!(help_for(&args), Some(*want), "{args:?}");
            }
        }
        for argv in [
            "agent-exec claude --help",
            "agent-exec claude -h",
            "topology plan",
            "status --json",
            "session-socket-path sot",
            "",
        ] {
            assert_eq!(help_for(&v(argv)), None, "{argv:?}");
        }
    }
}

fn main() {
    #[cfg(unix)]
    lifecycle::child_signal::reset_child_signal();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("sotd: runtime creation failed: {error}");
            lifecycle::shutdown::exit(1);
        }
    };
    complete_main(&runtime, daemon_main(), |code| {
        lifecycle::shutdown::exit(code)
    });
}

#[allow(
    clippy::too_many_lines,
    reason = "the daemon entry: startup checks, boot and the serve loop in one function; predates the 100-line limit"
)]
async fn daemon_main() -> Result<()> {
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
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if let Some(text) = help_for(&args) {
        println!("{}", sot_protocol::version_line("sotd"));
        println!("{text}");
        return Ok(());
    }
    if let Some(first) = std::env::args().nth(1) {
        match first.as_str() {
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
            // `sotd ancestors [--from <pid>]` (Windows only): comm-lib.sh reads it to count
            // the agents above a comm script. A pure query like the arms around it.
            "ancestors" => lifecycle::shutdown::exit(comm::registry::ancestors::run(
                &std::env::args().skip(2).collect::<Vec<_>>(),
            )),
            "agent-exec" => agents::ops::agent_exec(),
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
            // The declared topology (`hosts.toml` v2): what this box
            // derives from it — the launcher's tunnel/dial plan, the relay
            // endpoint, the declared table, a fetch of the hub's copy.
            "topology" => {
                let args: Vec<String> = std::env::args().skip(2).collect();
                lifecycle::shutdown::exit(topology::cli::run(&args));
            }
            // `sotd status` (topology plan §E): declared + LIVE, fanned out
            // to every reachable daemon concurrently — unlike `topology`
            // above this needs the runtime we're already inside (owned by the synchronous main boundary), so it's awaited here rather than called as
            // a plain synchronous query.
            "status" => {
                let args: Vec<String> = std::env::args().skip(2).collect();
                lifecycle::shutdown::exit(topology::status::run(&args).await);
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
    log_subscriber(filter, open_private_log_file()).init();

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

    tracing::info!(
        socket = ?opts.socket,
        project_root = ?opts.project_root,
        label = ?opts.label,
        "sotd starting"
    );

    comm::mail::record_at_boot();

    #[cfg(target_os = "linux")]
    topology::relay_units::spawn_refresh_at_start();

    server::run(opts).await
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
                 see docs/adr/0010-transport-and-persistence.md)"
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

#[cfg(test)]
mod log_tests {
    use super::*;

    /// ADR 0049, User isolation: the daemon's log file never holds a page secret an event carries.
    #[test]
    fn the_daemon_log_masks_page_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sotd.log");
        let file = std::fs::File::create(&path).unwrap();
        let subscriber = log_subscriber(EnvFilter::new("trace"), Some(Arc::new(Mutex::new(file))));
        let token = "0123456789abcdef0123456789abcdef";
        let _log = sot_log::test_log::install(subscriber);
        tracing::error!(%token, "open http://127.0.0.1:1/x?secret=Ab12Cd34");
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("<redacted>"),
            "the event did not reach the file: {written}"
        );
        assert!(
            !written.contains(token),
            "the token reached the file: {written}"
        );
        assert!(
            !written.contains("Ab12Cd34"),
            "the secret reached the file: {written}"
        );
        // No colour codes: they would split a field name from its `=`, so `secret=` would not read as one marker.
        assert!(
            !written.contains('\u{1b}'),
            "the file carries ANSI escapes: {written:?}"
        );
    }
}

/// Convert main completion while the caller still owns the runtime, then take its terminal path.
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
