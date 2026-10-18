//! What the daemon's updater refuses before it arms or applies anything: a host that runs no sotd of its own, an install
//! the release is not above, a pinned or dev install, and a pointer that no longer fits (or cannot be checked against) the
//! install on disk. Pure over what the caller read; `update.rs` reads the install and acts on the answer.

use sot_updater::pending::{InstalledBuild, PendingPointer};
use sot_updater::{ReleaseIdentity, UnreadableInstall};

/// Does the declared topology name this host and give it neither `daemon` nor `frontend`? Such a host runs no sotd of its
/// own: on a shared home it is a guest of the prefix, and a pointer it arms is prepared without the Julia environments
/// the host that does run the daemon needs. A host the topology does not name (or no topology) is not taken for one.
pub(super) fn host_runs_no_sotd() -> bool {
    let topo = sot_protocol::topology::load().ok().flatten().map(|(_, t)| t);
    let me = crate::comm::mail::filer::comm_self_host();
    topo.is_some_and(|t| sot_protocol::topology::runs_no_sotd(&t, &me))
}

/// Why this daemon arms nothing into the install it runs from, or `None` when it may: this host runs no sotd
/// (see [`host_runs_no_sotd`]), or the release is not strictly above the install on disk (the process may be
/// older than the install).
pub(super) fn arm_refusal(foreign: bool, installed: Option<&InstalledBuild>, id: &ReleaseIdentity) -> Option<String> {
    if foreign {
        return Some("the topology declares this host neither daemon nor frontend".into());
    }
    let installed = installed?;
    if installed.is_dev_build() {
        return Some(format!("the installed {} is a dev build", installed.version));
    }
    if let Some(note) = installed.pin_note() {
        return Some(note);
    }
    (!installed.admits(&id.version)).then(|| format!("{} is not above the installed {}", id.tag, installed.version))
}

/// Why `update.apply` must not exit for the apply owner to flip to `pending`, or `None`: the same foreign-install rule,
/// and a pointer that no longer fits the install on disk, or that cannot be checked against one that cannot be read.
pub(super) fn apply_refusal(
    foreign: bool,
    installed: std::result::Result<Option<InstalledBuild>, UnreadableInstall>,
    pending: &PendingPointer,
) -> Option<String> {
    if foreign {
        return Some("the topology declares this host neither daemon nor frontend".into());
    }
    let why = sot_updater::pending::pointer_refusal(pending, installed)?;
    Some(format!("the armed {} will not apply: {why}", pending.identity.tag))
}
