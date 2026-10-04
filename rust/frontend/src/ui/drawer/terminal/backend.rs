//! The Terminal drawer's backend: the choice between the in-process shell and the attach client, and its pump.

use crate::ui::*;

/// Which backend the drawer uses (ADR 0041, ADR 0050). The attach-only
/// drawer needs this computer's backend, and that backend must hold this
/// window's lease: a granted lease whose `state_root` is this window's own.
/// The choice is made once, so an attach drawer already running stays and a
/// plain terminal already running is never swapped out.
#[cfg_attr(not(windows), allow(dead_code))]
pub(in crate::ui) fn drawer_uses_attach(
    setting: bool,
    attach_live: bool,
    local_live: bool,
    granted_roots: &[String],
    own_root: Option<&str>,
) -> bool {
    attach_live || (setting && !local_live && own_root.is_some_and(|r| granted_roots.iter().any(|g| g == r)))
}

/// `scroll_ring` for the Terminal drawer, which has two possible clients
/// (the in-process shell, or the Windows attach client) of which only one
/// is ever live — three call sites share the dance.
pub(in crate::ui) fn scroll_drawer_ring(state: &mut State, delta: i32) {
    if let Some(t) = state.local_term.as_mut() {
        scroll_ring(t.screen_mut(), delta);
    }
    #[cfg(windows)]
    if let Some(t) = state.attach_term.as_mut() {
        scroll_ring(t.screen_mut(), delta);
    }
}

impl State {
    /// ADR 0041 step 6 U3: constructs the attach-only backend the first
    /// time the Terminal drawer opens with `drawer.attach_only` on
    /// (mirrors `LocalTerminal::spawn`'s own lazy-spawn site). Gated by
    /// the caller on `self.drawer == DrawerContent::Terminal` — spawning
    /// is a user-visible-drawer-only act; PUMPING the client once it
    /// exists is not (see `pump_attach_term`'s own doc).
    #[cfg(windows)]
    pub(in crate::ui) fn spawn_attach_term(&mut self) {
        let Some(state_dir) = crate::paths::sot_state_dir() else {
            tracing::warn!("attach-only: no per-machine state dir resolved");
            self.status = "attach-only terminal failed: no per-machine state dir".to_string();
            self.drawer = DrawerContent::Closed;
            return;
        };
        let controller_id = self_comm_handle();
        let fe_down_to = self_comm_handle();
        let waker = self.window.clone();
        match sot_log::attach_client::client::FeAttachClient::attach(
            sot_log::lane::client::PlatformEndpoint::default(),
            sot_log::host::state_dir::state_dir_hash(&state_dir),
            80,
            24,
            controller_id,
            fe_down_to,
            None,
            Box::new(move || waker.request_redraw()),
        ) {
            Ok(c) => {
                tracing::info!("fe attach-only client attaching");
                self.attach_term = Some(c);
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to start attach-only client");
                self.status = format!("attach-only terminal failed: {e}");
                self.drawer = DrawerContent::Closed;
            }
        }
    }

    /// ADR 0041 step 6 U3: drains checkpoint/output/notice/status/
    /// terminal/fe_down events from an EXISTING attach client. Called by
    /// the caller on every redraw whenever `self.attach_term.is_some()`
    /// — deliberately NOT gated on `self.drawer`.
    #[cfg(windows)]
    pub(in crate::ui) fn pump_attach_term(&mut self) {
        let Some(t) = self.attach_term.as_mut() else {
            return;
        };
        let changed = t.pump();
        // Codex review round, finding 11: status text (queue overflow/
        // expiry, geometry refusal, pen loss, input-delivery-unknown,
        // ...) must reach the drawer independently of `is_dead()` — the
        // first landing only copied `status_line()` once the client was
        // already terminal, so every one of those still-live diagnostics
        // was invisible. `notice`/`quit_message` are the more urgent
        // overlays and still take priority when present.
        if changed {
            if let Some(notice) = t.notice() {
                self.status = notice.to_string();
            } else if let Some(msg) = t.quit_message() {
                self.status = msg.to_string();
            } else {
                self.status = t.status_line().to_string();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_only_requires_matching_state_root() {
        let roots = vec!["r".to_string()];
        // (what, setting, attach_live, local_live, roots, own_root, want)
        let cases: &[(&str, bool, bool, bool, &[String], Option<&str>, bool)] = &[
            ("setting on, matching root, no terminals", true, false, false, &roots, Some("r"), true),
            ("setting off", false, false, false, &roots, Some("r"), false),
            ("no granted lease", true, false, false, &[], Some("r"), false),
            ("a root that differs", true, false, false, &roots, Some("x"), false),
            ("own root unknown", true, false, false, &roots, None, false),
            ("attach already live, setting off", false, true, false, &[], None, true),
            ("plain terminal already live", true, false, true, &roots, Some("r"), false),
        ];
        for (what, setting, attach, local, granted, own, want) in cases {
            assert_eq!(drawer_uses_attach(*setting, *attach, *local, granted, *own), *want, "{what}");
        }
    }
}
