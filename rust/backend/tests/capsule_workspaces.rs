#![cfg(any(windows, target_os = "linux"))]
//! ADR 0042 slice L1a / ADR 0043 decision 22 (LU4) real-process
//! integration test: a real `sotd`, a real `sot-capsule[.exe]` it spawns
//! DETACHED, talking the actual wire protocol over a real local socket
//! (a named pipe on Windows, an `AF_UNIX` socket on Linux) — the same
//! posture `rust/log/tests/supervisor_win.rs`/`supervisor.rs` take for
//! the supervisor authority one layer down. Requires `sot-capsule[.exe]`
//! already built into the SAME target directory as `sotd[.exe]` (the CI
//! job builds the whole workspace first — see `.github/workflows/rust.yml`'s
//! `conpty-windows-2022` and `ubuntu-latest` jobs; production locates it
//! the identical way, next to the daemon's own executable). Every
//! `workspace.create` in this file requests `"runtime": "capsule"`
//! explicitly (the field exists for exactly this — see `ops.rs`'s own
//! doc); the Linux platform default stays "tmux" until the bridge.
//!
//! Every wait below is a BOUNDED poll or `tokio::time::timeout` for an
//! external, observable fact (the socket accepting a connection, a
//! `workspace.list` row's own `phase` field, a supervisor lane going
//! silent) — never a sleep-and-hope, and never an unbounded read/write/
//! kill/wait (Codex review finding 13).
//!
//! Uses `sot_log::supervisor_client` directly (a real dependency of this
//! crate, not a test double) for two proofs the daemon's own wire
//! protocol has no op for: (1) `stop` ends JUST the supervisor authority
//! while its capsule leg survives (ADR 0041 Lifecycle — legs are
//! deliberately outside the supervisor's own job), which is how this
//! test proves ADOPTION (a fresh `--resume` finding the SAME leg still
//! alive, never bumping its epoch) rather than mere detachment (an
//! untouched, already-running supervisor merely surviving a daemon
//! restart); (2) `query_status` after `workspace.destroy` independently
//! confirms the record actually closed before this test ever asserts the
//! row is gone from `workspace.list`.

use std::cell::RefCell;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use interprocess::local_socket::tokio::{prelude::*, Stream as LocalStream};
use interprocess::local_socket::GenericFilePath;
use sot_protocol::{codec, op, Frame, HelloReq, Kind};
// LU5a: only the Linux-only unqualified-state-root refusal test below
// needs this -- Windows has no tmpfs-as-state-root concern (its own
// NTFS-only preflight is unrelated and unchanged), so an unguarded import
// here would warn unused on that leg.
#[cfg(target_os = "linux")]
use sot_protocol::slug;

/// Real-process tests share one CI runner; serialize them like
/// `supervisor_win.rs`'s own `SERIAL` — a spawned `sotd` plus a spawned
/// `sot-capsule` plus a spawned platform-shell leg is real load on a
/// two-core box. `tokio::sync::Mutex`, not `std::sync::Mutex`: this test
/// is async and holds the guard across `.await` points for its whole
/// body.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Every bounded wait in this file shares one figure — generous over any
/// single supervisor-lane round trip (connect 2s + hello 2s + status 5s
/// ~= 9s worst case) but still a real bound, never "forever."
const BOUND: Duration = Duration::from_secs(30);

/// Pinned `SOT_STATE_HOST` for every `spawn_sotd` in this file — a fixed,
/// known per-host registry dir name instead of whatever `%COMPUTERNAME%`
/// happens to be on the runner (`workspaces::state_host`'s fallback).
/// `Env::seed_default_capsule_toml` computes the same path from it.
const TEST_STATE_HOST: &str = "testhost";

fn sotd_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sotd"))
}

/// The capsule executable's own file name for this platform — mirrors
/// `capsule_workspace::runtime`'s own `CAPSULE_EXE` fork.
#[cfg(windows)]
const CAPSULE_EXE_NAME: &str = "sot-capsule.exe";
#[cfg(target_os = "linux")]
const CAPSULE_EXE_NAME: &str = "sot-capsule";

/// Resolved the same way production does — `current_exe().parent()` — but
/// from the TEST binary's own known sibling (`sotd[.exe]`'s own directory),
/// since a `tests/*.rs` binary itself lives in `target/<profile>/deps/`,
/// not `target/<profile>/`.
fn sot_capsule_exe() -> PathBuf {
    sotd_exe().with_file_name(CAPSULE_EXE_NAME)
}

/// This test file's own daemon-wire socket for `tag` — a named pipe on
/// Windows, a plain filesystem path on Linux (`interprocess::local_socket`'s
/// `GenericFilePath` name kind treats either shape as "just a path" — see
/// its own use in `try_connect` below). Unique per test process (its pid)
/// so a re-run never collides with a still-tearing-down prior instance.
/// This is a SEPARATE socket from every real capsule supervisor/voyage
/// lane the daemon itself spawns (those live under `SOT_RUNTIME_DIR`,
/// ADR 0043 decision 1) — this one is only the test-as-client's own
/// connection to `sotd`'s wire protocol. On Linux the daemon's own
/// `--socket` startup check refuses a group/other-accessible PARENT
/// directory (`secure socket dir ... mode ... is group/other-accessible`)
/// — plain `/tmp` fails that outright — so `runtime_dir` (the SAME
/// private, owner-only dir `SOT_RUNTIME_DIR` already points at) hosts
/// this socket too, under a name that cannot collide with a real
/// supervisor/voyage socket there.
#[cfg(windows)]
fn test_socket_path(_runtime_dir: &Path, tag: &str) -> PathBuf {
    PathBuf::from(format!(r"\\.\pipe\sot-test-{tag}-{}", std::process::id()))
}
#[cfg(target_os = "linux")]
fn test_socket_path(runtime_dir: &Path, tag: &str) -> PathBuf {
    runtime_dir.join(format!("wire-{tag}-{}.sock", std::process::id()))
}

/// Kill + wait a child with a real bound (Codex review finding 13: "the
/// test reuses ... unbounded ... kill waits"). Runs the blocking
/// kill+wait on a `spawn_blocking` thread so the bound is a real
/// `tokio::time::timeout`, not merely a hope that `wait()` returns fast
/// after `kill()`. This is the TEST's own deliberate, asserted teardown
/// of the daemon it owns (`Env::kill_daemon_bounded`); `Env`'s own `Drop`
/// (F4, LU4 review round 2) stays a best-effort, unbounded-but-brief
/// safety net for the panic/early-return paths a bounded async call
/// cannot run from — mirroring `supervisor_win.rs`'s own `KillGuard`,
/// which this file's `Env` now subsumes (the daemon `Child` moved from a
/// separate guard into `Env` itself so its `Drop` can order the daemon
/// kill before the leg sweep and the tmux teardown, F4's own ordering
/// requirement).
async fn kill_and_wait_bounded(child: Child) {
    let mut child = child;
    let res = tokio::time::timeout(
        BOUND,
        tokio::task::spawn_blocking(move || {
            let _ = child.kill();
            let _ = child.wait();
        }),
    )
    .await;
    assert!(res.is_ok(), "killing/waiting a spawned process exceeded {BOUND:?}");
}

/// Bounded async poll for an external, observable fact.
async fn poll_until<T, F, Fut>(mut attempt: F, timeout: Duration, what: &str) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = attempt().await {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// One isolated test environment. ADR 0042 L1a, Codex review finding 2:
/// the DAEMON's own `--project-root` (`daemon_project_root`) and the
/// workspace this test creates (`workspace_project_root`) are TWO
/// SEPARATE sibling directories — the daemon already registers its own
/// root as the default workspace at startup, so creating a second
/// workspace pointed at that SAME root trips the duplicate-root gate
/// (`code: "duplicate_root"`) before this test's own capsule logic is
/// ever exercised.
struct Env {
    _tmp: tempfile::TempDir,
    /// L1-unix LU4 (ADR 0043 decision 1): `SOT_RUNTIME_DIR` — the private
    /// dir every real supervisor/voyage socket on Linux lives under,
    /// named by hash rather than nested under `state_root`. A SEPARATE,
    /// SHORT-prefixed `tempdir_in("/tmp")` (never under `_tmp`, whose own
    /// prefix is not size-bounded): `sun_path` is 108 bytes including the
    /// NUL on Linux, so keeping this dir's own path short leaves headroom
    /// for the `supervisor-<h>.sock`/`voyage-<uuid>.sock` suffix. Unused
    /// on Windows (named pipes have no such path-length concern) but kept
    /// unconditional — one `Env` shape on both platforms.
    _runtime_tmp: tempfile::TempDir,
    /// LU5a: `Some` only for [`Env::new_with_state_root_on_tmpfs`] — a
    /// SEPARATE tempdir (under `/dev/shm`, never under `_tmp`) that
    /// `state_root` itself lives in for that constructor, kept alive here
    /// for this `Env`'s whole lifetime. `None` for the ordinary
    /// [`Env::new`], where `state_root` is just a subdirectory of `_tmp`.
    _state_root_tmp: Option<tempfile::TempDir>,
    daemon_project_root: PathBuf,
    workspace_project_root: PathBuf,
    state_root: PathBuf,
    config_root: PathBuf,
    socket_path: PathBuf,
    /// LU4 review round 2, F4: the currently-live `sotd` child, owned by
    /// `Env` itself (not a separate `KillGuard` local) so `Env`'s own
    /// `Drop` can kill it FIRST, before it ever sweeps this env's own
    /// legs or tears down its isolated tmux server — the exact ordering
    /// bug this replaces (`kill_any_lingering_leg` used to run BEFORE the
    /// daemon died, so its 1s crash-restart could spawn a fresh
    /// supervisor into an already-swept state dir). `RefCell`, not
    /// `Mutex`: every `#[tokio::test]` in this file runs on the default
    /// current-thread flavor, so `Env` is only ever touched by one task
    /// at a time — no real concurrent access to guard against, only the
    /// interior mutability `&self`-taking methods (`spawn_sotd`,
    /// `kill_daemon_bounded`) need. Re-armed on every `spawn_sotd`/
    /// `spawn_sotd_with_prepended_path` call (the daemon-restart tests
    /// spawn a second one after killing the first).
    daemon: RefCell<Option<Child>>,
}

impl Env {
    fn new(tag: &str) -> Self {
        Self::new_with_state_root_base(tag, None)
    }

    /// LU5a (ADR 0043 decision 23): the SAME environment, but `state_root`
    /// is minted under `/dev/shm` (tmpfs) instead of under the ordinary
    /// `_tmp` tempdir — for the daemon's own VOLATILE-type refusal test.
    /// Smallest-change shape: every other path (project roots, config
    /// root, runtime dir, socket) stays exactly what [`Env::new`] already
    /// gives them; only the one directory `qualified_state_root` actually
    /// judges moves.
    #[cfg(target_os = "linux")]
    fn new_with_state_root_on_tmpfs(tag: &str) -> Self {
        Self::new_with_state_root_base(tag, Some(Path::new("/dev/shm")))
    }

    /// Shared by [`Env::new`] (`state_root_base: None`, a subdirectory of
    /// `_tmp`) and [`Env::new_with_state_root_on_tmpfs`] (`Some(dir)`, a
    /// fresh tempdir directly under `dir`).
    fn new_with_state_root_base(tag: &str, state_root_base: Option<&Path>) -> Self {
        // `_tmp` (project/state/config) has no socket path deriving from
        // it directly — a real supervisor/voyage socket's own name is a
        // FIXED-LENGTH hash of the state dir path (`state_dir_hash`),
        // never that path itself nested under a socket directory — so
        // `std::env::temp_dir()` (which honours `$TMPDIR`) is fine here
        // on every platform.
        let tmp = tempfile::Builder::new()
            .prefix("sotcw-")
            .tempdir_in(std::env::temp_dir())
            .expect("tempdir");
        // `runtime_tmp` (`SOT_RUNTIME_DIR`) is different: every real
        // supervisor/voyage socket AND this test's own wire socket
        // (`test_socket_path`) live directly under it, so ITS OWN path
        // length is exactly the `sun_path` budget (108 bytes including
        // the NUL, ADR 0043 decision 1's own concern) every one of those
        // names eats into. `std::env::temp_dir()` would honour an
        // ambient `$TMPDIR`, which can be arbitrarily long (the LU1b
        // lesson — every other suite in this crate uses a literal `/tmp`
        // on Unix for exactly this reason) — a literal `/tmp` here,
        // short prefix, matches them. Windows has no such bound (named
        // pipes aren't real filesystem paths), so `temp_dir()` stays fine
        // there.
        #[cfg(unix)]
        let runtime_base = PathBuf::from("/tmp");
        #[cfg(windows)]
        let runtime_base = std::env::temp_dir();
        let runtime_tmp = tempfile::Builder::new()
            .prefix("sotrt-")
            .tempdir_in(runtime_base)
            .expect("runtime tempdir");
        // `tempfile` creates directories respecting the process umask
        // (typically 0755, not 0700) — both `SOT_RUNTIME_DIR`'s own
        // `is_private_dir` check and the daemon's `--socket` parent-dir
        // check (`secure socket dir ... is group/other-accessible`)
        // require owner-only. Unix only: harmless to skip on Windows,
        // where neither check applies.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(runtime_tmp.path(), std::fs::Permissions::from_mode(0o700))
                .expect("chmod runtime tempdir to 0700");
        }
        // ADR 0043 decision 1 (propagation, not discovery): every direct
        // `sot_log::supervisor_client::*`/`sot_log::fence::*` call THIS
        // TEST PROCESS ITSELF makes (never through the daemon's own wire
        // protocol) must resolve the SAME `SOT_RUNTIME_DIR` the spawned
        // `sotd` was launched with (`spawn_sotd`'s own `.env(...)`) — a
        // process-env var set only on the CHILD is invisible here, so
        // this process's own env is set too. Safe under `SERIAL`: every
        // test acquires that lock before ever calling `Env::new`, so only
        // one `Env`'s runtime dir is ever "active" at a time.
        std::env::set_var("SOT_RUNTIME_DIR", runtime_tmp.path());
        let daemon_project_root = tmp.path().join("daemon-project");
        std::fs::create_dir_all(&daemon_project_root).expect("mkdir daemon_project_root");
        let workspace_project_root = tmp.path().join("workspace-project");
        std::fs::create_dir_all(&workspace_project_root).expect("mkdir workspace_project_root");
        // LU5a: `state_root_base` overrides where `state_root` itself
        // lives — `None` is the ordinary case (a subdirectory of `_tmp`,
        // on whatever filesystem the system temp dir happens to sit on);
        // `Some(base)` mints a fresh tempdir directly under `base`
        // instead (the tmpfs refusal test's own `/dev/shm`).
        let (state_root, state_root_tmp) = match state_root_base {
            None => {
                let p = tmp.path().join("state");
                std::fs::create_dir_all(&p).expect("mkdir state_root");
                (p, None)
            }
            Some(base) => {
                let d = tempfile::Builder::new()
                    .prefix("sotcw-state-")
                    .tempdir_in(base)
                    .unwrap_or_else(|e| panic!("tempdir under {base:?} (is it mounted?): {e}"));
                let p = d.path().to_path_buf();
                (p, Some(d))
            }
        };
        let config_root = tmp.path().join("config");
        std::fs::create_dir_all(&config_root).expect("mkdir config_root");
        let socket_path = test_socket_path(runtime_tmp.path(), tag);
        Self {
            _tmp: tmp,
            _runtime_tmp: runtime_tmp,
            _state_root_tmp: state_root_tmp,
            daemon_project_root,
            workspace_project_root,
            state_root,
            config_root,
            socket_path,
            daemon: RefCell::new(None),
        }
    }

    /// This env's own ISOLATED tmux server socket (F3, LU4 review round
    /// 2): under `_runtime_tmp`, same directory every real supervisor/
    /// voyage socket lives under, so it shares that dir's short-prefix,
    /// 0700-owner-only properties. Passed to the spawned daemon as
    /// `SOT_TMUX_SOCK` (`paths::tmux_socket_path`'s own override, verified
    /// by grep against `rust/backend/src/paths.rs`) so the default row's
    /// own tmux-session-ensure at boot (`server.rs` ~:369 — runs whenever
    /// the default row's runtime is NOT "capsule", which is every Linux
    /// test's own default row) never touches the developer's REAL tmux
    /// server (`/run/user/<uid>/sot/tmux.sock`), the leak this review item
    /// closes. Unconditional (not `cfg(unix)`) for the same "one `Env`
    /// shape on both platforms" reason every other env var here is: the
    /// var is simply unread on a platform with no tmux server to ensure.
    fn tmux_sock(&self) -> PathBuf {
        self._runtime_tmp.path().join("tmux.sock")
    }

    /// The bounded, deliberate daemon teardown every test in this file
    /// ends its own run with (Codex review finding 13's own "never an
    /// unbounded kill/wait" rule) — takes `Env`'s own tracked child (if
    /// any is still live; a no-op after an already-completed restart-and-
    /// kill sequence) and reaps it via `kill_and_wait_bounded`. Leaves
    /// `Env`'s own `Drop` (F4) with nothing to do for its own daemon-kill
    /// step in the common, non-panicking case — exactly the relationship
    /// the old separate `KillGuard` had with its own `Drop`.
    async fn kill_daemon_bounded(&self) {
        let child = self.daemon.borrow_mut().take();
        if let Some(child) = child {
            kill_and_wait_bounded(child).await;
        }
    }

    /// Spawn a real `sotd` rooted at this env's project/state/config —
    /// `sot_log::state_dir::sot_state_dir()` reads `%LOCALAPPDATA%` on
    /// Windows / `$XDG_STATE_HOME` on Linux directly (no daemon CLI flag
    /// exists for it), and `workspaces.rs`'s own registry root reads
    /// `%XDG_CONFIG_HOME%`/`$XDG_CONFIG_HOME` on the respective platform —
    /// all overridden here so this process's capsule state and workspace
    /// registry both live under the SAME temp root a second `sotd` launch
    /// (the adoption leg of this test) can point at again. Every env var
    /// is set UNCONDITIONALLY (one shape, not a per-platform cfg split):
    /// the platform this daemon actually runs on only ever reads its own
    /// pair, so setting the other platform's var too is harmless.
    /// `SOT_STATE_HOST` is pinned so the per-host registry dir
    /// (`workspaces::state_host`, which otherwise falls back to
    /// `%COMPUTERNAME%`/the real hostname) is a fixed, known name —
    /// `seed_default_capsule_toml` below has to compute the SAME path
    /// from the test side to pre-write a toml this daemon will read.
    /// `SOT_RUNTIME_DIR` (Linux only, ADR 0043 decision 1) pins every
    /// real supervisor/voyage socket this daemon (and the `sot-capsule`
    /// it spawns, which inherits this env var) binds under `_runtime_tmp`
    /// instead of falling back to host discovery
    /// (`$XDG_RUNTIME_DIR`/`/run/user/<uid>`, not always present or
    /// writable on a CI runner with no login session). `SOT_TMUX_SOCK`
    /// (F3) pins the default row's own tmux-session-ensure at boot to
    /// this env's own isolated server instead of the developer's real
    /// one. The spawned child is stored into `self.daemon`, not returned
    /// — `Env` owns it now so its own `Drop` can order the daemon kill
    /// ahead of the leg sweep and tmux teardown (F4).
    fn spawn_sotd(&self) {
        let child = Command::new(sotd_exe())
            .arg("--socket")
            .arg(&self.socket_path)
            .arg("--project-root")
            .arg(&self.daemon_project_root)
            .env("LOCALAPPDATA", &self.state_root)
            .env("XDG_STATE_HOME", &self.state_root)
            .env("XDG_CONFIG_HOME", &self.config_root)
            .env("SOT_STATE_HOST", TEST_STATE_HOST)
            .env("SOT_RUNTIME_DIR", self._runtime_tmp.path())
            .env("SOT_TMUX_SOCK", self.tmux_sock())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sotd");
        let previous = self.daemon.borrow_mut().replace(child);
        debug_assert!(previous.is_none(), "spawn_sotd called while a prior daemon was still tracked");
    }

    /// The daemon's own app-config dir for THIS env, per platform:
    /// Windows joins "config" onto its state root
    /// (`app_config_dir`'s own Windows arm: `windows_state_root().join("config")`,
    /// i.e. `%LOCALAPPDATA%\sot\config`); everywhere else it is
    /// `$XDG_CONFIG_HOME/sot` directly, no "config" segment (that arm
    /// never joins "sot" onto the state root at all — a DIFFERENT root
    /// than `state_root`, which is why `config_root` is its own separate
    /// temp dir, not a subdirectory of `state_root`). Getting this
    /// arithmetic wrong silently seeds the pre-written toml at a path
    /// `workspaces::load_toml` never scans.
    #[cfg(windows)]
    fn app_config_dir(&self) -> PathBuf {
        self.state_root.join("sot").join("config")
    }
    #[cfg(target_os = "linux")]
    fn app_config_dir(&self) -> PathBuf {
        self.config_root.join("sot")
    }

    /// Pre-write an ARBITRARY capsule row's own toml BEFORE `spawn_sotd`
    /// boots the daemon, with `runtime = "capsule"` and the given
    /// `agent` — the same registry path `workspaces::save`/`load_toml`
    /// use (`<app config dir>/workspaces-<SOT_STATE_HOST>/<slug>.toml`,
    /// [`Env::app_config_dir`]). Only `workspace_id`/`slug`/`project_root`
    /// are required for `load_toml` to treat this as canonical
    /// (`workspaces.rs`'s own doc); every other field the daemon needs
    /// defaults sensibly.
    ///
    /// 2026-09-04 amendment: `scan_disk` (`workspaces.rs`, which loads
    /// this toml) runs BEFORE the daemon's own default-row seed logic
    /// and has no spawn side effect of its own (`scan_dir` only ever
    /// calls `reg.insert`) — so a NON-default slug pre-written this way
    /// registers as a plain, ordinary capsule workspace whose supervisor
    /// has NEVER been started by anything, the one precondition
    /// `workspace.create`'s own handler can never produce (it spawns
    /// synchronously as part of creation itself). This is what lets a
    /// test exercise `pty.open`'s start-on-attach (`ensure_started`) on
    /// an ordinary row instead of the default one.
    fn seed_capsule_toml(&self, workspace_id: &str, slug: &str, project_root: &Path, agent: &str) {
        let dir = self.app_config_dir().join(format!("workspaces-{TEST_STATE_HOST}"));
        std::fs::create_dir_all(&dir).expect("mkdir pre-seeded workspaces dir");
        let project_root = project_root.to_string_lossy();
        let body = format!(
            "workspace_id  = \"{workspace_id}\"\n\
             slug          = \"{slug}\"\n\
             project_root  = \"{project_root}\"\n\
             runtime       = \"capsule\"\n\
             agent         = \"{agent}\"\n"
        );
        std::fs::write(dir.join(format!("{slug}.toml")), body)
            .expect("write pre-seeded capsule row toml");
    }

    /// [`seed_capsule_toml`] specialized to the DEFAULT row: slug
    /// computed the same way the daemon computes it (`--project-root`'s
    /// own basename run through `sot_protocol::slug`; no `--label` is
    /// ever passed in this file) so `server.rs`'s own boot seed resolves
    /// THIS pre-written row as "the existing default" rather than
    /// minting a fresh one. Before the 2026-09-04 amendment this is what
    /// made a capsule default row runnable on a CI runner at all (the
    /// fresh-boot seed used to unconditionally pick `agent = "claude"`
    /// on Windows, and no `claude` binary exists on a CI runner); the
    /// fresh-boot seed is now `agent = "none"` on every host regardless
    /// (the default row's own inert-anchor default), so this helper
    /// today exists only for
    /// `capsule_row_with_an_unlaunchable_agent_reaches_terminal_and_is_destroyable`,
    /// which deliberately seeds a REAL (if unlaunchable) agent to
    /// reproduce a row that is NOT the inert anchor.
    fn seed_default_capsule_toml(&self, agent: &str) {
        let slug = sot_protocol::slug(
            self.daemon_project_root
                .file_name()
                .and_then(|n| n.to_str())
                .expect("daemon_project_root has a file name"),
        );
        self.seed_capsule_toml(
            "ws-preseeded-default",
            &slug,
            &self.daemon_project_root,
            agent,
        );
    }

    /// Linux only, used ONLY by
    /// `capsule_row_with_an_unlaunchable_agent_reaches_terminal_and_is_destroyable`:
    /// a directory containing a deliberately-broken, but genuinely
    /// resolvable+executable, `claude` script. A genuinely ABSENT
    /// `claude` does not reproduce that test's scenario the same way on
    /// Linux as it does on Windows — `agent_argv`'s own Linux resolution
    /// step (`resolve_claude`) would refuse at the DAEMON level instead,
    /// before `sot-capsule` ever gets a chance to spawn anything and run
    /// its own anti-flap/Terminal logic (see that test's own doc for the
    /// full reasoning). This script `exec`s a path that cannot possibly
    /// exist, so the shell itself fails and exits nonzero almost
    /// instantly, every single time it is spawned — exactly the
    /// "unstable leg" `sot-capsule supervise`'s own `FLAP_THRESHOLD`
    /// counts against.
    #[cfg(target_os = "linux")]
    fn seed_fake_unlaunchable_claude(&self) -> PathBuf {
        let dir = self._tmp.path().join("fakebin");
        std::fs::create_dir_all(&dir).expect("mkdir fakebin");
        let claude = dir.join("claude");
        std::fs::write(
            &claude,
            b"#!/bin/sh\nexec /no/such/binary/sot-test-unlaunchable-claude\n",
        )
        .expect("write fake claude stub");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake claude stub");
        dir
    }

    /// [`Env::spawn_sotd`], but with `prepend_dir` inserted at the FRONT
    /// of the daemon's own `PATH` — Linux only, used ONLY alongside
    /// [`Env::seed_fake_unlaunchable_claude`] to guarantee its stub
    /// resolves FIRST (`resolve_claude`'s own search order), regardless
    /// of whether a REAL `claude` also happens to be reachable on this
    /// test-runner's own `PATH`/`$HOME` — a real dev box, unlike a bare
    /// CI runner, routinely has one, and a REAL `claude` actually
    /// launching here would reach `Ready`, never the anti-flap/Terminal
    /// path the test exercises.
    #[cfg(target_os = "linux")]
    fn spawn_sotd_with_prepended_path(&self, prepend_dir: &Path) {
        let mut path = std::ffi::OsString::from(prepend_dir);
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let child = Command::new(sotd_exe())
            .arg("--socket")
            .arg(&self.socket_path)
            .arg("--project-root")
            .arg(&self.daemon_project_root)
            .env("LOCALAPPDATA", &self.state_root)
            .env("XDG_STATE_HOME", &self.state_root)
            .env("XDG_CONFIG_HOME", &self.config_root)
            .env("SOT_STATE_HOST", TEST_STATE_HOST)
            .env("SOT_RUNTIME_DIR", self._runtime_tmp.path())
            .env("SOT_TMUX_SOCK", self.tmux_sock())
            .env("PATH", path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sotd");
        let previous = self.daemon.borrow_mut().replace(child);
        debug_assert!(previous.is_none(), "spawn_sotd_with_prepended_path called while a prior daemon was still tracked");
    }

    /// LU4 review round 2, F4 (anchor tightened round 3, G2): the
    /// anchored `pgrep`/`pkill` pattern for every real leg THIS env's own
    /// daemon could ever have spawned, covering EITHER subcommand
    /// (`capsule_workspace.rs`'s own two spawn sites) against this env's
    /// own `state_root` (every workspace's own `state_dir_for` nests
    /// under it, `state_root.join("workspaces").join(workspace_id)`, so
    /// anchoring on the ROOT alone covers every row this `Env` could ever
    /// create without having to learn each workspace's own state dir as
    /// it's discovered). See [`build_leg_pgrep_pattern`] for why this
    /// anchors on the ESCAPED, EXACT executable path rather than a
    /// wildcard.
    #[cfg(target_os = "linux")]
    fn leg_pgrep_pattern(&self) -> String {
        build_leg_pgrep_pattern(&sot_capsule_exe(), "(supervise|run)", &self.state_root)
    }
}

/// LU4 review round 2, F4: kill the daemon FIRST (so its own crash-
/// restart policy can't spawn a fresh contender into a state dir this
/// impl is about to sweep), THEN sweep this env's own legs with the
/// ANCHORED pattern ([`Env::leg_pgrep_pattern`] — never the old unanchored
/// `pkill -f <state_dir>` substring match), THEN kill this env's own
/// ISOLATED tmux server (F3) — in that exact order, on EVERY exit path
/// including a panic, which a per-test teardown call can never guarantee.
/// `_tmp`/`_runtime_tmp`'s own `Drop` (temp dir removal, step 4) runs
/// automatically right after this method returns — Rust drops a value's
/// remaining fields, in declaration order, immediately after a manual
/// `Drop::drop` body finishes.
impl Drop for Env {
    fn drop(&mut self) {
        // (1) the daemon, first. `&mut self` here (not `&self`), so
        // `RefCell::get_mut` — no runtime borrow check needed, and this
        // can never race the `&self`-taking async methods above (nothing
        // else can be calling into this `Env` while it is being dropped).
        // Best-effort, unbounded-but-brief (mirrors the old `KillGuard`'s
        // own `Drop`) — the common, non-panicking path already emptied
        // this slot via `kill_daemon_bounded`, so this is a no-op there.
        if let Some(mut child) = self.daemon.get_mut().take() {
            let _ = child.kill();
            let _ = child.wait();
        }

        #[cfg(target_os = "linux")]
        {
            // (2) sweep this env's own legs, anchored, REPEATED until a
            // full pass finds nothing (G2/G3, LU4 review round 2): a
            // single `pkill` SELECTS its targets before signalling them,
            // so a supervisor process killed just now can still have
            // spawned a fresh leg a moment earlier that the same
            // selection pass never saw — the two-second follow-up this
            // used to be only ever OBSERVED that gap, never closed it.
            // Killing supervisors FIRST each pass (before their own
            // legs) means no NEW leg can be spawned after this pass's own
            // supervisor-kill lands; a leg from a supervisor killed on an
            // EARLIER pass is still caught by this pass's own `run`-kill.
            let exe = sot_capsule_exe();
            let supervise_pattern = build_leg_pgrep_pattern(&exe, "supervise", &self.state_root);
            let run_pattern = build_leg_pgrep_pattern(&exe, "run", &self.state_root);
            let combined_pattern = self.leg_pgrep_pattern();
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let _ = Command::new("pkill")
                    .arg("-9")
                    .arg("-f")
                    .arg(&supervise_pattern)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                let _ = Command::new("pkill")
                    .arg("-9")
                    .arg("-f")
                    .arg(&run_pattern)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                if !any_process_matches(&combined_pattern) || Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }

            // (3) this env's own isolated tmux server — never the
            // developer's real one (a different socket path entirely).
            let _ = Command::new("tmux")
                .arg("-S")
                .arg(self.tmux_sock())
                .arg("kill-server")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }

        // (4) `_tmp`/`_runtime_tmp` remove themselves right after this
        // method returns — see the doc comment above.
    }
}

/// Regex-escape a path for safe use inside an `-f` pattern ([`pkill`]/
/// [`pgrep`] use POSIX extended regex) — defensive: `tempfile`'s own
/// random suffixes are plain alphanumeric today, but a path is still
/// user-influenced-shaped data, not a literal we control end to end.
#[cfg(target_os = "linux")]
fn regex_escape_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The anchored `pgrep`/`pkill` pattern for a `sot-capsule` invocation:
/// `^<escaped exe path> <subcommand> <escaped state_root>`. G2 (LU4
/// review round 2): anchoring the OLD way, `^\S*sot-capsule`, silently
/// requires the character just before `sot-capsule` to be non-whitespace
/// — `\S*` cannot cross a space — so an executable path containing one
/// (a legal `CARGO_TARGET_DIR` with a space in it) never matches at all,
/// and the sweep quietly does nothing. Anchoring on the EXACT, escaped
/// executable path this suite itself resolved (`sot_capsule_exe()`) has
/// no such gap: a space in the path is not a regex metacharacter and
/// needs no escaping to match itself literally, so `regex_escape_path`
/// leaves it untouched. `subcommand` is a literal ("supervise", "run") or
/// an alternation ("(supervise|run)") — both are valid ERE on their own.
#[cfg(target_os = "linux")]
fn build_leg_pgrep_pattern(exe: &Path, subcommand: &str, state_root: &Path) -> String {
    format!("^{} {subcommand} {}", regex_escape_path(exe), regex_escape_path(state_root))
}

/// Whether any live process's command line matches `pattern` — the
/// read-only half of the anchored sweep, reused by [`Env`]'s own `Drop`
/// (to poll the sweep to completion) and by the F4 cleanup-contract test
/// below (to prove both "before" and "after").
#[cfg(target_os = "linux")]
fn any_process_matches(pattern: &str) -> bool {
    Command::new("pgrep")
        .arg("-f")
        .arg(pattern)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// G3 (LU4 review round 2): the SAME bounded, sweep-until-empty shape
/// `Env`'s own `Drop` uses for its active pkill loop, but read-only — no
/// re-signalling, since by the time a caller here needs it `Drop` has
/// already run its own loop to completion (or its own 2s bound). Exists
/// so the F4 cleanup-contract test's own "empty after" assertion is not a
/// single point-in-time check racing the exact moment `Drop`'s loop
/// itself gave up at its bound: a process that was still one syscall from
/// actually exiting when `Drop` observed its own deadline is not a real
/// leak, and re-polling here (rather than asserting instantly) is the
/// difference between a flaky false failure and a meaningful, still-
/// bounded proof.
#[cfg(target_os = "linux")]
fn poll_until_no_process_matches(pattern: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !any_process_matches(pattern) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(target_os = "linux")]
#[cfg(test)]
mod leg_pgrep_pattern_tests {
    use super::*;

    /// G2's own regression case: a space in the executable path (a legal
    /// `CARGO_TARGET_DIR` with a space in it) must still produce a
    /// pattern that matches that path LITERALLY — a space is not an ERE
    /// metacharacter, so it must pass through `regex_escape_path`
    /// untouched rather than being dropped or mis-escaped.
    #[test]
    fn build_leg_pgrep_pattern_keeps_a_literal_space_in_the_exe_path() {
        let exe = Path::new("/scratch/build target/debug/sot-capsule");
        let state_root = Path::new("/tmp/sotcw-abc123/state");
        let pattern = build_leg_pgrep_pattern(&exe, "supervise", &state_root);
        assert_eq!(
            pattern,
            r"^/scratch/build target/debug/sot-capsule supervise /tmp/sotcw-abc123/state"
        );
    }

    /// Regex metacharacters in EITHER half (`+`/`.`) must be escaped so
    /// they match themselves literally rather than being interpreted by
    /// `pkill`/`pgrep`'s own POSIX ERE engine (a stray `.` would
    /// otherwise match any single character, widening the match rather
    /// than narrowing it to this exact path).
    #[test]
    fn build_leg_pgrep_pattern_escapes_regex_metacharacters_in_both_halves() {
        let exe = Path::new("/scratch/target+build/sot-capsule");
        let state_root = Path::new("/tmp/sotcw-v1.2/state");
        let pattern = build_leg_pgrep_pattern(&exe, "run", &state_root);
        assert_eq!(
            pattern,
            r"^/scratch/target\+build/sot-capsule run /tmp/sotcw-v1\.2/state"
        );
    }
}

async fn try_connect(socket_path: &Path) -> Option<LocalStream> {
    let name = socket_path
        .to_str()
        .expect("socket path is valid UTF-8")
        .to_fs_name::<GenericFilePath>()
        .expect("interpret socket path as a local-socket name");
    // Bounded connect attempt (Codex review finding 13): a single try
    // never blocks past a small slice of the outer `poll_until` budget.
    tokio::time::timeout(Duration::from_secs(2), LocalStream::connect(name))
        .await
        .ok()
        .and_then(Result::ok)
}

type Conn = tokio::io::BufReader<LocalStream>;

/// One request/reply round trip, itself bounded (Codex review finding
/// 13): write `payload` under `op`, then read frames until one with the
/// matching `id` arrives (any `Kind::Evt` broadcast in between — e.g.
/// `workspace.created` — is skipped, exactly as a real client's
/// steady-state loop routes it aside), all within `BOUND`.
async fn call(conn: &mut Conn, id: u64, op: &str, payload: serde_json::Value) -> Frame {
    let body = async {
        codec::write_frame(conn, &Frame::req(id, op, payload), None)
            .await
            .expect("write_frame");
        loop {
            let (frame, _blob) = codec::read_frame(conn).await.expect("read_frame");
            if frame.id == id && frame.kind != Kind::Evt {
                return frame;
            }
        }
    };
    tokio::time::timeout(BOUND, body)
        .await
        .unwrap_or_else(|_| panic!("{op} (id {id}) did not reply within {BOUND:?}"))
}

/// Connect (bounded-retried — the pipe takes a moment to bind after
/// `spawn_sotd`) + hello, returning the connection and the next free
/// request id.
async fn connect_and_hello(socket_path: &Path) -> (Conn, u64) {
    let stream = poll_until(
        || async { try_connect(socket_path).await },
        BOUND,
        "sotd's local socket to accept a connection",
    )
    .await;
    let mut conn = tokio::io::BufReader::new(stream);
    let hello = HelloReq {
        client_id: "capsule-workspaces-test".to_string(),
        session_id: None,
        last_seen_revision: 0,
        token: None,
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
    };
    let reply = call(&mut conn, 1, op::HELLO, serde_json::to_value(&hello).unwrap()).await;
    assert!(reply.payload.get("error").is_none(), "hello refused: {:?}", reply.payload);
    (conn, 2)
}

fn find_row(payload: &serde_json::Value, workspace_id: &str) -> Option<serde_json::Value> {
    payload["workspaces"].as_array()?.iter().find(|w| w["workspace_id"] == workspace_id).cloned()
}

/// One bounded `query_status` attempt — `Ok` when the lane answered,
/// `None` (not an error) when it is legitimately absent/unreachable,
/// which two of this test's own polls treat as the fact they're waiting
/// for (the old supervisor going away after `stop`).
async fn try_query_status(state_dir: PathBuf) -> Option<sot_log::supervisor_client::StatusReport> {
    tokio::task::spawn_blocking(move || {
        sot_log::supervisor_client::query_status(&state_dir)
            .ok()
            .map(|(report, _process)| report)
    })
    .await
    .unwrap_or(None)
}

// A DETACHED leg this test's own row may have left running (`stop` ends
// ONLY the supervisor AUTHORITY — ADR 0041 Lifecycle, legs are
// deliberately outside the supervisor's own job — so a test whose own
// teardown calls `stop` but never a matching `end_run` for the row's
// CURRENT voyage leaves its platform-shell leg orphaned on Linux) no
// longer needs a per-test sweep call: `Env`'s own `Drop` (F4, LU4 review
// round 2) sweeps every leg this env could have spawned, anchored on its
// own `state_root`, unconditionally, on every exit path — see that impl.

/// Whether the supervisor AUTHORITY (not its capsule leg) is stopped
/// before [`restart_daemon_and_prove_adoption`] kills and relaunches the
/// daemon — the one axis that distinguishes this file's two adoption
/// scenarios.
#[derive(Debug, Clone, Copy)]
enum AuthorityAtRestart {
    /// The field bug's exact precondition (daemon-boot-adopts-a-live-
    /// supervisor fix): the authority is left ALIVE, only the daemon
    /// process itself is killed. Proves the rebooted daemon ADOPTS the
    /// still-answering lane rather than racing a competing `--resume`
    /// into its fence.
    Alive,
    /// The authority is explicitly stopped first
    /// (`sot_log::supervisor_client::stop`) — its capsule leg
    /// deliberately survives (ADR 0041 Lifecycle: legs are outside the
    /// supervisor's own job). Proves `sot-capsule`'s OWN leg-adoption of
    /// a still-alive orphaned leg behind a genuinely DEAD lane.
    Stopped,
}

/// Shared "restart the daemon and prove adoption" body for both of this
/// file's adoption scenarios (round-2 Codex finding: one helper, not two
/// near-duplicate ~150-line test bodies). Takes the state a preamble
/// (`workspace.create` + poll-to-"ready", which cannot itself be shared
/// — each test owns an independent `Env`/daemon) has already produced,
/// `authority`-conditionally stops the supervisor authority, kills and
/// relaunches the daemon, and polls the new daemon's `workspace.list`
/// for `state_dir` to keep matching and phase to NEVER read "terminal"
/// — covers BOTH scenarios' own regression (a competing spawn racing a
/// still-held fence marks terminal; a genuinely dead lane's own resume
/// should never either) — until it reaches "ready", with EXTRA
/// post-ready dwell for [`AuthorityAtRestart::Alive`] (the field bug's
/// own timing: a competing spawn's watchdog saw its contention/terminal
/// exit within a couple hundred ms, so a plain "stop at the first ready"
/// poll could exit before a DELAYED terminal-mark regression ever showed
/// up — the `Stopped` scenario's resume is a real process spawn with no
/// fence contention at risk once it reports ready, so it gets no extra
/// dwell). Finally asserts the leg epoch is UNCHANGED across the restart
/// — the proof that whichever mechanism resumed the run ADOPTED it
/// rather than spawning a fresh contender. Returns the new connection/
/// next-id so a caller (today: only the `Stopped` scenario) can continue
/// past this point on the SAME connection — the new daemon itself needs
/// no return: `env` already owns it (`Env::spawn_sotd`, F4), so a later
/// `env.kill_daemon_bounded()` at the caller's own teardown reaps it.
async fn restart_daemon_and_prove_adoption(
    env: &Env,
    conn: Conn,
    workspace_id: &str,
    state_dir: &str,
    state_dir_path: &Path,
    leg_before: u64,
    authority: AuthorityAtRestart,
) -> (Conn, u64) {
    if matches!(authority, AuthorityAtRestart::Stopped) {
        tokio::task::spawn_blocking({
            let dir = state_dir_path.to_path_buf();
            move || sot_log::supervisor_client::stop(&dir).expect("stop the supervisor authority")
        })
        .await
        .unwrap();

        poll_until(
            || {
                let dir = state_dir_path.to_path_buf();
                async move {
                    if try_query_status(dir).await.is_none() {
                        Some(())
                    } else {
                        None
                    }
                }
            },
            BOUND,
            "the stopped supervisor's own lane to go silent",
        )
        .await;
    }

    env.kill_daemon_bounded().await;
    drop(conn);

    env.spawn_sotd();
    let (mut conn2, mut next_id2) = connect_and_hello(&env.socket_path).await;

    let post_ready_dwell = match authority {
        AuthorityAtRestart::Alive => Some(Duration::from_secs(5)),
        AuthorityAtRestart::Stopped => None,
    };
    let ready_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    let mut dwell_until: Option<Instant> = None;
    loop {
        let id = next_id2;
        next_id2 += 1;
        let payload = call(&mut conn2, id, op::WORKSPACE_LIST, serde_json::json!({}))
            .await
            .payload;
        if let Some(row) = find_row(&payload, workspace_id) {
            assert_eq!(row["runtime"], "capsule", "row: {row:?}");
            assert_eq!(
                row["state_dir"].as_str(),
                Some(state_dir),
                "the resumed/adopted row's state_dir must be the SAME capsule (authority={authority:?})"
            );
            let phase = row["phase"].as_str();
            assert_ne!(
                phase,
                Some("terminal"),
                "row went terminal across the daemon restart (authority={authority:?}) -- a competing \
                 leg was spawned into a still-held fence and lost"
            );
            if phase == Some("ready") && dwell_until.is_none() {
                match post_ready_dwell {
                    Some(d) => dwell_until = Some(Instant::now() + d),
                    None => break,
                }
            }
        }
        if let Some(dl) = dwell_until {
            if Instant::now() >= dl {
                break;
            }
        } else {
            assert!(
                Instant::now() < ready_deadline,
                "timed out waiting for the resumed/adopted row to reach phase \"ready\" (authority={authority:?})"
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let leg_after = tokio::task::spawn_blocking({
        let dir = state_dir_path.to_path_buf();
        move || {
            sot_log::supervisor_client::query_status(&dir)
                .expect("query_status after restart")
                .0
                .leg
        }
    })
    .await
    .unwrap();
    assert_eq!(
        leg_after,
        Some(leg_before),
        "the leg epoch changed across the daemon restart (authority={authority:?}) -- a fresh/competing leg was spawned, not adopted"
    );

    (conn2, next_id2)
}

#[tokio::test]
async fn capsule_workspace_create_list_attach_refusal_adopt_and_destroy() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("cwl");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    // workspace.create — a SEPARATE project root from the daemon's own
    // default (finding 2). No autostart requested, so the capsule's own
    // producer is `agent_argv("none")` == the platform shell (ADR 0042
    // L1a's own fallback). `"runtime": "capsule"` is explicit — ADR 0043
    // decision 22, the field exists for exactly this.
    let create_req = serde_json::json!({
        "label": "cwl-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["tmux_session"]
        .as_str()
        .expect("tmux_session (the pty.open addressing token)")
        .to_string();

    // workspace.list: runtime "capsule", a state_dir, and — polled — phase
    // reaching "ready" (the capsule's platform-shell leg coming up and
    // the supervisor's own lane answering `status`).
    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    let state_dir = loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            assert_eq!(row["runtime"], "capsule", "row: {row:?}");
            if let (Some(sd), Some("ready")) = (row["state_dir"].as_str(), row["phase"].as_str()) {
                break sd.to_string();
            }
        }
        assert!(
            Instant::now() < list_deadline,
            "timed out waiting for workspace.list to report phase \"ready\" for the new capsule workspace"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);
    assert!(state_dir_path.is_dir(), "reported state_dir does not exist on disk: {state_dir}");

    // pty.open on a capsule workspace: refused, never proxied.
    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "target": target, "user_switch": true });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    // `next_id` has no further use on this connection (it is dropped and
    // replaced after the daemon restart below), so no further increment.
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);
    assert_eq!(
        pty_res.payload["state_dir"].as_str(),
        Some(state_dir.as_str()),
        "pty.open's attach_direct state_dir should match workspace.list's"
    );

    // --- Adoption proof (Codex review finding 13; folded round-2 into
    // the shared restart_daemon_and_prove_adoption helper below, which
    // also serves the boot-adopts-a-still-alive-supervisor test in this
    // same file) ---
    // Record the current leg epoch BEFORE stopping the supervisor
    // authority over its own lane (sot_log::supervisor_client::stop) --
    // its capsule leg is deliberately outside the supervisor's own job
    // (ADR 0041 Lifecycle) and survives.
    let leg_before = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            sot_log::supervisor_client::query_status(&dir)
                .expect("query_status before stop")
                .0
                .leg
        }
    })
    .await
    .unwrap()
    .expect("a ready capsule has a leg");

    let (mut conn2, mut next_id2) = restart_daemon_and_prove_adoption(
        &env,
        conn,
        &workspace_id,
        &state_dir,
        &state_dir_path,
        leg_before,
        AuthorityAtRestart::Stopped,
    )
    .await;

    // --- Destroy proof (Codex review finding 13) ---
    // workspace.destroy: ends the run; independently confirm via the
    // lane itself (not just the daemon's own say-so) that the record
    // actually closed BEFORE asserting the row disappears from
    // workspace.list; the state dir is NOT deleted (the record persists
    // by design).
    let destroy_req = serde_json::json!({ "workspace_id": workspace_id });
    let destroy_res = call(&mut conn2, next_id2, op::WORKSPACE_DESTROY, destroy_req).await;
    next_id2 += 1;
    assert!(destroy_res.payload.get("error").is_none(), "workspace.destroy failed: {:?}", destroy_res.payload);

    // The old "poll for phase EndedNoRespawn" expectation is obsolete:
    // `end_run`'s own wrapper now sends the (post-#184) WAITING `stop`
    // once the end is confirmed, so `workspace.destroy`'s own response
    // doesn't land until the authority has already exited (or is in
    // the process of it) — the lane goes SILENT instead of resting in
    // EndedNoRespawn, and polling for that resting phase here raced a
    // window too narrow to reliably observe (CI's own field finding).
    // Leak proof: mirror the SAME "lane goes silent after stop" idiom
    // the adoption proof above uses (`sot-capsule supervise` otherwise
    // idles in `EndedNoRespawn` forever without a `stop` request — see
    // `supervisor.rs`'s own exit-condition doc). Without this, the
    // field defect this closes reproduces exactly: one resident
    // `sot-capsule.exe` per destroy, holding `supervisor.lock` and the
    // exe, that nothing would ever reap.
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move { if try_query_status(dir).await.is_none() { Some(()) } else { None } }
        },
        BOUND,
        "the ended supervisor's own lane to go silent (workspace.destroy's end_run must also stop it)",
    )
    .await;

    let destroy_deadline = Instant::now() + BOUND;
    loop {
        let id = next_id2;
        next_id2 += 1;
        let payload = call(&mut conn2, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if find_row(&payload, &workspace_id).is_none() {
            break;
        }
        assert!(
            Instant::now() < destroy_deadline,
            "timed out waiting for the destroyed workspace to disappear from workspace.list"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        state_dir_path.is_dir(),
        "the capsule's state dir must survive workspace.destroy (the record persists by design): {state_dir}"
    );

    env.kill_daemon_bounded().await;
}

/// ADR 0030 §8 decision 31c (cross-referenced as ADR 0043 decision 31):
/// compiles a SECOND `sot-capsule` with a genuinely different
/// `SUPERVISOR_LANE_BUILD_ID`, for [`phase_of_reports_foreign_when_the_lane_refuses_a_different_build`]
/// below. That id is baked in at COMPILE time from `git rev-parse HEAD`
/// (`rust/log/build.rs`) — reusing THIS checkout's own build (as every
/// other test in this file does via [`sot_capsule_exe`]) can therefore
/// never produce a mismatch; only a genuinely separate build can. Two
/// env vars make `build.rs` take its "no git" branch without touching
/// this checkout at all: `GIT_DIR` points at a path that doesn't exist,
/// so every `git` call in `build.rs` fails closed (`Command::output()`
/// succeeds as a `Result`, but the process itself exits non-zero, which
/// `build.rs`'s own `git()` helper already treats as `None`) — proven by
/// hand: `GIT_DIR=/nonexistent git rev-parse HEAD` exits 128 with no
/// stdout; `SOT_BUILD_ID` then supplies the (deliberately different)
/// identity `build.rs` falls back to in exactly that case. A fresh
/// `--target-dir` under the system temp dir, never this test's own
/// `CARGO_TARGET_DIR`: reusing the real one would let `SOT_BUILD_ID`'s
/// `cargo:rerun-if-env-changed` force a rebuild of the REAL `sot-capsule`/
/// `sotd` this whole file's other tests depend on, with the FOREIGN
/// identity baked in — corrupting every test that runs after this one.
/// Only `-p sot-log --bin sot-capsule` (not the whole workspace) — the
/// daemon (`sot-backend`) is never rebuilt here, only the one binary this
/// test needs a foreign copy of. Slower than every other helper in this
/// file (a real nested compile of `sot-log` and its own small dependency
/// set) — bounded, never unbounded.
#[cfg(target_os = "linux")]
async fn build_foreign_sot_capsule() -> (PathBuf, tempfile::TempDir) {
    let target_tmp = tempfile::Builder::new()
        .prefix("sotcw-foreign-target-")
        .tempdir_in(std::env::temp_dir())
        .expect("foreign target tempdir");
    let target_path = target_tmp.path().to_path_buf();
    let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rust/backend has a parent directory (rust/)")
        .join("Cargo.toml");
    let build_id = format!("test-foreign-build-{}", std::process::id());

    let status = {
        let target_path = target_path.clone();
        let manifest_path = manifest_path.clone();
        let build_id = build_id.clone();
        tokio::time::timeout(
            Duration::from_secs(300),
            tokio::task::spawn_blocking(move || {
                Command::new("cargo")
                    .arg("build")
                    .arg("--manifest-path")
                    .arg(&manifest_path)
                    .arg("-p")
                    .arg("sot-log")
                    .arg("--bin")
                    .arg("sot-capsule")
                    .arg("--target-dir")
                    .arg(&target_path)
                    .env("GIT_DIR", target_path.join("no-such-git-dir"))
                    .env("SOT_BUILD_ID", &build_id)
                    .stdin(Stdio::null())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .status()
                    .expect("spawn cargo build for the foreign sot-capsule")
            }),
        )
        .await
        .expect("nested cargo build for the foreign sot-capsule exceeded its 300s bound")
        .unwrap()
    };
    assert!(status.success(), "nested cargo build for the foreign sot-capsule failed");

    let exe = target_path.join("debug").join(CAPSULE_EXE_NAME);
    assert!(exe.is_file(), "foreign sot-capsule not found at {exe:?} after the nested build");

    // Prove the mismatch is real (not this checkout's own commit sneaking
    // back in some other way) before this test ever trusts it.
    let reported = Command::new(&exe)
        .arg("build-id")
        .output()
        .expect("run the foreign sot-capsule's own build-id subcommand");
    let reported_id = String::from_utf8_lossy(&reported.stdout).trim().to_string();
    assert_eq!(
        reported_id, build_id,
        "the nested build did not embed the SOT_BUILD_ID override -- git was not actually made unreachable"
    );
    assert_ne!(
        reported_id,
        sot_log::exchange::SUPERVISOR_LANE_BUILD_ID,
        "the foreign build's id must differ from this test binary's own"
    );
    (exe, target_tmp)
}

/// ADR 0030 §8 decision 31c (cross-referenced as ADR 0043 decision 31):
/// `phase_of` reports `"foreign"` — never `"unreachable"` — for a capsule
/// row whose supervisor lane answered but refused THIS daemon's build
/// (`version_skew`). Reproduces the field incident exactly (a launcher
/// pair rebuild left a pinned `sot-capsule` behind — `pair_verdict`'s own
/// doc): a REAL supervisor (this checkout's own build) starts a real
/// run, its AUTHORITY alone is stopped (the leg survives, ADR 0041
/// Lifecycle), and a supervisor compiled with a genuinely different
/// build id ([`build_foreign_sot_capsule`]) resumes the SAME state dir,
/// bypassing the daemon's own spawn path entirely — `check_pair`/
/// `pair_verdict` would refuse to spawn this pair itself, exactly why the
/// field incident is a row an operator finds ALREADY held foreign, never
/// one this daemon started that way.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn phase_of_reports_foreign_when_the_lane_refuses_a_different_build() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let (foreign_exe, _foreign_target) = build_foreign_sot_capsule().await;

    let env = Env::new("foreign");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "foreign-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

    // Same "wait for ready" preamble as `capsule_workspace_create_list_attach_refusal_adopt_and_destroy`
    // — a real supervisor of THIS checkout's own build must be up and
    // answering before anything about "foreign" can mean anything.
    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    let state_dir = loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            assert_eq!(row["runtime"], "capsule", "row: {row:?}");
            if let (Some(sd), Some("ready")) = (row["state_dir"].as_str(), row["phase"].as_str()) {
                break sd.to_string();
            }
        }
        assert!(
            Instant::now() < list_deadline,
            "timed out waiting for workspace.list to report phase \"ready\" for the new capsule workspace"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);

    // Stop the REAL supervisor authority — its leg survives (ADR 0041
    // Lifecycle: legs are outside the supervisor's own job) — and wait
    // for the lane to actually go silent before resuming into the same
    // fence with the FOREIGN build (mirrors `restart_daemon_and_prove_adoption`'s
    // own `AuthorityAtRestart::Stopped` scenario).
    tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::stop(&dir).expect("stop the real supervisor authority")
    })
    .await
    .unwrap();
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move {
                if try_query_status(dir).await.is_none() {
                    Some(())
                } else {
                    None
                }
            }
        },
        BOUND,
        "the stopped supervisor's own lane to go silent",
    )
    .await;

    // Resume the SAME state dir with the FOREIGN build, directly — never
    // through the daemon (which would refuse this pair outright via
    // `check_pair`). Local fence-adoption of the surviving leg is build-
    // id-agnostic (only the WIRE hello checks it), so this succeeds
    // locally; only THIS daemon's own probe of it is refused.
    let foreign = Command::new(&foreign_exe)
        .arg("supervise")
        .arg(&state_dir_path)
        .arg("--resume")
        .arg("--survival")
        .arg("normal")
        .arg("--assume-no-rollback-target")
        .arg("--")
        .arg("/bin/sh")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn the foreign sot-capsule supervise");

    let phase_deadline = Instant::now() + BOUND.max(Duration::from_secs(60));
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            assert_eq!(row["runtime"], "capsule", "row: {row:?}");
            assert_ne!(
                row["phase"].as_str(),
                Some("terminal"),
                "row went terminal instead of foreign: {row:?}"
            );
            if row["phase"].as_str() == Some("foreign") {
                break;
            }
        }
        assert!(
            Instant::now() < phase_deadline,
            "timed out waiting for workspace.list to report phase \"foreign\" for the foreign-build row"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Teardown: this test's own foreign process is invisible to `Env`'s
    // own leg sweep (that pgreps for the REAL sot-capsule exe path, never
    // this test's throwaway one) — kill it directly. The surviving leg
    // (the platform shell the ORIGINAL, real-build supervisor spawned) IS
    // caught by that sweep, anchored on `env`'s own `state_root`.
    kill_and_wait_bounded(foreign).await;
    env.kill_daemon_bounded().await;
}

/// ADR 0042 amendment (2026-09-07), "a session types into and reads a
/// sibling row": the daemon-side proof that `pty.input`/`pty.screen`
/// actually reach a real capsule row over the wire, end to end — the
/// `sot_log::fe_client_io` mechanics themselves are proven directly in
/// `rust/log/tests/fe_client.rs`'s own `headless_*` tests; this test's
/// job is only "does the WIRE OP reach that machinery and answer
/// correctly for a real daemon."
#[tokio::test]
async fn capsule_pty_input_and_screen_reach_a_real_row_and_leave_the_lane_clean() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("pis");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "pis-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            if row["phase"].as_str() == Some("ready") {
                break;
            }
        }
        assert!(Instant::now() < list_deadline, "timed out waiting for the new capsule workspace to reach \"ready\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // `pty.screen` BEFORE any write: must succeed (a watcher attach, not
    // a refusal) and must NOT take the pen — proven below once a real
    // driving client's first input succeeds without contention.
    let pre_screen_req = serde_json::json!({ "workspace_id": workspace_id });
    let pre_screen_res = call(&mut conn, next_id, op::PTY_SCREEN, pre_screen_req).await;
    next_id += 1;
    assert!(
        pre_screen_res.payload.get("error").is_none(),
        "pty.screen before any write failed: {:?}",
        pre_screen_res.payload
    );
    assert_eq!(pre_screen_res.payload["runtime"], "capsule");

    // `pty.input`, base64("echo sot-lu6c-marker") + a separate Enter.
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let text = "echo sot-lu6c-marker";
    let input_req = serde_json::json!({
        "workspace_id": workspace_id,
        "data_b64": STANDARD.encode(text.as_bytes()),
        "enter": true,
        "origin": "lu6c-test",
    });
    let input_res = call(&mut conn, next_id, op::PTY_INPUT, input_req).await;
    next_id += 1;
    assert!(input_res.payload.get("error").is_none(), "pty.input failed: {:?}", input_res.payload);
    assert_eq!(input_res.payload["ok"], true);
    assert_eq!(input_res.payload["runtime"], "capsule");
    assert_eq!(input_res.payload["bytes"].as_u64(), Some(text.len() as u64));

    // Poll `pty.screen` until the echoed marker shows up.
    let screen_deadline = Instant::now() + Duration::from_secs(10);
    let final_screen = loop {
        let id = next_id;
        next_id += 1;
        let screen_req = serde_json::json!({ "workspace_id": workspace_id });
        let res = call(&mut conn, id, op::PTY_SCREEN, screen_req).await;
        assert!(res.payload.get("error").is_none(), "pty.screen failed: {:?}", res.payload);
        let lines: Vec<String> = res.payload["lines"]
            .as_array()
            .expect("lines array")
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect();
        if lines.iter().any(|l| l.contains("sot-lu6c-marker")) {
            break res.payload;
        }
        assert!(
            Instant::now() < screen_deadline,
            "timed out waiting for the echoed marker to appear on screen; last lines: {lines:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert!(final_screen["cols"].as_u64().unwrap_or(0) > 0, "cols must be the capsule's real geometry");
    assert!(final_screen["rows"].as_u64().unwrap_or(0) > 0, "rows must be the capsule's real geometry");
    assert!(final_screen["cursor"].is_object(), "cursor must be Some for a healthy row: {final_screen:?}");

    // The daemon's own headless client must have left the lane CLEAN:
    // a fresh `FeAttachClient` from the test itself reaches its own
    // checkpoint (proving the row is not wedged), and a real keystroke
    // from it is accepted WITHOUT any contention artifact left behind —
    // proving the earlier `pty.screen` (a watcher) never took the pen
    // either.
    let state_dir = crate::state_dir_from_list(&mut conn, &mut next_id, &workspace_id).await;
    let (_woke, wake) = wake_flag_for_test();
    let mut test_client = sot_log::fe_client_io::FeAttachClient::<sot_log::client::PlatformEndpoint>::attach(
        state_dir,
        80,
        24,
        "lu6c-test-post-check".to_string(),
        "lu6c-test-post-check".to_string(),
        None,
        wake,
    )
    .expect("attach a fresh FeAttachClient after the daemon's own headless ops");
    let checkpoint_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        test_client.pump();
        if test_client.is_checkpointed() {
            break;
        }
        assert!(!test_client.is_dead(), "post-check client died before a checkpoint: {}", test_client.status_line());
        assert!(Instant::now() < checkpoint_deadline, "post-check client never reached a checkpoint — the lane may be wedged");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    test_client.send_input(b"echo sot-lu6c-postcheck-marker\r\n");
    let took_pen_deadline = Instant::now() + Duration::from_secs(30);
    let got_marker = loop {
        test_client.pump();
        let (rows, cols) = test_client.screen().size();
        let mut text = String::new();
        for r in 0..rows {
            for c in 0..cols {
                if let Some(cell) = test_client.screen().cell(r, c) {
                    text.push_str(cell.contents());
                }
            }
        }
        if text.contains("sot-lu6c-postcheck-marker") {
            break true;
        }
        if Instant::now() >= took_pen_deadline {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        got_marker,
        "the pen was not free for a fresh client after the daemon's own pty.input/pty.screen ops (dead={}, status={})",
        test_client.is_dead(),
        test_client.status_line()
    );
    drop(test_client);

    env.kill_daemon_bounded().await;
}

/// One `workspace.list` round trip's `state_dir` for `workspace_id` —
/// factored out of the big test above so its own long body reads as one
/// story rather than three inlined polls of the same shape.
async fn state_dir_from_list(conn: &mut Conn, next_id: &mut u64, workspace_id: &str) -> PathBuf {
    let id = *next_id;
    *next_id += 1;
    let payload = call(conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    let row = find_row(&payload, workspace_id).expect("workspace.list row for this workspace_id");
    PathBuf::from(row["state_dir"].as_str().expect("state_dir"))
}

/// Local copy of `fe_client.rs`'s own `wake_flag` helper (a separate test
/// binary; not worth a shared dependency for four lines).
fn wake_flag_for_test() -> (std::sync::Arc<std::sync::atomic::AtomicBool>, Box<dyn Fn() + Send + 'static>) {
    let woke = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let woke2 = std::sync::Arc::clone(&woke);
    (woke, Box::new(move || woke2.store(true, std::sync::atomic::Ordering::Relaxed)))
}

/// 2026-09-04 amendment (owner ruling): the daemon's own default/home
/// row is now an INERT ANCHOR when it carries no agent (`agent ==
/// "none"`) — the workspace it falls back to and the way to browse this
/// machine's files, never a session. This supersedes the old
/// `capsule_default_workspace_starts_its_supervisor_on_first_attach`
/// (which this test replaces): before this amendment, `pty.open`'s
/// start-on-attach spawned a supervisor for this row unconditionally
/// (the v0.6.0-rc.2 field fix below); now it must NOT, specifically
/// because its agent is "none" and it is the daemon's default. Proves
/// the inverse of the old claim: `pty.open` still answers
/// `attach_direct` (the SAME response every capsule row gets, never a
/// special error — see `server.rs`'s own `pty.open` handler), but
/// nothing is ever spawned behind it — no state dir, no lane, the row's
/// own phase never leaves "stopped".
/// `capsule_created_workspace_starts_on_attach_and_recovers_via_reset_after_end`
/// (below) is where the "start-on-attach actually spawns something"
/// proof now lives, on an ordinary row.
///
/// (v0.6.0-rc.2 field finding, for context: the daemon's own default/home
/// workspace is registered with `runtime: "capsule"` at startup, but
/// `workspace.create` was the ONLY path that ever spawned a capsule's
/// supervisor — this row was never created through it, so it never got
/// one, and selecting it in the frontend parked it on an empty pane
/// forever. Start-on-attach closed that gap for every capsule row
/// generally; this amendment carves the DEFAULT-with-no-agent row back
/// out of it specifically.)
///
/// Windows-only (ADR 0043 decision 22): the default row's own STEADY
/// STATE is `runtime = "capsule"` only on Windows — the Linux platform
/// default stays "tmux" until the bridge, so this scenario (a capsule
/// DEFAULT row) is not a real day-to-day Linux configuration yet.
#[tokio::test]
#[cfg(windows)]
async fn capsule_default_workspace_with_no_agent_is_never_started_on_attach() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("dna");
    // Pre-write the default row's own toml as the INERT-ANCHOR agent,
    // "none" — the exact shape a fresh-boot default row seeds today on
    // EVERY host (server.rs's 2026-09-04 amendment). Pre-writing it here
    // keeps this test's precondition explicit and independent of that
    // default ever changing again.
    env.seed_default_capsule_toml("none");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let default_row = list_payload["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .find(|w| w["is_default"].as_bool() == Some(true))
        .cloned()
        .expect("a default workspace row");
    assert_eq!(default_row["runtime"], "capsule", "default row: {default_row:?}");
    assert_eq!(default_row["agent"], "none", "default row: {default_row:?}");
    let default_workspace_id = default_row["workspace_id"].as_str().expect("workspace_id").to_string();
    let default_target = default_row["tmux_session"].as_str().expect("tmux_session").to_string();

    // Same path arithmetic `capsule_workspace::state_dir_for` uses:
    // `<LOCALAPPDATA>\sot\workspaces\<workspace_id>` — `env.state_root` IS
    // the LOCALAPPDATA value this daemon was launched with (see
    // `Env::spawn_sotd`).
    let state_dir_path = env.state_root.join("sot").join("workspaces").join(&default_workspace_id);

    // Precondition: no state dir on disk at all, and `workspace.list`'s
    // own row already reads "stopped" from THIS first list call (rule B:
    // the startup resume-scan skips every row with no published voyage
    // pointer, so it never touched this one either).
    assert!(
        !state_dir_path.exists(),
        "the default capsule's state dir must not exist before this test's own attach: {state_dir_path:?}"
    );
    assert_eq!(
        default_row["phase"].as_str(),
        Some("stopped"),
        "the default row's phase must read \"stopped\" before its first attach: {default_row:?}"
    );

    // `target` MUST be the row's own `tmux_session` — a targetless
    // `pty.open` addresses the drawer's own special SoT LLM terminal
    // (`pty::DEFAULT_TMUX_TARGET` == "sot-llm"), never a workspace row;
    // `server.rs`'s `workspace_for_tmux(requested_target)` only resolves
    // to this row when `target` matches its `tmux_session`. This is
    // exactly what the frontend sends attaching a capsule row — though
    // in practice the frontend never sends it for THIS row at all
    // (2026-09-04's own frontend-side filter, tested separately in
    // `gpu.rs`); this is belt-and-suspenders coverage of the backend
    // guard alone.
    let pty_req = serde_json::json!({
        "cols": 80, "rows": 24, "user_switch": true, "target": default_target,
    });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);

    // Dwell across a window comfortably longer than every OTHER
    // start-on-attach proof in this file needs to first observe its own
    // state dir/lane — if the anchor rule regressed and a supervisor
    // silently started anyway, this window is generous enough to catch
    // it; asserted continuously throughout, never just once at the end.
    let never_started_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < never_started_deadline {
        assert!(
            !state_dir_path.exists(),
            "the default row's agent-none anchor must never spawn a supervisor on attach: {state_dir_path:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        try_query_status(state_dir_path.clone()).await.is_none(),
        "the default row's agent-none anchor's lane must never answer after an attach attempt"
    );

    // `workspace.list` must still read "stopped" — never "starting" or
    // "ready".
    let payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    let row = find_row(&payload, &default_workspace_id).expect("default row still listed");
    assert_eq!(
        row["phase"].as_str(),
        Some("stopped"),
        "the default row's agent-none anchor must still read \"stopped\" after an attach attempt: {row:?}"
    );

    env.kill_daemon_bounded().await;
}

/// 2026-09-04 amendment: the default row's own "never touched by
/// `workspace.create`" precondition no longer proves `pty.open`'s
/// start-on-attach actually spawns anything — that row is now the inert
/// anchor by design (see
/// `capsule_default_workspace_with_no_agent_is_never_started_on_attach`,
/// above, which this test's predecessor was split into). This moves
/// that proof, plus the #182 items A.1/C end -> reattach -> new-voyage-
/// via-reset proof, onto an ORDINARY (non-default) capsule row instead —
/// pre-seeded via `Env::seed_capsule_toml` rather than
/// `workspace.create`, since `workspace.create`'s own handler spawns
/// synchronously as part of creation and so never leaves a row in the
/// "registered but never started" state start-on-attach needs to prove
/// anything at all. Seeded with the placeholder `agent = "none"` —
/// unchanged and intended: the inert-anchor rule is scoped to the
/// DEFAULT row specifically (ADR 0042's amendment), so an ordinary row
/// with no agent still runs the same `agent_argv("none")` == the
/// platform shell placeholder every other created-workspace test in
/// this file relies on.
///
/// The #182 proof itself can't route through `workspace.destroy` here
/// the way the old default-row test did — that op only KEEPS a row's
/// registry entry for the DEFAULT workspace
/// (`handle_workspace_destroy`'s own doc: "the default workspace's ROW
/// is never destroyed here"); on a NON-default row it actually REMOVES
/// the registry entry once the run is confirmed ended, which would
/// delete the very row this test needs to re-attach to. Ends the run
/// directly over the lane instead (`sot_log::supervisor_client::end_run`
/// + `stop` — the same primitives `capsule_workspace::end_run`, the
/// daemon's OWN wrapper `workspace.destroy` calls, itself uses),
/// leaving the row fully registered and untouched; `pty.open`'s
/// start-on-attach (`ensure_started`) then discovers the durable end
/// marker on its very next attach and mints a new voyage via `reset` —
/// the exact mechanic this proves, regardless of which row it runs on.
#[tokio::test]
async fn capsule_created_workspace_starts_on_attach_and_recovers_via_reset_after_end() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("car");
    // Pre-seed an ORDINARY (non-default) capsule row — never touched by
    // `workspace.create`, so its supervisor has never been spawned.
    env.seed_capsule_toml(
        "ws-preseeded-extra",
        "extra",
        &env.workspace_project_root,
        "none",
    );
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let row = find_row(&list_payload, "ws-preseeded-extra").expect("the pre-seeded row is registered");
    assert_eq!(row["runtime"], "capsule", "row: {row:?}");
    let workspace_id = row["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = row["tmux_session"].as_str().expect("tmux_session").to_string();

    // Same path arithmetic `capsule_workspace::state_dir_for` uses.
    let state_dir_path = env.state_root.join("sot").join("workspaces").join(&workspace_id);

    // Rule H: prove the "never started" precondition BEFORE `pty.open` —
    // no state dir on disk at all, and `workspace.list`'s own row already
    // reads "stopped" from THIS first list call (rule B: the startup
    // resume-scan skips every row with no published voyage pointer, so
    // it never touched this one).
    assert!(
        !state_dir_path.exists(),
        "the pre-seeded row's state dir must not exist before its first attach: {state_dir_path:?}"
    );
    assert_eq!(
        row["phase"].as_str(),
        Some("stopped"),
        "the pre-seeded row's phase must read \"stopped\" before its first attach: {row:?}"
    );

    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": target });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);
    let expected_state_dir = state_dir_path.to_string_lossy().into_owned();
    assert_eq!(
        pty_res.payload["state_dir"].as_str(),
        Some(expected_state_dir.as_str()),
        "pty.open's attach_direct state_dir should be this row's own capsule state dir"
    );

    // The state dir appears on disk — start-on-attach actually spawned
    // something, not just answered a stale path.
    let dir_deadline = Instant::now() + BOUND;
    while !state_dir_path.is_dir() {
        assert!(
            Instant::now() < dir_deadline,
            "timed out waiting for the pre-seeded row's state dir to appear: {state_dir_path:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // The lane answers — a real supervisor authority is listening, not
    // just an empty directory left behind by a partial spawn.
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move { try_query_status(dir).await }
        },
        BOUND,
        "the pre-seeded row's supervisor lane to answer a status query",
    )
    .await;

    // The row reaches Ready (leg spawned, ConPTY up, challenge proven)
    // before this test ends its run.
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move {
                let report = try_query_status(dir).await?;
                (report.phase == sot_log::wire::SupervisorPhase::Ready).then_some(())
            }
        },
        BOUND,
        "the pre-seeded row's supervisor to reach phase Ready",
    )
    .await;

    // --- #182 items A.1/C: end the run directly over the lane (never
    // `workspace.destroy` — see this test's own doc for why), then
    // prove attach recovers it via `reset` with a NEW voyage (not the
    // old flat refusal, and not a resurrected ended one) ---
    let (original_status, _process) = sot_log::supervisor_client::query_status(&state_dir_path)
        .expect("query_status before ending the run");
    let original_voyage = original_status
        .voyage
        .expect("a ready capsule has a voyage");

    sot_log::supervisor_client::end_run(&state_dir_path, &original_voyage, "test end")
        .expect("end_run over the lane");
    // Mirrors `capsule_workspace::end_run`'s own follow-up (the daemon's
    // wrapper `workspace.destroy` calls): a confirmed end still leaves
    // the authority itself running until an explicit `stop`.
    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::stop(&dir)
    })
    .await;

    // Item A.1: the authority must already be gone before this test
    // re-attaches — same leak-proof idiom the create/destroy test uses.
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move {
                if try_query_status(dir).await.is_none() {
                    Some(())
                } else {
                    None
                }
            }
        },
        BOUND,
        "the ended row's supervisor lane to go silent",
    )
    .await;

    // Re-attach, repeatedly and bounded, until the row is genuinely
    // live again with a NEW voyage — item C, the claim this test
    // proves. Never assert a specific attach count or an intermediate
    // phase: `ensure_started`'s own inline settle loop after a resume
    // spawn usually catches a marker-only recovery's near-instant
    // `EndedNoRespawn` transition and resets it within the FIRST
    // re-attach, entirely inside that one `pty.open` round trip
    // (`reset` itself polls to completion before `ensure_started`
    // returns) — so `EndedNoRespawn` is often never independently
    // observable from here at all. A slower settle just needs one more
    // attach once it lands; repeated attaches are harmless either way
    // (a `Resetting`/already-live authority answers "already up",
    // nothing to do).
    let ready_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    let new_voyage = loop {
        let reattach_req = serde_json::json!({
            "cols": 80, "rows": 24, "user_switch": true, "target": target,
        });
        let reattach_res = call(&mut conn, next_id, op::PTY_OPEN, reattach_req).await;
        next_id += 1;
        assert_eq!(
            reattach_res.payload["code"], "attach_direct",
            "re-attach after end: {:?}",
            reattach_res.payload
        );
        if let Some(report) = try_query_status(state_dir_path.clone()).await {
            if report.phase == sot_log::wire::SupervisorPhase::Ready {
                break report.voyage.expect("a ready capsule has a voyage");
            }
        }
        assert!(
            Instant::now() < ready_deadline,
            "timed out waiting for the pre-seeded row to recover via reset after being ended"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_ne!(
        new_voyage, original_voyage,
        "reset must mint a NEW voyage, not resurrect the ended one"
    );

    // Rule H: the spawned supervisor's OWN leg is DETACHED (spawned by
    // the daemon, survives the daemon's own exit by design, ADR 0042
    // L1a), so killing the daemon below does NOT reap it and it would
    // otherwise leak past this test. There is no `std::process::Child`
    // for it here (the daemon owns the actual spawn), so this stops it
    // over its own lane instead — the same
    // `sot_log::supervisor_client::stop` the create-test's own adoption
    // proof uses. Best-effort: the AUTHORITY is gone either way; its
    // detached leg survives on Linux and is swept by `Env`'s own `Drop`
    // (F4) once this test's own `env` goes out of scope below.
    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::stop(&dir)
    })
    .await;

    env.kill_daemon_bounded().await;
}

/// Field finding (a Windows FE box, 2026-09): the daemon boot resume-scan
/// (`resume_all`) used to skip straight to spawning `--resume` for every
/// capsule row with a published pointer, with NO probe of whether that
/// row's supervisor was already alive. A capsule supervisor is spawned
/// DETACHED (ADR 0042) and survives its daemon by design, so an FE
/// relaunch that reboots the LOCAL daemon left every existing supervisor
/// running — and the rebooted daemon's `resume_all` then raced a brand
/// new `--resume` leg straight into the still-live `supervisor.lock`
/// fence. `sot-capsule supervise` failed that fence acquisition FAST
/// (`crate::fence::lock_supervisor`, `rust/log/src/supervisor.rs`) and
/// (round-1 of this fix) exited `EXIT_TERMINAL` (69) within a couple
/// hundred ms; the daemon's watchdog treated 69 as unconditionally
/// terminal (rule F — never re-diagnosed) and marked the row
/// `capsule_terminal`, so `workspace.list` reported the row PERMANENTLY
/// terminal even though the OLD supervisor — the one actually running
/// the FE's attached session — never stopped. Round 2 additionally gave
/// fence contention its own exit code (`EXIT_CONTENDED`, 70, distinct
/// from terminal) for the narrower race a pre-spawn probe alone cannot
/// close (the old lane going quiet before its fence actually releases)
/// — this test's own scenario never reaches that path at all, since
/// `resume_all`'s probe here finds the lane still answering and adopts
/// it directly, spawning nothing.
///
/// Unlike this file's own `..._adopt_and_destroy` test above (which
/// deliberately STOPS the supervisor authority before restarting the
/// daemon, proving `sot-capsule`'s own leg-adoption on a genuinely dead
/// lane), this test leaves the supervisor authority ALIVE across the
/// daemon restart — the field bug's exact precondition. It proves the
/// fix: the reboot must ADOPT the still-answering lane (no second leg
/// spawned, same leg epoch, and the row's reported phase stays whatever
/// the live supervisor actually reports) rather than ever reading
/// "terminal" for a workspace nothing has failed.
#[tokio::test]
async fn capsule_workspace_boot_adopts_a_still_alive_supervisor_without_spawning_a_second_one() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("bas");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "bas-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(
        create_res.payload.get("error").is_none(),
        "workspace.create failed: {:?}",
        create_res.payload
    );
    let workspace_id = create_res.payload["workspace_id"]
        .as_str()
        .expect("workspace_id")
        .to_string();

    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    let state_dir = loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({}))
            .await
            .payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            assert_eq!(row["runtime"], "capsule", "row: {row:?}");
            if let (Some(sd), Some("ready")) = (row["state_dir"].as_str(), row["phase"].as_str()) {
                break sd.to_string();
            }
        }
        assert!(
            Instant::now() < list_deadline,
            "timed out waiting for workspace.list to report phase \"ready\" for the new capsule workspace"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);

    let leg_before = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            sot_log::supervisor_client::query_status(&dir)
                .expect("query_status before daemon restart")
                .0
                .leg
        }
    })
    .await
    .unwrap()
    .expect("a ready capsule has a leg");

    // The key difference from the create/list/attach/adopt/destroy
    // test's own adoption proof (which stops the authority first): here
    // it is deliberately left ALIVE across the restart — the field
    // bug's exact precondition — reproduced via the SAME
    // restart_daemon_and_prove_adoption helper that test uses (round-2
    // fold: this test's own "never terminal, same leg epoch" regression
    // proof is now that shared helper's `Alive` arm; nothing past this
    // point needs `conn2`/`next_id2`, so both are discarded).
    let (_conn2, _next_id2) = restart_daemon_and_prove_adoption(
        &env,
        conn,
        &workspace_id,
        &state_dir,
        &state_dir_path,
        leg_before,
        AuthorityAtRestart::Alive,
    )
    .await;

    // Best-effort stop of the still-detached supervisor (see the
    // default-workspace test's own comment above) — this test never
    // spawned a second leg to worry about, only the one adopted one.
    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::stop(&dir)
    })
    .await;

    env.kill_daemon_bounded().await;
}

/// Round-2 Codex finding: the pre-spawn probe alone (the boot-adopts
/// test above) cannot close the narrower race where the OLD lane has
/// already gone quiet but its fence has not yet released (`sot-capsule
/// supervise` drops its lane BEFORE releasing `supervisor.lock` — up to
/// `transport::TEARDOWN_AGGREGATE_DEADLINE`, 20s). A spawn that starts
/// into that window must exit `EXIT_CONTENDED` (70), never
/// `EXIT_TERMINAL` (69), and the daemon's watchdog must re-probe for
/// adoption rather than immediately marking the row `capsule_terminal`.
///
/// This test creates the contention DIRECTLY — no timing race needed —
/// using a "fake lock holder": `sot_log::fence::lock_supervisor` is
/// `pub`, so this test pre-holds `supervisor.lock` at a workspace's
/// state dir from THIS TEST PROCESS itself, a real cross-process kernel
/// lock that `supervise_inner` acquires as its very FIRST act — BEFORE
/// it ever consults `--start` vs `--resume` (`rust/log/src/supervisor.rs`)
/// — so the contention this proves is identical whichever mode the next
/// spawn uses.
///
/// 2026-09-04 amendment: no longer the DEFAULT workspace. Before this
/// amendment, the default row's fixed, known-ahead-of-boot identity gave
/// this test a state dir that was pointer-free before its own first
/// attach — the one way to pre-fence a workspace before its FIRST spawn,
/// since a created workspace's `workspace_id` (hence its state dir) is
/// only known AFTER `workspace.create` returns, and that handler spawns
/// synchronously as part of creation itself, too late to pre-fence. The
/// default row is now the inert anchor when it has no agent (see
/// `capsule_default_workspace_with_no_agent_is_never_started_on_attach`)
/// and can no longer be used this way. This test instead creates an
/// ordinary row, lets it reach Ready once normally, STOPS its authority
/// (the leg, and the run's own published pointer, both survive — ADR
/// 0041 Lifecycle), THEN pre-fences its now-EXISTING state dir: the next
/// attach needs a RESUME spawn (pointer published, lane dead) rather
/// than a fresh Start, but the fence check above runs before mode is
/// ever consulted either way, so this is the exact same contention this
/// test always proved, just reached via `--resume` instead of `--start`.
#[tokio::test]
async fn capsule_supervisor_spawn_survives_fence_contention_without_marking_terminal() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("cnt");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "cnt-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(
        create_res.payload.get("error").is_none(),
        "workspace.create failed: {:?}",
        create_res.payload
    );
    let workspace_id = create_res.payload["workspace_id"]
        .as_str()
        .expect("workspace_id")
        .to_string();
    let target = create_res.payload["tmux_session"]
        .as_str()
        .expect("tmux_session")
        .to_string();

    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    let state_dir = loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({}))
            .await
            .payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            assert_eq!(row["runtime"], "capsule", "row: {row:?}");
            if let (Some(sd), Some("ready")) = (row["state_dir"].as_str(), row["phase"].as_str()) {
                break sd.to_string();
            }
        }
        assert!(
            Instant::now() < list_deadline,
            "timed out waiting for workspace.list to report phase \"ready\" for the new capsule workspace"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);

    // Stop the supervisor authority over its own lane — its capsule leg
    // and the run's own published pointer both survive (ADR 0041
    // Lifecycle) — so the NEXT attach needs a RESUME spawn, straight
    // into the fence this test is about to pre-hold.
    tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::stop(&dir).expect("stop the supervisor authority")
    })
    .await
    .unwrap();
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move { if try_query_status(dir).await.is_none() { Some(()) } else { None } }
        },
        BOUND,
        "the stopped supervisor's own lane to go silent",
    )
    .await;

    // The fake lock holder itself: held for this test's whole remaining
    // body, released only at the very end. The state dir already exists
    // (the earlier real spawn created it) — no `create_dir_all` needed.
    let fake_lock = sot_log::fence::lock_supervisor(&state_dir_path)
        .expect("pre-hold the fence from the test process");

    let pty_req = serde_json::json!({
        "cols": 80, "rows": 24, "user_switch": true, "target": target,
    });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(
        pty_res.payload["code"], "attach_direct",
        "pty.open payload: {:?}",
        pty_res.payload
    );

    // Poll workspace.list across a window comfortably longer than the
    // daemon's own contention-retry bound (private to
    // capsule_workspace.rs, ~25s) — the row must NEVER read "terminal"
    // (this test's own regression proof) throughout.
    let observe_deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() < observe_deadline {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({}))
            .await
            .payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            assert_ne!(
                row["phase"].as_str(),
                Some("terminal"),
                "a spawn that lost a contended fence must never be marked terminal -- row: {row:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    drop(fake_lock);
    env.kill_daemon_bounded().await;
}

/// The gap this test proves closed: a capsule row whose agent argv can
/// never launch was UNENDABLE from the UI. `sot-capsule supervise`'s own
/// anti-flap bound (`FLAP_THRESHOLD` == 3, `respawn_or_terminal` in
/// `rust/log/src/supervisor.rs`) trips within milliseconds of a real
/// `CreateProcess` failure and enters sticky `Lifecycle::Terminal`,
/// self-exiting `TERMINAL_EXIT_GRACE` (2s) later with no external `stop`
/// ever required -- so by the time a `workspace.list` poll (or a user)
/// ever observes phase "terminal", the authority process has almost
/// always ALREADY exited. Before this PR, `capsule_workspace::end_run`'s
/// wrapper only ever handled a LIVE lane answering `phase: Terminal`
/// (sending it `stop` and reporting a confirmed end); a lane that had
/// already gone fully silent by the time `workspace.destroy` reached it
/// surfaced as "supervisor lane unreachable" and was reported `Kept`
/// forever -- the row could never actually be destroyed. On Windows this
/// test uses the SAME "no `claude` on a CI runner's PATH" precondition to
/// force the failure deterministically (no new fixture machinery). On
/// Linux a genuinely absent `claude` does NOT reproduce the same
/// scenario: `agent_argv`'s own resolution step (`resolve_claude`)
/// refuses at the DAEMON level instead, before `sot-capsule` is ever
/// spawned at all -- so this test's Linux leg instead resolves to a fake,
/// deliberately-broken `claude` stub (`Env::seed_fake_unlaunchable_claude`)
/// that IS resolvable+executable but fails every time it actually runs,
/// reaching the SAME anti-flap/Terminal path through `sot-capsule`'s own
/// internal retry logic. Either way: seed the default row's own toml
/// with `agent = "claude"` before boot -- a REAL (if unlaunchable) agent,
/// so this row is NOT the 2026-09-04 inert-anchor amendment's concern
/// (that only ever applies to `agent == "none"`; see
/// `capsule_default_workspace_with_no_agent_is_never_started_on_attach`)
/// -- attach it once to trigger start-on-attach, and prove (1) the row
/// reaches phase "terminal" within a bound rather than cycling
/// Starting -> Terminal forever, and (2) `workspace.destroy` on it then
/// succeeds and the supervisor's own lane goes silent.
#[tokio::test]
async fn capsule_row_with_an_unlaunchable_agent_reaches_terminal_and_is_destroyable() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("ult");
    // Pre-write the default row's own toml with `agent = "claude"` BEFORE
    // boot -- `server.rs`'s fresh-boot seed already picks this agent on
    // Windows, but pre-writing it here makes the precondition explicit
    // and independent of that default ever changing.
    env.seed_default_capsule_toml("claude");
    // Windows: no `claude.exe` exists on a CI runner's PATH, so the
    // literal argv `agent_argv` hands `sot-capsule` fails to spawn every
    // time -- unchanged, exactly as before this port.
    #[cfg(windows)]
    env.spawn_sotd();
    // Linux: `agent_argv`'s own resolution step means a genuinely absent
    // `claude` would refuse at the DAEMON level instead (never reaching
    // `sot-capsule`'s own anti-flap/Terminal logic this test exercises)
    // -- a fake, deliberately-broken but resolvable `claude` reproduces
    // the same "unlaunchable agent" scenario portably (see
    // `Env::seed_fake_unlaunchable_claude`'s own doc).
    #[cfg(target_os = "linux")]
    let fake_claude_dir = env.seed_fake_unlaunchable_claude();
    #[cfg(target_os = "linux")]
    env.spawn_sotd_with_prepended_path(&fake_claude_dir);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let default_row = list_payload["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .find(|w| w["is_default"].as_bool() == Some(true))
        .cloned()
        .expect("a default workspace row");
    assert_eq!(default_row["runtime"], "capsule", "default row: {default_row:?}");
    let default_workspace_id = default_row["workspace_id"].as_str().expect("workspace_id").to_string();
    let default_target = default_row["tmux_session"].as_str().expect("tmux_session").to_string();
    let state_dir_path = env.state_root.join("sot").join("workspaces").join(&default_workspace_id);

    // Trigger start-on-attach: the capsule's producer (`claude`) will
    // fail to spawn every time this daemon retries it.
    let pty_req = serde_json::json!({
        "cols": 80, "rows": 24, "user_switch": true, "target": default_target,
    });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);

    // (1) The row reaches phase "terminal" within a bound -- never
    // cycling Starting -> Terminal -> Starting forever. Generous over
    // the anti-flap bound's own worst case (three near-instant spawn
    // failures) plus the authority's own 2s self-exit grace plus the
    // daemon watchdog's own child-wait — comfortably inside `BOUND`.
    let terminal_deadline = Instant::now() + BOUND;
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &default_workspace_id) {
            if row["phase"].as_str() == Some("terminal") {
                break;
            }
        }
        assert!(
            Instant::now() < terminal_deadline,
            "timed out waiting for the unlaunchable-agent capsule row to reach phase \"terminal\""
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // LU5a (ADR 0043 decision 25, Linux leg): the supervisor's own stderr
    // is the daemon's own log -- by the time this row is terminal, its
    // supervisor has necessarily written at least one diagnostic line
    // (the anti-flap/watchdog trail this whole test exercises), prefixed
    // with THIS workspace's own id so concurrent rows sharing the one
    // daemon log stay distinguishable.
    #[cfg(target_os = "linux")]
    {
        let log_path = env.state_root.join("sot").join("sotd.log");
        let log_contents = std::fs::read_to_string(&log_path)
            .unwrap_or_else(|e| panic!("could not read the daemon's own log {log_path:?}: {e}"));
        let wanted = format!("sot-capsule supervise[{default_workspace_id}]");
        assert!(
            log_contents.contains(&wanted),
            "expected {log_path:?} to contain a line with {wanted:?}; got:\n{log_contents}"
        );
    }

    // (2) `workspace.destroy` on a terminal row now succeeds (never the
    // typed `capsule_end_not_reached` error this gap used to produce
    // forever) -- the default row's own branch: kept (never deleted),
    // but its run is confirmed ended.
    let destroy_req = serde_json::json!({ "workspace_id": default_workspace_id });
    // `next_id` has no further use on this connection (mirrors the
    // create/list/destroy test above) -- no further increment.
    let destroy_res = call(&mut conn, next_id, op::WORKSPACE_DESTROY, destroy_req).await;
    assert!(
        destroy_res.payload.get("error").is_none(),
        "workspace.destroy on a terminal capsule row must succeed: {:?}",
        destroy_res.payload
    );
    assert!(
        destroy_res.payload.get("kept").and_then(|v| v.as_str()).is_some(),
        "default row destroy must report kept: {:?}",
        destroy_res.payload
    );

    // The supervisor's own lane is silent -- no resident `sot-capsule.exe`
    // leaked behind a row the UI now reports gone.
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move { if try_query_status(dir).await.is_none() { Some(()) } else { None } }
        },
        BOUND,
        "the terminal row's supervisor lane to be silent after workspace.destroy",
    )
    .await;

    env.kill_daemon_bounded().await;
}

/// LU4 review round 2, F4's own cleanup contract, proved directly here
/// rather than re-asserted in every test above (F4's own "whichever is
/// smaller" — one dedicated test beats touching all six bodies). Spawns
/// one real capsule row and waits for it to reach a leg worth cleaning up
/// (so the "empty after" assertions below are not vacuously true), then
/// drops `env` explicitly and proves `Env`'s own `Drop`:
///  - swept every process matching this env's own anchored leg pattern
///    ([`Env::leg_pgrep_pattern`], never the old unanchored substring
///    match a `tail -f` could false-match),
///  - killed this env's own ISOLATED tmux server (F3) rather than ever
///    touching the developer's real one (a different socket entirely —
///    this env's daemon never saw that path at all, `SOT_TMUX_SOCK`),
///  - and removed its own temp project/state/config dir AND its own
///    `SOT_RUNTIME_DIR` tempdir (no `/tmp/sotcw-*`/`/tmp/sotrt-*` left).
/// Linux only: `pkill`/`pgrep`/`tmux` are shelled out to directly.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_backend_test_env_drop_cleans_up_legs_daemon_and_tmux() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("cln");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "cln-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &workspace_id) {
            assert_eq!(row["runtime"], "capsule", "row: {row:?}");
            if row["phase"].as_str() == Some("ready") {
                break;
            }
        }
        assert!(
            Instant::now() < list_deadline,
            "timed out waiting for workspace.list to report phase \"ready\" for the new capsule workspace"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Not vacuous: a real `sot-capsule supervise`/leg pair matching this
    // env's own anchored pattern is alive right now. (The default row's
    // own boot-time tmux-session-ensure ALSO ran against this env's own
    // isolated socket — F3 — but whether it actually landed a session
    // there is host-dependent: on a box already running the ADR 0038
    // `sot-tmux.service` keeper for the developer's REAL socket, ADR
    // 0038's own conflict-refusal correctly declines to also start an
    // implicit server on a SECOND, isolated socket rather than risk
    // racing the keeper — so it fails closed instead, logged and
    // non-fatal, exactly its own documented "non-fatal" contract. Either
    // way no session ever reaches the developer's real socket, which is
    // F3's actual claim; this test's own proof stays on the leg, which
    // is deterministic regardless of that host difference.)
    let pattern = env.leg_pgrep_pattern();
    assert!(
        any_process_matches(&pattern),
        "expected a live sot-capsule leg matching {pattern:?} before the cleanup guard runs"
    );

    // The bounded, deliberate daemon teardown every other test in this
    // file ends its own run with — empties `Env`'s own daemon slot, so
    // `Drop`'s own step 1 below is a no-op, exactly like every other test.
    drop(conn);
    env.kill_daemon_bounded().await;

    // F4's actual claim: dropping `env` now sweeps this env's own legs
    // (anchored, so it can never touch anything else on the box) and
    // kills its OWN isolated tmux server (harmless whether or not a
    // session ever landed there), in that order, before its own temp
    // dirs vanish — capture what we need to verify BEFORE `env` (and the
    // fields these borrow from) are gone. Deletion (round 2 reviewer
    // note): no dedicated accessor for this same-module read — `_tmp`/
    // `_runtime_tmp` are plain private fields, directly readable here.
    let tmp_root = env._tmp.path().to_path_buf();
    let runtime_root = env._runtime_tmp.path().to_path_buf();
    drop(env);

    // G3: the same bounded, sweep-until-empty shape `Drop`'s own loop
    // uses, read-only here (see `poll_until_no_process_matches`'s doc).
    assert!(
        poll_until_no_process_matches(&pattern, Duration::from_secs(2)),
        "the cleanup guard must leave no process matching {pattern:?}"
    );
    assert!(
        !tmp_root.exists(),
        "the cleanup guard must remove the env's own temp project/state/config dir: {tmp_root:?}"
    );
    assert!(
        !runtime_root.exists(),
        "the cleanup guard must remove the env's own SOT_RUNTIME_DIR temp dir: {runtime_root:?}"
    );
}

/// ADR 0043 decision 23 (LU5a): `workspace.create` with `"runtime":
/// "capsule"` is refused OUTRIGHT — before any row persists — when the
/// state root resolves onto a VOLATILE filesystem (tmpfs here; ramfs is
/// the same code path, untested for lack of an easy-to-mount ramfs in
/// CI). Linux only: tmpfs-as-state-root is a Linux-specific concern here,
/// and Windows keeps its existing, unrelated NTFS-only preflight.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_create_is_refused_on_an_unqualified_state_root() {
    if !Path::new("/dev/shm").is_dir() {
        eprintln!(
            "skipping capsule_create_is_refused_on_an_unqualified_state_root: /dev/shm is not mounted here"
        );
        return;
    }
    let _serial = SERIAL.lock().await;

    let env = Env::new_with_state_root_on_tmpfs("cur");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let label = "cur-workspace";
    let create_req = serde_json::json!({
        "label": label,
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert_eq!(
        create_res.payload["code"], "state_root_unqualified",
        "payload: {:?}", create_res.payload
    );
    let error_text = create_res.payload["error"].as_str().expect("error text");
    assert!(error_text.contains("XDG_STATE_HOME"), "{error_text}");
    assert!(error_text.contains("tmpfs"), "{error_text}");
    assert!(
        create_res.payload.get("workspace_id").is_none(),
        "a refused create must mint no workspace_id: {:?}", create_res.payload
    );

    // workspace.list: no row for this (never-created) workspace.
    let ws_slug = slug(label);
    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let has_row = list_payload["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|w| w["slug"] == ws_slug);
    assert!(!has_row, "a refused create must not appear in workspace.list: {list_payload:?}");

    // No toml persisted for it either.
    let toml_path = env
        .app_config_dir()
        .join(format!("workspaces-{TEST_STATE_HOST}"))
        .join(format!("{ws_slug}.toml"));
    assert!(!toml_path.exists(), "a refused create must not persist a toml: {toml_path:?}");

    // The daemon stays healthy: a tmux-runtime create on the SAME daemon
    // still succeeds (the refusal above is per-request, never a
    // daemon-wide wedge).
    let tmux_project_root = env._tmp.path().join("tmux-workspace-project");
    std::fs::create_dir_all(&tmux_project_root).expect("mkdir tmux_project_root");
    let tmux_create_req = serde_json::json!({
        "label": "cur-tmux-workspace",
        "project_root": tmux_project_root.to_string_lossy(),
        "runtime": "tmux",
    });
    let tmux_create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, tmux_create_req).await;
    assert!(
        tmux_create_res.payload.get("error").is_none(),
        "tmux create on the same daemon failed: {:?}", tmux_create_res.payload
    );

    env.kill_daemon_bounded().await;
}
