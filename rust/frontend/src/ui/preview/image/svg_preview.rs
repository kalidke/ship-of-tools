//! An `.svg` file's preview: its picture drawn by the worker (`svg_draws`) at the pane's size, and the frame's one step
//! that hands the worker's outcomes to their owners. The window keeps only the upload.
//!
//! The store keeps the last two outcomes, a picture or a refusal, each with the bytes and the ask it came from, so a
//! switch between two rows that each show an SVG draws neither again and a refused file costs one child, not one per
//! return. The one job in flight is of the pane's generation; an outcome of another generation (a departed source's,
//! which can arrive after a resubmit) is ignored.

use std::collections::VecDeque;
use std::sync::Arc;

use super::figure_store::{FigureStore, Settled};
use super::svg_draws::{Done, SvgDraws};
use super::svg_run::{Job, Outcome};
use super::svg_wire::Ask;

/// The preview's key at the worker. A figure's key is a URL that resolved to a project file, and none holds `://`
/// (`resolve_figure_node_id`), so no figure has this one.
pub(super) const PREVIEW_KEY: &str = "://preview";

/// How many outcomes are kept.
const KEPT: usize = 2;

/// A drawn picture: `w` by `h` RGBA bytes.
pub(in crate::ui) struct Picture {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// What `show` and the frame's step tell the window.
pub(in crate::ui) enum Step {
    /// A picture to upload and install.
    Picture(Arc<Picture>),
    /// Refused, in the words the status line says: the text stays.
    Refused(String),
    /// With the worker: the text stays until the picture comes.
    Pending,
}

struct Kept {
    svg: Arc<[u8]>,
    ask: Ask,
    result: Result<Arc<Picture>, String>,
}

struct InFlight {
    generation: u64,
    svg: Arc<[u8]>,
    ask: Ask,
}

#[derive(Default)]
pub(in crate::ui) struct SvgPreview {
    /// The last `KEPT` outcomes, the newest last.
    kept: VecDeque<Kept>,
    in_flight: Option<InFlight>,
}

impl SvgPreview {
    /// The preview of `bytes` asked as `ask`, for the source of `generation`: a kept outcome with no child; the job
    /// already in flight for this generation (one submission per generation until it settles); else a new job.
    pub(in crate::ui) fn show(&mut self, draws: &mut SvgDraws, generation: u64, bytes: &[u8], ask: Ask) -> Step {
        if let Some(kept) = self.kept.iter().find(|kept| kept.ask == ask && *kept.svg == *bytes) {
            return kept.result.clone().map_or_else(Step::Refused, Step::Picture);
        }
        if self.in_flight.as_ref().is_some_and(|job| job.generation == generation) {
            return Step::Pending;
        }
        let svg: Arc<[u8]> = bytes.into();
        match draws.submit(PREVIEW_KEY.into(), generation, Job { ask, svg: svg.clone() }) {
            Ok(()) => {
                self.in_flight = Some(InFlight { generation, svg, ask });
                Step::Pending
            }
            Err(why) => {
                self.keep(svg, ask, Err(why.to_owned()));
                Step::Refused(why.to_owned())
            }
        }
    }

    /// The worker's outcome for the preview. Only the outcome of the generation in flight counts; a stale one frees
    /// the slot, and the pane's current source submits again when it is shown.
    fn settle(&mut self, done: Done) -> Option<Step> {
        if self.in_flight.as_ref().is_none_or(|job| job.generation != done.generation) {
            return None;
        }
        let job = self.in_flight.take()?;
        let result = match done.outcome {
            Outcome::Drawn { w, h, rgba } => Ok(Arc::new(Picture { w, h, rgba })),
            Outcome::Refused(why) | Outcome::Ended(why) => Err(why),
            Outcome::Stale => return None,
        };
        self.keep(job.svg, job.ask, result.clone());
        Some(result.map_or_else(Step::Refused, Step::Picture))
    }

    fn keep(&mut self, svg: Arc<[u8]>, ask: Ask, result: Result<Arc<Picture>, String>) {
        self.kept.retain(|kept| !(kept.ask == ask && *kept.svg == *svg));
        if self.kept.len() == KEPT {
            self.kept.pop_front();
        }
        self.kept.push_back(Kept { svg, ask, result });
    }
}

/// What a frame's step settled: each figure's outcome, and the preview's.
pub(in crate::ui) struct Frame {
    pub figures: Vec<(String, Settled)>,
    pub preview: Option<Step>,
}

/// The top of a frame: the worker is told the pane's `generation`, then every outcome it has returned goes to its
/// owner (the preview by `PREVIEW_KEY`, else a figure), and the window acts on what settled.
pub(in crate::ui) fn settle_frame(
    draws: &mut SvgDraws, figures: &mut FigureStore, preview: &mut SvgPreview, generation: u64,
) -> Frame {
    draws.advance(generation);
    let mut done = Vec::new();
    draws.drain(|each| done.push(each));
    let mut frame = Frame { figures: Vec::new(), preview: None };
    for each in done {
        if each.key == PREVIEW_KEY {
            frame.preview = preview.settle(each).or(frame.preview);
            continue;
        }
        let url = each.key.clone();
        match figures.settle_drawing(each) {
            Settled::Ignored => {}
            other => frame.figures.push((url, other)),
        }
    }
    frame
}

#[cfg(test)]
#[path = "svg_preview_tests.rs"]
mod tests;
