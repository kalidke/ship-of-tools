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
    /// ADR 0043 decision 32 (lane L2), Codex BLOCKER 2: the scratch
    /// `systemd --user` unit [`Env::spawn_sotd_as_user_service`] started,
    /// if any — owned here (never merely returned to the caller) so
    /// `Drop` can stop it on EVERY exit path, a panic mid-test included,
    /// before it ever sweeps this env's own legs or lets
    /// `_tmp`/`_runtime_tmp` delete the directories that unit's daemon
    /// may still be reading from. `SOT_TEST_REQUIRE_USER_MANAGER=1`-only
    /// naming (`sot-test-<uuid>`) prevents a COLLISION between runs, not
    /// a LEAK from one — this field is what closes that second gap.
    /// Cleared by [`Env::forget_user_service`] once the test body itself
    /// has already stopped it (so `Drop` does not redundantly re-stop an
    /// already-gone unit — harmless either way, but quieter).
    user_service_unit: RefCell<Option<String>>,
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
            user_service_unit: RefCell::new(None),
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

    /// ADR 0043 decision 32 (lane L2), test 1's own precondition: launches
    /// THIS env's daemon as a REAL `systemd --user` service — never the
    /// live `sotd`, a uniquely-named scratch unit
    /// (`sot-test-<uuid>.service`) this test alone starts and stops
    /// (`stop_user_service`) — the one way to prove a capsule supervisor's
    /// transient user scope actually survives its OWN unit being stopped;
    /// [`Env::spawn_sotd`]'s plain `Command::spawn()` has no unit at all
    /// for that. Every env var [`Env::spawn_sotd`] sets is threaded
    /// through as `--setenv` (a `systemd-run --user` child does NOT
    /// inherit this test process's own env the way a plain `Command`
    /// child does), plus `PATH` and `HOME` so the daemon can still resolve
    /// `sot-capsule`'s own PATH-searched `systemd-run` probe and locate
    /// its own home. Does NOT populate `self.daemon` — the unit itself is
    /// the daemon's lifecycle handle now; `Drop`'s best-effort daemon kill
    /// and [`Env::kill_daemon_bounded`] both no-op for it. Tracked instead
    /// in [`Env::user_service_unit`] (Codex BLOCKER 2) so `Drop` stops the
    /// unit itself on every exit path, and the MainPID is read back and
    /// returned alongside the unit name so [`stop_user_service`] can
    /// confirm the actual daemon process — not merely `is-active`'s own
    /// text — has exited before the caller's sustained-survival window
    /// starts (Codex BLOCKER 1).
    #[cfg(target_os = "linux")]
    fn spawn_sotd_as_user_service(&self) -> (String, u32) {
        let unit = format!("sot-test-{}.service", uuid::Uuid::now_v7());
        let setenv = |k: &str, v: &std::ffi::OsStr| format!("--setenv={k}={}", v.to_string_lossy());
        let tmux_sock = self.tmux_sock();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let home = std::env::var_os("HOME").unwrap_or_default();
        let status = Command::new("systemd-run")
            .arg("--user")
            .arg("--unit")
            .arg(&unit)
            .arg("--service-type=exec")
            .arg("--collect")
            .arg(setenv("LOCALAPPDATA", self.state_root.as_os_str()))
            .arg(setenv("XDG_STATE_HOME", self.state_root.as_os_str()))
            .arg(setenv("XDG_CONFIG_HOME", self.config_root.as_os_str()))
            .arg(setenv("SOT_STATE_HOST", std::ffi::OsStr::new(TEST_STATE_HOST)))
            .arg(setenv("SOT_RUNTIME_DIR", self._runtime_tmp.path().as_os_str()))
            .arg(setenv("SOT_TMUX_SOCK", tmux_sock.as_os_str()))
            .arg(setenv("PATH", &path))
            .arg(setenv("HOME", &home))
            .arg("--")
            .arg(sotd_exe())
            .arg("--socket")
            .arg(&self.socket_path)
            .arg("--project-root")
            .arg(&self.daemon_project_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("systemd-run --user --unit spawn_sotd_as_user_service");
        assert!(status.success(), "systemd-run --user --unit {unit} failed to start ({status})");
        // Tracked BEFORE returning, not after — a panic between here and
        // the caller ever reading the return value must still leave
        // `Drop` able to find and stop it.
        *self.user_service_unit.borrow_mut() = Some(unit.clone());
        // `--service-type=exec` (set above) makes `systemd-run` itself
        // return only once the unit's own `execve` has actually happened
        // — MainPID is therefore already populated the moment `status()`
        // returns, no poll needed.
        let pid_output = Command::new("systemctl")
            .arg("--user")
            .arg("show")
            .arg(&unit)
            .arg("--property=MainPID")
            .arg("--value")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .expect("systemctl --user show MainPID");
        let pid: u32 = String::from_utf8_lossy(&pid_output.stdout)
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("parse MainPID for {unit}: {e}"));
        (unit, pid)
    }

    /// Tell `Drop` the test body has already stopped
    /// [`Env::spawn_sotd_as_user_service`]'s own unit itself
    /// (`stop_user_service`) — so it is not redundantly re-stopped.
    #[cfg(target_os = "linux")]
    fn forget_user_service(&self) {
        self.user_service_unit.borrow_mut().take();
    }

    /// Linux only, used ONLY by
    /// `capsule_launch_degrades_when_no_user_scope_is_available`: a
    /// directory containing a `systemd-run` stub that always refuses —
    /// mirrors [`Env::seed_fake_unlaunchable_claude`]'s own shape, one
    /// script standing in for "no reachable `systemd --user` manager" so
    /// that test exercises the degraded fallback deterministically,
    /// regardless of whether THIS runner actually has one.
    #[cfg(target_os = "linux")]
    fn seed_stub_systemd_run(&self) -> PathBuf {
        let dir = self._tmp.path().join("fakebin-systemd-run");
        std::fs::create_dir_all(&dir).expect("mkdir fakebin-systemd-run");
        let stub = dir.join("systemd-run");
        std::fs::write(&stub, b"#!/bin/sh\necho 'stub: no user manager' >&2; exit 1\n")
            .expect("write stub systemd-run");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub systemd-run");
        dir
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

/// ADR 0043 decision 32 (lane L2), Codex BLOCKER 2: stop this env's own
/// scratch `systemd --user` service FIRST, if [`Env::spawn_sotd_as_user_
/// service`] ever started one and the test body never called
/// [`Env::forget_user_service`] — before ANY of the LU4 review round 2,
/// F4 ordering below, which otherwise assumes `self.daemon` is the only
/// live daemon process a panic could leave running. THEN: kill the
/// tracked daemon `Child` (so its own crash-restart policy can't spawn a
/// fresh contender into a state dir this impl is about to sweep), THEN
/// sweep this env's own legs with the ANCHORED pattern
/// ([`Env::leg_pgrep_pattern`] — never the old unanchored `pkill -f
/// <state_dir>` substring match), THEN kill this env's own ISOLATED tmux
/// server (F3) — in that exact order, on EVERY exit path including a
/// panic, which a per-test teardown call can never guarantee.
/// `_tmp`/`_runtime_tmp`'s own `Drop` (temp dir removal, the final step)
/// runs automatically right after this method returns — Rust drops a
/// value's remaining fields, in declaration order, immediately after a
/// manual `Drop::drop` body finishes.
impl Drop for Env {
    fn drop(&mut self) {
        // (0) ADR 0043 decision 32 (lane L2), Codex BLOCKER 2: any
        // scratch `systemd --user` service this env started, stopped
        // FIRST — before the tracked daemon `Child` below and well
        // before `_tmp`/`_runtime_tmp` (declared later in this struct,
        // so dropped after this method returns) delete the directories
        // out from under a daemon that is still running because the
        // test body panicked before it ever reached `stop_user_service`
        // itself. Best-effort, like the daemon kill just below (a failed
        // stop here has no better recovery than leaving it for the next
        // sweep) — never the asserting version test bodies use.
        #[cfg(target_os = "linux")]
        if let Some(unit) = self.user_service_unit.get_mut().take() {
            let _ = Command::new("systemctl")
                .arg("--user")
                .arg("stop")
                .arg(&unit)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }

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

/// Drains `pipe` to EOF or `bound`, whichever comes first — mirrors
/// `capsule_workspace::runtime::drain_stderr_bounded` (production, Codex
/// SHOULD-FIX: a wrapper that has already exited can still leave stderr
/// inherited by a still-running grandchild, and a plain `read_to_string`
/// then blocks until EVERY holder of the pipe's write end closes it, not
/// just the immediate child whose own exit was already observed —
/// measured 7 s in production's own repro). Same off-thread-plus-
/// `recv_timeout` shape, duplicated rather than shared for the same
/// crate-boundary reason [`user_manager_available_for_test`]'s own doc
/// gives for duplicating the probe itself.
#[cfg(target_os = "linux")]
fn drain_stderr_bounded(mut pipe: impl std::io::Read + Send + 'static, bound: Duration) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = pipe.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    rx.recv_timeout(bound).unwrap_or_default()
}

/// ADR 0043 decision 32, test 1's own SKIP gate: does THIS test runner
/// have a `systemd --user` manager reachable at all? Same capability
/// question `capsule_workspace::runtime::user_scope_available` answers in
/// production, duplicated here rather than exposed from `sot-backend`
/// (that function is private to its own crate) — the two probes are a
/// handful of lines each and answer the same question for genuinely
/// different callers, not worth a shared crate-boundary-crossing export.
/// `Err`'s message is the probe's own stderr, verbatim where there is
/// any, drained under its own separate bound — exactly what the caller
/// prints on `SKIPPED:`.
#[cfg(target_os = "linux")]
fn user_manager_available_for_test() -> Result<(), String> {
    let mut child = Command::new("systemd-run")
        .args(["--user", "--scope", "--quiet", "--", "/bin/true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("systemd-run --user --scope did not answer within 5s".to_string());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if status.success() {
        return Ok(());
    }
    let stderr = child
        .stderr
        .take()
        .map(|pipe| drain_stderr_bounded(pipe, Duration::from_secs(1)))
        .unwrap_or_default();
    Err(stderr.trim().to_string())
}

/// Stop the scratch unit [`Env::spawn_sotd_as_user_service`] started —
/// NEVER the live `sotd`'s own unit, a distinct `sot-test-<uuid>.service`
/// this test alone owns. Two proofs, both required (Codex BLOCKER 1,
/// reproduced with a failing `systemctl` stub: the old version's ignored
/// `status()` plus an `is-active` check that folded a command ERROR to
/// `unwrap_or(false)` — "not active" — let a stop that never actually ran
/// read as success): (1) `systemctl --user stop` itself reports success —
/// a command error, a nonzero exit, ANY failure here is a hard test
/// failure, never silently treated as "done"; (2) `daemon_pid` — read
/// back by the caller from `MainPID` right after spawn, never re-derived
/// here — has actually exited (`/proc/<pid>` gone), polled rather than
/// trusted the instant `stop` returns. `is-active` alone is not enough
/// for (2): it can still read `"deactivating"` mid-shutdown, which would
/// let the caller's sustained-survival window start before the daemon
/// backing this unit is actually dead.
#[cfg(target_os = "linux")]
fn stop_user_service(unit: &str, daemon_pid: u32) {
    let status = Command::new("systemctl")
        .arg("--user")
        .arg("stop")
        .arg(unit)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run systemctl --user stop");
    assert!(status.success(), "systemctl --user stop {unit} failed ({status})");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if !Path::new(&format!("/proc/{daemon_pid}")).exists() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "systemctl --user stop {unit} reported success but pid {daemon_pid} is still alive after 10s"
        );
        std::thread::sleep(Duration::from_millis(100));
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
        fe_handle: None,
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

/// One `MgmtRequest::Status` round trip against the LEG's own real
/// voyage socket (`sot_log::socket_unix::connect_voyage_socket`, ADR
/// 0043 decision 8 steps 1-3: connect + same-user auth, the ordinary
/// step-5-client-facing constructor) — the WIRE value the degrade test
/// proves against (Codex SHOULD-FIX: `/proc/<pid>/cmdline` text does not
/// prove propagation onto the wire; restoring the deleted Unix survival
/// clamp would still leave a cmdline-only check green). Distinct from
/// [`try_query_status`]'s own `sot_log::supervisor_client::query_status`:
/// that is the SUPERVISOR's own status (`SupervisorReply::StatusOk`,
/// which carries no `survival` field at all) — `survival` lives only on
/// `MgmtReply::StatusOk`, the LEG's own mgmt lane. The SOM0 mgmt lane
/// has no hello frame (unlike the supervisor/attach lanes) — `connect_
/// voyage_socket`'s own same-user auth already happened before this
/// function ever gets a `SocketClient` back, so the very first frame
/// sent here is the request itself. Bounded like every other wire round
/// trip in this file ([`call`]'s own `BOUND`) — the blocking body runs
/// on its own thread via `spawn_blocking`, abandoned (not cancelled) on
/// timeout, exactly [`drain_stderr_bounded`]'s own "leak, never hang"
/// tradeoff.
#[cfg(target_os = "linux")]
async fn leg_survival(voyage_id: &str) -> sot_log::wire::Survival {
    let voyage_id_owned = voyage_id.to_string();
    let voyage_id_for_body = voyage_id_owned.clone();
    tokio::time::timeout(
        BOUND,
        tokio::task::spawn_blocking(move || -> sot_log::wire::Survival {
            let client = sot_log::socket_unix::connect_voyage_socket(&voyage_id_for_body)
                .unwrap_or_else(|e| panic!("connect_voyage_socket({voyage_id_for_body}): {e}"));
            let body = sot_log::wire::encode_mgmt_request(&sot_log::wire::MgmtRequest::Status)
                .expect("MgmtRequest::Status has no fields; encoding cannot fail");
            client.write_all(&body).expect("write MgmtRequest::Status");
            let mut splitter = sot_log::wire::FrameSplitter::new();
            let mut buf = [0u8; 512];
            loop {
                let n = client.read(&mut buf).expect("read mgmt reply");
                assert!(n > 0, "voyage socket closed before answering status");
                let (frames, err) = splitter.feed(&buf[..n]);
                assert!(err.is_none(), "mgmt lane wire error: {err:?}");
                for frame in frames {
                    if let sot_log::wire::DecodedFrame::MgmtReply(sot_log::wire::MgmtReply::StatusOk {
                        survival,
                        ..
                    }) = frame
                    {
                        return survival;
                    }
                }
            }
        }),
    )
    .await
    .unwrap_or_else(|_| panic!("leg mgmt status for {voyage_id_owned} did not answer within {BOUND:?}"))
    .expect("leg_survival's own blocking task panicked")
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

/// A minimal lane-refusal FIXTURE standing in for a supervisor of another
/// build (ADR 0030 §8 decision 31c) — binds the EXACT unix socket path
/// `phase_of`'s own `query_status` will dial for `state_dir`
/// (`sot_log::socket_unix::supervisor_socket_path`, the same one this
/// process's own `SOT_RUNTIME_DIR` resolves it to), accepts ONE
/// connection, and writes back `reply_bytes` verbatim before closing.
/// Same-user peer credentials (the SID/`SO_PEERCRED` steps) pass for
/// free: this fixture runs as the test's own process, so the kernel
/// reports it as the caller's own user regardless of what this function's
/// code does — no second build, no real supervisor, and no compile step
/// are needed to prove either half of the "answered but ___" split;
/// only the one reply a real peer would send. Two callers below use this
/// with two different `reply_bytes`: an actual `Refused { VersionSkew }`
/// encoding proves "foreign"; anything else well-formed-but-wrong proves
/// "unreachable" stays unreachable.
#[cfg(target_os = "linux")]
fn spawn_lane_refusal_fixture(state_dir: &Path, reply_bytes: Vec<u8>) -> std::thread::JoinHandle<()> {
    let h = sot_log::state_dir::state_dir_hash(state_dir);
    let path = sot_log::socket_unix::supervisor_socket_path(&h).expect("supervisor socket path");
    let _ = std::fs::remove_file(&path);
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind the fixture supervisor socket");
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            use std::io::Write;
            let _ = stream.write_all(&reply_bytes);
            // Hold the connection open briefly so the client's own read
            // has time to land before this fixture (and its listener)
            // drop -- a one-shot fixture, not a persistent server.
            std::thread::sleep(Duration::from_millis(500));
        }
    })
}

/// Real-supervisor preamble shared by both lane-refusal tests below:
/// create a capsule workspace, wait for a REAL supervisor (this
/// checkout's own build) to reach "ready", then stop JUST the authority
/// (the leg survives, ADR 0041 Lifecycle) so the state dir carries a
/// published pointer with nothing currently answering its socket —
/// exactly the precondition [`spawn_lane_refusal_fixture`]'s caller needs
/// before binding in the real supervisor's place.
#[cfg(target_os = "linux")]
async fn create_ready_workspace_then_stop_its_supervisor(
    env: &Env,
    conn: &mut Conn,
    next_id: &mut u64,
    label: &str,
) -> (String, PathBuf) {
    let create_req = serde_json::json!({
        "label": label,
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(conn, *next_id, op::WORKSPACE_CREATE, create_req).await;
    *next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

    let list_deadline = Instant::now() + BOUND.max(Duration::from_secs(90));
    let state_dir = loop {
        let id = *next_id;
        *next_id += 1;
        let payload = call(conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
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

    (workspace_id, state_dir_path)
}

/// Poll `workspace.list` until `workspace_id`'s row reaches `want_phase`,
/// asserting it never reports `"terminal"` along the way (a competing
/// spawn racing the still-held fence would be the WRONG way to reach
/// this test's own target phase).
async fn poll_for_phase(conn: &mut Conn, next_id: &mut u64, workspace_id: &str, want_phase: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let id = *next_id;
        *next_id += 1;
        let payload = call(conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, workspace_id) {
            assert_eq!(row["runtime"], "capsule", "row: {row:?}");
            assert_ne!(row["phase"].as_str(), Some("terminal"), "row went terminal instead of {want_phase}: {row:?}");
            if row["phase"].as_str() == Some(want_phase) {
                return;
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for workspace.list to report phase \"{want_phase}\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// ADR 0030 §8 decision 31c (cross-referenced as ADR 0043 decision 31;
/// the gate itself is superseded by ADR 0045 decision 7 -- `proto`, not
/// build): `phase_of` reports `"foreign"` for a capsule row whose
/// supervisor lane answered but refused this daemon's protocol (typed as
/// `sot_log::Error::VersionSkew`, never a text match). Reproduces the
/// field incident's OBSERVABLE shape (a row an operator finds already
/// held by a foreign lane, not one this daemon started that way -- this
/// daemon would never spawn one itself) using
/// [`spawn_lane_refusal_fixture`] in place of a real foreign peer.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn phase_reports_foreign_for_a_version_skew_refusal() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("foreign");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let (workspace_id, state_dir_path) =
        create_ready_workspace_then_stop_its_supervisor(&env, &mut conn, &mut next_id, "foreign-workspace").await;

    // Bind the fixture where the (now-stopped) real supervisor was, and
    // reply with the ACTUAL wire encoding of `Refused { VersionSkew }` —
    // the one reply a real supervisor of another build would send.
    let reply = sot_log::wire::encode_supervisor_reply(&sot_log::wire::SupervisorReply::Refused {
        reason: sot_log::wire::SupervisorRefusedReason::VersionSkew,
    })
    .expect("Refused encodes unconditionally");
    let fixture = spawn_lane_refusal_fixture(&state_dir_path, reply);

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "foreign", BOUND.max(Duration::from_secs(60))).await;

    // Teardown: the surviving leg (the platform shell the ORIGINAL real
    // supervisor spawned) is caught by `Env`'s own leg sweep, anchored on
    // `env`'s own `state_root` — the fixture thread holds no leg of its
    // own and exits on its own once its one connection closes.
    let _ = fixture.join();
    env.kill_daemon_bounded().await;
}

/// Sibling of [`phase_reports_foreign_for_a_version_skew_refusal`]: a lane
/// that answers but with a MALFORMED/wrong-shape reply (never a
/// `Refused { VersionSkew }`) must stay `"unreachable"` — the exact
/// distinction ADR 0030 §8 decision 31c's typed check exists to draw
/// (Codex review: a broader `Foreign` classification, text-matched, would
/// have reported "foreign" here too).
#[tokio::test]
#[cfg(target_os = "linux")]
async fn phase_stays_unreachable_for_a_malformed_reply() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("malformed");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let (workspace_id, state_dir_path) =
        create_ready_workspace_then_stop_its_supervisor(&env, &mut conn, &mut next_id, "malformed-workspace").await;

    let fixture = spawn_lane_refusal_fixture(&state_dir_path, b"not a valid supervisor-lane frame".to_vec());

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "unreachable", BOUND.max(Duration::from_secs(60))).await;

    let _ = fixture.join();
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
        sot_log::client::PlatformEndpoint::default(),
        sot_log::state_dir::state_dir_hash(&state_dir),
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

/// ADR 0043 decision 32 (revised): a daemon that finds itself inside a job
/// forbidding breakaway is never refused the launch over its own
/// containment — it retries the spawn without `CREATE_BREAKAWAY_FROM_JOB`,
/// logs once, and the row still reaches "ready" (`--survival degraded`).
/// Honest for a hosted CI runner too, which keeps its own processes inside
/// a job lacking `JOB_OBJECT_LIMIT_BREAKAWAY_OK` regardless of this test's
/// own explicit assignment below — the row reaching ready is asserted
/// unconditionally, and containment is then proven directly
/// (`IsProcessInJob` against the ONE job this test built, standing in for
/// a capsule's own leg job) rather than assumed from the create call
/// alone.
#[tokio::test]
#[cfg(windows)]
async fn create_from_inside_a_job_that_forbids_breakaway_still_reaches_ready() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let _serial = SERIAL.lock().await;
    let env = Env::new("breakaway-contained");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    // A job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` only -- no
    // `JOB_OBJECT_LIMIT_BREAKAWAY_OK` -- standing in for a capsule's own
    // leg job that this `sotd` finds itself launched inside.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    assert!(!job.is_null(), "CreateJobObjectW: {}", std::io::Error::last_os_error());
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    assert!(ok != 0, "SetInformationJobObject: {}", std::io::Error::last_os_error());
    let daemon_handle = {
        let daemon = env.daemon.borrow();
        daemon.as_ref().expect("daemon spawned").as_raw_handle() as HANDLE
    };
    let ok = unsafe { AssignProcessToJobObject(job, daemon_handle) };
    assert!(ok != 0, "AssignProcessToJobObject(sotd): {}", std::io::Error::last_os_error());

    let create_req = serde_json::json!({
        "label": "breakaway-contained-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(
        create_res.payload.get("error").is_none(),
        "a job that forbids breakaway must never refuse the launch: {:?}", create_res.payload
    );
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;

    let state_dir = state_dir_from_list(&mut conn, &mut next_id, &workspace_id).await;
    let (_status, process) =
        tokio::task::spawn_blocking(move || sot_log::supervisor_client::query_status(&state_dir))
            .await
            .unwrap()
            .expect("query_status after ready");
    let supervisor = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process.pid()) };
    assert!(!supervisor.is_null(), "OpenProcess({}): {}", process.pid(), std::io::Error::last_os_error());
    let mut in_job: i32 = 0;
    let ok = unsafe { IsProcessInJob(supervisor, job, &mut in_job) };
    assert!(ok != 0, "IsProcessInJob: {}", std::io::Error::last_os_error());
    assert_eq!(in_job, 1, "the contained supervisor must stay in the job it could not break away from");

    unsafe {
        CloseHandle(supervisor);
        CloseHandle(job);
    }
    env.kill_daemon_bounded().await;
}

/// ADR 0043 decision 32 (lane L2), test 1: the actual proof the Linux
/// escape works — a supervisor spawned under a REAL `systemd --user`
/// service (never the live `sotd`; a uniquely-named scratch unit this
/// test alone starts and stops) survives that unit being stopped, because
/// it left the unit's own cgroup for its own transient scope at spawn
/// time (`systemd-run --user --scope`). Skips loudly (never silently)
/// when this host has no reachable `systemd --user` manager at all (a
/// bare CI container, commonly) — the ONE test in this suite that needs a
/// real answer to that question; force it with
/// `SOT_TEST_REQUIRE_USER_MANAGER=1` wherever a real user manager is
/// expected to exist.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_supervisor_survives_a_real_user_service_stop() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );
    if let Err(e) = user_manager_available_for_test() {
        if std::env::var("SOT_TEST_REQUIRE_USER_MANAGER").as_deref() == Ok("1") {
            panic!("SOT_TEST_REQUIRE_USER_MANAGER=1 but no user manager is reachable: {e}");
        }
        eprintln!("SKIPPED: no user manager: {e}");
        return;
    }

    let env = Env::new("uss");
    let (unit, daemon_pid) = env.spawn_sotd_as_user_service();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "uss-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;
    let state_dir = state_dir_from_list(&mut conn, &mut next_id, &workspace_id).await;

    let (status, process) = tokio::task::spawn_blocking({
        let dir = state_dir.clone();
        move || sot_log::supervisor_client::query_status(&dir)
    })
    .await
    .unwrap()
    .expect("query_status before stopping the daemon's own unit");
    let leg_before = status.leg.expect("a leg epoch on a ready row");
    let pid = process.pid();
    drop(process);

    // The supervisor left the DAEMON's own unit's cgroup for its own
    // transient scope at spawn time (ADR 0043 decision 32) -- proven
    // BEFORE the stop, not merely inferred from surviving it.
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .unwrap_or_else(|e| panic!("read /proc/{pid}/cgroup: {e}"));
    let last_segment = cgroup.trim().rsplit('/').next().unwrap_or("");
    assert!(
        last_segment.starts_with("run-") && last_segment.ends_with(".scope"),
        "supervisor's own cgroup does not end in a run-*.scope (still inside the daemon's own unit?): {cgroup:?}"
    );
    assert!(
        !cgroup.contains(&unit),
        "supervisor's own cgroup still names the daemon's own unit {unit:?}: {cgroup:?}"
    );

    stop_user_service(&unit, daemon_pid);
    env.forget_user_service();

    // 3 s SUSTAINED (never a single lucky sample): the supervisor's own
    // lane keeps answering and its leg keeps running for the WHOLE
    // window, proving survival actually crossed the unit stop rather than
    // merely outliving it by a race.
    let run_pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "run", &env.state_root);
    let sustain_deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let alive = try_query_status(state_dir.clone()).await.is_some() && any_process_matches(&run_pattern);
        assert!(alive, "supervisor lane or its leg went away within 3s of the daemon's own unit stopping");
        if Instant::now() >= sustain_deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    drop(conn);
    env.spawn_sotd();
    let (mut conn2, mut next_id2) = connect_and_hello(&env.socket_path).await;
    poll_for_phase(&mut conn2, &mut next_id2, &workspace_id, "ready", BOUND.max(Duration::from_secs(90))).await;

    let leg_after = tokio::task::spawn_blocking({
        let dir = state_dir.clone();
        move || sot_log::supervisor_client::query_status(&dir).expect("query_status after restart").0.leg
    })
    .await
    .unwrap();
    assert_eq!(
        leg_after,
        Some(leg_before),
        "the leg epoch changed across the daemon restart -- a fresh leg was spawned, not adopted"
    );

    env.kill_daemon_bounded().await;
}

/// ADR 0043 decision 32 (lane L2), test 2: on a host that denies the
/// escape (a stubbed `systemd-run` standing in for "no reachable
/// `systemd --user` manager", so this runs deterministically regardless
/// of whether a REAL one exists here too), the row still reaches "ready"
/// — contained, degraded, but never refused — and the daemon reports
/// exactly why: the LEG's own mgmt status reports `survival: Degraded`
/// on the wire ([`leg_survival`] — Codex SHOULD-FIX: cmdline text proves
/// only what was typed on the command line, not what the process
/// actually configured or reported; restoring the deleted Unix survival
/// clamp would still leave a cmdline-only check green), and the daemon's
/// own log names the probe's stderr.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_launch_degrades_when_no_user_scope_is_available() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("deg");
    let stub_dir = env.seed_stub_systemd_run();
    env.spawn_sotd_with_prepended_path(&stub_dir);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "deg-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(
        create_res.payload.get("error").is_none(),
        "a denied user scope must never refuse the launch: {:?}",
        create_res.payload
    );
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;
    let state_dir = state_dir_from_list(&mut conn, &mut next_id, &workspace_id).await;

    let (status, process) = tokio::task::spawn_blocking({
        let dir = state_dir.clone();
        move || sot_log::supervisor_client::query_status(&dir)
    })
    .await
    .unwrap()
    .expect("query_status on the degraded row");
    drop(process);
    let voyage_id = status.voyage.expect("a voyage id on a ready row");

    let survival = leg_survival(&voyage_id).await;
    assert_eq!(
        survival,
        sot_log::wire::Survival::Degraded,
        "the leg's own mgmt status must report survival=degraded when the user scope is denied"
    );

    let log_path = env.state_root.join("sot").join("sotd.log");
    let log_contents = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("could not read the daemon's own log {log_path:?}: {e}"));
    assert!(
        log_contents.contains("stub: no user manager"),
        "expected {log_path:?} to contain the stub systemd-run's own stderr; got:\n{log_contents}"
    );

    env.kill_daemon_bounded().await;
}
// --- ADR 0043 decision 33 (lane L1a): the per-row guard, resume_if_absent,
// and the watchdog's guard-through-backoff restart --- //

/// SIGKILL every process matching the SUPERVISE half of this env's own
/// anchored leg pattern ([`build_leg_pgrep_pattern`]), then poll it gone —
/// simulates the authority crashing outright (never a graceful `stop`,
/// which would publish its own end-of-authority state cleanly). The
/// capsule LEG (a separate process, ADR 0041 Lifecycle) is untouched.
#[cfg(target_os = "linux")]
fn kill_supervisor_only(state_root: &Path) {
    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", state_root);
    let _ = Command::new("pkill")
        .arg("-9")
        .arg("-f")
        .arg(&pattern)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    assert!(
        poll_until_no_process_matches(&pattern, BOUND),
        "a supervisor process still matches {pattern:?} after SIGKILL"
    );
}

/// [`kill_supervisor_only`]'s twin for the capsule LEG (the `run`
/// subcommand) — used only where a test needs the leg genuinely gone too
/// (no marker, no survivor to adopt), never on its own.
#[cfg(target_os = "linux")]
fn kill_leg_only(state_root: &Path) {
    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "run", state_root);
    let _ = Command::new("pkill")
        .arg("-9")
        .arg("-f")
        .arg(&pattern)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    assert!(
        poll_until_no_process_matches(&pattern, BOUND),
        "a leg process still matches {pattern:?} after SIGKILL"
    );
}

/// The number of live processes whose command line matches `pattern` —
/// [`any_process_matches`]'s counting twin, needed by the stale-attach
/// test below to prove "at most ONE," not merely "at least one." `Err`
/// only when `pgrep` itself could not be run at all (Codex review,
/// 2026-09-11: a query failure must fail the test, never silently count
/// as "zero processes" — a false "at most one" proves nothing). `pgrep`
/// exiting 1 (no match) is a normal, successful `Ok(0)`, not an error.
#[cfg(target_os = "linux")]
fn count_matching_processes(pattern: &str) -> std::io::Result<usize> {
    let output = Command::new("pgrep")
        .arg("-f")
        .arg(pattern)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).lines().filter(|l| !l.trim().is_empty()).count())
}

/// How many times `needle` appears in `sotd.log` so far — used to
/// synchronize on a NEW occurrence of a specific watchdog log line
/// (Codex review, 2026-09-11) rather than a fixed sleep, which proves
/// nothing about whether the watchdog has actually reached the point in
/// its own code that line marks.
#[cfg(target_os = "linux")]
fn count_log_occurrences(log_path: &Path, needle: &str) -> usize {
    std::fs::read_to_string(log_path).map(|s| s.matches(needle).count()).unwrap_or(0)
}

/// Decision 33: an ADOPTED row (`resume_all`'s boot scan found the
/// authority already alive and simply logged it — "a watchdog exists
/// only for a `Child` the daemon launched") gets no watchdog at all.
/// Destroying it must leave nothing behind that could respawn it: no
/// lingering `supervise` process sustained over a real window (not a
/// single point-in-time check), the fence freely acquirable again
/// afterward, and — the regression this specifically guards against —
/// `sotd.log` never carries the watchdog's own "treating as a crash"
/// text, which could only appear if some future change re-attached a
/// watchdog to an adopted leg (or the old `starting` claim's stale-
/// backoff window came back).
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_destroy_after_adoption_leaves_no_respawn() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("dan");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "dan-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

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
        assert!(Instant::now() < list_deadline, "timed out waiting for the new capsule workspace to reach \"ready\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);

    let leg_before = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            sot_log::supervisor_client::query_status(&dir)
                .expect("query_status before the daemon restart")
                .0
                .leg
        }
    })
    .await
    .unwrap()
    .expect("a ready capsule has a leg");

    // Adopt: kill only the DAEMON, leave the authority alive, reboot —
    // resume_all's own `None` arm now just logs and moves on (decision
    // 33: no watchdog for a row this daemon never itself spawned).
    let (mut conn2, next_id2) = restart_daemon_and_prove_adoption(
        &env,
        conn,
        &workspace_id,
        &state_dir,
        &state_dir_path,
        leg_before,
        AuthorityAtRestart::Alive,
    )
    .await;

    let destroy_req = serde_json::json!({ "workspace_id": workspace_id });
    // `next_id2` has no further use on this connection (mirrors the
    // create/list/destroy test's own convention) — no further increment.
    let destroy_res = call(&mut conn2, next_id2, op::WORKSPACE_DESTROY, destroy_req).await;
    assert!(destroy_res.payload.get("error").is_none(), "workspace.destroy failed: {:?}", destroy_res.payload);

    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move { if try_query_status(dir).await.is_none() { Some(()) } else { None } }
        },
        BOUND,
        "the destroyed row's supervisor lane to go silent",
    )
    .await;

    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
    // 5s SUSTAINED silence (250ms loop) — the regression this closes is a
    // respawn some moments AFTER destroy, not merely "not respawned yet."
    let sustain_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < sustain_deadline {
        assert!(
            try_query_status(state_dir_path.clone()).await.is_none(),
            "a destroyed, adopted row answered a status query again — it respawned"
        );
        assert!(
            !any_process_matches(&pattern),
            "a destroyed, adopted row's supervisor process reappeared — it respawned"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // The fence is freely acquirable — no lingering authority holds it.
    let fence = sot_log::fence::lock_supervisor(&state_dir_path);
    assert!(fence.is_ok(), "the fence must be acquirable after a clean destroy — something still holds it");
    drop(fence);

    let log_path = env.state_root.join("sot").join("sotd.log");
    let log_contents = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("could not read the daemon's own log {log_path:?}: {e}"));
    assert!(
        !log_contents.contains("treating as a crash"),
        "sotd.log has a \"treating as a crash\" line — no watchdog should ever have run for this adopted, then destroyed, row"
    );

    env.kill_daemon_bounded().await;
}

/// ADR 0043 decision 33's destroy proof, exercised end to end: a row
/// whose SUPERVISOR alone died (the leg survives headless) is still
/// destroyable. `destroy_capsule_workspace`'s own pre-step
/// (`capsule_workspace::resume_locked`, under the SAME row guard
/// `end_run` then runs under) re-establishes the authority first, so
/// `end_run` finds a real lane to ask — the fence/leg proof
/// (`leg_absent`) never needs to fire at all.
///
/// Codex review (2026-09-11): the authority is first ADOPTED across a
/// daemon restart (`restart_daemon_and_prove_adoption`,
/// `AuthorityAtRestart::Alive`) BEFORE it is killed — an authority this
/// daemon merely adopted at boot gets no watchdog at all (decision 33),
/// so the ONLY thing that can bring the row back for `end_run` to reach
/// is `destroy_capsule_workspace`'s own resume call below. Without this,
/// the row's ORIGINAL watchdog (installed by `workspace.create`) could
/// race to restart it on its own, and this test could pass even with
/// that resume call deleted. The restart itself proves the surviving
/// leg's identity is unchanged (`restart_daemon_and_prove_adoption`'s own
/// leg-epoch assertion) before the supervisor is ever killed.
///
/// Both the re-established `supervise` process and the `run` leg it ends
/// must be gone within a bound, and the row itself removed from
/// `workspace.list` — the ordinary confirmed-end removal, reached from a
/// row that looked dead going in.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_destroy_resumes_then_ends_a_leg_whose_supervisor_died() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("ddr");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "ddr-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

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
        assert!(Instant::now() < list_deadline, "timed out waiting for the new capsule workspace to reach \"ready\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);

    let leg_before = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            sot_log::supervisor_client::query_status(&dir)
                .expect("query_status before the daemon restart")
                .0
                .leg
        }
    })
    .await
    .unwrap()
    .expect("a ready capsule has a leg");

    // Adopt across a daemon restart FIRST -- the authority survives, this
    // daemon lifetime never spawned it, so no watchdog exists for it; the
    // ONLY thing left that can bring the row back is
    // `destroy_capsule_workspace`'s own resume call below.
    let (mut conn, mut next_id) = restart_daemon_and_prove_adoption(
        &env,
        conn,
        &workspace_id,
        &state_dir,
        &state_dir_path,
        leg_before,
        AuthorityAtRestart::Alive,
    )
    .await;

    kill_supervisor_only(&env.state_root);

    // `destroy_capsule_workspace`'s own resume pre-step re-establishes
    // the authority before `end_run` ever runs; a lane still settling
    // past `Starting` when `end_run` reaches it answers `Kept` with
    // "supervisor is starting; retry" — a legitimate retryable outcome
    // (`EndRunOutcome::Starting`'s own doc), never a failure. Retry
    // exactly that one reason; anything else fails the test at once.
    let destroy_deadline = Instant::now() + BOUND.max(Duration::from_secs(30));
    loop {
        let destroy_req = serde_json::json!({ "workspace_id": workspace_id });
        let destroy_res = call(&mut conn, next_id, op::WORKSPACE_DESTROY, destroy_req).await;
        next_id += 1;
        if destroy_res.payload.get("error").is_none() {
            break;
        }
        let detail = destroy_res.payload.get("error").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            detail.contains("starting"),
            "workspace.destroy failed for a reason other than a still-settling resume: {:?}",
            destroy_res.payload
        );
        assert!(
            Instant::now() < destroy_deadline,
            "workspace.destroy never got past \"starting; retry\": last reply {:?}",
            destroy_res.payload
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let supervise_pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
    let run_pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "run", &env.state_root);
    assert!(
        poll_until_no_process_matches(&supervise_pattern, Duration::from_secs(10)),
        "a supervisor process still matches {supervise_pattern:?} after destroy"
    );
    assert!(
        poll_until_no_process_matches(&run_pattern, Duration::from_secs(10)),
        "a leg process still matches {run_pattern:?} after destroy"
    );

    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let _ = next_id;
    assert!(
        find_row(&list_payload, &workspace_id).is_none(),
        "the destroyed row must no longer be listed: {list_payload:?}"
    );

    env.kill_daemon_bounded().await;
}

/// ADR 0043 decision 33's destroy proof, the markerless-death case: BOTH
/// the supervisor AND the leg die with no end marker on disk. Destroy
/// still succeeds — the resumed authority re-executes the leg within the
/// same voyage (the supervisor's own recovery rule, exercised by
/// `capsule_resume_reexecutes_a_leg_that_ended_without_a_marker`) and the
/// subsequent `end_run` ends THAT leg — the stated policy, not a gap
/// this proof leaves open. Nothing survives, and the row is removed.
///
/// Codex review (2026-09-11): the authority is first ADOPTED across a
/// daemon restart (`restart_daemon_and_prove_adoption`,
/// `AuthorityAtRestart::Alive`) BEFORE it (and its leg) are killed, so no
/// watchdog exists for this row and the ONLY thing that can re-execute
/// the leg and then end it is `destroy_capsule_workspace`'s own resume
/// call — the row's ORIGINAL watchdog (installed by `workspace.create`)
/// could otherwise race to recover it first, and this test could pass
/// even with that resume call deleted. The "no end marker" precondition
/// this test's own name claims is checked directly
/// (`sot_log::verify::leg_carries_run_end_marker`) right after both
/// SIGKILLs, rather than only inferred from the recovery behaviour
/// afterward — destroy's own success proves nothing about markerlessness
/// on its own; an adopted-but-cleanly-ended leg would also let destroy
/// succeed.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_destroy_after_a_markerless_leg_death_leaves_nothing() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("dml");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "dml-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

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
        assert!(Instant::now() < list_deadline, "timed out waiting for the new capsule workspace to reach \"ready\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);

    let (leg_before, voyage_before) = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            let (report, _process) = sot_log::supervisor_client::query_status(&dir)
                .expect("query_status before the daemon restart");
            (
                report.leg.expect("a ready capsule has a leg"),
                report.voyage.expect("a ready capsule has a voyage"),
            )
        }
    })
    .await
    .unwrap();

    // Adopt across a daemon restart FIRST -- the authority survives, this
    // daemon lifetime never spawned it, so no watchdog exists for it; the
    // ONLY thing left that can re-execute and then end the leg is
    // `destroy_capsule_workspace`'s own resume call below.
    let (mut conn, mut next_id) = restart_daemon_and_prove_adoption(
        &env,
        conn,
        &workspace_id,
        &state_dir,
        &state_dir_path,
        leg_before,
        AuthorityAtRestart::Alive,
    )
    .await;

    kill_supervisor_only(&env.state_root);
    kill_leg_only(&env.state_root);

    // The "markerless" precondition this test is named for, checked
    // directly rather than only inferred from the recovery behaviour
    // afterward.
    let seg_dir = sot_log::supervisor::voyage_root_path(&state_dir_path, &voyage_before).join("seg");
    let carries_marker = tokio::task::spawn_blocking({
        let seg_dir = seg_dir.clone();
        let voyage = voyage_before.clone();
        move || sot_log::verify::leg_carries_run_end_marker(&seg_dir, &voyage, leg_before)
    })
    .await
    .unwrap()
    .expect("leg_carries_run_end_marker must read the SIGKILLed leg's own segment cleanly");
    assert!(
        !carries_marker,
        "the killed leg carries an end marker — this is not the markerless-death precondition this test claims"
    );

    // Re-executing the leg from scratch (no survivor to adopt) is
    // slower than a plain adoption — `destroy_capsule_workspace`'s
    // resume pre-step may still be settling past `Starting` when
    // `end_run` first reaches it (`EndRunOutcome::Starting`'s own doc:
    // retryable, never a failure). Retry exactly that one reason; any
    // other failure fails the test at once.
    let destroy_deadline = Instant::now() + BOUND.max(Duration::from_secs(30));
    loop {
        let destroy_req = serde_json::json!({ "workspace_id": workspace_id });
        let destroy_res = call(&mut conn, next_id, op::WORKSPACE_DESTROY, destroy_req).await;
        next_id += 1;
        if destroy_res.payload.get("error").is_none() {
            break;
        }
        let detail = destroy_res.payload.get("error").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            detail.contains("starting"),
            "workspace.destroy failed for a reason other than a still-settling resume: {:?}",
            destroy_res.payload
        );
        assert!(
            Instant::now() < destroy_deadline,
            "workspace.destroy never got past \"starting; retry\": last reply {:?}",
            destroy_res.payload
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let supervise_pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
    let run_pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "run", &env.state_root);
    assert!(
        poll_until_no_process_matches(&supervise_pattern, Duration::from_secs(10)),
        "a supervisor process still matches {supervise_pattern:?} after destroy"
    );
    assert!(
        poll_until_no_process_matches(&run_pattern, Duration::from_secs(10)),
        "a leg process still matches {run_pattern:?} after destroy"
    );

    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let _ = next_id;
    assert!(
        find_row(&list_payload, &workspace_id).is_none(),
        "the destroyed row must no longer be listed: {list_payload:?}"
    );

    env.kill_daemon_bounded().await;
}

/// ADR 0043 decision 33's destroy proof, the missing-state-dir case: a
/// row that was cleanly ended and stopped, then had its whole state dir
/// removed out from under it (an operator `rm -rf`, or an external
/// volume issue) — never a licence to recreate anything. `end_run`'s own
/// `!state_dir.is_dir()` check reports this as `state_dir_missing`
/// BEFORE it ever reaches the fence/leg proof, and
/// `destroy_capsule_workspace` maps that to a `Kept` outcome with the
/// SAME code on the wire — the row is neither removed nor is its
/// directory ever recreated, and it stays listed.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_destroy_on_a_missing_state_dir_reports_and_creates_nothing() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("dsm");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "dsm-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

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
        assert!(Instant::now() < list_deadline, "timed out waiting for the new capsule workspace to reach \"ready\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);

    // End the run and stop the authority cleanly (`sot_log::
    // supervisor_client` directly, mirroring this file's own doc on why:
    // proving the record is closed before ever touching the directory).
    let voyage = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            sot_log::supervisor_client::query_status(&dir)
                .expect("query_status before ending the run")
                .0
                .voyage
                .expect("a ready capsule has a voyage")
        }
    })
    .await
    .unwrap();
    tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::end_run(&dir, &voyage, "test: state_dir_missing proof")
    })
    .await
    .unwrap()
    .expect("end_run must succeed on a ready row");
    tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::stop(&dir)
    })
    .await
    .unwrap()
    .expect("stop the ended authority");
    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move { if try_query_status(dir).await.is_none() { Some(()) } else { None } }
        },
        BOUND,
        "the stopped supervisor's own lane to go silent",
    )
    .await;

    std::fs::remove_dir_all(&state_dir_path)
        .unwrap_or_else(|e| panic!("rm -rf the state dir {state_dir_path:?}: {e}"));
    assert!(!state_dir_path.exists(), "the state dir must actually be gone before destroy is asked");

    let destroy_req = serde_json::json!({ "workspace_id": workspace_id });
    let destroy_res = call(&mut conn, next_id, op::WORKSPACE_DESTROY, destroy_req).await;
    next_id += 1;
    assert_eq!(
        destroy_res.payload.get("code").and_then(|v| v.as_str()),
        Some("state_dir_missing"),
        "payload: {:?}", destroy_res.payload
    );
    assert!(
        !state_dir_path.exists(),
        "destroy on a missing state dir must never recreate it: {state_dir_path:?}"
    );

    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let _ = next_id;
    assert!(
        find_row(&list_payload, &workspace_id).is_some(),
        "a kept row must still be listed: {list_payload:?}"
    );

    env.kill_daemon_bounded().await;
}

/// Decision 33: a headless op (`pty.input`) resumes a row whose
/// supervisor died between two ops, in place, under the row's own guard —
/// `resume_if_absent` in place of a bare `phase_of` read. The surviving
/// leg (a SIGKILL of the authority alone never touches it, ADR 0041
/// Lifecycle) is ADOPTED by the resume, not replaced: the leg epoch is
/// unchanged.
///
/// Codex review (2026-09-11): the authority is first ADOPTED across a
/// daemon restart (`restart_daemon_and_prove_adoption`,
/// `AuthorityAtRestart::Alive`) BEFORE it is killed — an authority this
/// daemon merely adopted at boot gets no watchdog at all (decision 33),
/// so the ONLY thing that can bring the row back is whatever
/// `pty.input` itself does. Without this, the row's ORIGINAL watchdog
/// (installed by `workspace.create`) would race to restart it on its
/// own, and this test could pass even with `resume_if_absent` deleted.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_headless_input_resumes_a_row_whose_supervisor_died() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("hir");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "hir-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

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
        assert!(Instant::now() < list_deadline, "timed out waiting for the new capsule workspace to reach \"ready\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);

    let leg_before = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            sot_log::supervisor_client::query_status(&dir)
                .expect("query_status before killing the supervisor")
                .0
                .leg
        }
    })
    .await
    .unwrap()
    .expect("a ready capsule has a leg");

    // Adopt across a daemon restart FIRST -- the authority survives, this
    // daemon lifetime never spawned it, so no watchdog exists for it.
    let (mut conn, mut next_id) = restart_daemon_and_prove_adoption(
        &env,
        conn,
        &workspace_id,
        &state_dir,
        &state_dir_path,
        leg_before,
        AuthorityAtRestart::Alive,
    )
    .await;

    kill_supervisor_only(&env.state_root);

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let text = "echo sot-l1a-resume-marker";
    let input_deadline = Instant::now() + BOUND.max(Duration::from_secs(60));
    loop {
        let input_req = serde_json::json!({
            "workspace_id": workspace_id,
            "data_b64": STANDARD.encode(text.as_bytes()),
            "enter": true,
            "origin": "l1a-resume-test",
        });
        let input_res = call(&mut conn, next_id, op::PTY_INPUT, input_req).await;
        next_id += 1;
        if input_res.payload.get("error").is_none() && input_res.payload["ok"] == true {
            break;
        }
        assert!(
            Instant::now() < input_deadline,
            "pty.input never succeeded after the supervisor died: last reply {:?}",
            input_res.payload
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND.max(Duration::from_secs(30))).await;

    let leg_after = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            sot_log::supervisor_client::query_status(&dir)
                .expect("query_status after the resume")
                .0
                .leg
        }
    })
    .await
    .unwrap();
    assert_eq!(
        leg_after,
        Some(leg_before),
        "the leg epoch changed — the surviving leg was not adopted by the resume"
    );

    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::stop(&dir)
    })
    .await;

    env.kill_daemon_bounded().await;
}

/// Decision 33: resume-only intent — `resume_if_absent` never sends
/// `reset`. A row already `EndedNoRespawn` whose authority then dies
/// (SIGKILL, no graceful `stop`) must, on the next headless op, come back
/// reporting its OWN ended phase — never resurrected to "ready," and its
/// durable voyage pointer must be byte-identical (only `reset` — attach's
/// own retirement path, unchanged by this lane — ever rewrites it).
///
/// Codex review (2026-09-11): the authority is first ADOPTED across a
/// daemon restart (own inline restart, not
/// `restart_daemon_and_prove_adoption` — that helper polls for "ready",
/// which an `EndedNoRespawn` row never reaches) BEFORE it is killed, so
/// no watchdog exists for it and the ONLY thing that can answer the
/// headless op afterward is `resume_if_absent` itself.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_resume_never_resets_an_ended_row() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("nre");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "nre-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

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
        assert!(Instant::now() < list_deadline, "timed out waiting for the new capsule workspace to reach \"ready\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);

    let (original_status, _process) = sot_log::supervisor_client::query_status(&state_dir_path)
        .expect("query_status before ending the run");
    let voyage = original_status.voyage.expect("a ready capsule has a voyage");

    sot_log::supervisor_client::end_run(&state_dir_path, &voyage, "test end").expect("end_run over the lane");

    poll_until(
        || {
            let dir = state_dir_path.clone();
            async move {
                let report = try_query_status(dir).await?;
                (report.phase == sot_log::wire::SupervisorPhase::EndedNoRespawn).then_some(())
            }
        },
        BOUND,
        "the ended row's authority to settle into EndedNoRespawn",
    )
    .await;

    let pointer_path = sot_log::pointer::pointer_path(&state_dir_path);
    let pointer_before = std::fs::read(&pointer_path).expect("read the pointer before killing the authority");

    // Adopt across a daemon restart FIRST -- the resting authority
    // survives (ADR 0041 Lifecycle: `EndedNoRespawn` persists until an
    // explicit `stop`), this daemon lifetime never spawned it, so no
    // watchdog exists for it.
    env.kill_daemon_bounded().await;
    drop(conn);
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    kill_supervisor_only(&env.state_root);

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let settle_deadline = Instant::now() + BOUND;
    loop {
        let input_req = serde_json::json!({
            "workspace_id": workspace_id,
            "data_b64": STANDARD.encode(b"echo should-never-run"),
            "enter": true,
            "origin": "l1a-ended-test",
        });
        let input_res = call(&mut conn, next_id, op::PTY_INPUT, input_req).await;
        next_id += 1;
        assert_ne!(
            input_res.payload["phase"].as_str(),
            Some("ready"),
            "resume must never bring an ended row to ready: {:?}",
            input_res.payload
        );
        if input_res.payload["phase"].as_str() == Some("ended_no_respawn") {
            assert_eq!(input_res.payload["code"], "capsule_not_ready", "{:?}", input_res.payload);
            break;
        }
        assert!(
            Instant::now() < settle_deadline,
            "pty.input never settled to ended_no_respawn: last reply {:?}",
            input_res.payload
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let pointer_after = std::fs::read(&pointer_path).expect("read the pointer after the resume attempt");
    assert_eq!(
        pointer_before, pointer_after,
        "resume must never touch the durable voyage pointer — only reset does, and resume never resets"
    );

    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::stop(&dir)
    })
    .await;

    env.kill_daemon_bounded().await;
}

/// Decision 33, the stated policy: a leg that ended WITHOUT a marker
/// (both the authority AND its leg SIGKILLed — no graceful end, nothing
/// to adopt) is RE-EXECUTED by `--resume` within the SAME voyage — the
/// supervisor's own recovery rule, never a `reset`'s fresh one. Proven by
/// a strictly higher leg epoch (a genuinely new leg process) alongside an
/// unchanged voyage id.
///
/// Codex review (2026-09-11): the authority is first ADOPTED across a
/// daemon restart (`restart_daemon_and_prove_adoption`,
/// `AuthorityAtRestart::Alive`) BEFORE it (and its leg) are killed, so no
/// watchdog exists for this row and the ONLY thing that can re-execute
/// the leg afterward is `pty.input`'s own `resume_if_absent` call.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_resume_reexecutes_a_leg_that_ended_without_a_marker() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("rrl");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "rrl-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();

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
        assert!(Instant::now() < list_deadline, "timed out waiting for the new capsule workspace to reach \"ready\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let state_dir_path = PathBuf::from(&state_dir);

    let (leg_before, voyage_before) = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            let (report, _process) = sot_log::supervisor_client::query_status(&dir)
                .expect("query_status before killing supervisor and leg");
            (
                report.leg.expect("a ready capsule has a leg"),
                report.voyage.expect("a ready capsule has a voyage"),
            )
        }
    })
    .await
    .unwrap();

    // Adopt across a daemon restart FIRST -- the authority survives,
    // this daemon lifetime never spawned it, so no watchdog exists for
    // it.
    let (mut conn, mut next_id) = restart_daemon_and_prove_adoption(
        &env,
        conn,
        &workspace_id,
        &state_dir,
        &state_dir_path,
        leg_before,
        AuthorityAtRestart::Alive,
    )
    .await;

    kill_supervisor_only(&env.state_root);
    kill_leg_only(&env.state_root);

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let text = "echo sot-l1a-reexec-marker";
    let input_deadline = Instant::now() + BOUND.max(Duration::from_secs(60));
    loop {
        let input_req = serde_json::json!({
            "workspace_id": workspace_id,
            "data_b64": STANDARD.encode(text.as_bytes()),
            "enter": true,
            "origin": "l1a-reexec-test",
        });
        let input_res = call(&mut conn, next_id, op::PTY_INPUT, input_req).await;
        next_id += 1;
        if input_res.payload.get("error").is_none() && input_res.payload["ok"] == true {
            break;
        }
        assert!(
            Instant::now() < input_deadline,
            "pty.input never succeeded after the supervisor AND leg both died: last reply {:?}",
            input_res.payload
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND.max(Duration::from_secs(30))).await;

    let (leg_after, voyage_after) = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || {
            let (report, _process) = sot_log::supervisor_client::query_status(&dir)
                .expect("query_status after the re-execution");
            (report.leg, report.voyage)
        }
    })
    .await
    .unwrap();
    assert!(
        leg_after.expect("the re-executed row has a leg") > leg_before,
        "the leg epoch must be STRICTLY higher — a fresh leg must have been re-executed, not merely adopted"
    );
    assert_eq!(
        voyage_after.as_deref(),
        Some(voyage_before.as_str()),
        "re-execution stays within the SAME voyage — never a reset's fresh one"
    );

    let _ = tokio::task::spawn_blocking({
        let dir = state_dir_path.clone();
        move || sot_log::supervisor_client::stop(&dir)
    })
    .await;

    env.kill_daemon_bounded().await;
}

/// Decision 33: the watchdog's own Crash arm holds this row's guard from
/// BEFORE the restart-budget check THROUGH the backoff sleep and the
/// restart spawn itself — closing the exact window the OLD `starting`
/// claim left open (released the instant a leg exited, before any
/// backoff, so a stale attach landing mid-backoff was free to spawn a
/// second authority). A `pty.open` fired during backoff must simply WAIT
/// for the SAME guard rather than race a second spawn: across three
/// SIGKILL/backoff/respawn cycles, sampled continuously (not at a
/// handful of point-in-time checks that could straddle the one instant a
/// bug would show up), at most one `supervise` process may ever match —
/// and, since no second spawn ever races the first into the fence,
/// `EXIT_CONTENDED` (the "contended (70)" log line) must never appear.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn capsule_stale_attach_during_backoff_spawns_no_second_authority() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("sab");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "sab-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["tmux_session"].as_str().expect("tmux_session").to_string();

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

    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
    let log_path = env.state_root.join("sot").join("sotd.log");
    let backoff_needle = "capsule supervisor watchdog: crashed, restarting with --resume";

    // Continuous background sampler — the claim under test is about
    // EVERY instant across the whole run below, not a handful of
    // point-in-time checks. A query failure FAILS the test (Codex
    // review, 2026-09-11) rather than silently counting as "zero
    // processes" — a false "at most one" proves nothing.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let max_seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sampler_error = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let sampler = {
        let stop = stop.clone();
        let max_seen = max_seen.clone();
        let sampler_error = sampler_error.clone();
        let pattern = pattern.clone();
        tokio::spawn(async move {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let pattern = pattern.clone();
                let outcome = tokio::task::spawn_blocking(move || count_matching_processes(&pattern)).await;
                match outcome {
                    Ok(Ok(n)) => max_seen.fetch_max(n, std::sync::atomic::Ordering::Relaxed),
                    Ok(Err(e)) => {
                        *sampler_error.lock().unwrap() = Some(format!("pgrep failed: {e}"));
                        return;
                    }
                    Err(join_err) => {
                        *sampler_error.lock().unwrap() = Some(format!("sampler task panicked: {join_err}"));
                        return;
                    }
                };
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
    };

    for _cycle in 0..3 {
        let backoff_seen_before = count_log_occurrences(&log_path, backoff_needle);
        kill_supervisor_only(&env.state_root);

        // Synchronize on the watchdog's OWN backoff log line (Codex
        // review, 2026-09-11) — a NEW occurrence of the line the Crash
        // arm logs immediately before its backoff sleep — rather than a
        // fixed sleep, which proves nothing about whether the watchdog
        // has actually reached that point by the time this test fires
        // its own stale attach.
        let backoff_deadline = Instant::now() + BOUND;
        loop {
            if count_log_occurrences(&log_path, backoff_needle) > backoff_seen_before {
                break;
            }
            assert!(
                Instant::now() < backoff_deadline,
                "timed out waiting for the watchdog's own backoff log line to appear"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let pty_req = serde_json::json!({
            "cols": 80, "rows": 24, "user_switch": true, "target": target,
        });
        let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
        next_id += 1;
        assert_eq!(
            pty_res.payload["code"], "attach_direct",
            "a stale pty.open during backoff must still answer attach_direct once the guard frees up: {:?}",
            pty_res.payload
        );

        poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND.max(Duration::from_secs(45))).await;
    }

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = sampler.await;

    if let Some(e) = sampler_error.lock().unwrap().take() {
        panic!("process sampler failed: {e}");
    }
    assert!(
        max_seen.load(std::sync::atomic::Ordering::Relaxed) <= 1,
        "more than one supervise process matched at some sampled instant across the SIGKILL/backoff cycles"
    );

    let log_contents = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("could not read the daemon's own log {log_path:?}: {e}"));
    assert!(
        !log_contents.contains("contended (70)"),
        "sotd.log has a contended (70) line — a second authority raced the first's own restart"
    );

    let state_dir = state_dir_from_list(&mut conn, &mut next_id, &workspace_id).await;
    let _ = tokio::task::spawn_blocking(move || sot_log::supervisor_client::stop(&state_dir)).await;

    env.kill_daemon_bounded().await;
}

// --- ADR 0045 decision 2 (lane B3): `lane.connect`, the daemon-side lane
// bridge --- //

/// The raw bytes of a `lane.connect` request frame — split out of
/// [`lane_connect`] so a caller that needs to prove peek-buffer
/// preservation (Codex review SHOULD-FIX, 2026-09-11: the daemon's own
/// `read_frame` peek must consume EXACTLY this envelope and leave
/// whatever follows untouched for the raw pipe) can concatenate it with
/// the FIRST lane bytes and write both in ONE call, before ever reading
/// a reply.
#[cfg(target_os = "linux")]
fn lane_connect_envelope_bytes(target: &str, lane: &str, voyage_id: Option<&str>) -> Vec<u8> {
    let mut req = serde_json::json!({ "target": target, "lane": lane });
    if let Some(v) = voyage_id {
        req["voyage_id"] = serde_json::json!(v);
    }
    let mut bytes = serde_json::to_vec(&Frame::req(1, op::LANE_CONNECT, req)).expect("serialize lane.connect envelope");
    bytes.push(b'\n');
    bytes
}

/// A FRESH connection to `env.socket_path` whose only frame is
/// `lane.connect` (ADR 0045 decision 2: a dedicated connection, never
/// through `connect_and_hello`'s multiplexed control loop). Returns the
/// still-open connection — a `Conn` so a raw byte read/write afterward
/// (on success) shares the SAME `BufReader` the response frame was read
/// through, never a throwaway second reader that would lose whatever
/// piped bytes it already buffered past the envelope — alongside the
/// parsed response payload.
#[cfg(target_os = "linux")]
async fn lane_connect(env: &Env, target: &str, lane: &str, voyage_id: Option<&str>) -> (Conn, serde_json::Value) {
    lane_connect_with_payload(env, target, lane, voyage_id, &[]).await
}

/// [`lane_connect`]'s own general form: the connect envelope AND
/// `extra_payload` (raw bytes meant for the lane, once piped) are
/// written in ONE `write_all` call, BEFORE this function ever reads
/// anything back — the peek-buffer-preservation proof. Still returns
/// only after the `LaneConnectRes` frame itself has been read (through
/// the SAME `BufReader` the caller goes on to read any piped reply
/// from), exactly like [`lane_connect`].
#[cfg(target_os = "linux")]
async fn lane_connect_with_payload(
    env: &Env,
    target: &str,
    lane: &str,
    voyage_id: Option<&str>,
    extra_payload: &[u8],
) -> (Conn, serde_json::Value) {
    use tokio::io::AsyncWriteExt;
    let stream = poll_until(
        || async { try_connect(&env.socket_path).await },
        BOUND,
        "sotd's local socket to accept a connection",
    )
    .await;
    let mut conn = tokio::io::BufReader::new(stream);
    let mut combined = lane_connect_envelope_bytes(target, lane, voyage_id);
    combined.extend_from_slice(extra_payload);
    conn.write_all(&combined).await.expect("write lane.connect envelope (+ payload) in ONE call");
    let (frame, _blob) = tokio::time::timeout(BOUND, codec::read_frame(&mut conn))
        .await
        .unwrap_or_else(|_| panic!("lane.connect reply did not arrive within {BOUND:?}"))
        .expect("read_frame lane.connect reply");
    (conn, frame.payload)
}

/// A refused `lane.connect` closes the connection (ADR 0045 decision 2:
/// "Either direction closing ends both") — the next read observes
/// ordered EOF, never a hang.
#[cfg(target_os = "linux")]
async fn assert_lane_connect_closes(conn: &mut Conn) {
    use tokio::io::AsyncReadExt;
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(BOUND, conn.read(&mut buf))
        .await
        .expect("read after a lane.connect refusal within BOUND")
        .expect("read after a lane.connect refusal");
    assert_eq!(n, 0, "the connection must close (EOF) after a lane.connect refusal");
}

/// On a ready row, `lane: "supervisor"` pipes the real supervisor lane:
/// `Hello` AND `Status` written in ONE write call (the buffered-bytes
/// proof that the daemon never decodes a lane frame after its own reply
/// — it is a raw pipe, not a second parser) come back as `HelloOk` (own
/// pid matching `lane.connect`'s own report) and `StatusOk{phase:
/// Ready}`. Dropping the pipe releases the lane slot without disturbing
/// the row: `workspace.list` still reports "ready" afterward.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn lane_connect_supervisor_pipes_hello_and_status() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("lch");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "lch-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["tmux_session"].as_str().expect("tmux_session").to_string();

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;

    // Codex review SHOULD-FIX (2026-09-11): the connect envelope AND the
    // first lane bytes (Hello + Status) are written in ONE call, BEFORE
    // this test ever reads the LaneConnectRes reply -- the real
    // peek-buffer-preservation proof (the daemon's own `read_frame` peek
    // must consume EXACTLY the envelope and hand everything past it,
    // untouched, to the raw pipe; sending it only AFTER reading the
    // reply would never exercise that).
    let mut payload = sot_log::wire::encode_supervisor_request(&sot_log::wire::SupervisorRequest::Hello {
        proto: sot_log::wire::SUPERVISOR_PROTO_V1,
        build: sot_log::exchange::SUPERVISOR_LANE_BUILD_ID.to_string(),
    })
    .expect("encode hello");
    payload.extend(
        sot_log::wire::encode_supervisor_request(&sot_log::wire::SupervisorRequest::Status).expect("encode status"),
    );
    let (mut lane_conn, res) = lane_connect_with_payload(&env, &target, "supervisor", None, &payload).await;
    assert!(res.get("error").is_none(), "lane.connect refused: {res:?}");
    assert_eq!(res["ok"].as_bool(), Some(true), "lane.connect payload: {res:?}");
    let pid = res["pid"].as_u64().expect("pid");
    let created = res["created"].as_u64().expect("created");
    assert!(pid > 0, "pid must be a real process id: {res:?}");

    use tokio::io::AsyncReadExt;
    let mut splitter = sot_log::wire::FrameSplitter::new();
    let mut got_hello: Option<(u32, u64)> = None;
    let mut got_status: Option<sot_log::wire::SupervisorPhase> = None;
    let deadline = Instant::now() + BOUND;
    let mut buf = [0u8; 4096];
    while got_hello.is_none() || got_status.is_none() {
        assert!(Instant::now() < deadline, "timed out waiting for HelloOk+StatusOk over the piped lane");
        let n = tokio::time::timeout(BOUND, lane_conn.read(&mut buf))
            .await
            .expect("read piped bytes within BOUND")
            .expect("read piped bytes");
        assert!(n > 0, "the piped connection EOF'd before HelloOk+StatusOk arrived");
        let (frames, err) = splitter.feed(&buf[..n]);
        assert!(err.is_none(), "wire decode error over the piped supervisor lane: {err:?}");
        for f in frames {
            match f {
                sot_log::wire::DecodedFrame::SupervisorReply(sot_log::wire::SupervisorReply::HelloOk {
                    pid: hp,
                    created: hc,
                    ..
                }) => got_hello = Some((hp, hc)),
                sot_log::wire::DecodedFrame::SupervisorReply(sot_log::wire::SupervisorReply::StatusOk {
                    phase,
                    ..
                }) => got_status = Some(phase),
                other => panic!("unexpected frame over the piped supervisor lane: {other:?}"),
            }
        }
    }
    assert_eq!(
        got_hello,
        Some((pid as u32, created)),
        "the piped HelloOk's own pid+created must match lane.connect's own report"
    );
    assert_eq!(got_status, Some(sot_log::wire::SupervisorPhase::Ready));

    drop(lane_conn);

    // The lane slot was released, not the row itself -- workspace.list
    // still reports "ready" afterward.
    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;

    env.kill_daemon_bounded().await;
}

/// `lane: "voyage"` (with the id from a real `status` reply) pipes the
/// attach lane: `AttachClient::Hello{proto: ATTACH_PROTO_V2}` comes back
/// `AttachServer::HelloOk{proto: 2}`.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn lane_connect_voyage_pipes_the_attach_hello() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("lcv");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "lcv-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["tmux_session"].as_str().expect("tmux_session").to_string();

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;
    let state_dir = state_dir_from_list(&mut conn, &mut next_id, &workspace_id).await;

    let voyage_id = tokio::task::spawn_blocking({
        let dir = state_dir.clone();
        move || sot_log::supervisor_client::query_status(&dir).expect("query_status on the ready row").0.voyage
    })
    .await
    .unwrap()
    .expect("a ready capsule has a voyage");

    let (mut lane_conn, res) = lane_connect(&env, &target, "voyage", Some(&voyage_id)).await;
    assert!(res.get("error").is_none(), "lane.connect refused: {res:?}");
    assert_eq!(res["ok"].as_bool(), Some(true), "lane.connect payload: {res:?}");

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let hello = sot_log::wire::encode_attach_client(&sot_log::wire::AttachClient::Hello {
        proto: sot_log::wire::ATTACH_PROTO_V2,
    })
    .expect("encode attach hello");
    lane_conn.write_all(&hello).await.expect("write attach hello");

    let mut splitter = sot_log::wire::FrameSplitter::new();
    let mut buf = [0u8; 4096];
    let deadline = Instant::now() + BOUND;
    let proto = loop {
        assert!(Instant::now() < deadline, "timed out waiting for the attach lane's own HelloOk");
        let n = tokio::time::timeout(BOUND, lane_conn.read(&mut buf))
            .await
            .expect("read piped bytes within BOUND")
            .expect("read piped bytes");
        assert!(n > 0, "the piped connection EOF'd before HelloOk arrived");
        let (frames, err) = splitter.feed(&buf[..n]);
        assert!(err.is_none(), "wire decode error over the piped voyage lane: {err:?}");
        if let Some(f) = frames.into_iter().next() {
            match f {
                sot_log::wire::DecodedFrame::AttachServer(sot_log::wire::AttachServer::HelloOk { proto }) => break proto,
                other => panic!("unexpected frame over the piped voyage lane: {other:?}"),
            }
        }
    };
    assert_eq!(proto, sot_log::wire::ATTACH_PROTO_V2);

    drop(lane_conn);
    env.kill_daemon_bounded().await;
}

/// (a) The client HALF-closes its own write side — never a full drop —
/// on one pipe. ADR 0045 decision 2's own "either direction closing ends
/// both": `pipe_bidirectional` tears down the WHOLE pipe (both
/// directions) the instant EITHER copy direction completes, so the
/// client's OWN read side then also observes a bounded EOF — the
/// directly observable proof, from the client's own vantage point, that
/// a client-initiated half-close reaches the upstream lane and the
/// daemon closes back. A fresh `lane.connect` against the SAME row
/// afterward still succeeds — the lane concurrency slot was released,
/// not leaked. (b) `workspace.destroy` while a DIFFERENT pipe is open
/// ends the row out from under it: that pipe's own client-side read
/// observes a bounded EOF too — daemon-initiated closure, the opposite
/// direction from (a)'s client-initiated one; together these are the
/// bounded-EOF proof at both ends, both directions.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn lane_connect_closes_when_either_side_closes() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("lcc");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "lcc-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["tmux_session"].as_str().expect("tmux_session").to_string();

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;

    // (a) HALF-close the client's own write side (never a full drop) --
    // the daemon must still tear down BOTH directions, so THIS
    // connection's own read side observes a bounded EOF back.
    let (mut first_conn, res) = lane_connect(&env, &target, "supervisor", None).await;
    assert_eq!(res["ok"].as_bool(), Some(true), "first lane.connect payload: {res:?}");
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        first_conn.shutdown().await.expect("half-close the client's own write side");
        let mut buf = [0u8; 1];
        let n = tokio::time::timeout(BOUND, first_conn.read(&mut buf))
            .await
            .expect("read after a client half-close within BOUND")
            .expect("read after a client half-close");
        assert_eq!(
            n, 0,
            "a client half-close must still produce a bounded EOF back -- the daemon tears down BOTH directions once either one closes"
        );
    }
    drop(first_conn);
    let (second_conn, res) = poll_until(
        || {
            let env = &env;
            let target = &target;
            async move {
                let (conn, res) = lane_connect(env, target, "supervisor", None).await;
                (res["ok"].as_bool() == Some(true)).then_some((conn, res))
            }
        },
        BOUND,
        "a second lane.connect to succeed after the first client dropped",
    )
    .await;
    assert_eq!(res["ok"].as_bool(), Some(true), "second lane.connect payload: {res:?}");

    // (b) workspace.destroy while a pipe is still open -> the client's
    // own next read observes EOF within a bound.
    let destroy_req = serde_json::json!({ "workspace_id": workspace_id });
    // `next_id` has no further use on this connection (mirrors the
    // create/list/destroy test's own convention) — no further increment.
    let destroy_res = call(&mut conn, next_id, op::WORKSPACE_DESTROY, destroy_req).await;
    assert!(destroy_res.payload.get("error").is_none(), "workspace.destroy failed: {:?}", destroy_res.payload);

    let mut second_conn = second_conn;
    use tokio::io::AsyncReadExt;
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(30), second_conn.read(&mut buf))
        .await
        .expect("read after workspace.destroy within 30s")
        .expect("read after workspace.destroy");
    assert_eq!(n, 0, "the piped connection must EOF once workspace.destroy ends the row out from under it");

    env.kill_daemon_bounded().await;
}

/// A stopped row's dial IS the recovery trigger (ADR 0045 decision 2):
/// `lane: "supervisor"` on a row whose authority died resumes it in
/// place rather than answering absent. TWO CONCURRENT initial dials
/// (Codex review SHOULD-FIX, 2026-09-11: sequential connects plus one
/// `pgrep` snapshot afterward cannot prove absence of a transient extra
/// spawn while the race is still live) both succeed, and a background
/// sampler running continuously across the WHOLE recovery window proves
/// at most one `supervise` process ever matched at any sampled instant —
/// the resume, never a second racing authority.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn lane_connect_resumes_a_stopped_row() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("lcr2");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let (workspace_id, _state_dir_path) =
        create_ready_workspace_then_stop_its_supervisor(&env, &mut conn, &mut next_id, "lcr2-workspace").await;

    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let row = find_row(&list_payload, &workspace_id).expect("the row is still registered");
    let target = row["tmux_session"].as_str().expect("tmux_session").to_string();

    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let max_seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sampler = {
        let stop = stop.clone();
        let max_seen = max_seen.clone();
        let pattern = pattern.clone();
        tokio::spawn(async move {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let pattern = pattern.clone();
                let n = tokio::task::spawn_blocking(move || count_matching_processes(&pattern).unwrap_or(0))
                    .await
                    .unwrap_or(0);
                max_seen.fetch_max(n, std::sync::atomic::Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
    };

    let ((first_conn, res_a), (second_conn, res_b)) = tokio::join!(
        lane_connect(&env, &target, "supervisor", None),
        lane_connect(&env, &target, "supervisor", None),
    );
    assert!(res_a.get("error").is_none(), "lane.connect must resume the stopped row rather than refuse it: {res_a:?}");
    assert_eq!(res_a["ok"].as_bool(), Some(true), "first concurrent lane.connect payload: {res_a:?}");
    assert!(res_b.get("error").is_none(), "lane.connect must resume the stopped row rather than refuse it: {res_b:?}");
    assert_eq!(res_b["ok"].as_bool(), Some(true), "second concurrent lane.connect payload: {res_b:?}");

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND.max(Duration::from_secs(90))).await;

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = sampler.await;
    assert!(
        max_seen.load(std::sync::atomic::Ordering::Relaxed) <= 1,
        "more than one supervise process matched at some sampled instant across the two concurrent initial dials"
    );

    drop(first_conn);
    drop(second_conn);
    env.kill_daemon_bounded().await;
}

/// Every `lane.connect` refusal code this daemon can answer, each closing
/// the connection: an unknown `target` (`unknown_workspace`), the tmux
/// default row (`not_capsule`), `lane: "voyage"` with no `voyage_id`
/// (`bad_lane`), a bogus `voyage_id` on a ready row (`lane_absent`, the
/// supervisor owns leg respawn so this is never resumed), and a TERMINAL
/// row's own supervisor lane (`lane_absent` with `kind` present, and —
/// the row already has no live authority to begin with — no process
/// spawned by the refusal).
#[tokio::test]
#[cfg(target_os = "linux")]
async fn lane_connect_refusals() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("lcf");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    // Unknown target.
    let (mut s, res) = lane_connect(&env, "sot-be-no-such-row", "supervisor", None).await;
    assert_eq!(res["code"].as_str(), Some("unknown_workspace"), "{res:?}");
    assert_lane_connect_closes(&mut s).await;

    // The tmux default row -- Linux stays "tmux" until the bridge (ADR
    // 0043 decision 22).
    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let default_row = list_payload["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .find(|w| w["is_default"].as_bool() == Some(true))
        .cloned()
        .expect("a default workspace row");
    assert_eq!(default_row["runtime"], "tmux", "default row: {default_row:?}");
    let default_target = default_row["tmux_session"].as_str().expect("tmux_session").to_string();
    let (mut s, res) = lane_connect(&env, &default_target, "supervisor", None).await;
    assert_eq!(res["code"].as_str(), Some("not_capsule"), "{res:?}");
    assert_lane_connect_closes(&mut s).await;

    // A ready capsule row, shared by the two voyage-lane sub-cases below.
    let create_req = serde_json::json!({
        "label": "lcf-ready",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let ready_workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let ready_target = create_res.payload["tmux_session"].as_str().expect("tmux_session").to_string();
    poll_for_phase(&mut conn, &mut next_id, &ready_workspace_id, "ready", BOUND).await;

    // voyage lane, no voyage_id.
    let (mut s, res) = lane_connect(&env, &ready_target, "voyage", None).await;
    assert_eq!(res["code"].as_str(), Some("bad_lane"), "{res:?}");
    assert_lane_connect_closes(&mut s).await;

    // voyage lane, a bogus (but well-formed) id on an otherwise-ready row
    // -- the row's own `drawer.voyage` pointer IS valid, just for a
    // DIFFERENT voyage, so this is `voyage_mismatch` (the ownership
    // check, Codex review BLOCKER 2026-09-11), never a dial attempt at
    // all -- `lane_connect_refuses_a_voyage_id_the_target_row_does_not_own`
    // covers the two-row form of this same check.
    let (mut s, res) = lane_connect(&env, &ready_target, "voyage", Some("00000000-0000-0000-0000-000000000000")).await;
    assert_eq!(res["code"].as_str(), Some("voyage_mismatch"), "{res:?}");
    assert_lane_connect_closes(&mut s).await;

    env.kill_daemon_bounded().await;

    // A TERMINAL row, in its own fresh daemon (the default row's own
    // toml must carry `agent = "claude"` BEFORE boot -- incompatible
    // with the plain tmux default row exercised above). Mirrors
    // `capsule_row_with_an_unlaunchable_agent_reaches_terminal_and_is_destroyable`.
    let env2 = Env::new("lcft");
    env2.seed_default_capsule_toml("claude");
    let fake_claude_dir = env2.seed_fake_unlaunchable_claude();
    env2.spawn_sotd_with_prepended_path(&fake_claude_dir);
    let (mut conn2, mut next_id2) = connect_and_hello(&env2.socket_path).await;

    let list_payload = call(&mut conn2, next_id2, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id2 += 1;
    let default_row2 = list_payload["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .find(|w| w["is_default"].as_bool() == Some(true))
        .cloned()
        .expect("a default workspace row");
    let default_workspace_id2 = default_row2["workspace_id"].as_str().expect("workspace_id").to_string();
    let default_target2 = default_row2["tmux_session"].as_str().expect("tmux_session").to_string();

    let pty_req = serde_json::json!({
        "cols": 80, "rows": 24, "user_switch": true, "target": default_target2,
    });
    let pty_res = call(&mut conn2, next_id2, op::PTY_OPEN, pty_req).await;
    next_id2 += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);

    let terminal_deadline = Instant::now() + BOUND;
    loop {
        let id = next_id2;
        next_id2 += 1;
        let payload = call(&mut conn2, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &default_workspace_id2) {
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

    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env2.state_root);
    // The anti-flap authority exits within a couple hundred ms of its own
    // last spawn attempt (`capsule_row_with_an_unlaunchable_agent_...`'s
    // own doc); `workspace.list`'s "terminal" is a POINTER read, so it
    // can be observed a hair before the OS finishes reaping the just-
    // exited process -- poll rather than a single point-in-time pgrep.
    assert!(
        poll_until_no_process_matches(&pattern, BOUND),
        "a terminal row must settle to no live supervise process"
    );

    let (mut s, res) = lane_connect(&env2, &default_target2, "supervisor", None).await;
    assert_eq!(res["code"].as_str(), Some("lane_absent"), "{res:?}");
    assert!(res.get("kind").and_then(|v| v.as_str()).is_some(), "lane_absent must carry a kind field: {res:?}");
    assert_lane_connect_closes(&mut s).await;

    assert_eq!(
        count_matching_processes(&pattern).expect("pgrep"),
        0,
        "a terminal row's own lane.connect refusal must never spawn a supervise process"
    );

    env2.kill_daemon_bounded().await;
}

/// Codex review BLOCKER (2026-09-11): a `voyage_id` names a socket by id
/// ALONE — `Endpoint::connect_voyage_unchallenged`'s own `lane` argument
/// is the daemon-lane endpoint's namespace, ignored by both platform
/// endpoints — so nothing about the dial itself ties a voyage to the row
/// that owns it. Two ready rows, A and B: `{target: A, voyage_id: B's
/// voyage}` must refuse `voyage_mismatch` (checked against A's own
/// `drawer.voyage` pointer BEFORE any dial — never piping B's voyage
/// through A's row), closing the connection; `{target: A, voyage_id: A's
/// own voyage}` must still succeed, proving the check is a real
/// comparison and not an unconditional refusal.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn lane_connect_refuses_a_voyage_id_the_target_row_does_not_own() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("lcvm");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    // Every `workspace.create` needs its OWN project root (the daemon
    // refuses a duplicate one) -- a fresh directory per row, under this
    // env's own private tempdir, mirrors this file's own
    // `tmux_project_root` precedent.
    async fn create_ready(conn: &mut Conn, next_id: &mut u64, env: &Env, label: &str) -> (String, PathBuf) {
        let project_root = env._tmp.path().join(label);
        std::fs::create_dir_all(&project_root).expect("mkdir project_root");
        let create_req = serde_json::json!({
            "label": label,
            "project_root": project_root.to_string_lossy(),
            "runtime": "capsule",
        });
        let create_res = call(conn, *next_id, op::WORKSPACE_CREATE, create_req).await;
        *next_id += 1;
        assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
        let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
        let target = create_res.payload["tmux_session"].as_str().expect("tmux_session").to_string();
        poll_for_phase(conn, next_id, &workspace_id, "ready", BOUND).await;
        (target, state_dir_from_list(conn, next_id, &workspace_id).await)
    }

    let (target_a, state_dir_a) = create_ready(&mut conn, &mut next_id, &env, "lcvm-a").await;
    // Only B's voyage id is needed (never B's own target) -- the whole
    // point is dialing it THROUGH A.
    let (_target_b, state_dir_b) = create_ready(&mut conn, &mut next_id, &env, "lcvm-b").await;

    let voyage_a = tokio::task::spawn_blocking({
        let dir = state_dir_a.clone();
        move || sot_log::supervisor_client::query_status(&dir).expect("query_status on row A").0.voyage
    })
    .await
    .unwrap()
    .expect("row A has a voyage");
    let voyage_b = tokio::task::spawn_blocking({
        let dir = state_dir_b.clone();
        move || sot_log::supervisor_client::query_status(&dir).expect("query_status on row B").0.voyage
    })
    .await
    .unwrap()
    .expect("row B has a voyage");
    assert_ne!(voyage_a, voyage_b, "two freshly created rows must never share a voyage id");

    // A's target with B's voyage id -- must never pipe B's voyage
    // through A's row.
    let (mut s, res) = lane_connect(&env, &target_a, "voyage", Some(&voyage_b)).await;
    assert_eq!(res["code"].as_str(), Some("voyage_mismatch"), "{res:?}");
    assert_lane_connect_closes(&mut s).await;

    // A's target with A's OWN voyage id -- the check is a real
    // comparison, not an unconditional refusal.
    let (_s, res) = lane_connect(&env, &target_a, "voyage", Some(&voyage_a)).await;
    assert!(res.get("error").is_none(), "A's own voyage id against A's own target must succeed: {res:?}");
    assert_eq!(res["ok"].as_bool(), Some(true), "{res:?}");

    env.kill_daemon_bounded().await;
}
