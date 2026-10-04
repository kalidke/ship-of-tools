//! The ssh child every non-local dial spawns (isolation-plan.md §3 C3, as
//! amended by `dev/output/c3-second-connection-amendment.md`). Reaching a
//! daemon that is not on this box now means `ssh <target> '<PATH prelude>;
//! sotd stdio-bridge [--host <host>]'`, piped stdio, no shell on THIS end —
//! never an `ssh -L` forward. One recipe type and one argv builder here,
//! used by every Rust site that spawns one:
//!
//! - the frontend's control connection (`rust/frontend/src/net/transport/mod.rs`);
//! - its per-host lane attach (`crate::topology::lane_client`, this crate);
//! - its per-browser-connection page-proxy leg
//!   (`rust/frontend/src/pages.rs`).
//!
//! C10's shell helper (`comm-lib.sh`'s `sot_ssh_bridge`) spawns the same
//! child from the comm scripts, with its own implementation — shell cannot
//! call into this crate, so that side owns its own copy of the option set
//! and prelude, kept identical by convention rather than by sharing code.

use std::process::Stdio;

use crate::topology::endpoint::is_plain_host_name;

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
        Ok(Self { target: target.to_string(), host: host.map(str::to_string) })
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
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

/// `ssh`'s own option set: the relay unit's `-T`, `BatchMode` and `ServerAlive` options
/// (`crate::topology`'s `ExecStart`) plus `ConnectTimeout=10`, the bridge's own bound on a dead hub.
const SSH_OPTS: &[&str] = &["-T", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3", "-o", "ConnectTimeout=10"];

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

/// Spawn the child for a synchronous caller — the lane client, which
/// implements `sot_log::client::Client`'s blocking `&self` methods and so
/// cannot hold a tokio `Child`.
fn spawn_sync(recipe: &SshRecipe) -> std::io::Result<std::process::Child> {
    let (program, args) = argv(recipe);
    std::process::Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Spawn the child for an async/tokio caller — the frontend's control
/// connection and its page-proxy splice. `kill_on_drop` so a dropped
/// `Child` (the branch returning, the splice task ending) never leaves an
/// orphaned `ssh` running past its client — the async twin of
/// `BridgedClient`'s own explicit `Drop` on the sync side.
fn spawn_async(recipe: &SshRecipe) -> std::io::Result<tokio::process::Child> {
    let (program, args) = argv(recipe);
    tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}

/// Why a gated spawn did not start a child.
#[derive(Debug)]
pub enum SpawnError {
    /// The host's link is down; no ssh was started.
    LinkDown,
    Io(std::io::Error),
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::LinkDown => write!(f, "the host's link is down; no ssh was started"),
            SpawnError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SpawnError {}

/// One host's link state, shared by every frontend site that starts an ssh
/// login to it. While the link is down no gated spawn starts a child, so a
/// new start site cannot be written without asking the gate: the ungated
/// spawn functions above are private, and [`LinkGate::probe`] is the one
/// ungated spawn. Only the host's control transport writes the gate (up at
/// any hello reply, down when its session ends), and it is the only caller
/// of `probe`. A default gate is up.
#[derive(Debug, Clone, Default)]
pub struct LinkGate {
    down: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl LinkGate {
    pub fn is_up(&self) -> bool {
        !self.down.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Transport only.
    pub fn set_up(&self, up: bool) {
        self.down.store(!up, std::sync::atomic::Ordering::Release);
    }

    pub fn spawn_sync(&self, recipe: &SshRecipe) -> Result<std::process::Child, SpawnError> {
        if !self.is_up() {
            return Err(SpawnError::LinkDown);
        }
        spawn_sync(recipe).map_err(SpawnError::Io)
    }

    pub fn spawn_async(&self, recipe: &SshRecipe) -> Result<tokio::process::Child, SpawnError> {
        if !self.is_up() {
            return Err(SpawnError::LinkDown);
        }
        spawn_async(recipe).map_err(SpawnError::Io)
    }

    /// The one ungated spawn: the transport's own reconnect probe, which
    /// is what discovers that a link is back.
    pub fn probe(recipe: &SshRecipe) -> std::io::Result<tokio::process::Child> {
        spawn_async(recipe)
    }
}

/// After an async caller's own operation over a spawned child has
/// already failed, give a stderr drainer task a short bounded window to
/// finish landing the child's last line before giving up — stdout and
/// stderr are separate pipes with no ordering guarantee between them, so
/// a child that writes a diagnosis to stderr and closes stdout in the
/// same instant can otherwise be observed here before its line lands.
/// Async twin of `lane_client::BridgedClient`'s own (sync, blocking)
/// version of this same wait.
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
mod tests {
    use super::*;

    #[test]
    fn a_down_gate_spawns_no_child() {
        let recipe = SshRecipe::new("hub", None).unwrap();
        let gate = LinkGate::default();
        assert!(gate.is_up());
        gate.set_up(false);
        assert!(matches!(gate.spawn_sync(&recipe), Err(SpawnError::LinkDown)));
        assert!(matches!(gate.spawn_async(&recipe), Err(SpawnError::LinkDown)));
        assert!(matches!(gate.clone().spawn_sync(&recipe), Err(SpawnError::LinkDown)), "clones share one flag");
    }

    #[test]
    fn recipe_rejects_a_leading_dash_in_either_half() {
        assert!(SshRecipe::new("-oProxyCommand=x", None).is_err());
        assert!(SshRecipe::new("hub", Some("-oProxyCommand=x")).is_err());
    }

    #[test]
    fn recipe_rejects_grammar_outside_a_plain_host_name() {
        for bad in ["Hub", "hub;rm -rf", "hub name", "hub@host", ""] {
            assert!(SshRecipe::new(bad, None).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn argv_has_no_shell_and_the_stated_option_set() {
        let recipe = SshRecipe::new("hub", None).unwrap();
        let (program, args) = argv(&recipe);
        assert_eq!(program, "ssh");
        assert_eq!(
            args,
            vec![
                "-T", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3", "-o", "ConnectTimeout=10", "hub",
                "export PATH=\"$HOME/.local/share/sot/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH\"; sotd stdio-bridge",
            ]
        );
    }

    #[test]
    fn argv_appends_host_only_when_the_recipe_carries_one() {
        let recipe = SshRecipe::new("hub", Some("gamma")).unwrap();
        let (_, args) = argv(&recipe);
        assert!(args.last().unwrap().ends_with("sotd stdio-bridge --host gamma"));

        let no_host = SshRecipe::new("hub", None).unwrap();
        let (_, args) = argv(&no_host);
        assert!(args.last().unwrap().ends_with("sotd stdio-bridge"));
        assert!(!args.last().unwrap().contains("--host"));
    }

    #[test]
    fn display_matches_the_endpoint_grammar() {
        assert_eq!(SshRecipe::new("hub", None).unwrap().to_string(), "ssh:hub");
        assert_eq!(SshRecipe::new("hub", Some("gamma")).unwrap().to_string(), "ssh:hub/gamma");
    }
}
