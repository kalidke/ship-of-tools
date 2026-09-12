//! Shared test support for this crate's real-process integration suites
//! (`capsule_workspaces.rs`, `lane_bridge.rs`): a real `sotd`, a real
//! `sot-capsule[.exe]` it spawns DETACHED, talking the actual wire
//! protocol over a real local socket. Lifted out of `capsule_workspaces
//! .rs` verbatim (ADR 0045 lane B4b) so `lane_bridge.rs`'s own cross-
//! process proofs — an attach client reaching a capsule row THROUGH a
//! daemon in the middle, over a test-owned TCP\u{2192}Unix relay — can
//! stand up the identical `Env`/wire-protocol fixture without a second,
//! drifting copy. Not itself a `tests/*.rs` file (Cargo only auto-
//! discovers direct children of `tests/` as integration-test binaries,
//! never a file inside a subdirectory), so each of the two real binaries
//! declares `mod support;` and gets its own compiled copy — no linkage
//! between them, no shared process state.
//!
//! Every wait below is a BOUNDED poll or `tokio::time::timeout` for an
//! external, observable fact — never a sleep-and-hope, and never an
//! unbounded read/write/kill/wait (Codex review finding 13, carried over
//! from `capsule_workspaces.rs`'s own header).

use std::cell::RefCell;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use interprocess::local_socket::tokio::{prelude::*, Stream as LocalStream};
use interprocess::local_socket::GenericFilePath;
use sot_protocol::{codec, op, Frame, HelloReq, Kind};
/// Every bounded wait in this file shares one figure — generous over any
/// single supervisor-lane round trip (connect 2s + hello 2s + status 5s
/// ~= 9s worst case) but still a real bound, never "forever."
pub const BOUND: Duration = Duration::from_secs(30);

/// Pinned `SOT_STATE_HOST` for every `spawn_sotd` in this file — a fixed,
/// known per-host registry dir name instead of whatever `%COMPUTERNAME%`
/// happens to be on the runner (`workspaces::state_host`'s fallback).
/// `Env::seed_default_capsule_toml` computes the same path from it.
pub const TEST_STATE_HOST: &str = "testhost";
pub fn sotd_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sotd"))
}
/// The capsule executable's own file name for this platform — mirrors
/// `capsule_workspace::runtime`'s own `CAPSULE_EXE` fork.
#[cfg(windows)]
pub const CAPSULE_EXE_NAME: &str = "sot-capsule.exe";
#[cfg(target_os = "linux")]
pub const CAPSULE_EXE_NAME: &str = "sot-capsule";
/// Resolved the same way production does — `current_exe().parent()` — but
/// from the TEST binary's own known sibling (`sotd[.exe]`'s own directory),
/// since a `tests/*.rs` binary itself lives in `target/<profile>/deps/`,
/// not `target/<profile>/`.
pub fn sot_capsule_exe() -> PathBuf {
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
pub async fn kill_and_wait_bounded(child: Child) {
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
pub async fn poll_until<T, F, Fut>(mut attempt: F, timeout: Duration, what: &str) -> T
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
pub struct Env {
    pub _tmp: tempfile::TempDir,
    /// L1-unix LU4 (ADR 0043 decision 1): `SOT_RUNTIME_DIR` — the private
    /// dir every real supervisor/voyage socket on Linux lives under,
    /// named by hash rather than nested under `state_root`. A SEPARATE,
    /// SHORT-prefixed `tempdir_in("/tmp")` (never under `_tmp`, whose own
    /// prefix is not size-bounded): `sun_path` is 108 bytes including the
    /// NUL on Linux, so keeping this dir's own path short leaves headroom
    /// for the `supervisor-<h>.sock`/`voyage-<uuid>.sock` suffix. Unused
    /// on Windows (named pipes have no such path-length concern) but kept
    /// unconditional — one `Env` shape on both platforms.
    pub _runtime_tmp: tempfile::TempDir,
    /// LU5a: `Some` only for [`Env::new_with_state_root_on_tmpfs`] — a
    /// SEPARATE tempdir (under `/dev/shm`, never under `_tmp`) that
    /// `state_root` itself lives in for that constructor, kept alive here
    /// for this `Env`'s whole lifetime. `None` for the ordinary
    /// [`Env::new`], where `state_root` is just a subdirectory of `_tmp`.
    pub _state_root_tmp: Option<tempfile::TempDir>,
    pub daemon_project_root: PathBuf,
    pub workspace_project_root: PathBuf,
    pub state_root: PathBuf,
    pub config_root: PathBuf,
    pub socket_path: PathBuf,
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
    pub daemon: RefCell<Option<Child>>,
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
    pub user_service_unit: RefCell<Option<String>>,
}

impl Env {
    pub fn new(tag: &str) -> Self {
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
    pub fn new_with_state_root_on_tmpfs(tag: &str) -> Self {
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
    pub fn tmux_sock(&self) -> PathBuf {
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
    pub async fn kill_daemon_bounded(&self) {
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
    pub fn spawn_sotd(&self) {
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
    pub fn app_config_dir(&self) -> PathBuf {
        self.state_root.join("sot").join("config")
    }
    #[cfg(target_os = "linux")]
    pub fn app_config_dir(&self) -> PathBuf {
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
    pub fn seed_capsule_toml(&self, workspace_id: &str, slug: &str, project_root: &Path, agent: &str) {
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
    pub fn seed_default_capsule_toml(&self, agent: &str) {
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
    pub fn seed_fake_unlaunchable_claude(&self) -> PathBuf {
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
    pub fn spawn_sotd_with_prepended_path(&self, prepend_dir: &Path) {
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
    pub fn spawn_sotd_as_user_service(&self) -> (String, u32) {
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
    pub fn forget_user_service(&self) {
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
    pub fn seed_stub_systemd_run(&self) -> PathBuf {
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
    pub fn leg_pgrep_pattern(&self) -> String {
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
pub fn build_leg_pgrep_pattern(exe: &Path, subcommand: &str, state_root: &Path) -> String {
    format!("^{} {subcommand} {}", regex_escape_path(exe), regex_escape_path(state_root))
}
/// Whether any live process's command line matches `pattern` — the
/// read-only half of the anchored sweep, reused by [`Env`]'s own `Drop`
/// (to poll the sweep to completion) and by the F4 cleanup-contract test
/// below (to prove both "before" and "after").
#[cfg(target_os = "linux")]
pub fn any_process_matches(pattern: &str) -> bool {
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
pub fn poll_until_no_process_matches(pattern: &str, timeout: Duration) -> bool {
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
pub fn user_manager_available_for_test() -> Result<(), String> {
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
pub fn stop_user_service(unit: &str, daemon_pid: u32) {
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
pub async fn try_connect(socket_path: &Path) -> Option<LocalStream> {
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
pub type Conn = tokio::io::BufReader<LocalStream>;
/// One request/reply round trip, itself bounded (Codex review finding
/// 13): write `payload` under `op`, then read frames until one with the
/// matching `id` arrives (any `Kind::Evt` broadcast in between — e.g.
/// `workspace.created` — is skipped, exactly as a real client's
/// steady-state loop routes it aside), all within `BOUND`.
pub async fn call(conn: &mut Conn, id: u64, op: &str, payload: serde_json::Value) -> Frame {
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
pub async fn connect_and_hello(socket_path: &Path) -> (Conn, u64) {
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
pub fn find_row(payload: &serde_json::Value, workspace_id: &str) -> Option<serde_json::Value> {
    payload["workspaces"].as_array()?.iter().find(|w| w["workspace_id"] == workspace_id).cloned()
}
/// One bounded `query_status` attempt — `Ok` when the lane answered,
/// `None` (not an error) when it is legitimately absent/unreachable,
/// which two of this test's own polls treat as the fact they're waiting
/// for (the old supervisor going away after `stop`).
pub async fn try_query_status(state_dir: PathBuf) -> Option<sot_log::supervisor_client::StatusReport> {
    tokio::task::spawn_blocking(move || {
        sot_log::supervisor_client::query_status(&state_dir)
            .ok()
            .map(|(report, _process)| report)
    })
    .await
    .unwrap_or(None)
}
/// Real-supervisor preamble shared by both lane-refusal tests below:
/// create a capsule workspace, wait for a REAL supervisor (this
/// checkout's own build) to reach "ready", then stop JUST the authority
/// (the leg survives, ADR 0041 Lifecycle) so the state dir carries a
/// published pointer with nothing currently answering its socket —
/// exactly the precondition [`spawn_lane_refusal_fixture`]'s caller needs
/// before binding in the real supervisor's place.
#[cfg(target_os = "linux")]
pub async fn create_ready_workspace_then_stop_its_supervisor(
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
pub async fn poll_for_phase(conn: &mut Conn, next_id: &mut u64, workspace_id: &str, want_phase: &str, timeout: Duration) {
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
