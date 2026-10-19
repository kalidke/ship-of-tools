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
mod tests {
    use super::*;
    use sot_updater::pending::PendingPointer;
    use sot_updater::UnreadableInstall;
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

        /// A command that leaves a file in its temporary folder, as Pkg leaves a package download there when the call
        /// that made it does not reach its end, writes the file's path to `report`, then runs `then`.
        fn leaving_command(report: &std::path::Path, then: &str) -> tokio::process::Command {
            let mut command = tokio::process::Command::new("sh");
            command
                .args(["-c", &format!("f=$(mktemp) && echo \"$f\" > \"$1\" && {then}"), "sh"])
                .arg(report);
            command
        }

        /// The file a [`leaving_command`] named, removed when the test ends however it ends.
        struct Left(std::path::PathBuf);

        impl Left {
            fn named_in(report: &std::path::Path) -> Self {
                Self(std::fs::read_to_string(report).expect("the command named its file").trim().into())
            }

            /// Whether the file is gone within 10 s.
            async fn goes(&self) -> bool {
                let began = Instant::now();
                while self.0.exists() && began.elapsed() < Duration::from_secs(10) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                !self.0.exists()
            }
        }

        impl Drop for Left {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
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

        /// What a command leaves in its temporary folder goes with it: a julia step that exits on an error Pkg rethrows
        /// leaves its download there, and nothing else removes it.
        #[tokio::test]
        async fn a_finished_updater_commands_temporary_files_go_with_it() {
            let signal = a_signal();
            let dir = tempfile::tempdir().unwrap();
            let report = dir.path().join("left");
            let output = UpdaterSpawner(signal)
                .output(&mut leaving_command(&report, "exit 1"))
                .await
                .expect("the command ran");
            assert_eq!(output.status.code(), Some(1));
            let left = Left::named_in(&report);
            assert!(
                left.goes().await,
                "the finished command's temporary file is still there: {}",
                left.0.display()
            );
        }

        /// A command its caller drops (a step past its bound) leaves nothing in its temporary folder either.
        #[tokio::test]
        async fn a_dropped_updater_commands_temporary_files_go_with_it() {
            let signal = a_signal();
            let dir = tempfile::tempdir().unwrap();
            let report = dir.path().join("left");
            let mut command = leaving_command(&report, "exec sleep 3180");
            let task =
                tokio::spawn(async move { UpdaterSpawner(signal).output(&mut command).await });
            pid_written(&report).await;
            let left = Left::named_in(&report);
            task.abort();
            let _ = task.await;
            assert!(
                left.goes().await,
                "the dropped command's temporary file is still there: {}",
                left.0.display()
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

    /// Only a backend role prepares Julia. prepare_julia returns the shared resolver's selected path or error unchanged; directory spelling does not establish alias identity.
    #[test]
    fn the_update_prepare_runs_the_resolvers_julia() {
        let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = crate::paths::EnvGuard::capture("SOT_JULIA_BIN");
        std::env::set_var("SOT_JULIA_BIN", "relative-julia");
        let refusal = crate::sidecars::julia::resolve_bin().unwrap_err().to_string();
        assert_eq!(prepare_julia(true), Err(refusal), "the resolver's error is returned unchanged");
        assert_eq!(prepare_julia(false), Ok(None), "a frontend-only role prepares no julia, whatever the override");
        // A path that does not exist, under a folder with the name an alias lives in, is a selection for the spawn
        // to fail on, not evidence of an alias.
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("WindowsApps").join("julia");
        std::env::set_var("SOT_JULIA_BIN", &stub);
        assert_eq!(prepare_julia(true), Ok(Some(stub.to_string_lossy().into_owned())));
        assert_eq!(prepare_julia(false), Ok(None));
    }

    /// A Windows prepare with no Julia anywhere skips the Julia steps; a Julia the resolver refused is an error on every
    /// platform, and the others refuse with the resolver's reason.
    #[test]
    fn a_windows_prepare_without_julia_skips_it_but_not_a_refused_one() {
        let none = || Err(JuliaError::NotFound("no julia".to_string()));
        let refused = || Err(JuliaError::Refused("SOT_JULIA_BIN=\"julia\" is not an absolute path".to_string()));
        assert_eq!(julia_or_skip(none(), true), Ok(None), "Windows: the launcher instantiates at first start");
        assert_eq!(julia_or_skip(none(), false), Err("no julia".to_string()), "elsewhere the reason ends the stage");
        assert_eq!(
            julia_or_skip(refused(), true),
            Err("SOT_JULIA_BIN=\"julia\" is not an absolute path".to_string()),
            "Windows: a refusal names itself and ends the stage"
        );
        assert_eq!(julia_or_skip(Ok("/x/julia".to_string()), true), Ok(Some("/x/julia".to_string())), "a resolved julia is used");
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

    // The case this closes: a listless frontend-only-over-ssh install has
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

    // The shape that must be stopped and the shapes that must not: through the real declared-host read, a host the
    // topology names with neither `daemon` nor `frontend` runs no sotd; a frontend host runs its own, and a host the
    // topology does not name is not said to be a guest.
    #[test]
    fn only_a_host_named_with_neither_key_runs_no_sotd() {
        let _guard = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = (std::env::var_os("SOT_HOSTS"), std::env::var_os("SOT_SELF_HOST"));
        let dir = tempfile::tempdir().unwrap();
        let hosts = dir.path().join("hosts.toml");
        std::fs::write(
            &hosts,
            "hub = \"hubbox\"\n[host.hubbox]\ndaemon = true\n[host.guest]\n[host.laptop]\nfrontend = true\n",
        )
        .unwrap();
        std::env::set_var("SOT_HOSTS", &hosts);
        let answers = ["guest", "laptop", "hubbox", "unlisted"].map(|me| {
            std::env::set_var("SOT_SELF_HOST", me);
            host_runs_no_sotd()
        });
        for (key, value) in [("SOT_HOSTS", saved.0), ("SOT_SELF_HOST", saved.1)] {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        assert_eq!(answers, [true, false, false, false]);
    }

    /// The recorded block is process-wide: tests that record or clear it run one at a time.
    static STAGE_BLOCK_TEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// A release folder for this platform that stages, and the config and identity that stage it.
    #[cfg(unix)]
    async fn staged_release(dir: &std::path::Path) -> (UpdaterConfig, ReleaseIdentity) {
        let target = sot_updater::platform::this_platform().unwrap();
        let asset = sot_updater::platform::platform_asset("9.9.9").unwrap();
        let top = dir.join("build").join(format!("sot-9.9.9-{target}"));
        std::fs::create_dir_all(&top).unwrap();
        for f in ["sot", "sotd", "sot-capsule"] {
            std::fs::write(top.join(f), f).unwrap();
        }
        let release = dir.join("release");
        std::fs::create_dir_all(&release).unwrap();
        sot_log::test_wait::run_within(
            std::process::Command::new("tar")
                .arg("-czf")
                .arg(release.join(&asset))
                .arg("-C")
                .arg(dir.join("build"))
                .arg(top.file_name().unwrap()),
            Duration::from_secs(30),
        )
        .ok();
        let digest = sot_updater::fetch::hash::sha256_file(&release.join(&asset)).await.unwrap();
        std::fs::write(release.join("SHA256SUMS"), format!("{digest}  {asset}\n")).unwrap();
        let sums = std::fs::read_to_string(release.join("SHA256SUMS")).unwrap();
        let id = sot_updater::fetch::sums::discover(&sot_updater::fetch::sums::parse_sums(&sums).unwrap(), "example/project", target).unwrap();
        let cfg = UpdaterConfig {
            repo: "example/project".into(),
            current_version: "0.1.0".into(),
            fetcher: Fetcher::Dir(release),
            updates_root: dir.join("updates"),
        };
        (cfg, id)
    }

    /// The block a failed commit recorded stops describing reality the moment the next attempt stages: the prepare that
    /// follows can take an hour, and `update.check` must not report the old reason meanwhile.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_successful_stage_clears_the_recorded_block_before_prepare() {
        let _block = STAGE_BLOCK_TEST.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let (cfg, id) = staged_release(dir.path()).await;
        record_stage_block(&id.tag, "Access is denied. (os error 5)");
        let staged = stage_prepare_arm_with(&cfg, &id, None, Writer::Automatic).await;
        assert!(matches!(staged, Ok(Pipeline::Staged)), "{staged:?}");
        assert_eq!(overlay_stage_block(ok_outcome(id)).status, "ok");
    }

    /// Each step says so on the log, which `sotd update stage` sends to stderr: the stage and, when there is an install to
    /// prepare for, the prepare and the arm.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_pipeline_logs_a_line_for_each_step() {
        #[derive(Clone, Default)]
        struct Buf(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let _block = STAGE_BLOCK_TEST.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let (cfg, id) = staged_release(dir.path()).await;
        let buf = Buf::default();
        let sink = buf.clone();
        let subscriber = tracing_subscriber::fmt().with_ansi(false).with_writer(move || sink.clone()).finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        stage_prepare_arm_with(&cfg, &id, None, Writer::Automatic).await.unwrap();
        let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("staging"), "{log}");
    }

    fn installed(version: &str, commit: &str) -> InstalledBuild {
        InstalledBuild { version: version.into(), commit: Some(commit.into()), pinned: false }
    }

    fn pointer(tag: &str, over: Option<(&str, &str)>) -> PendingPointer {
        PendingPointer {
            schema: 1,
            identity: fake_identity(tag),
            checkout: "/x".into(),
            commit: "cc".into(),
            armed_at: 0,
            armed_over_version: over.map(|o| o.0.into()),
            armed_over_commit: over.map(|o| o.1.into()),
        }
    }

    // The shared-home red: a daemon that booted on rc9.12 offers rc9.13 into an install that is already on rc9.14.
    #[test]
    fn a_daemon_arms_only_a_declared_host_and_only_above_the_install() {
        let id = fake_identity("v0.6.6-rc9.13");
        let at = |v: &str| installed(v, "c0");
        assert!(arm_refusal(false, Some(&at("0.6.6-rc9.12")), &id).is_none());
        assert!(arm_refusal(false, None, &id).is_none(), "no install manifest: nothing to compare");
        assert!(arm_refusal(false, Some(&at("0.6.6-rc9.13")), &id).unwrap().contains("not above"));
        assert!(arm_refusal(false, Some(&at("0.6.6-rc9.14-dev+d94ed0fac")), &id).unwrap().contains("dev build"));
        // Not a release above a dev stamp either.
        let above = fake_identity("v0.6.6-rc9.14");
        assert!(arm_refusal(false, Some(&at("0.6.6-rc9.14-dev+d94ed0fac")), &above).unwrap().contains("dev build"));
        assert!(arm_refusal(true, Some(&at("0.6.6-rc9.12")), &id).unwrap().contains("neither daemon nor frontend"));
    }

    // An operator's downgrade is not undone by the next daily check: the run that names a version pins the install.
    #[test]
    fn a_pinned_install_is_never_armed_over() {
        let id = fake_identity("v0.6.6-rc9.14");
        let pinned = InstalledBuild { pinned: true, ..installed("0.6.6-rc9.12", "c0") };
        assert!(arm_refusal(false, Some(&installed("0.6.6-rc9.12", "c0")), &id).is_none());
        assert_eq!(
            arm_refusal(false, Some(&pinned), &id).unwrap(),
            "pinned at 0.6.6-rc9.12: automatic updates stay off for this install until a run that names no version"
        );
        assert!(!pinned.admits("0.6.6-rc9.14"));
    }

    #[test]
    fn update_apply_refuses_a_foreign_host_and_a_pointer_that_no_longer_fits() {
        let fits = pointer("v0.6.6-rc9.14", Some(("0.6.6-rc9.13", "c0")));
        let now = installed("0.6.6-rc9.13", "c0");
        assert!(apply_refusal(false, Ok(Some(now.clone())), &fits).is_none());
        assert!(apply_refusal(false, Ok(None), &fits).is_none(), "no install manifest: nothing to compare");
        assert!(apply_refusal(true, Ok(Some(now)), &fits).unwrap().contains("neither daemon nor frontend"));
        let moved = installed("0.6.6-rc9.14-dev+d94ed0fac", "c1");
        assert!(apply_refusal(false, Ok(Some(moved.clone())), &fits).unwrap().contains("will not apply"));
        // A pointer armed before the record existed: judged by its version alone.
        let old = pointer("v0.6.6-rc9.13", None);
        assert!(apply_refusal(false, Ok(Some(moved)), &old).unwrap().contains("dev build"));
    }

    #[test]
    fn update_apply_refuses_a_pointer_armed_over_an_install_it_cannot_read() {
        let cut = UnreadableInstall { path: "/p/install.json".into(), cause: "EOF while parsing".into(), missing: false };
        let armed = pointer("v0.6.6-rc9.14", Some(("0.6.6-rc9.13", "c0")));
        let why = apply_refusal(false, Err(cut.clone()), &armed).unwrap();
        assert!(why.contains("will not apply") && why.contains(&cut.sentence()), "{why}");
        assert!(apply_refusal(false, Err(cut), &pointer("v0.6.6-rc9.14", None)).is_none(), "no record: the apply's tag check judges it");
    }

    /// The probe `a_check_at_a_non_default_prefix...` runs in a copy of this binary placed under a prefix: what
    /// `check_basis` says there, printed between markers (the test is a no-op without its marker variable).
    #[test]
    fn probe_check_basis_from_a_prefix() {
        if std::env::var_os("SOT_TEST_PROBE_CHECK_BASIS").is_none() {
            return;
        }
        let process = Updater { current: "0.6.6-rc9.12".into(), dev: false, mode: Mode::Notify, repo: "o/r".into() };
        println!("PROBE[{:?}]", process.check_basis());
    }

    // The review's measurement: the exe's location decides the root whenever install.json is there, parsed or not.
    // At a prefix that is not the default one, with a cut record, the check must not fall back to the boot version or
    // read another install's (SOT_UPDATE_ROOT here stands for the other install a fallback would reach).
    #[test]
    fn a_check_at_a_non_default_prefix_over_a_cut_record_says_so() {
        let scratch = tempfile::tempdir().unwrap();
        let (prefix, other) = (scratch.path().join("opt"), scratch.path().join("other"));
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::create_dir_all(other.join("updates")).unwrap();
        std::fs::write(other.join("install.json"), r#"{"schema":1,"prefix":"/o","version":"0.6.5"}"#).unwrap();
        let exe = prefix.join("bin").join(std::env::current_exe().unwrap().file_name().unwrap());
        sot_log::test_exec::write_executable(&exe, std::fs::read(std::env::current_exe().unwrap()).unwrap());
        let probe = |record: Option<&str>| {
            match record {
                Some(record) => std::fs::write(prefix.join("install.json"), record).unwrap(),
                None => drop(std::fs::remove_file(prefix.join("install.json"))),
            }
            let ran = sot_log::test_wait::run_within(
                std::process::Command::new(&exe)
                    .args(["--exact", "update::tests::probe_check_basis_from_a_prefix", "--nocapture", "--test-threads=1"])
                    .env("SOT_TEST_PROBE_CHECK_BASIS", "1")
                    .env("SOT_UPDATE_ROOT", other.join("updates")),
                Duration::from_secs(60),
            );
            let text = ran.stdout;
            text.split_once("PROBE[").and_then(|(_, rest)| rest.lines().next()?.strip_suffix(']')).map(str::to_string).unwrap_or(text)
        };
        let cut = probe(Some(r#"{"schema":1,"prefix":"/p","version":"0.6.7","pinned":tr"#));
        assert!(cut.starts_with("Err(\"disabled: install.json cannot be read at "), "{cut}");
        assert!(cut.contains(&prefix.join("install.json").display().to_string().replace('\\', "\\\\")), "names this install's file: {cut}");
        let run = if cfg!(windows) { "install.ps1 -Version" } else { "install.sh --version" };
        assert!(cut.contains(run), "names the remedy: {cut}");
        let whole = probe(Some(r#"{"schema":1,"prefix":"/p","version":"0.6.7"}"#));
        assert_eq!(whole, r#"Ok("0.6.7")"#, "the exe's own record decides, not the other install's");
        // Missing: on Windows, beside bin\sotd.exe, it is refused like a cut record; elsewhere absent is no install and
        // the check falls to the other root, as before.
        std::fs::write(prefix.join("bin").join("sotd.exe"), b"").unwrap();
        let missing = probe(None);
        if cfg!(windows) {
            assert!(missing.starts_with("Err(\"disabled: install.json is missing at "), "{missing}");
            assert!(missing.contains("install.ps1 -Version") && !missing.contains("repair"), "{missing}");
        } else {
            assert_eq!(missing, r#"Ok("0.6.5")"#, "Unix: absent is no install");
        }
    }

    // The channel comes from the install on disk, not from the version the process booted with.
    #[test]
    fn a_check_follows_the_install_not_the_process() {
        let _guard = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("SOT_UPDATE_ROOT");
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("updates")).unwrap();
        std::env::set_var("SOT_UPDATE_ROOT", dir.path().join("updates"));
        let installed_at = |version: &str| {
            std::fs::write(dir.path().join("install.json"), format!(r#"{{"schema":1,"prefix":"/p","version":"{version}"}}"#)).unwrap();
        };
        let process_on_an_rc = Updater { current: "0.6.6-rc9.12".into(), dev: false, mode: Mode::Notify, repo: "o/r".into() };
        let none = process_on_an_rc.check_basis();
        installed_at("0.6.7");
        let stable = process_on_an_rc.check_basis();
        installed_at("0.6.7-dev+abc");
        let dev = process_on_an_rc.check_basis();
        std::fs::write(dir.path().join("install.json"), r#"{"schema":1,"prefix":"/p","version":"0.6.6-rc9.12","pinned":true}"#).unwrap();
        let pinned = process_on_an_rc.check_basis();
        match saved {
            Some(v) => std::env::set_var("SOT_UPDATE_ROOT", v),
            None => std::env::remove_var("SOT_UPDATE_ROOT"),
        }
        assert_eq!(none, Ok("0.6.6-rc9.12".to_string()), "no install: the process's version");
        assert_eq!(stable, Ok("0.6.7".to_string()), "the install's channel, not the process's rc");
        assert!(dev.is_err(), "a dev install is no basis for a release check");
        assert_eq!(
            pinned,
            Err("disabled: pinned at 0.6.6-rc9.12: automatic updates stay off for this install until a run that names no version".to_string()),
            "a pinned install is no basis either, and the verdict says why"
        );
    }

    // An install.json that cannot be read may hold a pin, so the check does not fall back to the boot version.
    #[test]
    fn a_check_over_an_unreadable_install_says_so() {
        let _guard = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("SOT_UPDATE_ROOT");
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("updates")).unwrap();
        std::env::set_var("SOT_UPDATE_ROOT", dir.path().join("updates"));
        std::fs::write(dir.path().join("install.json"), r#"{"schema":1,"prefix":"/p","version":"0.6.7","pinned":tr"#).unwrap();
        let process = Updater { current: "0.6.6-rc9.12".into(), dev: false, mode: Mode::Notify, repo: "o/r".into() };
        let cut = process.check_basis();
        std::fs::write(dir.path().join("install.json"), "\u{feff}{\"schema\":1,\"prefix\":\"/p\",\"version\":\"0.6.7\"}").unwrap();
        let bom = process.check_basis();
        match saved {
            Some(v) => std::env::set_var("SOT_UPDATE_ROOT", v),
            None => std::env::remove_var("SOT_UPDATE_ROOT"),
        }
        let said = cut.unwrap_err();
        assert!(said.starts_with("disabled: install.json cannot be read"), "{said}");
        assert!(said.contains(if cfg!(windows) { "install.ps1 -Version" } else { "install.sh --version" }), "the status names the remedy: {said}");
        assert_eq!(bom, Ok("0.6.7".to_string()), "a BOM alone is not unreadable");
    }

    #[test]
    fn a_failure_names_every_context_down_to_its_cause() {
        let e = anyhow::anyhow!("os error 2").context("fetching tags into base clone");
        assert_eq!(chain(&e), "fetching tags into base clone: os error 2");
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
        let _block = STAGE_BLOCK_TEST.blocking_lock();
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
