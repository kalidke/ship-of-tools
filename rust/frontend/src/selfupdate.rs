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
//   - `-dev` builds never self-update (hard guard, same as the backend);
//   - `SOT_UPDATE_MODE=off` disables;
//   - no install manifest → not a release install → no-op (dev checkouts,
//     Windows dev launcher);
//   - role != "remote" → the BACKEND on this machine owns the full pipeline
//     (all-in-one / be-only); the FE staying out avoids a second writer
//     doing a julia-less prepare that could arm an env-less version.
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
pub fn spawn_startup_selfcheck() {
    let current = app_version();
    if current.contains("-dev") {
        tracing::debug!("fe self-update: dev build — hard guard, skipping");
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
        tracing::debug!("fe self-update: no install manifest — not a release install, skipping");
        return;
    };
    if install.role != "remote" {
        tracing::debug!(role = %install.role, "fe self-update: backend owns updates on this machine");
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
        tracing::debug!(status = %out.status, "fe self-update: nothing to do");
        return;
    }
    let Some(id) = out.identity else { return };
    tracing::info!(tag = %id.tag, "fe self-update: newer release found — staging in background");
    if let Err(e) = sot_updater::stage(&cfg, &id).await {
        tracing::warn!(tag = %id.tag, error = %e, "fe self-update: staging failed");
        return;
    }
    // Remote role: checkout only — julia envs are a backend-host concern.
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
