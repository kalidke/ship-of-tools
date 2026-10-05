//! The REPL log as display lines: the scroll anchor that holds a view still, and the inline-image slots.

use crate::ui::*;

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
pub(in crate::ui) fn pinned_repl_scroll(
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

/// A decoded inline REPL figure: GPU quad + natural pixel dimensions.
pub(in crate::ui) struct ReplImage {
    pub(in crate::ui) quad: Quad,
    pub(in crate::ui) w: u32,
    pub(in crate::ui) h: u32,
}

/// One reserved row-region in the REPL scrollback where an inline image
/// paints. `line` is the absolute index into the built line list.
#[derive(Clone, Copy)]
pub(in crate::ui) struct ReplImageSlot {
    pub(in crate::ui) line: usize,
    pub(in crate::ui) rows: u16,
    pub(in crate::ui) disp_w: f32,
    pub(in crate::ui) disp_h: f32,
    pub(in crate::ui) key: (u64, usize),
}

/// Project the REPL scrollback into ratatui `RtLine`s for the BR quadrant.
/// One entry contributes a `julia> {code}` header line, then one line per
/// rendered frame (stdout/stderr default+red, value green, error red with
/// dim stack lines). Multi-line frame bodies fan out into one line each so
/// ratatui's word wrap doesn't munge them. In-flight entries get a dim
/// `(running…)` placeholder until the response lands.
///
/// Image frames whose quad is already decoded (`images`) reserve
/// fit-to-width blank rows and report a `ReplImageSlot` — the paint pass
/// overlays the quad there, scissored to the scrollback rect. Frames not
/// yet decoded fall back to a one-line caption for a frame or two.
#[allow(clippy::too_many_lines, reason = "builds the REPL scrollback lines, one arm per frame kind; predates the 100-line limit")]
pub(in crate::ui) fn build_repl_lines(
    log: &[ReplEntry],
    images: &std::collections::HashMap<(u64, usize), ReplImage>,
    avail_w_px: f32,
    avail_h_px: f32,
    cell_w: f32,
    cell_h: f32,
    repl_starting: bool,
) -> (Vec<RtLine<'static>>, Vec<ReplImageSlot>, Vec<(u64, usize)>) {
    let mut slots: Vec<ReplImageSlot> = Vec::new();
    let mut out: Vec<RtLine<'static>> = Vec::new();
    // Where each entry's first line falls in this build. The REPL pin
    // measures how far the TAIL moved, which a total-line count cannot do
    // once the 256-entry cap starts dropping entries off the front.
    let mut starts: Vec<(u64, usize)> = Vec::new();
    for entry in log {
        starts.push((entry.eval_id, out.len()));
        if let Some(label) = &entry.origin {
            // A run this FE did NOT originate (a session's repl.execute, ADR
            // 0033 phase 2): show a distinct labelled prompt (magenta) instead
            // of the `julia>` code echo, so the user can tell it apart from
            // their own input at a glance.
            out.push(RtLine::from(vec![Span::styled(
                format!("⟨{label}⟩"),
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
            // Multi-line submissions (Shift+Enter while typing) get one
            // echo line per segment, first with the prompt and the rest
            // with same-width filler — same convention as the live input.
            let cont_pad: String = " ".repeat(prompt_text.len());
            for (i, seg) in entry.code.split('\n').enumerate() {
                let prefix_span = if i == 0 {
                    Span::styled(prompt_text.to_string(), Style::default().fg(prompt_color))
                } else {
                    Span::raw(cont_pad.clone())
                };
                out.push(RtLine::from(vec![
                    prefix_span,
                    Span::styled(seg.to_string(), Style::default()),
                ]));
            }
        }
        // Render whatever frames have streamed in so far — even while the
        // entry is still `in_flight` — so live output (ADR 0009 phase-2
        // streaming) appears tick-by-tick instead of all at once on
        // completion. The `(running…)` / elapsed line is appended AFTER the
        // frames below. (Previously this `continue`d past the frame loop while
        // in_flight, which worked only because the old acceptance ack flipped
        // in_flight=false within milliseconds — that race is now gone.)
        for (frame_idx, frame) in entry.frames.iter().enumerate() {
            match frame {
                ReplFrame::Stdout { text } => {
                    for line in text.lines() {
                        out.push(RtLine::from(line.to_string()));
                    }
                }
                ReplFrame::Stderr { text } => {
                    for line in text.lines() {
                        out.push(RtLine::from(vec![Span::styled(
                            line.to_string(),
                            Style::default().fg(Color::Red),
                        )]));
                    }
                }
                ReplFrame::Value { mime, text } => {
                    // Value frames carry the displayed repr; mime stays
                    // text/plain in the spike. Show in green so it pops
                    // against stdout.
                    let prefix = if mime == "text/plain" {
                        String::new()
                    } else {
                        format!("[{mime}] ")
                    };
                    for (i, line) in text.lines().enumerate() {
                        let s = if i == 0 {
                            format!("{prefix}{line}")
                        } else {
                            line.to_string()
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
                    for line in message.lines() {
                        out.push(RtLine::from(vec![Span::styled(
                            line.to_string(),
                            Style::default().fg(Color::LightRed),
                        )]));
                    }
                    for sf in stacktrace {
                        out.push(RtLine::from(vec![Span::styled(
                            format!("    at {} ({}:{})", sf.function, sf.file, sf.line),
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::DIM),
                        )]));
                    }
                }
                ReplFrame::Done { .. } => {}
                ReplFrame::Browser { url, .. } => {
                    // Browser frames are consumed as a side-effect (OS
                    // browser-open, or a no-open status for `open:false`) in
                    // drain_events and never appended to the log, so this arm
                    // is defensive — if one ever lands here, render a compact
                    // caption rather than dropping it silently.
                    out.push(RtLine::from(vec![Span::styled(
                        format!("↗ interactive figure · {}", crate::browser_open::origin_of(url)),
                        Style::default().fg(Color::LightBlue),
                    )]));
                }
                // Control frame — rendered via the entry's `origin` label
                // above, never as an output row (ADR 0033 phase 2).
                ReplFrame::Started { .. } => {}
                // Control frame — workspace-level lifecycle, consumed in
                // drain_events (the `repl_lifecycle` map) and never appended
                // to a log entry. Defensive arm, mirroring `Browser`.
                ReplFrame::Lifecycle { .. } => {}
                ReplFrame::Image { mime, bytes, .. } => {
                    let key = (entry.eval_id, frame_idx);
                    // Degenerate width (drawer not yet laid out — the fit
                    // width lags one frame) falls through to the caption
                    // rather than reserving sliver-scaled rows.
                    let sized = (avail_w_px > 4.0 * cell_w)
                        .then(|| images.get(&key))
                        .flatten();
                    if let Some(img) = sized {
                        // Reserve rows for the figure; the paint pass
                        // overlays the quad there. Fit BOTH the drawer's
                        // width and its scrollback height (a figure taller
                        // than the drawer otherwise renders permanently
                        // clipped — first repl-figure capture); never
                        // upscale — small figures render at natural size.
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
                        // Not decoded yet (arrives via the pre-draw pass a
                        // frame later) or decode failed: caption line.
                        out.push(RtLine::from(vec![Span::styled(
                            format!("[image · {mime} · {} bytes]", bytes),
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
                // The workspace's REPL child is still BOOTING (repl_state ==
                // "starting"): the first child per workspace precompiles its
                // project env, which can take minutes with zero frames. Say
                // so — before this line, a precompiling first run rendered
                // the same "(running…)" as a live eval and read as a dead
                // kernel. Yellow: attention-but-not-error, distinct from the
                // dim running indicator and the red error rows.
                out.push(RtLine::from(vec![Span::styled(
                    "(julia starting — precompiling this workspace's environment; \
                     a first run can take minutes…)"
                        .to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM),
                )]));
            } else {
                // Still streaming — a dim indicator AFTER the live frames so
                // the user sees output accumulating *and* that more is coming.
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

impl State {
    pub(in crate::ui) fn decode_repl_images(&mut self) {
        // Inline REPL figures, pass 1: decode any Image frame that has no
        // quad yet (base64 → RGBA → texture) and prune entries that aged
        // out of the log. Runs here, outside the draw closure, so texture
        // upload never contends with the frame's borrows.
        {
            let mut new_quads: Vec<((u64, usize), ReplImage)> = Vec::new();
            for entry in &self.repl_log {
                for (fi, fr) in entry.frames.iter().enumerate() {
                    if let sot_protocol::ReplFrame::Image { data_base64, .. } = fr {
                        let key = (entry.eval_id, fi);
                        if self.repl_images.contains_key(&key) {
                            continue;
                        }
                        use base64::Engine as _;
                        let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(data_base64)
                        else {
                            continue;
                        };
                        let Ok(img) = image::load_from_memory(&raw) else {
                            continue;
                        };
                        let rgba = img.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        if let Ok(quad) = Quad::from_rgba8(
                            &self.device,
                            &self.queue,
                            &self.quad_pipeline,
                            &rgba,
                            w,
                            h,
                        ) {
                            new_quads.push((key, ReplImage { quad, w, h }));
                        }
                    }
                }
            }
            for (k, v) in new_quads {
                self.repl_images.insert(k, v);
            }
            if !self.repl_images.is_empty() {
                let log = &self.repl_log;
                self.repl_images
                    .retain(|k, _| log.iter().any(|e| e.eval_id == k.0));
            }
        }
    }

    pub(in crate::ui) fn build_repl_view(&mut self, mut new_repl_scroll: u16) -> (Vec<RtLine<'static>>, u16) {
        // Pass 2: build the drawer lines, reserving rows for decoded
        // figures. Fit width comes from LAST frame's scrollback sub-rect —
        // the natural answer to the build-before-layout chicken-egg (review
        // note: NOT monitor_rect_px, which is the Ctrl+M drawer's rect).
        // One frame of lag on a resize, self-corrects; 0 before the
        // drawer's first draw, where the caption fallback covers the gap.
        let (repl_lines, repl_slots, repl_starts) = build_repl_lines(
            &self.repl_log,
            &self.repl_images,
            self.repl_scrollback_px.w,
            self.repl_scrollback_px.h,
            self.cell_w,
            self.cell_h,
            self.active_repl_starting(),
        );
        self.repl_image_slots = repl_slots;
        let build_key = (
            self.repl_scrollback_px.w.to_bits(),
            self.repl_scrollback_px.h.to_bits(),
            self.cell_w.to_bits(),
            self.cell_h.to_bits(),
        );
        if let Some((prev_key, anchor_id, anchor_span)) = self.repl_build_anchor {
            if prev_key == build_key {
                new_repl_scroll = pinned_repl_scroll(
                    new_repl_scroll,
                    anchor_id,
                    anchor_span,
                    repl_lines.len(),
                    &repl_starts,
                );
            }
        }
        self.repl_build_anchor = repl_starts
            .last()
            .map(|&(id, start)| (build_key, id, repl_lines.len().saturating_sub(start)));
        (repl_lines, new_repl_scroll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live means live: at the tail, new output follows, as a terminal does.
    #[test]
    fn pinned_repl_scroll_lets_a_live_pane_follow_new_output() {
        assert_eq!(pinned_repl_scroll(0, 7, 3, 12, &[(5, 0), (7, 5)]), 0);
    }

    /// Held back, the offset tracks the tail so the rows being read stay
    /// under the arriving output instead of sliding away.
    #[test]
    fn pinned_repl_scroll_holds_the_view_still_when_the_tail_grows() {
        // Anchored entry 7 started at line 5 and spanned 3; it now spans 5.
        assert_eq!(pinned_repl_scroll(4, 7, 3, 10, &[(5, 0), (7, 5)]), 6);
    }

    /// The case a total-line delta gets wrong: at the 256-entry cap, an
    /// arriving entry drops the oldest off the FRONT in the same frame. The
    /// total can even fall while the tail grew, and compensating on the
    /// total would slide the view once per eval. Measured at the tail, the
    /// drop contributes nothing and only the real growth counts.
    #[test]
    fn pinned_repl_scroll_counts_only_the_tail_when_the_cap_drops_an_entry() {
        // Was [(1,0),(5,4),(7,9)] with 12 lines, anchor span 3. Entry 1 is
        // gone and the tail grew by exactly one line: total FELL 12 -> 9.
        assert_eq!(pinned_repl_scroll(6, 7, 3, 9, &[(5, 0), (7, 5)]), 7);
    }

    /// Entries shrink as well as grow — a finished entry with no measurable
    /// elapsed time loses its "(running…)" line. A row removed between the
    /// viewed rows and the tail shortens their distance from it, so the
    /// offset drops with it.
    #[test]
    fn pinned_repl_scroll_follows_a_tail_that_shrank() {
        assert_eq!(pinned_repl_scroll(6, 7, 4, 8, &[(5, 0), (7, 5)]), 5);
    }

    /// The anchored entry is gone entirely: nothing trustworthy to measure
    /// from, so leave the view alone rather than guess at a delta.
    #[test]
    fn pinned_repl_scroll_leaves_the_view_alone_when_its_anchor_vanished() {
        assert_eq!(pinned_repl_scroll(6, 7, 4, 8, &[(5, 0), (9, 5)]), 6);
    }

    fn entry(eval_id: u64, code: &str, in_flight: bool) -> ReplEntry {
        ReplEntry {
            eval_id,
            code: code.to_string(),
            frames: Vec::new(),
            elapsed_ms: 0,
            in_flight,
            pkg_mode: false,
            origin: None,
        }
    }

    fn rendered_text(lines: &[RtLine<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn in_flight_entry_says_starting_while_repl_boots() {
        // THE repl_state deliverable: with the workspace's REPL child still
        // booting (precompiling), an in-flight entry must say so instead of
        // the generic "(running…)" — which is indistinguishable from a dead
        // kernel for the minutes a first-per-workspace boot takes.
        let log = vec![entry(1, "1+1", true)];
        let images = std::collections::HashMap::new();
        let (lines, _, _) = build_repl_lines(&log, &images, 800.0, 600.0, 8.0, 16.0, true);
        let text = rendered_text(&lines);
        assert!(
            text.contains("julia starting"),
            "starting boot must be named: {text}"
        );
        assert!(
            !text.contains("(running…)"),
            "the generic running line must be replaced, not doubled: {text}"
        );
    }

    #[test]
    fn in_flight_entry_says_running_once_ready() {
        let log = vec![entry(1, "1+1", true)];
        let images = std::collections::HashMap::new();
        let (lines, _, _) = build_repl_lines(&log, &images, 800.0, 600.0, 8.0, 16.0, false);
        let text = rendered_text(&lines);
        assert!(text.contains("(running…)"), "{text}");
        assert!(!text.contains("julia starting"), "{text}");
    }

    #[test]
    fn completed_entry_ignores_starting_flag() {
        // A finished entry renders its elapsed footer regardless of a boot
        // in progress (e.g. the user restarted the REPL after a run).
        let mut e = entry(1, "1+1", false);
        e.elapsed_ms = 42;
        let images = std::collections::HashMap::new();
        let (lines, _, _) = build_repl_lines(&[e], &images, 800.0, 600.0, 8.0, 16.0, true);
        let text = rendered_text(&lines);
        assert!(text.contains("(42 ms)"), "{text}");
        assert!(!text.contains("julia starting"), "{text}");
    }
}
