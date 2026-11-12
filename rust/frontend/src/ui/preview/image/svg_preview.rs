//! An `.svg` file's preview: its picture drawn by the worker (`svg_draws`) at the pane's size, and the frame's one step
//! that hands the worker's outcomes to their owners. The window keeps only the upload.
//!
//! The store keeps the last two outcomes, each with a digest of the file's bytes (SHA-256 and the length), never the
//! bytes: a drawn picture with the ask it was drawn for, a refusal for the file alone, so a switch between two rows that
//! each show an SVG draws neither again, and a refused file costs one child, not one per return or per size. The one job
//! in flight is of the pane's generation; an outcome of another generation (a departed source's, which can arrive after
//! a resubmit) is ignored. While a picture of the shown source is installed, a redraw at another size leaves it up
//! (`Step::Held`) until the new picture replaces it.

use std::collections::VecDeque;
use std::sync::Arc;

use sha2::{Digest as _, Sha256};

use super::figure_store::{FigureStore, Settled};
use super::svg_draws::{Done, SvgDraws};
use super::svg_run::{Job, Outcome};
use super::svg_wire::Ask;
use crate::ui::preview::markdown::media_store::{is_math_job, MarkdownMedia};

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
    /// A picture of this source is installed and its redraw is with the worker: leave the pane as it is.
    Held,
}

/// A file's identity in the store: its bytes' SHA-256 and length.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Digest([u8; 32], usize);

impl Digest {
    fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into(), bytes.len())
    }
}

enum Kept {
    Drawn { digest: Digest, ask: Ask, picture: Arc<Picture> },
    Refused { digest: Digest, why: String },
}

impl Kept {
    /// Whether this outcome answers a preview of `digest` asked as `ask`: a refusal answers the file at any size.
    fn answers(&self, digest: Digest, ask: Ask) -> bool {
        match self {
            Kept::Drawn { digest: d, ask: a, .. } => *d == digest && *a == ask,
            Kept::Refused { digest: d, .. } => *d == digest,
        }
    }
}

struct InFlight {
    generation: u64,
    digest: Digest,
    ask: Ask,
}

#[derive(Default)]
pub(in crate::ui) struct SvgPreview {
    /// The last `KEPT` outcomes, the newest last.
    kept: VecDeque<Kept>,
    in_flight: Option<InFlight>,
    /// The generation whose picture is installed, when one is.
    installed: Option<u64>,
    /// The digest of the source of one generation (a generation is one source, so its bytes are read once).
    digest: Option<(u64, Digest)>,
}

impl SvgPreview {
    /// The preview of `bytes` asked as `ask`, for the source of `generation`: a kept outcome with no child; the job
    /// already in flight for this generation (one submission per generation until it settles); else a new job.
    pub(in crate::ui) fn show(&mut self, draws: &mut SvgDraws, generation: u64, bytes: &[u8], ask: Ask) -> Step {
        let digest = match self.digest {
            Some((of, digest)) if of == generation => digest,
            _ => {
                let digest = Digest::of(bytes);
                self.digest = Some((generation, digest));
                digest
            }
        };
        let held = self.installed == Some(generation);
        if let Some(kept) = self.kept.iter().find(|kept| kept.answers(digest, ask)) {
            return match kept {
                Kept::Drawn { picture, .. } => {
                    self.installed = Some(generation);
                    Step::Picture(picture.clone())
                }
                Kept::Refused { why, .. } => {
                    self.installed = None;
                    Step::Refused(why.clone())
                }
            };
        }
        if self.in_flight.as_ref().is_none_or(|job| job.generation != generation) {
            let job = Job { ask, svg: bytes.into() };
            if let Err(why) = draws.submit(PREVIEW_KEY.into(), generation, job) {
                self.keep(Kept::Refused { digest, why: why.to_owned() });
                self.installed = None;
                return Step::Refused(why.to_owned());
            }
            self.in_flight = Some(InFlight { generation, digest, ask });
        }
        if held {
            return Step::Held;
        }
        self.installed = None;
        Step::Pending
    }

    /// The picture last handed out was not installed (the upload failed).
    pub(in crate::ui) fn not_installed(&mut self) {
        self.installed = None;
    }

    /// The worker's outcome for the preview. Only the outcome of the generation in flight counts; a stale one frees
    /// the slot, and the pane's current source submits again when it is shown.
    fn settle(&mut self, done: Done) -> Option<Step> {
        if self.in_flight.as_ref().is_none_or(|job| job.generation != done.generation) {
            return None;
        }
        let InFlight { generation, digest, ask } = self.in_flight.take()?;
        match done.outcome {
            Outcome::Drawn { w, h, rgba } => {
                let picture = Arc::new(Picture { w, h, rgba });
                self.keep(Kept::Drawn { digest, ask, picture: picture.clone() });
                self.installed = Some(generation);
                Some(Step::Picture(picture))
            }
            Outcome::Refused(why) | Outcome::Ended(why) => {
                self.keep(Kept::Refused { digest, why: why.clone() });
                Some(Step::Refused(why))
            }
            Outcome::Stale => None,
        }
    }

    fn keep(&mut self, new: Kept) {
        let same = |kept: &Kept| match (&new, kept) {
            (Kept::Drawn { digest, ask, .. }, Kept::Drawn { digest: d, ask: a, .. }) => d == digest && a == ask,
            (Kept::Refused { digest, .. }, Kept::Refused { digest: d, .. }) => d == digest,
            _ => false,
        };
        self.kept.retain(|kept| !same(kept));
        if self.kept.len() == KEPT {
            self.kept.pop_front();
        }
        self.kept.push_back(new);
    }
}

/// What a frame's step settled: each figure's outcome, each math snippet's, and the preview's.
pub(in crate::ui) struct Frame {
    pub figures: Vec<(String, Settled)>,
    pub math: Vec<((String, bool), Settled)>,
    pub preview: Option<Step>,
}

/// The top of a frame: the worker is told the pane's `generation`, then every outcome it has returned goes to its
/// owner (the preview by `PREVIEW_KEY`, a math snippet by its prefix, else a figure), and the window acts on what settled.
pub(in crate::ui) fn settle_frame(
    draws: &mut SvgDraws, figures: &mut FigureStore, preview: &mut SvgPreview, media: &mut MarkdownMedia, generation: u64,
) -> Frame {
    draws.advance(generation);
    let mut done = Vec::new();
    draws.drain(|each| done.push(each));
    let mut frame = Frame { figures: Vec::new(), math: Vec::new(), preview: None };
    for each in done {
        if each.key == PREVIEW_KEY {
            frame.preview = preview.settle(each).or(frame.preview);
            continue;
        }
        if is_math_job(&each.key) {
            frame.math.extend(media.settle_drawing(each));
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
