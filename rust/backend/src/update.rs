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
// - the HARD GUARD: a build whose `app_version()` carries a `+` (a source build)
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
use sot_updater::pending::{ArmOutcome, InstalledBuild, Writer};
use sot_updater::prepare::{PrepareSpec, PreparedState};
use sot_updater::{CheckOutcome, Fetcher, InstallFile, InstallManifest, ReleaseIdentity, UpdaterConfig, MISSING_LEAD, UNREADABLE_LEAD};

mod cli;
mod refusal;
pub(crate) use cli::{run, USAGE};
use refusal::{apply_refusal, arm_refusal, host_runs_no_sotd};

use crate::server::reply::HandlerOutput;
use crate::lifecycle::lease::Leases;
use crate::sidecars::julia::JuliaError;

/// Daemon policy: every updater command belongs to this signal's contained tree registry, and runs with a temporary
/// folder of its own (`ChildTmp`) that goes after its reap, so what a command leaves there goes with it: Pkg's package
/// download, which Pkg removes only at the end of the call that made it, from an instantiate that failed, was dropped
/// past its bound or was ended with the daemon.
pub(crate) struct UpdaterSpawner(pub(crate) &'static sot_containment::Signal);

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
            // Made before the child, so a dropped call asks the child's tree to end (its `Contained`'s drop) before
            // the folder goes.
            let tmp = crate::sidecars::child::ChildTmp::new()?;
            tmp.apply(command);
            let mut child = self.0.spawn(command)?;
            let mut stdout = child.stdout.take().expect("piped stdout");
            let mut stderr = child.stderr.take().expect("piped stderr");
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let (status, _, _) = tokio::try_join!(
                child.wait(),
                stdout.read_to_end(&mut out),
                stderr.read_to_end(&mut err)
            )?;
            // The child is reaped, so `retire` only removes the folder.
            let _ = tmp.retire(&mut child).await;
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
    UpdaterSpawner(sot_containment::process())
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
    /// `0.2.0+abc`.
    current: String,
    /// True when this is not the CI release build (`is_release_build`) — the hard guard.
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
        let updates_root = sot_updater::resolve_updates_root()?;
        Ok(UpdaterConfig {
            repo: self.repo.clone(),
            current_version: InstalledBuild::version_or(&updates_root, &self.current),
            fetcher: Fetcher::from_env(),
            updates_root,
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
        match self.check_basis() {
            Ok(version) => {
                sot_updater::check_release(&updater_spawner(), &self.repo, &version, &Fetcher::from_env()).await
            }
            Err(status) => disabled(&status),
        }
    }

    /// The version a check follows the channel of and compares with: the install's, read now. This process booted with
    /// `current`, and a shared prefix can have been installed past it, or onto another channel, since; `current` stands
    /// only where there is no install to read. A dev build or a pinned install is no basis for a release check.
    fn check_basis(&self) -> std::result::Result<String, String> {
        let installed = match sot_updater::resolve_updates_root() {
            Ok(root) => InstalledBuild::try_read(&root).map_err(|unreadable| format!("disabled: {}", unreadable.sentence()))?,
            Err(_) => None,
        };
        match installed {
            Some(i) if i.is_dev_build() => Err("disabled: the install is a dev build".into()),
            Some(i) if i.pinned => Err(format!("disabled: {}", i.pin_note().unwrap_or_default())),
            Some(i) => Ok(i.version),
            None => Ok(self.current.clone()),
        }
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

/// What one stage-prepare-arm run did when it did not fail.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Pipeline {
    /// Staged, prepared and armed.
    Armed,
    /// Staged and prepared, and the arm said why it armed nothing.
    NotArmed(ArmOutcome),
    /// Staged only: the box has no install manifest to prepare for.
    Staged,
}

/// The step a stage-prepare-arm run failed at, with the cause.
#[derive(Debug)]
pub(crate) enum PipelineError {
    Stage(String),
    Prepare(String),
    Arm(String),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stage(e) => write!(f, "staging update failed: {e}"),
            Self::Prepare(e) => write!(f, "preparing update failed: {e}"),
            Self::Arm(e) => write!(f, "arming update failed: {e}"),
        }
    }
}

/// An error with its whole context chain: the OS error at the end is what names the fault, and `to_string` drops it.
fn chain(e: &anyhow::Error) -> String {
    e.chain().map(|c| c.to_string()).collect::<Vec<_>>().join(": ")
}

/// The one stage-prepare-arm both the daemon and `sotd update stage` run; `install` is the manifest to prepare and
/// arm for, `None` stopping after the stage. `writer` says who is arming: only the operator's `update stage` may arm
/// a build at or below the install.
pub(crate) async fn stage_prepare_arm_with(
    cfg: &UpdaterConfig,
    id: &ReleaseIdentity,
    install: Option<&InstallManifest>,
    writer: Writer,
) -> Result<Pipeline, PipelineError> {
    tracing::info!(tag = %id.tag, "staging");
    sot_updater::stage(&updater_spawner(), cfg, id).await.map_err(|e| PipelineError::Stage(chain(&e)))?;
    // Defect 0c: the commit landed, so a block recorded for a PRIOR attempt (this tag or an older one) no longer
    // describes reality — and the prepare below can take an hour.
    clear_stage_block();
    let Some(install) = install else {
        return Ok(Pipeline::Staged);
    };
    tracing::info!(tag = %id.tag, "preparing the checkout and environments; this can take many minutes");
    let spec = prepare_spec(install, cfg, id).map_err(PipelineError::Prepare)?;
    let state = sot_updater::prepare::prepare(&updater_spawner(), &spec)
        .await
        .map_err(|e| PipelineError::Prepare(chain(&e)))?;
    tracing::info!(tag = %id.tag, "arming");
    match sot_updater::pending::arm(&cfg.updates_root, id, &state.checkout, &state.commit, writer).await {
        Ok(ArmOutcome::Armed) => Ok(Pipeline::Armed),
        Ok(not_armed) => Ok(Pipeline::NotArmed(not_armed)),
        Err(e) => Err(PipelineError::Arm(chain(&e))),
    }
}

async fn stage_prepare_arm_inner(cfg: &UpdaterConfig, id: &ReleaseIdentity) {
    let install = match InstallManifest::of_current_exe() {
        InstallFile::Read(install) => Some(install),
        InstallFile::Absent => None,
        InstallFile::Unreadable(unreadable) => {
            tracing::warn!(tag = %id.tag, "not staging: {}", unreadable.sentence());
            return;
        }
    };
    // Refused before the download: `arm` re-reads the install under the lock and is the authority, this only spares
    // a stale or foreign daemon the bytes and the prepare.
    let refusal = install.as_ref().and_then(|i| match InstalledBuild::try_read(&i.updates_root()) {
        Ok(installed) => arm_refusal(host_runs_no_sotd(), installed.as_ref(), id),
        Err(unreadable) => Some(unreadable.sentence()),
    });
    if let Some(why) = refusal {
        tracing::info!(tag = %id.tag, "not staging: {why}");
        return;
    }
    let result = stage_prepare_arm_with(cfg, id, install.as_ref(), Writer::Automatic).await;
    // Defect 0c: the shared crate already retried the commit rename for about a minute before giving up. Record it so
    // the NEXT check reports the concrete reason instead of a bare "ok" that hides a release which never manages to
    // land — the failure mode a healthy-looking, never-updating box actually exhibited. A landed stage clears it.
    if let Err(PipelineError::Stage(cause)) = &result {
        record_stage_block(&id.tag, cause);
    }
    match result {
        Err(PipelineError::Stage(cause)) => tracing::warn!(tag = %id.tag, error = %cause, "staging update failed"),
        Err(PipelineError::Prepare(e)) => tracing::warn!(tag = %id.tag, error = %e, "preparing update failed — not arming"),
        Err(PipelineError::Arm(e)) => tracing::warn!(tag = %id.tag, error = %e, "arming update failed"),
        Ok(Pipeline::Staged) => tracing::info!(tag = %id.tag, "staged (no install manifest — prepare/arm skipped)"),
        Ok(Pipeline::NotArmed(outcome)) => {
            let why = outcome.not_armed_because().unwrap_or_default();
            match outcome {
                ArmOutcome::UnreadableInstall(_) => tracing::warn!(tag = %id.tag, "not armed: {why}"),
                _ => tracing::info!(tag = %id.tag, "not armed: {why}"),
            }
        }
        Ok(Pipeline::Armed) => {}
    }
}

/// Pure: given the declared topology (if any), this box's own name, and
/// install.json's own recorded `daemon` bit (if any), does this box want a
/// backend prepared for the staged release (Julia envs, the MathJax
/// sidecar)? Priority order — see `backend_role_wanted`'s doc for why:
/// 1. the declared topology, when it names this host — canonical, can
///    change without a reinstall (D9, dev/output/topology-plan.md §D); a
///    `frontend` entry takes `recorded_daemon`, because only an install
///    from rc9.12 on gives it a daemon of its own
///    (`sot_protocol::topology::runs_backend`, the one rule);
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
    sot_protocol::topology::runs_backend(topo, me, recorded_daemon)
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
    backend_role_recorded(install.daemon)
}

/// [`backend_role_wanted`] for a caller that has read the record's `daemon` bit itself (or could not read the record).
fn backend_role_recorded(recorded_daemon: Option<bool>) -> bool {
    let topo = sot_protocol::topology::load().ok().flatten().map(|(_, t)| t);
    let me = crate::comm::mail::filer::comm_self_host();
    backend_role_from_topology(topo.as_ref(), &me, recorded_daemon)
}

/// The julia an update's prepare runs for its envs: only a backend role runs one, and it is the resolver's answer, the
/// one every other daemon child runs, with the resolver's reason when there is none. On Windows no answer skips the Julia
/// steps, as the window's own update does: the launcher instantiates a new version's environments at its first start, and
/// a box without Julia is a supported, degraded state, not a reason to refuse every update.
fn prepare_julia(backend_role: bool) -> Result<Option<String>, String> {
    if !backend_role {
        return Ok(None);
    }
    julia_or_skip(crate::sidecars::julia::resolve_bin().map(|(bin, _source)| bin), cfg!(windows))
}

/// Only "no julia anywhere" is skipped; a julia the resolver found and refused stays the stage's error, since the
/// daemon's kernels and REPLs would fail on the same one.
fn julia_or_skip(resolved: Result<String, JuliaError>, skip_when_none: bool) -> Result<Option<String>, String> {
    match resolved {
        Ok(bin) => Ok(Some(bin)),
        Err(JuliaError::NotFound(why)) if skip_when_none => {
            tracing::info!("preparing the update without Julia: {why}");
            Ok(None)
        }
        Err(e) => Err(e.to_string()),
    }
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
    } else if out.status.strip_prefix("disabled: ").is_some_and(|s| s.starts_with(UNREADABLE_LEAD) || s.starts_with(MISSING_LEAD)) {
        // An install that may hold a pin and cannot be read: the person has to repair it, so it is not a debug line.
        tracing::warn!(status = %out.status, "update check disabled");
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
    // Hard ceiling: this handler runs as a job off the connection's loop
    // (`place`), and a wedged network path must still degrade to a status
    // string, not leave the window's check unanswered (the curl budget is
    // shorter still; this is the backstop).
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
    // them too, or a hung NFS checkout leaves the check unanswered.
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

    let install = InstallManifest::of_current_exe();
    let foreign = !matches!(install, InstallFile::Absent) && host_runs_no_sotd();
    if let Some(why) = apply_refusal(foreign, InstalledBuild::try_read(&cfg.updates_root), &pending) {
        return refuse(&why);
    }
    let will_restart = install
        .manifest()
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
#[path = "update_tests.rs"]
mod tests;
