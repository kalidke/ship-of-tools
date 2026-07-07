// sotd
//
// Long-lived daemon that owns the per-session Ship of Tools state. Per ADR 0010 it
// runs on the remote inside a tmux session and accepts frontend connections
// over one or both transports:
//   --socket <path>   — local socket (AF_UNIX / Windows named pipe via
//                       interprocess::local_socket). Default for loopback.
//   --tcp <host:port> — TCP listener for the cross-machine path (Windows
//                       local → Linux remote, acceptance #4). Bind defaults
//                       to 127.0.0.1; remote access goes through SSH local
//                       forward, never by exposing the bind address.
//
// At least one transport must be configured. Both run concurrently and feed
// accepted streams into the same generic connection task — the codec is
// transport-agnostic, so the only thing that varies is auth (the optional
// app-level token is enforced on every transport when configured).

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

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
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
        eprintln!("sotd: could not create state dir {}: {e} (logging to stdout only)", dir.display());
        return None;
    }
    let path = dir.join("sotd.log");
    let file = match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("sotd: could not open log file {}: {e} (logging to stdout only)", path.display());
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
    // creation, tracing init) below. Previously `tmux-socket-path` and
    // `--version`/`-V` were handled inside `parse_args()`, which only runs
    // AFTER those side effects — so a shell script that just wants the
    // socket path (comm-lib.sh's `sot_tmux_socket`, potentially called
    // often) was spinning up the daemon's log file/state dir as a
    // byproduct of a read-only query. Only recognised as the FIRST
    // argument (true subcommand position, matching how both are actually
    // invoked — `sotd tmux-socket-path`, `sotd --version`); this replaces,
    // rather than duplicates, the arms that used to live in `parse_args()`.
    if let Some(first) = std::env::args().nth(1) {
        match first.as_str() {
            "tmux-socket-path" => {
                println!("{}", paths::tmux_socket_path().display());
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
        .with_writer(move || TeeWriter { file: log_file.clone() })
        .init();

    let opts = parse_args().context("parsing command-line arguments")?;

    // Fail-closed default (security review): a TCP listener with no resolved
    // auth token is reachable by any local user on this shared host (or
    // anyone who can reach the bound address) — refuse to start silently
    // open rather than let a missing token slip through unnoticed.
    // `--insecure-no-auth` is the explicit, named opt-out for a deliberately
    // open dev/test box. A unix-socket-only bind (no `--tcp`) is exempt:
    // its filesystem permissions already gate who can even connect.
    if let Some(tcp) = opts.tcp {
        if opts.token.is_none() && !opts.insecure_no_auth {
            anyhow::bail!(
                "refusing to start: --tcp {tcp} is configured with no auth token \
                 resolved (checked --token, $SOT_TOKEN, and {}). Set one of those, \
                 or pass --insecure-no-auth to run open anyway (not recommended on \
                 a shared host).",
                token_file_path().display(),
            );
        }
    }

    tracing::info!(
        socket = ?opts.socket,
        tcp = ?opts.tcp,
        token_set = opts.token.is_some(),
        insecure_no_auth = opts.insecure_no_auth,
        project_root = ?opts.project_root,
        label = ?opts.label,
        "sotd starting"
    );

    server::run(opts).await
}

#[derive(Debug, Clone)]
pub struct Opts {
    pub socket: Option<PathBuf>,
    pub tcp: Option<SocketAddr>,
    /// Required token on every transport when set. Resolution order:
    /// `--token` (explicit override, kept for back-compat with existing
    /// launchers) wins when given; else `$SOT_TOKEN`; else the canonical
    /// token file (`token_file_path()` —
    /// `${XDG_CONFIG_HOME:-~/.config}/sot/token`), the same convention the
    /// comm scripts and the systemd unit already read. The file fallback
    /// means a token can be configured without ever appearing in this
    /// process's argv (world-visible via `ps`) or requiring every launch
    /// site to export an env var — new launchers should prefer it or
    /// `$SOT_TOKEN` over `--token`. An empty string from any of the three
    /// sources (`--token ""`, `SOT_TOKEN=`, a blank/whitespace-only file) is
    /// treated as unset, never as a valid-but-empty token — this field is
    /// never `Some(String::new())` (security review: that value would make
    /// the hello token check trivially pass for a client presenting no
    /// token at all). The file is also refused outright — a hard startup
    /// error, not a silent fallback — if it's a symlink or group/other-
    /// readable; see `read_token_file_at`.
    pub token: Option<String>,
    /// Filesystem root the Files-mode tree exposes. Defaults to the current
    /// working directory; `--project-root <path>` overrides.
    pub project_root: PathBuf,
    /// Optional human-friendly label for this backend. When set, `--socket`
    /// defaults to `paths::session_socket_path(label)` per ADR 0013; goes
    /// into the per-backend toml the frontend writes and helps Sessions
    /// mode match the running daemon to its on-disk metadata.
    pub label: Option<String>,
    /// Explicit opt-out of the fail-closed "no token on a TCP listener"
    /// refusal (security review). Never implied by anything else — always a
    /// deliberate flag on this exact invocation.
    pub insecure_no_auth: bool,
}

/// Canonical token file (`${XDG_CONFIG_HOME:-~/.config}/sot/token`) — the
/// same path the comm scripts (`comm-relay.sh` etc.) and the systemd unit
/// already read. Shares `workspaces::app_config_dir` so every token/config
/// resolver in this codebase agrees on one dir.
fn token_file_path() -> PathBuf {
    workspaces::app_config_dir().join("token")
}

/// Read + trim the canonical token file. Thin wrapper over
/// `read_token_file_at` with the canonical path filled in.
fn read_token_file() -> Result<Option<String>> {
    read_token_file_at(&token_file_path())
}

/// Core of `read_token_file`, parameterized on `path` so it's unit-testable
/// without touching `$XDG_CONFIG_HOME` (a process-global env var other tests
/// in this binary mutate — see `trim_token_contents`'s doc comment for why
/// that's a real hazard here, not hypothetical).
///
/// Unlike `--token`/`$SOT_TOKEN`, a file on disk can be tampered with by
/// another local account or redirected via a symlink, so this refuses to
/// trust it (a hard `Err`, not a silent `Ok(None)`) rather than quietly
/// falling through to "no token configured" — that fallthrough would look
/// identical to an intentionally-unset token and could combine with
/// `--insecure-no-auth` reasoning the user never made. Concretely refuses:
/// - a symlink at `path` — opened with `O_NOFOLLOW`, so this surfaces as an
///   open error (`ELOOP`) instead of transparently following it. A same-
///   workspace-writable attacker could otherwise repoint "the token file" at
///   something they control, or at a target whose content never changes.
/// - (Unix) a file that's group- or other-readable (`mode & 0o077 != 0`) —
///   a token another local account can read isn't a secret on a shared host.
///
/// A file that's simply absent is `Ok(None)` — the expected, unremarkable
/// case on a fresh install with no token configured yet.
fn read_token_file_at(path: &Path) -> Result<Option<String>> {
    let mut open_opts = std::fs::OpenOptions::new();
    open_opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open_opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = match open_opts.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(anyhow::anyhow!(
                "refusing to read token file {}: {e} (is it a symlink?)",
                path.display()
            ))
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = file.metadata()?.mode();
        if mode & 0o077 != 0 {
            anyhow::bail!(
                "refusing to trust token file {} — mode {:o} is group/other-readable; \
                 chmod 0600 it (owner read/write only) before restarting sotd",
                path.display(),
                mode & 0o777,
            );
        }
    }
    let mut contents = String::new();
    std::io::Read::read_to_string(&mut file, &mut contents)
        .with_context(|| format!("read {}", path.display()))?;
    Ok(trim_token_contents(&contents))
}

/// Trim + empty-check for token-file content, pulled out as a pure function
/// so it's unit-testable without touching `$XDG_CONFIG_HOME` — a
/// process-global env var that other tests in this binary also mutate
/// (`session_state.rs`'s `write_backend_identity_round_trip`), and
/// `set_var`/`var` aren't synchronized against concurrently-running
/// `#[test]` fns, so an env-var-based hermetic test here raced them.
fn trim_token_contents(contents: &str) -> Option<String> {
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_args() -> Result<Opts> {
    let mut socket: Option<PathBuf> = None;
    let mut tcp: Option<SocketAddr> = None;
    let mut token_arg: Option<String> = None;
    let mut project_root_arg: Option<PathBuf> = None;
    let mut label: Option<String> = None;
    let mut insecure_no_auth = false;

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
            "--tcp" => {
                let s = args.next().context("--tcp requires host:port")?;
                let addr: SocketAddr = s
                    .parse()
                    .with_context(|| format!("parse --tcp {s:?} as host:port"))?;
                tcp = Some(addr);
            }
            "--token" => {
                token_arg = Some(args.next().context("--token requires a value")?);
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
            "--insecure-no-auth" => {
                insecure_no_auth = true;
            }
            other => anyhow::bail!("unrecognised argument: {other}"),
        }
    }

    if socket.is_none() {
        if let Ok(p) = std::env::var("SOT_SOCKET") {
            socket = Some(PathBuf::from(p));
        }
    }
    // Token resolution (security review) — see `Opts::token`'s doc comment
    // for the precedence order. An empty `--token`/`$SOT_TOKEN` is treated
    // as unset (not a valid-but-empty token) — `--token ""` used to slip
    // through here unfiltered (only `$SOT_TOKEN` was), which set
    // `expected_token` to `Some("")` downstream; `handle_hello` then
    // compared it against a client's default-empty presented token and
    // matched trivially, authenticating with NO real secret while still
    // reading as "a token is configured" (bypassing `--insecure-no-auth`
    // too). All three sources are now filtered the same way.
    let token = token_arg
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("SOT_TOKEN").ok().filter(|s| !s.is_empty()));
    let token = match token {
        Some(t) => Some(t),
        None => read_token_file().context("reading token file")?,
    };
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

    if socket.is_none() && tcp.is_none() {
        anyhow::bail!(
            "no transport configured: pass --socket <path>, --label <name>, --tcp <host:port>, or set SOT_SOCKET"
        );
    }

    Ok(Opts {
        socket,
        tcp,
        token,
        project_root,
        label,
        insecure_no_auth,
    })
}

#[cfg(test)]
mod token_file_tests {
    use super::trim_token_contents;

    #[test]
    fn trims_content() {
        assert_eq!(trim_token_contents("  s3cr3t\n").as_deref(), Some("s3cr3t"));
    }

    #[test]
    fn whitespace_only_is_none() {
        assert_eq!(trim_token_contents("   \n"), None);
    }

    #[test]
    fn empty_is_none() {
        assert_eq!(trim_token_contents(""), None);
    }
}

/// Unix-only: exercises `read_token_file_at` against real temp files with
/// controlled permissions/symlink-ness, parameterized on `path` (not the
/// canonical `token_file_path()`) for exactly the same env-var-race reason
/// `trim_token_contents` was pulled out pure — see its doc comment.
#[cfg(all(test, unix))]
mod read_token_file_tests {
    use super::read_token_file_at;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_path(name: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "sot-token-test-{}-{}-{name}",
            std::process::id(),
            n
        ))
    }

    fn write_file(path: &PathBuf, contents: &str, mode: u32) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn missing_file_is_ok_none() {
        let p = scratch_path("missing");
        assert!(read_token_file_at(&p).unwrap().is_none());
    }

    #[test]
    fn private_file_with_content_is_ok_some() {
        let p = scratch_path("private");
        write_file(&p, "s3cr3t\n", 0o600);
        assert_eq!(read_token_file_at(&p).unwrap().as_deref(), Some("s3cr3t"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn group_readable_file_is_rejected() {
        let p = scratch_path("group-readable");
        write_file(&p, "s3cr3t\n", 0o640);
        assert!(read_token_file_at(&p).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn world_readable_file_is_rejected() {
        let p = scratch_path("world-readable");
        write_file(&p, "s3cr3t\n", 0o644);
        assert!(read_token_file_at(&p).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn symlink_to_token_file_is_rejected() {
        let target = scratch_path("symlink-target");
        write_file(&target, "s3cr3t\n", 0o600);
        let link = scratch_path("symlink-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read_token_file_at(&link).is_err());
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(&link);
    }
}
