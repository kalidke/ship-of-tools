// ir.rs — Rust mirrors of the Julia core IR types.
//
// `TreeNode` and `PreviewPayload` here serialize to the same JSON the Julia
// kernel emits from `core/src/ConceptExplorerCore.jl`. Treat both as one
// shared schema; the moment the Julia struct gains a field, this one does too.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeNode {
    pub id: String,
    pub label: String,
    /// `:module`, `:function`, `:pngfile`, … — Julia-side this is a Symbol;
    /// the wire form is the symbol's name as a plain string.
    pub kind: String,
    pub has_children: bool,
    #[serde(default)]
    pub badges: Vec<String>,
    #[serde(default)]
    pub payload: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewPayload {
    pub mime: String,
    pub blob: BlobDescriptor,
    #[serde(default)]
    pub extras: serde_json::Map<String, serde_json::Value>,
}

/// Tells the codec how many bytes to read after the envelope. `mime` is the
/// canonical content type; the outer payload may repeat it for convenience.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobDescriptor {
    pub len: u64,
    pub mime: String,
}
