//! The markdown media the walk and the paint read: wide tables' buffers and their shared scroll, rendered math,
//! per-fence token overlays, and the one-shot reflow a fresh cache entry asks for.

use std::collections::{HashMap, HashSet};

use super::media::{MathSvg, TableBufferEntry, MATHJAX_EX_FACTOR};
use crate::ui::preview::image::figure_store::Settled;
use crate::ui::preview::image::svg_draws::{Done, SvgDraws};
use crate::ui::preview::image::svg_run::{Job, Outcome};
use crate::ui::preview::image::svg_wire::Ask;
use super::{MathMetrics, MathMetricsMap, BODY_SIZE as MD_BODY_SIZE};
use crate::net::transport::MarkdownToken;
use crate::ui::render::quad::ScreenRect;

/// A math snippet's key at the drawing worker. A figure's is a URL that resolved to a project file, none of which holds
/// `://` (`resolve_figure_node_id`), so the two never meet.
const JOB_PREFIX: &str = "://math/";

fn job_key((latex, display): &(String, bool)) -> String {
    format!("{JOB_PREFIX}{}{latex}", if *display { 'd' } else { 'i' })
}

/// Whether `job` is a math snippet's key at the worker.
pub(in crate::ui) fn is_math_job(job: &str) -> bool { job.starts_with(JOB_PREFIX) }

fn math_key(job: &str) -> Option<(String, bool)> {
    let rest = job.strip_prefix(JOB_PREFIX)?;
    let display = match rest.chars().next()? {
        'd' => true,
        'i' => false,
        _ => return None,
    };
    Some((rest[1..].to_owned(), display))
}

/// The most bytes of rendered table text (`TableBufferEntry::charge`) kept for documents not shown. Shaped, a byte of it
/// held 76 to 520 bytes in the measurements (the worst is padding: a table with one long cell pads every row to it),
/// so the kept sets hold up to about 104 MB beside the shown set, which nothing bounds (the text cap bounds the
/// markdown file, not the text its tables render to). 200,000 keeps one file of the diagnosed size (140,198 bytes of
/// rendered table text) beside the shown one; a document with more than this is never kept.
pub(in crate::ui) const PARKED_TABLE_BYTES: usize = 200_000;

/// The time a frame that shapes no row in view spends shaping rows beyond the view, nearest it first, until the set is
/// whole: rows until it has passed, so at least one. It is the quantity itself, since nothing countable in a row's
/// text follows its cost (cosmic-text shapes a word for each break opportunity and one for each space, so padding and
/// CJK text cost what their words do not say). A row that alone takes longer is still the unit, and one row can be any
/// length. A person's key that comes during the frame waits for it: at 3 ms a fill frame was 3 to 4.5 ms longer than
/// the same page's frame with nothing to shape, on every page measured whose rows each take less than that; on the
/// three pages also tried at 2 and at 4 ms it was 2.6 to 3.2 and 4.5 to 5.1 ms longer, and 4 ms ended the fill only 6
/// to 20 percent sooner. The frames and times to a whole set are in `docs/defects.md`,
/// PREVIEW-TABLES-FIRST-VIEW-SHAPES-ALL.
pub(in crate::ui) const BACKGROUND_SHAPING: std::time::Duration = std::time::Duration::from_millis(3);

/// Math and token requests go out only for keys neither cached nor in flight; a reply caches its entry, settles the
/// key and asks for one reflow, which the frame takes once.
#[derive(Default)]
pub(in crate::ui) struct MarkdownMedia {
    /// Horizontal scroll for wide markdown tables. Shared across every
    /// `MediaBlock::Table` on the current preview — Windows's (e) work
    /// item ships Path 1, where each table buffer is at natural width
    /// and we shift it left by this many pixels then let `TextBounds`
    /// clip the overflow to the preview pane. One scroll var keeps the
    /// state model trivially small; in practice a markdown doc only
    /// ever has one wide table on screen at a time. h/l (Preview focus)
    /// step ±~1 cell; Shift+wheel-Y also adjusts. Reset to 0 whenever
    /// the shaped buffer is rebuilt so navigating between docs starts fresh.
    table_scroll_px: f32,
    /// Per-table cosmic-text buffers hosted as ExtraAreas. Indexed in
    /// `media_blocks` Table-encounter order so multiple tables on one
    /// page each get their own slot. `rendered` is the source text the
    /// buffer was built from; on every redraw the chrome compares it
    /// against the current `media_blocks` so navigating to a doc with
    /// the same table count but different content still rebuilds.
    /// The set holds every table of the page; a row is shaped, at huge
    /// width with no wrap, once the pane shows it or a frame shapes it
    /// beyond the view, and `natural_w_px` is the max line width, from
    /// the monospace advance and the rows shaped — used by the scroll
    /// clamp.
    tables: Vec<TableBufferEntry>,
    /// The table sets of documents shown before, newest first, within `PARKED_TABLE_BYTES`: a return to a document
    /// takes its set back instead of shaping it again. A set is taken only for the same sources at the same metrics.
    parked: Vec<Vec<TableBufferEntry>>,
    /// The next frame that shows the tables shapes none (`State::switch_to_workspace` and every frame that shows no
    /// tables set it): it draws the panes with the tables' places empty and asks for the next frame.
    defer: bool,
    /// Cache of MathJax-rendered SVGs keyed by `(latex, display)`.
    /// Populated by `IncomingEvt::MathRendered`; consumed by the
    /// markdown render path to paint quads over FFFC placeholders.
    /// Survives navigation so re-opening a doc with the same math
    /// doesn't re-roundtrip.
    math: HashMap<(String, bool), MathSvg>,
    /// In-flight math.render requests, same key shape as `math`.
    /// Prevents duplicate dispatch when a markdown doc has the same
    /// equation twice or when the user re-loads a partially-fetched
    /// doc before the first replies land.
    math_pending: HashSet<(String, bool)>,
    /// Per-fence semantic-overlay cache keyed by `(lang, source_hash)`.
    /// Populated by `IncomingEvt::MarkdownTokens`; consumed by the
    /// CodeBlock walk to overlay tree-sitter base spans with backend-
    /// derived semantic spans (function-def, call-site, type, etc.).
    /// Survives navigation so re-rendering identical fences in another
    /// doc (Julia ecosystem reuses the same snippets) skips the round
    /// trip.
    tokens: HashMap<(String, u64), Vec<MarkdownToken>>,
    /// In-flight markdown.tokenize requests, same key shape as
    /// `tokens`. Prevents duplicate dispatch when the
    /// same fence appears twice in a doc or the user re-renders before
    /// the reply lands.
    tokens_pending: HashSet<(String, u64)>,
    /// Set when a fresh SVG, figure or token overlay is cached. The redraw loop checks this and rebuilds
    /// the shaped buffer from the installed source so the markdown walk can size
    /// per-block placeholders to the SVG's natural dimensions instead
    /// of the pre-render letterbox default. Coalesces a burst of
    /// MathRendered events into a single rebuild per frame.
    reflow: bool,
}

impl MarkdownMedia {
    pub(in crate::ui) fn tables(&self) -> &[TableBufferEntry] { &self.tables }

    /// Whether the table buffers were built from exactly these sources at these metrics, in order.
    pub(in crate::ui) fn tables_match(&self, sources: &[(String, f32, f32)]) -> bool { set_matches(&self.tables, sources) }

    /// New table buffers replace the old, which are kept; the shared scroll returns to the left edge.
    pub(in crate::ui) fn replace_tables(&mut self, tables: Vec<TableBufferEntry>) {
        let old = std::mem::replace(&mut self.tables, tables);
        self.park(old, PARKED_TABLE_BYTES);
        self.table_scroll_px = 0.0;
    }

    pub(in crate::ui) fn clear_tables(&mut self) {
        let old = std::mem::take(&mut self.tables);
        self.park(old, PARKED_TABLE_BYTES);
    }

    /// Each table of the shown set at `places` (its index and place) starts its buffer at its first row `pane` shows
    /// and, when `shape`, shapes the rows there not shaped yet; whether a row in view is left unshaped, and how many
    /// rows it shaped.
    pub(in crate::ui) fn show_rows(&mut self, font_system: &mut cosmic_text::FontSystem, places: &[(usize, ScreenRect)],
                     pane: ScreenRect, shape: bool) -> (bool, usize) {
        let (mut waiting, mut shaped) = (false, 0);
        for (table, rect) in places {
            if let Some(entry) = self.tables.get_mut(*table) {
                let rows = entry.rows_in(*rect, pane);
                shaped += entry.show(font_system, rows.clone(), shape);
                waiting |= !entry.rows_shaped(&rows);
            }
        }
        (waiting, shaped)
    }

    /// Shapes rows of the shown set not shaped yet, those nearest `pane` first (then by table and row), until `budget`
    /// has passed: at least one row, so a budget of zero shapes one and `Duration::MAX` every row left.
    pub(in crate::ui) fn shape_beyond(&mut self, font_system: &mut cosmic_text::FontSystem, places: &[(usize, ScreenRect)],
                        pane: ScreenRect, budget: std::time::Duration) {
        let mut wanted: Vec<(f32, usize, usize)> = self.tables.iter().enumerate().flat_map(|(table, entry)| {
            let rect = places.iter().find(|(placed, _)| *placed == table).map(|(_, rect)| *rect);
            entry.unshaped(rect, pane).map(move |(distance, row)| (distance, table, row))
        }).collect();
        wanted.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then((a.1, a.2).cmp(&(b.1, b.2))));
        let began = std::time::Instant::now();
        for (_, table, row) in wanted {
            self.tables[table].shape_row(font_system, row);
            if began.elapsed() >= budget {
                break;
            }
        }
    }

    /// Whether a row of the shown set is not shaped yet.
    pub(in crate::ui) fn has_unshaped(&self) -> bool {
        self.tables.iter().any(|entry| entry.buffer.lines.iter().any(|line| line.layout_opt().is_none()))
    }

    /// The kept set built from exactly `sources`, shaped in whole or in part, becomes the shown one; whether there was
    /// one.
    pub(in crate::ui) fn take_parked(&mut self, sources: &[(String, f32, f32)]) -> bool {
        let Some(i) = self.parked.iter().position(|set| set_matches(set, sources)) else { return false };
        let set = self.parked.remove(i);
        self.replace_tables(set);
        true
    }

    /// `set` is kept newest first, the oldest dropped to stay within `budget` bytes of rendered table text; a set over the budget alone
    /// is not kept.
    fn park(&mut self, set: Vec<TableBufferEntry>, budget: usize) {
        let charge = |set: &[TableBufferEntry]| set.iter().map(TableBufferEntry::charge).sum::<usize>();
        if set.is_empty() || charge(&set) > budget {
            return;
        }
        self.parked.insert(0, set);
        let mut kept = 0;
        self.parked.retain(|set| {
            kept += charge(set);
            kept <= budget
        });
    }

    /// The next frame that shows the tables shapes none.
    pub(in crate::ui) fn defer_next(&mut self) { self.defer = true; }

    /// Whether this frame shapes none; asked on every frame, so a deferral never outlives the frame that spends it.
    pub(in crate::ui) fn take_defer(&mut self) -> bool { std::mem::take(&mut self.defer) }

    pub(in crate::ui) fn table_scroll_px(&self) -> f32 { self.table_scroll_px }
    pub(in crate::ui) fn clamp_table_scroll(&mut self, max: f32) {
        self.table_scroll_px = self.table_scroll_px.clamp(0.0, max);
    }
    /// Move by `px`, stopping at the left edge.
    pub(in crate::ui) fn scroll_tables(&mut self, px: f32) { self.table_scroll_px = (self.table_scroll_px + px).max(0.0); }
    pub(in crate::ui) fn scroll_tables_left(&mut self, px: f32) {
        self.table_scroll_px = (self.table_scroll_px - px).max(0.0);
    }
    /// Unclamped here; the next frame clamps to the widest table.
    pub(in crate::ui) fn scroll_tables_right(&mut self, px: f32) { self.table_scroll_px += px; }
    pub(in crate::ui) fn reset_table_scroll(&mut self) { self.table_scroll_px = 0.0; }

    /// The per-block pixel metrics the markdown walk consumes. Each cached SVG's ex-unit dimensions
    /// become unscaled pixels in body-font space:
    /// `value_ex * MATHJAX_EX_FACTOR * MD_BODY_SIZE`. The walk applies
    /// the per-context `scale` itself, so the values stored here are
    /// scale-1 reference numbers — same coordinate system as
    /// `MATH_BLOCK_H_DEFAULT` and friends. Entries without parsed
    /// dimensions are skipped (the walk will see no cache hit and use
    /// its fallback path).
    pub(in crate::ui) fn math_metrics(&self) -> MathMetricsMap {
        let mut out = MathMetricsMap::new();
        for (key, entry) in self.math.iter() {
            let (Some(w_ex), Some(h_ex)) = (entry.width_ex, entry.height_ex) else {
                continue;
            };
            let width_px = w_ex * MATHJAX_EX_FACTOR * MD_BODY_SIZE;
            let height_px = h_ex * MATHJAX_EX_FACTOR * MD_BODY_SIZE;
            // vertical_align_ex is negative when the SVG hangs below
            // the baseline. Drop is the positive distance below; default
            // 0 for display blocks that don't always include the style.
            let baseline_drop_px = entry
                .vertical_align_ex
                .map(|v| (-v) * MATHJAX_EX_FACTOR * MD_BODY_SIZE)
                .unwrap_or(0.0);
            out.insert(
                key.clone(),
                MathMetrics {
                    width_px,
                    height_px,
                    baseline_drop_px,
                },
            );
        }
        out
    }

    pub(in crate::ui) fn math(&self, key: &(String, bool)) -> Option<&MathSvg> { self.math.get(key) }
    pub(in crate::ui) fn math_handled(&self, key: &(String, bool)) -> bool {
        self.math.contains_key(key) || self.math_pending.contains(key)
    }
    pub(in crate::ui) fn mark_math_pending(&mut self, key: (String, bool)) { self.math_pending.insert(key); }

    /// A rendered SVG: cached, its key settled, and a reflow asked so the walk sizes its block from the new dims.
    pub(in crate::ui) fn install_math(&mut self, key: (String, bool), svg: MathSvg) {
        self.math.insert(key.clone(), svg);
        self.math_pending.remove(&key);
        self.reflow = true;
    }

    /// A render that will not come: the key is settled with no reflow, so it cannot loop.
    pub(in crate::ui) fn math_failed(&mut self, key: &(String, bool)) { self.math_pending.remove(key); }

    /// A cached SVG whose raster is neither drawn nor asked of the worker under `generation`.
    pub(in crate::ui) fn math_to_rasterise(&self, key: &(String, bool), generation: u64) -> Option<&MathSvg> {
        self.math.get(key).filter(|entry| entry.wants_raster(generation))
    }

    /// Asks the worker, in one batch, for each of `wanted` (a key and its pixel size) not yet drawn or asked under
    /// `generation`; the keys are then asked, so a refusal, a resize or a later frame submits nothing again.
    pub(in crate::ui) fn request_rasters(&mut self, draws: &mut SvgDraws, generation: u64, wanted: Vec<((String, bool), (u32, u32))>) {
        let mut jobs = Vec::new();
        for (key, (w, h)) in wanted {
            if let Some(entry) = self.math.get_mut(&key).filter(|entry| entry.wants_raster(generation)) {
                entry.asked = Some(generation);
                jobs.push((job_key(&key), Job { ask: Ask::Stretch(w, h), svg: entry.svg_bytes.as_slice().into() }));
            }
        }
        if jobs.is_empty() {
            return;
        }
        if let Err(why) = draws.submit_batch(generation, jobs) {
            tracing::warn!(why, "math svg not drawn");
        }
    }

    /// One outcome from the worker for a math job, or `None` for another owner's. Only the outcome of the generation
    /// the snippet was asked under counts: a departed generation's `Stale` can arrive after the new one has asked
    /// again, and a replaced entry was asked of nothing.
    pub(in crate::ui) fn settle_drawing(&mut self, done: Done) -> Option<((String, bool), Settled)> {
        let key = math_key(&done.key)?;
        self.math.get(&key).filter(|entry| entry.asked == Some(done.generation))?;
        let settled = match done.outcome {
            Outcome::Drawn { w, h, rgba } => Settled::Drawn { w, h, rgba },
            Outcome::Refused(why) | Outcome::Ended(why) => Settled::Failed(why),
            // A `Stale` is of a generation the pane has left, and `wants_raster` asks again under the new one.
            Outcome::Stale | Outcome::Picture { .. } => Settled::Unhandled,
        };
        Some((key, settled))
    }

    pub(in crate::ui) fn set_math_raster(&mut self, key: &(String, bool), quad: crate::ui::render::quad::Quad) {
        if let Some(entry) = self.math.get_mut(key) {
            entry.rasterised = Some(quad);
        }
    }

    pub(in crate::ui) fn token_cache(&self) -> &HashMap<(String, u64), Vec<MarkdownToken>> { &self.tokens }
    pub(in crate::ui) fn tokens_handled(&self, key: &(String, u64)) -> bool {
        self.tokens.contains_key(key) || self.tokens_pending.contains(key)
    }
    pub(in crate::ui) fn mark_tokens_pending(&mut self, key: (String, u64)) { self.tokens_pending.insert(key); }

    /// A fence's semantic overlay: cached, its key settled, and a reflow asked.
    pub(in crate::ui) fn install_tokens(&mut self, key: (String, u64), spans: Vec<MarkdownToken>) {
        self.tokens.insert(key.clone(), spans);
        self.tokens_pending.remove(&key);
        self.reflow = true;
    }

    /// A figure landed or failed: the walk must size its reservation again.
    pub(in crate::ui) fn request_reflow(&mut self) { self.reflow = true; }

    /// The frame's one reflow, taken.
    pub(in crate::ui) fn take_reflow(&mut self) -> bool { std::mem::take(&mut self.reflow) }
}

fn set_matches(set: &[TableBufferEntry], sources: &[(String, f32, f32)]) -> bool {
    sources.len() == set.len() && sources.iter().zip(set).all(|((rendered, font_px, line_h_px), entry)| {
        entry.built_from(rendered, *font_px, *line_h_px)
    })
}

#[cfg(test)]
#[path = "media_store_tests.rs"]
mod tests;
