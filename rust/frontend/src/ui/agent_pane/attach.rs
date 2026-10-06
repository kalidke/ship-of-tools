//! The agent pane's attach client: dialing a row, pumping its events, and the warm pool of parked clients.

use super::*;

pub(in crate::ui) type PaneAttachClient = sot_log::attach_client::client::FeAttachClient<sot_protocol::topology::lane_client::DaemonLaneEndpoint>;

/// Attach clients parked when the session pane leaves their row, so a
/// switch back reuses the live lane instead of dialing again (a remote
/// dial is several tunnel round trips). Newest last; per-host bound =
/// that host's row count, capped at [`WARM_ATTACH_CAP`].
pub(in crate::ui) struct WarmAttachPool<C> {
    entries: Vec<((crate::net::dial::HostKey, String), C)>,
}

const WARM_ATTACH_CAP: usize = 16;

impl<C> WarmAttachPool<C> {
    pub(in crate::ui) fn new() -> Self {
        Self { entries: Vec::new() }
    }

    fn take(&mut self, key: &(crate::net::dial::HostKey, String)) -> Option<C> {
        let i = self.entries.iter().position(|(k, _)| k == key)?;
        Some(self.entries.remove(i).1)
    }

    /// Parks `client` under `key`; returns the clients evicted to keep
    /// the key's host within `bound` — the caller shuts those down.
    fn park(&mut self, key: (crate::net::dial::HostKey, String), client: C, bound: usize) -> Vec<C> {
        let mut evicted = Vec::new();
        if let Some(old) = self.take(&key) {
            evicted.push(old);
        }
        let host = key.0.clone();
        self.entries.push((key, client));
        let bound = bound.clamp(1, WARM_ATTACH_CAP);
        while self.entries.iter().filter(|(k, _)| k.0 == host).count() > bound {
            let i = self.entries.iter().position(|(k, _)| k.0 == host).expect("over bound");
            evicted.push(self.entries.remove(i).1);
        }
        evicted
    }

    /// Drops every entry of `host` whose row `is_live` rejects (destroyed
    /// rows); returns them for shutdown.
    fn retain_rows(&mut self, host: &crate::net::dial::HostKey, is_live: impl Fn(&str) -> bool) -> Vec<C> {
        let (dead, live): (Vec<_>, Vec<_>) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|(k, _)| &k.0 == host && !is_live(&k.1));
        self.entries = live;
        dead.into_iter().map(|(_, c)| c).collect()
    }
}

fn shutdown_detached(clients: Vec<PaneAttachClient>) {
    for mut c in clients {
        std::thread::spawn(move || {
            c.shutdown(std::time::Duration::from_millis(250));
        });
    }
}

impl State {
    /// Sessions-mode (ADR 0013) attach action (B3): re-target the BL pane
    /// to the selected session. For a `session` row, attach to that
    /// session's name; for a `pane` row, attach to the parent session
    /// (selecting a specific pane within the session is left to tmux's
    /// own default, which restores the most-recently-active pane).
    /// Sends a `pty.open` carrying the new target; backend kills the
    /// existing pty and respawns against the new session.
    ///
    /// `host` (ADR 0042 L2a) — the connection that owns `session_name`.
    /// Every caller either IS `switch_to_workspace` (which sets
    /// `active_host` to this same value immediately before calling here,
    /// so `self.active_host.clone()` there is correct BY CONSTRUCTION —
    /// not a default) or resolves it from the cursored row's own
    /// `payload.host` (the Sessions-Enter foreign-session fallback below):
    /// a row can be cursored without ever having been switched to, so
    /// `active_host` alone would be wrong there.
    ///
    /// ADR 0042 shrink round (rule A): drops any live `pane_attach_term`
    /// from the DEPARTING row, marks `pane_feed = Pending`, and ALWAYS
    /// routes through `send_to(&host, ...)` (ADR 0042 L2a — was
    /// `self.send`, i.e. `active_host`, before this row's own host was
    /// threaded through) — the daemon decides whether a capsule's
    /// supervisor needs starting via its `pty.open` reply, an
    /// `attach_direct` refusal carrying `PtyAttachDirect{target}` (the
    /// only kind this build's daemon sends), never a frontend-side
    /// cache-hit guess.
    pub(in crate::ui) fn attach_session_to_bl(&mut self, host: HostKey, session_name: String) {
        // ADR 0042 L2a codex review, item D: compare the OWNER pair, not
        // just the session name -- two hosts can both name a session
        // "sot-be-sot", and a bare-name compare made switching to the
        // same-named session on a DIFFERENT host early-return here,
        // leaving the pty open on the OLD host.
        if self.bl_pane_target.as_ref() == Some(&(host.clone(), session_name.clone())) {
            // Already attached — no-op rather than churn the pty.
            self.status = format!("already attached · {session_name}");
            self.window.request_redraw();
            return;
        }
        // A switch clears the departing row's discard count.
        self.pane_inputs_discarded = 0;
        // ADR 0042 shrink round (rule A): the frontend ALWAYS asks the
        // daemon first — no more cache-hit fast path that attached
        // straight from a cached `workspace_runtime` entry. That fast
        // path made the daemon's own decision (does this row's
        // supervisor even need starting? — start-on-attach) unreachable:
        // a cached "capsule, here's a state_dir" entry attached directly,
        // never going through `pty.open` at all, to a supervisor that was
        // never confirmed running. Every capsule attach now goes through
        // `pty.open` and resolves via the `PtyAttachDirect` handler below
        // — the daemon is the one place that decides whether a session
        // must start.
        //
        // Still drop any live client from the DEPARTING row first
        // ("deselecting detaches; a watcher leaving costs nothing") so a
        // switch away from a capsule row never leaks the old attach
        // client, and set `pane_feed = Pending`: the attach is still
        // async until the daemon's `pty.open` reply (an `attach_direct`
        // refusal, the only kind this build's daemon sends) lands.
        //
        // LU6a: before dropping it, capture whatever the DEPARTING feed
        // was painting into `pane_hold` — the capsule client's own
        // screen — so the pane keeps showing that until the new client's
        // checkpoint lands, rather than the new client's freshly-
        // constructed, still-empty parser (`pane_screen_choice`'s own
        // doc). A departing `Pending` feed (no client yet) leaves
        // whatever hold is already there alone — nothing new was ever
        // painted for it to replace.
        // LU6a design-review amendment: the "since request" clock
        // starts HERE for an ordinary switch — or, for a switch that
        // is really the tail of a workspace CREATE
        // (`switch_to_workspace` from the `WorkspaceCreated` reply),
        // inherits the earlier stamp `commit_workspace_create` took
        // when it sent `workspace.create`, so the daemon's create
        // round trip counts toward the acceptance metric too. Taken,
        // not merely read, so it can't leak into a later, unrelated
        // switch.
        self.pane_attach_requested_at = Some(
            self.pending_capsule_create_requested_at
                .take()
                .unwrap_or_else(std::time::Instant::now),
        );
        if let Some(mut t) = self.pane_attach_term.take() {
            self.pane_hold = Some(HeldPaneScreen(t.screen().clone()));
            match self.bl_pane_target.clone() {
                Some(key) if t.is_checkpointed() && !t.is_dead() => self.park_warm_attach(key, t),
                _ => {}
            }
        }
        // SHOULD-FIX (Codex review, lane B5 discharge): a dial
        // configuration error belongs to the DEPARTING row only — the
        // row we're switching to gets its own fresh attempt.
        self.pane_dial_error = None;
        self.pane_feed = PaneFeed::Pending;
        let (cols, rows) = self.pty_size.unwrap_or((80, 24));
        if let Err(e) = self.send_to(
            &host,
            crate::net::transport::OutgoingReq::PtyOpen {
                cols,
                rows,
                target: Some(session_name.clone()),
                // #5 guard: attach_session_to_bl is reached only via an explicit
                // user workspace-switch (switch_to_workspace / Sessions-mode
                // attach), so this open IS allowed to re-target the foreground pty.
                user_switch: true,
            },
        ) {
            tracing::warn!(error = %e, %session_name, "drop pty.open re-target request — channel closed");
            return;
        }
        self.bl_pane_target = Some((host, session_name.clone()));
        self.status = format!("attached BL → {session_name}");
        // Claude boot is owned by the BE tmux start-command wrapper
        // (`boot_wrapper_command`, ADR 0023 unified spawn): every
        // `autostart_claude` workspace is created with the wait-for-attach
        // wrapper as its pane command, so claude `exec`s the instant THIS
        // attach flips `session_attached>0` — no FE-typed `ccb`, no prompt
        // race. The old FE autostart-on-attach launch (pending_autostart →
        // advance_autostart_scan → autostart_claude_in_pane) is retired; the
        // single boot path is the wrapper.
        self.window.request_redraw();
        self.persist_resume_state();
    }

    /// ADR 0042 slice L1b, revised by ADR 0045 decision 1: constructs the
    /// session pane's OWN attach client through `host`'s own daemon —
    /// never a supervisor socket or a state-dir path directly. This
    /// client's `fe_down_last_evidence` is always `None` (this client's
    /// markers are the drawer's alone per ADR 0042 L1's own "fe_down
    /// markers ... NOT written" — `FeDownBaseline::capture(None)` makes
    /// `marker_for_attach` return `None` forever, so this client
    /// never produces one), and the initial size is the
    /// CALLER's `(cols, rows)` (the session pane's current rect) not the
    /// drawer's 80×24 default — a freshly-selected row should show at
    /// its real size on the very first paint, not resize a frame later.
    ///
    /// ADR 0042 slice L1b fix 1: unconditionally drops any existing
    /// `pane_attach_term` BEFORE constructing the new one — the single
    /// enforcement point for "never two clients alive," independent of
    /// caller discipline. Constructing first and only then overwriting
    /// the field would briefly hold two live clients (two connections,
    /// two controller ids) against possibly the same lane, which is what
    /// let `attach_session_to_bl`'s own pre-drop and the
    /// `PtyAttachDirect` handler (which had no pre-drop of its own)
    /// disagree before this fix.
    ///
    /// ADR 0042 slice L1b fix 2: returns whether the client actually
    /// started — every caller must check this rather than assume success,
    /// so a spawn failure's `self.status` (set here) is never immediately
    /// overwritten by an "attached" message.
    ///
    /// Switch-latency Phase 1, item 2: a bare `self.pane_attach_term =
    /// None` only runs `Drop`, which SENDS the old client's `Shutdown` but
    /// never waits for its worker thread to act on it — the departing
    /// worker can keep talking to the daemon for an unbounded time after,
    /// overlapping the replacement client's own connection on the SAME
    /// capsule lane. `FeAttachClient::shutdown(wait)` is the fix already
    /// on offer here (same send, then blocks polling the worker's
    /// `JoinHandle` up to `wait`) — but calling it inline, synchronously,
    /// would stall every capsule switch by up to `wait`, directly working
    /// against the keypress→paint metric this lane exists to shrink,
    /// which is why the wait is moved to a detached helper thread rather
    /// than paid for on the switch path. Only the SEND has to happen
    /// before the replacement client dials in, and it effectively does:
    /// a thread spawn plus one channel send costs low-single-digit
    /// microseconds, versus the real pipe connect + handshake
    /// `FeAttachClient::attach` below has to do — in practice `Shutdown`
    /// reaches the old worker well before the new one could plausibly
    /// finish connecting, without formally blocking this thread on it. If
    /// the old worker is unusually slow to exit, `shutdown`'s own 250ms
    /// timeout just warns (on the helper thread) and moves on — the
    /// worker is not joined, but per its own doc that never leaks the
    /// thread, it just finishes on its own.
    pub(in crate::ui) fn spawn_pane_attach_term(&mut self, host: &HostKey, target: &str, cols: u16, rows: u16) -> bool {
        if let Some(mut old) = self.pane_attach_term.take() {
            std::thread::spawn(move || {
                old.shutdown(std::time::Duration::from_millis(250));
            });
        }
        if let Some(mut c) = self.warm_attach.take(&(host.clone(), target.to_string())) {
            if c.is_dead() {
                shutdown_detached(vec![c]);
            } else {
                if c.screen().size() != (rows, cols) {
                    c.resize(cols, rows);
                }
                let since_request_ms = self
                    .pane_attach_requested_at
                    .map(|s| s.elapsed().as_millis() as u64)
                    .unwrap_or(0);
                tracing::info!(since_request_ms, "session pane: warm capsule client reused");
                c.set_viewed(true);
                if c.status_line() == "attached" {
                    self.pane_inputs_discarded = 0;
                }
                self.pane_attach_term = Some(c);
                self.pane_dial_error = None;
                self.pane_hold = None;
                self.pane_attach_started_at = Some(std::time::Instant::now());
                self.pane_attach_episode_warnings = 0;
                self.pane_attach_presented = false;
                return true;
            }
        }
        // SHOULD-FIX (Codex review, lane B5 discharge): distinguish a
        // genuine configuration error (persistent, no auto-retry — set
        // `pane_dial_error`) from "the control connection for this host
        // hasn't resolved a transport yet" (transient — a `Connected` evt
        // may land any moment; leaves `pane_dial_error` untouched so the
        // ordinary reconnect retry, gated on it below, stays enabled).
        let Some(config) = self.hosts.host_transports.get(host) else {
            let msg = format!("'{host}' has no transport configured");
            self.pane_dial_error = Some(msg.clone());
            self.status = msg;
            self.pane_hold = None;
            return false;
        };
        let Some(resolved) = self.hosts.host_resolved_dial.get(host).cloned() else {
            self.status = format!("'{host}' not yet connected — capsule attach waiting");
            self.pane_hold = None;
            return false;
        };
        // ADR 0045 decision 1 (Codex review): dials the SAME endpoint the
        // control transport already resolved for this host — never a
        // second, independent preference guess (`lane_dial`'s own doc).
        let gate = self.hosts.link_gates.entry(host.clone()).or_default().clone();
        let Some((dial, token)) = lane_dial(config, resolved, &gate) else {
            let msg = format!("'{host}' has no usable transport for its resolved connection");
            self.pane_dial_error = Some(msg.clone());
            self.status = msg;
            self.pane_hold = None;
            return false;
        };
        let endpoint = sot_protocol::topology::lane_client::DaemonLaneEndpoint::new(dial, token);
        // Invariant: the record names the frontend that typed —
        // `fe_instance_component`'s own doc.
        let controller_id = format!("{}#{}", self_comm_handle(), frontend_identity().instance);
        let fe_down_to = self_comm_handle();
        let waker = self.window.clone();
        match sot_log::attach_client::client::FeAttachClient::attach(
            endpoint,
            target.to_string(),
            cols,
            rows,
            controller_id,
            fe_down_to,
            None,
            Box::new(move || waker.request_redraw()),
        ) {
            Ok(c) => {
                tracing::info!("session pane: capsule attach client attaching");
                self.pane_attach_term = Some(c);
                self.pane_dial_error = None;
                // LU6a: the base every attach-outcome log line in
                // `pump_pane_attach_term` reports `since_client_ms`
                // against, and a fresh count for this client's own
                // episode warnings.
                self.pane_attach_started_at = Some(std::time::Instant::now());
                self.pane_attach_episode_warnings = 0;
                self.pane_attach_presented = false;
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "session pane: capsule attach client failed to start");
                self.status = format!("session attach failed: {e}");
                // LU6a: the attach itself failed — a hold captured for
                // this switch has nothing left to wait for (see
                // `pane_hold`'s own doc: "never left to outlive the
                // switch it belongs to").
                self.pane_hold = None;
                false
            }
        }
    }

    /// ADR 0042 slice L1b: drains checkpoint/output/notice/status/
    /// terminal events from the session pane's `pane_attach_term` —
    /// mirrors `pump_attach_term`'s own drain/status contract. The
    /// scrollback offset is the emulator's own (see `scroll_ring`), so
    /// pumping never touches it. Called EVERY redraw regardless of what's on screen
    /// (the session pane, unlike the drawer, is always visible — there is
    /// no visibility gate to mirror here).
    ///
    /// Deliberately does NOT call `should_exit()`/`drain_fe_down_markers()`:
    /// this client is never asked to quit (ending a capsule workspace is
    /// `workspace.destroy`, Sessions-mode `D`, already routed through
    /// `end_run` on the daemon side — ADR 0042 L1a — not an FE quit
    /// dispatcher; the session pane is not the drawer's fixed tenant), and
    /// `fe_down_last_evidence: None` at construction means
    /// `pending_fe_down_markers` can never receive anything to drain.
    ///
    /// LU6a: also the one place the attach-outcome log lines fire, each
    /// gated on an edge (this pump call's before/after, not the client's
    /// raw `changed` flag — that also fires on ordinary output, which
    /// would otherwise re-log every redraw) so each fires exactly once
    /// per outcome: checkpoint applied (clears `pane_hold` — the pane may
    /// now safely paint the client's own, now-restored screen), attached,
    /// and — anything else the status line moves to — a warn carrying how
    /// many such non-success moves this client has made so far — and, if
    /// that move is a TERMINAL one (`is_dead`) that arrived before this
    /// client ever checkpointed, also clears `pane_hold` (coordinator
    /// amendment: a dead end must not keep showing the departed row's
    /// screen — see `pane_hold`'s own doc). Design-review amendment:
    /// every line carries TWO clocks — `since_request_ms` (from
    /// `pane_attach_requested_at`, the switch or create the user actually
    /// asked for — the acceptance metric) and `since_client_ms` (from
    /// `pane_attach_started_at`, narrower: since THIS client object was
    /// installed).
    pub(in crate::ui) fn pump_pane_attach_term(&mut self) {
        let Some(t) = self.pane_attach_term.as_mut() else {
            return;
        };
        let was_checkpointed = t.is_checkpointed();
        let status_before = t.status_line().to_string();
        let changed = t.pump();
        let since_request_ms = self
            .pane_attach_requested_at
            .map(|s| s.elapsed().as_millis() as u64)
            .unwrap_or(0);
        let since_client_ms = self
            .pane_attach_started_at
            .map(|s| s.elapsed().as_millis() as u64)
            .unwrap_or(0);
        if !was_checkpointed && t.is_checkpointed() {
            self.pane_hold = None;
            let (rows, cols) = t.screen().size();
            tracing::info!(
                since_request_ms,
                since_client_ms,
                cols,
                rows,
                "session pane: capsule attach checkpoint applied"
            );
        }
        let status_after = t.status_line().to_string();
        if status_after != status_before {
            if status_after == "attached" {
                tracing::info!(since_request_ms, since_client_ms, "session pane: capsule attached");
                self.pane_inputs_discarded = 0;
            } else {
                self.pane_attach_episode_warnings += 1;
                tracing::warn!(
                    since_request_ms,
                    since_client_ms,
                    episode = self.pane_attach_episode_warnings,
                    status = %status_after,
                    "session pane: capsule attach episode failure"
                );
                // Coordinator amendment: a TERMINAL failure that never
                // checkpointed is a dead end, not a stall — clear
                // `pane_hold` right here, on the same path as the warn
                // line above, so a dead new row can never keep showing
                // the departed row's screen. `pane_screen_choice` also
                // stops painting this dead client's own (blank) screen
                // once `is_dead` is set, so the pane falls all the way
                // through to blank instead of either. A client that
                // checkpointed and only later
                // died is untouched (its own last content keeps showing).
                if t.is_dead() && !t.is_checkpointed() {
                    self.pane_hold = None;
                }
            }
        }
        if changed {
            // BLOCKER (Codex review, lane B5 discharge): a `notice()` set
            // by an EARLIER checkpoint ("attached to leg started …") is
            // retracted only on the NEXT checkpoint (`attach_client/client.rs`'s
            // own doc) — between the two, a mid-outage `Unreachable`
            // retry, a refusal, or any other non-"attached" status must
            // win over that stale text, never be hidden behind it. The
            // notice is subordinate: it adds color ONLY once the client
            // is honestly attached again.
            let status_line = t.status_line().to_string();
            if status_line != "attached" {
                self.status = status_line;
            } else if let Some(notice) = t.notice() {
                self.status = notice.to_string();
            } else if let Some(msg) = t.quit_message() {
                self.status = msg.to_string();
            } else {
                self.status = status_line;
            }
        }
    }

    fn park_warm_attach(&mut self, key: (HostKey, String), client: PaneAttachClient) {
        let bound = self
            .workspace_lists
            .get(&key.0)
            .map(|l| l.iter().filter(|w| !w.is_inert_anchor()).count())
            .unwrap_or(1);
        // A parked client's worker stays alive but must not dial after an
        // outage; it resumes when the row is viewed again.
        client.set_viewed(false);
        shutdown_detached(self.warm_attach.park(key, client, bound));
    }

    pub(in crate::ui) fn prune_warm_attach(&mut self, host: &HostKey) {
        let live: std::collections::HashSet<String> = self
            .workspace_lists
            .get(host)
            .map(|l| l.iter().map(|w| w.session_name.clone()).collect())
            .unwrap_or_default();
        shutdown_detached(self.warm_attach.retain_rows(host, |row| live.contains(row)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bl_pane_target_owner_pair_distinguishes_same_named_sessions_across_hosts() {
        // ADR 0042 L2a codex review, item D: attach_session_to_bl's
        // "already attached" early-return, and the PtyAttachDirect
        // owner gate, both compare the FULL (host, session) pair -- a
        // bare session-name compare let a switch from host alpha's
        // "sot-be-sot" to host beta's "sot-be-sot" (every host uses the
        // same "sot-be-<slug>" naming convention) hit the early return,
        // leaving the pty open on alpha while every later
        // write/resize/scroll (routed via active_host) went to beta.
        let bl_pane_target: Option<(HostKey, String)> =
            Some(("alpha".to_string(), "sot-be-sot".to_string()));

        // The switch target names the SAME session on a DIFFERENT host.
        let switch_to: (HostKey, String) = ("beta".to_string(), "sot-be-sot".to_string());
        assert_ne!(
            bl_pane_target.as_ref(),
            Some(&switch_to),
            "same session name on a different host must not read as already-attached"
        );

        // Once re-attached, an event tagged with the OLD host must not be
        // mistaken for the new owner (the PtyAttachDirect event_host
        // gate).
        let new_owner: Option<(HostKey, String)> = Some(switch_to);
        let stale_event_host: HostKey = "alpha".to_string();
        assert_ne!(
            new_owner.as_ref().map(|(h, _)| h),
            Some(&stale_event_host),
            "bytes tagged with the departed host must be recognized as non-owner"
        );
    }

    /// BLOCKER (Codex review, lane B5 discharge): `lane_dial` must dial
    /// the SAME endpoint the control transport actually resolved for a
    /// host — never a second, independent preference guess. A
    /// `ResolvedDial::Ssh(recipe)` carries the exact resolved recipe
    /// through untouched, proving no second, independent read of
    /// `config.dial` ever happens; a `ResolvedDial::Local` against an
    /// ssh-configured host — the mismatch that used to trigger the old
    /// "guess from config" bug — degrades to no dial instead of reaching
    /// a DIFFERENT daemon. `LaneDial` has no `Debug`/`PartialEq`
    /// (its own doc: an endpoint value names one dial, nothing to
    /// compare structurally), so each case matches the variant directly.
    #[test]
    fn a_switch_back_to_a_warm_row_takes_the_parked_client_without_a_new_dial() {
        let host = || "h".to_string();
        let a = (host(), "sot-be-a".to_string());
        let b = (host(), "sot-be-b".to_string());
        let c = (host(), "sot-be-c".to_string());
        let mut pool: WarmAttachPool<&'static str> = WarmAttachPool::new();
        let dials = std::cell::Cell::new(0);
        let attach = |pool: &mut WarmAttachPool<&'static str>, key: &(String, String)| -> &'static str {
            pool.take(key).unwrap_or_else(|| {
                dials.set(dials.get() + 1);
                "dialed"
            })
        };
        assert!(pool.park(a.clone(), "client-a", 2).is_empty());
        let on_b = attach(&mut pool, &b);
        assert_eq!((on_b, dials.get()), ("dialed", 1));
        assert!(pool.park(b.clone(), on_b, 2).is_empty());
        assert_eq!((attach(&mut pool, &a), dials.get()), ("client-a", 1));
        assert!(pool.park(a.clone(), "client-a", 2).is_empty());
        assert_eq!(pool.park(c, "client-c", 2), vec!["dialed"]);
        assert_eq!(pool.retain_rows(&host(), |row| row == "sot-be-c"), vec!["client-a"]);
        assert!(pool.take(&a).is_none());
    }
}
