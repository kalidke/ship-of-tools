//! The chat drawer's view: where the held records sit in the body and their shaped buffers. The frame measures and
//! shapes only the records that reach the body; every other buffer is kept for a scroll, up to `SPARE` of them.

use std::collections::HashMap;

use chrono::FixedOffset;
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, Weight};
use sot_protocol::chat::ChatRecord;

use super::project::{project, Kind, Span};
use crate::ui::preview::markdown::BODY_SIZE;
use crate::ui::render::quad::ScreenRect;
use crate::ui::render::text::{set_buffer_rich_text, ExtraArea, EXTRA_TOP_PAD_PX};

/// Buffers kept besides those in view.
const SPARE: usize = 64;
const LINE_H: f32 = 22.0;

/// A record's place: `top` is relative to the body's top and negative for the one that crosses it.
#[derive(Debug, PartialEq)]
pub(super) struct Placed {
    pub index: usize,
    pub top: f32,
    pub height: f32,
}

/// The records that reach a body `body_h` tall, in order. `from` is the first record at the top; `None` follows the
/// newest, placed upward from the bottom. `height` is asked only of a record that reaches the body.
pub(super) fn place(count: usize, body_h: f32, from: Option<usize>, mut height: impl FnMut(usize) -> f32) -> Vec<Placed> {
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
                out.push(Placed { index, top: y, height: h });
            }
            out.reverse();
        }
        Some(from) => {
            let mut y = 0.0;
            for index in from.min(count)..count {
                if y >= body_h {
                    break;
                }
                let h = height(index);
                out.push(Placed { index, top: y, height: h });
                y += h;
            }
        }
    }
    out
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
    /// The first record at the body's top; `None` follows the newest.
    pub(super) from: Option<usize>,
    cache: HashMap<Key, Shaped>,
    placed: Vec<(Key, f32)>,
    body: Option<ScreenRect>,
}

impl ChatView {
    /// Places the records in `body` and shapes those that reach it; the rest of the cache is trimmed.
    pub(super) fn layout(&mut self, fonts: &mut FontSystem, records: &[&ChatRecord], body: ScreenRect, scale: f32, now: i64, offset: FixedOffset) {
        let width = body.w.max(1.0);
        let cache = &mut self.cache;
        let mut used: HashMap<usize, Key> = HashMap::new();
        let spots = place(records.len(), body.h, self.from, |at| {
            let key = (project(records[at], now, offset, false), width.to_bits(), scale.to_bits());
            let shaped = cache.entry(key.clone()).or_insert_with(|| shape(fonts, &key.0, width, scale));
            shaped.at = at;
            used.insert(at, key);
            shaped.height
        });
        self.placed = spots.iter().map(|p| (used.remove(&p.index).expect("asked"), p.top)).collect();
        self.body = Some(body);
        let centre = self.from.unwrap_or(records.len());
        let mut others: Vec<_> =
            self.cache.iter().filter(|(k, _)| !self.placed.iter().any(|(u, _)| u == *k)).map(|(k, s)| (s.at.abs_diff(centre), k.clone())).collect();
        others.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, key) in others.into_iter().take(self.cache.len().saturating_sub(self.placed.len() + SPARE)) {
            self.cache.remove(&key);
        }
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
    pub(super) fn cached(&self) -> usize {
        self.cache.len()
    }

    #[cfg(test)]
    pub(super) fn placed_tops(&self) -> Vec<f32> {
        self.placed.iter().map(|(_, t)| *t).collect()
    }
}

fn shape(fonts: &mut FontSystem, spans: &[Span], width: f32, scale: f32) -> Shaped {
    let mut buffer = Buffer::new(fonts, Metrics::new(BODY_SIZE * scale, LINE_H * scale));
    buffer.set_size(fonts, Some(width), None);
    let default = Attrs::new().family(Family::SansSerif);
    set_buffer_rich_text(&mut buffer, fonts, spans.iter().map(|s| (s.text.as_str(), attrs(s.kind))), default, Shaping::Advanced);
    buffer.shape_until_scroll(fonts, false);
    let height = buffer.layout_runs().map(|r| r.line_height).sum();
    Shaped { buffer, height, at: 0 }
}
