//! Window-wide request clocks, concept follow commitment and driven-preview suppression.

use crate::net::transport::OutgoingReq;
use crate::ui::HostKey;
use super::concept::{change_effect, ChangeEffect};

#[derive(Default)]
pub(in crate::ui) struct PreviewRequests {
    preview_req_gen: u64,
    concept_req_gen: u64,
    concept_target_fired: Option<String>,
    driven_preview_hold_cursor: Option<String>,
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
