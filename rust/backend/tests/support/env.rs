//! The isolated test environment: `Env`, its spawn helpers and its teardown.

use super::*;

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
    /// [`comm_isolation_dirs`]'s own pair — every spawned daemon's `HOME`/
    /// `USERPROFILE`/`SOT_COMM_HOME` point here, never at the developer's
    /// real comm registry.
    pub home_root: PathBuf,
    pub comm_root: PathBuf,
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
        // `sot_log::attach_client::supervisor_client::*`/`sot_log::supervisor::journal::fence::*` call THIS
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
        let (home_root, comm_root) = comm_isolation_dirs(tmp.path());
        let socket_path = test_socket_path(runtime_tmp.path(), tag);
        Self {
            _tmp: tmp,
            _runtime_tmp: runtime_tmp,
            _state_root_tmp: state_root_tmp,
            daemon_project_root,
            workspace_project_root,
            state_root,
            config_root,
            home_root,
            comm_root,
            socket_path,
            daemon: RefCell::new(None),
            user_service_unit: RefCell::new(None),
        }
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
    /// `sot_log::host::state_dir::sot_state_dir()` reads `%LOCALAPPDATA%` on
    /// Windows / `$XDG_STATE_HOME` on Linux directly (no daemon CLI flag
    /// exists for it), and `rows/store/`'s own registry root reads
    /// `%XDG_CONFIG_HOME%`/`$XDG_CONFIG_HOME` on the respective platform —
    /// all overridden here so this process's capsule state and workspace
    /// registry both live under the SAME temp root a second `sotd` launch
    /// (the adoption leg of this test) can point at again. Every env var
    /// is set UNCONDITIONALLY (one shape, not a per-platform cfg split):
    /// the platform this daemon actually runs on only ever reads its own
    /// pair, so setting the other platform's var too is harmless.
    /// `SOT_SELF_HOST` is pinned so the per-host registry dir
    /// (`rows::store::declared_host`, which otherwise falls back to the
    /// real hostname) is a fixed, known name —
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
        let child = sotd_command()
            .arg("--socket")
            .arg(&self.socket_path)
            .arg("--project-root")
            .arg(&self.daemon_project_root)
            .env("LOCALAPPDATA", &self.state_root)
            .env("XDG_STATE_HOME", &self.state_root)
            .env("XDG_CONFIG_HOME", &self.config_root)
            .env("SOT_SELF_HOST", TEST_STATE_HOST)
            .env("SOT_RUNTIME_DIR", self._runtime_tmp.path())
            .env("HOME", &self.home_root)
            .env("USERPROFILE", &self.home_root)
            .env("SOT_COMM_HOME", &self.comm_root)
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
    /// `rows::store::load_toml` never scans.
    #[cfg(windows)]
    pub fn app_config_dir(&self) -> PathBuf {
        self.state_root.join("sot").join("config")
    }
    /// macOS lane: `not(windows)`, mirroring the SHIPPED
    /// `rows::store::app_config_dir`, whose non-Windows arm is
    /// `sot_log::host::state_dir::sot_config_dir()` (`$XDG_CONFIG_HOME`, else
    /// `$HOME/.config`) on every Unix, macOS included — so the Linux gate
    /// here was narrower than the behaviour it mirrors.
    #[cfg(not(windows))]
    pub fn app_config_dir(&self) -> PathBuf {
        self.config_root.join("sot")
    }

    /// Pre-write an ARBITRARY capsule row's own toml BEFORE `spawn_sotd`
    /// boots the daemon, with `runtime = "capsule"` and the given
    /// `agent` — the same registry path `rows::store::save`/`load_toml`
    /// use (`<app config dir>/workspaces-<SOT_SELF_HOST>/<slug>.toml`,
    /// [`Env::app_config_dir`]). Only `workspace_id`/`slug`/`project_root`
    /// are required for `load_toml` to treat this as canonical
    /// (`rows/store/`'s own doc); every other field the daemon needs
    /// defaults sensibly.
    ///
    /// 2026-09-04 amendment: `scan_disk` (`rows/store/`, which loads
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
    /// ever passed in this file) so `rows/anchor.rs`'s own boot seed resolves
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
        sot_log::test_exec::write_executable(
            &claude,
            b"#!/bin/sh\nexec /no/such/binary/sot-test-unlaunchable-claude\n",
        );
        dir
    }

    /// A4b: a fake `claude` that starts a child in a NEW session
    /// (`setsid`), which the leg's `killpg` cannot reach, then stays up
    /// itself. Returns the fakebin dir and the pidfile the escapee's
    /// session leader writes its pid to. The 120 s lifetimes cap any leak.
    #[cfg(target_os = "linux")]
    pub fn seed_fake_claude_with_escapee(&self) -> (PathBuf, PathBuf) {
        let dir = self._tmp.path().join("fakebin");
        std::fs::create_dir_all(&dir).expect("mkdir fakebin");
        let pidfile = self._tmp.path().join("escapee.pid");
        let claude = dir.join("claude");
        sot_log::test_exec::write_executable(
            &claude,
            format!(
                "#!/bin/sh\nsetsid sh -c 'echo $$ > \"{}\"; sleep 120; :' </dev/null >/dev/null 2>&1 &\nexec sleep 120\n",
                pidfile.display()
            ),
        );
        (dir, pidfile)
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
        self.spawn_sotd_with_path_and_env(prepend_dir, &[]);
    }

    /// [`Env::spawn_sotd_with_prepended_path`], plus `extra` env vars.
    #[cfg(target_os = "linux")]
    pub fn spawn_sotd_with_path_and_env(&self, prepend_dir: &Path, extra: &[(&str, &str)]) {
        let mut path = std::ffi::OsString::from(prepend_dir);
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let child = sotd_command()
            .arg("--socket")
            .arg(&self.socket_path)
            .arg("--project-root")
            .arg(&self.daemon_project_root)
            .env("LOCALAPPDATA", &self.state_root)
            .env("XDG_STATE_HOME", &self.state_root)
            .env("XDG_CONFIG_HOME", &self.config_root)
            .env("SOT_SELF_HOST", TEST_STATE_HOST)
            .env("SOT_RUNTIME_DIR", self._runtime_tmp.path())
            .env("HOME", &self.home_root)
            .env("USERPROFILE", &self.home_root)
            .env("SOT_COMM_HOME", &self.comm_root)
            .env("PATH", path)
            .envs(extra.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sotd");
        let previous = self.daemon.borrow_mut().replace(child);
        debug_assert!(previous.is_none(), "spawn_sotd_with_path_and_env called while a prior daemon was still tracked");
    }

    /// [`Env::spawn_sotd`], plus `extra` env vars — for a test-only knob
    /// (e.g. `SOT_TEST_SLOW_CAPSULE_ACTIVATION_MS`) that only one test
    /// needs, without adding a parameter to the shared `spawn_sotd`.
    /// Portable like `spawn_sotd` itself — must still compile on Windows.
    pub fn spawn_sotd_with_env(&self, extra: &[(&str, &str)]) {
        self.spawn_sotd_at(&sotd_program(), extra);
    }

    /// [`Env::spawn_sotd_with_env`] for the binary at `program`: a copy of the built `sotd`, or a link to one.
    pub fn spawn_sotd_at(&self, program: &Path, extra: &[(&str, &str)]) {
        let mut cmd = sotd_command_at(program);
        cmd.arg("--socket")
            .arg(&self.socket_path)
            .arg("--project-root")
            .arg(&self.daemon_project_root)
            .env("LOCALAPPDATA", &self.state_root)
            .env("XDG_STATE_HOME", &self.state_root)
            .env("XDG_CONFIG_HOME", &self.config_root)
            .env("SOT_SELF_HOST", TEST_STATE_HOST)
            .env("SOT_RUNTIME_DIR", self._runtime_tmp.path())
            .env("HOME", &self.home_root)
            .env("USERPROFILE", &self.home_root)
            .env("SOT_COMM_HOME", &self.comm_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("spawn sotd");
        let previous = self.daemon.borrow_mut().replace(child);
        debug_assert!(
            previous.is_none(),
            "spawn_sotd_at called while a prior daemon was still tracked"
        );
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
            .arg(setenv("SOT_SELF_HOST", std::ffi::OsStr::new(TEST_STATE_HOST)))
            .arg(setenv("SOT_RUNTIME_DIR", self._runtime_tmp.path().as_os_str()))
            .arg(setenv("PATH", &path))
            .arg(setenv("HOME", &home))
            .arg("--")
            .arg(sotd_program())
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
        sot_log::test_exec::write_executable(&stub, b"#!/bin/sh\necho 'stub: no user manager' >&2; exit 1\n");
        dir
    }

    /// LU4 review round 2, F4 (anchor tightened round 3, G2): the
    /// anchored `pgrep`/`pkill` pattern for every real leg THIS env's own
    /// daemon could ever have spawned, covering EITHER subcommand
    /// (`rows/spawn/detach.rs`'s own two spawn sites) against this env's
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

        // (2, Windows) the counterpart of the Linux leg sweep below: a
        // capsule outlives a killed daemon BY DESIGN (it breaks away from
        // the daemon's job), so without this a test's supervisors, `run`
        // legs and the agent trees under them stay alive after the test
        // ends. Supervisors first so no NEW leg appears after a pass's
        // own kill; `taskkill /T` takes each one's whole tree (the agent,
        // its cmd.exe and conhost.exe). Repeated until a pass finds none
        // or 5 s pass. Best-effort: never panics.
        #[cfg(windows)]
        {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let pids = own_capsule_pids(&self.state_root);
                if pids.is_empty() || Instant::now() >= deadline {
                    break;
                }
                for pid in pids {
                    let _ = Command::new("taskkill")
                        .arg("/F")
                        .arg("/T")
                        .arg("/PID")
                        .arg(pid.to_string())
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
                std::thread::sleep(Duration::from_millis(200));
            }
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

        }

        // (4) `_tmp`/`_runtime_tmp` remove themselves right after this
        // method returns — see the doc comment above.
    }
}
