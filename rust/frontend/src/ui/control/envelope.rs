//! The `sot_ui` nav envelope an agent drives in its message text.

use super::*;

impl State {
    /// Goto, preview, reveal and nav envelopes resolve the producing host's listed slug or id once; an unknown target changes no view, badge, caption or ROI and reports a refusal.
    pub(in crate::ui) fn handle_nav_envelope(&mut self, host: &HostKey, env: &NavEnvelope) {
        match self.result_route(host, &env.workspace, false, false) {
            super::dispatch::ResultRoute::Render(_) => {}
            super::dispatch::ResultRoute::Badge(target) => {
                let (host, slug) = target.row_key().clone();
                self.mark_pending_nav(host, slug, env.path.clone());
                return;
            }
            super::dispatch::ResultRoute::Refusal(reason) => {
                self.refuse_result(&reason);
                return;
            }
            super::dispatch::ResultRoute::Switch(_) => {
                unreachable!("nav envelope never force-switches")
            }
        }
        // Drive both panes via the shared same-ws open: preview body now, and a
        // deep-path cursor reveal so the cursor follows the file even when its
        // ancestor dirs aren't expanded yet. Without the reveal the header kept
        // labelling the OLD cursor node while the body showed the driven file —
        // the header/body mismatch the maintainer hit (preview · circles.png over
        // KNOWLEDGE_BASE.md content).
        self.drive_same_ws_open(&env.path);
        self.status = format!("nav ← agent · {}", env.path);
        self.window.request_redraw();
        tracing::info!(node_id = %format!("files:{}", env.path), ws = %env.workspace,
            "nav.preview driven by agent");
    }
}

/// A parsed `sot_ui` nav command (Item 2 — a session driving this FE's
/// nav). Carried in an `agent.message` text payload; intercepted before the
/// inbox append so it never renders as chat. v1 is `nav.preview` only.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::ui) struct NavEnvelope {
    /// Workspace slug the path is relative to — the FE acts only when this
    /// matches its currently-active workspace (the gate).
    workspace: String,
    /// Workspace-relative file path to preview (→ `files:<path>` node id).
    path: String,
}

/// Parse an `agent.message` text payload as a v1 `sot_ui` nav.preview
/// envelope. Exact shape (anything else → `None`, so the caller falls
/// through to ordinary inbox/chat rendering):
///   {"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"<slug>",
///                 "mode":"files","path":"<ws-rel>"}}
/// Pure + total so it's unit-testable and can't panic in the event drain.
/// `mode` is accepted-and-ignored in v1 (only files-mode preview exists);
/// `cmd` leaves room to grow. The path is taken verbatim — the emitter
/// already relativized any absolute path against the workspace root.
pub(in crate::ui) fn parse_nav_envelope(text: &str) -> Option<NavEnvelope> {
    let v: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let ui = v.get("sot_ui")?;
    if ui.get("v").and_then(|x| x.as_i64()) != Some(1) {
        return None;
    }
    if ui.get("cmd").and_then(|x| x.as_str()) != Some("nav.preview") {
        return None;
    }
    let workspace = ui.get("workspace").and_then(|x| x.as_str())?.to_string();
    let path = ui.get("path").and_then(|x| x.as_str())?.to_string();
    if workspace.is_empty() || path.is_empty() {
        return None;
    }
    Some(NavEnvelope { workspace, path })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_envelope_parses_valid_v1() {
        let txt = r#"{"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"mypackage","mode":"files","path":"src/edge.jl"}}"#;
        assert_eq!(
            parse_nav_envelope(txt),
            Some(NavEnvelope {
                workspace: "mypackage".to_string(),
                path: "src/edge.jl".to_string(),
            })
        );
    }

    #[test]
    fn nav_envelope_tolerates_surrounding_whitespace() {
        let txt = "  \n{\"sot_ui\":{\"v\":1,\"cmd\":\"nav.preview\",\"workspace\":\"w\",\"path\":\"a/b.md\"}}\n ";
        assert_eq!(
            parse_nav_envelope(txt),
            Some(NavEnvelope {
                workspace: "w".to_string(),
                path: "a/b.md".to_string()
            })
        );
    }

    #[test]
    fn nav_envelope_rejects_ordinary_chat() {
        // Ordinary prose, empty, malformed JSON, and valid-but-foreign JSON
        // all fall through to normal inbox rendering.
        assert_eq!(parse_nav_envelope("hey, look at src/edge.jl?"), None);
        assert_eq!(parse_nav_envelope(""), None);
        assert_eq!(parse_nav_envelope("{not json"), None);
        assert_eq!(parse_nav_envelope(r#"{"hello":"world"}"#), None);
    }

    #[test]
    fn nav_envelope_rejects_wrong_version_cmd_or_missing_fields() {
        // Wrong version.
        assert_eq!(
            parse_nav_envelope(
                r#"{"sot_ui":{"v":2,"cmd":"nav.preview","workspace":"w","path":"p"}}"#
            ),
            None
        );
        // Unknown cmd (v1 only handles nav.preview).
        assert_eq!(
            parse_nav_envelope(r#"{"sot_ui":{"v":1,"cmd":"nav.jump","workspace":"w","path":"p"}}"#),
            None
        );
        // Missing path / missing workspace.
        assert_eq!(
            parse_nav_envelope(r#"{"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"w"}}"#),
            None
        );
        assert_eq!(
            parse_nav_envelope(r#"{"sot_ui":{"v":1,"cmd":"nav.preview","path":"p"}}"#),
            None
        );
        // Empty workspace / path.
        assert_eq!(
            parse_nav_envelope(
                r#"{"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"","path":"p"}}"#
            ),
            None
        );
        assert_eq!(
            parse_nav_envelope(
                r#"{"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"w","path":""}}"#
            ),
            None
        );
    }
}
