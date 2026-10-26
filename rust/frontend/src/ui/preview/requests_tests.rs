use super::*;
use std::collections::{HashSet, VecDeque};
use sot_platform::test_log::{capture, Capture};

/// Portable transport/cache/drift recorder. The owner is real; sends have scripted outcomes.
struct RecordingRequests {
    requests: PreviewRequests,
    host: HostKey,
    workspace: Option<String>,
    outcomes: VecDeque<bool>,
    sent: Vec<(String, u64, Option<String>)>,
    events: Vec<String>,
    logs_at_event: Vec<String>,
    capture: Option<Capture>,
    parsed: HashSet<String>,
    concept_cache: Option<String>,
    preview_concept_cache: Option<String>,
    editor_target: Option<String>,
    editor_content: Option<String>,
}

impl RecordingRequests {
    fn new(workspace: Option<&str>) -> Self {
        Self {
            requests: PreviewRequests::default(), host: "active".into(), workspace: workspace.map(str::to_owned),
            outcomes: VecDeque::new(), sent: Vec::new(), events: Vec::new(), logs_at_event: Vec::new(),
            capture: None, parsed: HashSet::new(), concept_cache: None, preview_concept_cache: None,
            editor_target: None, editor_content: None,
        }
    }

    fn record(&mut self, event: &str) {
        self.events.push(event.into());
        self.logs_at_event.push(self.capture.as_ref().map(Capture::text).unwrap_or_default());
    }

    fn follow(&mut self, target: Option<&str>, path: Option<&str>) {
        let follow = self.requests.prepare_concept_follow(target.map(str::to_owned), self.workspace.clone());
        let workspace = self.workspace.clone();
        continue_concept_follow(self, follow, path.map(str::to_owned), "source label".into(), workspace);
    }

    fn reload(&mut self, target: &str) -> u64 {
        let generation = self.requests.issue_concept_reload();
        self.send_request(OutgoingReq::ConceptRead {
            target: target.into(), workspace_id: self.workspace.clone(), generation,
        }).unwrap();
        generation
    }
}

impl ConceptRequestContext for RecordingRequests {
    fn send_request(&mut self, request: OutgoingReq) -> Result<(), String> {
        match request {
            OutgoingReq::ConceptRead { target, generation, workspace_id } => {
                self.record("send-concept");
                self.sent.push((target, generation, workspace_id));
            }
            OutgoingReq::FileParse { path, workspace_id } => {
                assert_eq!(workspace_id, self.workspace);
                self.record("send-parse");
                assert!(self.parsed.contains(&path));
            }
            _ => panic!("unexpected request"),
        }
        if self.outcomes.pop_front().unwrap_or(true) { Ok(()) } else { Err("scripted send failure".into()) }
    }

    fn commit_follow(&mut self, commit: ConceptCommit) {
        let ClearConceptCaches = self.requests.commit_concept_follow(commit);
        self.concept_cache = None;
        self.preview_concept_cache = None;
        self.record("clear");
    }

    fn take_drift_parse(&mut self, path: &str) -> bool {
        self.record("drift");
        self.parsed.insert(path.into())
    }

    fn forget_drift_fire(&mut self, path: &str) {
        self.parsed.remove(path);
        self.record("forget-parse");
    }
}

/// Reply fixture borrows the real owner and the consumer recorders.
struct RecordingReplies<'a>(&'a mut RecordingRequests);
impl RequestReplyContext for RecordingReplies<'_> {
    type Preview = ();
    type Concept = String;
    type Failure = ();
    fn requests(&self) -> &PreviewRequests { &self.0.requests }
    fn active_host(&self) -> &HostKey { &self.0.host }
    fn active_workspace(&self) -> &Option<String> { &self.0.workspace }
    fn install_preview(&mut self, _: ()) {
        for effect in ["source", "page", "save", "status", "ledger-install"] { self.0.record(effect); }
    }
    fn stale_editor_target(&self) -> Option<&str> { self.0.editor_target.as_deref() }
    fn reload_editor(&mut self, content: &String) {
        self.0.editor_content = Some(content.clone());
        self.0.editor_target = None;
        self.0.record("editor");
    }
    fn install_concept(&mut self, _: String, content: String) {
        self.0.concept_cache = Some(content.clone());
        self.0.preview_concept_cache = Some(content);
        self.0.record("concept");
    }
    fn preview_failed(&mut self, _: ()) { self.0.record("failure-status"); }
}

fn preview(cx: &mut RecordingRequests, host: &str, workspace: Option<&str>, generation: u64) {
    continue_preview_reply(&mut RecordingReplies(cx), host.into(), workspace.map(str::to_owned), generation, ());
}

fn concept(cx: &mut RecordingRequests, host: &str, workspace: Option<&str>, generation: u64, target: &str) {
    continue_concept_reply(&mut RecordingReplies(cx), host.into(), workspace.map(str::to_owned), generation, target.into(), "fresh".into());
}

#[test]
fn stale_preview_reply_never_installs() {
    let mut cx = RecordingRequests::new(None);
    assert_eq!(cx.requests.issue_preview(), 1);
    assert_eq!(cx.requests.issue_preview(), 2);
    preview(&mut cx, "active", None, 1);
    assert!(cx.events.is_empty());
    preview(&mut cx, "active", None, 2);
    assert_eq!(cx.events, ["source", "page", "save", "status", "ledger-install"]);
    // Current replies remain repeatable, and zero retains its initial admission behavior.
    preview(&mut cx, "active", None, 2);
    assert_eq!(cx.events.len(), 10);
    let mut initial = RecordingRequests::new(None);
    preview(&mut initial, "active", None, 0);
    assert_eq!(initial.events.len(), 5);
}

#[test]
fn a_future_generation_never_installs() {
    let mut cx = RecordingRequests::new(None);
    cx.requests.issue_preview();
    preview(&mut cx, "active", None, 2);
    assert!(cx.events.is_empty());
}

#[test]
fn equal_default_workspaces_still_require_the_dial_host() {
    let mut cx = RecordingRequests::new(None);
    preview(&mut cx, "other", None, 0);
    assert!(cx.events.is_empty());
    preview(&mut cx, "active", None, 0);
    assert_eq!(cx.events.len(), 5);
}

#[test]
fn equal_generations_still_require_the_active_workspace() {
    let mut cx = RecordingRequests::new(Some("current"));
    for workspace in [None, Some("other")] { preview(&mut cx, "active", workspace, 0); }
    assert!(cx.events.is_empty());
    preview(&mut cx, "active", Some("current"), 0);
    assert_eq!(cx.events.len(), 5);
    let mut default = RecordingRequests::new(None);
    preview(&mut default, "active", Some("current"), 0);
    assert!(default.events.is_empty());
    preview(&mut default, "active", None, 0);
    assert_eq!(default.events.len(), 5);
}

#[test]
fn stale_failures_and_concept_replies_have_no_effects() {
    let mut cx = RecordingRequests::new(None);
    cx.requests.issue_preview();
    cx.requests.issue_preview();
    cx.follow(Some("files/A"), None);
    cx.reload("files/A");
    cx.editor_target = Some("files/A".into());
    cx.events.clear();
    for (host, workspace, generation) in [("active", None, 1), ("other", None, 2), ("active", Some("other"), 2)] {
        continue_preview_failure(&mut RecordingReplies(&mut cx), host.into(), workspace.map(str::to_owned), generation, ());
        concept(&mut cx, host, workspace, generation, "files/A");
    }
    assert!(cx.events.is_empty());
    assert!(cx.editor_content.is_none() && cx.concept_cache.is_none());
    concept(&mut cx, "active", None, 2, "files/A");
    continue_preview_failure(&mut RecordingReplies(&mut cx), "active".into(), None, 2, ());
    assert_eq!(cx.events, ["editor", "concept", "failure-status"]);
}

#[test]
fn preview_and_concept_clocks_advance_independently() {
    let mut cx = RecordingRequests::new(Some("current"));
    assert_eq!(cx.requests.issue_preview(), 1);
    cx.follow(Some("files/A"), None);
    assert_eq!(cx.reload("files/B"), 2);
    assert_eq!(cx.requests.issue_preview(), 2);
    cx.follow(Some("files/A"), None);
    assert_eq!(cx.sent, [("files/A".to_string(), 1, Some("current".to_string())), ("files/B".to_string(), 2, Some("current".to_string()))]);
    cx.events.clear();
    preview(&mut cx, "active", Some("current"), 2);
    concept(&mut cx, "active", Some("current"), 2, "files/A");
    assert_eq!(cx.events.last().map(String::as_str), Some("concept"));
    assert_eq!(cx.events.len(), 6);
    assert_eq!((cx.requests.preview_generation(), cx.requests.concept_generation()), (2, 2));
}

#[test]
fn concept_follow_fires_once_until_invalidated() {
    let mut cx = RecordingRequests::new(None);
    for (target, sends, clears) in [(Some("files/A"), 1, 1), (Some("files/A"), 1, 1),
        (Some("files/B"), 2, 2), (None, 2, 3), (None, 2, 3), (Some("files/B"), 3, 4)] {
        cx.follow(target, None);
        assert_eq!(cx.sent.len(), sends);
        assert_eq!(cx.events.iter().filter(|e| e.as_str() == "clear").count(), clears);
        assert_eq!(cx.requests.concept_target(), target);
        assert_eq!(cx.requests.concept_generation(), sends as u64);
    }
    cx.requests.invalidate_changed("files:.concept/files/B.md");
    cx.follow(Some("files/B"), None);
    assert_eq!(cx.sent.last().unwrap().1, 4);
    // Even an unchanged target still reaches the lazy drift gate.
    cx.follow(Some("files/B"), Some("B.jl"));
    cx.follow(Some("files/B"), Some("B.jl"));
    assert_eq!(cx.events.iter().filter(|e| e.as_str() == "send-parse").count(), 1);
    assert_eq!(cx.sent.len(), 4);
}

#[test]
fn failed_concept_send_consumes_a_number_without_committing_the_target() {
    let mut cx = RecordingRequests::new(None);
    cx.follow(Some("files/A"), None);
    concept(&mut cx, "active", None, 1, "files/A");
    cx.events.clear();
    cx.outcomes.push_back(false);
    cx.follow(Some("files/B"), Some("B.jl"));
    assert_eq!(cx.requests.concept_generation(), 2);
    assert_eq!(cx.requests.concept_target(), Some("files/A"));
    assert_eq!(cx.concept_cache.as_deref(), Some("fresh"));
    assert_eq!(cx.preview_concept_cache.as_deref(), Some("fresh"));
    assert_eq!(cx.events, ["send-concept"]);
    cx.events.clear();
    cx.follow(Some("files/B"), Some("B.jl"));
    assert_eq!(cx.sent.iter().map(|s| s.1).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(cx.requests.concept_target(), Some("files/B"));
    assert!(cx.concept_cache.is_none() && cx.preview_concept_cache.is_none());
    assert_eq!(cx.events, ["send-concept", "clear", "drift", "send-parse"]);
}

#[test]
fn concept_reload_shares_the_clock_without_retargeting_follow() {
    let mut cx = RecordingRequests::new(None);
    cx.follow(Some("files/A"), None);
    assert_eq!(cx.reload("files/B"), 2);
    cx.editor_target = Some("files/B".into());
    cx.events.clear();
    concept(&mut cx, "active", None, 1, "files/A");
    assert!(cx.events.is_empty());
    concept(&mut cx, "active", None, 2, "files/B");
    assert_eq!(cx.events, ["editor"]);
    assert_eq!(cx.editor_content.as_deref(), Some("fresh"));
    assert!(cx.concept_cache.is_none());
    assert_eq!(cx.requests.concept_target(), Some("files/A"));
    // The two consumers are independent, and editor replacement precedes cursor installation.
    cx.editor_target = Some("files/A".into());
    concept(&mut cx, "active", None, 2, "files/A");
    assert_eq!(cx.events, ["editor", "editor", "concept"]);
}

#[test]
fn annotation_invalidation_and_view_resets_rearm_without_resetting_clocks() {
    let mut cx = RecordingRequests::new(None);
    cx.requests.issue_preview();
    cx.follow(Some("files/A"), None);
    assert_eq!(cx.requests.invalidate_changed("files:.concept/files/other.md"), ChangeEffect::Nothing);
    assert_eq!(cx.requests.concept_target(), Some("files/A"));
    assert_eq!(cx.requests.invalidate_changed("files:A.jl"), ChangeEffect::ForgetFile("A.jl"));
    assert_eq!(cx.requests.concept_target(), Some("files/A"));
    assert_eq!(cx.requests.invalidate_changed("files:.concept/files/A.md"), ChangeEffect::ReadAnnotationAgain);
    assert_eq!(cx.requests.concept_target(), None);
    assert_eq!(cx.requests.concept_generation(), 1);
    cx.follow(Some("files/A"), None);
    for (i, transition) in ["restore", "first visit", "active reconnect"].iter().enumerate() {
        cx.requests.reset_concept_follow();
        assert_eq!(cx.requests.concept_target(), None, "{transition}");
        assert_eq!((cx.requests.preview_generation(), cx.requests.concept_generation()), (1, i as u64 + 2));
        cx.follow(Some("files/A"), None);
        assert_eq!(cx.requests.concept_generation(), i as u64 + 3);
    }
    assert_eq!(cx.sent.iter().map(|s| s.1).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
}

#[test]
fn driven_hold_suppresses_the_old_row_until_the_cursor_moves() {
    let mut requests = PreviewRequests::default();
    let cursor = "files:Old.jl".to_string();
    requests.arm_driven_hold(Some(cursor.clone()));
    for _ in 0..3 { assert!(requests.suppress_follow(&cursor)); }
    // Exact row spelling: this different candidate releases the hold immediately.
    assert!(!requests.suppress_follow("files:old.jl"));
    assert!(!requests.suppress_follow(&cursor));
    assert_eq!(cursor, "files:Old.jl");
    requests.arm_driven_hold(None);
    assert!(!requests.suppress_follow(&cursor));
}

#[test]
fn reveal_completion_abort_and_row_departure_clear_only_the_hold() {
    let transitions: [fn(&mut PreviewRequests); 3] = [
        PreviewRequests::finish_reveal, PreviewRequests::abort_reveal, PreviewRequests::leave_row,
    ];
    for clear in transitions {
        let mut cx = RecordingRequests::new(None);
        cx.requests.issue_preview();
        cx.requests.issue_preview();
        cx.follow(Some("files/A"), None);
        cx.requests.arm_driven_hold(Some("files:old".into()));
        assert!(cx.requests.suppress_follow("files:old"));
        clear(&mut cx.requests);
        assert!(!cx.requests.suppress_follow("files:old"));
        assert_eq!(cx.requests.concept_target(), Some("files/A"));
        assert_eq!((cx.requests.preview_generation(), cx.requests.concept_generation()), (2, 1));
    }
}

#[test]
fn concept_send_diagnostics_keep_target_fields_and_order() {
    let mut cx = RecordingRequests::new(None);
    cx.capture = Some(capture());
    cx.outcomes.push_back(false);
    cx.follow(Some("files/A"), Some("A.jl"));
    let text = cx.capture.as_ref().unwrap().text();
    assert!(text.contains("WARN sot::ui::preview::concept:") && text.contains("target=files/A"));
    assert!(text.contains("error=scripted send failure") && text.contains("drop concept.read request — channel closed"));
    assert!(!cx.logs_at_event[0].contains("drop concept.read"), "send precedes warning");
    assert_eq!(cx.events, ["send-concept"]);
    cx.follow(Some("files/A"), Some("A.jl"));
    assert_eq!(cx.events, ["send-concept", "send-concept", "clear", "drift", "send-parse"]);
    assert!(!cx.logs_at_event[2].contains("→ file.parse"), "cache clear precedes drift diagnostic");
    assert!(!cx.logs_at_event[3].contains("→ file.parse"), "drift admission precedes diagnostic");
    let text = &cx.logs_at_event[4];
    assert!(text.contains("DEBUG sot::ui::preview::concept:") && text.contains("→ file.parse for drift check"));
    assert!(text.contains("path=A.jl") && text.contains("label=source label"));
    cx.outcomes.extend([true, false]);
    cx.follow(Some("files/B"), Some("B.jl"));
    let text = cx.capture.as_ref().unwrap().text();
    assert!(text.contains("drop file.parse request — channel closed") && text.contains("path=B.jl"));
    assert_eq!(cx.events.last().map(String::as_str), Some("forget-parse"));
    assert!(!cx.parsed.contains("B.jl"));
}
