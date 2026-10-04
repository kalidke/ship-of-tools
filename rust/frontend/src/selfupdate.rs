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
use sot_updater::prepare::{PrepareSpec, PreparedState};
use sot_updater::{Fetcher, InstallManifest, UpdaterConfig};

/// The guard chain, as a value: the install to act on, or the one sentence
/// saying why this box does nothing. A value rather than four early returns
/// because `--update-status` has to answer the same question, and a guard
/// evaluated twice is a guard that can disagree with itself — which is the
/// whole failure this reporting exists to end.
///
/// **Each reason is a whole message logged at `info`, not `debug`.** The paths
/// where this decides to do NOTHING are the ones a user reports ("it never
/// updated"), and at the default `info` filter a `debug!` line is dropped —
/// leaving a log with no self-update entry at all, indistinguishable from the
/// check never having run. That makes a missing manifest, a wrong role, an
/// already-current install, and a dev build all look identical in the one
/// artifact available for diagnosis. One line per launch is a cheap price for
/// a self-reporting updater; the acting/erroring paths were already visible.
fn guard() -> std::result::Result<InstallManifest, &'static str> {
    // Asks the build flags, not the string: a clean checkout on a release
    // tag prints the same bare version the release does, so a substring test
    // could not tell them apart (ADR 0030 §8 decision 31c).
    if !sot_protocol::is_release_build() {
        return Err("fe self-update: not a release build — hard guard, skipping (update with git pull + cargo build)");
    }
    if matches!(
        std::env::var("SOT_UPDATE_MODE").ok().as_deref().map(str::trim),
        Some("off")
    ) {
        return Err("fe self-update disabled: SOT_UPDATE_MODE=off");
    }
    let Some(install) = InstallManifest::for_current_exe() else {
        // The single most likely reason a release install never updates, and
        // the one that used to leave no trace at all. Name the file so the
        // fix is obvious from the line alone.
        return Err(
            "fe self-update: no install manifest at <prefix>/install.json — not a release install, skipping \
             (Windows: run scripts\\install-manifest.ps1; Linux/macOS: re-run install.sh)",
        );
    };
    if backend_owns_updates_here(&install) {
        return Err("fe self-update: the backend on this machine owns updates (declared topology, install.json, or the default), skipping");
    }
    Ok(install)
}

/// Spawn the startup self-check. Never blocks; all failures are log lines.
pub fn spawn_startup_selfcheck() {
    let current = app_version();
    let install = match guard() {
        Ok(install) => install,
        Err(why) => {
            tracing::info!(%current, "{why}");
            return;
        }
    };
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
    let me = sot_log::host::state_dir::host_name().unwrap_or_default();
    backend_owns_updates(topo.as_ref(), &me, install.daemon)
}

/// The repo this box watches and the updates root it stages into. Shared by
/// the startup pipeline and `--update-status` so the status reads the state of
/// the pipeline that actually runs here, not of a second one derived alike.
fn config(current: String) -> std::result::Result<UpdaterConfig, String> {
    let repo = sot_updater::identity::repo_from_env();
    let updates_root = sot_updater::resolve_updates_root().map_err(|e| format!("no updates root: {e}"))?;
    Ok(UpdaterConfig {
        repo,
        current_version: current,
        fetcher: Fetcher::from_env(),
        updates_root,
    })
}

/// How far this box's own pipeline has got on one release — the four facts
/// `update.check` answers on a daemon box, from the same probes.
struct Phase {
    /// Bytes of the asset downloaded but not yet committed. A progress
    /// reading: verification happens when `stage` resumes from it.
    partial_bytes: Option<u64>,
    staged: bool,
    prepared: bool,
    armed: bool,
}

/// Probe the pipeline's state for `id`. Bounded like the daemon's own probe
/// (`update::handle_update_check`): `prepared` shells out to git in the
/// versioned checkout, and a hung NFS path must degrade to "don't know"
/// rather than stall the caller — a startup thread or a one-shot CLI.
/// `None` on timeout: the caller decides what "don't know" means for it —
/// silently all-`false` is safe for the background path, but a diagnostic
/// must say it doesn't know rather than print a `false` it never measured.
async fn phase(cfg: &UpdaterConfig, id: &sot_updater::ReleaseIdentity) -> Option<Phase> {
    let probes = async {
        Phase {
            partial_bytes: sot_updater::partial_asset_bytes(&cfg.updates_root, id).await,
            staged: sot_updater::is_staged(&cfg.updates_root, id).await,
            prepared: PreparedState::matches(&sot_updater::stage_dir(&cfg.updates_root, id), id).await,
            armed: matches!(
                sot_updater::pending::read(&cfg.updates_root, &id.target).await,
                Ok(Some(p)) if p.identity == *id
            ),
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), probes)
        .await
        .ok()
}

async fn run(install: InstallManifest, current: String) {
    let cfg = match config(current) {
        Ok(cfg) => cfg,
        Err(why) => {
            tracing::warn!("fe self-update: {why}");
            return;
        }
    };
    let out = sot_updater::check_release(&cfg.repo, &cfg.current_version, &cfg.fetcher).await;
    if !out.update_available {
        tracing::info!(status = %out.status, current = %cfg.current_version, "fe self-update: no newer release — nothing to do");
        return;
    }
    let Some(id) = out.identity else { return };
    // Name the point this launch starts from. Every stage below short-circuits
    // on work a previous launch finished, so without this line a log cannot
    // distinguish a run that resumed a nearly-finished pipeline from one that
    // began again — the distinction four blind converge cycles turned on.
    let at = phase(&cfg, &id).await.unwrap_or(Phase {
        partial_bytes: None,
        staged: false,
        prepared: false,
        armed: false,
    });
    if at.armed {
        tracing::info!(tag = %id.tag, "fe self-update: already armed — the next converge or launcher start applies it, nothing to do this run");
        return;
    }
    tracing::info!(
        tag = %id.tag,
        staged = at.staged,
        prepared = at.prepared,
        partial_bytes = at.partial_bytes.unwrap_or(0),
        "fe self-update: newer release found — continuing from what is already on disk"
    );
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

/// `sot --update-status`: print how far this box's own self-update pipeline
/// has got, then exit.
///
/// A frontend-only box answers no `update.check` — that op reaches the daemon
/// it is attached to, which is on another machine and reports THAT box's
/// pipeline. So the one state nobody could read was the state of the box that
/// had to move, and cycles were spent guessing at it from the outside. These
/// are the same four facts from the same probes, printed where the only person
/// who can reach such a box is standing.
///
/// Reads and prints; it never stages, prepares or arms. A diagnostic that
/// could change the thing it reports would be one more writer racing the
/// startup pipeline for the staging lock.
pub fn print_status() -> ! {
    println!("{}", sot_protocol::version_line("sot"));
    let install = match guard() {
        Ok(install) => install,
        Err(why) => {
            println!("  {why}");
            std::process::exit(0);
        }
    };
    let cfg = match config(app_version()) {
        Ok(cfg) => cfg,
        Err(why) => {
            println!("  fe self-update: {why}");
            std::process::exit(1);
        }
    };
    println!("  install        {}", install.prefix.display());
    println!("  repo           {}", cfg.repo);
    println!("  updates root   {}", cfg.updates_root.display());
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            println!("  no runtime: {e}");
            std::process::exit(1);
        }
    };
    let code = rt.block_on(async {
        // Same 45 s ceiling the daemon's handler uses: a wedged network path
        // must degrade to a printed status, not hang at the keyboard.
        let check = sot_updater::check_release(&cfg.repo, &cfg.current_version, &cfg.fetcher);
        let Ok(out) = tokio::time::timeout(std::time::Duration::from_secs(45), check).await else {
            println!("  release        check unavailable: timed out");
            return 1;
        };
        let Some(id) = out.identity.filter(|_| out.update_available) else {
            println!("  release        none newer than {} ({})", cfg.current_version, out.status);
            return 0;
        };
        let Some(at) = phase(&cfg, &id).await else {
            println!("  probe timed out — the filesystem or git is not answering");
            return 1;
        };
        for line in phase_lines(&id, &at) {
            println!("{line}");
        }
        0
    });
    std::process::exit(code);
}

/// The body of `--update-status` for a release this box is chasing. Pure so
/// the `next` sentence can be pinned by a test: it is an instruction to the
/// one person standing at an unreachable box, and a wrong one leaves that box
/// stale (a converge DOES apply an already-armed update —
/// `scripts/relaunch-sot.ps1`'s `Invoke-PendingApply`; only arming was ever
/// the gap).
fn phase_lines(id: &sot_updater::ReleaseIdentity, at: &Phase) -> Vec<String> {
    let yes_no = |b: bool| if b { "yes" } else { "no" };
    vec![
        format!("  release        {} (newer than this build)", id.tag),
        match at.partial_bytes {
            Some(n) => format!(
                "  downloaded     {:.1} MiB of {} (unverified — re-checked when staging resumes)",
                n as f64 / (1024.0 * 1024.0),
                id.asset
            ),
            None => "  downloaded     nothing yet".to_string(),
        },
        format!("  staged         {}", yes_no(at.staged)),
        format!("  prepared       {}", yes_no(at.prepared)),
        format!("  armed          {}", yes_no(at.armed)),
        // The operator's actual question. Two readings of `downloaded` a
        // minute apart say whether the box is working or wedged; this says
        // what, if anything, to do about it.
        format!(
            "  next           {}",
            if at.armed {
                "nothing — the next converge or launcher start applies it"
            } else {
                "the next frontend start continues from the above; another converge interrupts the work in flight without losing the download"
            }
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use sot_updater::identity::DEFAULT_REPO;

    fn topo(text: &str) -> sot_protocol::topology::Topology {
        sot_protocol::topology::parse(text).expect("fixture must parse")
    }

    // The regression this pins: `install.role != "remote"` used to decide
    // this; install.json carries no role any more (plan step 6), so a
    // daemon box and a frontend-only box must still take the branch they
    // always did, now read from the declared topology first, install.json's
    // recorded `daemon` bit second (a listless box), `true` last.

    fn an_identity() -> sot_updater::ReleaseIdentity {
        sot_updater::ReleaseIdentity {
            repo: DEFAULT_REPO.to_string(),
            tag: "v9.9.9".into(),
            version: "9.9.9".into(),
            target: "x86_64-unknown-linux-musl".into(),
            asset: "sot-9.9.9-x86_64-unknown-linux-musl.tar.gz".into(),
            asset_sha256: "0".repeat(64),
        }
    }

    // What `--update-status` tells the one person who can reach a
    // frontend-only box. An interrupted stage must read as progress (bytes on
    // disk, and the next start continuing from them), never as a reason to
    // converge again — converging again is what four cycles did.
    #[test]
    fn an_interrupted_stage_reads_as_progress_not_as_a_reason_to_converge() {
        let at = Phase {
            partial_bytes: Some(44_040_192),
            staged: false,
            prepared: false,
            armed: false,
        };
        let lines = phase_lines(&an_identity(), &at).join("\n");
        assert!(lines.contains("downloaded     42.0 MiB of sot-9.9.9-"), "{lines}");
        assert!(lines.contains("staged         no"), "{lines}");
        assert!(lines.contains("next           the next frontend start continues"), "{lines}");
    }

    // Armed is the end of this box's own work: a converge applies it
    // (`relaunch-sot.ps1`'s Invoke-PendingApply), so the instruction must not
    // send anyone looking for a different ritual.
    #[test]
    fn an_armed_box_is_told_a_converge_applies_it() {
        let at = Phase {
            partial_bytes: None,
            staged: true,
            prepared: true,
            armed: true,
        };
        let lines = phase_lines(&an_identity(), &at).join("\n");
        assert!(lines.contains("downloaded     nothing yet"), "{lines}");
        assert!(lines.contains("armed          yes"), "{lines}");
        assert!(
            lines.contains("next           nothing — the next converge or launcher start applies it"),
            "{lines}"
        );
    }

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
