//! Ending a row's run: destroy_capsule_workspace under the row's guard, the outcome it reports, and
//! the durable removal of the row's tomls.

use crate::rows::Workspaces;
use serde_json::json;

/// ADR 0042 slice L1a (Codex review finding 3): whether a capsule
/// workspace's row (and its persisted toml) may be safely removed by
/// `workspace.destroy`.
pub(crate) enum CapsuleDestroyOutcome {
    /// The run was CONFIRMED ended (`RecordVerified`/`RecordClosed`/
    /// `AlreadyEnded` — see `rows::run::end_run::EndRunOutcome`) — the row
    /// may be removed; the state directory never is. Human-readable
    /// (never the raw, Windows-only `EndRunOutcome` type) so this enum
    /// stays portable and unit-testable.
    Removable(String),
    /// Not confirmed (unreachable/starting/failed/refused/unknown) — the
    /// row and toml MUST be kept: never orphan a live run, never claim
    /// "ended" for one that wasn't.
    Kept { detail: String },
    /// Another path removed the row before this end reached its run: not
    /// ours to end, and nothing of it is kept.
    AlreadyRemoved,
}

/// What `workspace.destroy` answers for [`CapsuleDestroyOutcome::AlreadyRemoved`].
pub(crate) const ALREADY_REMOVED: &str = "workspace was removed before its capsule run could be ended";

/// Maps a `rows::run::end_run::EndRunOutcome` to whether `workspace.destroy`
/// may remove the row. Pure/portable so it's unit-testable without a real
/// Windows lane.
pub(crate) fn capsule_destroy_outcome_of(o: crate::rows::run::end_run::EndRunOutcome) -> CapsuleDestroyOutcome {
    use crate::rows::run::end_run::EndRunOutcome as O;
    match o {
        O::RecordVerified => CapsuleDestroyOutcome::Removable("run ended and verified".to_string()),
        O::RecordClosed => CapsuleDestroyOutcome::Removable(
            "run ended (record closed, not yet verified)".to_string(),
        ),
        O::AlreadyEnded => CapsuleDestroyOutcome::Removable("run had already ended".to_string()),
        // A `Terminal` authority has no leg left to orphan -- `end_run`
        // already sent it `stop` and waited for confirmed exit (see
        // `EndRunOutcome::Terminal`'s own doc). Without this arm a
        // capsule row whose agent argv can never launch was UNENDABLE:
        // `end_run` used to report this as `NotEnded` (kept) forever.
        O::Terminal => CapsuleDestroyOutcome::Removable(
            "the run was terminal; the supervisor was stopped".to_string(),
        ),
        O::NotEnded(detail) => CapsuleDestroyOutcome::Kept { detail },
        // The lane was unreachable but the supervisor lock itself was
        // free to take -- nobody holds this row (see `EndRunOutcome::
        // Unheld`'s own doc). A run with no holder is not running.
        O::Unheld => {
            CapsuleDestroyOutcome::Removable("no supervisor held the row".to_string())
        }
        // No state dir, no reachable lane at any point this daemon could
        // check (see `EndRunOutcome::Orphaned`'s own doc) -- its own
        // code, distinct from `Unheld`'s "no supervisor held the row":
        // this row never ran under this daemon's state root at all.
        O::Orphaned => CapsuleDestroyOutcome::Removable("orphan_removed".to_string()),
    }
}

/// `reason` is the immutable end-run reason recorded on the wire —
/// parameterized so each caller (a real delete vs. the default row's
/// own kept-not-deleted branch) supplies its own honest text.
/// `agent_kind`/`agent_name`/`slug`/`project_root` are `ws`'s own fields,
/// passed through (rather than re-resolved) so this can call
/// `rows::run::activation::resume_locked` — the guard-free inner
/// `resume_if_absent` itself uses — under the SAME row guard `end_run`
/// then runs under (ADR 0043 decision 33's own resume-before-end
/// destroy caller): a row whose supervisor died leaves a live LEG behind
/// with no authority to end it; resuming re-establishes the authority so
/// `end_run` has a real lane to ask, rather than falling straight to its
/// own fence/leg proof. A resume failure is logged and never fails the
/// call — `end_run`'s own arms decide the outcome regardless.
/// `resume_first` false skips that resume: a window's close ends rows
/// without resuming any (`shutdown::end_rows`).
///
/// Every mutation runs under the row's own guard, from the first probe
/// through the outcome this returns — a terminal `Phase` mark alone
/// proves the authority exited, never that a leg is also gone.
///
/// Returns the row's own guard alongside the outcome, still HELD
/// (`None` only when no real lane call was ever attempted) — Codex
/// review round 2 on the L1a PR: an owned watchdog can check membership,
/// enter its own backoff, and restart the very row a caller is mid-way
/// through removing, unless the SAME guard covers both the end/stop
/// call here AND whatever the caller does with a confirmed outcome
/// (row removal, or the default row's own reset) afterward. The caller
/// holds it through that follow-up, then drops it.
#[allow(clippy::too_many_lines, reason = "ends a capsule workspace's run under the caller's guard and judges the outcome; predates the 100-line limit")]
pub(crate) async fn destroy_capsule_workspace(
    workspace_id: &str,
    reason: &str,
    agent_kind: &str,
    agent_name: &str,
    slug: &str,
    project_root: &std::path::Path,
    workspaces: &Workspaces,
    resume_first: bool,
) -> (CapsuleDestroyOutcome, Option<tokio::sync::OwnedMutexGuard<()>>) {
    {
        let Some(state_root) = sot_log::host::state_dir::sot_state_dir() else {
            return (
                CapsuleDestroyOutcome::Kept {
                    detail: format!(
                        "could not resolve this machine's state root ({} unset)",
                        crate::rows::spawn::state_root::STATE_ROOT_HINT
                    ),
                },
                None,
            );
        };
        // SAFETY (Fable review): `sot_log::host::state_dir::state_dir_hash`
        // canonicalizes `state_dir` itself, falling back to the RAW path
        // only when that fails -- which is exactly the missing-directory
        // case the orphan proof exists for. If `state_root` is reached
        // through a symlink, a live supervisor (spawned while its own
        // `state_dir` existed) canonicalized the FULL real path and
        // bound its lane there; dialing the raw, non-canonical path
        // here would miss it -- ENOENT for the wrong reason, not because
        // nothing is running. Canonicalizing the ROOT (which, unlike the
        // row's own `state_dir`, is expected to exist) before joining
        // the workspace id closes this: the joined path then matches
        // what a live supervisor's own canonicalize would have produced,
        // whether or not this row's own `state_dir` still exists. When
        // the root itself cannot be canonicalized, `root_canonicalized`
        // is `false` and `end_run` must never attempt the orphan proof
        // on this call -- it keeps today's unconditional refusal instead
        // of trusting a hash built from an unresolved path.
        let (state_root, root_canonicalized) = match state_root.canonicalize() {
            Ok(canonical) => (canonical, true),
            Err(e) => {
                tracing::debug!(
                    state_root = ?state_root, error = %e,
                    "workspace.destroy: state root did not canonicalize; the orphan proof is refused this call"
                );
                (state_root, false)
            }
        };
        let state_dir = crate::rows::spawn::state_root::state_dir_for(&state_root, workspace_id);
        let reason = reason.to_string();
        let workspace_id = workspace_id.to_string();
        let agent_kind = agent_kind.to_string();
        let agent_name = agent_name.to_string();
        let slug = slug.to_string();
        let project_root = project_root.to_path_buf();
        let workspaces_for_guard = workspaces.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            // ADR 0043 decision 33: this row's own guard, taken OWNED so
            // it survives this closure's return and stays held by the
            // caller through the row's actual removal/reset — see this
            // function's own doc. `None` (Codex review, 2026-09-11:
            // `capsule_guard` itself now refuses to mint one for a row
            // that is not currently registered) means a concurrent
            // remover already won this race — nothing left here to end.
            let Some(guard) = workspaces_for_guard.capsule_guard(&workspace_id) else {
                return (
                    Err(std::io::Error::new(std::io::ErrorKind::NotFound, "unknown workspace")),
                    None,
                );
            };
            let held = guard.blocking_lock_owned();
            // Without the resume, its membership recheck still runs, so the
            // "row already gone" arm below holds for an end with no resume.
            let resumed = if resume_first {
                crate::rows::run::activation::resume_locked(
                    &state_root,
                    &workspace_id,
                    &agent_kind,
                    &agent_name,
                    &slug,
                    &project_root,
                    workspaces_for_guard.clone(),
                )
            } else {
                workspaces_for_guard
                    .resolve(Some(&workspace_id))
                    .map(|_| "not resumed")
                    .ok_or_else(|| "unknown workspace".to_string())
            };
            match resumed {
                // BLOCKER (Codex review, 2026-09-11): a pending resume can
                // outlive deletion. `resume_locked` returns this exact
                // sentinel phase ONLY when it just spawned a fresh
                // authority (its own probe first read `UNREACHABLE_PHASE`)
                // and `start_supervisor`'s settle deadline elapsed with
                // the lane STILL unobserved — an unresolved spawn is still
                // in flight under THIS SAME guard. Falling through to
                // `end_run` regardless (the old behaviour) would race it:
                // the freshly spawned process has not yet taken the fence
                // or re-executed the leg, so `end_run`'s own absence proof
                // could read both as acquirable and report the row
                // Removable an instant before that supervisor starts.
                // There is no cheap way to cancel or reap it from here —
                // the spawned `Child` is already owned by its own
                // watchdog, installed inside `resume_locked`'s own call,
                // never handed back to this caller — so a timeout stays
                // non-removable: `Kept` with an honest code
                // (`supervisor_starting`), never a guess.
                Ok(phase) if phase == crate::rows::run::probe::UNREACHABLE_PHASE => {
                    return (
                        Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "supervisor_starting")),
                        Some(held),
                    );
                }
                Ok(_) => {}
                // SHOULD-FIX (Codex review, 2026-09-11): a destroy that
                // waited behind another remover's SAME guard must not
                // continue into `end_run` once THIS recheck (run only
                // after the guard was actually acquired) finds the row
                // already gone — the old state dir's fence and leg really
                // are free once nothing owns it any more, so `end_run`'s
                // own proof would still succeed and report `Removable`,
                // and the caller would then delete a SLUG-keyed toml that
                // may since belong to a REPLACEMENT registration under
                // the same slug. `held` is dropped (not carried) so this
                // lands on the SAME "row already gone" `NotFound` arm
                // below the top-of-function race already uses.
                Err(e) if e == "unknown workspace" => {
                    return (Err(std::io::Error::new(std::io::ErrorKind::NotFound, e)), None);
                }
                Err(e) => {
                    tracing::warn!(
                        workspace_id = %workspace_id, error = %e,
                        "workspace.destroy: resume before end_run failed; end_run's own arms decide"
                    );
                }
            }
            let result = crate::rows::run::end_run::end_run(&state_dir, &reason, root_canonicalized);
            (result, Some(held))
        })
        .await;
        match outcome {
            Ok((Ok(o), held)) => (capsule_destroy_outcome_of(o), held),
            // `end_run`'s own `state_dir_missing` (ADR 0043 decision 33's
            // destroy proof: a missing state dir proves nothing and is
            // reported, never recreated) gets its own typed code rather
            // than folding into the generic "lane unreachable" detail —
            // `capsule_end_not_reached_payload` reads it back off this
            // exact sentinel string. A `None` guard here is the "row
            // already gone" race above, reusing the SAME NotFound kind —
            // never mistaken for a missing state dir.
            Ok((Err(e), held)) if held.is_none() && e.kind() == std::io::ErrorKind::NotFound => {
                (CapsuleDestroyOutcome::AlreadyRemoved, None)
            }
            Ok((Err(e), held)) if e.kind() == std::io::ErrorKind::NotFound => {
                (CapsuleDestroyOutcome::Kept { detail: "state_dir_missing".to_string() }, held)
            }
            // The pending-resume sentinel above — a timeout stays
            // non-removable with its own honest code, never folded into
            // the generic "supervisor lane unreachable" catch-all below.
            Ok((Err(e), held)) if e.kind() == std::io::ErrorKind::WouldBlock => (
                CapsuleDestroyOutcome::Kept { detail: "supervisor_starting".to_string() },
                held,
            ),
            Ok((Err(e), held)) => (
                CapsuleDestroyOutcome::Kept {
                    detail: format!("supervisor lane unreachable: {e}"),
                },
                held,
            ),
            Err(join_err) => (
                CapsuleDestroyOutcome::Kept {
                    detail: format!("end_run task panicked: {join_err}"),
                },
                None,
            ),
        }
    }
}

/// The typed error `workspace.destroy` returns for a `Kept` outcome —
/// shared by the non-default path and the default row's own branch.
/// `"state_dir_missing"` and `"supervisor_starting"` are
/// `destroy_capsule_workspace`'s own sentinel details (ADR 0043 decision
/// 33) — the two `Kept` reasons with a code more specific than the
/// generic catch-all, so a caller can tell "nothing durable was ever
/// established here" and "a resume is still in flight, retry" apart from
/// every other kept reason without parsing prose.
pub(crate) fn capsule_end_not_reached_payload(detail: &str) -> serde_json::Value {
    let code = match detail {
        "state_dir_missing" => "state_dir_missing",
        "supervisor_starting" => "supervisor_starting",
        _ => "capsule_end_not_reached",
    };
    json!({
        "error": format!("capsule workspace could not be safely deleted: {detail}"),
        "code": code,
    })
}

/// The default row's own `workspace.destroy` response, built from an
/// already-computed outcome (pure/portable, unit-testable without a real
/// lane). Returns the payload and whether to broadcast `run_ended` —
/// `true` only for a CONFIRMED end; `Kept` gets the typed error instead.
pub(crate) fn default_row_end_response(
    workspace_id: &str,
    slug: &str,
    label: &str,
    outcome: CapsuleDestroyOutcome,
) -> (serde_json::Value, bool) {
    match outcome {
        CapsuleDestroyOutcome::Removable(detail) => {
            let res = sot_protocol::WorkspaceDestroyRes {
                workspace_id: workspace_id.to_string(),
                slug: slug.to_string(),
                label: label.to_string(),
                tmux_killed: false,
                toml_removed: false,
                kept: Some(format!("ended run of '{label}' ({detail})")),
            };
            (
                serde_json::to_value(res).expect("WorkspaceDestroyRes always serializes"),
                true,
            )
        }
        CapsuleDestroyOutcome::Kept { detail } => (capsule_end_not_reached_payload(&detail), false),
        CapsuleDestroyOutcome::AlreadyRemoved => (capsule_end_not_reached_payload(ALREADY_REMOVED), false),
    }
}

/// Remove a row's tomls from disk so neither registration path brings the
/// workspace back on next daemon startup: `scan_disk` reads the modern
/// workspaces/ toml, and the ADR-0013 migration reads the legacy
/// sessions/ toml. A missing file is success; `false` means a remove or
/// its directory sync failed (logged).
pub(crate) fn remove_row_files(slug: &str) -> bool {
    remove_registration(&[
        crate::rows::store::toml_path_for(slug),
        crate::rows::store::legacy_toml_path_for(slug),
    ])
}

/// Each registration file removed and its directory synced, so the delete
/// survives a power loss: a registration that came back under a record no
/// longer `closing` would resume an ended row (ruling e).
fn remove_registration(paths: &[std::path::PathBuf]) -> bool {
    let mut toml_removed = true;
    for toml_path in paths {
        if let Err(e) = crate::durable::remove(toml_path) {
            tracing::warn!(error = %e, path = ?toml_path, "workspace toml remove failed");
            toml_removed = false;
        }
    }
    toml_removed
}

#[cfg(test)]
mod destroy_outcome_tests {
    use super::*;

    // An authority found ALREADY resting in `EndedNoRespawn` is
    // `AlreadyEnded`, not a fabricated `RecordVerified` -- still
    // `Removable` (safe to report "ended").
    #[test]
    fn already_ended_outcome_is_removable_and_distinct_from_record_verified() {
        let outcome =
            capsule_destroy_outcome_of(crate::rows::run::end_run::EndRunOutcome::AlreadyEnded);
        match outcome {
            CapsuleDestroyOutcome::AlreadyRemoved => unreachable!("never an end_run mapping"),
            CapsuleDestroyOutcome::Removable(detail) => {
                assert!(
                    !detail.contains("verified"),
                    "must not claim verification it never observed: {detail}"
                );
            }
            CapsuleDestroyOutcome::Kept { detail } => {
                panic!("AlreadyEnded is a confirmed end — must not be Kept: {detail}");
            }
        }
    }

    // Rounds out coverage of `capsule_destroy_outcome_of`'s remaining
    // variants: a real end_run's own two confirmed outcomes both map to
    // `Removable`, and `NotEnded` (failed/refused/outcome-unknown) maps
    // to `Kept`.
    #[test]
    fn record_verified_and_closed_are_removable_not_ended_is_kept() {
        use crate::rows::run::end_run::EndRunOutcome as O;
        for outcome in [O::RecordVerified, O::RecordClosed] {
            assert!(
                matches!(
                    capsule_destroy_outcome_of(outcome.clone()),
                    CapsuleDestroyOutcome::Removable(_)
                ),
                "{outcome:?} must map to Removable"
            );
        }
        match capsule_destroy_outcome_of(O::NotEnded("end_run failed: boom".to_string())) {
            CapsuleDestroyOutcome::AlreadyRemoved => unreachable!("never an end_run mapping"),
            CapsuleDestroyOutcome::Kept { detail } => assert_eq!(detail, "end_run failed: boom"),
            CapsuleDestroyOutcome::Removable(detail) => {
                panic!("NotEnded must never map to Removable: {detail}");
            }
        }
    }

    // A leg that went `Terminal` (e.g. an unlaunchable agent argv) has no
    // live run to orphan — `end_run` already sent it `stop` and waited
    // for confirmed exit before ever reporting this outcome, so the row
    // must be `Removable`, never stuck `Kept` forever (the gap this
    // whole variant closes: an unendable capsule row).
    #[test]
    fn terminal_outcome_is_removable_not_kept() {
        use crate::rows::run::end_run::EndRunOutcome as O;
        match capsule_destroy_outcome_of(O::Terminal) {
            CapsuleDestroyOutcome::AlreadyRemoved => unreachable!("never an end_run mapping"),
            CapsuleDestroyOutcome::Removable(detail) => {
                assert!(
                    detail.contains("terminal"),
                    "detail should explain the row was terminal: {detail}"
                );
            }
            CapsuleDestroyOutcome::Kept { detail } => {
                panic!("Terminal is a confirmed end (stop was sent and awaited) — must not be Kept: {detail}");
            }
        }
    }

    // `Unheld` (no supervisor holds the row — see its own doc) is a
    // confirmed end, same family as `Terminal`/`AlreadyEnded`: `Removable`,
    // never `Kept`.
    #[test]
    fn unheld_outcome_is_removable_not_kept() {
        use crate::rows::run::end_run::EndRunOutcome as O;
        match capsule_destroy_outcome_of(O::Unheld) {
            CapsuleDestroyOutcome::AlreadyRemoved => unreachable!("never an end_run mapping"),
            CapsuleDestroyOutcome::Removable(detail) => {
                assert_eq!(detail, "no supervisor held the row");
            }
            CapsuleDestroyOutcome::Kept { detail } => {
                panic!("Unheld means nobody holds the row — must not be Kept: {detail}");
            }
        }
    }

    // A `Kept` outcome must build the SAME typed error the non-default
    // path returns, and must NEVER signal a `run_ended` broadcast.
    #[test]
    fn kept_outcome_builds_the_typed_error_and_never_broadcasts_run_ended() {
        let (payload, confirmed_ended) = default_row_end_response(
            "ws-local-1",
            "local",
            "local",
            CapsuleDestroyOutcome::Kept {
                detail: "supervisor is starting; retry".to_string(),
            },
        );
        assert_eq!(
            payload.get("code").and_then(|v| v.as_str()),
            Some("capsule_end_not_reached")
        );
        assert!(payload.get("error").is_some());
        assert!(
            payload.get("workspace_id").is_none(),
            "must not carry the success shape's own fields: {payload:?}"
        );
        assert!(payload.get("kept").is_none());
        assert!(
            !confirmed_ended,
            "a Kept outcome must never signal a run_ended broadcast"
        );
    }

    // The mirror case: a `Removable` (confirmed) outcome DOES build the
    // success shape and DOES signal the broadcast.
    #[test]
    fn removable_outcome_builds_success_and_signals_run_ended() {
        let (payload, confirmed_ended) = default_row_end_response(
            "ws-local-1",
            "local",
            "local",
            CapsuleDestroyOutcome::Removable("run ended and verified".to_string()),
        );
        assert!(
            payload.get("error").is_none(),
            "must not error: {payload:?}"
        );
        assert_eq!(
            payload.get("workspace_id").and_then(|v| v.as_str()),
            Some("ws-local-1")
        );
        assert!(payload.get("kept").and_then(|v| v.as_str()).is_some());
        assert!(confirmed_ended);
    }
}

#[cfg(all(test, unix))]
mod registration_delete_tests {
    use super::*;

    /// A pin, not a power loss (no test can cut the power): a delete whose
    /// directory cannot be synced is not a removed registration.
    #[test]
    fn registration_delete_syncs_its_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let toml = dir.path().join("row.toml");
        let mode = |m: u32| std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(m)).unwrap();
        std::fs::write(&toml, "x").unwrap();
        // Write and search but no read: the unlink succeeds, and only the
        // directory's open for its sync fails.
        mode(0o300);
        let removed = remove_registration(&[toml.clone(), dir.path().join("absent.toml")]);
        // The shutdown's retry finds the file gone; with its directory still
        // unsynced, that is not a removal either.
        let retried = remove_registration(&[toml.clone()]);
        mode(0o700);
        assert!(!toml.exists(), "the unlink itself failed");
        assert!(!removed, "a registration delete whose directory was not synced counted as removed");
        assert!(!retried, "a retry that found the file gone counted it removed while its directory was still unsynced");
        assert!(remove_registration(&[toml]), "a missing file is success");
    }
}
