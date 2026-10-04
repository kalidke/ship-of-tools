//! The `sot_ui` nav envelope an agent drives in its message text.

use super::*;

impl State {
    /// Item 2: a session drove a `nav.preview` envelope at us. Act ONLY when
    /// it targets our currently-active workspace (the gate the maintainer and I locked —
    /// a broadcast reaches every FE, so each acts only for the workspace it's
    /// viewing; others ignore it, and it's NEVER rendered as chat either way).
    /// On a match: switch to Files mode and fire `preview.get` for
    /// `files:<path>`. node ids are workspace-relative and the backend
    /// resolves them directly, so no tree expansion is required to show the
    /// file. (Cursor-reveal — expanding the tree to select the row — is a
    /// follow-up; the preview pane is the payload of `nav.preview`.)
    pub(in crate::ui) fn handle_nav_envelope(&mut self, host: &HostKey, env: &NavEnvelope) {
        let current = self
            .active_workspace_id
            .clone()
            .or_else(|| self.default_workspace_slug.clone());
        // ADR 0042 L2a codex review, item E: the slug alone isn't enough
        // — two hosts can share a slug (their own default workspace, say),
        // so this must also confirm the push arrived on active_host.
        if host != &self.active_host || current.as_deref() != Some(env.workspace.as_str()) {
            // Badge floor (ADR 0025 §1): the result targets a workspace we're
            // not viewing (or arrived from a non-active host). Don't silently
            // drop it — record + badge it so it reaches the user when they
            // switch to that workspace.
            tracing::debug!(target_host = %host, target_ws = %env.workspace, ?current,
                active_host = %self.active_host,
                "nav.preview targets a non-active (host, workspace) — badging as pending (not chat)");
            self.mark_pending_nav(host.clone(), env.workspace.clone(), env.path.clone());
            return;
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
