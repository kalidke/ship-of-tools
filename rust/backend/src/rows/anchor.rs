//! The default row: the inert anchor rule and its launch seed.

use super::*;

use crate::handlers::remove_comm_agents_for_workspace;
use crate::paths;
use crate::workspaces;

impl Workspaces {
    /// ADR 0042 amendment (owner rulings 2026-09-04 and 2026-09-06): is `ws`
    /// this daemon's default row in its INERT ANCHOR state -- the fallback
    /// row that browses the machine's files but is NOT a session, on every
    /// host and runtime alike. The ONE predicate for every daemon-side
    /// consequence of "not a session": nothing starts it on attach, and it
    /// does not claim its root against a real session (the duplicate-root
    /// gate). Ship of Tools development happens in a `ship-of-tools` row.
    pub fn is_inert_default_anchor(&self, ws: &Workspace) -> bool {
        ws.agent() == "none" && self.default_id().as_deref() == Some(ws.workspace_id.as_str())
    }

    /// Reset `workspace_id`'s `agent`/`agent_name` back to the inert-anchor
    /// shape ("none" / "") — called once a default row's run is CONFIRMED
    /// ended (`handle_workspace_destroy`'s default-capsule-row branch), so
    /// `is_inert_default_anchor` reads true again and the row disappears
    /// from `workspace.list` instead of surviving forever with a stale
    /// `agent` from before the run ended (field defect, v0.6.0-rc.12: a
    /// row whose agent predated the "nothing runs in the anchor" rule
    /// never went inert again after its run was ended). Mutates `agent`/`agent_name`
    /// IN PLACE, never via `insert`'s replacement path, so the phase cell, activation
    /// error and observer task survive.
    pub fn reset_agent_to_none(&self, workspace_id: &str) -> Option<Arc<Workspace>> {
        let ws = {
            let g = self.inner.read().expect("workspaces lock");
            g.by_id.get(workspace_id)?.clone()
        };
        ws.reset_agent_in_place();
        Some(ws)
    }
}

/// The single decision of what LAUNCH FIELDS (`autostart_claude`, `agent`,
/// `agent_name`, `task`) the daemon's own default/home row gets at boot —
/// the counterpart of [`default_row_runtime`] above, for the launch
/// fields rather than the runtime string.
///
/// 2026-09-04 amendment (owner ruling): the default/home row is an INERT
/// ANCHOR, not a session — the workspace the daemon falls back to and
/// the way to browse this machine's files. A genuinely first-ever launch
/// (`existing: None`) therefore seeds no agent and no autostart, on
/// EVERY host alike — no OS branch needed here any more (before this
/// amendment Windows alone seeded `agent = "claude"`,
/// `autostart_claude = true`, so pressing Enter on the row silently
/// started a claude capsule and it looked like every other session).
///
/// `existing` survives verbatim UNLESS it is a Windows CORRUPTED row —
/// `existing.0` (its on-disk `runtime`) is not `"capsule"`, the same
/// field incident [`default_row_runtime`]'s own doc describes: whatever
/// wrote a stale `"tmux"` there flipped `agent`/`autostart_claude`
/// alongside it, so preserving them verbatim would boot a capsule with a
/// corrupted agent. That row re-seeds to the SAME inert defaults a
/// first-ever launch gets, rather than carrying the corruption forward.
///
/// `existing` is `(runtime, autostart_claude, agent, agent_name, task)`
/// — the persisted row's own fields, read back before `Workspace::from_label`
/// would otherwise silently replace them (`insert`'s "new metadata wins"
/// semantics, `server::run`'s own doc).
pub(crate) fn default_row_launch_seed(
    existing: Option<(bool, &str, &str, &str)>,
) -> (bool, String, String, String) {
    match existing {
        Some((autostart_claude, agent, agent_name, task)) => (
            autostart_claude,
            agent.to_string(),
            agent_name.to_string(),
            task.to_string(),
        ),
        None => (false, "none".to_string(), String::new(), String::new()),
    }
}

/// Seeds the default row from the persisted one, or as the inert anchor on a first launch, and saves it.
pub(crate) fn seed_default_row(opts: &crate::Opts, files_mode: &FilesMode, workspaces: &Workspaces) {
    let default_label = opts
        .label
        .clone()
        .or_else(|| {
            files_mode
                .root_path()
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "home".to_string());
    // ADR 0042 slice L1a, Codex review finding 5: the default workspace's
    // OWN `runtime` must survive this re-registration. `scan_disk` (just
    // above) already loaded it correctly from its toml if one exists —
    // read it back BEFORE constructing a fresh seed, whose own
    // `Workspace::from_label` default ("tmux") would otherwise silently
    // clobber a scanned capsule default back to tmux on every restart
    // (`insert`'s own "new metadata wins" semantics, working exactly as
    // designed, applied to the wrong source of truth). `None` means a
    // genuinely first-ever launch on this machine.
    //
    // Rule G (shrink round): the SAME clobber risk applies to the launch
    // fields — `insert`'s own doc ("the rest of the metadata is taken
    // from the new ws") means whatever `from_label` builds here REPLACES
    // the persisted row's `agent`/`agent_name`/`autostart_claude`/`task`
    // on EVERY restart, not just at create time. An existing default
    // row's persisted launch fields must survive re-registration the
    // same way its runtime does (below), computed here BEFORE
    // construction rather than patched after, since `from_label` takes
    // them as constructor args.
    let existing_default = workspaces.resolve(Some(&paths::slug(&default_label)));
    // ADR 0042 amendment (2026-09-04) governs a FIRST-EVER row only: the
    // preserve arm below keeps an existing default row's launch fields
    // verbatim, so a box whose row a pre-amendment daemon had already
    // stamped with an agent keeps behaving as before — a visible, startable
    // session at the home root — with no signal that a one-time cleanup is
    // owed (field day 2026-09-05: found by forensics on a Windows box). Say
    // so at boot, once, naming the remedy; never rewrite the row (it may be
    // a session the user is relying on).
    if let Some(existing) = &existing_default {
        if existing.runtime == "capsule" && existing.agent() != "none" {
            tracing::warn!(
                workspace_id = %existing.workspace_id,
                agent = %existing.agent(),
                toml = %workspaces::toml_path_for(&existing.slug).display(),
                "default workspace carries an agent, so it lists and starts as an ordinary session \
                 (a pre-2026-09-04 seed, or a deliberate choice); to make it the inert anchor: stop \
                 the daemon, set agent = \"none\" and autostart_claude = false in that toml, start again"
            );
        }
    }
    // 2026-09-04 amendment (owner ruling): the daemon's own home/default
    // row is an INERT ANCHOR — the workspace it falls back to and the
    // way to browse this machine's files, not a session — so a
    // genuinely first-ever launch seeds no agent and no autostart on
    // every host alike (before this amendment, Windows seeded
    // `agent = "claude"`, `autostart_claude = true` here, so pressing
    // Enter on it silently started a claude capsule and the row looked
    // like every other session — the exact confusion this amendment
    // removes). `default_row_launch_seed` (workspaces.rs, the launch-field
    // counterpart of `default_row_runtime` below) is the one place this
    // decision — and the Windows corrupted-row re-seed's OWN identical
    // fallback — is made, so it stays unit-testable without a live
    // registry.
    let existing_agent = existing_default.as_ref().map(|e| e.agent());
    let existing_agent_name = existing_default.as_ref().map(|e| e.agent_name());
    let (seed_autostart, seed_agent, seed_agent_name, seed_task) =
        workspaces::default_row_launch_seed(existing_default.as_deref().map(|e| {
            (
                e.autostart_claude,
                existing_agent.as_deref().unwrap_or_default(),
                existing_agent_name.as_deref().unwrap_or_default(),
                e.task.as_str(),
            )
        }));
    let mut default_ws_seed = Workspace::from_label(
        &default_label,
        files_mode.root_path().to_path_buf(),
        seed_autostart,
        seed_agent,
        seed_agent_name,
        seed_task,
    );
    // ADR 0042 slice L1a: route through the ONE function that decides
    // this row's runtime for this OS (`workspaces::default_row_runtime`
    // — see its own doc) rather than re-deciding it here. On Windows
    // this is unconditionally "capsule", correcting rather than
    // preserving a stale on-disk "tmux" leftover — the field incident
    // this fixes: the old preserve-verbatim behaviour never self-healed
    // such a value, and the daemon then refused to start the row at all
    // (`pty spawn failed error=tmux is not available on Windows`), a
    // dead end (`default_workspace_not_destroyable`, below).
    if let Some(existing) = &existing_default {
        if cfg!(windows) && existing.runtime != "capsule" {
            tracing::info!(
                workspace_id = %existing.workspace_id,
                on_disk_runtime = %existing.runtime,
                "default workspace runtime on Windows must be capsule (ADR 0042 L1a); \
                 correcting a stale on-disk value and re-seeding its agent/autostart \
                 to the inert anchor defaults (a corrupted row's launch fields, not \
                 just its runtime)"
            );
        }
    }
    // Manager review (S16, Codex finding S16): carry the existing row's
    // declared `agent_handle` (ADR 0046 decision 1's `agent.join`)
    // forward the same way `runtime` is above — `from_label` seeds a
    // fresh row with none at all, so without this every boot silently
    // wiped a default row's already-joined handle on the very next save.
    if let Some(existing) = &existing_default {
        default_ws_seed.agent_handle = std::sync::Mutex::new(existing.agent_handle());
    }
    workspaces.insert(default_ws_seed);
    let default_ws = workspaces
        .resolve(Some(&paths::slug(&default_label)))
        .expect("default workspace just inserted");
    workspaces.set_default(&default_ws.workspace_id);
    if let Err(e) = workspaces::save(&default_ws) {
        tracing::warn!(error = %e, "could not persist default workspace toml");
    } else {
        tracing::info!(
            workspace_id = %default_ws.workspace_id,
            slug = %default_ws.slug,
            "default workspace ready"
        );
    }
}

/// After a default row's run is CONFIRMED ended (`confirmed_ended` from
/// `default_row_end_response`), reset the row's `agent`/`agent_name` back
/// to the inert-anchor shape and persist + broadcast the change — the
/// ADR 0042 amendment invariant ("an anchor with no run is inert, and
/// inert anchors are hidden") applied to the one path that used to leave
/// a carried-over `agent` stuck forever (field defect, v0.6.0-rc.12: the
/// owner once started an agent in this row before that rule existed, and
/// nothing ever reset `agent` back to "none" once its run ended, so
/// `Workspaces::is_inert_default_anchor` never went true again). A
/// `false` confirmed_ended is a no-op: `default_row_end_response` already
/// built the typed-error response for a `Kept` outcome, and neither the
/// row nor its toml may change under a refusal.
///
/// ADR 0043 decision 35: also prunes the row's sot-comm registry entries,
/// the same way `workspace.destroy`'s non-default path does below — a
/// killed default-row agent can't run its own `comm-leave`, so without
/// this its row lingered as a ghost `workspace.list` merges back in.
///
/// `held_guard` is `destroy_capsule_workspace`'s own row guard, carried
/// through unexamined so it stays locked across the reset below too
/// (ADR 0043 decision 33, Codex review round 2) — dropped only once this
/// function returns, whichever arm it takes.
pub(crate) async fn end_default_row_run(
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
    workspace_id: &str,
    slug: &str,
    agent_name: &str,
    confirmed_ended: bool,
    _held_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
) {
    if !confirmed_ended {
        return;
    }
    let reg_agent = agent_name.to_string();
    let reg_ws = workspace_id.to_string();
    let reg_host = crate::workspaces::declared_host();
    let comm_removed = tokio::task::spawn_blocking(move || {
        remove_comm_agents_for_workspace(&reg_agent, &reg_ws, &reg_host)
    })
    .await
    .unwrap_or_default();
    if !comm_removed.is_empty() {
        tracing::info!(
            removed = ?comm_removed,
            slug = %slug,
            "pruned sot-comm registry rows for the default row's ended run"
        );
    }
    if let Some(reset) = workspaces.reset_agent_to_none(workspace_id) {
        if let Err(e) = crate::workspaces::save(&reset) {
            tracing::warn!(error = %e, workspace_id = %workspace_id,
                "default row agent-reset toml persist failed; workspace is in-memory only");
        }
    }
    // Live-push so the Sessions strip re-lists — the row's phase is
    // derived fresh from the supervisor lane on every `workspace.list`
    // call. `action` is informational only: every `workspace.changed`
    // push just triggers an FE re-list.
    let _ = ws_events.send(WorkspaceChanged {
        action: "run_ended".into(),
        slug: slug.to_string(),
        workspace_id: workspace_id.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inert_default_anchor_is_the_default_row_with_no_agent_on_any_runtime() {
        let reg = Workspaces::new();
        let mut row = Workspace::from_label("local", PathBuf::from("/home/u"), false, "none".into(), String::new(), String::new());
        row.runtime = "capsule".to_string();
        let row = reg.insert(row);
        // Not the default yet -> an ordinary (if agent-less) capsule row.
        assert!(!reg.is_inert_default_anchor(&row));
        reg.set_default(&row.workspace_id);
        assert!(reg.is_inert_default_anchor(&row));
        // The runtime does not matter (2026-09-06): a tmux backend's default
        // row with no agent is the anchor too, and is hidden the same way.
        // Default capsule row that carries an agent: a session, not the anchor.
        let mut agent = Workspace::from_label("local", PathBuf::from("/home/u"), true, "claude".into(), String::new(), String::new());
        agent.runtime = "capsule".to_string();
        let agent = reg.insert(agent);
        assert!(!reg.is_inert_default_anchor(&agent));
    }

    /// Field defect (v0.6.0-rc.12): a default row that once carried an
    /// agent (pre-dating the "nothing runs in the anchor" rule) never
    /// went inert again after its run ended, because nothing ever reset
    /// `agent` back to "none" — `reset_agent_to_none` is that reset, and
    /// this proves it flips `is_inert_default_anchor` from false to true
    /// while preserving the row's id and every other field.
    #[test]
    fn reset_agent_to_none_makes_a_carried_over_default_row_inert_again() {
        let reg = Workspaces::new();
        let mut row = Workspace::from_label(
            "local",
            PathBuf::from("/home/u"),
            true,
            "claude".into(),
            "kal-local".into(),
            "hello".into(),
        );
        row.runtime = "capsule".to_string();
        let row = reg.insert(row);
        reg.set_default(&row.workspace_id);
        let original_id = row.workspace_id.clone();
        assert!(
            !reg.is_inert_default_anchor(&row),
            "a default row that carries an agent is a real session, not the anchor"
        );

        let reset = reg
            .reset_agent_to_none(&row.workspace_id)
            .expect("the row is registered");
        assert_eq!(reset.workspace_id, original_id, "reset must preserve the id");
        assert_eq!(reset.agent(), "none");
        assert_eq!(reset.agent_name(), "");
        assert_eq!(reset.runtime, "capsule", "unrelated metadata must survive the reset");
        assert!(
            reg.is_inert_default_anchor(&reset),
            "with agent reset to none, the default row must be inert again"
        );
        assert!(
            Arc::ptr_eq(&reset, &row),
            "reset must mutate the EXISTING row's Arc in place, never replace it"
        );
        // The registry's own row (not just the returned handle) reflects
        // the reset -- resolve() must see it too.
        let resolved = reg.resolve(Some(&original_id)).unwrap();
        assert!(reg.is_inert_default_anchor(&resolved));
        assert!(Arc::ptr_eq(&resolved, &row), "resolve() must return the SAME Arc too");
    }

    /// 2026-09-04 amendment: a genuinely first-ever launch seeds the
    /// default/home row as an INERT ANCHOR — no agent, no autostart — on
    /// EVERY host, not just non-Windows. Runs (and must pass) on every
    /// platform: unlike the corrupted-row case below, this arm no longer
    /// branches on `cfg!(windows)` at all.
    #[test]
    fn default_row_launch_seed_first_launch_is_inert_everywhere() {
        assert_eq!(
            default_row_launch_seed(None),
            (false, "none".to_string(), String::new(), String::new())
        );
    }

    /// An existing default row whose on-disk `runtime` is `"capsule"` —
    /// healthy on every platform this function runs on (Windows requires
    /// it; every other host merely permits it) — survives verbatim: its
    /// launch fields are never silently clobbered back to the inert
    /// defaults just because it was re-registered at boot.
    #[test]
    fn default_row_launch_seed_preserves_a_healthy_existing_row() {
        assert_eq!(
            default_row_launch_seed(Some((true, "claude", "kal-sot", "hello"))),
            (
                true,
                "claude".to_string(),
                "kal-sot".to_string(),
                "hello".to_string()
            )
        );
    }
}
