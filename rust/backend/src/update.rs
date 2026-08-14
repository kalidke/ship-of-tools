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
use std::time::Duration;
use tokio::sync::broadcast;

use sot_updater::prepare::{PrepareSpec, PreparedState};
use sot_updater::{CheckOutcome, Fetcher, InstallManifest, ReleaseIdentity, UpdaterConfig};

use crate::handlers::HandlerOutput;

/// Default release repo. Overridable via `SOT_UPDATE_REPO` for testing against
/// a fork.
const DEFAULT_REPO: &str = "kalidke/ship-of-tools";

/// Delay before the first automatic check after boot, then the steady cadence.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(120);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 3600);

// ─── Config ─────────────────────────────────────────────────────────────

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

fn repo_from_env() -> String {
    std::env::var("SOT_UPDATE_REPO")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_REPO.to_string())
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
        let dev = current.contains("-dev");
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
        sot_updater::check_release(&self.repo, &self.current, &Fetcher::from_env()).await
    }
}

/// Process-local dedupe for the background pipeline: repeated `update.check`
/// ops during one in-flight multi-minute download must NOT pile up tasks
/// that all block on (their own process's) staging lock. One pipeline run at
/// a time per daemon; extra requests are a cheap no-op — the next check
/// reports the truth.
static PIPELINE_RUNNING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

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
    if let Err(e) = sot_updater::stage(cfg, id).await {
        tracing::warn!(tag = %id.tag, error = %e, "staging update failed");
        return;
    }
    let Some(install) = InstallManifest::for_current_exe() else {
        tracing::info!(tag = %id.tag, "staged (no install manifest — prepare/arm skipped)");
        return;
    };
    let spec = prepare_spec(&install, cfg, id);
    let state = match sot_updater::prepare::prepare(&spec).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(tag = %id.tag, error = %e, "preparing update failed — not arming");
            return;
        }
    };
    match sot_updater::pending::arm(&cfg.updates_root, id, &state.checkout, &state.commit).await {
        Ok(true) => {}
        Ok(false) => tracing::info!(tag = %id.tag, "a newer release is already armed"),
        Err(e) => tracing::warn!(tag = %id.tag, error = %e, "arming update failed"),
    }
}

fn prepare_spec(install: &InstallManifest, cfg: &UpdaterConfig, id: &ReleaseIdentity) -> PrepareSpec {
    let backend_role = install.role != "remote";
    PrepareSpec {
        identity: id.clone(),
        repo_dir: install.prefix.join("repo"),
        stage_dir: sot_updater::stage_dir(&cfg.updates_root, id),
        origin_url: None,
        julia_bin: backend_role.then(|| {
            std::env::var("SOT_JULIA_BIN").unwrap_or_else(|_| "julia".to_string())
        }),
        npm: backend_role,
    }
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
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            run_check_once(&updater, &fe_command_tx, &clients).await;
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
) {
    let out = updater.check().await;
    if out.update_available {
        tracing::info!(latest = %out.latest, current = %updater.current, "update available");
        let evt = FeCommandEvt {
            v: 1,
            cmd: "notify".into(),
            args: json!({ "text": notify_text(&out.latest, &updater.current) }),
            target: None,
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
                sot_updater::pending::read(&cfg.updates_root).await,
                Ok(Some(p)) if p.identity == id
            );
            let attached = clients.count();
            if armed && attached == 0 {
                tracing::info!(tag = %id.tag, "auto mode: armed and no clients attached — exiting for the apply owner");
                tokio::time::sleep(Duration::from_millis(250)).await;
                std::process::exit(0);
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
    let mechanism = updater.mechanism().ok();
    let (staged, prepared, armed) = match (&out.identity, &mechanism) {
        (Some(id), Some(cfg)) => {
            let stage_dir = sot_updater::stage_dir(&cfg.updates_root, id);
            (
                sot_updater::is_staged(&cfg.updates_root, id).await,
                PreparedState::matches(&stage_dir, id).await,
                matches!(
                    sot_updater::pending::read(&cfg.updates_root).await,
                    Ok(Some(p)) if p.identity == *id
                ),
            )
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
    let pending = match sot_updater::pending::read(&cfg.updates_root).await {
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
    };
    let _ = fe_command_tx.send(evt);

    // Give the response + notify time to flush, then exit 0. Under systemd
    // (Restart=always) the ExecStartPre apply runs on the way back up; for
    // launcher-managed daemons the next sot-launch applies and starts fresh.
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(750)).await;
        tracing::info!("update.apply: exiting now");
        std::process::exit(0);
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
