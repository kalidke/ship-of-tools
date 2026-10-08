//! The capsule-only birth parent: a supervisor is forked by a process that is not the daemon, after the row's fence
//! is claimed, so the daemon's death neither ends an accepted birth nor lets a second birth take the fence from it.
//! The daemon's end is [`proxy`]; the parent's is [`parent`]; [`accept`] is one launch's acceptance; [`wire`] is the
//! private channel between them. Unix only: Windows creates the supervisor directly.

pub(crate) mod accept;
pub(crate) mod parent;
pub(crate) mod proxy;
pub(crate) mod wire;

use std::ffi::OsString;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

pub use proxy::DurableChild;

/// One supervisor launch, as the daemon builds it.
pub struct Spec {
    pub state_dir: PathBuf,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Where in `args` the parent inserts the claim flags: just after the supervisor's mode flag.
    pub inject_at: usize,
    pub env: Vec<(OsString, OsString)>,
    pub cwd: PathBuf,
}

/// The outcome of asking the parent to launch a supervisor.
pub enum Launched {
    /// The row's fence is held by a live authority or another birth's claim: nothing was forked.
    Contended,
    /// The supervisor is forked, its claim carried to its first act, and its target has exec'd.
    Child(DurableChild),
}

/// Start the daemon's durable parent now, if it is not already running.
pub fn start_parent() -> std::io::Result<()> {
    proxy::client().map(|_| ())
}

/// Launch `spec` through the daemon's durable parent, with `stderr` as the supervisor's standard error.
pub fn launch(spec: Spec, stderr: &std::fs::File) -> std::io::Result<Launched> {
    let wire = wire::LaunchSpec {
        id: 0,
        state_dir: spec.state_dir,
        program: spec.program.into_os_string().into_vec(),
        args: spec.args.into_iter().map(OsStringExt::into_vec).collect(),
        inject_at: spec.inject_at,
        env: spec
            .env
            .into_iter()
            .map(|(k, v)| (k.into_vec(), v.into_vec()))
            .collect(),
        cwd: spec.cwd,
    };
    let client = proxy::client()?;
    Ok(match client.launch(wire, stderr.as_raw_fd())? {
        Some(child) => Launched::Child(child),
        None => Launched::Contended,
    })
}
