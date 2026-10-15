//! The one place a suite builds a command for `sotd`: the built binary (`sotd_command`), or a copy or link of it for a
//! suite that tests how the daemon was started (`sotd_command_at`). Either inherits no `SOT_` variable the runner
//! holds, so a daemon reads only the variables its test sets. The built binary's path (`sotd_exe`) is private here.
//! `sotd_daemon_at` and `sotd_client_of` start a daemon, or a client that finds one through `local_endpoint()`, at a
//! label given by the caller; both refuse this box's own daemon's endpoint before anything starts. `end_daemon` ends a
//! serving daemon a suite started.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

fn sotd_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sotd"))
}

/// The path of the `sotd` binary, for a suite that runs it through another program (a shell) and so passes a path,
/// not a command.
#[allow(dead_code)] // only some suites run `sotd` through a shell
pub fn sotd_program() -> PathBuf {
    sotd_exe()
}

/// A command for `sotd` with every inherited `SOT_` variable removed.
pub fn sotd_command() -> Command {
    sotd_command_at(&sotd_exe())
}

/// [`sotd_command`] for the binary at `program`, a copy of the built `sotd` or a link to one, for a suite that tests
/// how the daemon was started. The same `SOT_` scrub.
#[allow(dead_code)] // only some suites start `sotd` from another path
pub fn sotd_command_at(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().to_ascii_uppercase().starts_with("SOT_") {
            cmd.env_remove(name);
        }
    }
    cmd
}

/// How long a stopped daemon's guard may take to end: the daemon's own end, after its fire's wait of at most 2 s, then
/// the guard's drain of what is left, within its 10 s bound (`daemon_children::guard::DRAIN_BOUND`).
#[cfg(target_os = "linux")]
const STOP_BOUND: std::time::Duration = std::time::Duration::from_secs(20);

/// Ends a serving daemon this suite launched, and reaps it. On Linux the launched process is the daemon's lifetime
/// guard and the daemon is its child: TERM, which the guard forwards, ends the daemon through its terminal (each
/// contained child's process group is killed), and the guard kills what is left of the daemon's tree before it exits,
/// as at a service manager's stop. A SIGKILL of the guard would end the daemon but leave what it started to end on its
/// own (ADR 0050, residual 7), and a REPL's or Pluto's X server never does. Only a guard that has not ended within
/// `STOP_BOUND` is killed. Elsewhere the launched process is the daemon, and it is killed.
#[allow(dead_code)] // only suites that start a serving daemon end one
pub fn end_daemon(daemon: &mut Child) {
    #[cfg(target_os = "linux")]
    if daemon.try_wait().ok().flatten().is_none() {
        // SAFETY: a signal to the child this suite spawned and has not reaped.
        unsafe { libc::kill(daemon.id() as libc::pid_t, libc::SIGTERM) };
        let deadline = std::time::Instant::now() + STOP_BOUND;
        while std::time::Instant::now() < deadline {
            if daemon.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    let _ = daemon.kill();
    let _ = daemon.wait();
}

/// A daemon label of this test process's own: `sot-test-<tag>-<pid>`.
#[allow(dead_code)] // only some suites start a daemon by label
pub fn own_label(tag: &str) -> String {
    format!("sot-test-{tag}-{}", std::process::id())
}

/// The endpoint `label` derives in this process's environment: the one `sotd --label <label>` binds and a client
/// given `SOT_BACKEND_LABEL=<label>` dials. Panics, before anything is started or dialled, when it is the endpoint
/// `local_daemon_label()` derives, this box's own daemon's: on Windows a label's endpoint is the per-user pipe
/// `\\.\pipe\sot-<user>-<label>`, which no runtime folder moves, so the label alone keeps a test off that daemon.
#[allow(dead_code)] // only some suites start a daemon by label
pub fn label_endpoint(label: &str) -> PathBuf {
    let endpoint = sot_protocol::session_socket_path(label);
    let own_daemon = sot_protocol::session_socket_path(sot_protocol::local_daemon_label());
    assert_ne!(
        endpoint, own_daemon,
        "label {label:?} derives this box's own daemon's endpoint; a test starts its daemon at a label of its own (own_label)"
    );
    endpoint
}

/// [`sotd_command`] for a daemon at `label`, `--label <label>`, after [`label_endpoint`]'s refusal.
#[allow(dead_code)] // only some suites start a daemon by label
pub fn sotd_daemon_at(label: &str) -> Command {
    label_endpoint(label);
    let mut cmd = sotd_command();
    cmd.arg("--label").arg(label);
    cmd
}

/// [`sotd_command`] for a client of the daemon at `label`, after the same refusal: `SOT_BACKEND_LABEL=<label>`, which a
/// subcommand's `local_endpoint()` reads before the default label. The daemon itself reads no `SOT_BACKEND_LABEL`.
#[allow(dead_code)] // only some suites start a daemon by label
pub fn sotd_client_of(label: &str) -> Command {
    label_endpoint(label);
    let mut cmd = sotd_command();
    cmd.env("SOT_BACKEND_LABEL", label);
    cmd
}
