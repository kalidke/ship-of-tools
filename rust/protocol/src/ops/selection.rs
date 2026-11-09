// the selection op (A1): a window tells the daemon which text of a previewed file it shows selected

use super::*;
use crate::chat::within;

/// The longest `workspace_id` (a row id, as the chat family bounds a row).
pub const MAX_SELECTION_WORKSPACE: usize = 256;
/// The longest `node_id`: a project path.
pub const MAX_SELECTION_NODE: usize = 4096;
/// The most bytes a selection may span, the daemon's read of the project file.
pub const MAX_SELECTION_LEN: u32 = 2 * 1024 * 1024;
/// The most points a drawing may hold in all strokes together; each stroke has a point, so this also bounds the strokes.
pub const MAX_INK_POINTS: usize = 16384;

/// The fixed refusal codes of `selection.set`, the `code` of the standard error payload.
pub mod selection_code {
    pub const UNKNOWN_WORKSPACE: &str = "unknown_workspace";
    pub const BAD_NODE_ID: &str = "bad_node_id";
    pub const NOT_FILE_TEXT: &str = "not_file_text";
    pub const NO_AGENT: &str = "no_agent";
    pub const NOT_A_WINDOW: &str = "not_a_window";
    pub const PUBLISH_FAILED: &str = "publish_failed";
    pub const NOT_AN_IMAGE: &str = "not_an_image";
    pub const TOO_LARGE: &str = "too_large";
}

/// FNV-1a 64 of the selected text's UTF-8 bytes, lines joined by LF: what the window computes over what it shows and
/// the daemon over the file's text at the range. The one definition.
pub fn selection_digest(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// The range and text of the 1-based lines `first..=last` of `text`, as `selection.set` names a file's text and the
/// daemon's check reads it: a line ends at LF, a CR before the LF is dropped, the range runs from the start of `first`
/// to the end of `last` (`end` exclusive, positions `(line, byte)` from 0), and the text is those lines joined by LF.
/// `None` when `first` is 0 or after `last`, `last` is past the text, or the lines hold no byte.
pub fn whole_lines(text: &str, first: usize, last: usize) -> Option<(((usize, usize), (usize, usize)), String)> {
    if first == 0 || first > last {
        return None;
    }
    let lines: Vec<&str> =
        text.split('\n').skip(first - 1).take(last - first + 1).map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    if lines.len() != last - first + 1 {
        return None;
    }
    let end = (last - 1, lines.last()?.len());
    let joined = lines.join("\n");
    (!joined.is_empty()).then_some((((first - 1, 0), end), joined))
}

/// A position in the shown file's text: a 0-based line and a UTF-8 byte offset in it (the preview buffer's own for a
/// plain or token preview, a source line for rendered markdown, `whole_lines`).
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

/// What a drawing is drawn on: the row's blank canvas, or an image node of the project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InkBase {
    Canvas,
    Figure {
        #[serde(deserialize_with = "within::<_, MAX_SELECTION_NODE>")]
        node_id: String,
    },
}

/// What a decoder reads before it checks the bounds a `Drawing` keeps.
#[derive(Deserialize)]
struct RawDrawing {
    base: InkBase,
    strokes: Vec<Vec<[u16; 2]>>,
}

/// Ink over a base: each point is `[x, y]`, `x / 65535` and `y / 65535` the fractions of the base's width and height.
/// At least one stroke, each with a point, and at most `MAX_INK_POINTS` points in all. A
/// one-point stroke is a dot. Decoding refuses a drawing that breaks these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawDrawing")]
pub struct Drawing {
    pub base: InkBase,
    pub strokes: Vec<Vec<[u16; 2]>>,
}

impl TryFrom<RawDrawing> for Drawing {
    type Error = String;
    fn try_from(r: RawDrawing) -> Result<Self, String> {
        if r.strokes.is_empty() || r.strokes.iter().any(Vec::is_empty) {
            return Err("a drawing needs strokes, each with a point".to_string());
        }
        if r.strokes.iter().map(Vec::len).sum::<usize>() > MAX_INK_POINTS {
            return Err(format!("more than {MAX_INK_POINTS} points"));
        }
        Ok(Drawing { base: r.base, strokes: r.strokes })
    }
}

/// What a decoder reads before it checks that the request shares one thing.
#[derive(Deserialize)]
struct RawSelectionSetReq {
    #[serde(deserialize_with = "within::<_, MAX_SELECTION_WORKSPACE>")]
    workspace_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    selection: Option<SelectedText>,
    #[serde(default)]
    drawing: Option<Drawing>,
}

/// `selection.set`: `selection: null` and no `drawing` clears; at most one of the two is set. The `selection` field is
/// required, so a request that lost it is refused rather than read as a clear; `drawing` defaults to none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawSelectionSetReq")]
pub struct SelectionSetReq {
    pub workspace_id: String,
    pub selection: Option<SelectedText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drawing: Option<Drawing>,
}

impl TryFrom<RawSelectionSetReq> for SelectionSetReq {
    type Error = String;
    fn try_from(r: RawSelectionSetReq) -> Result<Self, String> {
        if r.selection.is_some() && r.drawing.is_some() {
            return Err("both a selection and a drawing".to_string());
        }
        Ok(SelectionSetReq { workspace_id: r.workspace_id, selection: r.selection, drawing: r.drawing })
    }
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
