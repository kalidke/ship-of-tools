// preview/ — preview-layer surface (parallel to ratatui chrome).
//
// Per ADR 0011: takes a Rect from ratatui's layout pass plus a PreviewPayload
// from the kernel; draws directly into the wgpu surface inside that rect,
// bypassing the cell stream entirely.

pub(crate) mod image;
pub(crate) mod markdown;
pub(crate) mod editor;
pub(crate) mod pane;
pub(crate) mod concept;
mod fetch;
pub(in crate::ui) mod keys;
mod open;
mod replies;
pub(in crate::ui) use fetch::reply_is_current;
pub(crate) use crate::ui::render::quad;

pub(crate) use image::{png, svg};
pub(crate) use markdown::highlight;
