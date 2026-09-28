// reauth.rs — `workspace.reauth` (ADR 0046 decision 6): move a LIVE
// capsule row to another account and resume the same conversation there.
//
// The row's own session is the thing being replaced, so the ordering is
// the whole design. Validate everything first; record the new account on
// the row (and in its toml) while the old leg is still running; hand the
// accept frame back to the caller to WRITE; only then end the leg and
// spawn the replacement. A refusal therefore changes nothing at all — the
// session that reads it is still the live one — and an accept is the last
// thing that session ever prints. Same shape `sot_log::capsule`'s mgmt
// lane already uses for the identical problem (it drives `Action::Shutdown`
// only after its ack was physically written).
//
// Three outcomes, not two, and all three leave ONE truth: a refusal (the
// record never moved), an accept whose frame reached its reader (the record
// moved and the leg is replaced), and an accept whose frame did NOT reach
// it — a dead or non-draining peer, the connection ending on the `?` that
// write returns. The third is the one that needs an act rather than a
// return: nothing was torn down, so the live leg still spends the old
// login, and the record is rolled back to say so
// ([`ReauthRestart::rollback`]) before the error propagates.
//
// Why the record moves before the restart: every path that later starts a
// leg for this row reads `Workspace::account` from the registry at spawn
// time (`capsule_workspace::runtime::spawn_and_watch`) — this call's own
// spawn, the watchdog's restart, the daemon's boot resume, and `pty.open`'s
// start-on-attach. Writing it first means there is ONE truth no matter
// which of those wins the race, and the worst case after an accept is a
// row resting at `ended_no_respawn` that the next attach revives on the
// NEW login — never a row with no way back.
//
// Why not `workspace.create` for the same slug: `Workspaces::insert`
// rebuilds the row from `Workspace::meta_only`, which BLANKS the declared
// `agent_handle` (the row goes colourless and loses its comm identity), it
// spawns a second supervisor for a row that already has one, and on a
// synchronous spawn failure `workspace.create` rolls back — deleting a
// live row. This op mutates exactly one field instead.

use anyhow::Result;
use serde_json::json;
use sot_protocol::{op, Frame, WorkspaceReauthRes};
use std::path::{Path, PathBuf};

use crate::accounts::DiscoveredAccount;
use crate::handlers::HandlerOutput;
use crate::workspaces::Workspaces;

/// The reply code an accepted reauth answers with, before anything is
/// torn down. The caller (`server.rs`) writes this frame and THEN performs
/// [`restart_blocking`]; nothing else may reorder those two.
pub const ACCEPTED_CODE: &str = "reauth_accepted";

/// A refusal: the wire `code`, the human line, and the accounts this
/// daemon can actually see right now — carried on EVERY refusal so the
/// caller never re-implements discovery to explain one.
pub(crate) struct Refusal {
    pub code: &'static str,
    pub error: String,
    pub accounts: Vec<String>,
}

/// Everything the restart needs, and the row's guard it runs under. Built
/// only on the accept path, and only after every refusal has been ruled
/// out, so holding one means the record is already updated and the ack is
/// the caller's next act.
pub struct ReauthRestart {
    /// The row itself — the `Arc` [`Workspaces::set_account`] mutated,
    /// carried rather than copied field by field so nothing here can drift
    /// from the registry it was taken from: the id, slug, agent name, root
    /// and the account all come off it, and the ACCOUNT is read fresh at
    /// spawn time, the same rule the watchdog's crash restart follows.
    row: std::sync::Arc<crate::workspaces::Workspace>,
    /// The account this row ran as before the record moved — the value
    /// [`Self::rollback`] puts back on every path that leaves the OLD leg
    /// running.
    previous: String,
    /// Absolute, canonicalized where possible (see `root_canonicalized`).
    state_root: PathBuf,
    root_canonicalized: bool,
    /// The replacement leg's producer argv — resolved BEFORE the ack, so
    /// an unresolvable `claude` is a refusal the caller can still read.
    argv: Vec<String>,
    /// The registry the replacement spawn reads this row's account back
    /// out of (`spawn_and_watch`), carried rather than re-resolved.
    workspaces: Workspaces,
    /// Held from validation through the spawn, so no watchdog restart, no
    /// boot resume and no start-on-attach can land a second supervisor in
    /// the window this call opens.
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

impl ReauthRestart {
    /// Put the record back where the accept moved it from. Every caller is
    /// the same shape: the switch did NOT happen, the old leg is still
    /// running on the old login, and the record is the half that can still
    /// be made to agree — a write that never reached its reader
    /// ([`write_accept_then`]) and every outcome in [`restart_blocking`]
    /// that could not end the leg. Consumes the plan, so the row's guard is
    /// released only once the record is honest again.
    pub fn rollback(self) {
        // Through the registry, not through the `Arc` this plan holds: if a
        // concurrent `workspace.create` swapped the row, the record that
        // needs correcting is the one the registry answers with now.
        let Some(row) = self.workspaces.set_account(&self.row.workspace_id, &self.previous) else {
            return; // the row is gone; there is no record left to correct
        };
        tracing::warn!(
            workspace_id = %self.row.workspace_id, account = %discovery_name(&self.previous),
            "workspace.reauth: the switch did not take; the row's account is back to the login its live leg actually spends"
        );
        if let Err(e) = crate::workspaces::save(&row) {
            tracing::error!(
                workspace_id = %self.row.workspace_id, error = %e,
                "workspace.reauth: rolled the account back in memory but could not persist it; a daemon restart would read the account this row never moved to"
            );
        }
    }
}

/// The accept's ordering, in ONE place rather than in two adjacent
/// statements: the frame goes out, and only then does anything touch the
/// leg. A write that FAILS is the third outcome (see the module doc) — the
/// caller never learned the switch happened, so the record is rolled back
/// and the error still propagates, ending the connection exactly as a bare
/// `?` did. Generic over the writer and over what a restart is handed to,
/// so the order is pinned by a test instead of by adjacency; `server.rs`
/// passes its own connection and its own detached-restart spawn.
pub async fn write_accept_then<W, F>(
    tx: &mut W,
    out: &HandlerOutput,
    restart: Option<ReauthRestart>,
    run: F,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
    F: FnOnce(ReauthRestart),
{
    for (frame, blob) in out {
        if let Err(e) = crate::server::write_frame_to(tx, frame, blob.as_deref()).await {
            if let Some(plan) = restart {
                plan.rollback();
            }
            return Err(e);
        }
    }
    if let Some(plan) = restart {
        run(plan);
    }
    Ok(())
}

/// `""` and the literal `"default"` both name the agent's own config
/// folder — the convention `accounts::account_env` and `Workspace::account`
/// already share (a row NEVER persists `"default"`). Normalized once here
/// so "switch to the account I am already on" is refused whichever
/// spelling the caller used.
fn normalize(account: &str) -> &str {
    if account == "default" {
        ""
    } else {
        account
    }
}

/// The name discovery reports for an account: `""` is `"default"` there.
fn discovery_name(account: &str) -> &str {
    if account.is_empty() {
        "default"
    } else {
        account
    }
}

/// Whether `account` can actually open transcript `resume`: one
/// `projects/<project>/<resume>.jsonl` under its own config dir
/// ([`crate::accounts::claude_config_dir`]). Globbed over `projects`'
/// children rather than rebuilding claude's own cwd-to-directory mangling
/// — the id is unique across that tree, and a rule this daemon copied
/// would be a rule it could get wrong.
fn resume_reachable(home: &Path, account: &str, resume: &str) -> bool {
    let transcript = format!("{resume}.jsonl");
    let projects = crate::accounts::claude_config_dir(home, account).join("projects");
    let Ok(entries) = std::fs::read_dir(projects) else {
        return false;
    };
    entries.flatten().any(|e| e.path().join(&transcript).is_file())
}

/// Every refusal this op owns, decided BEFORE anything is touched, pure
/// over the row's own facts plus the home the accounts live in — the same
/// reason [`crate::accounts::account_env`] is pure: the check and the real
/// spawn share ONE rule instead of a copy each could drift from.
///
/// `resume` is required with no default: `--continue` resolves "the most
/// recent conversation" from a per-account `.claude.json` that is never
/// shared, so across a switch of login it either resolves to nothing or to
/// a DIFFERENT conversation. An explicit transcript id is the only honest
/// selector, and an absent one is a refusal rather than a fallback.
pub(crate) fn check(
    runtime: &str,
    agent_kind: &str,
    current_account: &str,
    account: &str,
    resume: &str,
    home: &Path,
    accounts: &[DiscoveredAccount],
) -> Result<(), Refusal> {
    let names: Vec<String> = accounts.iter().map(|a| a.name.clone()).collect();
    let refuse = |code: &'static str, error: String| {
        Err(Refusal { code, error, accounts: names.clone() })
    };
    let want = normalize(account);

    if runtime != "capsule" {
        return refuse(
            "runtime_not_capsule",
            format!("this row's runtime is {runtime:?}; only a capsule row's agent can be replaced in place"),
        );
    }
    if agent_kind != "claude" {
        return refuse(
            "agent_not_claude",
            format!("this row's agent is {agent_kind:?}; only a claude row has an account to switch and a transcript to resume"),
        );
    }
    // `account_env`'s own refusals, verbatim: an invalid name, a codex or
    // bash row, and an undiscovered folder — including the exact one-line
    // `mkdir` fix for the last. Never restated here.
    if let Err(e) = crate::accounts::account_env(agent_kind, want, home) {
        return refuse("unknown_account", e);
    }
    // A folder with no login is a valid account (the first session in it
    // logs in) — but switching a LIVE conversation into one strands it
    // behind a login prompt, so this op refuses what `workspace.create`
    // accepts.
    let logged_in = accounts
        .iter()
        .find(|a| a.name == discovery_name(want))
        .and_then(|a| a.logged_in.get("claude").copied())
        .unwrap_or(false);
    if !logged_in {
        return refuse(
            "account_not_logged_in",
            format!(
                "account {:?} has no claude login yet: run a session in it once (its own `claude` login) before moving a conversation there",
                discovery_name(want)
            ),
        );
    }
    if want == normalize(current_account) {
        return refuse(
            "already_on_account",
            format!(
                "this row already runs as {:?}; a restart would cost the conversation's cache for nothing",
                discovery_name(want)
            ),
        );
    }
    if resume.is_empty() {
        return refuse(
            "resume_required",
            "resume is required: name the transcript id to resume (the row's own CLAUDE_CODE_SESSION_ID) — there is no \"most recent conversation\" to fall back to on another login".to_string(),
        );
    }
    // The refusal that protects the kill, and the reason it lives HERE:
    // `claude --resume <id>` on an id the target cannot see exits at once,
    // the supervisor flaps the row to `Terminal`, and the conversation is
    // reachable again only by reauthing back — so the only actor that can
    // read both accounts' folders proves reachability BEFORE the accept,
    // rather than asking the leg to check its own grave.
    if !resume_reachable(home, want, resume) {
        return refuse(
            "resume_unreachable",
            format!(
                "account {:?} cannot see transcript {resume:?}: no projects/*/{resume}.jsonl under its config dir — either that is not this conversation's id, or that account folder has its own REAL `projects` instead of the shared symlink, in which case the resume would land in a fresh, empty conversation",
                discovery_name(want)
            ),
        );
    }
    Ok(())
}

/// The ONE refusal frame this op builds — one constructor, so the
/// discovered accounts ride on every refusal as both the module doc above
/// and `ops.rs`'s wire doc promise. The only refusals that answer with an
/// empty list are the two that genuinely precede discovery (a payload that
/// will not parse, a home that will not resolve); a second bare
/// constructor for them is exactly how the promise stopped being true.
fn refused(req_id: u64, r: Refusal) -> HandlerOutput {
    vec![(
        Frame::res(
            req_id,
            op::WORKSPACE_REAUTH,
            json!({ "error": r.error, "code": r.code, "accounts": r.accounts }),
        ),
        None,
    )]
}

/// Validate, record, and hand back BOTH the frame to write and (on an
/// accept) the restart to run after writing it. The split is the ordering:
/// this function never touches the leg, so a caller that writes the frame
/// first cannot get it wrong.
pub async fn handle_workspace_reauth(
    req_id: u64,
    payload_json: serde_json::Value,
    workspaces: &Workspaces,
) -> Result<(HandlerOutput, Option<ReauthRestart>)> {
    // The two refusals that precede discovery: no home is resolved yet, so
    // their account list is empty because there is nothing to list, not
    // because this op withheld it.
    let req: sot_protocol::WorkspaceReauthReq = match serde_json::from_value(payload_json) {
        Ok(r) => r,
        Err(e) => {
            let error = format!("workspace.reauth payload: {e}");
            return Ok((refused(req_id, Refusal { code: "bad_request", error, accounts: Vec::new() }), None));
        }
    };
    tracing::info!(workspace_id = %req.workspace_id, account = %req.account, "workspace.reauth");

    let Some(home) = crate::accounts::account_home() else {
        let error = "could not resolve this daemon's own home, so no account can be resolved against it".to_string();
        return Ok((refused(req_id, Refusal { code: "no_home", error, accounts: Vec::new() }), None));
    };
    let accounts = crate::accounts::discover_accounts(&home);
    // Every refusal from here down carries what this daemon can see, by
    // construction rather than per call site.
    let names: Vec<String> = accounts.iter().map(|a| a.name.clone()).collect();
    let refuse = |code: &'static str, error: String| -> Result<(HandlerOutput, Option<ReauthRestart>)> {
        Ok((refused(req_id, Refusal { code, error, accounts: names.clone() }), None))
    };
    // The one refusal a row can hit twice: once here, and once more after
    // the guard is held, because a destroy could have won the wait.
    let gone = || "the workspace was removed before its reauth could start".to_string();

    let Some(ws) = workspaces.resolve(Some(&req.workspace_id)) else {
        return refuse("unknown_workspace", format!("no workspace {:?} is registered here", req.workspace_id));
    };
    let want = normalize(&req.account).to_string();
    if let Err(r) = check(
        &ws.runtime,
        &ws.agent(),
        &ws.account(),
        &req.account,
        &req.resume,
        &home,
        &accounts,
    ) {
        return Ok((refused(req_id, r), None));
    }

    // ADR 0043 decision 33: this row's own guard, taken OWNED so it stays
    // held past this function's return — through the caller's frame write
    // and the restart that follows it. `None` means the row is already
    // gone; membership is rechecked once the lock is actually held,
    // because a destroy could have won the wait.
    let Some(guard) = workspaces.capsule_guard(&ws.workspace_id) else {
        return refuse("unknown_workspace", gone());
    };
    let guard = guard.lock_owned().await;
    if workspaces.resolve(Some(&ws.workspace_id)).is_none() {
        return refuse("unknown_workspace", gone());
    }

    // Everything that can still fail has to fail BEFORE the record moves
    // and before the ack: after the ack there is no reader left to tell.
    let Some(state_root) = sot_log::state_dir::sot_state_dir() else {
        return refuse(
            "no_state_root",
            format!(
                "could not resolve this machine's state root ({} unset)",
                crate::capsule_workspace::STATE_ROOT_HINT
            ),
        );
    };
    // Same reason `workspace.destroy` canonicalizes the ROOT before
    // joining the row's id (see `destroy_capsule_workspace`): a live
    // supervisor bound its lane at the canonical path, so dialing a
    // symlinked one would miss it. A root that will not canonicalize
    // disables `end_run`'s orphan proof for this call, nothing more.
    let (state_root, root_canonicalized) = match state_root.canonicalize() {
        Ok(canonical) => (canonical, true),
        Err(e) => {
            tracing::debug!(
                state_root = ?state_root, error = %e,
                "workspace.reauth: state root did not canonicalize; the orphan proof is refused this call"
            );
            (state_root, false)
        }
    };
    let argv = match crate::capsule_workspace::claude_resume_argv(
        &req.resume,
        Some(std::path::Path::new(&ws.project_root)),
    ) {
        Ok(argv) => argv,
        Err(e) => return refuse("launcher_unresolved", e),
    };

    // The record moves now, while the old leg is still running. What is
    // persisted — and what the restart is built from — is the `Arc`
    // `set_account` itself mutated, never the one resolved before the
    // guard: `Workspaces::insert` is NOT taken under this guard, so a
    // concurrent `workspace.create` for the same slug can swap the
    // registry's `Arc` for this row inside the window, and saving the
    // stale one would write the OLD account into a toml the registry no
    // longer agrees with.
    let previous = ws.account();
    let Some(row) = workspaces.set_account(&ws.workspace_id, &want) else {
        return refuse("unknown_workspace", gone());
    };
    if let Err(e) = crate::workspaces::save(&row) {
        // An unpersisted switch is a row that comes back on the OLD login
        // after any daemon restart while its live leg spends the new one —
        // two truths. Put the field back and refuse; nothing else has been
        // touched yet, so this is still a reauth that changed nothing.
        workspaces.set_account(&ws.workspace_id, &previous);
        return refuse("persist_failed", format!("could not persist the row's new account: {e}"));
    }

    // `discovery_name`, not the normalized value: a switch TO the default
    // account records `""` and must still ANSWER with a name, or the CLI
    // prints `account=` and the human reads it as a missing field.
    let res = WorkspaceReauthRes {
        code: ACCEPTED_CODE.to_string(),
        workspace_id: row.workspace_id.clone(),
        account: discovery_name(&want).to_string(),
    };
    let out = vec![(
        Frame::res(req_id, op::WORKSPACE_REAUTH, serde_json::to_value(res)?),
        None,
    )];
    let restart = ReauthRestart {
        row,
        previous,
        state_root,
        root_canonicalized,
        argv,
        workspaces: workspaces.clone(),
        _guard: guard,
    };
    Ok((out, Some(restart)))
}

/// Whether an `end_run` outcome means the run is OVER, so a replacement
/// supervisor may be spawned for this row. Exhaustive on purpose: a new
/// `EndRunOutcome` variant must be classified here as deliberately as in
/// `handlers.rs`'s `capsule_destroy_outcome_of`, which partitions the same
/// enum for the same underlying question ("does anything still hold this
/// row?"). Anything not-over leaves the leg exactly as it is — still
/// running on the old login — so the caller rolls the RECORD back to that
/// same login: a row nobody could replace must not be a row whose record
/// already says it was.
fn run_ended(outcome: &crate::capsule_workspace::EndRunOutcome) -> Result<(), String> {
    use crate::capsule_workspace::EndRunOutcome as O;
    match outcome {
        O::RecordVerified | O::RecordClosed | O::AlreadyEnded | O::Terminal | O::Unheld | O::Orphaned => Ok(()),
        O::Starting => Err("the supervisor was still starting".to_string()),
        O::NotEnded(detail) => Err(detail.clone()),
    }
}

/// The four effects a revival is made of, behind one name so the ORDER they
/// happen in is pinned by a test instead of by adjacency.
///
/// INVARIANT THIS CARRIES: a revival is three effects — end the old leg,
/// spawn the replacement, mint its voyage — and a path that performs two of
/// them produces a row that LOGS as revived and has no leg. That failure is
/// an ABSENT call, and no pure function can observe an absence:
/// [`ready_to_mint`] can be entirely correct, fully tested, and never
/// invoked. This is the only thing in the module that can see that, and it
/// is the only reason it exists.
///
/// Every method mirrors the signature of the real function it forwards to,
/// so [`LiveSupervisor`]'s impl is a literal forward and nothing can drift
/// between the seam and the thing it stands for. `spawn_replacement` is the
/// exception: its real call takes eight arguments, all read off the plan.
pub(crate) trait RestartEffects {
    fn query_status(&self, state_dir: &Path) -> Result<sot_log::supervisor_client::StatusReport, String>;
    fn end_run(
        &self,
        state_dir: &Path,
        reason: &str,
        root_canonicalized: bool,
    ) -> std::io::Result<crate::capsule_workspace::EndRunOutcome>;
    fn spawn_replacement(&self, plan: &ReauthRestart) -> Result<&'static str, String>;
    fn reset(&self, state_dir: &Path) -> Result<String, String>;
}

/// The production impl, and the ONLY place in this module that names the
/// real `end_run`, `start_supervisor`, `query_status` or `reset`. A future
/// edit that calls one of them directly from [`restart_blocking`] defeats
/// the ordering test silently; the guard is that each of those four paths
/// appears exactly once in this file, inside this impl. Rust cannot enforce
/// that, so it is stated here and checked by grep in the lane's evidence.
pub(crate) struct LiveSupervisor;

impl RestartEffects for LiveSupervisor {
    fn query_status(&self, state_dir: &Path) -> Result<sot_log::supervisor_client::StatusReport, String> {
        sot_log::supervisor_client::query_status(state_dir).map(|(s, _)| s).map_err(|e| e.to_string())
    }
    fn end_run(
        &self,
        state_dir: &Path,
        reason: &str,
        root_canonicalized: bool,
    ) -> std::io::Result<crate::capsule_workspace::EndRunOutcome> {
        crate::capsule_workspace::end_run(state_dir, reason, root_canonicalized)
    }
    fn spawn_replacement(&self, plan: &ReauthRestart) -> Result<&'static str, String> {
        crate::capsule_workspace::start_supervisor(
            &plan.state_root,
            &plan.row.workspace_id,
            crate::capsule_workspace::StartMode::Resume,
            &plan.argv,
            &plan.row.project_root,
            &plan.row.agent_name(),
            &plan.row.slug,
            plan.workspaces.clone(),
        )
    }
    fn reset(&self, state_dir: &Path) -> Result<String, String> {
        sot_log::supervisor_client::reset(state_dir).map_err(|e| e.to_string())
    }
}

/// End the row's current leg and spawn its replacement on the new account.
/// BLOCKING — the caller runs it via `spawn_blocking`, AFTER the accept
/// frame is physically written. Nothing here can answer the caller (it is
/// the process being replaced), so every outcome is a log line — and every
/// outcome that leaves the OLD leg running is a [`ReauthRestart::rollback`]
/// too: the record may not claim a switch that did not happen. Once the leg
/// IS ended the record stands, whatever the replacement spawn does: every
/// later start path reads the account off the registry.
pub fn restart_blocking(plan: ReauthRestart, fx: &dyn RestartEffects) {
    let state_dir = crate::capsule_workspace::state_dir_for(&plan.state_root, &plan.row.workspace_id);
    // Read off the ROW, not off a copy taken before the ack: what the
    // replacement actually spends is whatever the registry says when
    // `spawn_and_watch` resolves it, so naming that same value here keeps
    // the log and the spawn from ever disagreeing.
    let account = plan.row.account();
    let reason = format!("reauth to account {:?}", discovery_name(&account));
    // Who holds the authority BEFORE anything is retired. Read here and
    // nowhere later: `end_run` is about to stop this process, so after it
    // there is nothing left to identify, and the whole point is to notice
    // afterwards if it is STILL the one answering. See
    // [`supervisor_identity`].
    let retired = supervisor_identity(fx, &state_dir);
    let outcome = match fx.end_run(&state_dir, &reason, plan.root_canonicalized) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(
                workspace_id = %plan.row.workspace_id, error = %e,
                "workspace.reauth: could not end the row's leg, so the switch did not happen; rolling the record back to the login the leg still spends"
            );
            return plan.rollback();
        }
    };
    if let Err(detail) = run_ended(&outcome) {
        tracing::warn!(
            workspace_id = %plan.row.workspace_id, detail = %detail,
            "workspace.reauth: the row's run did not end, so no replacement was spawned; rolling the record back to the login the leg still spends"
        );
        return plan.rollback();
    }
    // `StartMode::Resume` — a reauth is never a row's first-ever run, and
    // the account itself is read back off the registry inside this call
    // (`spawn_and_watch`), which is why the record had to move first.
    let phase = match fx.spawn_replacement(&plan) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                workspace_id = %plan.row.workspace_id, error = %e,
                "workspace.reauth: the replacement leg did not spawn; the row rests until it is opened again, on the new account"
            );
            return;
        }
    };
    // A spawn is only two thirds of a revival, and this is the third.
    // `StartMode::Resume` DELIBERATELY never resurrects an ended run (ADR
    // 0041's no-resurrection rule), so the replacement settles at
    // `ended_no_respawn` BY DESIGN: a live supervisor, holding this row's
    // producer argv, with no voyage and nothing on the pty. `reset` mints
    // the voyage the leg actually runs in. `ensure_started`'s `Selection`
    // arm does all three (retire, resume, reset); this path used to do the
    // first two and stop, which left a row that logged as spawned and had
    // no leg at all — data-safe, and silent, which is worse.
    //
    // Why the reset belongs HERE and may not be left to the next attach:
    // `plan.argv` carries `--resume <id>` from `claude_resume_argv`, built
    // for this one call, and that id is never persisted on the row. An
    // attach retires this supervisor and respawns from the ordinary
    // `agent_argv`, whose `--continue` selects by recency rather than by
    // id. `--resume` names the conversation outright, so the reset belongs
    // to the one call that still holds the id.
    //
    // But NOT at the instant the spawn returns. `start_supervisor`'s own
    // settle is best effort: it waits `SPAWN_SETTLE_DEADLINE` (2s) and on
    // timeout WARNS and hands back the transient phase anyway. A reauth's
    // state dir is the worst case for that — `end_run` has just run, so
    // the fresh authority has journal reconciliation to do before it can
    // rest, and the attach path's own comment records that such a recovery
    // "can report `starting` through its own recovery for many seconds".
    // `Reset` is admissible ONLY from `EndedNoRespawn`, so a reset fired
    // two seconds into a three-second recovery is REFUSED, leaving exactly
    // the live-supervisor-no-voyage-no-leg row this change exists to
    // prevent — with a red log instead of a green one, which is no better
    // for the conversation. So wait for the authority to rest on its own
    // clock, and then let the phase decide.
    let voyage = match mint_replacement_voyage(fx, &state_dir, retired) {
        Ok(voyage) => voyage,
        Err(MintRefusal::NeverAnswered(detail)) => {
            tracing::error!(
                workspace_id = %plan.row.workspace_id, account = %discovery_name(&account),
                spawn_phase = phase, detail = %detail,
                "workspace.reauth: the replacement authority never answered, so no voyage was minted and this row has no leg"
            );
            return;
        }
        Err(MintRefusal::Refused { settled, detail }) => {
            tracing::error!(
                workspace_id = %plan.row.workspace_id, account = %discovery_name(&account),
                phase = settled, detail = %detail,
                "workspace.reauth: refusing to mint a voyage on this authority, so the switch left no new leg"
            );
            return;
        }
        Err(MintRefusal::MintedNothing { settled, error }) => {
            tracing::error!(
                workspace_id = %plan.row.workspace_id, account = %discovery_name(&account),
                phase = settled, error = %error,
                "workspace.reauth: the reset minted no voyage, so this row has no leg; \
                 opening the row retires this authority and revives it on the conversation `--continue` selects"
            );
            return;
        }
    };
    tracing::info!(
        workspace_id = %plan.row.workspace_id, account = %discovery_name(&account),
        voyage = %voyage,
        // Deliberately narrower than "the leg is running": `reset`
        // answers as soon as the pointer moves, and the leg is spawned
        // after that, in the supervisor's own Resetting -> Spawning
        // transition, where it can still fail into `terminal`. A
        // minted voyage is evidence of the third step being taken, not
        // of a process on the pty.
        "workspace.reauth: a fresh voyage was minted for the replacement leg on the new account"
    );
}

/// Why a revival's tail stopped short of the mint. One variant per line
/// [`restart_blocking`] owes its log: the three failures are not
/// interchangeable to whoever reads it — nothing answered, something
/// answered and was refused, or the mint itself answered nothing — and two
/// of them carry the phase the authority settled at, which only the tail
/// has read. THE LOG RECORDS ARE THE CONTRACT, and one `String` cannot
/// carry three of them.
enum MintRefusal {
    NeverAnswered(String),
    Refused { settled: &'static str, detail: String },
    MintedNothing { settled: &'static str, error: String },
}

/// The revival tail: wait for the replacement authority to come to rest on
/// its own clock, judge it, and mint its voyage. ONE name for the rule, so
/// [`restart_blocking`] above reads as exactly the three effects it is.
///
/// There is a SECOND reset in this tree — `ensure_started_locked`, in
/// `capsule_workspace.rs`, reached when a selection finds the row resting
/// at `ended_no_respawn`. The two are deliberately not shared, and the next
/// reader who finds two of them must not have to re-derive why:
///
///   1. How the predecessor is ended. Attach sends a WAITING stop, with a
///      confirmed exit; a reauth ends the RUN, which also closes the record
///      and whose stop is best effort — which is why only this path owes
///      the identity check [`supervisor_identity`] describes.
///   2. What the replacement spends. Attach resolves the row's ordinary
///      agent argv, whose `--continue` selects by recency; a reauth spends
///      the plan's `--resume <id>`, which selects by name and is never
///      persisted on the row, so this is the only call that can mint it.
///   3. How the wait is performed. Attach returns a wait step that RELEASES
///      the row guard and re-runs its whole decision from scratch, because
///      the watchdog takes that guard while restarting a crashed leg. This
///      path polls in place while HOLDING the guard, which is safe by
///      construction: a resumed run is never resurrected, so its
///      replacement rests with no leg at all and no leg can exit. Merging
///      the two waits would break one of them — that is the difference
///      that forecloses unification.
///   4. What identity means. Attach uses it as a fast path to SKIP the
///      retire; this path uses it as a REFUSAL. Opposite polarity, opposite
///      consequence.
fn mint_replacement_voyage(
    fx: &dyn RestartEffects,
    state_dir: &Path,
    retired: Option<(u32, u64)>,
) -> Result<String, MintRefusal> {
    let report = wait_until_resting(fx, state_dir).map_err(MintRefusal::NeverAnswered)?;
    let settled = crate::capsule_workspace::phase_str(report.phase);
    if let Err(detail) = ready_to_mint(report.phase, retired, (report.pid, report.created)) {
        return Err(MintRefusal::Refused { settled, detail });
    }
    match fx.reset(state_dir) {
        Ok(voyage) if !voyage.trim().is_empty() => Ok(voyage),
        other => Err(MintRefusal::MintedNothing {
            settled,
            error: other.err().unwrap_or_else(|| "the reset answered an empty voyage".into()),
        }),
    }
}

/// How long to let the replacement authority recover before giving up on
/// it. `start_supervisor`'s own 2s settle is too short to be believed here
/// (see the call site), and this is a background task on the reauth op, so
/// the cost of waiting is latency nobody is watching, while the cost of
/// not waiting is a refused reset and a row with no leg.
const REPLACEMENT_SETTLE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
const REPLACEMENT_SETTLE_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// The identity of whatever authority currently answers for this row, as
/// the supervisor itself reports it. `None` when nothing answers.
///
/// This exists for one failure this path cannot otherwise see. `end_run`'s
/// own stop is BEST EFFORT: when it leaks, the old supervisor stays
/// resident, this call's `sot-capsule` exits at the authority fence, the
/// spawn still reports `Ok`, and every probe afterwards reads that OLD
/// process resting at `ended_no_respawn` — indistinguishable from a
/// healthy replacement. Minting there would have the old authority spawn
/// the leg from ITS cached argv and ITS environment, the old account
/// included, and this path would log a success naming the new one. So
/// remember who was there before, and require that it changed.
fn supervisor_identity(fx: &dyn RestartEffects, state_dir: &std::path::Path) -> Option<(u32, u64)> {
    fx.query_status(state_dir).ok().map(|s| (s.pid, s.created))
}

/// Polls the replacement authority's own `status` until it rests, or until
/// [`REPLACEMENT_SETTLE_DEADLINE`]. Answers the last report it managed to
/// read; `Err` only when nothing ever answered, since a recovering
/// authority legitimately refuses connections for a while and a single
/// failed read says nothing.
fn wait_until_resting(
    fx: &dyn RestartEffects,
    state_dir: &std::path::Path,
) -> Result<sot_log::supervisor_client::StatusReport, String> {
    let deadline = std::time::Instant::now() + REPLACEMENT_SETTLE_DEADLINE;
    let mut last: Option<sot_log::supervisor_client::StatusReport> = None;
    loop {
        match fx.query_status(state_dir) {
            Ok(report) => {
                if phase_rests(report.phase) {
                    return Ok(report);
                }
                last = Some(report);
            }
            Err(e) => {
                if last.is_none() && std::time::Instant::now() >= deadline {
                    return Err(format!("no status answered within {REPLACEMENT_SETTLE_DEADLINE:?}: {e}"));
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return last.ok_or_else(|| format!("no status answered within {REPLACEMENT_SETTLE_DEADLINE:?}"));
        }
        std::thread::sleep(REPLACEMENT_SETTLE_POLL);
    }
}

/// Whether a phase is one the authority will stay at until somebody acts.
/// The same partition `capsule_workspace`'s own `is_resting_phase` draws,
/// restricted to the phases a live supervisor can report over `status` —
/// this caller has just spawned one and is holding its reply, so the
/// "nothing is there" phases that function also admits cannot arise here.
fn phase_rests(phase: sot_log::wire::SupervisorPhase) -> bool {
    use sot_log::wire::SupervisorPhase as P;
    matches!(phase, P::Ready | P::EndedNoRespawn | P::Terminal)
}

/// Whether this call may mint the replacement's voyage — from the phase
/// its authority settled at, and from whether that authority is actually a
/// NEW one. Pure and exhaustive so both rules are testable without a
/// supervisor, the same reason [`run_ended`] is factored out above.
///
/// This encodes the 0.6.6 defect it was written for and the two the review
/// of that fix found. `restart_blocking` used to report success on a
/// `start_supervisor` success ALONE, with the settled phase merely printed
/// in the same line; the first version of the fix then minted regardless
/// of the phase, merely warning when it was unexpected. Both are wrong in
/// the same direction — they announce a revival they have not established:
///
///   * `EndedNoRespawn` is the ONLY phase `Reset` is admissible from, so
///     it is the only one where minting can succeed at all;
///   * `Ready` means a leg is ALREADY live — an `end_run` whose stop ended
///     the authority but not the leg, which `--resume` then adopted — so
///     the row has a leg, on the OLD account, and minting is refused with
///     "a leg is currently live";
///   * `Terminal`, or any phase still transient at the deadline, is a
///     replacement that did not come up, and a reset would be refused.
///
/// The identity rule is separate and catches the leaked retire described
/// on [`supervisor_identity`]: an authority that is the same process the
/// switch was supposed to retire must never be minted on, whatever phase
/// it rests at.
fn ready_to_mint(
    phase: sot_log::wire::SupervisorPhase,
    retired: Option<(u32, u64)>,
    resident: (u32, u64),
) -> Result<(), String> {
    use sot_log::wire::SupervisorPhase as P;
    if retired == Some(resident) {
        return Err(format!(
            "the authority answering is the same process the switch was supposed to retire (pid {}), \
             so the retire leaked and its own cached argv and account would spawn the leg",
            resident.0
        ));
    }
    match phase {
        P::EndedNoRespawn => Ok(()),
        P::Ready => Err("a leg is already live on this row, so the retire did not take; it runs on the login the switch moved away from".into()),
        other => Err(format!(
            "the replacement authority rests at {:?} rather than the phase a resumed run must rest at, and a reset is admissible only from ended_no_respawn",
            crate::capsule_workspace::phase_str(other)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::claude_config_dir;
    use crate::workspaces::Workspace;

    /// Restores every variable these tests pin, under the crate-wide
    /// serialization every env-mutating test module here shares
    /// (`paths::ENV_TEST_LOCK`) — `HOME`/`USERPROFILE` because
    /// `accounts::account_home` reads them, `XDG_CONFIG_HOME` because
    /// `workspaces::save` writes under it, and `XDG_STATE_HOME`/
    /// `LOCALAPPDATA` because the accept path resolves this machine's
    /// state root.
    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        home: Option<std::ffi::OsString>,
        userprofile: Option<std::ffi::OsString>,
        xdg_config_home: Option<std::ffi::OsString>,
        xdg_state_home: Option<std::ffi::OsString>,
        localappdata: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, val) in [
                ("HOME", &self.home),
                ("USERPROFILE", &self.userprofile),
                ("XDG_CONFIG_HOME", &self.xdg_config_home),
                ("XDG_STATE_HOME", &self.xdg_state_home),
                ("LOCALAPPDATA", &self.localappdata),
            ] {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn env_guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            _serial: serial,
            home: std::env::var_os("HOME"),
            userprofile: std::env::var_os("USERPROFILE"),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            xdg_state_home: std::env::var_os("XDG_STATE_HOME"),
            localappdata: std::env::var_os("LOCALAPPDATA"),
        }
    }

    /// A home with a default `.claude` folder and one `.claude-auth`
    /// subdirectory per named account — `logged_in` decides whether its
    /// `.credentials.json` exists, which is the whole difference between
    /// an account this op accepts and one it refuses.
    fn home_with(default_logged_in: bool, named: &[(&str, bool)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let claude = dir.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        if default_logged_in {
            std::fs::write(claude.join(".credentials.json"), b"{}").unwrap();
        }
        for (name, logged_in) in named {
            let acct = dir.path().join(".claude-auth").join(name);
            std::fs::create_dir_all(&acct).unwrap();
            if *logged_in {
                std::fs::write(acct.join(".credentials.json"), b"{}").unwrap();
            }
        }
        dir
    }

    /// `claude_resume_argv` resolves a real binary on Unix, so the accept
    /// path needs one under the pinned home; the Windows arm resolves
    /// nothing and needs no fixture.
    #[cfg(unix)]
    fn seed_claude_binary(home: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let claude = bin.join("claude");
        std::fs::write(&claude, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(windows)]
    fn seed_claude_binary(_home: &Path) {}

    /// One transcript the account owning `config_dir` can open:
    /// `projects/<project>/<id>.jsonl`, the shape `check` globs for. In the
    /// real tree a named account reaches the very same file through the
    /// shared `projects` symlink (`accounts::SHARED_ENTRIES`); these tests
    /// seed the folder being asked about directly, which is what the glob
    /// resolves to either way.
    fn seed_transcript(config_dir: &Path, id: &str) {
        let project = config_dir.join("projects").join("-a-project-root");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join(format!("{id}.jsonl")), b"{}\n").unwrap();
    }

    fn refusal_of(
        runtime: &str,
        agent_kind: &str,
        current_account: &str,
        account: &str,
        resume: &str,
        home: &Path,
    ) -> Refusal {
        let accounts = crate::accounts::discover_accounts(home);
        check(runtime, agent_kind, current_account, account, resume, home, &accounts)
            .err()
            .expect("this case must be refused")
    }

    // Every refusal below is decided with nothing touched: `check` reads
    // the row's facts and the accounts on disk, and nothing else.
    #[test]
    fn a_non_capsule_row_is_refused() {
        let home = home_with(true, &[("team", true)]);
        let r = refusal_of("tmux", "claude", "", "team", "sid", home.path());
        assert_eq!(r.code, "runtime_not_capsule");
    }

    #[test]
    fn a_row_whose_agent_is_not_claude_is_refused() {
        let home = home_with(true, &[("team", true)]);
        for kind in ["codex", "none"] {
            let r = refusal_of("capsule", kind, "", "team", "sid", home.path());
            assert_eq!(r.code, "agent_not_claude", "kind {kind}");
        }
    }

    // `account_env`'s own message, carried verbatim — including the exact
    // one-line fix, which the skill prints rather than restating.
    #[test]
    fn an_undiscovered_account_is_refused_with_the_fix_line() {
        let home = home_with(true, &[("team", true)]);
        let r = refusal_of("capsule", "claude", "", "ghost", "sid", home.path());
        assert_eq!(r.code, "unknown_account");
        assert!(r.error.contains("mkdir -p"), "{}", r.error);
    }

    #[test]
    fn an_invalid_account_name_is_refused_before_any_path_join() {
        let home = home_with(true, &[("team", true)]);
        let r = refusal_of("capsule", "claude", "", "../.claude", "sid", home.path());
        assert_eq!(r.code, "unknown_account");
        assert!(r.error.contains("invalid account name"), "{}", r.error);
    }

    // A folder with no login is a valid account to CREATE a row in and a
    // refusal to move a live conversation into: the switch would strand it
    // behind a login prompt.
    #[test]
    fn an_account_with_no_login_is_refused() {
        let home = home_with(true, &[("fresh", false)]);
        let r = refusal_of("capsule", "claude", "", "fresh", "sid", home.path());
        assert_eq!(r.code, "account_not_logged_in");
        let no_default_login = home_with(false, &[("team", true)]);
        let r = refusal_of("capsule", "claude", "team", "default", "sid", no_default_login.path());
        assert_eq!(r.code, "account_not_logged_in", "a DEFAULT folder with no login is refused too");
    }

    #[test]
    fn the_account_the_row_already_runs_is_refused_in_both_spellings() {
        let home = home_with(true, &[("team", true)]);
        for (current, asked) in [("", "default"), ("", ""), ("team", "team")] {
            let r = refusal_of("capsule", "claude", current, asked, "sid", home.path());
            assert_eq!(r.code, "already_on_account", "current {current:?} asked {asked:?}");
        }
    }

    #[test]
    fn an_empty_resume_is_refused_rather_than_meaning_continue() {
        let home = home_with(true, &[("team", true)]);
        let r = refusal_of("capsule", "claude", "", "team", "", home.path());
        assert_eq!(r.code, "resume_required");
    }

    // The refusal that protects the kill: an id the TARGET cannot open would
    // be an accept, a killed session, a `claude --resume` that exits at once
    // and a row flapped to Terminal. The daemon is the only actor that can
    // see both folders, so it is the one that proves reachability.
    #[test]
    fn a_transcript_the_target_account_cannot_see_is_refused() {
        let home = home_with(true, &[("team", true)]);
        // The conversation exists — under the DEFAULT folder only, which is
        // exactly what an account folder carrying its own REAL `projects`
        // looks like from the target's side.
        seed_transcript(&claude_config_dir(home.path(), ""), "sid");
        let r = refusal_of("capsule", "claude", "", "team", "sid", home.path());
        assert_eq!(r.code, "resume_unreachable");
        assert!(r.error.contains("sid.jsonl"), "{}", r.error);
        // …and an id that is simply not this conversation's is the same
        // refusal, even with the folder fully shared.
        seed_transcript(&claude_config_dir(home.path(), "team"), "sid");
        let r = refusal_of("capsule", "claude", "", "team", "stale-id", home.path());
        assert_eq!(r.code, "resume_unreachable");
    }

    #[test]
    fn a_logged_in_account_on_a_claude_capsule_row_is_accepted() {
        let home = home_with(true, &[("team", true)]);
        seed_transcript(&claude_config_dir(home.path(), "team"), "sid");
        seed_transcript(&claude_config_dir(home.path(), ""), "sid");
        let accounts = crate::accounts::discover_accounts(home.path());
        assert!(check("capsule", "claude", "", "team", "sid", home.path(), &accounts).is_ok());
        // …and back to the default login, which is an account like any other.
        assert!(check("capsule", "claude", "team", "default", "sid", home.path(), &accounts).is_ok());
    }

    /// Pins HOME, the config root and the state root at scratch dirs and
    /// returns the home. Caller holds the `EnvGuard` FIRST.
    fn pin_home(home: &Path, scratch: &Path) {
        std::env::set_var("HOME", home);
        std::env::set_var("USERPROFILE", home);
        std::env::set_var("XDG_CONFIG_HOME", scratch);
        std::env::set_var("XDG_STATE_HOME", scratch);
        std::env::set_var("LOCALAPPDATA", scratch);
    }

    fn seed_capsule_row(account: &str, handle: &str) -> (Workspaces, String, String) {
        let reg = Workspaces::new();
        let mut ws = Workspace::from_label(
            "reauth-row",
            std::path::PathBuf::from("/p/reauth-row"),
            true,
            "claude".into(),
            "row-agent".into(),
            String::new(),
        );
        ws.runtime = "capsule".to_string();
        ws.account = std::sync::Mutex::new(account.to_string());
        let id = ws.workspace_id.clone();
        let slug = ws.slug.clone();
        reg.insert(ws);
        reg.set_agent_handle(&id, handle);
        (reg, id, slug)
    }

    async fn reauth_out(reg: &Workspaces, id: &str, account: &str, resume: &str) -> (HandlerOutput, Option<ReauthRestart>) {
        handle_workspace_reauth(
            1,
            serde_json::json!({"workspace_id": id, "account": account, "resume": resume}),
            reg,
        )
        .await
        .expect("the handler itself never errors; it refuses")
    }

    async fn reauth(reg: &Workspaces, id: &str, account: &str, resume: &str) -> (serde_json::Value, Option<ReauthRestart>) {
        let (out, restart) = reauth_out(reg, id, account, resume).await;
        (out[0].0.payload.clone(), restart)
    }

    // The refusal payload carries the accounts this daemon can see, so no
    // caller re-implements discovery to explain one.
    #[tokio::test]
    async fn an_unknown_row_is_refused_and_the_refusal_names_the_accounts() {
        let _g = env_guarded();
        let home = home_with(true, &[("team", true)]);
        let scratch = tempfile::tempdir().unwrap();
        pin_home(home.path(), scratch.path());
        let reg = Workspaces::new();
        let (payload, restart) = reauth(&reg, "sot-ws-nope", "team", "sid").await;
        assert!(restart.is_none(), "a refusal hands back no restart");
        assert_eq!(payload["code"], "unknown_workspace");
        let names: Vec<String> = payload["accounts"]
            .as_array()
            .expect("accounts array")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["default".to_string(), "team".to_string()]);
    }

    // ORDERING 1 (ADR 0046 decision 6): the new account is on the row AND
    // in its toml before anything can spawn a replacement — the spawn reads
    // `Workspace::account` back off the registry, so this is the one order
    // that leaves a single truth. The row is mutated in exactly ONE field:
    // id, slug, root and the DECLARED handle all survive.
    #[tokio::test]
    async fn the_record_carries_the_new_account_before_the_replacement_is_spawned() {
        let _g = env_guarded();
        let home = home_with(true, &[("team", true)]);
        let scratch = tempfile::tempdir().unwrap();
        pin_home(home.path(), scratch.path());
        seed_claude_binary(home.path());
        seed_transcript(&claude_config_dir(home.path(), "team"), "sid-7");
        let (reg, id, slug) = seed_capsule_row("", "row-declared-handle");

        let (payload, restart) = reauth(&reg, &id, "team", "sid-7").await;
        assert!(payload.get("error").is_none(), "must be accepted: {payload:?}");
        let plan = restart.expect("an accept hands the restart back to the caller");

        let after = reg.resolve(Some(&id)).expect("the row is never replaced");
        assert_eq!(after.account(), "team");
        assert_eq!(after.workspace_id, id);
        assert_eq!(after.slug, slug);
        assert_eq!(after.agent_handle(), "row-declared-handle");
        assert_eq!(after.agent(), "claude");

        let toml = std::fs::read_to_string(crate::workspaces::toml_path_for(&slug))
            .expect("the row's toml is persisted, not just held in memory");
        assert!(toml.contains("account       = \"team\""), "{toml}");
        assert!(toml.contains("agent_handle  = \"row-declared-handle\""), "{toml}");

        // Nothing has spawned or been ended yet: that is `restart_blocking`'s
        // job, and it has not run.
        drop(plan);
    }

    // ORDERING 2: the accept is ANSWERABLE before anything is torn down.
    // The accept path never dials the row's lane — its state dir does not
    // even exist here — and the only thing that can end the leg needs the
    // `ReauthRestart` this call hands BACK. That the frame is physically
    // written before the restart is handed that plan is ORDERING 3 below.
    #[tokio::test]
    async fn the_accept_is_answered_before_the_leg_is_touched() {
        let _g = env_guarded();
        let home = home_with(true, &[("team", true)]);
        let scratch = tempfile::tempdir().unwrap();
        pin_home(home.path(), scratch.path());
        seed_claude_binary(home.path());
        seed_transcript(&claude_config_dir(home.path(), "team"), "sid-7");
        let (reg, id, _slug) = seed_capsule_row("", "row-declared-handle");

        let (payload, restart) = reauth(&reg, &id, "team", "sid-7").await;
        assert_eq!(payload["code"], ACCEPTED_CODE);
        assert_eq!(payload["account"], "team");
        assert_eq!(payload["workspace_id"], id);
        assert!(restart.is_some(), "the effect is deferred to the caller, after the write");

        let state_root = sot_log::state_dir::sot_state_dir().expect("pinned state root");
        let state_dir = crate::capsule_workspace::state_dir_for(&state_root, &id);
        assert!(
            !state_dir.exists(),
            "the accept path must not have dialed, created or ended anything: {state_dir:?}"
        );
        drop(restart);
    }

    /// The peer the ordering rule turns on, in its two states: one that
    /// accepts what it is handed, one that is already GONE (the dead or
    /// non-draining peer `write_frame_to` answers with an `Err`). The bytes
    /// are shared rather than owned so the restart closure can see what
    /// reached the wire BEFORE it ran.
    struct Peer {
        written: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        gone: bool,
    }

    impl tokio::io::AsyncWrite for Peer {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            if self.gone {
                return std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "the peer is gone",
                )));
            }
            self.written.lock().unwrap().extend_from_slice(buf);
            std::task::Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// The four effects, named so the ORDER they happened in is a value a
    /// test can compare rather than a shape a reader has to infer.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Effect {
        Status,
        EndRun,
        Spawn,
        Reset,
    }

    /// The four effects, recorded in the order `restart_blocking` performs
    /// them, with each answer scripted. Shared state behind `Arc<Mutex<..>>`
    /// like `Peer`'s bytes, for the same reason: the thing under test is a
    /// call that may be ABSENT, and an absence has no return value to
    /// assert on — only the record shows it.
    struct FakeSupervisor {
        record: std::sync::Arc<std::sync::Mutex<Vec<Effect>>>,
        /// The argv the replacement was actually handed, so one test can
        /// pin that it spends `--resume <id>` rather than `--continue`.
        spawned_argv: std::sync::Arc<std::sync::Mutex<Option<Vec<String>>>>,
        /// The two status answers, drained in order: the pre-retire
        /// identity read, then the post-spawn settle read. Both are phases
        /// `phase_rests` accepts, so the wait returns on its first poll and
        /// no test ever sleeps on the real 30 s deadline.
        status: std::sync::Mutex<std::collections::VecDeque<(u32, u64, sot_log::wire::SupervisorPhase)>>,
        end_run: Result<crate::capsule_workspace::EndRunOutcome, String>,
        spawn: Result<&'static str, String>,
        reset: Result<String, String>,
    }

    impl FakeSupervisor {
        /// The healthy revival: an authority at `(1111, 800)` before the
        /// retire, a different one at `(4242, 900)` after it, both resting
        /// where a resumed run rests, an end that verified and a reset that
        /// minted.
        fn healthy() -> Self {
            use sot_log::wire::SupervisorPhase as P;
            Self {
                record: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                spawned_argv: std::sync::Arc::new(std::sync::Mutex::new(None)),
                status: std::sync::Mutex::new(
                    [(1111, 800, P::EndedNoRespawn), (4242, 900, P::EndedNoRespawn)].into_iter().collect(),
                ),
                end_run: Ok(crate::capsule_workspace::EndRunOutcome::RecordVerified),
                spawn: Ok("starting"),
                reset: Ok("voyage-1".to_string()),
            }
        }

        /// Re-script the two reads: the identity read, then the settle read.
        fn status_reads(
            &self,
            identity: (u32, u64, sot_log::wire::SupervisorPhase),
            settled: (u32, u64, sot_log::wire::SupervisorPhase),
        ) {
            *self.status.lock().unwrap() = [identity, settled].into_iter().collect();
        }

        fn record(&self) -> Vec<Effect> {
            self.record.lock().unwrap().clone()
        }

        fn spawned_argv(&self) -> Vec<String> {
            self.spawned_argv.lock().unwrap().clone().expect("the replacement was spawned")
        }
    }

    impl RestartEffects for FakeSupervisor {
        fn query_status(&self, _state_dir: &Path) -> Result<sot_log::supervisor_client::StatusReport, String> {
            self.record.lock().unwrap().push(Effect::Status);
            let (pid, created, phase) = self
                .status
                .lock()
                .unwrap()
                .pop_front()
                .expect("the fake was asked for more status reads than it was scripted");
            Ok(sot_log::supervisor_client::StatusReport { pid, created, voyage: None, leg: None, phase })
        }
        fn end_run(
            &self,
            _state_dir: &Path,
            _reason: &str,
            _root_canonicalized: bool,
        ) -> std::io::Result<crate::capsule_workspace::EndRunOutcome> {
            self.record.lock().unwrap().push(Effect::EndRun);
            self.end_run.clone().map_err(std::io::Error::other)
        }
        fn spawn_replacement(&self, plan: &ReauthRestart) -> Result<&'static str, String> {
            self.record.lock().unwrap().push(Effect::Spawn);
            *self.spawned_argv.lock().unwrap() = Some(plan.argv.clone());
            self.spawn.clone()
        }
        fn reset(&self, _state_dir: &Path) -> Result<String, String> {
            self.record.lock().unwrap().push(Effect::Reset);
            self.reset.clone()
        }
    }

    // ORDERING 5: a revival is THREE effects and this is the one test that
    // can see all three happen, in order, at all — the defect it exists for
    // is a reset that is never called, and an absent call has no return
    // value for a pure function to observe. The single sequence assertion
    // below pins every ordering this path depends on: the identity is read
    // BEFORE the retire (so a leaked retire is detectable at all), the
    // spawn follows the end, and the mint follows a settle read that
    // follows the spawn.
    #[tokio::test]
    async fn a_revival_is_the_identity_then_end_run_then_spawn_then_the_settle_then_the_mint() {
        let _g = env_guarded();
        let home = home_with(true, &[("team", true)]);
        let scratch = tempfile::tempdir().unwrap();
        let (reg, id, _slug) = accept_fixture(home.path(), scratch.path());

        let (payload, restart) = reauth(&reg, &id, "team", "sid-7").await;
        assert_eq!(payload["code"], ACCEPTED_CODE);
        let fake = FakeSupervisor::healthy();
        restart_blocking(restart.expect("the fixture reaches the accept"), &fake);

        assert_eq!(
            fake.record(),
            vec![Effect::Status, Effect::EndRun, Effect::Spawn, Effect::Status, Effect::Reset]
        );
        assert_eq!(
            reg.resolve(Some(&id)).unwrap().account(),
            "team",
            "a revival that completed never rolls the record back"
        );
        // The one call that still holds the conversation id spends it: the
        // id is never persisted on the row, so a later attach's `--continue`
        // would select by recency instead.
        let argv = fake.spawned_argv();
        assert!(argv.iter().any(|a| a == "--resume"), "{argv:?}");
        assert!(argv.iter().any(|a| a == "sid-7"), "{argv:?}");
        assert!(!argv.iter().any(|a| a == "--continue"), "{argv:?}");
    }

    // An end that could not run leaves the OLD leg on the OLD login, so
    // nothing may be spawned and the record has to go back to saying so.
    #[tokio::test]
    async fn an_end_run_that_cannot_run_spawns_nothing_and_rolls_the_record_back() {
        let _g = env_guarded();
        let home = home_with(true, &[("team", true)]);
        let scratch = tempfile::tempdir().unwrap();
        let (reg, id, slug) = accept_fixture(home.path(), scratch.path());

        let (_payload, restart) = reauth(&reg, &id, "team", "sid-7").await;
        let mut fake = FakeSupervisor::healthy();
        fake.end_run = Err("the lane never answered".to_string());
        restart_blocking(restart.expect("the fixture reaches the accept"), &fake);

        assert_eq!(fake.record(), vec![Effect::Status, Effect::EndRun]);
        assert_eq!(
            reg.resolve(Some(&id)).unwrap().account(),
            "",
            "the record is back on the login the live leg still spends"
        );
        let toml = std::fs::read_to_string(crate::workspaces::toml_path_for(&slug)).unwrap();
        assert!(toml.contains("account       = \"\""), "{toml}");
    }

    // `end_run` can SUCCEED and still leave the run unended: `Starting` means
    // the authority had not reached the lifecycle where an end takes. Nothing
    // else pins that `restart_blocking` consults that judgement — the fake's
    // only scripted outcome is the healthy one, so without this the guard at
    // `run_ended` could be deleted with every other test still green. An
    // absent call is exactly what no pure test of `run_ended` can observe.
    #[tokio::test]
    async fn an_end_run_that_did_not_end_the_run_spawns_nothing_and_rolls_the_record_back() {
        let _g = env_guarded();
        let home = home_with(true, &[("team", true)]);
        let scratch = tempfile::tempdir().unwrap();
        let (reg, id, slug) = accept_fixture(home.path(), scratch.path());

        let (_payload, restart) = reauth(&reg, &id, "team", "sid-7").await;
        let mut fake = FakeSupervisor::healthy();
        fake.end_run = Ok(crate::capsule_workspace::EndRunOutcome::Starting);
        restart_blocking(restart.expect("the fixture reaches the accept"), &fake);

        assert_eq!(
            fake.record(),
            vec![Effect::Status, Effect::EndRun],
            "the run did not end, so nothing may be spawned or minted"
        );
        assert_eq!(
            reg.resolve(Some(&id)).unwrap().account(),
            "",
            "the record is back on the login the live leg still spends"
        );
        let toml = std::fs::read_to_string(crate::workspaces::toml_path_for(&slug)).unwrap();
        assert!(toml.contains("account       = \"\""), "{toml}");
    }

    // The asymmetry `restart_blocking`'s own doc states and nothing tested:
    // once the leg IS ended the record stands, whatever the spawn does,
    // because every later start path reads the account off the registry. A
    // rollback here would point the next attach at the login whose leg no
    // longer exists.
    #[tokio::test]
    async fn a_spawn_that_fails_leaves_the_record_on_the_new_account() {
        let _g = env_guarded();
        let home = home_with(true, &[("team", true)]);
        let scratch = tempfile::tempdir().unwrap();
        let (reg, id, slug) = accept_fixture(home.path(), scratch.path());

        let (_payload, restart) = reauth(&reg, &id, "team", "sid-7").await;
        let mut fake = FakeSupervisor::healthy();
        fake.spawn = Err("the capsule binary is missing".to_string());
        restart_blocking(restart.expect("the fixture reaches the accept"), &fake);

        assert_eq!(fake.record(), vec![Effect::Status, Effect::EndRun, Effect::Spawn]);
        assert_eq!(
            reg.resolve(Some(&id)).unwrap().account(),
            "team",
            "the leg is ended; the record must name the login the next start will spend"
        );
        let toml = std::fs::read_to_string(crate::workspaces::toml_path_for(&slug)).unwrap();
        assert!(toml.contains("account       = \"team\""), "{toml}");
    }

    // `ready` means a leg is ALREADY live — an `end_run` whose stop ended
    // the authority but not the leg — so the row has a leg on the OLD
    // login. The path must consult the judgement and STOP, which is a
    // missing `Reset` no pure test of `ready_to_mint` can observe.
    #[tokio::test]
    async fn a_replacement_that_rests_with_a_leg_live_is_never_minted_on() {
        use sot_log::wire::SupervisorPhase as P;
        let _g = env_guarded();
        let home = home_with(true, &[("team", true)]);
        let scratch = tempfile::tempdir().unwrap();
        let (reg, id, _slug) = accept_fixture(home.path(), scratch.path());

        let (_payload, restart) = reauth(&reg, &id, "team", "sid-7").await;
        let fake = FakeSupervisor::healthy();
        fake.status_reads((1111, 800, P::EndedNoRespawn), (4242, 900, P::Ready));
        restart_blocking(restart.expect("the fixture reaches the accept"), &fake);

        assert_eq!(
            fake.record(),
            vec![Effect::Status, Effect::EndRun, Effect::Spawn, Effect::Status],
            "a live leg is never minted on"
        );
        assert_eq!(reg.resolve(Some(&id)).unwrap().account(), "team");
    }

    // The leaked retire driven through the WHOLE path: the same process
    // answers before and after, resting at exactly the phase a healthy
    // replacement rests at. Only the identity tells them apart, and this is
    // what makes the identity read's POSITION load-bearing rather than
    // incidental — read after the retire, there would be nothing to compare.
    #[tokio::test]
    async fn a_leaked_retire_is_never_minted_on_end_to_end() {
        use sot_log::wire::SupervisorPhase as P;
        let _g = env_guarded();
        let home = home_with(true, &[("team", true)]);
        let scratch = tempfile::tempdir().unwrap();
        let (reg, id, _slug) = accept_fixture(home.path(), scratch.path());

        let (_payload, restart) = reauth(&reg, &id, "team", "sid-7").await;
        let fake = FakeSupervisor::healthy();
        fake.status_reads((1111, 800, P::EndedNoRespawn), (1111, 800, P::EndedNoRespawn));
        restart_blocking(restart.expect("the fixture reaches the accept"), &fake);

        assert_eq!(
            fake.record(),
            vec![Effect::Status, Effect::EndRun, Effect::Spawn, Effect::Status],
            "the process the switch was supposed to retire is never minted on"
        );
        assert_eq!(reg.resolve(Some(&id)).unwrap().account(), "team");
    }

    /// The accept path with a transcript the target can open and a row on
    /// the default login — the fixture both ordering tests start from.
    /// Returns the registry, the row's id and slug.
    fn accept_fixture(home: &Path, scratch: &Path) -> (Workspaces, String, String) {
        pin_home(home, scratch);
        seed_claude_binary(home);
        seed_transcript(&claude_config_dir(home, "team"), "sid-7");
        seed_capsule_row("", "row-declared-handle")
    }

    // ORDERING 3: the accept frame is PHYSICALLY WRITTEN before anything is
    // handed the plan that can end the leg — the dispatcher's whole
    // contract, pinned here rather than left to two adjacent statements in
    // `server.rs`.
    #[tokio::test]
    async fn the_accept_frame_is_on_the_wire_before_the_restart_is_handed_the_plan() {
        let _g = env_guarded();
        let home = home_with(true, &[("team", true)]);
        let scratch = tempfile::tempdir().unwrap();
        let (reg, id, slug) = accept_fixture(home.path(), scratch.path());

        let (out, restart) = reauth_out(&reg, &id, "team", "sid-7").await;
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut peer = Peer { written: written.clone(), gone: false };
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let (seen_then, written_then) = (seen.clone(), written.clone());
        write_accept_then(&mut peer, &out, restart, move |plan| {
            *seen_then.lock().unwrap() = Some(written_then.lock().unwrap().clone());
            // Never end a real leg from a unit test: dropping the plan
            // releases the row's guard and touches nothing.
            drop(plan);
        })
        .await
        .expect("a peer that takes the write leaves the restart to run");

        let seen = seen.lock().unwrap().clone().expect("the restart was handed the plan");
        assert!(
            String::from_utf8_lossy(&seen).contains(ACCEPTED_CODE),
            "the accept must already be on the wire when the restart runs: {:?}",
            String::from_utf8_lossy(&seen)
        );
        // Nothing rolled back: this is the accept that was read.
        assert_eq!(reg.resolve(Some(&id)).unwrap().account(), "team");
        let toml = std::fs::read_to_string(crate::workspaces::toml_path_for(&slug)).unwrap();
        assert!(toml.contains("account       = \"team\""), "{toml}");
    }

    // ORDERING 4: the THIRD outcome — the accept never reached its reader.
    // Nothing was torn down, so the live leg still spends the OLD login and
    // the record has to go back to saying so; the error still propagates,
    // because that connection is over either way.
    #[tokio::test]
    async fn an_accept_that_cannot_be_written_rolls_the_record_back() {
        let _g = env_guarded();
        let home = home_with(true, &[("team", true)]);
        let scratch = tempfile::tempdir().unwrap();
        let (reg, id, slug) = accept_fixture(home.path(), scratch.path());

        let (out, restart) = reauth_out(&reg, &id, "team", "sid-7").await;
        assert!(restart.is_some(), "the fixture must reach the accept");
        let mut peer = Peer { written: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())), gone: true };
        let err = write_accept_then(&mut peer, &out, restart, |_plan| {
            panic!("the restart must never run when the accept did not reach the caller")
        })
        .await
        .expect_err("a write that fails still ends the connection");
        assert!(format!("{err:#}").contains("the peer is gone"), "{err:#}");

        let after = reg.resolve(Some(&id)).expect("the row itself survives");
        assert_eq!(after.account(), "", "the record is back on the login the live leg spends");
        let toml = std::fs::read_to_string(crate::workspaces::toml_path_for(&slug)).unwrap();
        assert!(toml.contains("account       = \"\""), "{toml}");
        assert!(toml.contains("agent_handle  = \"row-declared-handle\""), "{toml}");
    }

    // NOTE 7: the record keeps the normalized `""`, but the REPLY names the
    // account, or `sot-fe` prints `account=` and the human reads a field
    // that failed to fill.
    #[tokio::test]
    async fn switching_to_the_default_account_answers_with_its_name_not_an_empty_string() {
        let _g = env_guarded();
        let home = home_with(true, &[("team", true)]);
        let scratch = tempfile::tempdir().unwrap();
        pin_home(home.path(), scratch.path());
        seed_claude_binary(home.path());
        seed_transcript(&claude_config_dir(home.path(), ""), "sid-7");
        let (reg, id, _slug) = seed_capsule_row("team", "row-declared-handle");

        let (payload, restart) = reauth(&reg, &id, "default", "sid-7").await;
        assert_eq!(payload["code"], ACCEPTED_CODE);
        assert_eq!(payload["account"], "default", "the reply names the account: {payload:?}");
        assert_eq!(
            reg.resolve(Some(&id)).unwrap().account(),
            "",
            "the record still holds the normalized default"
        );
        drop(restart);
    }

    // NOTE 8: the accounts ride on refusals decided AFTER the guard too,
    // not just on the pre-guard ones — `persist_failed` is the one such
    // refusal a test can provoke on demand (a config root that cannot hold
    // the row's toml), and it is also the only path that puts the account
    // back without a `ReauthRestart` to do it.
    #[tokio::test]
    async fn a_row_whose_toml_cannot_be_written_is_refused_with_the_accounts_and_the_account_put_back() {
        let _g = env_guarded();
        let home = home_with(true, &[("team", true)]);
        let scratch = tempfile::tempdir().unwrap();
        pin_home(home.path(), scratch.path());
        seed_claude_binary(home.path());
        seed_transcript(&claude_config_dir(home.path(), "team"), "sid-7");
        // A config root that is a FILE: nothing can create the row's toml
        // under it, so `save` fails where every other step has succeeded.
        let blocked = scratch.path().join("not-a-directory");
        std::fs::write(&blocked, b"").unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &blocked);
        std::env::set_var("LOCALAPPDATA", &blocked);
        let (reg, id, _slug) = seed_capsule_row("", "row-declared-handle");

        let (payload, restart) = reauth(&reg, &id, "team", "sid-7").await;
        assert!(restart.is_none(), "a refusal hands back no restart");
        assert_eq!(payload["code"], "persist_failed");
        let names: Vec<String> = payload["accounts"]
            .as_array()
            .expect("a post-guard refusal carries the accounts too")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["default".to_string(), "team".to_string()]);
        assert_eq!(
            reg.resolve(Some(&id)).unwrap().account(),
            "",
            "an unpersisted switch leaves the record on the account the leg spends"
        );
    }

    /// The defect this whole change exists for, and the one the review of
    /// its first version found. A spawn alone was reported as a running
    /// leg; the first fix then minted on ANY phase, merely warning when it
    /// was unexpected. `Reset` is admissible only from `ended_no_respawn`,
    /// so every other phase is a mint that would be refused -- and `ready`
    /// is worse than refused, because it means a leg is already live on the
    /// login the switch moved away from.
    #[test]
    fn only_the_resting_phase_of_a_resumed_run_licenses_a_mint() {
        use sot_log::wire::SupervisorPhase as P;
        let fresh = (4242u32, 900u64);
        let retired = Some((1111u32, 800u64));
        assert!(super::ready_to_mint(P::EndedNoRespawn, retired, fresh).is_ok());
        for refused in [P::Ready, P::Terminal, P::Starting, P::Ending] {
            assert!(
                super::ready_to_mint(refused, retired, fresh).is_err(),
                "{refused:?} must never be minted on"
            );
        }
    }

    /// `ready` is named separately because its failure is not "nothing came
    /// up" but "something did, on the wrong account" -- the message has to
    /// say so, or the next reader chases a missing leg that is running.
    #[test]
    fn a_live_leg_is_reported_as_the_old_login_not_as_an_absence() {
        use sot_log::wire::SupervisorPhase as P;
        let detail = super::ready_to_mint(P::Ready, Some((1111, 800)), (4242, 900))
            .expect_err("a live leg is not a licence to mint");
        assert!(detail.contains("already live"), "got {detail:?}");
        assert!(detail.contains("moved away from"), "got {detail:?}");
    }

    /// `end_run`'s stop is best effort. When it leaks, the OLD supervisor
    /// is still resident, this call's own spawn exits at the authority
    /// fence, and the status read comes back from that old process resting
    /// at exactly the phase a healthy replacement rests at. Only its
    /// identity tells the two apart, and minting on it would spawn the leg
    /// from its own cached argv and account.
    #[test]
    fn a_leaked_retire_is_never_minted_on_however_it_rests() {
        use sot_log::wire::SupervisorPhase as P;
        let same = (1111u32, 800u64);
        let detail = super::ready_to_mint(P::EndedNoRespawn, Some(same), same)
            .expect_err("the process the switch retired is not a replacement");
        assert!(detail.contains("retire leaked"), "the failure must name the cause, got {detail:?}");
        assert!(detail.contains("1111"), "the failure must name the process, got {detail:?}");
    }

    /// A pid that merely REPEATS is not the same authority: the supervisor
    /// reports its own creation stamp alongside, and the pair is what is
    /// compared, so a recycled pid on a genuinely new process still mints.
    #[test]
    fn a_recycled_pid_on_a_new_authority_still_mints() {
        use sot_log::wire::SupervisorPhase as P;
        assert!(super::ready_to_mint(P::EndedNoRespawn, Some((1111, 800)), (1111, 901)).is_ok());
    }

    /// Nothing answering before the retire is the ordinary case for a row
    /// whose authority had already gone; it is not evidence of a leak.
    #[test]
    fn an_unknown_predecessor_does_not_block_the_mint() {
        use sot_log::wire::SupervisorPhase as P;
        assert!(super::ready_to_mint(P::EndedNoRespawn, None, (4242, 900)).is_ok());
    }

    /// The wait's partition is the supervisor's own: these three are where
    /// an authority stays until somebody acts, and the rest are phases it
    /// is still moving through -- which is the whole reason the previous
    /// version's two-second settle was not enough to believe.
    #[test]
    fn only_settled_phases_end_the_wait() {
        use sot_log::wire::SupervisorPhase as P;
        for resting in [P::Ready, P::EndedNoRespawn, P::Terminal] {
            assert!(super::phase_rests(resting), "{resting:?} rests");
        }
        for moving in [P::Starting, P::Ending] {
            assert!(!super::phase_rests(moving), "{moving:?} is still moving");
        }
    }

    // `end_run`'s outcomes partition into "the run is over" (spawn the
    // replacement) and "something still holds this row" (leave it alone) —
    // the same partition `capsule_destroy_outcome_of` makes for removal.
    #[test]
    fn only_an_ended_run_licenses_a_replacement_spawn() {
        use crate::capsule_workspace::EndRunOutcome as O;
        for over in [O::RecordVerified, O::RecordClosed, O::AlreadyEnded, O::Terminal, O::Unheld, O::Orphaned] {
            assert!(run_ended(&over).is_ok(), "{over:?}");
        }
        assert!(run_ended(&O::Starting).is_err());
        match run_ended(&O::NotEnded("a leg is running".to_string())) {
            Err(detail) => assert_eq!(detail, "a leg is running"),
            Ok(()) => panic!("a run that did not end must never license a spawn"),
        }
    }
}
