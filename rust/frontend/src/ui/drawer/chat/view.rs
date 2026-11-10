//! The chat drawer's view: where the held records sit in the body and their shaped buffers. The frame measures and
//! shapes only the records that reach the body; every other buffer is kept for a scroll, up to `SPARE` of them.

use std::collections::HashMap;

use chrono::FixedOffset;
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, Weight};
use sot_protocol::chat::{ChatRecord, Ts};

use super::owner::ts_order;
use super::project::{project, Kind, Span};
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
    placed: Vec<(Key, f32)>,
    body: Option<ScreenRect>,
    /// The oldest held record reaches the body: older history is wanted.
    pub(super) at_oldest: bool,
    /// The marked record (by its time), always a held record in view; `None` until a key marks.
    pub(super) mark: Option<Ts>,
    /// What the mark keys asked since the last frame, toward older records: the frame that spends them knows the records.
    mark_moves: i32,
}

impl ChatView {
    /// Places the records in `body` and shapes those that reach it; the rest of the cache is trimmed.
    pub(super) fn layout(&mut self, fonts: &mut FontSystem, records: &[(&ChatRecord, bool)], body: ScreenRect, scale: f32, now: i64, offset: FixedOffset) {
        let width = body.w.max(1.0);
        let cache = &mut self.cache;
        let mut used: HashMap<usize, Key> = HashMap::new();
        // The mark passes on like the anchor: to the nearest newer held record, or to none.
        let held_mark = self.mark.as_ref().map(|ts| records.partition_point(|r| ts_order(&r.0.ts) < ts_order(ts))).filter(|i| *i < records.len());
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
        let found = self.anchor.as_ref().map(|(ts, dy)| (records.partition_point(|r| ts_order(&r.0.ts) < ts_order(ts)), *dy));
        let pending = self.pending.0 * BODY_LINE_H * scale + self.pending.1 * body.h / 3.0;
        let found = match (found, pending > 0.0) {
            (None, true) if count > 0 => Some((count - 1, body.h - height(count - 1))),
            (found, _) => found,
        };
        let from = found.and_then(|(i, dy)| settle(count, body.h, (i, dy + pending), &mut height));
        self.pending = (0.0, 0.0);
        let mut spots = place(count, body.h, from, &mut height);
        // The mark: keys move it and the view shows it (a record above the body has its top at the body's top, one below its
        // bottom at the foot, one taller than the body its top); otherwise a mark outside the body goes to the nearest record
        // in view, one with at least one row inside the body, which the placed records are.
        let mut mark = held_mark;
        let moves = std::mem::take(&mut self.mark_moves);
        let from = if moves != 0 && count > 0 {
            let (start, rest) = mark.map_or((count - 1, moves - moves.signum()), |i| (i, moves));
            let i = (start as i64 - i64::from(rest)).clamp(0, count as i64 - 1) as usize;
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
        if let (Some(i), Some(first), Some(last)) = (mark, spots.first(), spots.last()) {
            mark = Some(i.clamp(first.index, last.index));
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
        self.placed = spots.iter().map(|p| (used.remove(&p.index).expect("asked"), p.top)).collect();
        self.body = Some(body);
        let centre = from.map_or(count, |f| f.0);
        let mut others: Vec<_> =
            self.cache.iter().filter(|(k, _)| !self.placed.iter().any(|(u, _)| u == *k)).map(|(k, s)| (s.at.abs_diff(centre), k.clone())).collect();
        others.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, key) in others.into_iter().take(self.cache.len().saturating_sub(self.placed.len() + SPARE)) {
            self.cache.remove(&key);
        }
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
        self.mark_moves += if older { 1 } else { -1 };
    }

    /// Another row or channel: the reader follows the newest, and the old place's mark is no one's.
    pub(super) fn reset(&mut self) {
        self.latest();
        (self.mark, self.mark_moves) = (None, 0);
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
            .map(|(key, top)| ExtraArea {
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

    #[cfg(test)]
    pub(super) fn holds(&self, spans: &[Span]) -> bool {
        self.cache.keys().any(|k| k.0 == spans)
    }

    /// The placed records' text, in placement order.
    #[cfg(test)]
    pub(super) fn placed_text(&self) -> Vec<String> {
        self.placed.iter().map(|(k, _)| k.0.iter().map(|s| s.text.as_str()).collect()).collect()
    }

    /// The placed records' tops, in placement order.
    #[cfg(test)]
    pub(super) fn placed_tops(&self) -> Vec<f32> {
        self.placed.iter().map(|(_, top)| *top).collect()
    }

    /// Whether each placed record's header is the marked one, in placement order.
    #[cfg(test)]
    pub(super) fn placed_marks(&self) -> Vec<bool> {
        self.placed.iter().map(|(k, _)| k.0[0].kind == Kind::Header(true)).collect()
    }

    #[cfg(test)]
    pub(super) fn cached(&self) -> usize {
        self.cache.len()
    }
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
