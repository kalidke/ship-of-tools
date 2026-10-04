//! Launching a row's `sot-capsule supervise` detached: start-mode flags, the sibling check and the spawn per OS.

use crate::capsule_workspace::{capsule_supervisor_env, NESTING_ENV_VARS_TO_SCRUB};
use crate::workspaces::StartPermit;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};

/// `sot-capsule supervise`'s own `--first-leg-without --continue`, passed
/// only for [`StartMode::Start`] (a row's first-ever run) so that leg's own
/// producer argv starts a fresh claude conversation. The daemon never
/// passes it again for a resumed or restarted supervisor; the supervisor's
/// own self-heal, using this SAME token, is what strips `--continue` a
/// second time for a leg that follows an unstable one (see
/// [`agent_argv`]'s own doc).
pub fn first_leg_without_continue(mode: StartMode) -> &'static [&'static str] {
    match mode {
        StartMode::Start => &["--first-leg-without", "--continue"],
        StartMode::Resume => &[],
    }
}

/// `sot-capsule supervise`'s own start-mode flag.
pub fn mode_flag(mode: StartMode) -> &'static str {
    match mode {
        StartMode::Start => "--start",
        StartMode::Resume => "--resume",
    }
}

/// Mirrors `sot_log::supervisor::StartMode` (portable re-statement: that
/// type lives in a platform-gated module, and this crate's own pure
/// tests need to name a mode without pulling in a platform-specific
/// type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    Start,
    Resume,
}

/// The daemon's capsule runtime — spawning, watching, querying, and
/// ending a supervisor over `sot_log::attach_client::supervisor_client`. Platform
/// chosen by exactly TWO forks inside (ADR 0043 decision 22): the
/// capsule executable's name ([`CAPSULE_EXE`]) and the detach mechanism
/// ([`spawn_detached`]'s two twins) — everything else below is
/// byte-identical on both platforms. (A third fork, the adopted leg's
/// exit-status read, existed here before decision 33 deleted the
/// adopted-leg watch entirely — a watchdog now exists only for a
/// `Child` this daemon itself spawned.)
/// This platform's `sot-capsule` sibling file name — kept OUTSIDE `mod
/// runtime` so the daemon-startup sanity check right below it compiles
/// and runs even where that module does not (a Unix that is neither
/// Linux nor macOS). Duplicates `mod runtime`'s own `CAPSULE_EXE` value
/// rather than reaching across the cfg boundary — the two are pinned
/// together by the test below.
#[cfg(windows)]
const CAPSULE_SIBLING_NAME: &str = "sot-capsule.exe";
#[cfg(not(windows))]
const CAPSULE_SIBLING_NAME: &str = "sot-capsule";

/// Whether the `sot-capsule` sibling binary exists next to `daemon_exe`
/// and (on Unix) is executable — ADR 0043 decision 22's sibling-binary
/// contract, checked once at daemon startup (`main.rs`, right after arg
/// parsing, before the socket is bound). `false` here after an in-place
/// upgrade means a pre-0.6 `sot-apply` swapped only `sot`/`sotd` and left
/// the newer `sot-capsule` unstaged (finding 1, v0.6.5 macOS field
/// report): every capsule row then blinks "supervisor lane not
/// answering" forever while the journal claims a start that produced no
/// process. Pure path/metadata check, no `current_exe()` call, so it is
/// unit-testable against a plain temp directory.
pub fn capsule_sibling_present(daemon_exe: &Path) -> bool {
    let sibling = daemon_exe.with_file_name(CAPSULE_SIBLING_NAME);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(&sibling)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        sibling.is_file()
    }
}

/// `DETACHED_PROCESS` (Win32): the child gets no console of its own —
/// right for a background authority that is never an interactive
/// console session.
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
/// `CREATE_NEW_PROCESS_GROUP`: the supervisor becomes its own process
/// group, so a Ctrl+C delivered to the daemon's own console (if any)
/// never propagates to a process the daemon just detached from itself.
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// `CREATE_BREAKAWAY_FROM_JOB`: per MSDN, ignored when the calling
/// process is not itself in a job — so there is nothing to probe
/// first for the common case. When the daemon IS in a job whose limit
/// flags lack `JOB_OBJECT_LIMIT_BREAKAWAY_OK`, `CreateProcess` fails
/// `ERROR_ACCESS_DENIED` rather than silently dropping the flag —
/// the signal [`spawn_detached`] retries on, without this flag,
/// rather than refusing the launch: a contained daemon (CI; a
/// terminal that is itself inside a job) is a context the daemon
/// cannot change, only report (ADR 0043 decision 32, revised —
/// survival is the launcher's to grant, never the daemon's to refuse
/// over). Linux's own escape is not a job flag but a transient user
/// scope (`systemd-run --user --scope`) — see the Linux twin of
/// [`spawn_detached`] below for that platform's attempt-then-contained
/// shape.
#[cfg(windows)]
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
/// Win32 `ERROR_ACCESS_DENIED` — what a denied breakaway attempt
/// reports on `CreateProcess`; [`spawn_detached`]'s own retry signal.
#[cfg(windows)]
const ERROR_ACCESS_DENIED: i32 = 5;

/// The capsule executable's own file name — the FIRST of the three
/// forks decision 22 names. Resolved next to the daemon's own
/// executable ("the `sot-capsule` binary path: next to the daemon's
/// own executable (`current_exe().parent()`), which is where the
/// install layout puts it", ADR 0042 L1a) on both platforms.
#[cfg(windows)]
const CAPSULE_EXE: &str = "sot-capsule.exe";
/// `not(windows)`, not `target_os = "linux"`: the extensionless name
/// is a Unix fact, not a Linux one, and the release archive stages it
/// next to `sotd` on the macOS leg exactly as it does on the Linux
/// one — so this arm is already correct for the day `mod runtime`'s
/// own gate widens, and gating it narrower would only make that day's
/// diff bigger without naming an invariant of its own.
#[cfg(not(windows))]
const CAPSULE_EXE: &str = "sot-capsule";

pub fn sot_capsule_exe() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| {
        std::io::Error::new(ErrorKind::NotFound, "daemon executable has no parent directory")
    })?;
    Ok(dir.join(CAPSULE_EXE))
}

/// ADR 0043 decision 25: the supervisor's stderr is the daemon's OWN
/// log — a FRESH `O_APPEND` open onto it per spawn (the daemon need
/// not share its handle; `main.rs`'s `open_private_log_file` already
/// creates the file in append mode, untouched here), so an inherited
/// descriptor keeps its original inode across a daemon restart or an
/// `XDG_STATE_HOME` change — a long-lived supervisor keeps writing to
/// the log it was born with. When the daemon has no log file at all
/// (or this open fails for any other reason), the supervisor inherits
/// the daemon's OWN stderr instead — a daemon run by hand in a
/// terminal shows the supervisor's lines there. Called fresh from
/// INSIDE `build` below (never hoisted out), so the descriptor is
/// opened right alongside the rest of the command's own stdio wiring.
fn supervisor_stderr() -> Stdio {
    let log_path = crate::paths::state_dir().join("sotd.log");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&log_path)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::inherit())
}

/// Spawn `sot-capsule supervise <state_dir> <--start|--resume>
/// --survival <normal|degraded> --assume-no-rollback-target -- <agent
/// argv>` DETACHED, so the supervisor authority survives the
/// daemon's own exit — the daemon must not be its kill domain (ADR
/// 0042 L1a). `--survival` is decided by [`spawn_detached`]'s own
/// escape attempt, never guessed here; on Linux that SAME attempt
/// also decides `scoped`, `build`'s second parameter — whether the
/// head this closure constructs is `systemd-run --user --scope … --
/// <sot-capsule>` (the escape) or `<sot-capsule>` directly (bare) —
/// so the shared tail (`supervise`, `state_dir`, mode, survival,
/// `--assume-no-rollback-target`, argv, cwd, stdio, env) is written
/// ONCE regardless of which head it lands on (Codex review deletion:
/// the earlier design built the bare command first and REWROTE it
/// into the scoped one via `Command::as_std()` accessors afterward —
/// gone; this closure just branches on `scoped` up front instead, so
/// there is no second log-file open, no workspace id reconstructed
/// from a path, and no future `env_clear` call that replay could ever
/// silently lose). `scoped` is always `false` on Windows (no scope
/// concept there) — only how the head is built differs; only how the
/// result is actually detached — [`spawn_detached`], the second of
/// decision 22's three forks — differs per platform.
/// `--assume-no-rollback-target` is mandatory: `sot_log::supervisor::supervise`
/// itself refuses (exit 69) without it pre-U4. The nesting env vars
/// are scrubbed and `SOT_COMM_NAME` exported (Codex review finding
/// 9) — the same contract `boot_wrapper_command`'s tmux path already
/// gives every autostart workspace.
///
/// ADR 0043 decision 23: [`super::qualified_state_root`] runs BEFORE
/// the build — this is the ONE mechanism every capsule launch shares
/// (create, attach start-on-attach, boot resume, the watchdog's own
/// restart), so each of those paths refuses an
/// unqualified root exactly here rather than needing its own copy of
/// the check. `state_dir` (a subdirectory of the qualified root) is
/// deliberately NOT what gets checked — the root itself is, via a
/// fresh resolution matching `handlers.rs`'s own earlier check for a
/// `workspace.create` (both resolve the SAME env-derived root, so they
/// agree by construction, not by sharing a value across the wire).
///
/// A second refusal right after it: [`super::state_root_inside_project`]
/// against `cwd` (every caller passes its `project_root` here) —
/// unlike the root check above this one DOES need `state_dir`, since
/// nesting is a property of THIS row, not of the machine.
pub(crate) fn spawn_detached_supervisor(
    _permit: &StartPermit,
    sot_capsule_exe: &Path,
    state_dir: &Path,
    mode: StartMode,
    agent_argv: &[String],
    cwd: &Path,
    agent_name: &str,
    workspace_id: &str,
    slug: &str,
    agent_kind: &str,
    account: &str,
) -> std::io::Result<Child> {
    super::qualified_state_root().map_err(|msg| std::io::Error::new(ErrorKind::Unsupported, msg))?;
    if super::state_root_inside_project(state_dir, cwd) {
        return Err(std::io::Error::new(
            ErrorKind::Unsupported,
            format!(
                "state directory {state_dir:?} lies inside the project root {cwd:?}: this \
                 workspace's own file watcher would hold directory handles under it, and on \
                 Windows an open handle blocks the renames capsule publication depends on \
                 (point this machine's state root, {}, outside the project tree)",
                super::STATE_ROOT_HINT
            ),
        ));
    }
    let account_env_extra = crate::agents::env::account_spawn_env(agent_kind, account, cwd, workspace_id)?;
    let build = |survival: &str, scoped: bool| -> Command {
        let mut cmd = if scoped {
            let mut c = Command::new("systemd-run");
            c.arg("--user")
                .arg("--scope")
                .arg("--quiet")
                .arg("--collect")
                .arg("--description")
                .arg(format!("sot-capsule {workspace_id}"));
            // A4b: the unit name is the row's scope record.
            #[cfg(target_os = "linux")]
            c.arg("--unit").arg(super::row_scope::unit_name(state_dir));
            c.arg("--").arg(sot_capsule_exe);
            c
        } else {
            Command::new(sot_capsule_exe)
        };
        cmd.arg("supervise")
            .arg(state_dir)
            .arg(mode_flag(mode))
            .arg("--survival")
            .arg(survival)
            .arg("--assume-no-rollback-target")
            .args(first_leg_without_continue(mode))
            .arg("--")
            .args(agent_argv)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(supervisor_stderr());
        for var in NESTING_ENV_VARS_TO_SCRUB {
            cmd.env_remove(var);
        }
        for (k, v) in capsule_supervisor_env(workspace_id, slug, cwd, agent_name) {
            cmd.env(k, v);
        }
        for (k, v) in &account_env_extra {
            cmd.env(k, v);
        }
        cmd
    };
    spawn_detached(build, state_dir, workspace_id)
}

/// Decision 22's second fork: how a built `Command` is actually
/// detached from the daemon so the supervisor authority survives the
/// daemon's own exit.
///
/// Windows: attempts `CREATE_BREAKAWAY_FROM_JOB` alongside
/// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` —
/// `tokio::process::Command` re-exposes `.creation_flags()` natively
/// (no `std::os::windows::process::CommandExt` import needed, unlike
/// `std::process::Command`). A denied breakaway
/// (`ERROR_ACCESS_DENIED` — this daemon's own job forbids it: CI, or
/// a terminal that is itself inside a job) is not refused: the SAME
/// spawn is retried without the flag, logged once, and launched
/// `--survival degraded` — the daemon reports its containment, it
/// never fabricates it as an error (ADR 0043 decision 32, revised).
/// Any OTHER spawn error propagates unchanged. `scoped` has no
/// Windows meaning (no scope concept there) — `build` is always
/// called with `false`; `workspace_id` is the Linux twin's own
/// concern (its scoped `--description` string), unused here but
/// shared across the signature both platforms call through.
#[cfg(windows)]
fn spawn_detached(
    build: impl Fn(&str, bool) -> Command,
    state_dir: &Path,
    workspace_id: &str,
) -> std::io::Result<Child> {
    let _ = workspace_id;
    let mut cmd = build("normal", false);
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
    match cmd.spawn() {
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
            tracing::warn!(
                state_dir = ?state_dir,
                "capsule supervisor: this daemon's own job forbids breakaway; the supervisor \
                 is contained in it and will not outlive it (ADR 0043 decision 32)"
            );
            let mut cmd = build("degraded", false);
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
            cmd.spawn()
        }
        other => other,
    }
}

/// Bounds a wedged user bus so a launch never hangs.
///
/// This constant and the three items after it ([`STDERR_DRAIN_BOUND`],
/// [`drain_stderr_bounded`], [`user_scope_available`]) are correctly
/// Linux-only and stay that way: every one of them exists to bound a
/// `systemd-run --user --scope` probe, and the thing they are
/// escaping -- a `KillMode=control-group` user service whose cgroup
/// reaps everything the daemon leaves behind -- has no Darwin
/// counterpart. launchd's own reaping is by PROCESS GROUP, and
/// `pre_exec(setsid)` in the shared spawn below already leaves it;
/// so on macOS the bare detached spawn IS the normal-survival case
/// and there is nothing to probe, no degraded fallback to fall to,
/// and no bus to wedge. A macOS `spawn_detached` is therefore the
/// Linux one with the whole probe deleted, not a port of it -- which
/// is why these four have no `cfg(unix)` future and are left alone.
#[cfg(target_os = "linux")]
const USER_SCOPE_PROBE_BOUND: Duration = Duration::from_secs(5);

/// Bounds draining a probe child's stderr AFTER it has already
/// exited (Codex review, reproduced: extracted code took 7 s when a
/// wrapper exited but left stderr inherited by a still-running
/// GRANDCHILD — `read_to_string` blocks until EVERY holder of the
/// pipe's write end closes it, not just the immediate child whose own
/// exit [`USER_SCOPE_PROBE_BOUND`]'s loop already observed). A
/// separate bound from that one: the process-exit wait and the
/// stderr drain can each hang for their own, independent reason.
#[cfg(target_os = "linux")]
const STDERR_DRAIN_BOUND: Duration = Duration::from_secs(1);

/// Drains `pipe` to EOF or [`STDERR_DRAIN_BOUND`], whichever comes
/// first, by reading it on a throwaway thread and joining that with a
/// bounded `recv_timeout` — the only way to cap a blocking
/// `read_to_string` without relying on the pipe's own non-blocking
/// mode. Past the bound the read is simply abandoned (its thread
/// leaks, but harmlessly: nothing else waits on it, and the pipe's
/// own fd closes when the thread eventually finishes or the process
/// exits) — the caller gets whatever text arrived in time, which for
/// a probe's own diagnostic stderr is "none" in the timeout case,
/// never a hang.
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

/// The escape [`spawn_detached`]'s Linux twin attempts before falling
/// back to a contained, degraded spawn (ADR 0043 decision 32): does a
/// reachable `systemd --user` manager grant a transient scope at all?
/// `systemd-run` fails before it ever execs the payload when it
/// cannot, indistinguishable from the payload's own instant death, so
/// this is a probe of the CAPABILITY (`/bin/true`), never a cache of
/// a past answer — a user manager can appear or vanish between
/// launches, and a launch is rare next to a whole supervisor's
/// lifetime. `Err`'s message is the probe's own stderr, verbatim
/// where there is any, drained under its own separate bound
/// ([`drain_stderr_bounded`]).
#[cfg(target_os = "linux")]
fn user_scope_available() -> std::io::Result<()> {
    let mut command = std::process::Command::new("systemd-run");
    command
        .args(["--user", "--scope", "--quiet", "--", "/bin/true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let deadline = Instant::now() + USER_SCOPE_PROBE_BOUND;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                ErrorKind::TimedOut,
                format!("systemd-run --user --scope did not answer within {USER_SCOPE_PROBE_BOUND:?}"),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if status.success() {
        return Ok(());
    }
    let stderr = child
        .stderr
        .take()
        .map(|pipe| drain_stderr_bounded(pipe, STDERR_DRAIN_BOUND))
        .unwrap_or_default();
    let stderr = stderr.trim();
    Err(std::io::Error::other(if stderr.is_empty() {
        format!("systemd-run --user --scope exited {status} with no stderr")
    } else {
        stderr.to_string()
    }))
}

/// Linux: attempts the platform's escape from the daemon's own kill
/// domain — a transient user scope, probed once per launch by
/// [`user_scope_available`]. A granted probe launches `build("normal",
/// true)` — the SAME closure that would have built the bare command,
/// just pointed at the `systemd-run … --scope` head instead (Codex
/// review deletion: no second construction, no
/// `Command::as_std()` replay of an already-built command); a denied
/// probe launches `build("degraded", false)`, one warn line naming
/// `workspace_id`, `state_dir` and the denial (ADR 0043 decision 32).
/// `pre_exec(setsid)` runs on EITHER head: a `systemd-run --scope`
/// child execs the supervisor in place (verified on systemd 249), so
/// the session id set here before `systemd-run`'s OWN exec survives
/// into the supervisor unchanged, same as the bare spawn. A spawn
/// error after a GRANTED probe propagates here unchanged — never a
/// retry into the bare branch.
///
/// The macOS twin below is this body minus the probe; see its own
/// doc for why that platform needs no escape.
///
/// `setsid`'s failure is PROPAGATED (review round, reproduced): in a
/// FRESH fork child, immediately post-fork, pre-exec, it cannot fail
/// for the "already a session/process-group leader" reason a plain
/// re-run of THIS process might (a fork always starts a brand-new
/// process that has never called `setsid` before) — `EPERM` here
/// means something else entirely denied it (a seccomp filter, most
/// plausibly), a real, reportable failure this must not silently
/// swallow: a detached supervisor spawned WITHOUT a new session would
/// stay attached to the daemon's own controlling terminal/session,
/// silently breaking the whole point of detaching it.
#[cfg(target_os = "linux")]
fn spawn_detached(
    build: impl Fn(&str, bool) -> Command,
    state_dir: &Path,
    workspace_id: &str,
) -> std::io::Result<Child> {
    let mut cmd = match user_scope_available() {
        Ok(()) => {
            tracing::info!(
                workspace_id,
                "capsule supervisor: launching in a transient user scope (ADR 0043 decision 32)"
            );
            build("normal", true)
        }
        Err(e) => {
            tracing::warn!(
                workspace_id,
                state_dir = ?state_dir,
                error = %e,
                "capsule supervisor: no transient user scope available; this supervisor \
                 shares the daemon's kill domain (ADR 0043 decision 32)"
            );
            build("degraded", false)
        }
    };
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn()
}

/// Every platform `sotd` ships for. The gate this module carried until
/// the supervisor-epoch ruling was never about what macOS can do:
/// `sot-capsule supervise` is built and shipped in the macOS release
/// archive, `sot_log::attach_client::supervisor_client` compiles there, the death watch
/// is a kqueue `NOTE_EXIT` knote and the parent-death lease works. The
/// single blocker was a daemon-side read of a FRESHLY SPAWNED
/// supervisor's identity, in the unit that supervisor would later report
/// over the wire — on macOS the kernel's `pidversion`, readable only out
/// of an audit token (`mach_task_self()` for oneself, a socket peer's
/// `LOCAL_PEERTOKEN`), neither of which exists for a child this daemon
/// has only just forked.
///
/// The ruling deleted the read rather than porting it: a supervisor's
/// identity is authored by the supervisor and learned over the lane, on
/// all three platforms. What macOS could not do, no platform now does —
/// so there is nothing left here to gate. Linux lost only earliness,
/// provably: `challenge_unix::self_start_ticks` (what a supervisor
/// reports) is literally `process_start_ticks(std::process::id())`, the
/// same read the daemon used to perform on the same pid, asserted by
/// `rust/log/tests/supervisor/`'s own equality test.
///
/// What this module's macOS arm does NOT claim: that a capsule row
/// actually works on a Mac. Nothing here has ever run on one. It
/// compiles honestly and spawns honestly; the ruling's §7 lists what a
/// real Mac must settle — that a supervisor's reported `pidversion`
/// matches what this daemon's own `query_status` sees for it, that a
/// `setsid` capsule survives its spawning `sotd` under launchd, that a
/// row reaches `ready` with a real agent attached, and that a
/// bootstrap-failing capsule latches `TerminalUnclaimed`.
/// macOS: the Linux twin's body minus the probe. There is no
/// transient-scope equivalent to attempt — `systemd-run --user
/// --scope` is a systemd mechanism, not a Unix one — and none is
/// needed, so `"normal"` is the honest survival value here rather
/// than a concession: launchd reaps a stopped job by killing its
/// process group unless `AbandonProcessGroup` is set, and `setsid`
/// puts the supervisor in a brand-new session and process group
/// before the exec, already outside that domain. `build`'s `scoped`
/// argument is therefore always `false`, as it is on Windows.
/// `setsid`'s failure propagates for exactly the reason the Linux
/// twin's does — see its own doc.
///
/// Unrun on a real Mac (`mod runtime`'s own gate doc, and the
/// ruling's §7 item 2): that a `setsid` capsule survives its
/// spawning `sotd`'s exit under launchd is a claim only a Mac
/// settles. The Linux cgroup hazard has no macOS analogue, which is
/// why this arm is plausible, not why it is proven.
#[cfg(target_os = "macos")]
fn spawn_detached(
    build: impl Fn(&str, bool) -> Command,
    _state_dir: &Path,
    _workspace_id: &str,
) -> std::io::Result<Child> {
    let mut cmd = build("normal", false);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn()
}

#[cfg(test)]
mod capsule_sibling_present_tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sot-capsule-sibling-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn present_when_executable_sibling_exists() {
        let dir = scratch_dir("present");
        let daemon = dir.join("sotd");
        let sibling = dir.join(CAPSULE_SIBLING_NAME);
        std::fs::write(&sibling, b"#!/bin/sh\n").expect("write sibling");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        assert!(capsule_sibling_present(&daemon));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn absent_when_sibling_missing() {
        let dir = scratch_dir("absent");
        let daemon = dir.join("sotd");
        assert!(!capsule_sibling_present(&daemon));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn absent_when_sibling_not_executable() {
        let dir = scratch_dir("noexec");
        let daemon = dir.join("sotd");
        let sibling = dir.join(CAPSULE_SIBLING_NAME);
        std::fs::write(&sibling, b"#!/bin/sh\n").expect("write sibling");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(!capsule_sibling_present(&daemon));
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod start_mode_tests {
    use super::*;

#[test]
fn mode_flag_matches_the_sot_capsule_cli() {
    assert_eq!(mode_flag(StartMode::Start), "--start");
    assert_eq!(mode_flag(StartMode::Resume), "--resume");
}

#[test]
fn first_leg_without_continue_only_on_start() {
    assert_eq!(first_leg_without_continue(StartMode::Start), ["--first-leg-without", "--continue"]);
    assert!(first_leg_without_continue(StartMode::Resume).is_empty());
}
}
