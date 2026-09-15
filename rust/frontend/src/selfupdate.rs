// selfupdate.rs — frontend-side update staging (ADR 0030 Phase C3).
//
// A remote frontend runs on a DIFFERENT machine than the backend: the
// daemon's stage/prepare/arm pipeline can't put bits on this box, and the
// handshake gate can't be relied on to deliver an `update.check` (a breaking
// protocol mismatch kills the connection before any op runs — the exact
// situation an updater must survive). So the FE runs its own check → stage →
// prepare(checkout-only) → arm at process startup, independently of the
// control channel, through the same `sot-updater` mechanism the daemon uses.
// `sot-launch`'s sot-apply pick-up then applies at the next launch.
//
// Guards, in order:
//   - only an official release build self-updates (`is_release_build()`,
//     hard guard, same as the backend — see ADR 0030 §8 decision 31c for
//     why the version string cannot answer this);
//   - `SOT_UPDATE_MODE=off` disables;
//   - no install manifest → not a release install → no-op (dev checkouts,
//     Windows dev launcher);
//   - the backend on this machine owns updates (`backend_owns_updates_here`)
//     → the FE staying out avoids a second writer doing a julia-less prepare
//     that could arm an env-less version. Used to be `role != "remote"`;
//     install.json no longer records a role (plan step 6, dev/output/
//     topology-plan.md §D), so this reads the same declared topology the
//     installer and `sot-backend`'s own `update::backend_role_from_topology`
//     do (duplicated rather than shared: `sot-updater` is mechanism only,
//     this is policy, and each side already carries its own guards).
//
// Runs on a small dedicated thread + current-thread runtime so the winit
// main thread and the transport runtime never wait on it. Outcomes go to
// tracing; the in-UI badge is Phase C4 polish (the daemon's broadcast notify
// already surfaces availability to attached FEs).

use sot_protocol::app_version;
use sot_updater::prepare::PrepareSpec;
use sot_updater::{Fetcher, InstallManifest, UpdaterConfig};

const DEFAULT_REPO: &str = "kalidke/ship-of-tools";

/// Spawn the startup self-check. Never blocks; all failures are log lines.
///
/// **Every early return below logs at `info`, not `debug`.** The paths where
/// this decides to do NOTHING are the ones a user reports ("it never
/// updated"), and at the default `info` filter a `debug!` line is dropped —
/// leaving a log with no self-update entry at all, indistinguishable from the
/// check never having run. That makes a missing manifest, a wrong role, an
/// already-current install, and a dev build all look identical in the one
/// artifact available for diagnosis. One line per launch is a cheap price for
/// a self-reporting updater; the acting/erroring paths were already visible.
pub fn spawn_startup_selfcheck() {
    let current = app_version();
    // Asks the build flags, not the string: a clean checkout on a release
    // tag prints the same bare version the release does, so a substring test
    // could not tell them apart (ADR 0030 §8 decision 31c).
    if !sot_protocol::is_release_build() {
        tracing::info!(%current, "fe self-update: not a release build — hard guard, skipping (update with git pull + cargo build)");
        return;
    }
    if matches!(
        std::env::var("SOT_UPDATE_MODE").ok().as_deref().map(str::trim),
        Some("off")
    ) {
        tracing::info!("fe self-update disabled: SOT_UPDATE_MODE=off");
        return;
    }
    let Some(install) = InstallManifest::for_current_exe() else {
        // The single most likely reason a release install never updates, and
        // the one that used to leave no trace at all. Name the file so the
        // fix is obvious from the log line alone.
        tracing::info!(
            "fe self-update: no install manifest at <prefix>/install.json — not a release install, skipping \
             (Windows: run scripts\\install-manifest.ps1; Linux/macOS: re-run install.sh)"
        );
        return;
    };
    if backend_owns_updates_here(&install) {
        tracing::info!("fe self-update: the backend on this machine owns updates (declared topology, install.json, or the default), skipping");
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("sot-selfupdate".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!(error = %e, "fe self-update: no runtime");
                    return;
                }
            };
            rt.block_on(run(install, current));
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "fe self-update: could not spawn worker thread — self-update disabled this run");
    }
}

/// Pure: given the declared topology (if any), this box's own name, and
/// install.json's own recorded `daemon` bit (if any), does the backend on
/// this machine own the update pipeline? Priority order — mirrors
/// `sot-backend`'s own `update::backend_role_from_topology` (duplicated, not
/// shared: `sot-updater` is mechanism only, this is policy):
/// 1. the declared topology, when it names this host — canonical, can
///    change without a reinstall;
/// 2. else `recorded_daemon` — what THIS install was actually given at
///    install time, for a listless box the topology can't answer for (a
///    real shape: a frontend-only-over-ssh install with no hosts.toml);
/// 3. else `true` — the old default for every role but `remote`.
fn backend_owns_updates(
    topo: Option<&sot_protocol::topology::Topology>,
    me: &str,
    recorded_daemon: Option<bool>,
) -> bool {
    match topo.and_then(|t| t.host(me)) {
        Some(h) => h.daemon,
        None => recorded_daemon.unwrap_or(true),
    }
}

/// Does the backend on THIS machine own the update pipeline, so the FE
/// should stay out of it? See `backend_owns_updates` for the decision and
/// `spawn_startup_selfcheck`'s doc comment for the guard order this sits in.
fn backend_owns_updates_here(install: &InstallManifest) -> bool {
    let topo = sot_protocol::topology::load().ok().flatten().map(|(_, t)| t);
    let me = sot_log::state_dir::host_name().unwrap_or_default();
    backend_owns_updates(topo.as_ref(), &me, install.daemon)
}

async fn run(install: InstallManifest, current: String) {
    let repo = std::env::var("SOT_UPDATE_REPO")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_REPO.to_string());
    let updates_root = match sot_updater::resolve_updates_root() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "fe self-update: no updates root");
            return;
        }
    };
    let cfg = UpdaterConfig {
        repo,
        current_version: current,
        fetcher: Fetcher::from_env(),
        updates_root,
    };
    let out = sot_updater::check_release(&cfg.repo, &cfg.current_version, &cfg.fetcher).await;
    if !out.update_available {
        tracing::info!(status = %out.status, current = %cfg.current_version, "fe self-update: no newer release — nothing to do");
        return;
    }
    let Some(id) = out.identity else { return };
    tracing::info!(tag = %id.tag, "fe self-update: newer release found — staging in background");
    if let Err(e) = sot_updater::stage(&cfg, &id).await {
        tracing::warn!(tag = %id.tag, error = %e, "fe self-update: staging failed");
        return;
    }
    // The FE only ever reaches here when the backend doesn't own updates
    // (backend_owns_updates_here() was false): checkout only, julia envs
    // are a backend-host concern.
    let spec = PrepareSpec {
        identity: id.clone(),
        repo_dir: install.prefix.join("repo"),
        stage_dir: sot_updater::stage_dir(&cfg.updates_root, &id),
        origin_url: None,
        julia_bin: None,
        npm: false,
    };
    let state = match sot_updater::prepare::prepare(&spec).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(tag = %id.tag, error = %e, "fe self-update: prepare failed — not arming");
            return;
        }
    };
    match sot_updater::pending::arm(&cfg.updates_root, &id, &state.checkout, &state.commit).await {
        Ok(true) => {
            tracing::info!(tag = %id.tag, "fe self-update: armed — applies at next launch")
        }
        Ok(false) => tracing::info!(tag = %id.tag, "fe self-update: a newer/blocked arm exists"),
        Err(e) => tracing::warn!(tag = %id.tag, error = %e, "fe self-update: arming failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topo(text: &str) -> sot_protocol::topology::Topology {
        sot_protocol::topology::parse(text).expect("fixture must parse")
    }

    // The regression this pins: `install.role != "remote"` used to decide
    // this; install.json carries no role any more (plan step 6), so a
    // daemon box and a frontend-only box must still take the branch they
    // always did, now read from the declared topology first, install.json's
    // recorded `daemon` bit second (a listless box), `true` last.

    #[test]
    fn a_daemon_box_leaves_updates_to_the_backend() {
        let t = topo(
            "hub = \"hubbox\"\n\
             [host.hubbox]\n\
             daemon = true\n\
             [host.host-2]\n\
             daemon = true\n",
        );
        assert!(backend_owns_updates(Some(&t), "host-2", None));
    }

    #[test]
    fn a_frontend_only_box_self_updates() {
        let t = topo(
            "hub = \"hubbox\"\n\
             [host.hubbox]\n\
             daemon = true\n\
             [host.laptop]\n\
             frontend = true\n",
        );
        assert!(!backend_owns_updates(Some(&t), "laptop", None));
    }

    #[test]
    fn a_box_the_topology_does_not_name_falls_back_to_the_recorded_bit() {
        let t = topo("hub = \"hubbox\"\n[host.hubbox]\ndaemon = true\n");
        assert!(backend_owns_updates(Some(&t), "nowhere", None));
    }

    #[test]
    fn no_topology_and_no_recorded_bit_defaults_to_the_backend_owning_it() {
        assert!(backend_owns_updates(None, "anything", None));
    }

    // The ruling this closes: a listless frontend-only-over-ssh install has
    // no hosts.toml at all, so nothing but install.json's own recorded
    // `daemon: false` can tell the FE it owns its own updates here.
    #[test]
    fn a_listless_frontend_only_box_self_updates_per_its_recorded_bit() {
        assert!(!backend_owns_updates(None, "laptop", Some(false)));
    }

    #[test]
    fn a_listless_daemon_box_leaves_updates_to_the_backend_per_its_recorded_bit() {
        assert!(backend_owns_updates(None, "host-2", Some(true)));
    }

    // The declared topology is canonical and can change without a
    // reinstall — a stale recorded bit from install time must never win
    // over what the list says NOW.
    #[test]
    fn the_declared_topology_wins_over_a_stale_recorded_bit() {
        let t = topo(
            "hub = \"hubbox\"\n\
             [host.hubbox]\n\
             daemon = true\n\
             [host.laptop]\n\
             frontend = true\n",
        );
        // install.json still says "daemon: true" from before this host was
        // switched to frontend-only on the list.
        assert!(!backend_owns_updates(Some(&t), "laptop", Some(true)));
    }
}
