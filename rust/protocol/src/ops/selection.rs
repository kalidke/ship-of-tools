// the selection op (A1): a window tells the daemon which text of a previewed file it shows selected

use super::*;
use crate::chat::within;

/// The longest `workspace_id` (a row id, as the chat family bounds a row).
pub const MAX_SELECTION_WORKSPACE: usize = 256;
/// The longest `node_id`: a project path.
pub const MAX_SELECTION_NODE: usize = 4096;
/// The most bytes a selection may span, the daemon's read of the project file.
pub const MAX_SELECTION_LEN: u32 = 2 * 1024 * 1024;

/// The fixed refusal codes of `selection.set`, the `code` of the standard error payload.
pub mod selection_code {
    pub const UNKNOWN_WORKSPACE: &str = "unknown_workspace";
    pub const BAD_NODE_ID: &str = "bad_node_id";
    pub const NOT_FILE_TEXT: &str = "not_file_text";
    pub const NO_AGENT: &str = "no_agent";
    pub const NOT_A_WINDOW: &str = "not_a_window";
    pub const PUBLISH_FAILED: &str = "publish_failed";
}

/// FNV-1a 64 of the selected text's UTF-8 bytes, lines joined by LF: what the window computes over what it shows and
/// the daemon over the file's text at the range. The one definition.
pub fn selection_digest(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// A position in the preview buffer: a line, and a UTF-8 byte offset in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextPos {
    pub line: u32,
    pub byte: u32,
}

/// What a decoder reads before it checks the bounds a `SelectedText` keeps.
#[derive(Deserialize)]
struct RawSelectedText {
    #[serde(deserialize_with = "within::<_, MAX_SELECTION_NODE>")]
    node_id: String,
    start: TextPos,
    end: TextPos,
    len: u32,
    digest: u64,
}

/// The selected range of one node: `end` exclusive, `start` before `end`, `len` the bytes of the text the window shows
/// (1 to `MAX_SELECTION_LEN`). The text itself never travels. Decoding refuses a range that breaks these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawSelectedText")]
pub struct SelectedText {
    pub node_id: String,
    pub start: TextPos,
    pub end: TextPos,
    pub len: u32,
    pub digest: u64,
}

impl TryFrom<RawSelectedText> for SelectedText {
    type Error = String;
    fn try_from(r: RawSelectedText) -> Result<Self, String> {
        if (r.start.line, r.start.byte) >= (r.end.line, r.end.byte) {
            return Err("start is not before end".to_string());
        }
        if r.len == 0 || r.len > MAX_SELECTION_LEN {
            return Err(format!("len is not 1 to {MAX_SELECTION_LEN}"));
        }
        Ok(SelectedText { node_id: r.node_id, start: r.start, end: r.end, len: r.len, digest: r.digest })
    }
}

/// `selection.set`: `selection: null` clears. The field is required, so a request that lost it is refused rather than
/// read as a clear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionSetReq {
    #[serde(deserialize_with = "within::<_, MAX_SELECTION_WORKSPACE>")]
    pub workspace_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub selection: Option<SelectedText>,
}

/// `shared`: published and the row's agent takes it; `held`: published, the agent started without the hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionState {
    Shared,
    Held,
    /// A state added later: a reader takes it as not shared.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionSetRes {
    pub state: SelectionState,
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod selection_tests;
