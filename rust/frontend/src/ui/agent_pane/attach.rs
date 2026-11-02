//! Dialing, pumping and the bounded warm pool. State applies transport, logging and retirement effects.

use super::*;

pub(in crate::ui) type PaneAttachClient = sot_capsule::attach_client::client::FeAttachClient;

/// Newest last; bounds are host-local attachment counts, capped at sixteen.
pub(super) struct WarmAttachPool<C> {
    entries: Vec<(WsKey, C)>,
}
const WARM_ATTACH_CAP: usize = 16;
impl<C> WarmAttachPool<C> {
    pub(super) fn new() -> Self { Self { entries: Vec::new() } }
    fn take(&mut self, key: &WsKey) -> Option<C> {
        let i = self.entries.iter().position(|(k, _)| k == key)?;
        Some(self.entries.remove(i).1)
    }
    fn park(&mut self, key: WsKey, client: C, bound: usize) -> Vec<C> {
        let mut evicted = Vec::new();
        if let Some(old) = self.take(&key) { evicted.push(old); }
        let host = key.0.clone();
        self.entries.push((key, client));
        let bound = bound.clamp(1, WARM_ATTACH_CAP);
        while self.entries.iter().filter(|(k, _)| k.0 == host).count() > bound {
            let i = self.entries.iter().position(|(k, _)| k.0 == host).expect("over bound");
            evicted.push(self.entries.remove(i).1);
        }
        evicted
    }
    fn retain_rows(&mut self, host: &HostKey, live: &std::collections::HashSet<String>) -> Vec<C> {
        let (dead, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.entries).into_iter()
            .partition(|(k, _)| &k.0 == host && !live.contains(&k.1));
        self.entries = kept;
        dead.into_iter().map(|(_, c)| c).collect()
    }
}

pub(in crate::ui) fn shutdown_detached(clients: Vec<PaneAttachClient>, reason: &'static str) {
    for mut c in clients {
        c.set_shutdown_reason(reason);
        std::thread::spawn(move || { c.shutdown(Duration::from_millis(250)); });
    }
}

pub(in crate::ui) fn input_outcome_notice(outcome: Option<sot_capsule::attach_client::client::InputOutcome>) -> Option<&'static str> {
    use sot_capsule::attach_client::client::InputOutcome;
    match outcome {
        Some(InputOutcome::DeliveryUnknown) => Some("delivery unknown: the last key may not have reached the agent; type it again if it did not"),
        Some(InputOutcome::RunEnded) => Some("run ended: keys typed before the agent restarted were not sent to the new run"),
        Some(InputOutcome::Recorded | InputOutcome::RefusedStale) | None => None,
    }
}

/// Owned send intent; completion commits this intent only after the transport accepted it.
pub(in crate::ui) struct PaneOpen {
    target: WsKey,
    size: (u16, u16),
    user_switch: bool,
}
impl PaneOpen {
    pub(in crate::ui) fn host(&self) -> &HostKey { &self.target.0 }
    pub(in crate::ui) fn attachment(&self) -> &str { &self.target.1 }
    pub(in crate::ui) fn request(&self) -> OutgoingReq {
        OutgoingReq::PtyOpen { cols: self.size.0, rows: self.size.1,
            target: Some(self.target.1.clone()), user_switch: self.user_switch }
    }
}
pub(in crate::ui) struct PaneSwitch<C> {
    pub open: Option<PaneOpen>,
    pub retired: Vec<C>,
    pub status: Option<String>,
}

/// State retires rejected warm clients before the factory can dial.
pub(in crate::ui) struct AttachPlan<C> {
    target: WsKey,
    size: (u16, u16),
    warm: Option<C>,
}
#[derive(Debug)]
pub(in crate::ui) enum AttachFailure { MissingConfig, Unresolved, Unusable, Startup(String) }
pub(in crate::ui) struct AttachEffect {
    pub status: String,
    pub trace: AttachTrace,
}
pub(in crate::ui) enum AttachTrace { Warm(u64), Cold, Failed(AttachFailure) }

/// A prepared client stays opaque and uninstalled until the coordinator has logged its attach.
struct PendingAttach<C> {
    client: Option<C>,
    effect: AttachEffect,
}

/// The coordinator logs before warm resume and before sampling the installed client's clock.
/// Both production and the process-free tests use this ordering with narrow log and clock ports.
pub(super) fn install_pane_attach<C: PaneClient>(
    pane: &mut AgentPane<C>, plan: AttachPlan<C>,
    factory: impl FnOnce(&HostKey, &str, (u16, u16)) -> std::result::Result<C, AttachFailure>,
    mut now: impl FnMut() -> Instant,
    log: impl FnOnce(&AttachTrace),
) -> AttachEffect {
    let pending = pane.start_attach(plan, factory, &mut now);
    log(&pending.effect.trace);
    pane.finish_attach(pending, now)
}
#[derive(Debug, PartialEq, Eq)]
pub(in crate::ui) enum AttachOutcome { Checkpoint { cols: u16, rows: u16 }, Attached, Warning { episode: u32, status: String } }
pub(in crate::ui) struct PumpEffect {
    pub status: Option<String>,
    pub outcomes: Vec<AttachOutcome>,
    pub since_request_ms: u64,
    pub since_client_ms: u64,
    /// An OSC 52 copy the pane's row asked for (`/copy` in Claude Code), as base64, for this window's clipboard.
    pub copy: Option<Vec<u8>>,
}

impl<C: PaneClient> AgentPane<C> {
    pub(in crate::ui) fn begin_switch(&mut self, next: WsKey, origin: Option<Instant>, bound: usize) -> PaneSwitch<C> {
        if self.is_target(&next) {
            return PaneSwitch { open: None, retired: vec![], status: Some(format!("already attached · {}", next.1)) };
        }
        self.pane_inputs_discarded = 0;
        self.pane_attach_requested_at = origin;
        self.pane_presentation.begin(origin);
        let mut retired = Vec::new();
        if let Some(mut client) = self.pane_attach_term.take() {
            self.pane_hold = Some(HeldPaneScreen(client.screen().clone()));
            match self.bl_pane_target.clone() {
                Some(key) if client.is_checkpointed() && !client.is_dead() => {
                    client.set_viewed(false);
                    retired = self.warm_attach.park(key, client, bound);
                }
                _ => client.set_shutdown_reason("switch away"),
            }
        }
        self.pane_dial_error = None;
        self.pane_feed = PaneFeed::Pending;
        PaneSwitch { open: Some(PaneOpen { target: next, size: self.open_size(), user_switch: true }), retired, status: None }
    }

    pub(in crate::ui) fn finish_open(&mut self, open: PaneOpen, sent: bool) -> Option<String> {
        if !sent { return None; }
        let status = format!("attached BL → {}", open.target.1);
        self.bl_pane_target = Some(open.target);
        Some(status)
    }

    pub(in crate::ui) fn prepare_attach(&mut self, target: AdmittedAttach) -> (AttachPlan<C>, Vec<C>) {
        // Direct-reply admission guarantees an empty active slot; only a dead warm client needs retirement.
        let mut retired = Vec::new();
        let warm = self.warm_attach.take(&target.0).and_then(|mut c| {
            if c.is_dead() { retired.push(c); None } else { Some(c) }
        });
        (AttachPlan { target: target.0, size: self.open_size(), warm }, retired)
    }

    fn start_attach(
        &mut self, plan: AttachPlan<C>,
        factory: impl FnOnce(&HostKey, &str, (u16, u16)) -> std::result::Result<C, AttachFailure>,
        mut now: impl FnMut() -> Instant,
    ) -> PendingAttach<C> {
        let AttachPlan { target: (host, target), size: (cols, rows), warm } = plan;
        let (client, trace) = match warm {
            Some(mut c) => {
                if c.screen().size() != (rows, cols) { c.resize(cols, rows); }
                let elapsed = self.pane_attach_requested_at.map(|s| now().saturating_duration_since(s).as_millis() as u64).unwrap_or(0);
                (c, AttachTrace::Warm(elapsed))
            }
            None => match factory(&host, &target, (cols, rows)) {
                Ok(c) => (c, AttachTrace::Cold),
                Err(failure) => {
                    let msg = match &failure {
                        AttachFailure::MissingConfig => format!("'{host}' has no transport configured"),
                        AttachFailure::Unresolved => format!("'{host}' not yet connected — capsule attach waiting"),
                        AttachFailure::Unusable => format!("'{host}' has no usable transport for its resolved connection"),
                        AttachFailure::Startup(e) => format!("session attach failed: {e}"),
                    };
                    if matches!(failure, AttachFailure::MissingConfig | AttachFailure::Unusable) {
                        self.pane_dial_error = Some(msg.clone());
                    }
                    self.pane_hold = None;
                    self.pane_feed = PaneFeed::Pending;
                    return PendingAttach { client: None, effect: AttachEffect { status: msg, trace: AttachTrace::Failed(failure) } };
                }
            },
        };
        PendingAttach { client: Some(client), effect: AttachEffect {
            status: format!("attached BL → {target} (capsule, corrected)"), trace,
        } }
    }

    fn finish_attach(&mut self, pending: PendingAttach<C>, now: impl FnOnce() -> Instant) -> AttachEffect {
        if let Some(c) = pending.client {
            let warm = matches!(pending.effect.trace, AttachTrace::Warm(_));
            if warm {
                c.set_viewed(true);
                if c.status_line() == "attached" { self.pane_inputs_discarded = 0; }
            }
            self.pane_attach_term = Some(c);
            self.copy_quiet = true;
            self.copy_tail = false;
            self.pane_dial_error = None;
            if warm { self.pane_hold = None; }
            self.pane_attach_started_at = Some(now());
            self.pane_attach_episode_warnings = 0;
            self.pane_feed = PaneFeed::Capsule;
        }
        pending.effect
    }

    pub(in crate::ui) fn reconnect_open(&self, host: &HostKey) -> Option<PaneOpen> {
        if self.pane_attach_term.is_some() || self.pane_dial_error.is_some() { return None; }
        let target = self.bl_pane_target.as_ref()?;
        (target.0 == *host).then(|| PaneOpen { target: target.clone(), size: self.open_size(), user_switch: false })
    }

    pub(in crate::ui) fn pump(&mut self, mut now: impl FnMut() -> Instant) -> PumpEffect {
        let mut effect = PumpEffect { status: None, outcomes: vec![], since_request_ms: 0, since_client_ms: 0, copy: None };
        let Some(t) = self.pane_attach_term.as_mut() else { return effect };
        let was_checkpointed = t.is_checkpointed();
        let status_before = t.status_line().to_string();
        let changed = t.pump();
        let copy = t.take_copy();
        if self.copy_quiet {
            if t.backlog_drained() {
                self.copy_quiet = false;
                self.copy_tail = !t.at_ground();
            }
        } else if self.copy_tail {
            if copy.is_some() || t.at_ground() {
                self.copy_tail = false;
            }
        } else {
            effect.copy = copy;
        }
        effect.since_request_ms = self.pane_attach_requested_at.map(|s| now().saturating_duration_since(s).as_millis() as u64).unwrap_or(0);
        effect.since_client_ms = self.pane_attach_started_at.map(|s| now().saturating_duration_since(s).as_millis() as u64).unwrap_or(0);
        if !was_checkpointed && t.is_checkpointed() {
            self.pane_hold = None;
            let (rows, cols) = t.screen().size();
            effect.outcomes.push(AttachOutcome::Checkpoint { cols, rows });
        }
        let status_after = t.status_line().to_string();
        if status_after != status_before {
            if status_after == "attached" {
                effect.outcomes.push(AttachOutcome::Attached);
                self.pane_inputs_discarded = 0;
            } else {
                self.pane_attach_episode_warnings += 1;
                effect.outcomes.push(AttachOutcome::Warning { episode: self.pane_attach_episode_warnings, status: status_after });
                if t.is_dead() && !t.is_checkpointed() { self.pane_hold = None; }
            }
        }
        if changed {
            effect.status = Some(input_outcome_notice(t.last_input_outcome()).map_or_else(|| t.status_line().to_string(), str::to_string));
        }
        effect
    }

    pub(in crate::ui) fn prune_warm(&mut self, host: &HostKey, live: &std::collections::HashSet<String>) -> Vec<C> {
        self.warm_attach.retain_rows(host, live)
    }
}

impl State {
    pub(in crate::ui) fn attach_session_to_bl(&mut self, host: HostKey, session_name: String) {
        let key = (host, session_name);
        // The create's request time is the selected row's, consumed only for an actual switch.
        let origin = if self.agent_pane.is_target(&key) { None } else {
            Some(self.selected.take_create_requested().unwrap_or_else(Instant::now))
        };
        let bound = self.agent_pane.target().and_then(|(h, _)| self.row_directory.rows(&h)
            .map(|l| l.iter().filter(|w| !w.is_inert_anchor()).count())).unwrap_or(1);
        let plan = self.agent_pane.begin_switch(key, origin, bound);
        shutdown_detached(plan.retired, "warm pool eviction");
        if let Some(status) = plan.status {
            self.status.set(status);
            self.window.request_redraw();
        }
        let Some(open) = plan.open else { return };
        let sent = match self.send_to(open.host(), open.request()) {
            Ok(()) => true,
            Err(e) => {
                let session_name = open.attachment();
                tracing::warn!(error = %e, %session_name, "drop pty.open re-target request — channel closed");
                false
            }
        };
        if let Some(status) = self.agent_pane.finish_open(open, sent) {
            self.status.set(status);
            self.window.request_redraw();
            self.persist_resume_state();
        }
    }

    pub(in crate::ui) fn spawn_pane_attach_term(&mut self, target: AdmittedAttach) {
        let (plan, retired) = self.agent_pane.prepare_attach(target);
        shutdown_detached(retired, "dead warm client");
        let hosts = &mut self.hosts;
        let wake = self.wake.clone();
        let effect = install_pane_attach(&mut self.agent_pane, plan, |host, target, (cols, rows)| {
            let config = hosts.host_transports.get(host).ok_or(AttachFailure::MissingConfig)?;
            let resolved = hosts.host_resolved_dial.get(host).cloned().ok_or(AttachFailure::Unresolved)?;
            let link = hosts.links.entry(host.clone()).or_default().clone();
            let (dial, token) = lane_dial(config, resolved, &link).ok_or(AttachFailure::Unusable)?;
            let endpoint = sot_topology::lane_client::DaemonLaneEndpoint::new(dial, token);
            PaneAttachClient::attach_on_host(endpoint, host.to_string(), target.to_string(), cols, rows, Box::new(move || wake.wake()))
                .map_err(|e| AttachFailure::Startup(e.to_string()))
        }, Instant::now, |trace| match trace {
            AttachTrace::Warm(since_request_ms) => tracing::info!(since_request_ms, "session pane: warm capsule client reused"),
            AttachTrace::Cold => tracing::info!("session pane: capsule attach client attaching"),
            AttachTrace::Failed(AttachFailure::Startup(e)) => tracing::warn!(error = %e, "session pane: capsule attach client failed to start"),
            AttachTrace::Failed(_) => {}
        });
        self.status.set(effect.status);
    }

    pub(in crate::ui) fn pump_pane_attach_term(&mut self) {
        let effect = self.agent_pane.pump(Instant::now);
        let (since_request_ms, since_client_ms) = (effect.since_request_ms, effect.since_client_ms);
        for outcome in effect.outcomes {
            match outcome {
                AttachOutcome::Checkpoint { cols, rows } => tracing::info!(since_request_ms, since_client_ms, cols, rows, "session pane: capsule attach checkpoint applied"),
                AttachOutcome::Attached => tracing::info!(since_request_ms, since_client_ms, "session pane: capsule attached"),
                AttachOutcome::Warning { episode, status } => tracing::warn!(since_request_ms, since_client_ms, episode, %status, "session pane: capsule attach episode failure"),
            }
        }
        if let Some(status) = effect.status { self.status.set(status); }
        if let Some(b64) = effect.copy { self.copy_to_clipboard(&b64); }
    }

    /// A copy the pane's row asked for (`/copy` in Claude Code, any OSC 52 write) lands on this window's clipboard, the
    /// computer the person is at, whatever computer the row runs on; the status line says so either way.
    fn copy_to_clipboard(&mut self, b64: &[u8]) {
        let status = match copied_text(b64) {
            Err(why) => format!("copy from the row not done: {why}"),
            Ok(text) => {
                let (bytes, chars) = (text.len(), text.chars().count());
                match crate::ui::input::write_clipboard_text(text) {
                Ok(()) => {
                    tracing::info!(bytes, "row copy → clipboard");
                    format!("copied {chars} characters from the row to the clipboard")
                }
                Err(e) => {
                    tracing::warn!(error = %e, "row copy: clipboard write failed");
                    format!("copy from the row not done: the clipboard refused it ({e})")
                }
                }
            }
        };
        self.status.set(status);
        self.status.hold();
        self.window.request_redraw();
    }

    pub(in crate::ui) fn prune_warm_attach(&mut self, host: &HostKey) {
        let live = self.row_directory.rows(host).map(|l| l.iter().map(|w| w.session_name.clone()).collect()).unwrap_or_default();
        shutdown_detached(self.agent_pane.prune_warm(host, &live), "row removed");
    }
}

/// The text an OSC 52 copy carries: its base64 decoded, a byte that is not UTF-8 shown as U+FFFD. Empty or undecodable
/// base64 is refused with why.
pub(in crate::ui) fn copied_text(b64: &[u8]) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| format!("not base64 ({e})"))?;
    if bytes.is_empty() {
        return Err("nothing to copy".into());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
#[path = "attach_tests.rs"]
mod tests;
