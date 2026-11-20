//! Window-wide REPL presentation: image identity, line projection, layout and scroll anchoring.

use crate::ui::*;
use super::{colors, sgr::{self, Sgr}, wrap::{self, Wrap}, ReplEffects};
use crate::ui::render::scissor::scissor_px;

// State supplies a normalized row key; presentation never resolves row identities.
type ImageKey = (WsKey, u64, usize);
type BuildKey = (u32, u32, u32, u32);

fn image_key(row: &WsKey, eval_id: u64, frame_index: usize) -> ImageKey {
    (row.clone(), eval_id, frame_index)
}

struct ReplImage<I> {
    image: I,
    w: u32,
    h: u32,
}

struct ReplImageSlot {
    line: usize,
    rows: u16,
    disp_w: f32,
    disp_h: f32,
    key: ImageKey,
}

/// Upload is the only GPU service needed while refreshing real encoded images.
pub(in crate::ui) trait ReplImageUpload<I> {
    fn upload(&mut self, rgba: &[u8], width: u32, height: u32) -> Result<I>;
}

/// Both production and recorder use the owner's clipping and placement decisions.
pub(in crate::ui) trait ReplImagePainter<I> {
    fn scissor(&mut self, x: u32, y: u32, width: u32, height: u32);
    fn image(&mut self, image: &I, rect: ScreenRect, surface: (u32, u32)) -> Result<()>;
}

pub(in crate::ui) struct ReplPresentation<I = Quad> {
    images: HashMap<ImageKey, ReplImage<I>>,
    image_slots: Vec<ReplImageSlot>,
    scrollback_px: ScreenRect,
    window: (usize, usize),
    build_anchor: Option<(BuildKey, u64, usize)>,
    /// The wrapped text frames, and the pane width in cells they were wrapped for.
    wrap: Wrap,
    /// What the bottom row of a scrolled-up view showed, and the width it was wrapped for: a re-wrap puts it back at
    /// the bottom, so a resize keeps a reader on the same text.
    view_anchor: Option<(usize, wrap::Anchor)>,
}

impl<I> Default for ReplPresentation<I> {
    fn default() -> Self {
        Self {
            images: HashMap::new(),
            image_slots: Vec::new(),
            scrollback_px: ScreenRect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 },
            window: (0, 0),
            build_anchor: None,
            wrap: Wrap::default(),
            view_anchor: None,
        }
    }
}

/// Owned cell layout; the caller commits scroll only after ratatui succeeds.
pub(in crate::ui) struct ReplDrawerLayout {
    pub(in crate::ui) scrollback: ratatui::layout::Rect,
    pub(in crate::ui) input: ratatui::layout::Rect,
    pub(in crate::ui) scroll: u16,
    pub(in crate::ui) window: (usize, usize),
}

impl<I> ReplPresentation<I> {
    pub(in crate::ui) fn refresh_images(
        &mut self,
        row: &WsKey,
        log: &[ReplEntry],
        upload: &mut impl ReplImageUpload<I>,
    ) {
        // Decode before pruning, as in the original pre-draw pass. Failures remain retryable.
        let mut new_images = Vec::new();
        for entry in log {
            for (index, frame) in entry.frames.iter().enumerate() {
                if let ReplFrame::Image { data_base64, .. } = frame {
                    let key = image_key(row, entry.eval_id, index);
                    if self.images.contains_key(&key) {
                        continue;
                    }
                    use base64::Engine as _;
                    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(data_base64) else {
                        continue;
                    };
                    let Ok(decoded) = image::load_from_memory(&raw) else {
                        continue;
                    };
                    let rgba = decoded.to_rgba8();
                    let (w, h) = rgba.dimensions();
                    if let Ok(image) = upload.upload(&rgba, w, h) {
                        new_images.push((key, ReplImage { image, w, h }));
                    }
                }
            }
        }
        self.images.extend(new_images);
        // Keep all frame indices of each surviving eval, without a byte/texture budget or parked cache.
        self.images.retain(|key, _| {
            &key.0 == row && log.iter().any(|entry| entry.eval_id == key.1)
        });
    }

    pub(in crate::ui) fn build(
        &mut self,
        row: &WsKey,
        log: &[ReplEntry],
        starting: bool,
        cell_w: f32,
        cell_h: f32,
        mut scroll: u16,
    ) -> (Vec<RtLine<'static>>, u16) {
        // Use the previous completed pixel layout, including the first frame's zero geometry.
        self.wrap.begin();
        let (lines, slots, starts, marks) = project_lines(
            row, log, &self.images, (self.scrollback_px.w, self.scrollback_px.h),
            (cell_w, cell_h), starting, &mut self.wrap,
        );
        self.wrap.end();
        self.image_slots = slots;
        let build_key = (
            self.scrollback_px.w.to_bits(), self.scrollback_px.h.to_bits(),
            cell_w.to_bits(), cell_h.to_bits(),
        );
        let cols = self.wrap.cols;
        // A new width puts the anchored line back at the bottom; any other change pins the view to the tail's growth.
        let rewrapped = self.view_anchor.filter(|&(anchored, _)| anchored != cols && scroll > 0);
        if let Some((_, anchor)) = rewrapped {
            if let Some(at) = self.wrap.find(row, &marks, anchor).filter(|&at| at < lines.len()) {
                scroll = (lines.len() - at - 1).min(u16::MAX as usize) as u16;
            }
        } else if let Some((previous_key, id, span)) = self.build_anchor {
            if previous_key == build_key {
                scroll = pinned_repl_scroll(scroll, id, span, lines.len(), &starts);
            }
        }
        self.build_anchor = starts.last()
            .map(|&(id, start)| (build_key, id, lines.len().saturating_sub(start)));
        let bottom = lines.len().checked_sub(scroll as usize + 1).filter(|_| scroll > 0);
        self.view_anchor = bottom.and_then(|at| self.wrap.anchor(row, &marks, at)).map(|a| (cols, a));
        (lines, scroll)
    }

    pub(in crate::ui) fn drawer_layout(
        rect: ratatui::layout::Rect,
        input_line_count: u16,
        total: usize,
        scroll: u16,
    ) -> ReplDrawerLayout {
        let max_input_rows = rect.height.saturating_sub(1).max(1);
        let input_rows = input_line_count.min(max_input_rows).max(1);
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(input_rows)])
            .split(rect);
        let scroll_h = split[0].height as usize;
        // Retain the existing u16 conversion, including its overflow behavior.
        let max_scroll = total.saturating_sub(scroll_h) as u16;
        let scroll = scroll.min(max_scroll);
        let end = total.saturating_sub(scroll as usize);
        let start = end.saturating_sub(scroll_h);
        ReplDrawerLayout { scrollback: split[0], input: split[1], scroll, window: (start, end) }
    }

    /// The scrollback's width in cells; true when it changed, so the next frame wraps for it. A width of 0 (no drawer
    /// rect yet) keeps the last known one, and before any the first frame is drawn unwrapped, as before the wrap.
    pub(in crate::ui) fn set_cols(&mut self, cols: usize) -> bool {
        cols != 0 && std::mem::replace(&mut self.wrap.cols, cols) != cols
    }

    /// The layout the frame ended on: the scrollback's pixels, the rows in view and its width in cells (`set_cols`: true
    /// when it changed, so the caller asks for the frame that wraps for it).
    pub(in crate::ui) fn commit_layout(&mut self, scrollback: ScreenRect, window: (usize, usize), cols: usize) -> bool {
        self.scrollback_px = scrollback;
        self.window = window;
        self.set_cols(cols)
    }

    pub(in crate::ui) fn apply_effects(&mut self, effects: &ReplEffects) {
        if effects.clear_images {
            self.clear_images();
        }
        if effects.reset_build_anchor {
            self.reset_anchor();
        }
    }

    fn clear_images(&mut self) {
        self.images.clear();
        self.image_slots.clear();
    }

    fn reset_anchor(&mut self) {
        self.build_anchor = None;
        self.view_anchor = None;
    }

    pub(in crate::ui) fn paint(
        &self,
        visible: bool,
        cell_w: f32,
        cell_h: f32,
        surface: (u32, u32),
        painter: &mut impl ReplImagePainter<I>,
    ) -> Result<()> {
        if visible && !self.image_slots.is_empty() {
            let area = self.scrollback_px;
            let (win_start, win_end) = self.window;
            let (width, height) = surface;
            if let Some((sx, sy, sw, sh)) = scissor_px(area, surface) {
                let mut painted = false;
                for slot in &self.image_slots {
                    if slot.line + slot.rows as usize <= win_start || slot.line >= win_end {
                        continue;
                    }
                    let Some(image) = self.images.get(&slot.key) else { continue };
                    if !painted {
                        painter.scissor(sx, sy, sw, sh);
                        painted = true;
                    }
                    let rect = ScreenRect {
                        x: area.x + cell_w,
                        y: area.y + (slot.line as f32 - win_start as f32) * cell_h,
                        w: slot.disp_w,
                        h: slot.disp_h,
                    };
                    painter.image(&image.image, rect, surface)?;
                }
                if painted {
                    painter.scissor(0, 0, width, height);
                }
            }
        }
        Ok(())
    }
}

/// The REPL pane's offset counts rows back from a tail that keeps moving,
/// so the rows being read slide away as output arrives. Track how far the
/// tail moved and the rows stay put — the rule the emulator applies to the
/// pty panes, which this pane has no emulator to get.
///
/// Measured as the span from the newest entry's first line to the end of
/// the build, never as a change in total lines: the 256-entry cap drops an
/// entry off the FRONT in the same frame a new one arrives, which leaves a
/// total-line delta short by the dropped entry and slides the view once per
/// eval. The delta is signed, because entries shrink as well as grow — a
/// finished entry with no measurable elapsed time loses its "(running…)"
/// line, and an evicted image falls back to a single caption line. A row
/// removed between the viewed rows and the tail shortens their distance
/// from it, so the offset must drop with it.
///
/// At the live tail (0) it stays 0: there, new output *should* follow,
/// which is what a terminal does.
fn pinned_repl_scroll(
    scroll: u16,
    anchor_eval_id: u64,
    anchor_tail_span: usize,
    total: usize,
    starts: &[(u64, usize)],
) -> u16 {
    if scroll == 0 {
        return scroll;
    }
    // From the tail: the anchor is always the newest entry, and a
    // peer-originated entry can carry an eval_id that collides numerically
    // with an older local one in the same log.
    let Some(i) = starts.iter().rposition(|(id, _)| *id == anchor_eval_id) else {
        // The anchored entry is gone: nothing trustworthy to measure from
        // this frame, so leave the view alone and re-seed.
        return scroll;
    };
    let new_span = total.saturating_sub(starts[i].1) as i64;
    let delta = new_span - anchor_tail_span as i64;
    (scroll as i64 + delta).clamp(0, u16::MAX as i64) as u16
}

/// A frame's text if it is stdout or stderr, else none.
fn stream_text(frame: &ReplFrame) -> &str {
    match frame {
        ReplFrame::Stdout { text } | ReplFrame::Stderr { text } => text,
        _ => "",
    }
}

/// Where the writes that share a row with frame `start` end (exclusive): a program's text continues the line the write
/// before it left open (no newline at its end), as a terminal's does, so `print("a"); print("b")` is one row, a `Pkg`
/// status line written in pieces is one row, and a `\r` repaint overwrites the row it repaints. Only the same stream
/// continues (another frame between, a note or the other stream, ends the line).
fn open_run(frames: &[ReplFrame], own: &[usize], start: usize) -> usize {
    let same = |a: &ReplFrame, b: &ReplFrame| matches!((a, b), (ReplFrame::Stdout { .. }, ReplFrame::Stdout { .. }) | (ReplFrame::Stderr { .. }, ReplFrame::Stderr { .. }));
    let mut end = start + 1;
    while end < frames.len() && !own.contains(&end) && same(&frames[start], &frames[end])
        && !stream_text(&frames[end - 1]).ends_with('\n')
    {
        end += 1;
    }
    end
}

/// The rows of the writes `texts` of a stream (stdout or stderr: `base` and `stderr`), after the writes of its stream before
/// them: read as a terminal reads them (`\r` overwrites, SGR escapes color), wrapped and cached by `Wrap::run`.
fn stream_rows(
    wrap: &mut Wrap,
    states: &mut HashMap<(u64, bool), Sgr>,
    key: (WsKey, u64, usize),
    texts: &[&str],
    (base, stderr): (Style, bool),
    out: &mut Vec<RtLine<'static>>,
) {
    let slot = (key.1, stderr);
    let ((done, tail), end) = wrap.run(key, texts, base, states.get(&slot).copied().unwrap_or_default());
    out.extend(done.0.iter().chain(&tail.0).cloned());
    states.insert(slot, end);
}

/// The live input's rows for a pane `cols` cells wide: the prompt first, the code under it at the same column, long lines
/// wrapped as the log's echo of code is, and, while the drawer has focus, the cursor block at the end of the last row, on a
/// row of its own when that row is full. The draft is edited at its end only, so the cursor is always there.
pub(in crate::ui) fn input_lines(input: &str, pkg_mode: bool, focused: bool, cols: usize) -> Vec<RtLine<'static>> {
    let mut rows = wrap::code_rows(input, pkg_mode, cols, colors::prompt(pkg_mode, focused));
    if focused {
        let cursor = Span::styled("\u{2588}", colors::cursor(pkg_mode));
        match rows.last_mut() {
            Some(last) if cols == 0 || last.width() < cols => last.spans.push(cursor),
            _ => {
                // Under the code; in a pane no wider than the prompt, in its last cell.
                let hang = if pkg_mode { 5 } else { 7 };
                rows.push(RtLine::from(vec![Span::raw(" ".repeat(hang.min(cols.saturating_sub(1)))), cursor]));
            }
        }
    }
    rows
}

/// Project entries into cell-safe rows; decoded figures reserve rows for the later clipped paint.
/// Entry starts measure tail growth independently of entries trimmed from the front.
#[allow(clippy::too_many_lines, reason = "builds the REPL scrollback lines, one arm per frame kind; predates the 100-line limit")]
fn project_lines<I>(
    row: &WsKey,
    log: &[ReplEntry],
    images: &std::collections::HashMap<ImageKey, ReplImage<I>>,
    geometry: (f32, f32),
    cells: (f32, f32),
    repl_starting: bool,
    wrap: &mut Wrap,
) -> (Vec<RtLine<'static>>, Vec<ReplImageSlot>, Vec<(u64, usize)>, Vec<(usize, u64, usize)>) {
    let (avail_w_px, avail_h_px) = geometry;
    let (cell_w, cell_h) = cells;
    let mut slots: Vec<ReplImageSlot> = Vec::new();
    let mut out: Vec<RtLine<'static>> = Vec::new();
    let mut starts: Vec<(u64, usize)> = Vec::new();
    // Where each entry (frame `usize::MAX`) and each frame starts, for `Wrap::anchor`.
    let mut marks: Vec<(usize, u64, usize)> = Vec::new();
    // The SGR state each stream (eval, stderr or not) has left, which its next frame starts from.
    let mut states: HashMap<(u64, bool), Sgr> = HashMap::new();
    for entry in log {
        starts.push((entry.eval_id, out.len()));
        marks.push((out.len(), entry.eval_id, usize::MAX));
        if let Some(label) = &entry.origin {
            out.extend(wrap::one(&format!("⟨{}⟩", cell_row(label)), colors::origin(), wrap.cols));
        } else {
            out.extend(wrap::code_rows(&entry.code, entry.pkg_mode, wrap.cols, colors::scrollback_prompt(entry.pkg_mode)));
        }
        let mut joined_to = 0;
        for (frame_idx, frame) in entry.frames.iter().enumerate() {
            if frame_idx < joined_to {
                continue; // a write the row of the one before it already holds
            }
            marks.push((out.len(), entry.eval_id, frame_idx));
            let fkey = (row.clone(), entry.eval_id, frame_idx);
            if let (true, ReplFrame::Stderr { text }) = (entry.own_frames.contains(&frame_idx), frame) {
                // The window's own note starts from no program state and leaves none: a color a program left open on
                // stderr is not the note's.
                let made = |cols| {
                    let lines = sgr::styled_lines(text, colors::notice(), &mut Sgr::default(), true);
                    (wrap::rows(&lines.into_iter().map(|runs| wrap::Logical { runs, head: None, indent: 0 }).collect::<Vec<_>>(), cols), Sgr::default())
                };
                out.extend(wrap.frame(fkey, text.len(), made).0 .0.iter().cloned());
                continue;
            }
            match frame {
                ReplFrame::Stdout { .. } | ReplFrame::Stderr { .. } => {
                    joined_to = open_run(&entry.frames, &entry.own_frames, frame_idx);
                    let (stderr, base) = if matches!(frame, ReplFrame::Stderr { .. }) { (true, colors::stderr()) } else { (false, colors::plain()) };
                    let texts: Vec<&str> = entry.frames[frame_idx..joined_to].iter().map(stream_text).collect();
                    stream_rows(wrap, &mut states, fkey, &texts, (base, stderr), &mut out);
                }
                ReplFrame::Value { mime, text } => {
                    let prefix = (mime != "text/plain").then(|| Span::raw(format!("[{}] ", cell_row(mime))));
                    let made = |cols| {
                        let lines = sgr::styled_lines(text, colors::plain(), &mut Sgr::default(), false);
                        let lines: Vec<_> = lines.into_iter().enumerate()
                            .map(|(i, runs)| wrap::Logical { runs, head: if i == 0 { prefix.clone() } else { None }, indent: 0 })
                            .collect();
                        (wrap::rows(&lines, cols), Sgr::default())
                    };
                    out.extend(wrap.frame(fkey, mime.len() + text.len(), made).0 .0.iter().cloned());
                }
                ReplFrame::Error {
                    message,
                    stacktrace,
                } => {
                    // The message's lines and then each stack frame are logical lines of one cached set of rows, so a
                    // reader inside a stack trace keeps their place through a resize and its rows are not rebuilt.
                    let made = |cols| {
                        let mut lines = sgr::styled_lines(message, colors::plain(), &mut Sgr::default(), false);
                        if lines.is_empty() {
                            lines.push(vec![(String::new(), colors::plain())]);
                        }
                        let mut lines: Vec<_> = lines.into_iter().enumerate().map(|(i, runs)| wrap::Logical {
                            runs, head: (i == 0).then(|| Span::styled("ERROR: ", colors::error_label())), indent: 0,
                        }).collect();
                        // A frame's row continues under its `at`, four cells in.
                        lines.extend(stacktrace.iter().map(|sf| wrap::Logical {
                            runs: vec![
                                ("    at ".to_string(), colors::trace_gray()),
                                (cell_row(&sf.function).into_owned(), colors::trace_function(&sf.function)),
                                (format!(" ({}:{})", cell_row(&sf.file), sf.line), colors::trace_gray()),
                            ],
                            head: None, indent: 4,
                        }));
                        (wrap::rows(&lines, cols), Sgr::default())
                    };
                    out.extend(wrap.frame(fkey, message.len() + stacktrace.len(), made).0 .0.iter().cloned());
                }
                ReplFrame::Done { .. } => {}
                ReplFrame::Browser { url, .. } => {
                    let text = format!("↗ interactive figure · {}", cell_row(&crate::browser_open::origin_of(url)));
                    out.extend(wrap::one(&text, colors::browser_link(), wrap.cols));
                }
                ReplFrame::Started { .. } => {}
                ReplFrame::Lifecycle { .. } => {}
                ReplFrame::Image { mime, bytes, .. } => {
                    let key = image_key(row, entry.eval_id, frame_idx);
                    // The previous layout's degenerate width uses a caption instead of sliver-scaled rows.
                    let sized = (avail_w_px > 4.0 * cell_w)
                        .then(|| images.get(&key))
                        .flatten();
                    if let Some(img) = sized {
                        // Fit both dimensions, without upscaling; reserve only the resulting cell rows.
                        let max_w = (avail_w_px - 2.0 * cell_w).max(cell_w);
                        let max_h = (avail_h_px - 3.0 * cell_h).max(cell_h);
                        let scale = (max_w / img.w.max(1) as f32)
                            .min(max_h / img.h.max(1) as f32)
                            .min(1.0);
                        let disp_w = img.w as f32 * scale;
                        let disp_h = img.h as f32 * scale;
                        let rows = ((disp_h / cell_h.max(1.0)).ceil() as u16).max(1);
                        slots.push(ReplImageSlot {
                            line: out.len(),
                            rows,
                            disp_w,
                            disp_h,
                            key,
                        });
                        for _ in 0..rows {
                            out.push(RtLine::from(""));
                        }
                    } else {
                        let text = format!("[image · {} · {} bytes]", cell_row(mime), bytes);
                        out.extend(wrap::one(&text, colors::image_note(), wrap.cols));
                    }
                }
            }
        }
        if entry.in_flight {
            if repl_starting {
                let note = "(julia starting — precompiling this workspace's environment; a first run can take minutes…)";
                out.extend(wrap::one(note, colors::starting(), wrap.cols));
            } else {
                out.extend(wrap::one("(running…)", colors::running(), wrap.cols));
            }
        } else if entry.elapsed_ms > 0 {
            out.extend(wrap::one(&format!("  ({} ms)", entry.elapsed_ms), colors::elapsed(), wrap.cols));
        }
    }
    (out, slots, starts, marks)
}

#[cfg(test)]
#[path = "presentation_tests.rs"]
mod tests;
