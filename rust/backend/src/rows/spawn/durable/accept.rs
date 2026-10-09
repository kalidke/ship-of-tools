//! The parent's acceptance of one capsule launch: validate the request, claim the row's fence, fork the supervisor held
//! at its gate. A launch is accepted only with the claim in hand; before that nothing is forked, and a refusal leaves
//! nothing alive.

use super::wire::LaunchSpec;
use sot_log::host::process_tree::{Birth, Launch, Ready};
use sot_log::supervisor::birth_claim::BirthClaim;
use std::ffi::{OsStr, OsString};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a forked child gets to report that it is set up.
const READY_BOUND: Duration = Duration::from_secs(10);
/// The most arguments and environment entries a launch may carry.
const MAX_ENTRIES: usize = 8192;

/// Why a launch was not accepted.
#[derive(Debug)]
pub enum Refusal {
    /// The row's fence is held: a live authority, or another birth's claim. Nothing was forked.
    Contended,
    /// The request was invalid or a step failed; nothing is left alive.
    Failed(String),
}

/// An accepted launch: the claim on the row's fence, the supervisor forked and set up with its target held at the
/// gate, and the channel the supervisor takes the claim over on.
pub struct Accepted {
    pub birth: Birth,
    /// The parent's copy of the claim; the child holds the same locked open file description.
    pub claim: Option<BirthClaim>,
    /// The parent's end of the takeover channel; the child's writer is the only one left open.
    pub takeover: std::io::PipeReader,
    pub ready: Ready,
}

/// Hold the test harness's barrier for `phase` (a build with the daemon-lifetime faults only).
pub fn barrier(phase: &str) {
    #[cfg(feature = "daemon-lifetime-faults")]
    sot_log::test_barrier::hold(phase);
    #[cfg(not(feature = "daemon-lifetime-faults"))]
    let _ = phase;
}

fn absolute(path: &Path, what: &str) -> Result<(), Refusal> {
    path.is_absolute().then_some(()).ok_or_else(|| {
        Refusal::Failed(format!(
            "the launch's {what} is not an absolute path: {path:?}"
        ))
    })
}

fn failed(what: &str, e: impl std::fmt::Display) -> Refusal {
    Refusal::Failed(format!("{what}: {e}"))
}

/// Accept `spec`. `stderr` is the supervisor's standard error; `parent_only` are the descriptors the parent keeps for
/// itself (its channel to the daemon, the other births' gates and claims), which the new child must not hold.
pub fn accept(
    spec: &LaunchSpec,
    stderr: OwnedFd,
    parent_only: &[RawFd],
) -> Result<Accepted, Refusal> {
    let program = PathBuf::from(OsStr::from_bytes(&spec.program));
    absolute(&program, "program")?;
    absolute(&spec.state_dir, "state folder")?;
    absolute(&spec.cwd, "working folder")?;
    if spec.args.len() > MAX_ENTRIES
        || spec.env.len() > MAX_ENTRIES
        || spec.inject_at > spec.args.len()
    {
        return Err(Refusal::Failed(
            "the launch's argument list is out of bounds".into(),
        ));
    }

    // The folder first: the claim is a file in it. Created here, under the daemon's umask, so a row's fence exists
    // before any supervisor does.
    std::fs::create_dir_all(&spec.state_dir)
        .map_err(|e| failed("could not create the state folder", e))?;
    let claim = match BirthClaim::take(&spec.state_dir) {
        Ok(claim) => claim,
        Err(sot_log::Error::State(_)) => return Err(Refusal::Contended),
        Err(e) => return Err(failed("could not claim the row's authority fence", e)),
    };
    barrier("parent_accepted");

    let (takeover_reader, takeover_writer) =
        std::io::pipe().map_err(|e| failed("could not make the takeover channel", e))?;
    let cwd = std::fs::File::open(&spec.cwd)
        .map_err(|e| failed("could not open the working folder", e))?;

    let mut args: Vec<OsString> = spec
        .args
        .iter()
        .map(|a| OsStr::from_bytes(a).to_os_string())
        .collect();
    let flags = [
        "--claim-fd".to_string(),
        claim.as_raw_fd().to_string(),
        "--takeover-fd".to_string(),
        takeover_writer.as_raw_fd().to_string(),
    ];
    args.splice(
        spec.inject_at..spec.inject_at,
        flags.into_iter().map(OsString::from),
    );

    let mut launch = Launch::new(&program);
    launch
        .args(args)
        .env_clear()
        .cwd(OwnedFd::from(cwd))
        .stdio(2, stderr)
        .new_session(true);
    for (key, value) in &spec.env {
        launch.env(OsStr::from_bytes(key), OsStr::from_bytes(value));
    }
    launch
        .inherit_across_exec(claim.as_raw_fd())
        .inherit_across_exec(takeover_writer.as_raw_fd());
    launch.close_in_child(takeover_reader.as_raw_fd());
    for fd in parent_only {
        launch.close_in_child(*fd);
    }

    let mut birth = launch
        .begin()
        .map_err(|e| failed("could not fork the supervisor", e))?;
    // The child's copy of the takeover writer is the only one that may remain.
    drop(takeover_writer);
    let ready = match birth.ready(READY_BOUND) {
        Ok(ready) => ready,
        Err(e) => {
            // The child ends and is reaped before the claim is let go.
            drop(birth);
            drop(claim);
            return Err(failed("the forked supervisor did not become ready", e));
        }
    };
    barrier("parent_ready");
    Ok(Accepted {
        birth,
        claim: Some(claim),
        takeover: takeover_reader,
        ready,
    })
}

/// The fd of a `Birth`'s parent-only ends, for the children forked after it.
pub fn parent_only_of(accepted: &Accepted) -> Vec<RawFd> {
    let mut fds = accepted.birth.owned_fds();
    fds.push(accepted.takeover.as_raw_fd());
    if let Some(claim) = &accepted.claim {
        fds.push(claim.as_raw_fd());
    }
    fds
}
