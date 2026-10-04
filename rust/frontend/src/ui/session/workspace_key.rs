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

/// Slug storage key for a `lifecycle` repl.frame evt's workspace hint (core
/// of `lifecycle_store_key`, free-function for unit tests — the `ws_key_of`
/// pattern). The hint is the Repl supervisor's identity: a CANONICAL
/// workspace_id (`ws-<slug>-<hash>`) for per-workspace REPLs (translate via
/// `id_slugs`; a canonical-id key against slug-keyed maps silently never
/// matches — the `started`-frame lesson) or `None` for a frame with no
/// workspace id, which only an older daemon sends (= the default workspace's
/// slug, "<default>" until the first list reply resolves it). An unknown hint is stored as-is: it may already be a slug.
/// ADR 0042 L2a codex review, item J: `id_slugs` is keyed by `(host, id)`,
/// not a bare id — a canonical workspace_id is `slug+pid+time`, but a
/// LEGACY id (older workspaces, or a daemon that hasn't rotated to the
/// new scheme) is just the bare slug, so two hosts' same-slug legacy ids
/// collide on a bare-id key. `host` (the frame's OWN connection, always
/// correct) resolves the lookup — never trusted from whatever the map
/// entry happened to store, which is exactly the class of bug a
/// bare-id key could produce (a same-id collision silently returning
/// the WRONG host's slug).
fn lifecycle_key_of(
    host: &str,
    wire_hint: Option<&str>,
    id_slugs: &HashMap<(HostKey, String), WsKey>,
    default_slug: Option<&str>,
) -> WsKey {
    match wire_hint {
        None => (
            host.to_string(),
            default_slug.unwrap_or("<default>").to_string(),
        ),
        Some(h) => id_slugs
            .get(&(host.to_string(), h.to_string()))
            .cloned()
            .unwrap_or_else(|| (host.to_string(), h.to_string())),
    }
}

impl State {
    /// Key used to index `workspace_ui_snapshots` for the *current*
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

    /// Caption-store key for an fe-command's `workspace` argument. The wire
    /// spells the default workspace three ways (`""`, `"default"`,
    /// `"<default>"`, per `preview_targets_active_ws`) plus its real slug;
    /// all four must land on the one key `current_workspace_key` will later
    /// look up, or a caption addressed to the default workspace is stored
    /// under a key nothing reads.
    pub(in crate::ui) fn caption_ws_key(&self, workspace: &str) -> String {
        let is_default = is_default_workspace_name(workspace);
        if is_default {
            "<default>".to_string()
        } else {
            ws_key_of(Some(workspace), self.default_workspace_slug.as_deref())
        }
    }

    /// Workspace-key for a wire `workspace_id` — the ONE normalization both
    /// the active key and every reply key go through (see `ws_key_of` for
    /// the default-slug aliasing this collapses).
    pub(in crate::ui) fn reply_ws_key(&self, workspace_id: Option<&str>) -> String {
        ws_key_of(workspace_id, self.default_workspace_slug.as_deref())
    }

    /// Storage key (a `WsKey`) for a `lifecycle` repl.frame evt's workspace
    /// hint — see `lifecycle_key_of` for the translation rules. `host` is
    /// the connection that delivered the frame (ADR 0042 L2a).
    pub(in crate::ui) fn lifecycle_store_key(&self, host: &str, wire_hint: Option<&str>) -> WsKey {
        lifecycle_key_of(
            host,
            wire_hint,
            &self.workspace_id_slugs,
            self.default_workspace_slug.as_deref(),
        )
    }

    /// Whether the ACTIVE workspace's REPL child is in the `starting`
    /// (precompiling/booting) state — drives the drawer's in-flight line
    /// ("julia starting…" instead of "(running…)"). `current_workspace_key`
    /// speaks "<default>" for the default workspace; `repl_lifecycle` keys
    /// by raw slug, so resolve the collapse before the lookup.
    pub(in crate::ui) fn active_repl_starting(&self) -> bool {
        let key = self.current_workspace_key();
        let slug = if key == "<default>" {
            match &self.default_workspace_slug {
                Some(s) => s.clone(),
                None => key,
            }
        } else {
            key
        };
        let ws_key: WsKey = (self.active_host.clone(), slug);
        matches!(
            self.repl_lifecycle.get(&ws_key).map(String::as_str),
            Some("starting")
        )
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
    pub(in crate::ui) fn migrate_default_slug_keys(&mut self) {
        let Some(slug) = self.default_workspace_slug.clone() else {
            return;
        };
        const DEFAULT_KEY: &str = "<default>";
        // ADR 0042 L2a: `default_workspace_slug` is scoped to
        // `active_host`'s own default (its own doc comment) -- so this
        // migration operates on active_host's WsKey space specifically.
        // Every map it touches is host-qualified now; migrating the wrong
        // host's entries would be a silent no-op (miss) at best.
        let host = self.active_host.clone();
        let from_key: WsKey = (host.clone(), slug.clone());
        let to_key: WsKey = (host.clone(), DEFAULT_KEY.to_string());
        if let Some(v) = self.workspace_ui_snapshots.remove(&from_key) {
            self.workspace_ui_snapshots
                .entry(to_key.clone())
                .or_insert(v);
        }
        if let Some(v) = self.workspace_repl_snapshots.remove(&from_key) {
            self.workspace_repl_snapshots
                .entry(to_key.clone())
                .or_insert(v);
        }
        for (k, v) in self.eval_id_workspace.iter_mut() {
            if k.0 == host && *v == from_key {
                *v = to_key.clone();
            }
        }
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
mod tests {
    use super::*;

    #[test]
    fn default_workspace_names() {
        for name in ["", "default", "<default>"] {
            assert!(is_default_workspace_name(name), "{name:?}");
        }
        for name in ["Default", " default", "default ", "<Default>", "defaults", "x"] {
            assert!(!is_default_workspace_name(name), "{name:?}");
        }
    }

    #[test]
    fn lifecycle_key_translates_canonical_id_to_slug() {
        // Lifecycle frames stamp the CANONICAL workspace id; every FE surface
        // keys by slug (the `started`-frame lesson: a canonical-id key
        // silently never matches). The map from the last workspace.list is
        // the translation. ADR 0042 L2a: the returned `WsKey` carries the
        // HOST that `workspace_id_slugs` recorded for the id, which can
        // differ from the frame's own connection (`host` below) — the
        // lookup wins over the fallback whenever the id IS known.
        let mut ids: HashMap<(HostKey, String), WsKey> = HashMap::new();
        ids.insert(
            ("myhost".to_string(), "ws-alpha-1a2b".to_string()),
            ("myhost".to_string(), "alpha".to_string()),
        );
        assert_eq!(
            lifecycle_key_of("myhost", Some("ws-alpha-1a2b"), &ids, Some("home")),
            ("myhost".to_string(), "alpha".to_string())
        );
        // Already-a-slug (or unknown) hints store as-is, under the frame's
        // OWN host (the only host information available for an unknown id).
        assert_eq!(
            lifecycle_key_of("myhost", Some("beta"), &ids, Some("home")),
            ("myhost".to_string(), "beta".to_string())
        );
        // ADR 0042 L2a codex review, item J: the SAME canonical id under a
        // DIFFERENT host must not resolve — otherwise ws-alpha-1a2b known
        // only to "myhost" would leak into "otherhost"'s lookup.
        assert_eq!(
            lifecycle_key_of("otherhost", Some("ws-alpha-1a2b"), &ids, Some("home")),
            ("otherhost".to_string(), "ws-alpha-1a2b".to_string()),
            "an id known only to a different host falls through to the unknown-hint case"
        );
    }

    #[test]
    fn lifecycle_key_none_hint_is_the_default_workspace() {
        // A frame with no workspace id (only an older daemon sends one) IS
        // the default workspace. Resolve to its slug once known, "<default>" before the
        // first workspace.list reply (transient; the list's repl_state
        // catch-up re-keys it). Always under the frame's own host.
        let ids: HashMap<(HostKey, String), WsKey> = HashMap::new();
        assert_eq!(
            lifecycle_key_of("myhost", None, &ids, Some("home")),
            ("myhost".to_string(), "home".to_string())
        );
        assert_eq!(
            lifecycle_key_of("myhost", None, &ids, None),
            ("myhost".to_string(), "<default>".to_string())
        );
    }
}
