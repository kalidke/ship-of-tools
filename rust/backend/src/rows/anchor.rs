//! The default row: the inert anchor rule and its launch seed.

use super::*;

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
