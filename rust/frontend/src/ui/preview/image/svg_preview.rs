//! The frame's one step over the drawing worker's outcomes (`svg_draws`): the worker is told the pane's generation, its
//! channel is drained once, and each outcome goes to its owner by key. The owners are the markdown figures (a key is
//! the figure's URL) and, in a later landing, the SVG file preview (`PREVIEW_KEY`).

use super::figure_store::{FigureStore, Settled};
use super::svg_draws::SvgDraws;

/// The preview's key at the worker. A figure's key is a URL that resolved to a project file, and none holds `://`
/// (`resolve_figure_node_id`), so no figure has this one.
pub(super) const PREVIEW_KEY: &str = "://preview";

/// The top of a frame: the worker is told the pane's `generation`, then every outcome it has returned goes to its
/// owner; the window acts on each figure that settled. An outcome under `PREVIEW_KEY` has no owner yet and is dropped.
pub(in crate::ui) fn settle_frame(draws: &mut SvgDraws, figures: &mut FigureStore, generation: u64) -> Vec<(String, Settled)> {
    draws.advance(generation);
    let mut done = Vec::new();
    draws.drain(|each| done.push(each));
    let mut settled = Vec::new();
    for each in done {
        if each.key == PREVIEW_KEY {
            continue;
        }
        let url = each.key.clone();
        match figures.settle_drawing(each) {
            Settled::Ignored => {}
            other => settled.push((url, other)),
        }
    }
    settled
}
