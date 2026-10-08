// rows/reauth/mod.rs — `workspace.reauth` (ADR 0046 decision 6): move a LIVE
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
// time (`rows::run::start::spawn_and_watch`) — this call's own
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
use tokio::io::AsyncWrite;

use crate::agents::accounts::DiscoveredAccount;
use crate::server::reply::HandlerOutput;
use crate::rows::Workspaces;

/// The reply code an accepted reauth answers with, before anything is
/// torn down. The caller (`server/dispatch.rs`) writes this frame and THEN performs
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
    row: std::sync::Arc<crate::rows::Workspace>,
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
        if let Err(e) = crate::rows::store::save(&row) {
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
/// so the order is pinned by a test instead of by adjacency; `server/dispatch.rs`
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
        if let Err(e) = crate::server::reply::write_frame_to(tx, frame, blob.as_deref()).await {
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

/// How far into a transcript its start directory is looked for. The first
/// line that records a `cwd` ended within 104 KB of the file's start in every
/// one of 11,628 transcripts Claude Code 2.1.278 to 2.1.288 wrote; a
/// transcript that records none in its first MiB is not provably any row's.
const HEAD_SCAN_BYTES: u64 = 1 << 20;

/// The directory transcript `path` was started in: the `cwd` of its first
/// line that records one. Only the first: later lines record wherever the
/// session's shell has `cd`'d since, and a `--resume` run in another
/// directory appends lines that record THAT directory to this same file.
fn started_in(path: &Path) -> Option<PathBuf> {
    use std::io::{BufRead, Read};
    #[derive(serde::Deserialize)]
    struct Line {
        cwd: Option<String>,
    }
    let file = std::fs::File::open(path).ok()?;
    std::io::BufReader::new(file.take(HEAD_SCAN_BYTES))
        .split(b'\n')
        .map_while(std::result::Result::ok)
        .find_map(|line| serde_json::from_slice::<Line>(&line).ok()?.cwd)
        .map(PathBuf::from)
}

/// Whether `id` has the shape of a Claude Code session id: a lowercase UUID,
/// 8-4-4-4-12 hex digits. Only that shape is joined into a path or handed to
/// `claude --resume`: it holds no separator, so it names one transcript under
/// `projects`, and no leading `-`, so claude never reads it as a flag.
fn is_session_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

/// The refusal, if any, for resuming transcript `resume` as `account` in the
/// row rooted at `root`. In order: `resume` must be a session id
/// ([`is_session_id`]); the account must be able to open that
/// transcript, found by globbing `projects`' children rather than by
/// rebuilding claude's own cwd-to-directory mangling (a rule this daemon
/// copied would be a rule it could get wrong); and the transcript must have
/// been started in THIS row's root. The last is the row's claim on its
/// conversation: Claude Code's `--resume <id>` opens a transcript from any
/// project folder and appends to it, so an id from another row would restart
/// this row on that row's conversation, two legs writing one file.
/// Directories compare by kernel identity, so spelling, case, separators and
/// symlinks do not matter; the recorded directory must be absolute.
fn transcript_refusal(
    home: &Path,
    account: &str,
    resume: &str,
    root: &Path,
) -> Option<(&'static str, String)> {
    if !is_session_id(resume) {
        return Some((
            "resume_unreachable",
            format!("{resume:?} is not a session id: claude names a conversation by a lowercase UUID"),
        ));
    }
    let name = format!("{resume}.jsonl");
    let projects = crate::agents::accounts::claude_config_dir(home, account).join("projects");
    let transcript = std::fs::read_dir(projects).ok().and_then(|entries| {
        entries
            .flatten()
            .map(|e| e.path().join(&name))
            .find(|p| p.is_file())
    });
    // The refusal that protects the kill, and the reason it lives HERE:
    // `claude --resume <id>` on an id the target cannot see exits at once,
    // the supervisor flaps the row to `Terminal`, and the conversation is
    // reachable again only by reauthing back — so the only actor that can
    // read both accounts' folders proves reachability BEFORE the accept,
    // rather than asking the leg to check its own grave.
    let Some(transcript) = transcript else {
        return Some((
            "resume_unreachable",
            format!(
                "account {:?} cannot see transcript {resume:?}: no projects/*/{resume}.jsonl under its config dir — either that is not this conversation's id, or that account folder has its own REAL `projects` instead of the shared symlink, in which case the resume would land in a fresh, empty conversation",
                discovery_name(account)
            ),
        ));
    };
    let root_identity = match sot_log::host::dir_identity(root) {
        Ok(identity) => identity,
        Err(e) => {
            let error = format!("this row's root {root:?} cannot be opened: {e}");
            return Some(("resume_not_this_row", error));
        }
    };
    let Some(started) = started_in(&transcript) else {
        return Some((
            "resume_not_this_row",
            format!(
                "transcript {resume:?} records no working directory in its first {HEAD_SCAN_BYTES} bytes: a reauth resumes only a conversation started in this row's root {root:?}"
            ),
        ));
    };
    // Absolute only: a relative directory would be read against this daemon's
    // own working directory, which says nothing about where the session ran.
    let same = started.is_absolute()
        && sot_log::host::dir_identity(&started).is_ok_and(|identity| identity == root_identity);
    if same {
        return None;
    }
    Some((
        "resume_not_this_row",
        format!(
            "transcript {resume:?} was started in {started:?}, not in this row's root {root:?}: a reauth resumes only a conversation started in the row's own root"
        ),
    ))
}

/// Every refusal this op owns, decided BEFORE anything is touched, over the
/// row's own facts (its runtime, agent, account and root) plus the home the
/// accounts live in.
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
    root: &Path,
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
    if let Err(e) = crate::agents::accounts::account_env(agent_kind, want, home) {
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
    // The refusals that guard the replacement's `--resume`: an id the target
    // account cannot open, and a conversation that is not this row's own.
    if let Some((code, error)) = transcript_refusal(home, want, resume, root) {
        return refuse(code, error);
    }
    Ok(())
}

/// The ONE refusal frame this op builds — one constructor, so the
/// discovered accounts ride on every refusal as both the module doc above
/// and `ops/workspace.rs`'s wire doc promise. The only refusals that answer with an
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

    let Some(home) = crate::agents::accounts::account_home() else {
        let error = "could not resolve this daemon's own home, so no account can be resolved against it".to_string();
        return Ok((refused(req_id, Refusal { code: "no_home", error, accounts: Vec::new() }), None));
    };
    let accounts = crate::agents::accounts::discover_accounts(&home);
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
        &ws.project_root,
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
    let Some(state_root) = sot_log::host::state_dir::sot_state_dir() else {
        return refuse(
            "no_state_root",
            format!(
                "could not resolve this machine's state root ({} unset)",
                crate::rows::spawn::state_root::STATE_ROOT_HINT
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
    let argv = match crate::agents::argv::claude_resume_argv(
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
    if let Err(e) = crate::rows::store::save(&row) {
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

/// Answers `workspace.reauth`: records the switch, writes the accept frame, then hands the restart to a detached task.
pub(crate) async fn answer_workspace_reauth<W>(tx: &mut W, frame: Frame, workspaces: &Workspaces) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let (out, restart) =
        crate::rows::reauth::handle_workspace_reauth(frame.id, frame.payload, &workspaces).await?;
    // Both halves of the ordering live in `write_accept_then`,
    // which a test pins: the frame goes out first, and a write
    // that fails rolls the record back before the `?` here ends
    // the connection.
    crate::rows::reauth::write_accept_then(tx, &out, restart, |plan| {
        // Detached: this connection is about to lose its peer,
        // and the restart holds the row's guard for its whole
        // duration wherever it runs.
        tokio::spawn(async move {
            if let Err(e) =
                tokio::task::spawn_blocking(move || {
                    crate::rows::reauth::restart_blocking(plan, &crate::rows::reauth::LiveSupervisor)
                })
                .await
            {
                tracing::warn!(error = %e, "workspace.reauth: the restart task panicked");
            }
        });
    })
    .await?;
    return Ok(());
}

mod restart;
use restart::{restart_blocking, LiveSupervisor};

#[cfg(test)]
mod support_tests;
#[cfg(test)]
mod check_tests;
#[cfg(test)]
mod accept_tests;
#[cfg(test)]
mod own_transcript_tests;
