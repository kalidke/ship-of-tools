//! The ssh login every non-local dial makes (isolation-plan.md §3 C3, as
//! amended by `dev/output/c3-second-connection-amendment.md`). Reaching a
//! daemon that is not on this box means `ssh <target> '<PATH prelude>;
//! sotd stdio-bridge [--host <host>]'`, piped stdio, no shell on THIS end —
//! never an `ssh -L` forward. One recipe type and one argv builder here,
//! used by every Rust site that starts one:
//!
//! - the window's link (`crate::link::Link::start`), its one login per computer, which carries its control
//!   connection, its pages and its rows as channels;
//! - the daemon's own logins (the hub link, `sotd topology sync` and `dial_and_call`), started through containment
//!   from [`SshRecipe::command`].
//!
//! C10's shell helper (`comm-lib.sh`'s `sot_ssh_bridge`) spawns the same
//! child from the comm scripts, with its own implementation — shell cannot
//! call into this crate, so that side owns its own copy of the option set
//! and prelude, kept identical by convention rather than by sharing code.

use std::process::Stdio;

use crate::endpoint::is_plain_host_name;

/// Where an ssh child should connect, and — when that target is a hub
/// relaying on another daemon's behalf — which daemon. `target` is an ssh
/// destination (a `hosts.toml` hub, or a bare `--dial`-supplied name);
/// `host`, when set, is `ssh:<target>/<host>`'s own suffix, passed on to
/// `sotd stdio-bridge --host <host>` on the far end.
///
/// Constructible only through [`SshRecipe::new`] — never with the struct
/// literal from outside this module — so nothing downstream can hand
/// `argv` an unchecked half. `target` becomes ssh's own argv element (a
/// value beginning with `-` would otherwise be read as an ssh OPTION, e.g.
/// `-oProxyCommand=…`, run on THIS box); `host` is interpolated into the
/// remote command STRING a shell on the far end parses. Both are checked
/// against the same plain-host-name grammar `sotd topology plan` emits
/// hosts in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshRecipe {
    target: String,
    host: Option<String>,
    #[cfg(any(test, feature = "test-handshake-bound"))]
    test_command: Option<TestCommand>,
}

#[cfg(any(test, feature = "test-handshake-bound"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct TestCommand {
    program: std::path::PathBuf,
    args: Vec<std::ffi::OsString>,
}

impl SshRecipe {
    pub fn new(target: &str, host: Option<&str>) -> Result<Self, String> {
        if !is_plain_host_name(target) {
            return Err(format!("`{target}` is not a plain host name"));
        }
        if let Some(h) = host {
            if !is_plain_host_name(h) {
                return Err(format!("`{h}` is not a plain host name"));
            }
        }
        Ok(Self {
            target: target.to_string(),
            host: host.map(str::to_string),
            #[cfg(any(test, feature = "test-handshake-bound"))]
            test_command: None,
        })
    }

    /// Test builds only: every start this recipe drives runs `program` with `args` in place of `ssh`.
    #[cfg(any(test, feature = "test-handshake-bound"))]
    pub fn with_test_command(mut self, program: impl Into<std::path::PathBuf>, args: impl IntoIterator<Item = impl Into<std::ffi::OsString>>) -> Self {
        self.test_command = Some(TestCommand { program: program.into(), args: args.into_iter().map(Into::into).collect() });
        self
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    /// The login, not yet started: its argv (or a test build's fixture command) and three piped stdio. The window's
    /// link starts it (`Link::start`); the daemon starts it through containment.
    pub fn command(&self) -> std::process::Command {
        command(self)
    }
}

impl std::fmt::Display for SshRecipe {
    /// The same spelling `sotd topology plan` emits an `ssh:` endpoint in
    /// (`ssh:<target>` / `ssh:<target>/<host>`) — used for logging only;
    /// nothing re-parses this string.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.host {
            Some(h) => write!(f, "ssh:{}/{h}", self.target),
            None => write!(f, "ssh:{}", self.target),
        }
    }
}

/// The remote PATH prelude every spawned child's command string opens
/// with, so a non-interactive remote account whose shell doesn't source
/// the usual profile still finds `sotd` — the launchers' own
/// (`scripts/launch-sot.sh:229-231`; C10's shell helper spells the
/// identical string on its own side).
pub const PATH_PRELUDE: &str = r#"export PATH="$HOME/.local/share/sot/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH""#;

/// `ssh`'s own option set: the relay unit's `ExecStart` options (`crate::relay_units`), sharing off included
/// (`ControlMaster=no`, `ControlPath=none`, `ControlPersist=no`, so no master forks away from the tree the daemon
/// kills), plus `ConnectTimeout=10`, the bridge's own bound on a dead hub. The daemon's monitor starts its sampler's ssh
/// from the same list.
pub const SSH_OPTS: &[&str] = &["-T", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3", "-o", "ConnectTimeout=10", "-o", "ControlMaster=no", "-o", "ControlPath=none", "-o", "ControlPersist=no"];

/// How long one login may take to answer: the window's link waits this long for its `link.hello` answer (a whole login
/// comes before it: the connect, key exchange, authentication, the remote shell and `sotd stdio-bridge`), and the
/// window's connect replies over it and the daemon's own logins (`sotd topology sync`, the installer's probe) wait the
/// same. A shorter bound would kill a login that is merely slow, and its retry is one more login on a busy host.
pub const SSH_LOGIN_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// `recipe` → `ssh`'s own program name and argv, built with no shell on
/// this end: `target` is its own argv element (never folded into the
/// remote command string), the remote command is ONE further argv
/// element that `sotd stdio-bridge` command with `--host <host>` appended
/// only when the recipe carries one.
fn argv(recipe: &SshRecipe) -> (&'static str, Vec<String>) {
    let mut args: Vec<String> = SSH_OPTS.iter().map(|s| s.to_string()).collect();
    args.push(recipe.target.clone());
    let mut remote = format!("{PATH_PRELUDE}; sotd stdio-bridge");
    if let Some(h) = &recipe.host {
        remote.push_str(" --host ");
        remote.push_str(h);
    }
    args.push(remote);
    ("ssh", args)
}

/// The ssh command for `recipe`: its argv and three piped stdio.
fn command(recipe: &SshRecipe) -> std::process::Command {
    type Argv = (std::ffi::OsString, Vec<std::ffi::OsString>);
    #[cfg(any(test, feature = "test-handshake-bound"))]
    let fixture: Option<Argv> = recipe.test_command.as_ref().map(|f| (f.program.clone().into(), f.args.clone()));
    #[cfg(not(any(test, feature = "test-handshake-bound")))]
    let fixture: Option<Argv> = None;
    let (program, args) = fixture.unwrap_or_else(|| {
        let (program, args) = argv(recipe);
        (program.into(), args.into_iter().map(Into::into).collect())
    });
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    keep_no_descriptor(&mut cmd);
    cmd
}

/// Marks this process's descriptors close-on-exec through the one sweep the daemon's and the capsule's children use, so
/// the child keeps none past its exec. On macOS a descriptor is close-on-exec only after a second call, and the window
/// starts its ssh children from several threads, so one started between the two calls would keep it.
fn keep_no_descriptor(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: the hook runs in the child between fork and exec and makes only async-signal-safe calls.
        unsafe { cmd.pre_exec(sot_platform::host::inherit_none) };
    }
    #[cfg(not(unix))]
    let _ = cmd;
}

/// After an async caller's own operation over a spawned child has
/// already failed, give a stderr drainer task a short bounded window to
/// finish landing the child's last line before giving up — stdout and
/// stderr are separate pipes with no ordering guarantee between them, so
/// a child that writes a diagnosis to stderr and closes stdout in the
/// same instant can otherwise be observed here before its line lands.
pub async fn last_stderr_after_failure(
    last_stderr: &std::sync::Arc<std::sync::Mutex<Option<String>>>,
) -> Option<String> {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(500);
    loop {
        if let Some(line) = last_stderr.lock().ok().and_then(|g| g.clone()) {
            return Some(line);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}

#[cfg(test)]
#[path = "ssh_bridge_tests.rs"]
mod tests;
