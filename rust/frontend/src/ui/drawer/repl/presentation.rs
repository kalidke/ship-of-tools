//! Window-wide REPL presentation: image identity, line projection, layout and scroll anchoring.

use crate::ui::*;
use super::ReplEffects;

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
}

impl<I> Default for ReplPresentation<I> {
    fn default() -> Self {
        Self {
            images: HashMap::new(),
            image_slots: Vec::new(),
            scrollback_px: ScreenRect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 },
            window: (0, 0),
            build_anchor: None,
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
        let (lines, slots, starts) = project_lines(
            row, log, &self.images, (self.scrollback_px.w, self.scrollback_px.h),
            (cell_w, cell_h), starting,
        );
        self.image_slots = slots;
        let build_key = (
            self.scrollback_px.w.to_bits(), self.scrollback_px.h.to_bits(),
            cell_w.to_bits(), cell_h.to_bits(),
        );
        if let Some((previous_key, id, span)) = self.build_anchor {
            if previous_key == build_key {
                scroll = pinned_repl_scroll(scroll, id, span, lines.len(), &starts);
            }
        }
        self.build_anchor = starts.last()
            .map(|&(id, start)| (build_key, id, lines.len().saturating_sub(start)));
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

    pub(in crate::ui) fn commit_layout(&mut self, scrollback: ScreenRect, window: (usize, usize)) {
        self.scrollback_px = scrollback;
        self.window = window;
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
            let sw = (area.w.max(0.0) as u32).min(width);
            let sh = (area.h.max(0.0) as u32).min(height);
            if sw > 0 && sh > 0 {
                let sx = (area.x.max(0.0) as u32).min(width - 1);
                let sy = (area.y.max(0.0) as u32).min(height - 1);
                let sw = sw.min(width - sx);
                let sh = sh.min(height - sy);
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

/// A program's output stream as a terminal shows it: `\r\n` and `\n` end a line,
/// and within a line a lone `\r` returns to its start, so what follows
/// overwrites it (a progress bar's updates make one row, not a blank row and a
/// row each). Then the window's rule for each line (`cell_lines`); a blank line
/// stays a row.
fn stream_lines(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
        .lines()
        .flat_map(|line| {
            let mut row: Vec<char> = Vec::new();
            for part in line.split('\r') {
                for (i, c) in part.chars().enumerate() {
                    match row.get_mut(i) {
                        Some(cell) => *cell = c,
                        None => row.push(c),
                    }
                }
            }
            let rows = cell_lines(&row.into_iter().collect::<String>());
            if rows.is_empty() { vec![String::new()] } else { rows }
        })
        .collect()
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
) -> (Vec<RtLine<'static>>, Vec<ReplImageSlot>, Vec<(u64, usize)>) {
    let (avail_w_px, avail_h_px) = geometry;
    let (cell_w, cell_h) = cells;
    let mut slots: Vec<ReplImageSlot> = Vec::new();
    let mut out: Vec<RtLine<'static>> = Vec::new();
    let mut starts: Vec<(u64, usize)> = Vec::new();
    for entry in log {
        starts.push((entry.eval_id, out.len()));
        if let Some(label) = &entry.origin {
            out.push(RtLine::from(vec![Span::styled(
                format!("⟨{}⟩", cell_row(label)),
                Style::default()
                    .fg(Color::LightMagenta)
                    .add_modifier(Modifier::BOLD),
            )]));
        } else {
            let (prompt_text, prompt_color) = if entry.pkg_mode {
                ("pkg> ", Color::LightBlue)
            } else {
                ("julia> ", Color::LightCyan)
            };
            let cont_pad: String = " ".repeat(prompt_text.len());
            for (i, seg) in entry.code.split('\n').enumerate() {
                let prefix_span = if i == 0 {
                    Span::styled(prompt_text.to_string(), Style::default().fg(prompt_color))
                } else {
                    Span::raw(cont_pad.clone())
                };
                out.push(RtLine::from(vec![
                    prefix_span,
                    Span::styled(cell_row(seg).into_owned(), Style::default()),
                ]));
            }
        }
        for (frame_idx, frame) in entry.frames.iter().enumerate() {
            match frame {
                ReplFrame::Stdout { text } => {
                    for line in stream_lines(text) {
                        out.push(RtLine::from(line));
                    }
                }
                ReplFrame::Stderr { text } => {
                    for line in stream_lines(text) {
                        out.push(RtLine::from(vec![Span::styled(
                            line,
                            Style::default().fg(Color::Red),
                        )]));
                    }
                }
                ReplFrame::Value { mime, text } => {
                    let prefix = if mime == "text/plain" {
                        String::new()
                    } else {
                        format!("[{}] ", cell_row(mime))
                    };
                    for (i, line) in cell_lines(text).into_iter().enumerate() {
                        let s = if i == 0 {
                            format!("{prefix}{line}")
                        } else {
                            line
                        };
                        out.push(RtLine::from(vec![Span::styled(
                            s,
                            Style::default().fg(Color::LightGreen),
                        )]));
                    }
                }
                ReplFrame::Error {
                    message,
                    stacktrace,
                } => {
                    for line in cell_lines(message) {
                        out.push(RtLine::from(vec![Span::styled(
                            line,
                            Style::default().fg(Color::LightRed),
                        )]));
                    }
                    for sf in stacktrace {
                        out.push(RtLine::from(vec![Span::styled(
                            format!(
                                "    at {} ({}:{})",
                                cell_row(&sf.function),
                                cell_row(&sf.file),
                                sf.line
                            ),
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::DIM),
                        )]));
                    }
                }
                ReplFrame::Done { .. } => {}
                ReplFrame::Browser { url, .. } => {
                    out.push(RtLine::from(vec![Span::styled(
                        format!(
                            "↗ interactive figure · {}",
                            cell_row(&crate::browser_open::origin_of(url))
                        ),
                        Style::default().fg(Color::LightBlue),
                    )]));
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
                        out.push(RtLine::from(vec![Span::styled(
                            format!("[image · {} · {} bytes]", cell_row(mime), bytes),
                            Style::default()
                                .fg(Color::LightMagenta)
                                .add_modifier(Modifier::DIM),
                        )]));
                    }
                }
            }
        }
        if entry.in_flight {
            if repl_starting {
                out.push(RtLine::from(vec![Span::styled(
                    "(julia starting — precompiling this workspace's environment; \
                     a first run can take minutes…)"
                        .to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM),
                )]));
            } else {
                out.push(RtLine::from(vec![Span::styled(
                    "(running…)".to_string(),
                    Style::default().add_modifier(Modifier::DIM),
                )]));
            }
        } else if entry.elapsed_ms > 0 {
            out.push(RtLine::from(vec![Span::styled(
                format!("  ({} ms)", entry.elapsed_ms),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )]));
        }
    }
    (out, slots, starts)
}

#[cfg(test)]
#[path = "presentation_tests.rs"]
mod tests;
