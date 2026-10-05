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
        let mut g = self.inner.write().expect("workspaces lock");
        let preserved_id = g
            .by_slug
            .get(&ws.slug)
            .cloned()
            .and_then(|id| g.by_id.get(&id).map(|w| w.workspace_id.clone()));
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
                w.agent_handle = Mutex::new(ws.agent_handle());
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
        arc
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
    /// (manager review round 3: `workspace.destroy`'s tmux arm and
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

    /// Record the sot-comm handle a session inside `workspace_id` DECLARED
    /// via `agent.join` (ADR 0046 decision 1) — the daemon is told once
    /// instead of re-deriving it from a self-file read-back on every
    /// `workspace.list`/`clear_comm_unread` call — and MOVE it: ADR 0049
    /// gives a handle to one row only, so every OTHER row whose declared
    /// handle equals `handle` loses it (cell cleared) in the same step.
    /// Returns the row and the ids of the rows that lost the handle, for
    /// the caller to persist; an empty `handle` moves nothing. `None` if
    /// `workspace_id` is not registered (the `agent.join` handler reports
    /// this as `unknown_workspace`).
    ///
    /// Manager review (S6, Codex finding B5): a GUARDED IN-PLACE update,
    /// never a replacement `insert`. The write lands on each row's OWN
    /// interior `agent_handle` cell (the SAME `Arc<Workspace>`, resource
    /// caches — kernel, repl, watcher — untouched), not on a freshly
    /// reconstructed `Workspace` that would start every cache cold. This
    /// also closes the destroy race the old replace-based version had:
    /// since this method never touches `by_id`/`by_slug`, a `destroy` that
    /// removes the row between the lookup and this write leaves the row
    /// destroyed — the write lands on an orphaned `Arc` nobody can
    /// `resolve()` to anymore, never a resurrection.
    ///
    /// The registry's WRITE lock is held across the whole move, so two
    /// concurrent declarations of one handle serialise and the later one
    /// wins; a cell is locked only for one clone or one write, so taking
    /// cells under the registry lock cannot deadlock.
    pub fn set_agent_handle(&self, workspace_id: &str, handle: &str) -> Option<(Arc<Workspace>, Vec<String>)> {
        let g = self.inner.write().expect("workspaces lock");
        let ws = g.by_id.get(workspace_id)?.clone();
        let mut moved = Vec::new();
        if !handle.is_empty() {
            for (id, other) in g.by_id.iter() {
                if id == workspace_id {
                    continue;
                }
                let mut cell = other.agent_handle.lock().unwrap_or_else(|e| e.into_inner());
                if *cell == handle {
                    cell.clear();
                    moved.push(id.clone());
                }
            }
        }
        *ws.agent_handle.lock().unwrap_or_else(|e| e.into_inner()) = handle.to_string();
        Some((ws, moved))
    }

    /// Boot's side of one row per handle (ADR 0049): every non-empty
    /// declared handle that two or more rows hold stays only on the row
    /// `keep(handle)` names (a workspace id) and is cleared on the rest; a
    /// `keep` that names none of them clears it on all. `keep` runs with
    /// no lock held; the clears run under the registry's write lock, like
    /// [`set_agent_handle`](Self::set_agent_handle)'s. Returns each such
    /// handle with the rows it was cleared on, for the caller to save.
    pub(crate) fn clear_shared_handles(
        &self,
        keep: impl Fn(&str) -> Option<String>,
    ) -> Vec<(String, Vec<Arc<Workspace>>)> {
        let mut by_handle: std::collections::BTreeMap<String, Vec<Arc<Workspace>>> = Default::default();
        for ws in self.list() {
            let h = ws.agent_handle();
            if !h.is_empty() {
                by_handle.entry(h).or_default().push(ws);
            }
        }
        let shared: Vec<(String, Vec<Arc<Workspace>>, Option<String>)> = by_handle
            .into_iter()
            .filter(|(_, rows)| rows.len() > 1)
            .map(|(h, rows)| {
                let kept = keep(&h);
                (h, rows, kept)
            })
            .collect();
        let _g = self.inner.write().expect("workspaces lock");
        let mut out = Vec::new();
        for (handle, rows, kept) in shared {
            let mut cleared = Vec::new();
            for ws in rows {
                if kept.as_deref() == Some(ws.workspace_id.as_str()) {
                    continue;
                }
                let mut cell = ws.agent_handle.lock().unwrap_or_else(|e| e.into_inner());
                if *cell == handle {
                    cell.clear();
                    cleared.push(ws.clone());
                }
            }
            out.push((handle, cleared));
        }
        out
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

    pub fn remove_by_id(&self, id: &str) -> Option<Arc<Workspace>> {
        let mut g = self.inner.write().expect("workspaces lock");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_insert_and_resolve_by_id_and_slug() {
        let reg = Workspaces::new();
        let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
        let id = ws.workspace_id.clone();
        reg.insert(ws);
        assert!(reg.resolve(Some(&id)).is_some());
        assert_eq!(reg.resolve(Some("alpha")).unwrap().slug, "alpha");
    }

    #[test]
    fn registry_default_resolves_when_id_missing() {
        let reg = Workspaces::new();
        let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
        let id = ws.workspace_id.clone();
        reg.insert(ws);
        reg.set_default(&id);
        assert_eq!(reg.resolve(None).unwrap().slug, "alpha");
    }

    /// ADR 0046 decision 1: `agent.join`'s persistence target. Mirrors
    /// `reset_agent_to_none_makes_a_carried_over_default_row_inert_again`'s
    /// shape — mutate through the registry, then prove BOTH the returned
    /// handle and a fresh `resolve()` see it, and that unrelated metadata
    /// (here `runtime`) survives the `insert()` round trip unchanged.
    #[test]
    fn set_agent_handle_persists_in_the_registry_and_preserves_other_fields() {
        let reg = Workspaces::new();
        let mut row = Workspace::from_label(
            "capsuleprobe",
            PathBuf::from("/home/u/capsuleprobe"),
            false,
            "claude".into(),
            String::new(),
            String::new(),
        );
        row.runtime = "capsule".to_string();
        let row = reg.insert(row);
        let original_id = row.workspace_id.clone();
        assert_eq!(row.agent_handle(), "", "a fresh row has never joined");

        let joined = reg
            .set_agent_handle(&row.workspace_id, "capsuleprobe-testhost")
            .expect("the row is registered")
            .0;
        assert_eq!(joined.workspace_id, original_id, "set_agent_handle must preserve the id");
        assert_eq!(joined.agent_handle(), "capsuleprobe-testhost");
        assert_eq!(joined.runtime, "capsule", "unrelated metadata must survive the update");

        // The registry's own row (not just the returned handle) reflects
        // the join -- resolve() must see it too.
        let resolved = reg.resolve(Some(&original_id)).unwrap();
        assert_eq!(resolved.agent_handle(), "capsuleprobe-testhost");
    }

    #[test]
    fn set_agent_handle_on_an_unknown_workspace_is_none() {
        let reg = Workspaces::new();
        assert!(reg.set_agent_handle("nope", "someone").is_none());
    }

    #[test]
    fn set_agent_handle_mutates_the_same_arc_never_a_replacement() {
        // Manager review (S6, Codex finding B5): the row's resource caches
        // live on the SAME struct instance as agent_handle -- a guarded
        // in-place update must never discard them by reconstructing the
        // Workspace. Constructing the kernel handle BEFORE the join, then
        // checking it is still built afterward, proves no replacement
        // happened (a `meta_only`-rebuilt row would start with a cold,
        // unbuilt kernel cache).
        let reg = Workspaces::new();
        let row = Workspace::from_label(
            "capsuleprobe2",
            PathBuf::from("/home/u/capsuleprobe2"),
            false,
            "claude".into(),
            String::new(),
            String::new(),
        );
        let row = reg.insert(row);
        let _ = row.kernel();
        assert!(row.kernel_built(), "test setup: the kernel cache must be built before the join");

        let joined = reg.set_agent_handle(&row.workspace_id, "capsuleprobe2-testhost").unwrap().0;
        assert!(
            joined.kernel_built(),
            "a guarded in-place update must never discard a live resource cache"
        );
        assert!(
            Arc::ptr_eq(&row, &joined),
            "set_agent_handle must mutate the SAME Arc, never hand back a replacement"
        );
    }

    #[test]
    fn set_agent_handle_after_the_row_is_destroyed_never_resurrects_it() {
        // Manager review (S6, Codex finding B5): the old replace-based
        // implementation read the row, then reinserted a fresh copy --
        // a destroy landing between those two steps got resurrected by
        // the reinsert. The guarded in-place version never touches
        // by_id/by_slug at all, so this is structurally impossible: a
        // destroyed row simply has no entry for a later join to find.
        let reg = Workspaces::new();
        let row = Workspace::from_label(
            "capsuleprobe3",
            PathBuf::from("/home/u/capsuleprobe3"),
            false,
            "claude".into(),
            String::new(),
            String::new(),
        );
        let row = reg.insert(row);
        let id = row.workspace_id.clone();

        reg.remove_by_id(&id);
        assert!(reg.resolve(Some(&id)).is_none(), "test setup: the row must actually be gone");

        assert!(
            reg.set_agent_handle(&id, "capsuleprobe3-testhost").is_none(),
            "a join against a destroyed workspace_id must report unknown_workspace, never resurrect the row"
        );
        assert!(
            reg.resolve(Some(&id)).is_none(),
            "the destroyed row must stay gone -- no resurrection"
        );
    }

    #[test]
    fn registry_resolve_unknown_is_none() {
        let reg = Workspaces::new();
        assert!(reg.resolve(Some("nope")).is_none());
        assert!(reg.resolve(None).is_none());
    }

    #[test]
    fn registry_default_id_round_trip() {
        let reg = Workspaces::new();
        assert!(reg.default_id().is_none());
        let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
        let id = ws.workspace_id.clone();
        reg.insert(ws);
        reg.set_default(&id);
        assert_eq!(reg.default_id().as_deref(), Some(id.as_str()));
    }

    #[test]
    fn registry_reinsert_preserves_workspace_id() {
        let reg = Workspaces::new();
        let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
        let original_id = ws.workspace_id.clone();
        reg.insert(ws);
        // Same slug, different label — id should stay the same.
        let again = Workspace::from_label("alpha", PathBuf::from("/p/alpha-renamed"), false, "none".into(), String::new(), String::new());
        reg.insert(again);
        let resolved = reg.resolve(Some("alpha")).unwrap();
        assert_eq!(resolved.workspace_id, original_id);
        assert_eq!(resolved.project_root, PathBuf::from("/p/alpha-renamed"));
    }

    // The default row's boot re-seed (`seed_default_row`) carries the row's
    // declared handle into a fresh `Workspace` and re-inserts it; the
    // same-slug arm used to blank it through `meta_only`.
    #[test]
    fn a_same_slug_reinsert_takes_the_new_rows_declared_handle() {
        let reg = Workspaces::new();
        let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
        let original_id = ws.workspace_id.clone();
        reg.insert(ws);
        reg.set_agent_handle(&original_id, "m5-carried").expect("registered");

        let mut carried = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
        carried.agent_handle = Mutex::new("m5-carried".to_string());
        reg.insert(carried);
        let resolved = reg.resolve(Some("alpha")).unwrap();
        assert_eq!(resolved.workspace_id, original_id);
        assert_eq!(resolved.agent_handle(), "m5-carried");

        let fresh = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
        reg.insert(fresh);
        assert_eq!(reg.resolve(Some("alpha")).unwrap().agent_handle(), "");
    }

    /// ADR 0042 slice L1a: `insert`'s own doc says the id-preserving
    /// re-insert (same slug) takes "the REST of the metadata" from the
    /// NEW `ws` — `runtime` must be no exception. Before `insert`'s own
    /// fix, the reconstruction branch called `Workspace::meta_only`
    /// (which always defaults `runtime` to "tmux" internally) without
    /// threading the incoming `ws.runtime` through at all, so EVERY
    /// same-slug reinsert silently reset it to "tmux" regardless of what
    /// the caller passed — exactly the "id-preserving refresh" a second
    /// `workspace.create` for an existing capsule workspace's slug is
    /// (`rows/ops/create.rs`'s own duplicate-root gate comment names this case),
    /// which still sets `runtime = "capsule"` on every call.
    #[test]
    fn registry_reinsert_takes_the_new_runtime_not_metas_default() {
        let reg = Workspaces::new();
        let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
        reg.insert(ws);
        let mut again = Workspace::from_label("alpha", PathBuf::from("/p/alpha-renamed"), false, "none".into(), String::new(), String::new());
        again.runtime = "capsule".to_string();
        reg.insert(again);
        let resolved = reg.resolve(Some("alpha")).unwrap();
        assert_eq!(resolved.runtime, "capsule");
    }

    #[tokio::test]
    async fn remove_by_id_aborts_the_row_s_observer_task() {
        let reg = Workspaces::new();
        let ws = Workspace::from_label("obs", PathBuf::from("/p/obs"), false, "none".into(), String::new(), String::new());
        let id = ws.workspace_id.clone();
        reg.insert(ws);
        assert!(!reg.has_observer(&id), "no observer installed yet");

        let ticks = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let ticks_for_task = ticks.clone();
        let handle = tokio::spawn(async move {
            loop {
                ticks_for_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        });
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled_for_closure = cancelled.clone();
        let cancel: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            cancelled_for_closure.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        reg.install_observer(&id, handle, cancel);
        assert!(reg.has_observer(&id));

        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(ticks.load(std::sync::atomic::Ordering::SeqCst) > 0, "the task must actually be running");

        reg.remove_by_id(&id);
        assert!(!reg.has_observer(&id), "removal must forget the handle");
        assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst), "removal must call the paired cancel closure too");

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let after_removal = ticks.load(std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(
            ticks.load(std::sync::atomic::Ordering::SeqCst),
            after_removal,
            "the observer task must stop ticking once its row is removed"
        );
    }
}
