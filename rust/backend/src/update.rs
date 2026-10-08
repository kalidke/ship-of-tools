// update.rs — ADR 0030 §4 Phase-C updater POLICY: when to check, whom to
// notify, whether to stage.
//
// The mechanism — release discovery, fetch backends, digest verification,
// validated extraction, cross-process staging — lives in the shared
// `sot-updater` crate (the frontend embeds the same crate for its own
// platform's staging in Phase C2). What stays HERE is backend policy:
//
// - the periodic check task (first check ~2 min after boot, then daily),
// - the FE notify broadcast over the ADR 0025 daemon→FE command channel,
// - the `update.check` op handler,
// - the update mode (`SOT_UPDATE_MODE`: notify default, off; auto reserved),
// - the HARD GUARD: a build whose `app_version()` carries the `-dev` marker
//   never checks and never stages — the updater must not clobber a locally
//   built binary. Unconditional and independent of config.
//
// Fetch backend selection is `SOT_UPDATE_FETCHER` (curl default — the repo is
// public; gh for private forks; dir:<path> for sideload/testing), see
// `sot_updater::Fetcher::from_env`.

use anyhow::Result;
use serde_json::json;
use sot_protocol::{app_version, op, FeCommandEvt, Frame, UpdateApplyRes, UpdateCheckRes};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

use sot_updater::identity::repo_from_env;
use sot_updater::prepare::{PrepareSpec, PreparedState};
use sot_updater::{CheckOutcome, Fetcher, InstallManifest, ReleaseIdentity, UpdaterConfig};

use crate::server::reply::HandlerOutput;
use crate::lifecycle::lease::Leases;

/// Daemon policy: every updater command belongs to this signal's contained tree registry.
pub(crate) struct UpdaterSpawner(pub(crate) &'static crate::lifecycle::child_signal::Signal);

impl sot_updater::Spawner for UpdaterSpawner {
    fn output<'a>(
        &'a self,
        command: &'a mut tokio::process::Command,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<std::process::Output>> + Send + 'a>,
    > {
        Box::pin(async move {
            use tokio::io::AsyncReadExt;
            command
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true);
            let mut child = self.0.spawn(command)?;
            let mut stdout = child.stdout.take().expect("piped stdout");
            let mut stderr = child.stderr.take().expect("piped stderr");
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let (status, _, _) = tokio::try_join!(
                child.wait(),
                stdout.read_to_end(&mut out),
                stderr.read_to_end(&mut err)
            )?;
            Ok(std::process::Output {
                status,
                stdout: out,
                stderr: err,
            })
        })
    }
}

/// The updater's spawner: the process's one signal.
fn updater_spawner() -> UpdaterSpawner {
    UpdaterSpawner(crate::lifecycle::child_signal::process())
}

/// Delay before the first automatic check after boot, then the steady cadence.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(120);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 3600);

// ─── Config ─────────────────────────────────────────────────────────────

/// The wait before the first automatic check: two minutes, or, in the daemon-lifetime harness, the held point
/// `update-check-go` that the case opens when it is ready (`lifecycle::test_gates`).
async fn first_check_wait() {
    #[cfg(feature = "daemon-lifetime-faults")]
    if crate::lifecycle::test_gates::enabled() {
        crate::lifecycle::test_gates::wait("update-check-go").await;
        return;
    }
    tokio::time::sleep(FIRST_CHECK_DELAY).await;
}

/// Update behavior from `SOT_UPDATE_MODE` (ADR 0030 §4). `notify` (default):
/// stage + prepare + arm in the background, apply at next launch. `auto`:
/// additionally exit for the apply owner once armed, but ONLY while no
/// clients are attached. Caveat (documented in the ADR amendment): "no
/// clients" means no live daemon connections — detached tmux workspaces and
/// their REPLs can still be running, and an auto restart interrupts them;
/// `auto` is opt-in for exactly that reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    Notify,
    Auto,
}

fn mode_from_env() -> Mode {
    match std::env::var("SOT_UPDATE_MODE").ok().as_deref().map(str::trim) {
        Some("off") => Mode::Off,
        Some("auto") => Mode::Auto,
        Some("notify") | None | Some("") => Mode::Notify,
        Some(other) => {
            tracing::warn!(value = %other, "unknown SOT_UPDATE_MODE; defaulting to notify");
            Mode::Notify
        }
    }
}

/// Backend updater policy. Cheap to construct from env (config is
/// process-static), so the periodic task and each on-demand op make their own.
#[derive(Debug, Clone)]
pub struct Updater {
    /// Running product version (`app_version()`), e.g. `0.2.0` or
    /// `0.2.0-dev+abc`.
    current: String,
    /// True when `current` carries the `-dev` marker — the hard guard.
    dev: bool,
    mode: Mode,
    repo: String,
}

impl Updater {
    pub fn from_env() -> Self {
        let current = app_version();
        // NOT a substring test on `current`: a clean checkout parked on a
        // release tag prints a version indistinguishable from the release's
        // own by construction, so `contains("-dev")` used to pass it through
        // as a release install (ADR 0030 §8 decision 31c).
        let dev = !sot_protocol::is_release_build();
        // The daemon-lifetime harness drives the real update ops on a dev binary: the fault feature, and nothing else, lets
        // `SOT_TEST_RELEASE_BUILD` stand for a release build. An installed binary is built without the feature.
        #[cfg(feature = "daemon-lifetime-faults")]
        let dev = dev && std::env::var_os("SOT_TEST_RELEASE_BUILD").is_none();
        Self {
            dev,
            mode: mode_from_env(),
            repo: repo_from_env(),
            current,
        }
    }

    /// Mechanism config for the shared crate; errors when no updates root can
    /// be resolved (no install manifest and no usable env — never a temp dir).
    fn mechanism(&self) -> Result<UpdaterConfig> {
        Ok(UpdaterConfig {
            repo: self.repo.clone(),
            current_version: self.current.clone(),
            fetcher: Fetcher::from_env(),
            updates_root: sot_updater::resolve_updates_root()?,
        })
    }

    /// Query the latest release and compare against `current`. Never errors:
    /// a dev build / mode=off / unreachable release all map to a structured
    /// status. Deliberately does NOT require a staging root — hosts where no
    /// root resolves must still be able to report availability.
    async fn check(&self) -> CheckOutcome {
        let disabled = |status: &str| CheckOutcome {
            identity: None,
            latest: String::new(),
            update_available: false,
            status: status.into(),
        };
        if self.dev {
            return disabled("disabled: dev build");
        }
        if self.mode == Mode::Off {
            return disabled("disabled: update mode off");
        }
        sot_updater::check_release(
            &updater_spawner(),
            &self.repo,
            &self.current,
            &Fetcher::from_env(),
        )
        .await
    }
}

/// Process-local dedupe for the background pipeline: repeated `update.check`
/// ops during one in-flight multi-minute download must NOT pile up tasks
/// that all block on (their own process's) staging lock. One pipeline run at
/// a time per daemon; extra requests are a cheap no-op — the next check
/// reports the truth.
static PIPELINE_RUNNING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// (tag, reason) of the most recent stage that exhausted its commit-rename
/// backoff (Defect 0c), or `None` once a stage has since succeeded. A single
/// slot, not a map: only the LATEST release being chased can be blocked in a
/// way worth surfacing, and [`overlay_stage_block`] only applies it to a
/// check whose identity carries the SAME tag, so a superseding release's
/// check is never tainted by an older tag's failure.
static LAST_STAGE_BLOCK: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);

fn record_stage_block(tag: &str, reason: &str) {
    if let Ok(mut guard) = LAST_STAGE_BLOCK.lock() {
        *guard = Some((tag.to_string(), reason.to_string()));
    }
}

fn clear_stage_block() {
    if let Ok(mut guard) = LAST_STAGE_BLOCK.lock() {
        *guard = None;
    }
}

/// Overlay a recorded stage block onto a check outcome's status — the check
/// itself succeeded ("ok"), but the release it names never manages to
/// commit, and a bare "ok" is exactly how one box hid the fact that it
/// never updated for two releases running. Only applies when `out`'s
/// identity is the SAME tag that failed to commit.
fn overlay_stage_block(mut out: CheckOutcome) -> CheckOutcome {
    if let Some(id) = &out.identity {
        if let Ok(guard) = LAST_STAGE_BLOCK.lock() {
            if let Some((tag, reason)) = guard.as_ref() {
                if *tag == id.tag {
                    out.status = format!("update blocked: {reason}");
                }
            }
        }
    }
    out
}

/// Stage → prepare → arm: the full background pipeline for one discovered
/// release (Phase C2). Prepare and arm run only on release installs (an
/// install manifest exists) — a dev/canary box without one stops after the
/// stage, exactly the pre-C2 behavior. Julia envs are instantiated for
/// backend roles (`local`/`be-only`); `remote` prepares the checkout only.
async fn stage_prepare_arm(cfg: &UpdaterConfig, id: &ReleaseIdentity) {
    use std::sync::atomic::Ordering;
    if PIPELINE_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        tracing::debug!(tag = %id.tag, "update pipeline already running — skipping duplicate");
        return;
    }
    let result = stage_prepare_arm_inner(cfg, id).await;
    PIPELINE_RUNNING.store(false, Ordering::Release);
    result
}

async fn stage_prepare_arm_inner(cfg: &UpdaterConfig, id: &ReleaseIdentity) {
    let staged = sot_updater::stage(&updater_spawner(), cfg, id).await;
    if let Err(e) = staged {
        // The whole chain, not just the outermost context: the OS error is the
        // thing that names the fault, and `%e` drops it.
        let cause = e.chain().map(|c| c.to_string()).collect::<Vec<_>>().join(": ");
        tracing::warn!(tag = %id.tag, error = %cause, "staging update failed");
        // Defect 0c: the shared crate already retried the commit rename for
        // about a minute before giving up. Record it so the NEXT check
        // reports the concrete reason instead of a bare "ok" that hides a
        // release which never manages to land — the failure mode a healthy-
        // looking, never-updating box actually exhibited.
        record_stage_block(&id.tag, &cause);
        return;
    }
    // The commit landed — any block recorded for a PRIOR attempt (this tag
    // or an older one) no longer describes reality.
    clear_stage_block();
    let Some(install) = InstallManifest::for_current_exe() else {
        tracing::info!(tag = %id.tag, "staged (no install manifest — prepare/arm skipped)");
        return;
    };
    let spec = match prepare_spec(&install, cfg, id) {
        Ok(spec) => spec,
        Err(e) => {
            tracing::warn!(tag = %id.tag, error = %e, "preparing update failed — not arming");
            return;
        }
    };
    let state = match sot_updater::prepare::prepare(&updater_spawner(), &spec).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(tag = %id.tag, error = %e, "preparing update failed — not arming");
            return;
        }
    };
    match sot_updater::pending::arm(&cfg.updates_root, id, &state.checkout, &state.commit).await {
        Ok(true) => {}
        Ok(false) => tracing::info!(tag = %id.tag, "a newer release is already armed (or this one is marked bad)"),
        Err(e) => tracing::warn!(tag = %id.tag, error = %e, "arming update failed"),
    }
}

/// Pure: given the declared topology (if any), this box's own name, and
/// install.json's own recorded `daemon` bit (if any), does this box want a
/// backend prepared for the staged release (Julia envs, the MathJax
/// sidecar)? Priority order — see `backend_role_wanted`'s doc for why:
/// 1. the declared topology, when it names this host — canonical, can
///    change without a reinstall (D9, dev/output/topology-plan.md §D);
/// 2. else `recorded_daemon` — what THIS install was actually given at
///    install time (the list, or the flags), persisted for exactly a
///    listless box the topology can't answer for;
/// 3. else `true` — the old default for every role but `remote`, and the
///    only sane answer for a manifest from before this field shipped.
/// Split out from `backend_role_wanted` so the decision is testable against
/// a fixture, with no env vars or files involved
/// (`scripts/tests/installer-state.sh`'s `installer_running_daemon_decision`
/// is the same split, for the same reason).
fn backend_role_from_topology(
    topo: Option<&sot_protocol::topology::Topology>,
    me: &str,
    recorded_daemon: Option<bool>,
) -> bool {
    match topo.and_then(|t| t.host(me)) {
        Some(h) => h.daemon,
        None => recorded_daemon.unwrap_or(true),
    }
}

/// Does THIS box want a backend prepared for the staged release? Used to be
/// `install.role != "remote"` — install.json no longer records a role (plan
/// step 6, dev/output/topology-plan.md §D); the declared topology is now
/// asked first, in-process (`sot_protocol::topology`, read directly here —
/// no subprocess, unlike the installer's own `sotd topology status`), and
/// `install.daemon` — what THIS install was actually given, whether from the
/// topology or the flags, at install time — is the fallback for a listless
/// box the topology has no entry for (a real shape: a frontend-only-over-ssh
/// install with no hosts.toml at all).
fn backend_role_wanted(install: &InstallManifest) -> bool {
    let topo = sot_protocol::topology::load().ok().flatten().map(|(_, t)| t);
    let me = crate::comm::mail::filer::comm_self_host();
    backend_role_from_topology(topo.as_ref(), &me, install.daemon)
}

/// The julia an update's prepare runs for its envs: only a backend role runs one, and it is the resolver's answer, the
/// one every other daemon child runs, with the resolver's reason when there is none.
fn prepare_julia(backend_role: bool) -> Result<Option<String>, String> {
    backend_role.then(|| crate::sidecars::julia::resolve_bin().map(|(bin, _source)| bin)).transpose()
}

fn prepare_spec(install: &InstallManifest, cfg: &UpdaterConfig, id: &ReleaseIdentity) -> Result<PrepareSpec, String> {
    let backend_role = backend_role_wanted(install);
    Ok(PrepareSpec {
        identity: id.clone(),
        repo_dir: install.prefix.join("repo"),
        stage_dir: sot_updater::stage_dir(&cfg.updates_root, id),
        origin_url: None,
        julia_bin: prepare_julia(backend_role)?,
        npm: backend_role,
    })
}

/// Notify text ADR 0030 §4 specifies.
fn notify_text(latest: &str, current: &str) -> String {
    format!(
        "Ship of Tools v{} available (running v{}) — it will stage in the background",
        latest,
        sot_updater::semver::strip_v(current)
    )
}

// ─── Periodic (daily) check task ────────────────────────────────────────

/// Spawn the background check task: first check ~2 min after boot, then every
/// 24 h. On a newer release it logs, broadcasts an `FE_COMMAND` `notify` to all
/// connected FEs (via the existing ADR 0025 channel — the identical mechanism
/// `fe.command.send` uses), then stages. Emits exactly one boot log describing
/// the updater's state (the ADR-required dev-build info line lives here).
pub fn spawn_periodic(
    fe_command_tx: broadcast::Sender<FeCommandEvt>,
    clients: crate::clients::Clients,
    leases: Arc<Leases>,
) {
    let updater = Updater::from_env();
    if updater.dev {
        tracing::info!(
            version = %updater.current,
            "auto-update disabled: dev build (hard guard — never self-updates)"
        );
        return;
    }
    if updater.mode == Mode::Off {
        tracing::info!("auto-update disabled: SOT_UPDATE_MODE=off");
        return;
    }
    tracing::info!(
        repo = %updater.repo,
        current = %updater.current,
        mode = ?updater.mode,
        "auto-update active; first check in ~2min, then daily"
    );
    tokio::spawn(async move {
        first_check_wait().await;
        loop {
            run_check_once(&updater, &fe_command_tx, &clients, &leases).await;
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

/// One check cycle for the periodic task: check, and on a newer release notify
/// + stage/prepare/arm — then, in `auto` mode with nobody attached, exit for
/// the apply owner. All failures degrade to a log line; the task never dies.
async fn run_check_once(
    updater: &Updater,
    fe_command_tx: &broadcast::Sender<FeCommandEvt>,
    clients: &crate::clients::Clients,
    leases: &Leases,
) {
    let out = updater.check().await;
    if out.update_available {
        tracing::info!(latest = %out.latest, current = %updater.current, "update available");
        let evt = FeCommandEvt {
            v: 1,
            cmd: "notify".into(),
            args: json!({ "text": notify_text(&out.latest, &updater.current) }),
            target: None,
            // A genuine broadcast: every FE should see an update notice,
            // not just whoever's active. See `FeCommandEvt::target_serial`.
            target_serial: None,
        };
        // Fire-and-forget broadcast; a send error just means no FE is attached.
        let _ = fe_command_tx.send(evt);
        let Some(id) = out.identity else { return };
        let cfg = match updater.mechanism() {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!(error = %e, "no staging root — skipping stage");
                return;
            }
        };
        stage_prepare_arm(&cfg, &id).await;

        if updater.mode == Mode::Auto {
            let armed = matches!(
                sot_updater::pending::read(&cfg.updates_root, &id.target).await,
                Ok(Some(p)) if p.identity == id
            );
            let attached = clients.count();
            if armed && attached == 0 {
                tracing::info!(tag = %id.tag, "auto mode: armed and no clients attached — exiting for the apply owner");
                tokio::time::sleep(Duration::from_millis(250)).await;
                // Re-check after the grace sleep: a client that attached in
                // the window must not have its session killed.
                if clients.count() == 0 {
                    exit_for_update(leases, |code| crate::lifecycle::shutdown::exit(code)).await;
                } else {
                    tracing::info!(tag = %id.tag, "auto mode: a client attached during the exit window — deferring");
                }
            } else if armed {
                tracing::info!(tag = %id.tag, attached, "auto mode: armed but clients attached — applying at next launch/restart instead");
            }
        }
    } else if out.status != "ok" && !out.status.starts_with("disabled") {
        // Off-matrix platform, malformed SHA256SUMS, tag mismatch, network —
        // all of these mean "this host cannot see updates" and deserve a
        // warn, not a debug line nobody reads.
        tracing::warn!(status = %out.status, "update check could not run");
    } else {
        tracing::debug!(latest = %out.latest, status = %out.status, "no update available");
    }
}

// ─── On-demand op handler ───────────────────────────────────────────────

/// `update.check` op (ADR 0030 §4). Runs the check synchronously (fast — one
/// HTTPS request), reports current/latest/availability + the pinned release
/// identity + whether the release is already staged, and — when an update is
/// available but not yet staged — kicks a background stage so the response
/// isn't blocked on a multi-MB download.
/// Never errors on a failed check: the failure rides in `status`.
pub async fn handle_update_check(req_id: u64) -> Result<HandlerOutput> {
    let updater = Updater::from_env();
    // Hard ceiling: this handler runs inline on the connection — a wedged
    // network path must degrade to a status string, not stall the daemon's
    // op loop (the curl budget is shorter still; this is the backstop).
    let out = match tokio::time::timeout(Duration::from_secs(45), updater.check()).await {
        Ok(out) => out,
        Err(_) => CheckOutcome {
            identity: None,
            latest: String::new(),
            update_available: false,
            status: "check unavailable: timed out".into(),
        },
    };
    // Defect 0c: a healthy check keeps saying "ok" even while the release it
    // found is stuck failing to commit — this is what the hub reports, so
    // it's where the block has to surface.
    let out = overlay_stage_block(out);
    let mechanism = updater.mechanism().ok();
    // The status probes hit the filesystem (and git, for prepared) — bound
    // them too, or a hung NFS checkout wedges this connection's op loop.
    let (staged, prepared, armed) = match (&out.identity, &mechanism) {
        (Some(id), Some(cfg)) => {
            let stage_dir = sot_updater::stage_dir(&cfg.updates_root, id);
            let probes = async {
                let staged = sot_updater::is_staged(&cfg.updates_root, id).await;
                let prepared = PreparedState::matches(&updater_spawner(), &stage_dir, id).await;
                (
                    staged,
                    prepared,
                    matches!(
                        sot_updater::pending::read(&cfg.updates_root, &id.target).await,
                        Ok(Some(p)) if p.identity == *id
                    ),
                )
            };
            tokio::time::timeout(Duration::from_secs(10), probes)
                .await
                .unwrap_or((false, false, false))
        }
        _ => (false, false, false),
    };

    if out.update_available && !armed {
        if let (Some(id), Some(cfg)) = (out.identity.clone(), mechanism) {
            // Fire-and-forget: make progress without holding the op response open.
            tokio::spawn(async move { stage_prepare_arm(&cfg, &id).await });
        }
    }

    let res = UpdateCheckRes {
        current: updater.current.clone(),
        latest: out.latest,
        update_available: out.update_available,
        staged,
        prepared,
        armed,
        status: out.status,
        tag: out
            .identity
            .as_ref()
            .map(|id| id.tag.clone())
            .unwrap_or_default(),
        repo: updater.repo.clone(),
        target: out
            .identity
            .as_ref()
            .map(|id| id.target.clone())
            .unwrap_or_default(),
        asset_sha256: out
            .identity
            .as_ref()
            .map(|id| id.asset_sha256.clone())
            .unwrap_or_default(),
    };
    Ok(vec![(
        Frame::res(req_id, op::UPDATE_CHECK, serde_json::to_value(res)?),
        None,
    )])
}

/// `update.apply` op (ADR 0030 Phase C3): validate the armed pending pointer,
/// answer, broadcast a notify, then EXIT so the single apply owner (systemd
/// `ExecStartPre` via Restart=always, or the user's next `sot-launch`) runs
/// the fast offline flip. The daemon deliberately does NOT apply in-process —
/// one apply owner per platform, and it isn't the running binary being
/// replaced.
pub async fn handle_update_apply(
    req_id: u64,
    fe_command_tx: &broadcast::Sender<FeCommandEvt>,
    leases: &Leases,
) -> Result<HandlerOutput> {
    let refuse = |status: &str| -> Result<HandlerOutput> {
        let res = UpdateApplyRes {
            ok: false,
            tag: String::new(),
            will_restart: false,
            status: status.into(),
        };
        Ok(vec![(
            Frame::res(req_id, op::UPDATE_APPLY, serde_json::to_value(res)?),
            None,
        )])
    };

    let updater = Updater::from_env();
    if updater.dev {
        return refuse("disabled: dev build");
    }
    let cfg = match updater.mechanism() {
        Ok(c) => c,
        Err(e) => return refuse(&format!("no updates root: {e}")),
    };
    let Some(target) = sot_updater::platform::this_platform() else {
        return refuse("platform not in the release matrix");
    };
    let pending = match sot_updater::pending::read(&cfg.updates_root, target).await {
        Ok(Some(p)) => p,
        Ok(None) => return refuse("nothing armed"),
        Err(e) => return refuse(&format!("pending pointer unreadable: {e}")),
    };

    let will_restart = InstallManifest::for_current_exe()
        .and_then(|m| m.service)
        .is_some_and(|s| s == "systemd");
    let tag = pending.identity.tag.clone();
    tracing::info!(tag = %tag, will_restart, "update.apply: exiting for the apply owner to flip");

    let evt = FeCommandEvt {
        v: 1,
        cmd: "notify".into(),
        args: json!({ "text": format!(
            "Applying Ship of Tools {tag} — backend {}",
            if will_restart { "restarting" } else { "exiting; your next launch completes the update" }
        )}),
        target: None,
        // A genuine broadcast: see `FeCommandEvt::target_serial`.
        target_serial: None,
    };
    let _ = fe_command_tx.send(evt);

    // Give the response + notify time to flush, then exit 0. Under systemd
    // (Restart=always) the ExecStartPre apply runs on the way back up; for
    // launcher-managed daemons the next sot-launch applies and starts fresh.
    // (A flush-coupled exit — after the writer confirms the frame left — is
    // a tracked follow-up; 1.5s is comfortably beyond a loopback write.)
    let leases = leases.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        #[cfg(feature = "daemon-lifetime-faults")]
        crate::lifecycle::test_gates::held("update-go").await;
        exit_for_update(&leases, |code| {
            tracing::info!("update.apply: exiting now");
            crate::lifecycle::shutdown::exit(code)
        })
        .await;
    });

    let res = UpdateApplyRes {
        ok: true,
        tag,
        will_restart,
        status: "applying".into(),
    };
    Ok(vec![(
        Frame::res(req_id, op::UPDATE_APPLY, serde_json::to_value(res)?),
        None,
    )])
}

/// An update's exit, a restart (75) handed to `exit`, taken only while no shutdown is under way. The commit is made under
/// the lease lock and the exit after it, outside the lock: a close that comes later cannot begin, and the exit's wait for
/// the child fire holds no lease. Once a shutdown has begun its own exit stands and the update's is skipped (ruling f).
async fn exit_for_update(leases: &Leases, exit: impl FnOnce(i32)) {
    if leases.commit_update() {
        tracing::info!("update committed: exiting 75 for the apply owner");
        #[cfg(feature = "daemon-lifetime-faults")]
        crate::lifecycle::test_gates::held("update-committed").await;
        exit(sot_protocol::ops::lease::EXIT_UPDATE_RESTART);
    } else {
        tracing::info!("update exit skipped: a shutdown is under way, and its own exit stands");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sot_updater::identity::DEFAULT_REPO;

    /// Every updater command is a contained child of the signal: its two streams and its status come back whole, and a
    /// command its caller drops or a fire ends the command's whole tree.
    #[cfg(unix)]
    mod spawner {
        use super::*;
        use crate::lifecycle::child_signal::{tests::Leftover, Signal};
        use sot_updater::Spawner;
        use std::time::{Duration, Instant};

        fn a_signal() -> &'static Signal {
            Box::leak(Box::new(Signal::new()))
        }

        /// A command that starts a grandchild, writes its pid to `pid_file` and waits for it.
        fn tree_command(pid_file: &std::path::Path) -> tokio::process::Command {
            let mut command = tokio::process::Command::new("sh");
            command
                .args(["-c", "sleep 3180 & echo $! > \"$1\"; wait", "sh"])
                .arg(pid_file);
            command
        }

        async fn pid_written(file: &std::path::Path) {
            let began = Instant::now();
            while std::fs::read_to_string(file).map_or(true, |t| t.trim().is_empty()) {
                assert!(
                    began.elapsed() < Duration::from_secs(10),
                    "the updater command never started its child"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        #[tokio::test]
        async fn the_spawner_returns_both_streams_and_the_status() {
            let signal = a_signal();
            let mut command = tokio::process::Command::new("sh");
            command.args(["-c", "echo out; echo err >&2; exit 3"]);
            let output = UpdaterSpawner(signal)
                .output(&mut command)
                .await
                .expect("the command ran");
            assert_eq!(output.status.code(), Some(3));
            assert_eq!(output.stdout, b"out\n");
            assert_eq!(output.stderr, b"err\n");
            assert!(
                signal.held_groups().is_empty(),
                "a finished command's tree is still held"
            );
        }

        #[tokio::test]
        async fn a_dropped_updater_command_ends_its_tree() {
            let signal = a_signal();
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("pid");
            let mut command = tree_command(&file);
            let task =
                tokio::spawn(async move { UpdaterSpawner(signal).output(&mut command).await });
            pid_written(&file).await;
            let grandchild = Leftover::of_file(&file);
            assert_eq!(
                signal.held_groups().len(),
                1,
                "the running command's tree is not held"
            );
            task.abort();
            let _ = task.await;
            assert!(
                grandchild.gone(),
                "the dropped command's grandchild outlived it"
            );
            assert!(signal.held_groups().is_empty());
        }

        #[tokio::test]
        async fn a_fire_ends_a_running_updater_command_tree() {
            let signal = a_signal();
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("pid");
            let mut command = tree_command(&file);
            let task =
                tokio::spawn(async move { UpdaterSpawner(signal).output(&mut command).await });
            pid_written(&file).await;
            let grandchild = Leftover::of_file(&file);
            signal.fire().expect("fire");
            assert!(
                grandchild.gone(),
                "the fired signal left the grandchild running"
            );
            let _ = tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .expect("the command outlived the fire");
            let mut late = tokio::process::Command::new("sh");
            late.args(["-c", "exit 0"]);
            assert!(
                UpdaterSpawner(signal).output(&mut late).await.is_err(),
                "a command started after the fire"
            );
        }
    }

    /// The daemon's release repo: the default when the variable is unset, empty
    /// or blank, the trimmed value otherwise; the prior value is restored.
    #[test]
    fn repo_from_env_trims_and_defaults() {
        let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = crate::paths::EnvGuard::capture("SOT_UPDATE_REPO");
        std::env::remove_var("SOT_UPDATE_REPO");
        assert_eq!(repo_from_env(), DEFAULT_REPO);
        std::env::set_var("SOT_UPDATE_REPO", "");
        assert_eq!(repo_from_env(), DEFAULT_REPO);
        std::env::set_var("SOT_UPDATE_REPO", "   ");
        assert_eq!(repo_from_env(), DEFAULT_REPO);
        std::env::set_var("SOT_UPDATE_REPO", "  fork/x  ");
        assert_eq!(repo_from_env(), "fork/x");
    }

    /// The julia an update's prepare runs is the resolver's, which never returns a path with a `WindowsApps` component; only a
    /// backend role runs one.
    #[test]
    fn the_update_prepare_runs_the_resolvers_julia() {
        let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = crate::paths::EnvGuard::capture("SOT_JULIA_BIN");
        std::env::set_var("SOT_JULIA_BIN", r"C:\Users\x\AppData\Local\Microsoft\WindowsApps\julia.exe");
        let err = prepare_julia(true).unwrap_err();
        assert!(err.contains("app-execution alias"), "unexpected error: {err}");
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("julia");
        std::env::set_var("SOT_JULIA_BIN", &stub);
        assert_eq!(prepare_julia(true), Ok(Some(stub.to_string_lossy().into_owned())));
        assert_eq!(prepare_julia(false), Ok(None));
    }

    /// Once a shutdown has begun its own exit stands: the update's is
    /// skipped (ruling f).
    #[tokio::test]
    async fn update_exit_yields_to_shutdown() {
        let leases = Leases::new(Some("boot".into()), None, None, false);
        async fn exit_code(leases: &Leases) -> Option<i32> {
            let mut code = None;
            exit_for_update(leases, |c| code = Some(c)).await;
            code
        }
        assert_eq!(
            exit_code(&leases).await,
            Some(sot_protocol::ops::lease::EXIT_UPDATE_RESTART),
            "no shutdown under way: the update exits 75"
        );
        let leases = Leases::new(Some("boot".into()), None, None, false);
        leases.begin_close();
        assert_eq!(
            exit_code(&leases).await,
            None,
            "the update exits 75 during a shutdown"
        );
        leases.finish_shutdown(0, Vec::new()).unwrap();
        assert_eq!(
            exit_code(&leases).await,
            None,
            "the update exits 75 after the shutdown's final record"
        );
    }

    /// An update committed first stands: a close that comes after it does not begin, so the daemon cannot be sent down the
    /// close's exit 0 instead of the restart.
    #[tokio::test]
    async fn a_committed_update_is_not_undone_by_a_later_close() {
        let leases = Leases::new(Some("boot".into()), None, None, false);
        let mut code = None;
        exit_for_update(&leases, |c| code = Some(c)).await;
        assert_eq!(code, Some(sot_protocol::ops::lease::EXIT_UPDATE_RESTART));
        leases.begin_close();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), leases.gone())
                .await
                .is_err(),
            "a close began after the update was committed"
        );
        assert!(!leases.commit_update(), "a second update committed");
    }

    fn topo(text: &str) -> sot_protocol::topology::Topology {
        sot_protocol::topology::parse(text).expect("fixture must parse")
    }

    // The regression this pins: `install.role != "remote"` used to decide
    // this; install.json carries no role any more (plan step 6), so a
    // daemon box and a frontend-only box must still take the branch they
    // always did, now read from the declared topology first, install.json's
    // recorded `daemon` bit second (a listless box), `true` last.

    #[test]
    fn a_daemon_box_still_prepares_julia_and_npm() {
        let t = topo(
            "hub = \"hubbox\"\n\
             [host.hubbox]\n\
             daemon = true\n\
             [host.host-2]\n\
             daemon = true\n",
        );
        assert!(backend_role_from_topology(Some(&t), "host-2", None));
    }

    #[test]
    fn a_frontend_only_box_does_not() {
        let t = topo(
            "hub = \"hubbox\"\n\
             [host.hubbox]\n\
             daemon = true\n\
             [host.laptop]\n\
             frontend = true\n",
        );
        assert!(!backend_role_from_topology(Some(&t), "laptop", None));
    }

    #[test]
    fn a_box_the_topology_does_not_name_falls_back_to_the_recorded_bit() {
        let t = topo("hub = \"hubbox\"\n[host.hubbox]\ndaemon = true\n");
        assert!(backend_role_from_topology(Some(&t), "nowhere", None));
    }

    #[test]
    fn no_topology_and_no_recorded_bit_defaults_to_true() {
        assert!(backend_role_from_topology(None, "anything", None));
    }

    // The ruling this closes: a listless frontend-only-over-ssh install has
    // no hosts.toml at all, so nothing but install.json's own recorded
    // `daemon: false` can tell the update path to skip Julia/npm here.
    #[test]
    fn a_listless_frontend_only_box_uses_its_recorded_bit() {
        assert!(!backend_role_from_topology(None, "laptop", Some(false)));
    }

    #[test]
    fn a_listless_daemon_box_uses_its_recorded_bit() {
        assert!(backend_role_from_topology(None, "host-2", Some(true)));
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
             [host.host-2]\n\
             daemon = true\n",
        );
        // install.json still says "daemon: false" from an install run
        // before this host was added to the list as a daemon.
        assert!(backend_role_from_topology(Some(&t), "host-2", Some(false)));
    }

    // The update path reads this host's own entry in the declared topology:
    // the name comes from `SOT_SELF_HOST` here, and only that entry's
    // `daemon` flag decides (the recorded bit is `true` and must not win).
    #[test]
    fn backend_role_wanted_reads_this_hosts_declared_entry() {
        let _guard = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = (std::env::var_os("SOT_HOSTS"), std::env::var_os("SOT_SELF_HOST"));
        let dir = tempfile::tempdir().unwrap();
        let hosts = dir.path().join("hosts.toml");
        std::fs::write(
            &hosts,
            "hub = \"mw21-hub\"\n[host.mw21-hub]\ndaemon = true\n[host.mw21-laptop]\nfrontend = true\n",
        )
        .unwrap();
        let install: InstallManifest =
            serde_json::from_value(json!({"schema": 1, "prefix": "/nowhere", "daemon": true})).unwrap();
        std::env::set_var("SOT_HOSTS", &hosts);
        std::env::set_var("SOT_SELF_HOST", "mw21-laptop");
        let laptop = backend_role_wanted(&install);
        std::env::set_var("SOT_SELF_HOST", "mw21-hub");
        let hub = backend_role_wanted(&install);
        for (key, value) in [("SOT_HOSTS", saved.0), ("SOT_SELF_HOST", saved.1)] {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        assert!(!laptop, "a frontend-only entry wants no backend role");
        assert!(hub, "a daemon entry wants the backend role");
    }

    fn fake_identity(tag: &str) -> ReleaseIdentity {
        ReleaseIdentity {
            repo: "kalidke/ship-of-tools".into(),
            tag: tag.into(),
            version: tag.trim_start_matches('v').into(),
            target: "linux-x86_64".into(),
            asset: format!("sot-{}-linux-x86_64.tar.gz", tag.trim_start_matches('v')),
            asset_sha256: "0".repeat(64),
        }
    }

    fn ok_outcome(id: ReleaseIdentity) -> CheckOutcome {
        CheckOutcome {
            latest: id.version.clone(),
            identity: Some(id),
            update_available: true,
            status: "ok".into(),
        }
    }

    // Defect 0c: a check that itself succeeded ("ok") must still surface a
    // release stuck failing to commit — that silence is exactly how one box
    // hid never having updated. Pins the tag-scoping too: a DIFFERENT
    // release's check is not tainted by an older tag's recorded block.
    #[test]
    fn a_recorded_stage_block_overlays_status_for_its_own_tag_only() {
        clear_stage_block(); // isolate from any other test's leftover state
        record_stage_block("v9.9.9", "Access is denied. (os error 5)");

        let blocked = overlay_stage_block(ok_outcome(fake_identity("v9.9.9")));
        assert_eq!(blocked.status, "update blocked: Access is denied. (os error 5)");

        let unrelated = overlay_stage_block(ok_outcome(fake_identity("v9.9.10")));
        assert_eq!(unrelated.status, "ok");

        clear_stage_block();
        let after_clear = overlay_stage_block(ok_outcome(fake_identity("v9.9.9")));
        assert_eq!(after_clear.status, "ok");
    }
}
