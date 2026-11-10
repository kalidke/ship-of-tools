//! The chat drawer's view: where the held records sit in the body and their shaped buffers. The frame measures and
//! shapes only the records that reach the body; every other buffer is kept for a scroll, up to `SPARE` of them.

use std::collections::HashMap;

use chrono::FixedOffset;
use cosmic_text::{Attrs, Buffer, Color, Cursor, Family, FontSystem, Metrics, Shaping, Style, Weight};
use sot_protocol::chat::{ChatRecord, Ts};

use super::owner::ts_order;
use super::project::{project, Kind, Span};
use crate::ui::flowed_selection::{FlowedSelection, FlowedText};
use crate::ui::preview::markdown::{BODY_LINE_H, BODY_SIZE};
use crate::ui::render::quad::ScreenRect;
use crate::ui::render::text::{set_buffer_rich_text, ExtraArea, EXTRA_TOP_PAD_PX};

/// Buffers kept besides those in view.
const SPARE: usize = 64;

/// A record's place: `top` is relative to the body's top and negative for the one that crosses it.
#[derive(Debug, PartialEq)]
pub(super) struct Placed {
    pub index: usize,
    pub top: f32,
}

/// The records that reach a body `body_h` tall, in order. `from` is the record at the top and its top's distance below the
/// body's top; `None` follows the newest, placed upward from the bottom. `height` is asked only of a record that reaches
/// the body.
pub(super) fn place(count: usize, body_h: f32, from: Option<(usize, f32)>, mut height: impl FnMut(usize) -> f32) -> Vec<Placed> {
    let mut out = Vec::new();
    match from {
        None => {
            let mut y = body_h;
            for index in (0..count).rev() {
                if y <= 0.0 {
                    break;
                }
                let h = height(index);
                y -= h;
                out.push(Placed { index, top: y });
            }
            out.reverse();
        }
        Some((from, dy)) => {
            let mut y = dy;
            for index in from.min(count)..count {
                if y >= body_h {
                    break;
                }
                let h = height(index);
                out.push(Placed { index, top: y });
                y += h;
            }
        }
    }
    out
}

/// A position kept honest: the records above the body's top are taken up while there is room above, none is left wholly
/// above it, and a position whose newest record is wholly in view is `None` (following).
pub(super) fn settle(count: usize, body_h: f32, (mut i, mut dy): (usize, f32), mut height: impl FnMut(usize) -> f32) -> Option<(usize, f32)> {
    while dy > 0.0 && i > 0 {
        i -= 1;
        dy -= height(i);
    }
    dy = dy.min(0.0);
    while i < count && dy + height(i) <= 0.0 {
        dy += height(i);
        i += 1;
    }
    let mut end = dy;
    (i..count).any(|j| { end += height(j); end > body_h }).then_some((i, dy))
}

/// How far a key moves the position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::ui) enum Step {
    Line,
    Page,
}

/// Each span kind has a style of its own; the kind alone tells a header from a body line that imitates one.
pub(super) fn attrs(kind: Kind) -> Attrs<'static> {
    let a = Attrs::new().family(Family::SansSerif);
    match kind {
        Kind::Header(marked) => a.weight(Weight::BOLD).color(if marked { Color::rgb(255, 215, 120) } else { Color::rgb(130, 200, 255) }),
        Kind::Origin => a.color(Color::rgb(200, 160, 255)),
        Kind::Body => a,
        Kind::Markup => a.color(Color::rgb(120, 220, 170)),
        Kind::Meta => a.style(Style::Italic).color(Color::rgb(150, 150, 150)),
    }
}

/// A buffer is shaped for its spans at a width and text scale (the floats' bits).
type Key = (Vec<Span>, u32, u32);

/// A press in the records area: the record (its time and buffer key) and the buffer position under the pointer when it
/// is on a body line, else none.
pub(in crate::ui) struct ChatHit(Option<((Ts, Key), Cursor)>);

struct Shaped {
    buffer: Buffer,
    height: f32,
    /// The record index it last served, for the eviction's distance.
    at: usize,
}

#[derive(Default)]
pub(in crate::ui) struct ChatView {
    /// The record at the body's top (by its time) and its top's distance below the body's top; `None` follows the newest.
    pub(super) anchor: Option<(Ts, f32)>,
    /// What keys and the wheel asked since the last frame, toward older records: lines and pages, turned into pixels by
    /// the frame that spends them, so a key before the first frame is kept and a resize or scale change does not matter.
    pending: (f32, f32),
    cache: HashMap<Key, Shaped>,
    placed: Vec<(Key, f32, Ts)>,
    body: Option<ScreenRect>,
    /// The oldest held record reaches the body: older history is wanted.
    pub(super) at_oldest: bool,
    /// The marked record (by its time), always a held record in view; `None` until a key marks.
    pub(super) mark: Option<Ts>,
    /// The mark keys since the last frame, in order (`true` toward older records): the frame that spends them knows the
    /// records, and folds them one at a time, each with its clamp.
    mark_keys: Vec<bool>,
    /// The text selected in one record's body (never its header), and that record.
    selection: FlowedSelection,
    selected: Option<(Ts, Key)>,
}

impl ChatView {
    /// Places the records in `body` and shapes those that reach it; the rest of the cache is trimmed.
    pub(in crate::ui) fn layout(&mut self, fonts: &mut FontSystem, records: &[(&ChatRecord, bool)], body: ScreenRect, scale: f32, now: i64, offset: FixedOffset) {
        let width = body.w.max(1.0);
        let cache = &mut self.cache;
        let mut used: HashMap<usize, Key> = HashMap::new();
        // The mark passes on like the anchor: to the nearest newer held record, or to none.
        let held_mark = self.mark.as_ref().map(|ts| find(records, ts)).filter(|i| *i < records.len());
        let marked = std::cell::Cell::new(held_mark);
        let mut height = |at: usize| {
            let key = (project(records[at].0, now, offset, marked.get() == Some(at), records[at].1), width.to_bits(), scale.to_bits());
            let shaped = cache.entry(key.clone()).or_insert_with(|| shape(fonts, &key.0, width, scale));
            shaped.at = at;
            used.insert(at, key);
            shaped.height
        };
        let count = records.len();
        // The anchor is found by its time: a record gone passes it to the nearest newer one, and a snapshot that replaced
        // every record does the same against the new set.
        let found = self.anchor.as_ref().map(|(ts, dy)| (find(records, ts), *dy));
        let pending = self.pending.0 * BODY_LINE_H * scale + self.pending.1 * body.h / 3.0;
        let found = match (found, pending > 0.0) {
            (None, true) if count > 0 => Some((count - 1, body.h - height(count - 1))),
            (found, _) => found,
        };
        let from = found.and_then(|(i, dy)| settle(count, body.h, (i, dy + pending), &mut height));
        self.pending = (0.0, 0.0);
        let mut spots = place(count, body.h, from, &mut height);
        // The mark: keys move it, one at a time, and the view shows it (a record above the body has its top at the body's
        // top, one below its bottom at the foot, one taller than the body its top). Then the mark is in view: its header
        // row is inside the body, else it goes to the nearest record whose is; when none is, to the first placed
        // record, which crosses the top and so covers all of the body but one line.
        let mut mark = held_mark;
        let keys = std::mem::take(&mut self.mark_keys);
        let from = if !keys.is_empty() && count > 0 {
            let i = keys.iter().fold(mark, |at, older| Some(at.map_or(count - 1, |i| if *older { i.saturating_sub(1) } else { (i + 1).min(count - 1) }))).expect("a key");
            mark = Some(i);
            let top = spots.iter().find(|p| p.index == i).map(|p| p.top);
            let h = height(i);
            if top.is_some_and(|t| t >= 0.0 && t + h <= body.h) {
                from
            } else {
                let above = top.map_or(spots.first().is_none_or(|p| i < p.index), |t| t < 0.0);
                let from = settle(count, body.h, (i, if above || h > body.h { 0.0 } else { body.h - h }), &mut height);
                spots = place(count, body.h, from, &mut height);
                from
            }
        } else {
            from
        };
        let line = BODY_LINE_H * scale;
        let mut seen: Vec<usize> = spots.iter().filter(|p| p.top >= 0.0 && p.top + line <= body.h).map(|p| p.index).collect();
        if seen.is_empty() {
            seen = spots.first().map(|p| p.index).into_iter().collect();
        }
        if let Some(i) = mark {
            mark = seen.iter().copied().min_by_key(|s| s.abs_diff(i)).or(mark);
        }
        marked.set(mark);
        if mark != held_mark {
            for p in &spots {
                height(p.index);
            }
        }
        self.mark = mark.map(|i| records[i].0.ts.clone());
        self.anchor = from.map(|(i, dy)| (records[i].0.ts.clone(), dy));
        self.at_oldest = spots.first().is_some_and(|p| p.index == 0);
        self.placed = spots.iter().map(|p| (used.remove(&p.index).expect("asked"), p.top, records[p.index].0.ts.clone())).collect();
        self.body = Some(body);
        let centre = from.map_or(count, |f| f.0);
        let mut others: Vec<_> =
            self.cache.iter().filter(|(k, _)| !self.placed.iter().any(|(u, ..)| u == *k)).map(|(k, s)| (s.at.abs_diff(centre), k.clone())).collect();
        others.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, key) in others.into_iter().take(self.cache.len().saturating_sub(self.placed.len() + SPARE)) {
            self.cache.remove(&key);
        }
        // A change to the selected record's buffer ends the selection: an edit, a delete, the mark's header, a record a
        // snapshot dropped, a new width or scale, or a buffer the trim let go.
        let live = self.selected.as_ref().is_none_or(|(ts, key)| {
            let i = find(records, ts);
            i < records.len() && records[i].0.ts == *ts && self.cache.contains_key(key)
                && key.0 == project(records[i].0, now, offset, mark == Some(i), records[i].1) && key.1 == width.to_bits() && key.2 == scale.to_bits()
        });
        if !live {
            self.clear_selection();
        }
    }

    /// `shaped` as the selection lends it: placed at `top`, or nowhere (an empty rect) when it is not in the body.
    fn lend<'a>(&self, shaped: &'a Shaped, top: Option<f32>) -> FlowedText<'a> {
        let body = self.body.unwrap_or(ScreenRect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 });
        let y = body.y + top.unwrap_or(-1e9);
        let (from, to) = (y.max(body.y), (y + shaped.height).min(body.y + body.h));
        let rect = ScreenRect { x: body.x, y: from, w: body.w, h: (to - from).max(0.0) };
        FlowedText { buffer: &shaped.buffer, rect, origin_y: y, clip_bottom: to, reserved: None }
    }

    /// The selected record's buffer, lent.
    fn lent(&self) -> Option<FlowedText<'_>> {
        let (ts, key) = self.selected.as_ref()?;
        Some(self.lend(self.cache.get(key)?, self.placed.iter().find(|p| p.2 == *ts).map(|p| p.1)))
    }

    /// A press in the body focuses the drawer; one on a body line (never a header or between records) also names the
    /// record and position.
    pub(in crate::ui) fn hit(&self, at: (f32, f32)) -> Option<ChatHit> {
        let body = self.body?;
        (at.0 >= body.x && at.0 < body.x + body.w && at.1 >= body.y && at.1 < body.y + body.h).then_some(())?;
        let line = self.placed.iter().find(|(k, top, _)| at.1 >= body.y + top && at.1 < body.y + top + self.cache[k].height).and_then(|(key, top, ts)| {
            let cursor = FlowedSelection::hit(&self.lend(&self.cache[key], Some(*top)), at, true).filter(|c| c.line > 0)?;
            Some(((ts.clone(), key.clone()), cursor))
        });
        Some(ChatHit(line))
    }

    /// A press replaces the selection: the record and position, or none; whether anything changed.
    pub(in crate::ui) fn press(&mut self, hit: Option<ChatHit>) -> bool {
        let line = hit.and_then(|h| h.0);
        let cursor = line.as_ref().map(|l| l.1);
        self.selected = line.map(|l| l.0);
        self.selection.press(cursor)
    }

    /// The drag's end follows the pointer, clamped to the record's body.
    pub(in crate::ui) fn extend_drag(&mut self, at: (f32, f32)) -> bool {
        let end = self.lent().and_then(|t| FlowedSelection::hit(&t, at, false)).map(|c| if c.line == 0 { Cursor::new(1, 0) } else { c });
        self.selection.extend_drag(end)
    }

    pub(in crate::ui) fn release(&mut self) {
        self.selection.release();
    }

    pub(in crate::ui) fn clear_selection(&mut self) {
        self.selection.clear();
        self.selected = None;
    }

    #[cfg(test)]
    pub(in crate::ui) fn selected_text(&self) -> Option<String> {
        self.selection.text(&self.lent()?)
    }

    /// The tint of the selection, one rectangle a visual line.
    pub(in crate::ui) fn selection_rects(&self) -> Vec<ScreenRect> {
        self.lent().map_or_else(Vec::new, |t| self.selection.rects(&t))
    }

    /// A key or the wheel moves the position `step` toward older records (`up`) or newer; it takes effect at the next frame.
    pub(super) fn scroll(&mut self, up: bool, step: Step) {
        let n = if up { 1.0 } else { -1.0 };
        match step {
            Step::Line => self.pending.0 += n,
            Step::Page => self.pending.1 += n,
        }
    }

    /// A mark key moves the mark one record toward older records (`older`) or newer; it takes effect at the next frame.
    pub(super) fn mark(&mut self, older: bool) {
        self.mark_keys.push(older);
    }

    /// A row change: the reader follows the newest, and the old row's mark is no one's.
    pub(super) fn reset(&mut self) {
        self.latest();
        (self.mark, self.mark_keys) = (None, Vec::new());
        self.clear_selection();
    }

    /// The reader asks for the newest: the view follows it again.
    pub(super) fn latest(&mut self) {
        (self.anchor, self.pending) = (None, (0.0, 0.0));
    }

    /// The drawer is not showing the chat: nothing is prepared.
    pub(super) fn hide(&mut self) {
        self.placed.clear();
        self.body = None;
    }

    /// The placed records as areas clipped to the body.
    pub(super) fn areas(&self) -> Vec<ExtraArea<'_>> {
        let Some(body) = self.body else { return Vec::new() };
        self.placed
            .iter()
            .map(|(key, top, _)| ExtraArea {
                buffer: &self.cache[key].buffer,
                x: body.x,
                y: body.y + top - EXTRA_TOP_PAD_PX,
                right: body.x + body.w,
                bottom: body.y + body.h,
                clip_left: Some(body.x),
                clip_top: Some(body.y),
                color: (220, 220, 220),
                scroll_y_px: 0.0,
            })
            .collect()
    }

    /// Where the glyph of `ts`'s record at buffer `(line, byte)` sits on screen.
    #[cfg(test)]
    pub(in crate::ui) fn point_at(&self, ts: &Ts, line: usize, byte: usize) -> Option<(f32, f32)> {
        let body = self.body?;
        let (key, top, _) = self.placed.iter().find(|p| &p.2 == ts)?;
        self.cache[key].buffer.layout_runs().filter(|r| r.line_i == line).find_map(|run| {
            let g = run.glyphs.iter().find(|g| g.start == byte)?;
            Some((body.x + g.x + g.w * 0.15, body.y + top + run.line_top + run.line_height * 0.5))
        })
    }

    #[cfg(test)]
    pub(super) fn holds(&self, spans: &[Span]) -> bool {
        self.cache.keys().any(|k| k.0 == spans)
    }

    /// The placed records' text, in placement order.
    #[cfg(test)]
    pub(super) fn placed_text(&self) -> Vec<String> {
        self.placed.iter().map(|(k, ..)| k.0.iter().map(|s| s.text.as_str()).collect()).collect()
    }

    /// The placed records' tops, in placement order.
    #[cfg(test)]
    pub(super) fn placed_tops(&self) -> Vec<f32> {
        self.placed.iter().map(|(_, top, _)| *top).collect()
    }

    /// Whether each placed record's header is the marked one, in placement order.
    #[cfg(test)]
    pub(super) fn placed_marks(&self) -> Vec<bool> {
        self.placed.iter().map(|(k, ..)| k.0[0].kind == Kind::Header(true)).collect()
    }

    #[cfg(test)]
    pub(super) fn cached(&self) -> usize {
        self.cache.len()
    }
}

/// Where `ts` sits among the records by time: a record gone gives the nearest newer one, and `records.len()` when none is.
fn find(records: &[(&ChatRecord, bool)], ts: &Ts) -> usize {
    records.partition_point(|r| ts_order(&r.0.ts) < ts_order(ts))
}

fn shape(fonts: &mut FontSystem, spans: &[Span], width: f32, scale: f32) -> Shaped {
    let mut buffer = Buffer::new(fonts, Metrics::new(BODY_SIZE * scale, BODY_LINE_H * scale));
    buffer.set_size(fonts, Some(width), None);
    let default = Attrs::new().family(Family::SansSerif);
    set_buffer_rich_text(&mut buffer, fonts, spans.iter().map(|s| (s.text.as_str(), attrs(s.kind))), default, Shaping::Advanced);
    buffer.shape_until_scroll(fonts, false);
    let height = buffer.layout_runs().map(|r| r.line_height).sum();
    Shaped { buffer, height, at: 0 }
}
