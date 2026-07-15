// sotd
//
// Long-lived daemon that owns the per-session Ship of Tools state. Per ADR 0010 it
// runs on the remote inside a tmux session and accepts frontend connections
// over a single transport:
//   --socket <path>   — local socket (AF_UNIX / Windows named pipe via
//                       interprocess::local_socket). Cross-machine access
//                       goes through an SSH local forward that terminates
//                       at this socket — never a network listener.
//
// The daemon TCP listener (and the app-level auth token that existed to
// gate it) was removed in 0.4.0: every deployment rides the private local
// socket, whose filesystem / named-pipe ownership under a private parent
// directory is the access control. Its one field use was the 2026-07-11
// twin-daemon split-brain. See ADR 0010's 0.4.0 update block.

mod clients;
mod concept;
mod file_io;
mod files_mode;
mod handlers;
mod http_serve;
mod kernel;
mod mathjax;
mod monitor;
mod paths;
mod pluto;
mod pty;
mod repl;
mod server;
mod session;
mod session_state;
mod site_serve;
mod tmux;
mod update;
mod watcher;
mod workspaces;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

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
/// stdout (unchanged — existing launchers that redirect it, e.g.
/// `scripts/install.sh`'s `>/tmp/sotd.log`, keep working exactly as before)
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

#[tokio::main]
async fn main() -> Result<()> {
    // Pure query subcommands (security review): checked against raw argv
    // BEFORE any startup side effect (umask, private log file/state dir
    // creation, tracing init) below. Previously pure path/version queries were
    // handled inside `parse_args()`, which only runs
    // AFTER those side effects — so a shell script that just wants the
    // socket path (comm-lib.sh's `sot_tmux_socket`, potentially called
    // often, and optionally steered by `SOT_TMUX_SOCK` for tmux-server
    // migration) was spinning up the daemon's log file/state dir as a
    // byproduct of a read-only query. Only recognised as the FIRST
    // argument (true subcommand position, matching how both are actually
    // invoked — `sotd tmux-socket-path`, `sotd session-socket-path sot`,
    // `sotd --version`); this replaces,
    // rather than duplicates, the arms that used to live in `parse_args()`.
    if let Some(first) = std::env::args().nth(1) {
        match first.as_str() {
            "tmux-socket-path" => {
                println!("{}", paths::tmux_socket_path().display());
                return Ok(());
            }
            "session-socket-path" => {
                let label = std::env::args()
                    .nth(2)
                    .unwrap_or_else(|| "default".to_string());
                println!("{}", paths::session_socket_path(&label).display());
                return Ok(());
            }
            "--version" | "-V" => {
                println!("{}", sot_protocol::version_line("sotd"));
                return Ok(());
            }
            _ => {}
        }
    }

    apply_umask();
    let log_file = open_private_log_file();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(move || TeeWriter {
            file: log_file.clone(),
        })
        .init();

    let opts = parse_args().context("parsing command-line arguments")?;

    tracing::info!(
        socket = ?opts.socket,
        project_root = ?opts.project_root,
        label = ?opts.label,
        "sotd starting"
    );

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
    /// defaults to `paths::session_socket_path(label)` per ADR 0013; goes
    /// into the per-backend toml the frontend writes and helps Sessions
    /// mode match the running daemon to its on-disk metadata.
    pub label: Option<String>,
}

fn parse_args() -> Result<Opts> {
    let mut socket: Option<PathBuf> = None;
    let mut project_root_arg: Option<PathBuf> = None;
    let mut label: Option<String> = None;

    // `--version`/`-V` and `tmux-socket-path` are handled earlier, in
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
    })
}


