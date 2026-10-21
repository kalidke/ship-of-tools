//! The row registry: `Workspaces` insert, lookup, removal and the per-row guard.

use super::*;

impl Workspaces {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a workspace. Idempotent on slug: if an entry exists for
    /// the same slug we keep its workspace_id (a stable id across daemon
    /// restarts is the contract), but the rest of the metadata, its declared
    /// handle included, is taken from the new `ws` so a fresh project_root
    /// from disk wins over a stale one in memory. Resource caches on the *old* entry are
    /// discarded — the assumption is that re-insertion happens at most
    /// once at startup (scan_disk) and during explicit workspace
    /// metadata edits, neither of which is on a hot path.
    pub fn insert(&self, ws: Workspace) -> Arc<Workspace> {
        self.insert_replacing(ws).0
    }

    /// `insert`, also returning the row of the same slug it replaced.
    pub fn insert_replacing(&self, ws: Workspace) -> (Arc<Workspace>, Option<Arc<Workspace>>) {
        let mut g = self.inner.write().expect("workspaces lock");
        let replaced = g.by_slug.get(&ws.slug).cloned().and_then(|id| g.by_id.get(&id).cloned());
        let preserved_id = replaced.as_ref().map(|w| w.workspace_id.clone());
        let final_ws = match preserved_id {
            Some(id) => {
                // Same slug → keep id, new metadata wins.
                let mut w = Workspace::meta_only(
                    id,
                    ws.slug.clone(),
                    ws.label.clone(),
                    ws.project_root.clone(),
                    ws.session_name.clone(),
                    ws.created,
                    ws.autostart_claude,
                    ws.agent(),
                    ws.agent_name(),
                    ws.task.clone(),
                );
                w.runtime = ws.runtime.clone();
                w.account = Mutex::new(ws.account());
                w.agent_handle = ws.agent_handle.copy();
                w
            }
            None => ws,
        };
        let arc = Arc::new(final_ws);
        g.by_slug
            .insert(arc.slug.clone(), arc.workspace_id.clone());
        g.by_id.insert(arc.workspace_id.clone(), arc.clone());
        if let Some(tx) = g.watch_bus.clone() {
            Self::spawn_workspace_watcher(&arc, tx);
        }
        (arc, replaced)
    }

    /// Spawn `ws`'s file watcher onto the shared bus. Cheap inline
    /// (`Watcher::spawn` opens the inotify fd and defers the recursive
    /// registration walk to a background thread — the NFS-stall hardening),
    /// so calling under the registry lock is fine. Failure is a warning:
    /// previews still work, that workspace just won't live-refresh.
    fn spawn_workspace_watcher(ws: &Arc<Workspace>, tx: broadcast::Sender<PreviewChanged>) {
        if ws.watcher.get().is_some() {
            return; // already spawned (or recorded as failed)
        }
        let spawned = match ws.files_mode() {
            Ok(fm) => match Watcher::spawn(fm.root_path(), fm.clone(), tx, Some(ws.slug.clone())) {
                Ok(w) => Some(Arc::new(w)),
                Err(e) => {
                    tracing::warn!(slug = %ws.slug, error = %e,
                        "workspace watcher spawn failed; nav will not live-refresh here");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(slug = %ws.slug, error = %e,
                    "workspace watcher: files_mode init failed");
                None
            }
        };
        let _ = ws.watcher.set(spawned);
    }

    /// Install the watch bus and spawn watchers for every ALREADY-registered
    /// workspace (registration order at startup isn't guaranteed relative to
    /// bus creation). Idempotent per workspace via the `watcher` OnceLock.
    pub fn set_watch_bus(&self, tx: broadcast::Sender<PreviewChanged>) {
        let existing: Vec<Arc<Workspace>> = {
            let mut g = self.inner.write().expect("workspaces lock");
            g.watch_bus = Some(tx.clone());
            g.by_id.values().cloned().collect()
        };
        for ws in existing {
            Self::spawn_workspace_watcher(&ws, tx.clone());
        }
    }

    pub fn set_default(&self, workspace_id: &str) {
        let mut g = self.inner.write().expect("workspaces lock");
        g.default_id = Some(workspace_id.to_string());
    }

    /// Install the per-backend `repl.frame` broadcast sender. Called once at
    /// startup, right after the channel is created in `run()`, before any
    /// connection is accepted.
    pub fn set_repl_frame_tx(&self, tx: broadcast::Sender<ReplFrameMsg>) {
        let mut g = self.inner.write().expect("workspaces lock");
        g.repl_frame_tx = Some(tx);
    }

    /// Clone the per-backend `repl.frame` broadcast sender. Handed to
    /// `Workspace::repl` so a per-workspace Repl publishes onto the bus every
    /// connection subscribes to. Panics if called before `set_repl_frame_tx`
    /// — startup always wires it before serving, so a `None` here is a bug.
    pub fn repl_frame_tx(&self) -> broadcast::Sender<ReplFrameMsg> {
        let g = self.inner.read().expect("workspaces lock");
        g.repl_frame_tx
            .clone()
            .expect("repl_frame_tx set at startup")
    }

    /// Install the server-monitoring hub. Called once at startup, right after
    /// `MonitorHub::start`, before any connection is accepted.
    pub fn set_monitor_hub(&self, hub: crate::sidecars::monitor::MonitorHub) {
        let mut g = self.inner.write().expect("workspaces lock");
        g.monitor_hub = Some(hub);
    }

    /// Clone the server-monitoring hub, if installed. `None` before startup
    /// wires it (or in tests) so callers degrade gracefully.
    pub fn monitor_hub(&self) -> Option<crate::sidecars::monitor::MonitorHub> {
        let g = self.inner.read().expect("workspaces lock");
        g.monitor_hub.clone()
    }

    /// A finished handle reads as absent.
    pub(crate) fn has_observer(&self, workspace_id: &str) -> bool {
        let g = self.inner.read().expect("workspaces lock");
        g.observers.get(workspace_id).map(|(h, _)| !h.is_finished()).unwrap_or(false)
    }

    /// Checked against the SAME registry lock `remove_by_id` uses: a row removed
    /// mid-spawn gets its handle aborted immediately instead of stored.
    pub(crate) fn install_observer(
        &self,
        workspace_id: &str,
        handle: tokio::task::JoinHandle<()>,
        cancel: Arc<dyn Fn() + Send + Sync>,
    ) -> bool {
        let mut g = self.inner.write().expect("workspaces lock");
        if !g.by_id.contains_key(workspace_id) {
            cancel();
            handle.abort();
            return false;
        }
        if let Some((prev, prev_cancel)) = g.observers.insert(workspace_id.to_string(), (handle, cancel)) {
            prev_cancel();
            prev.abort();
        }
        true
    }

    /// This row's lifecycle guard (ADR 0043 decision 33) — despite the
    /// name, mints/returns one for ANY registered row, tmux included
    /// (round 3: `workspace.destroy`'s tmux arm and
    /// `agent.join` both take it, closing the same join-vs-destroy race
    /// for every runtime, not only capsules) — created on demand under
    /// the SAME write lock the registry itself uses, so two concurrent
    /// first-callers for one never-before-seen id can never mint two
    /// different mutexes for it. Every lifecycle mutation of a row holds
    /// the returned `Arc` for its whole duration and rechecks
    /// membership/phase once it actually has the lock. `None` when
    /// `workspace_id` is not currently registered (Codex review,
    /// 2026-09-11): checked under this SAME write lock before inserting,
    /// so a caller that races `remove_by_id` never mints an orphan entry
    /// for a row already gone — the one thing `remove_by_id`'s own
    /// cleanup cannot prevent on its own, since it runs under a
    /// DIFFERENT acquisition of this lock.
    pub fn capsule_guard(&self, workspace_id: &str) -> Option<Arc<tokio::sync::Mutex<()>>> {
        let mut g = self.inner.write().expect("workspaces lock");
        if !g.by_id.contains_key(workspace_id) {
            return None;
        }
        Some(
            g.capsule_guards
                .entry(workspace_id.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone(),
        )
    }

    /// Current default workspace id, if one has been set. Consumed by
    /// `workspace.list` to mark the default entry; the frontend uses it
    /// to render a "(default)" badge and skip switch-back logic.
    pub fn default_id(&self) -> Option<String> {
        let g = self.inner.read().expect("workspaces lock");
        g.default_id.clone()
    }

    /// Record which account this row's agent runs as (ADR 0046 decision
    /// 6, `workspace.reauth`), in place on the SHARED `Arc` exactly like
    /// [`set_agent_handle`](Self::set_agent_handle) — the row keeps its
    /// id, slug, root, session name and declared handle, and every later
    /// spawn path reads the new value from the registry at spawn time
    /// (`rows::run::start::spawn_and_watch`). `None` when the
    /// row is not registered; the caller persists the toml itself.
    pub fn set_account(&self, workspace_id: &str, account: &str) -> Option<Arc<Workspace>> {
        let ws = {
            let g = self.inner.read().expect("workspaces lock");
            g.by_id.get(workspace_id)?.clone()
        };
        *ws.account.lock().unwrap_or_else(|e| e.into_inner()) = account.to_string();
        Some(ws)
    }

    /// [`set_account`](Self::set_account), but only while `row` is still the
    /// registered row for its id: the check and the write hold the registry's
    /// lock, which `insert` needs to replace the row, so nothing replaces it
    /// in between. `false` when it was replaced or removed; nothing is
    /// written then.
    pub fn set_account_on(&self, row: &Arc<Workspace>, account: &str) -> bool {
        let g = self.inner.read().expect("workspaces lock");
        let registered = g
            .by_id
            .get(&row.workspace_id)
            .is_some_and(|w| Arc::ptr_eq(w, row));
        if registered {
            *row.account.lock().unwrap_or_else(|e| e.into_inner()) = account.to_string();
        }
        registered
    }

    /// Resolve an optional workspace_id to a workspace handle. `None`
    /// → default. A non-default id that's missing is `None` (caller's
    /// responsibility to error). The returned `Arc` shares the same
    /// resource caches with all other holders.
    pub fn resolve(&self, id: Option<&str>) -> Option<Arc<Workspace>> {
        let g = self.inner.read().expect("workspaces lock");
        let key = id.map(|s| s.to_string()).or_else(|| g.default_id.clone())?;
        // The id might actually be a slug (we accept both). Try id
        // first, then slug-index.
        if let Some(ws) = g.by_id.get(&key) {
            return Some(ws.clone());
        }
        if let Some(real_id) = g.by_slug.get(&key) {
            return g.by_id.get(real_id).cloned();
        }
        None
    }

    /// Exact slug membership — the collision key `insert` replaces on.
    /// Unlike `resolve`, an id is never accepted in place of a slug.
    pub fn has_slug(&self, slug: &str) -> bool {
        self.inner
            .read()
            .expect("workspaces lock")
            .by_slug
            .contains_key(slug)
    }

    pub fn list(&self) -> Vec<Arc<Workspace>> {
        let g = self.inner.read().expect("workspaces lock");
        let mut out: Vec<Arc<Workspace>> = g.by_id.values().cloned().collect();
        out.sort_by(|a, b| a.slug.cmp(&b.slug));
        out
    }

    /// The whole workspace owning `target` — see `Workspace::runtime`'s
    /// own doc for why a capsule workspace still has one. `pty.open`
    /// resolves the target through this before answering `attach_direct`.
    pub fn workspace_for_tmux(&self, target: &str) -> Option<Arc<Workspace>> {
        let g = self.inner.read().expect("workspaces lock");
        g.by_id.values().find(|ws| ws.session_name == target).cloned()
    }

    /// Claims `slug` for one `workspace.create`, or for the startup Cleanup that ends its row; `None` when another
    /// holds it. The claim ends when dropped.
    pub fn claim_create(&self, slug: &str) -> Option<CreateClaim> {
        let mut g = self.inner.write().expect("workspaces lock");
        g.creating
            .insert(slug.to_string())
            .then(|| CreateClaim { workspaces: self.clone(), slug: slug.to_string() })
    }

    /// Undoes a create's insert: when the registry still holds `inserted`, puts `prior` (the row it replaced) back
    /// under its slug and id, or removes the row if there was none. Returns whether it acted; `false` means a
    /// destroy or another insert got there first, and the caller leaves the row and its toml alone. A restored
    /// row's observer, guard and watcher were never touched.
    pub fn undo_insert(&self, inserted: &Arc<Workspace>, prior: Option<Arc<Workspace>>) -> bool {
        let mut g = self.inner.write().expect("workspaces lock");
        if !g.by_id.get(&inserted.workspace_id).is_some_and(|w| Arc::ptr_eq(w, inserted)) {
            return false;
        }
        match prior {
            Some(prior) => {
                g.by_slug.insert(prior.slug.clone(), prior.workspace_id.clone());
                g.by_id.insert(prior.workspace_id.clone(), prior);
            }
            None => {
                Self::remove_locked(&mut g, &inserted.workspace_id);
            }
        }
        true
    }

    pub fn remove_by_id(&self, id: &str) -> Option<Arc<Workspace>> {
        let mut g = self.inner.write().expect("workspaces lock");
        Self::remove_locked(&mut g, id)
    }

    fn remove_locked(g: &mut Inner, id: &str) -> Option<Arc<Workspace>> {
        let removed = g.by_id.remove(id)?;
        g.by_slug.remove(&removed.slug);
        // ADR 0043 decision 33: the guard dies with the row. A caller
        // that is still holding its own `Arc` clone (e.g. the watchdog
        // mid-restart) keeps that mutex alive until it drops it — this
        // only stops a FUTURE `capsule_guard(id)` call for this id from
        // handing out the same, now-retired mutex; a fresh id reuse
        // starts with a fresh one.
        g.capsule_guards.remove(id);
        // Cancel before abort: a round blocked in `spawn_blocking` only returns via the cancelled connection.
        if let Some((h, cancel)) = g.observers.remove(id) {
            cancel();
            h.abort();
        }
        if g.default_id.as_deref() == Some(id) {
            g.default_id = None;
        }
        Some(removed)
    }
}

/// A slug held by one `workspace.create` or by the startup Cleanup (`Workspaces::claim_create`); released on drop.
pub struct CreateClaim {
    workspaces: Workspaces,
    slug: String,
}

impl Drop for CreateClaim {
    fn drop(&mut self) {
        let mut g = self.workspaces.inner.write().unwrap_or_else(|e| e.into_inner());
        g.creating.remove(&self.slug);
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
