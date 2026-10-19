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

/// Why a pointer that fits the install must still not apply on this host, or `None`: the stage records no prepare
/// (`prepared` is the reason it could not be read), or this host's role wants a backend and the prepare did not
/// instantiate the Julia environments, which the new checkout would meet with none. The arming host decided the prepare's
/// depth from its own entry, so the applier checks it against its own. A Windows target is exempt from the second: its
/// launcher instantiates a new version's environments at its first start, and a box with no Julia is a supported state.
pub(super) fn fit_refusal(wants_backend: bool, heals_at_start: bool, prepared: Result<bool, String>) -> Option<String> {
    match prepared {
        Err(why) => Some(format!("no record of a prepare for this stage: {why}")),
        Ok(false) if wants_backend && !heals_at_start => {
            Some("this host runs a backend and the prepare did not instantiate the Julia environments".into())
        }
        Ok(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::fit_refusal;

    // Role x prepare: only a backend role over an env-less prepare is refused for its depth; no record is refused for any role.
    #[test]
    fn a_pointer_applies_only_where_its_prepare_fits_the_hosts_role() {
        let missing = || Err("reading prepared.json: No such file or directory".to_string());
        // (wants_backend, windows, prepared, refused)
        let table = [
            (true, false, Ok(true), false),
            (true, false, Ok(false), true),
            (true, false, missing(), true),
            (false, false, Ok(true), false),
            (false, false, Ok(false), false),
            (false, false, missing(), true),
            (true, true, Ok(false), false),
            (true, true, missing(), true),
        ];
        for (wants, windows, prepared, refused) in table {
            let said = fit_refusal(wants, windows, prepared.clone());
            assert_eq!(said.is_some(), refused, "backend {wants} windows {windows} prepared {prepared:?}: {said:?}");
        }
    }
}
