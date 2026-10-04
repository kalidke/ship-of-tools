//! Replies that build the nav trees: tree.root and tree.children (Files); project.scan,
//! file.parse and function.methods (Modules). A reply for a tree not on screen goes
//! to that tree's own slot.

use crate::ui::*;

impl State {
    pub(crate) fn on_tree_root(
        &mut self,
        event_host: HostKey,
        workspace_id: Option<String>,
        root: sot_protocol::TreeNode,
        children: Vec<sot_protocol::TreeNode>,
    ) {
        // Route by the REPLY's key. Every tree.root this chrome
        // fires is a Files root, so the reply keys as
        // (Files, reply workspace). A reply for a key we're not
        // currently viewing — a stale in-flight root after a
        // switch, the connect-time default fetch, a files root
        // while another mode is up — installs into ITS OWN slot
        // instead of being dropped (old behavior) or clobbering
        // the active view (the original 2026-05-29 desync). The
        // active-only side-effects below (cursor defaults,
        // reveals, capture one-shots) don't apply to a parked
        // tree.
        let reply_key: TreeKey = (
            Mode::Files,
            TreeScope::Workspace((
                event_host.clone(),
                self.reply_ws_key(workspace_id.as_deref()),
            )),
        );
        if reply_key != self.active_tree_key() {
            // Park ONLY into an empty slot: a root-only rebuild
            // is STRICTLY POORER than a parked expanded tree
            // (repro: reconnect arms a restore for ws A, user
            // switches to B before A's root lands — replacing
            // A's expanded slot with a root-only view loses the
            // expansion AND suppresses the return-visit refetch,
            // since non-empty slots skip the loader). A richer
            // parked tree wins; drop the reply like the old
            // guard did.
            let slot = self.tree_store.slot_mut(reply_key.clone());
            if slot.view.rows.is_empty() || slot.from_reply {
                // Empty, or holding an EARLIER parked reply —
                // replies are server-ordered, newest wins (the
                // double-toggle race, codex r5). Only user-
                // stashed state (from_reply=false) is sacred.
                tracing::info!(
                    ?reply_key,
                    "tree.root parked into its slot (not the active view)"
                );
                slot.view.set_root(root, children);
                slot.from_reply = true;
            } else {
                tracing::info!(
                    ?reply_key,
                    "tree.root dropped — parked slot holds user state"
                );
            }
            return;
        }
        // TRACED (2026-07-10 nav-collapse diagnosis): set_root
        // rebuilds the whole tree (expansion lost) — every
        // request origin is traced too, so an unexpected
        // collapse names its trigger in the log.
        tracing::info!(
            rows_before = self.tree.rows.len(),
            new_children = children.len(),
            "tree.root applied — nav tree rebuilt (set_root)"
        );
        self.tree.set_root(root, children);
        // Reconnect nav restore (2026-07-11): if this root is the
        // hello/reconnect rebuild, re-reveal the pre-reconnect
        // cursor through the reveal machinery — its ancestor path
        // re-expands level by level and the cursor lands back on
        // the exact row (preview re-anchors on landing). Consumed
        // once; discarded when the workspace changed in between
        // (a switch's fresh tree must not chase the old path).
        if let Some((armed_ws, sel)) = self.restore_nav_after_resume.take() {
            if armed_ws == self.active_workspace_id {
                tracing::info!(target = %sel,
                    "resume: restoring nav cursor after reconnect rebuild");
                self.driven_preview_hold_cursor = self
                    .tree
                    .rows
                    .get(self.tree.selected)
                    .map(|r| r.node.id.clone());
                self.pending_reveal = Some(sel);
                self.reveal_awaiting = None;
                self.reveal_refetched = None;
                self.drive_reveal_step(None);
            } else {
                tracing::info!(?armed_ws, active = ?self.active_workspace_id,
                    "resume: nav restore discarded — workspace changed before the root arrived");
            }
        }
        // Restore the nav cursor persisted across an ADR-0017
        // relaunch, best-effort: select the saved node id if it's
        // present in the freshly loaded tree (one-shot, gated by
        // the workspace check above so it only lands in the
        // matching workspace's tree). A deeply-collapsed node that
        // isn't loaded yet just leaves the default cursor. A
        // CLI --start-selected below still overrides this.
        let mut resume_landed = false;
        if let Some((id, scroll)) = self.pending_resume_nav.take() {
            if let Some(idx) = self.tree.rows.iter().position(|r| r.node.id == id) {
                self.tree.selected = idx;
                self.tree_scroll = scroll;
                resume_landed = true;
            }
        }
        // Fresh session (no resume landed): default the first
        // Files-mode cursor to the project README so the preview
        // opens onto rendered docs instead of the root row.
        // The one-shot is consumed on the FIRST Files tree.root
        // either way, so later refreshes never yank the cursor.
        // `--start-selected` below still overrides.
        // `--capture-preview` runs skip the README default: it
        // moves the cursor, and the cursor-driven readme fetch
        // then lands after (and clobbers) the captured preview —
        // the first-row "pretend we already fired" suppression
        // below only holds while the cursor stays on row 0.
        let ws_key = self.active_ws_key();
        let readme_default = matches!(self.mode, Mode::Files)
            && self.capture_preview.is_none()
            && self.nav_readme_defaulted.insert(ws_key);
        if !resume_landed && readme_default {
            if let Some(idx) = self
                .tree
                .rows
                .iter()
                .position(|r| r.node.label.eq_ignore_ascii_case("readme.md"))
            {
                self.tree.selected = idx;
            }
        }
        self.consume_files_root_one_shots();
    }

    fn consume_files_root_one_shots(&mut self) {
        // Only consume the start-selected one-shot if this
        // event matches our startup mode; otherwise the files-
        // mode `tree.root` that always fires at connect would
        // eat the selection meant for the modules tree.
        if matches!(self.mode, Mode::Files) {
            if let Some(n) = self.pending_initial_selection.take() {
                self.tree.selected = n.min(self.tree.rows.len().saturating_sub(1));
            }
            if let Some(rel) = self.capture_preview.take() {
                let node_id = format!("files:{rel}");
                tracing::info!(%node_id, "firing --capture-preview");
                let (fit_w, fit_h) = self.preview_fit_px();
                let generation = self.next_preview_gen();
                if let Err(e) = self.send(crate::transport::OutgoingReq::PreviewGet {
                    node_id: node_id.clone(),
                    workspace_id: None,
                    page: None,
                    fit_w,
                    fit_h,
                    generation,
                }) {
                    tracing::warn!(error = %e, %node_id, "drop --capture-preview request — channel closed");
                }
                // Record the REAL fired node id. The cursor-
                // driven auto-fire race this used to paper over
                // (by pretending the root row was fired) is now
                // killed at the source — maybe_fire_preview
                // stands down while capture_preview_armed. The
                // honest id matters: the pane title and the
                // ADR-0021 page transport (PgDn/PgUp/n/p re-fire
                // the *shown* node) both read it — the root-row
                // lie made a page turn re-fetch the root preview
                // and clobber the captured node.
                self.preview_node_id_fired = Some(node_id);
            }
            // #4 fix (cursor-reveal-on-switch): a preview driven via
            // a workspace switch armed a one-shot reveal before this
            // workspace's rows existed. They're loaded now — land the
            // cursor on the driven file. `drive_reveal_step` lands a
            // top-level row directly and expands ancestors for a
            // nested one. Runs after the resume/README cursor
            // defaults above so the explicit switch-reveal wins.
            if let Some(node_id) = self.pending_switch_reveal.take() {
                // Hold the per-frame preview-follow off the just-applied
                // README/default cursor while this deep reveal lands, so
                // `maybe_fire_preview` can't clobber the driven badge
                // preview with README (the post-relaunch badge-consume
                // race — two repros 2026-06-30). Mirrors
                // `drive_same_ws_open`; cleared on landing.
                if !self.tree.rows.iter().any(|r| r.node.id == node_id) {
                    self.driven_preview_hold_cursor = self
                        .tree
                        .rows
                        .get(self.tree.selected)
                        .map(|r| r.node.id.clone());
                }
                self.pending_reveal = Some(node_id);
                self.reveal_awaiting = None;
                self.reveal_refetched = None;
                self.drive_reveal_step(None);
            }
        }
    }

    pub(crate) fn on_tree_children(
        &mut self,
        event_host: HostKey,
        workspace_id: Option<String>,
        parent_id: String,
        children: Vec<sot_protocol::TreeNode>,
    ) {
        // Route by the reply's key, like TreeRoot: a lazy-expand
        // reply for a (workspace, mode) we're no longer viewing
        // splices into ITS OWN slot, not the active view.
        let reply_key: TreeKey = (
            Mode::Files,
            TreeScope::Workspace((
                event_host.clone(),
                self.reply_ws_key(workspace_id.as_deref()),
            )),
        );
        if reply_key != self.active_tree_key() {
            tracing::info!(?workspace_id, active = ?self.active_workspace_id,
                %parent_id, "tree.children parked into its own slot");
            self.tree_store
                .slot_mut(reply_key)
                .view
                .apply_children(&parent_id, children);
            // No reveal-abort here: switch_to_workspace clears the
            // reveal bookkeeping, so an armed reveal's awaited
            // parent always belongs to the ACTIVE key — a parked
            // reply can never be the awaited one (a same-string
            // parent_id from another workspace is a different
            // node; aborting on it was the cross-key bug).
            return;
        }
        self.tree.apply_children(&parent_id, children);
        // Advance an in-flight deep-path reveal: this splice may have
        // just made the next ancestor (or the target row) visible.
        // No-op when no reveal is armed.
        if self.pending_reveal.is_some() {
            tracing::info!(%parent_id, "reveal: re-entering after children splice");
        }
        self.drive_reveal_step(Some(&parent_id));
    }

    pub(crate) fn on_tree_children_failed(
        &mut self,
        event_host: HostKey,
        workspace_id: Option<String>,
        parent_id: String,
        error: String,
    ) {
        // Key-gate: a failed expand for a PARKED workspace's tree
        // is not the active view's problem — and its parent_id
        // could string-match an ACTIVE reveal's awaited parent
        // (same relative path in another project), which must not
        // abort that reveal. Trace and move on.
        let reply_key: TreeKey = (
            Mode::Files,
            TreeScope::Workspace((
                event_host.clone(),
                self.reply_ws_key(workspace_id.as_deref()),
            )),
        );
        if reply_key != self.active_tree_key() {
            tracing::info!(?workspace_id, %parent_id, %error,
                "tree.children failure for a non-active slot — ignored");
            return;
        }
        // A tree.children request errored (backend error frame or
        // parse failure). Surface it and abort any reveal waiting
        // on this parent — previously this was warn-and-drop in
        // the transport and the reveal starved silently.
        tracing::info!(%parent_id, %error, "tree.children FAILED");
        self.status = format!("tree expand failed · {parent_id}: {error}");
        let refetch_gated = self
            .reveal_refetched
            .as_ref()
            .is_some_and(|(_, anc)| anc == &parent_id);
        if self.reveal_awaiting.as_deref() == Some(parent_id.as_str()) || refetch_gated
        {
            // Covers BOTH wait states (codex round 4): a failed
            // reply for the awaited level OR for the force-
            // refreshed ancestor would otherwise leave the reveal
            // gated forever (only that dir's reply advances the
            // walk now).
            tracing::info!(%parent_id, "reveal: aborted — awaited children failed");
            self.pending_reveal = None;
            self.reveal_awaiting = None;
            self.reveal_refetched = None;
        }
        self.window.request_redraw();
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn on_project_scan(
        &mut self,
        event_host: HostKey,
        workspace_id: Option<String>,
        project_root: Option<String>,
        package_name: Option<String>,
        entry_file: Option<String>,
        modules: Vec<crate::transport::ScanModule>,
        generation: u64,
    ) {
        // `kernel.request` runs off-loop (switch-latency): two
        // scans fired close together for the SAME (host,
        // workspace) can complete in EITHER order now, so —
        // exactly like `preview.get`'s `reply_is_current` —
        // drop one that isn't the latest generation issued for
        // its own key. Per-key (not global) because scans for
        // DIFFERENT workspaces are independently valid in
        // flight together; see `next_project_scan_gen`.
        let latest = self
            .project_scan_req_gen
            .get(&(event_host.clone(), workspace_id.clone()))
            .copied()
            .unwrap_or(0);
        if generation != latest {
            tracing::debug!(?workspace_id, generation, latest, %event_host,
                "drop stale project.scan reply");
            return;
        }
        tracing::info!(
            ?workspace_id,
            ?project_root,
            ?package_name,
            ?entry_file,
            module_count = modules.len(),
            type_count = modules.iter().map(|m| m.types.len()).sum::<usize>(),
            fn_count = modules.iter().map(|m| m.functions.len()).sum::<usize>(),
            "project.scan reply"
        );
        // Route by the reply's key (the set_flat hole, closed): a
        // Modules scan that isn't for the active (Modules, ws)
        // lands in its own slot — it can no longer replace
        // another workspace's tree, or ANY tree while Files mode
        // is up. `scan_project_root` rides the slot so the
        // parked tree keeps the root it was scanned against.
        let rows = scan_to_tree_rows(&modules);
        let reply_key: TreeKey = (
            Mode::Modules,
            TreeScope::Workspace((
                event_host.clone(),
                self.reply_ws_key(workspace_id.as_deref()),
            )),
        );
        if reply_key != self.active_tree_key() {
            tracing::info!(?reply_key, active = ?self.active_tree_key(),
                "project.scan parked into its own slot (not the active view)");
            let slot = self.tree_store.slot_mut(reply_key);
            slot.view.set_flat(rows);
            slot.scan_project_root = project_root;
            slot.from_reply = true;
            return;
        }
        self.scan_project_root = project_root;
        self.tree.set_flat(rows);
        // Key match implies Modules mode — the old mode gate on
        // this consume is subsumed.
        if let Some(n) = self.pending_initial_selection.take() {
            self.tree.selected = n.min(self.tree.rows.len().saturating_sub(1));
        }
    }

    pub(crate) fn on_file_parse_failed(
        &mut self,
        event_host: HostKey,
        workspace_id: Option<String>,
        path: String,
    ) {
        // Record the failure; the retry gate in
        // maybe_fire_concept_read re-arms after a backoff. Do
        // NOT un-latch `file_parse_fired` here — an instant
        // un-latch let the redraw loop re-fire every frame
        // against a fast-failing kernel (the ~4.7k req/s storm).
        //
        // Ws-gated like the FileParsed success path (codex r3);
        // host-qualified too (ADR 0042 L2a) for the same
        // reasoning -- the counter is keyed by workspace-RELATIVE
        // path, so a late failure fired for another workspace
        // (or another HOST's colliding path) would advance THIS
        // workspace's backoff (or hit its retry cap) for a path
        // it never parsed.
        let failed_ws_key: WsKey = (
            event_host.clone(),
            self.reply_ws_key(workspace_id.as_deref()),
        );
        if failed_ws_key == self.active_ws_key() {
            let e = self
                .file_parse_retry
                .entry(path)
                .or_insert((std::time::Instant::now(), 0));
            e.0 = std::time::Instant::now();
            e.1 += 1;
        }
    }

    pub(crate) fn on_file_parsed(
        &mut self,
        event_host: HostKey,
        workspace_id: Option<String>,
        path: String,
        ast_hash: String,
        definitions: Vec<crate::transport::DefinitionInfo>,
    ) {
        let reply_ws = self.reply_ws_key(workspace_id.as_deref());
        // ADR 0042 L2a: host-qualified -- two hosts can each
        // have a workspace at the same slug, and the drift
        // check's collision concern below (a shared relative
        // path across two PROJECTS) applies at least as much
        // across two HOSTS.
        let reply_ws_key: WsKey = (event_host.clone(), reply_ws);
        // Drift-badge bookkeeping is ACTIVE-workspace state (both
        // maps are per-workspace snapshotted), and the drift
        // check's `path` is workspace-RELATIVE (`files:` strip) —
        // so a late reply fired for another workspace could
        // insert a COLLIDING relative path (both projects have a
        // `src/lib.jl`) into this workspace's map and fake its
        // drift verdict. Gate on the reply's workspace. The
        // skipped insert isn't lost: the owning workspace's
        // restore drops hash-less fire-latches and re-fires.
        if reply_ws_key == self.active_ws_key() {
            self.file_parse_retry.remove(&path);
            self.file_ast_hashes.insert(path.clone(), ast_hash);
        }
        // If a module row's payload.path matches, synthesize
        // child TreeNodes from the parsed definitions and
        // splice. Files-mode drift-detection callers ignore
        // `definitions` (they just want ast_hash); modules-mode
        // expansion callers consume it here. Same wire shape,
        // both consumers happy. Routed by the reply's TREE key:
        // the splice lands in the active view only when
        // (Modules, reply host+ws) is what's on screen; otherwise
        // in that key's parked slot — module `path`s are
        // absolute, but two workspaces CAN define the same
        // module file (a shared package checked out twice, or
        // now two hosts running the same project), and a
        // host-blind lookup would cross-splice them.
        let reply_key: TreeKey = (Mode::Modules, TreeScope::Workspace(reply_ws_key));
        let splice_active = reply_key == self.active_tree_key();
        let module_id = {
            let view = if splice_active {
                &self.tree
            } else {
                &self.tree_store.slot_mut(reply_key.clone()).view
            };
            view.rows
                .iter()
                .find(|r| {
                    r.node.kind == "module"
                        && r.node.payload.get("path").and_then(|v| v.as_str())
                            == Some(path.as_str())
                })
                .map(|r| r.node.id.clone())
        };
        if let Some(parent_id) = module_id {
            // Strip the `modules:` prefix to recover the module
            // name for col-3's `function.methods` call later.
            // The module's TreeNode lives at parent_id, so this
            // is the same string the kernel knows it by.
            let module_name = parent_id
                .strip_prefix("modules:")
                .unwrap_or(&parent_id)
                .to_string();
            let kids: Vec<TreeNode> = definitions
                .into_iter()
                .map(|d| {
                    // Function rows get has_children=true so
                    // Enter/Right fires `function.methods` for
                    // them. Module name rides on payload so the
                    // chrome doesn't have to re-parse the id.
                    // Non-function defs (struct, abstract, …)
                    // stay leaves for now.
                    let is_function = d.kind == "function";
                    let mut payload = serde_json::Map::new();
                    if is_function {
                        payload.insert(
                            "module".to_string(),
                            serde_json::Value::String(module_name.clone()),
                        );
                        payload.insert(
                            "name".to_string(),
                            serde_json::Value::String(d.name.clone()),
                        );
                    }
                    TreeNode {
                        id: format!("{parent_id}:{}", d.name),
                        label: format!("{} ({})", d.name, d.kind),
                        kind: d.kind,
                        has_children: is_function,
                        badges: Vec::new(),
                        payload,
                    }
                })
                .collect();
            if splice_active {
                self.tree.apply_children(&parent_id, kids);
            } else {
                self.tree_store
                    .slot_mut(reply_key)
                    .view
                    .apply_children(&parent_id, kids);
            }
        }
    }

    pub(crate) fn on_function_methods_received(
        &mut self,
        event_host: HostKey,
        workspace_id: Option<String>,
        module: String,
        name: String,
        methods: Vec<crate::transport::MethodInfo>,
    ) {
        // Find the function row whose id matches `modules:<mod>:<name>`.
        // The exact id is what we built when modules-col-2 splice
        // ran, so reconstruct it from the request echo. Routed by
        // the reply's tree key (same rationale as FileParsed's
        // splice — a same-named module in two workspaces must not
        // cross-splice); the existence check runs within the
        // ROUTED view.
        let parent_id = format!("modules:{module}:{name}");
        let reply_key: TreeKey = (
            Mode::Modules,
            TreeScope::Workspace((
                event_host.clone(),
                self.reply_ws_key(workspace_id.as_deref()),
            )),
        );
        let splice_active = reply_key == self.active_tree_key();
        let exists = {
            let view = if splice_active {
                &self.tree
            } else {
                &self.tree_store.slot_mut(reply_key.clone()).view
            };
            view.rows.iter().any(|r| r.node.id == parent_id)
        };
        if !exists {
            tracing::debug!(
                %parent_id,
                "function.methods reply for unknown row — ignoring"
            );
            return;
        }
        let kids: Vec<TreeNode> = methods
            .into_iter()
            .enumerate()
            .map(|(i, m)| {
                // `sig` is the standard `string(m)` repr, which
                // ends in ` @ <module> <file>:<line>`. Trim that
                // tail for the row label so the parameter
                // signature reads cleanly; the location lives
                // on payload for a future jump-to-line UX.
                let label = m
                    .sig
                    .split_once(" @ ")
                    .map(|(head, _)| head.to_string())
                    .unwrap_or(m.sig.clone());
                TreeNode {
                    id: format!("{parent_id}#{i}"),
                    label,
                    kind: "method".to_string(),
                    has_children: false,
                    badges: Vec::new(),
                    payload: Default::default(),
                }
            })
            .collect();
        if splice_active {
            self.tree.apply_children(&parent_id, kids);
        } else {
            self.tree_store
                .slot_mut(reply_key)
                .view
                .apply_children(&parent_id, kids);
        }
    }
}
