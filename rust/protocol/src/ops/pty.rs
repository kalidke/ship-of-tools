// a row's agent pane: pty.open and the named-row input and screen ops

use super::*;

/// Attach a row's agent pane, sized (cols, rows). Every row is a
/// capsule (ADR 0046): the daemon always answers with an
/// `attach_direct` refusal (see `PtyAttachDirect`'s doc at the
/// `attach_direct` code site in the backend), never a size-confirming
/// `PtyOpenRes` success.
///
/// `target` selects the row: `None` defaults to the daemon's own
/// default target; Sessions mode (ADR 0013) passes a backend session
/// name so the BL pane shows that backend's row instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyOpenReq {
    pub cols: u16,
    pub rows: u16,
    /// Tmux session name. `None` → `sot-llm`. Sessions mode passes the
    /// `sot-be-<slug>` name from ADR 0013 paths.
    #[serde(default)]
    pub target: Option<String>,
    /// True ONLY when this open is an explicit USER workspace-switch (the FE
    /// sets it at `switch_to_workspace` → `attach_session_to_bl`). The daemon
    /// re-targets the single foreground pty (ADR-0014) to a DIFFERENT session
    /// ONLY when this is true; a background/roaming re-attach or a daemon-boot
    /// open leaves it false so it can't yank the foreground away from where the
    /// user put it (the #5 single-pty thrash that froze create-session for
    /// ~1min). `serde(default)` = false keeps the wire compatible with an FE
    /// that predates the field.
    #[serde(default)]
    pub user_switch: bool,
}

/// A size-confirmation success case — no runtime on this branch ever
/// answers `pty.open` with one; every row is a capsule (ADR 0046) and
/// the daemon always refuses with `attach_direct` instead (see
/// `PtyAttachDirect`). Kept for wire compatibility with an old daemon
/// build that might still send it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyOpenRes {
    pub cols: u16,
    pub rows: u16,
}

/// Resize an already-open pty (e.g. when the LLM pane changes size).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyResizeReq {
    pub cols: u16,
    pub rows: u16,
}

/// User keystroke bytes from the LLM pane → the pty. Carried in the
/// envelope payload as a base64 blob to keep the wire JSON-safe;
/// terminal key sequences include escape (`\x1b`) and other binary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyWriteReq {
    /// Base64-encoded byte string.
    pub data_b64: String,
}

/// pty → frontend bytes. Same base64 encoding for the same reasons.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyEvt {
    pub data_b64: String,
}

/// One keyboard page-scroll of the LLM pane's tmux scrollback (see
/// `op::PTY_SCROLL`). `direction` is "up" | "down"; anything else is
/// treated as "up" backend-side rather than erroring (fire-and-forget op).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyScrollReq {
    pub direction: String,
}

/// `op::PTY_INPUT` — a session types into a NAMED row, answered (unlike
/// `PtyWriteReq`, which is always THIS connection's own pty and
/// fire-and-forget). `data_b64` is the same base64-encoded byte string
/// `PtyWriteReq` carries. `enter`: append the byte a terminal sends for
/// Enter, applied by the DAEMON in a runtime-appropriate way — a literal
/// CR (`0x0d`) on a capsule row, a separate `send-keys Enter` after the
/// literal text on a tmux row (`send-keys -l` mangles a trailing newline
/// inside the literal text itself, so the text is never altered to carry
/// it). `origin` names the controller for the record's own controller-actor
/// frames; `#[serde(default)]` so an old client that omits it still parses.
/// **`origin` is ATTRIBUTION, not authentication** — the caller's CLAIMED
/// handle, exactly like `HelloReq::client_id`. The record stores who typed,
/// how many bytes, and when; it never stores the content (redacted in the
/// WAL) and this op grants no privilege `origin` alone could forge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyInputReq {
    pub workspace_id: String,
    /// Base64-encoded byte string — see `PtyWriteReq::data_b64`.
    pub data_b64: String,
    #[serde(default)]
    pub enter: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// Whether a `pty.input` Enter reached the row. `Sent` only when the Enter byte was recorded; `NotSent` only when
/// Enter was never attempted or the supervisor refused it, so nothing was written; every other outcome is `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PtyEnter {
    Sent,
    NotSent,
    #[default]
    Unknown,
}

/// `op::PTY_INPUT` response: `ok` is `true` iff the daemon delivered the
/// bytes to the row's own input path (tmux `send-keys`, or a capsule's
/// `InputRecorded`); `bytes` is the payload length delivered (the `enter`
/// byte, if requested, is not counted — it rides the runtime's own
/// separate mechanism, not the payload). `enter` is `sent` iff the Enter
/// byte was written and recorded (never a claim the row treated it as a
/// submitted turn), `not_sent` iff Enter was not requested or the
/// supervisor refused it, and `unknown` otherwise (a reply without the
/// field reads as `unknown`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyInputRes {
    pub ok: bool,
    pub runtime: String,
    pub bytes: usize,
    #[serde(default)]
    pub enter: PtyEnter,
}

/// `op::PTY_SCREEN` request: the row to read, by workspace_id (accepted as
/// either a workspace_id or a slug — see `TreeRootReq::workspace_id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyScreenReq {
    pub workspace_id: String,
}

/// A cursor position within `PtyScreenRes::lines`. 0-based, matching the
/// vt100 parser's own indexing (row 0 = the top visible line).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PtyCursor {
    pub row: u16,
    pub col: u16,
}

/// `op::PTY_SCREEN` response: the row's CURRENT screen only — no
/// scrollback, no history. `lines.len() == rows as usize`, one entry per
/// visible row, trailing spaces trimmed, blank rows kept as empty strings.
/// `cursor` is `None` only when the runtime could not report one (never
/// for a healthy row of either kind).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PtyScreenRes {
    pub runtime: String,
    pub cols: u16,
    pub rows: u16,
    pub lines: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<PtyCursor>,
}

#[cfg(test)]
#[path = "pty_input_screen_tests.rs"]
mod pty_input_screen_tests;
