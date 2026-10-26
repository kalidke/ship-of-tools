//! Window-wide request clocks, the fired preview target and its page re-raster, concept follow commitment,
//! driven-preview suppression, and the crop, scale-save and `--roi` aim transactions.

use crate::net::transport::OutgoingReq;
use crate::ui::{HostKey, WsKey};
use super::concept::{change_effect, ChangeEffect};
use super::image::view::RoiAim;

#[derive(Default)]
pub(in crate::ui) struct PreviewRequests {
    preview_req_gen: u64,
    concept_req_gen: u64,
    concept_target_fired: Option<String>,
    driven_preview_hold_cursor: Option<String>,
    /// The node the pane's latest preview request asked for; not the installed reply's node.
    preview_target_fired: Option<String>,
    /// `(page, zoom)` of an in-flight zoom re-raster of the shown page.
    page_raster_pending: Option<(u32, f32)>,
    /// The host a crop was sent to; whichever crop reply lands first takes it.
    crop_host: Option<HostKey>,
    /// `(node_id, typed value)` of an in-flight `preview.set_scale`.
    scale_save: Option<(String, String)>,
    /// One armed `preview --roi` viewport aim, latest wins.
    roi_aim: Option<RoiAim>,
}

/// A prepared change can be committed only by consuming its token after a successful send.
pub(in crate::ui) struct ConceptCommit {
    target: Option<String>,
}

pub(in crate::ui) struct ConceptFollow {
    change: Option<ConceptChange>,
}

struct ConceptChange {
    request: Option<OutgoingReq>,
    commit: ConceptCommit,
}

pub(in crate::ui) struct ClearConceptCaches;

impl PreviewRequests {
    pub(in crate::ui) fn issue_preview(&mut self) -> u64 {
        self.preview_req_gen += 1;
        self.preview_req_gen
    }

    pub(in crate::ui) fn issue_concept(&mut self) -> u64 {
        self.concept_req_gen += 1;
        self.concept_req_gen
    }

    pub(in crate::ui) fn issue_concept_reload(&mut self) -> u64 {
        self.issue_concept()
    }

    pub(in crate::ui) fn preview_generation(&self) -> u64 { self.preview_req_gen }
    pub(in crate::ui) fn concept_generation(&self) -> u64 { self.concept_req_gen }
    pub(in crate::ui) fn concept_target(&self) -> Option<&str> { self.concept_target_fired.as_deref() }

    pub(in crate::ui) fn admits_preview(
        &self, generation: u64, event_host: &HostKey, active_host: &HostKey,
        reply_workspace: &Option<String>, active_workspace: &Option<String>,
    ) -> bool {
        generation == self.preview_req_gen
            && Self::same_destination(event_host, active_host, reply_workspace, active_workspace)
    }

    fn admits_concept(
        &self, generation: u64, event_host: &HostKey, active_host: &HostKey,
        reply_workspace: &Option<String>, active_workspace: &Option<String>,
    ) -> bool {
        generation == self.concept_req_gen
            && Self::same_destination(event_host, active_host, reply_workspace, active_workspace)
    }

    fn same_destination(
        event_host: &HostKey, active_host: &HostKey,
        reply_workspace: &Option<String>, active_workspace: &Option<String>,
    ) -> bool {
        event_host == active_host && reply_workspace == active_workspace
    }

    pub(in crate::ui) fn prepare_concept_follow(
        &mut self, target: Option<String>, workspace_id: Option<String>,
    ) -> ConceptFollow {
        if self.concept_target_fired == target {
            return ConceptFollow { change: None };
        }
        let request = target.as_ref().map(|target| OutgoingReq::ConceptRead {
            target: target.clone(), workspace_id, generation: self.issue_concept(),
        });
        ConceptFollow { change: Some(ConceptChange { request, commit: ConceptCommit { target } }) }
    }

    pub(in crate::ui) fn commit_concept_follow(&mut self, commit: ConceptCommit) -> ClearConceptCaches {
        self.concept_target_fired = commit.target;
        ClearConceptCaches
    }

    pub(in crate::ui) fn invalidate_changed<'a>(&mut self, node_id: &'a str) -> ChangeEffect<'a> {
        let effect = change_effect(node_id, self.concept_target());
        if effect == ChangeEffect::ReadAnnotationAgain {
            self.invalidate_annotation_latch();
        }
        effect
    }

    fn invalidate_annotation_latch(&mut self) {
        self.concept_target_fired = None;
    }

    pub(in crate::ui) fn reset_concept_follow(&mut self) {
        self.concept_target_fired = None;
    }

    pub(in crate::ui) fn arm_driven_hold(&mut self, cursor_row: Option<String>) {
        self.driven_preview_hold_cursor = cursor_row;
    }

    /// Compare the derived preview candidate at the existing follow point; never move the cursor.
    pub(in crate::ui) fn suppress_follow(&mut self, candidate: &str) -> bool {
        if let Some(held) = self.driven_preview_hold_cursor.as_deref() {
            if held == candidate { return true; }
            self.driven_preview_hold_cursor = None;
        }
        false
    }

    pub(in crate::ui) fn finish_reveal(&mut self) {
        self.driven_preview_hold_cursor = None;
    }

    pub(in crate::ui) fn abort_reveal(&mut self) {
        self.driven_preview_hold_cursor = None;
    }

    pub(in crate::ui) fn leave_row(&mut self) {
        self.driven_preview_hold_cursor = None;
    }

    pub(in crate::ui) fn fired_target(&self) -> Option<&str> { self.preview_target_fired.as_deref() }

    /// A preview request for `node_id` went out (or, for a capture, was asked for).
    pub(in crate::ui) fn record_fired(&mut self, node_id: String) {
        self.preview_target_fired = Some(node_id);
    }

    /// Forget the fired target, so the next cursor follow fetches the cursored node again.
    pub(in crate::ui) fn forget_fired(&mut self) {
        self.preview_target_fired = None;
    }

    /// A row's snapshot restores the target it had fired when it was left.
    pub(in crate::ui) fn restore_fired(&mut self, node_id: Option<String>) {
        self.preview_target_fired = node_id;
    }

    /// A fetch of a new target or a page turn: a pending re-raster's reply must not be taken for it.
    pub(in crate::ui) fn cancel_reraster(&mut self) {
        self.page_raster_pending = None;
    }

    /// The zoom an in-flight re-raster asked for, 0.0 when none is.
    pub(in crate::ui) fn reraster_zoom(&self) -> f32 {
        self.page_raster_pending.map(|(_, z)| z).unwrap_or(0.0)
    }

    /// Called after the re-raster request was sent.
    pub(in crate::ui) fn arm_reraster(&mut self, page: u32, zoom: f32) {
        self.page_raster_pending = Some((page, zoom));
    }

    /// An admitted preview reply ends any pending re-raster: its zoom when the reply shows the pending page, else
    /// `None`, a real navigation that resets the view.
    pub(in crate::ui) fn settle_page_reply(&mut self, shown_page: Option<u32>) -> Option<f32> {
        match (self.page_raster_pending.take(), shown_page) {
            (Some((pending, zoom)), Some(shown)) if pending == shown => Some(zoom),
            _ => None,
        }
    }

    /// Called after the crop request was sent to `host`.
    pub(in crate::ui) fn arm_crop(&mut self, host: HostKey) {
        self.crop_host = Some(host);
    }

    /// A crop reply takes the pin before comparing, so a reply from another host consumes it too.
    pub(in crate::ui) fn settle_crop(&mut self, event_host: &HostKey) -> bool {
        self.crop_host.take().as_ref() == Some(event_host)
    }

    /// Called after the `preview.set_scale` request was sent.
    pub(in crate::ui) fn arm_scale_save(&mut self, node_id: String, raw: String) {
        self.scale_save = Some((node_id, raw));
    }

    /// An admitted preview reply for the save's own target resolves it, returning the typed value.
    pub(in crate::ui) fn settle_scale_save(&mut self, node_id: Option<&str>) -> Option<String> {
        match self.scale_save.as_ref() {
            Some((target, _)) if node_id == Some(target.as_str()) => self.scale_save.take().map(|(_, raw)| raw),
            _ => None,
        }
    }

    /// A rejected save resolves whatever save is pending.
    pub(in crate::ui) fn fail_scale_save(&mut self) {
        self.scale_save = None;
    }

    pub(in crate::ui) fn arm_roi_aim(&mut self, aim: RoiAim) {
        self.roi_aim = Some(aim);
    }

    /// A preview without `--roi` for the aimed row and node retires that aim.
    pub(in crate::ui) fn drop_roi_aim_for(&mut self, row_key: &WsKey, node_id: &str) {
        if self.roi_aim.as_ref().is_some_and(|aim| &aim.row_key == row_key && aim.node_id == node_id) {
            self.roi_aim = None;
        }
    }

    /// An installed reply for the aim's row and node certifies it when a raster was installed, and otherwise retires
    /// it, returning the retired aim.
    pub(in crate::ui) fn certify_roi_aim(
        &mut self, row_key: Option<&WsKey>, node_id: Option<&str>, raster_installed: bool,
    ) -> Option<RoiAim> {
        let drop_aim = match self.roi_aim.as_mut() {
            Some(aim) if !aim.ready && row_key == Some(&aim.row_key) && node_id == Some(aim.node_id.as_str()) => {
                if raster_installed {
                    aim.ready = true;
                    false
                } else {
                    true
                }
            }
            _ => false,
        };
        if drop_aim { self.roi_aim.take() } else { None }
    }

    /// The render pass consumes a certified aim for the active row that is still the fired target.
    pub(in crate::ui) fn take_ready_roi_aim(&mut self, row_key: Option<&WsKey>) -> Option<RoiAim> {
        let fired = self.preview_target_fired.as_deref();
        if self.roi_aim.as_ref().is_some_and(|a| {
            a.ready && row_key == Some(&a.row_key) && Some(a.node_id.as_str()) == fired
        }) {
            self.roi_aim.take()
        } else {
            None
        }
    }

    #[cfg(all(test, feature = "test-window-progress"))]
    pub(in crate::ui) fn roi_aim(&self) -> Option<&RoiAim> { self.roi_aim.as_ref() }
}

/// The coordinator supplies transport and drift caches; no owner operation takes State.
pub(in crate::ui) trait ConceptRequestContext {
    fn send_request(&mut self, request: OutgoingReq) -> Result<(), String>;
    fn commit_follow(&mut self, commit: ConceptCommit);
    fn take_drift_parse(&mut self, path: &str) -> bool;
    fn forget_drift_fire(&mut self, path: &str);
}

pub(in crate::ui) fn continue_concept_follow(
    cx: &mut impl ConceptRequestContext, follow: ConceptFollow,
    files_path: Option<String>, node_label: String, workspace_id: Option<String>,
) {
    if let Some(ConceptChange { request, commit }) = follow.change {
        if let Some(request) = request {
            let t = match &request {
                OutgoingReq::ConceptRead { target, .. } => target.clone(),
                _ => unreachable!("concept follow prepares only concept.read"),
            };
            if let Err(e) = cx.send_request(request) {
                tracing::warn!(target: "sot::ui::preview::concept", error = %e, target = %t, "drop concept.read request — channel closed");
                // A failed send consumes its generation, but cannot commit or parse drift.
                return;
            }
        }
        cx.commit_follow(commit);
    }
    if let Some(path) = files_path {
        if cx.take_drift_parse(&path) {
            tracing::debug!(target: "sot::ui::preview::concept", %path, label = %node_label, "→ file.parse for drift check");
            if let Err(e) = cx.send_request(OutgoingReq::FileParse { path: path.clone(), workspace_id }) {
                tracing::warn!(target: "sot::ui::preview::concept", error = %e, %path, "drop file.parse request — channel closed");
                cx.forget_drift_fire(&path);
            }
        }
    }
}

/// Shared admission-and-continuation seam for the real reply entries and portable recorders.
pub(in crate::ui) trait RequestReplyContext {
    type Preview;
    type Concept;
    type Failure;
    fn requests(&self) -> &PreviewRequests;
    fn active_host(&self) -> &HostKey;
    fn active_workspace(&self) -> &Option<String>;
    fn install_preview(&mut self, payload: Self::Preview);
    fn stale_editor_target(&self) -> Option<&str>;
    fn reload_editor(&mut self, payload: &Self::Concept);
    fn install_concept(&mut self, target: String, payload: Self::Concept);
    fn preview_failed(&mut self, payload: Self::Failure);
}

pub(in crate::ui) fn continue_preview_reply<C: RequestReplyContext>(
    cx: &mut C, event_host: HostKey, workspace_id: Option<String>, generation: u64, payload: C::Preview,
) {
    if !cx.requests().admits_preview(generation, &event_host, cx.active_host(), &workspace_id, cx.active_workspace()) {
        tracing::debug!(target: "sot::ui::preview::replies", ?workspace_id, generation, latest = cx.requests().preview_generation(),
            %event_host, active_host = %cx.active_host(), "drop stale preview.get/set_scale reply");
        return;
    }
    cx.install_preview(payload);
}

pub(in crate::ui) fn continue_concept_reply<C: RequestReplyContext>(
    cx: &mut C, event_host: HostKey, workspace_id: Option<String>, generation: u64, target: String, payload: C::Concept,
) {
    if !cx.requests().admits_concept(generation, &event_host, cx.active_host(), &workspace_id, cx.active_workspace()) {
        tracing::debug!(target: "sot::ui::preview::replies", %target, ?workspace_id, generation,
            latest = cx.requests().concept_generation(), %event_host, active_host = %cx.active_host(),
            "concept.read reply dropped — stale generation or non-active (host, workspace)");
        return;
    }
    if cx.stale_editor_target() == Some(target.as_str()) {
        cx.reload_editor(&payload);
    }
    if cx.requests().concept_target() == Some(target.as_str()) {
        cx.install_concept(target, payload);
    }
}

pub(in crate::ui) fn continue_preview_failure<C: RequestReplyContext>(
    cx: &mut C, event_host: HostKey, workspace_id: Option<String>, generation: u64, payload: C::Failure,
) {
    if !cx.requests().admits_preview(generation, &event_host, cx.active_host(), &workspace_id, cx.active_workspace()) {
        tracing::debug!(target: "sot::ui::preview::replies", ?workspace_id, generation, latest = cx.requests().preview_generation(),
            %event_host, active_host = %cx.active_host(), "drop stale preview.get failure");
        return;
    }
    cx.preview_failed(payload);
}

#[cfg(test)]
#[path = "requests_tests.rs"]
mod tests;
