//! Workspace keys: the (host, slug) key every per-row map uses, and the current and reply keys `State` derives from it.

use super::*;

/// Normalize a wire `workspace_id` to the workspace-key literal. BOTH
/// spellings of the daemon-default workspace — `None` AND its actual slug
/// (`Some(default_slug)`) — map to `"<default>"`: startup keys the default
/// as `None`, but workspace cycling and Sessions-Enter address it by slug,
/// and without this collapse one physical workspace would split into two
/// tree/snapshot keys (slot miss, cursor reset, duplicate root fetch).
/// Pure so the aliasing is unit-testable; `State::reply_ws_key` /
/// `current_workspace_key` supply `default_slug` so active-key and
/// reply-key computation can never disagree.
pub(in crate::ui) fn ws_key_of(workspace_id: Option<&str>, default_slug: Option<&str>) -> String {
    match workspace_id {
        None => "<default>".to_string(),
        Some(s) if default_slug == Some(s) => "<default>".to_string(),
        Some(s) => s.to_string(),
    }
}

/// Whether an fe-command's `workspace` argument names the daemon-default workspace: `""`, `"default"` or `"<default>"`.
pub(in crate::ui) fn is_default_workspace_name(name: &str) -> bool {
    name.is_empty() || name == "default" || name == "<default>"
}

impl State {
    pub(in crate::ui) fn active_result_workspace(&self) -> Option<ResolvedWorkspace> {
        self.row_directory.resolve(
            &self.active_host,
            self.active_workspace_id.as_deref().unwrap_or(""),
        )
        .ok()
    }

    pub(in crate::ui) fn active_result_row_key(&self) -> Option<WsKey> {
        self.active_result_workspace()
            .map(|target| target.row_key().clone())
    }

    /// Key used to index the parked views for the *current*
    /// workspace. The daemon's default workspace doesn't carry a slug
    /// in `active_workspace_id`; `<default>` is the literal we use
    /// instead so it has a snapshot slot too.
    pub(in crate::ui) fn current_workspace_key(&self) -> String {
        self.reply_ws_key(self.active_workspace_id.as_deref())
    }

    /// The host-qualified sibling of `current_workspace_key` (ADR 0042
    /// L2a, Codex review PR #163): `(active_host, current_workspace_key())`
    /// -- what every host-aware workspace-view map (`TreeScope::Workspace`,
    /// the UI/REPL snapshot maps, `nav_readme_defaulted`) is actually keyed
    /// by now. `current_workspace_key()` itself stays bare -- it's still
    /// the wire-level `workspace_id` normalization AND half of this pair,
    /// not a value to retype.
    pub(in crate::ui) fn active_ws_key(&self) -> WsKey {
        (self.active_host.clone(), self.current_workspace_key())
    }

    /// Workspace-key for a wire `workspace_id` — the ONE normalization both
    /// the active key and every reply key go through (see `ws_key_of` for
    /// the default-slug aliasing this collapses).
    pub(in crate::ui) fn reply_ws_key(&self, workspace_id: Option<&str>) -> String {
        ws_key_of(workspace_id, self.row_presentation.default_slug())
    }

    /// Storage key (a `WsKey`) for a `lifecycle` repl.frame evt's workspace
    /// hint — see `RowDirectory::lifecycle_key` for the translation rules. `host` is
    /// the connection that delivered the frame (ADR 0042 L2a).
    pub(in crate::ui) fn lifecycle_store_key(&self, host: &str, wire_hint: Option<&str>) -> WsKey {
        self.row_directory.lifecycle_key(
            host,
            wire_hint,
            self.row_presentation.default_slug(),
        )
    }

    /// Whether the ACTIVE workspace's REPL child is in the `starting`
    /// (precompiling/booting) state — drives the drawer's in-flight line
    /// ("julia starting…" instead of "(running…)"). `current_workspace_key`
    /// speaks "<default>" for the default workspace; the model keys lifecycle
    /// by raw slug, so resolve the collapse before the lookup.
    pub(in crate::ui) fn active_repl_starting(&self) -> bool {
        self.repl_model.starting(&self.active_ws_key(), self.row_presentation.default_slug())
    }

    /// Rename state keyed under the default workspace's RAW SLUG to the
    /// `"<default>"` literal. Called right after `workspace.list` (re)sets
    /// `default_workspace_slug`: before that reply, `reply_ws_key` couldn't
    /// collapse `Some(default_slug)`, so anything keyed in that window for
    /// the default ws addressed by slug landed under the slug — orphaned
    /// once every later lookup collapses (codex r3: a slug-keyed ReplEntry
    /// drops the whole eval's stream). Collision rule: an existing
    /// `"<default>"`-keyed entry wins (both describe the same physical ws;
    /// the None-addressed one was already routed correctly).
    pub(in crate::ui) fn migrate_default_slug_keys(&mut self, from_key: WsKey) {
        let (host, slug) = &from_key;
        const DEFAULT_KEY: &str = "<default>";
        // ADR 0042 L2a: `default_workspace_slug` is scoped to
        // `active_host`'s own default (its own doc comment) -- so this
        // migration operates on active_host's WsKey space specifically.
        // Every map it touches is host-qualified now; migrating the wrong
        // host's entries would be a silent no-op (miss) at best.
        let to_key: WsKey = (host.clone(), DEFAULT_KEY.to_string());
        self.parked_views.migrate(&from_key, to_key.clone());
        self.repl_model.migrate_default_key(&from_key, to_key.clone());
        // The README-defaulted one-shot marker is keyed the same way (an
        // ADR-0017 resume can restore the active ws BY SLUG pre-learn); a
        // slug-keyed marker left behind would let the README default fire a
        // second time and yank the cursor (codex r4).
        if self.nav_readme_defaulted.remove(&from_key) {
            self.nav_readme_defaulted.insert(to_key.clone());
        }
        for mode in [Mode::Files, Mode::Modules] {
            let from = (mode, TreeScope::Workspace(from_key.clone()));
            if let Some(slot) = self.tree_store.take(&from) {
                let to = (mode, TreeScope::Workspace(to_key.clone()));
                match self.tree_store.take(&to) {
                    None => self.tree_store.stash(to, slot),
                    Some(existing) => {
                        // Target existed — the default-keyed slot wins; put
                        // it back and drop the slug-keyed one.
                        self.tree_store.stash(to, existing);
                        tracing::debug!(?mode, %slug, %host,
                            "learn-migration: dropped slug-keyed tree slot (default-keyed slot exists)");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "workspace_key_tests.rs"]
mod tests;
