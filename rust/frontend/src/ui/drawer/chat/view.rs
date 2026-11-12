//! The chat drawer's view: where the held records sit in the body and their shaped buffers. The frame measures and
//! shapes only the records that reach the body; every other buffer is kept for a scroll, up to `SPARE` of them.

use std::collections::HashMap;

use chrono::FixedOffset;
use cosmic_text::{Align, Attrs, Buffer, Color, Cursor, Family, FontSystem, Metrics, Shaping, Style, Weight};
use sot_protocol::chat::{ChatAuthor, ChatRecord, Ts};

use super::owner::ts_order;
use super::mrkdwn::{BOLD, CODE, ITALIC, STRIKE};
use super::project::{clock, project, Kind, Span};
use crate::ui::flowed_selection::{FlowedSelection, FlowedText};
use crate::ui::preview::markdown::{BODY_LINE_H, BODY_SIZE};
use crate::ui::render::cells::CellQuad;
use crate::ui::render::quad::ScreenRect;
use crate::ui::render::text::{flagged_runs, set_buffer_rich_text, ExtraArea, EXTRA_TOP_PAD_PX};

/// Buffers kept besides those in view.
const SPARE: usize = 64;

// The palette: Slack Kit's dark mode chart is the source of every value but the selection's tint, which is the chart's
// Sky Blue at 30% over the background. Every rectangle is opaque on `BG`.
const BG: (u8, u8, u8) = (0x1A, 0x1D, 0x21);
const TEXT: (u8, u8, u8) = (0xD1, 0xD2, 0xD3);
const MUTED: (u8, u8, u8) = (0x9A, 0x9C, 0x9E);
/// The badge's fill.
const LOW: (u8, u8, u8) = (0x32, 0x35, 0x38);
/// The mark's band, and an inline code's fill.
const BAND: (u8, u8, u8) = (0x21, 0x24, 0x28);
const SKY: (u8, u8, u8) = (0x1D, 0x9B, 0xD1);
/// A mention's slug: Sky Blue at about 10% over the background.
const SLUG: (u8, u8, u8) = (0x18, 0x29, 0x34);
const SELECTION: (u8, u8, u8) = (0x1B, 0x43, 0x56);
/// Inline code's text: the one value here that is not the chart's, chosen to read as code on `BAND`.
const CODE_TEXT: (u8, u8, u8) = (0xE8, 0x91, 0x2D);
/// The mark's bar at the record's left edge, in pixels at text scale 1.
const MARK_BAR_PX: f32 = 2.0;
/// A badge's rectangle spans this part of the header line, starting this far down it (the rest is above and below).
const BADGE_TOP: f32 = 0.2;
const BADGE_HEIGHT: f32 = 0.6;
/// The glyph flag `Attrs::metadata` carries on a badge's text, which the badge's rectangle is drawn behind.
const BADGE: usize = 0x01;
/// The same for a day divider's label, which its pill is drawn around.
const DAY: usize = 0x02;
/// The glyph flags of inline code, whose rectangle is drawn behind it, and of strike, which a rule is drawn through.
const CODE_RUN: usize = 0x04;
const STRIKE_RUN: usize = 0x08;
/// The same for a mention, whose slug is drawn behind it: a rectangle 1 px wider each side than its run, inset like code's.
const MENTION_RUN: usize = 0x10;
const MENTION_PAD_PX: f32 = 1.0;
/// Inline code's size (of the body's 15), and its rectangle's padding left and right and its inset top and bottom, in pixels
/// at text scale 1.
const CODE_SIZE: f32 = 14.0;
const CODE_PAD_PX: f32 = 3.0;
const CODE_INSET_PX: f32 = 2.0;
/// A divider's line, in pixels at text scale 1: its height, and the label's size (of the body's 15). The pill is a body line
/// tall and centered in it, and the rule runs from the tile's left edge to the pill and from it to the same distance from the
/// right edge.
const DAY_PX: f32 = 30.0;
const DAY_SIZE: f32 = 13.0;
/// The gutter left of every record holds a group's tile and a marked continuation's time. These are in pixels at text
/// scale 1: the gutter's width, the tile's side and its place in the gutter, and the time's size (of the body's 15) and its
/// gap to the gutter's right edge.
pub(super) const GUTTER_PX: f32 = 52.0;
pub(super) const TILE_PX: f32 = 34.0;
pub(super) const TILE_LEFT_PX: f32 = 8.0;
const TILE_TOP_PX: f32 = 2.0;
const STAMP_SIZE: f32 = 11.0;
const STAMP_GAP_PX: f32 = 8.0;
/// The tile colors, chosen by a hash of the author's id and name.
pub(super) const TILES: [(u8, u8, u8); 8] =
    [(0x2D, 0x9E, 0xE0), (0xE0, 0xA0, 0x2D), (0x3F, 0xA3, 0x6B), (0xC9, 0x50, 0x7B), (0x8A, 0x63, 0xD2), (0xD9, 0x69, 0x3B), (0x2F, 0xA8, 0xA8), (0x7A, 0x8B, 0x3A)];

/// The author's tile: FNV-1a over the id and the name, so one author keeps one color from run to run.
pub(super) fn tile_color(a: &ChatAuthor) -> (u8, u8, u8) {
    let hash = a.id.bytes().chain([0xff]).chain(a.name.bytes()).fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    TILES[(hash % TILES.len() as u64) as usize]
}

/// Where a record's text sits in `body`: right of the gutter. The buffers are shaped at its width, and the areas, the
/// selection's lend, the press and the glyphs' places all start from it.
fn text_rect(body: ScreenRect, scale: f32) -> ScreenRect {
    let gutter = (GUTTER_PX * scale).min(body.w);
    ScreenRect { x: body.x + gutter, w: body.w - gutter, ..body }
}

fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::rgb(r, g, b)
}

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

/// Each span kind has a style of its own; the kind alone tells a header from a body line that imitates one. The time and
/// the badge are smaller than the body, but keep its line height, so a header line is as tall as a body line.
pub(super) fn attrs(kind: Kind, scale: f32) -> Attrs<'static> {
    let a = Attrs::new().family(Family::SansSerif).color(rgb(TEXT));
    let small = |size: f32| Metrics::new(BODY_SIZE * scale * size / 15.0, BODY_LINE_H * scale);
    match kind {
        Kind::Name => a.weight(Weight::BOLD),
        Kind::Day => a.weight(Weight::BOLD).metrics(Metrics::new(BODY_SIZE * scale * DAY_SIZE / 15.0, DAY_PX * scale)).metadata(DAY),
        Kind::Time => a.color(rgb(MUTED)).metrics(small(12.0)),
        Kind::Replies => a.weight(Weight::BOLD).color(rgb(SKY)).metrics(small(DAY_SIZE)),
        Kind::Last => a.color(rgb(MUTED)).metrics(small(DAY_SIZE)),
        Kind::Badge => a.weight(Weight::BOLD).color(rgb(MUTED)).metrics(small(10.0)).metadata(BADGE),
        Kind::Body => a,
        Kind::Markup | Kind::Link => a.color(rgb(SKY)),
        Kind::Mention => a.color(rgb(SKY)).metadata(MENTION_RUN),
        Kind::Meta => a.style(Style::Italic).color(rgb(MUTED)),
    }
}

/// `attrs`, and a span's inline styles: weight, slant, the strike's glyph flag, and code's monospace face, size, color and
/// glyph flag. Code takes no other style.
pub(super) fn styled_attrs(kind: Kind, style: u8, scale: f32) -> Attrs<'static> {
    let mut a = attrs(kind, scale);
    if style & CODE != 0 {
        return a.family(Family::Monospace).color(rgb(CODE_TEXT)).metrics(Metrics::new(BODY_SIZE * scale * CODE_SIZE / 15.0, BODY_LINE_H * scale)).metadata(CODE_RUN);
    }
    if style & BOLD != 0 {
        a = a.weight(Weight::BOLD);
    }
    if style & ITALIC != 0 {
        a = a.style(Style::Italic);
    }
    if style & STRIKE != 0 {
        a = a.metadata(a.metadata | STRIKE_RUN);
    }
    a
}

/// A buffer is shaped for its spans at a width and text scale (the floats' bits).
type Key = (Vec<Span>, u32, u32);

/// A press in the records area: the record (its time and buffer key) and the buffer position under the pointer when it
/// is on a body line, else none.
pub(in crate::ui) struct ChatHit(Option<((Ts, Key), Cursor)>);

struct Shaped {
    buffer: Buffer,
    height: f32,
    /// The lines before the body: a divider's, if any, and a header's, if any. Only lines from here on select or copy.
    lead: usize,
    /// The divider's height (0 without one; one line, more where a narrow drawer wraps its label), at the top of the record: the band, the bar and the tile start below it.
    day: f32,
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
    /// The records that reach the body, in order: buffer key, top, time, and the group's tile color when it starts one.
    placed: Vec<(Key, f32, Ts, Option<(u8, u8, u8)>)>,
    body: Option<ScreenRect>,
    /// The text scale of the last layout.
    scale: f32,
    /// A marked continuation's time for the gutter: its text, the scale (bits) and its buffer, shaped again only when either
    /// changes.
    stamp: Option<(String, u32, Buffer)>,
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
        let width = text_rect(body, scale).w.max(1.0);
        let cache = &mut self.cache;
        let mut used: HashMap<usize, Key> = HashMap::new();
        // The mark passes on like the anchor: to the nearest newer held record, or to none.
        let held_mark = self.mark.as_ref().map(|ts| find(records, ts)).filter(|i| *i < records.len());
        let mut height = |at: usize| {
            let key = (project(records[at].0, at.checked_sub(1).map(|i| records[i].0), now, offset, records[at].1), width.to_bits(), scale.to_bits());
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
        // top, one below its bottom at the foot, one taller than the body its top). Then the mark is in view: its first
        // line is inside the body, else it goes to the nearest record whose is; when none is, to the first placed
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
        // A record's band starts below its divider, so its first line is the one that must fit.
        let line = BODY_LINE_H * scale;
        let mut seen: Vec<usize> = spots.iter().filter(|p| p.top >= 0.0 && p.top + cache[&used[&p.index]].day + line <= body.h).map(|p| p.index).collect();
        if seen.is_empty() {
            seen = spots.first().map(|p| p.index).into_iter().collect();
        }
        if let Some(i) = mark {
            mark = seen.iter().copied().min_by_key(|s| s.abs_diff(i)).or(mark);
        }
        self.mark = mark.map(|i| records[i].0.ts.clone());
        self.anchor = from.map(|(i, dy)| (records[i].0.ts.clone(), dy));
        self.at_oldest = spots.first().is_some_and(|p| p.index == 0);
        self.placed = spots
            .iter()
            .map(|p| {
                let key = used.remove(&p.index).expect("asked");
                let tile = (cache[&key].lead > 0).then(|| tile_color(&records[p.index].0.author));
                (key, p.top, records[p.index].0.ts.clone(), tile)
            })
            .collect();
        // A marked continuation shows its time in the gutter.
        let time = self.mark.as_ref().and_then(|ts| {
            let (key, ..) = self.placed.iter().find(|p| &p.2 == ts)?;
            if cache[key].lead > 0 { return None; }
            clock(records[find(records, ts)].0.ts.as_str(), offset)
        });
        self.stamp = match (time, self.stamp.take()) {
            (None, _) => None,
            (Some(t), Some((old, bits, b))) if old == t && bits == scale.to_bits() => Some((old, bits, b)),
            (Some(t), _) => {
                let b = shape_stamp(fonts, &t, scale);
                Some((t, scale.to_bits(), b))
            }
        };
        self.body = Some(body);
        self.scale = scale;
        let centre = from.map_or(count, |f| f.0);
        let mut others: Vec<_> =
            self.cache.iter().filter(|(k, _)| !self.placed.iter().any(|(u, ..)| u == *k)).map(|(k, s)| (s.at.abs_diff(centre), k.clone())).collect();
        others.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, key) in others.into_iter().take(self.cache.len().saturating_sub(self.placed.len() + SPARE)) {
            self.cache.remove(&key);
        }
        // A change to the selected record's buffer ends the selection: an edit, a delete, a record a snapshot dropped, a new
        // width or scale, or a buffer the trim let go.
        let live = self.selected.as_ref().is_none_or(|(ts, key)| {
            let i = find(records, ts);
            i < records.len() && records[i].0.ts == *ts && self.cache.contains_key(key)
                && key.0 == project(records[i].0, i.checked_sub(1).map(|j| records[j].0), now, offset, records[i].1) && key.1 == width.to_bits() && key.2 == scale.to_bits()
        });
        if !live {
            self.clear_selection();
        }
    }

    /// `shaped` as the selection lends it: placed at `top`, or nowhere (an empty rect) when it is not in the body.
    fn lend<'a>(&self, shaped: &'a Shaped, top: Option<f32>) -> FlowedText<'a> {
        let body = self.body.unwrap_or(ScreenRect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 });
        let text = text_rect(body, self.scale);
        let y = body.y + top.unwrap_or(-1e9);
        let (from, to) = (y.max(body.y), (y + shaped.height).min(body.y + body.h));
        let rect = ScreenRect { x: text.x, y: from, w: text.w, h: (to - from).max(0.0) };
        FlowedText { buffer: &shaped.buffer, rect, origin_y: y, clip_bottom: to, reserved: None }
    }

    /// The selected record's buffer, lent.
    fn lent(&self) -> Option<FlowedText<'_>> {
        let (ts, key) = self.selected.as_ref()?;
        Some(self.lend(self.cache.get(key)?, self.placed.iter().find(|p| p.2 == *ts).map(|p| p.1)))
    }

    /// A press in the body focuses the drawer; one on a body line (never a header, the gutter or between records) also names the
    /// record and position.
    pub(in crate::ui) fn hit(&self, at: (f32, f32)) -> Option<ChatHit> {
        let body = self.body?;
        (at.0 >= body.x && at.0 < body.x + body.w && at.1 >= body.y && at.1 < body.y + body.h).then_some(())?;
        let line = self.placed.iter().find(|(k, top, ..)| at.1 >= body.y + top && at.1 < body.y + top + self.cache[k].height).and_then(|(key, top, ts, _)| {
            let cursor = FlowedSelection::hit(&self.lend(&self.cache[key], Some(*top)), at, true).filter(|c| c.line >= self.cache[key].lead)?;
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
        let lead = self.selected.as_ref().and_then(|(_, key)| self.cache.get(key)).map_or(0, |s| s.lead);
        let end = self.lent().and_then(|t| FlowedSelection::hit(&t, at, false)).map(|c| if c.line < lead { Cursor::new(lead, 0) } else { c });
        self.selection.extend_drag(end)
    }

    pub(in crate::ui) fn release(&mut self) {
        self.selection.release();
    }

    pub(in crate::ui) fn clear_selection(&mut self) {
        self.selection.clear();
        self.selected = None;
    }

    /// The selected bytes of the record's buffer.
    pub(in crate::ui) fn selected_text(&self) -> Option<String> {
        self.selection.text(&self.lent()?)
    }

    /// The marked record's shown text: its buffer after its lead (the header line, if it has one).
    pub(in crate::ui) fn marked_text(&self) -> Option<String> {
        let ts = self.mark.as_ref()?;
        let key = &self.placed.iter().find(|p| &p.2 == ts)?.0;
        let text = self.cache[key].buffer.lines.iter().skip(self.cache[key].lead).map(|l| l.text()).collect::<Vec<_>>().join("\n");
        (!text.is_empty()).then_some(text)
    }

    /// What `chat.copy_selection` copies: the selection, else the marked message's shown text.
    pub(in crate::ui) fn copy_text(&self) -> Option<String> {
        self.selected_text().or_else(|| self.marked_text())
    }

    /// The tint of the selection, one rectangle a visual line.
    pub(super) fn selection_rects(&self) -> Vec<ScreenRect> {
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

    /// The placed records as areas clipped to the body, and a marked continuation's time in its gutter.
    pub(super) fn areas(&self) -> Vec<ExtraArea<'_>> {
        let Some(body) = self.body else { return Vec::new() };
        let text = text_rect(body, self.scale);
        let area = |buffer, x, top: f32, color| ExtraArea {
            buffer,
            x,
            y: body.y + top - EXTRA_TOP_PAD_PX,
            right: body.x + body.w,
            bottom: body.y + body.h,
            clip_left: Some(body.x),
            clip_top: Some(body.y),
            color,
            scroll_y_px: 0.0,
        };
        let mut out: Vec<_> = self.placed.iter().map(|(key, top, ..)| area(&self.cache[key].buffer, text.x, *top, TEXT)).collect();
        let marked = self.mark.as_ref().and_then(|ts| self.placed.iter().find(|p| &p.2 == ts));
        out.extend(self.stamp.as_ref().zip(marked).map(|((_, _, buffer), p)| area(buffer, body.x, p.1, MUTED)));
        out
    }

    /// The frame's rectangles in painting order: the background of the drawer's `inside` (the body and the compose foot),
    /// then each placed record's mark band and bar and badges, and last the selection's tint over every fill. All but the
    /// background are clipped to the body. Nothing while the chat is hidden. They are rectangles of the cell backgrounds'
    /// palette, which paints first in the pass, so the drawer's cells and the glyphs lie over them.
    pub(super) fn backs(&self, inside: ScreenRect) -> Vec<CellQuad> {
        let Some(body) = self.body else { return Vec::new() };
        let quad = |r: ScreenRect, color| {
            let (left, top) = (r.x.max(body.x), r.y.max(body.y));
            let (right, bottom) = ((r.x + r.w).min(body.x + body.w), (r.y + r.h).min(body.y + body.h));
            (right > left && bottom > top).then_some(CellQuad { x: left, y: top, w: right - left, h: bottom - top, color })
        };
        // A `LOW` border one pixel wide (at the text scale) and `fill` inside it: the day pill's, and an inline code run's.
        let mut out = vec![CellQuad { x: inside.x, y: inside.y, w: inside.w, h: inside.h, color: BG }];
        let text = text_rect(body, self.scale);
        for (key, top, ts, tile) in &self.placed {
            let shaped = &self.cache[key];
            let scale = f32::from_bits(key.2);
            let boxed = |r: ScreenRect, fill| [quad(r, LOW), quad(ScreenRect { x: r.x + scale, y: r.y + scale, w: r.w - 2.0 * scale, h: r.h - 2.0 * scale }, fill)].into_iter().flatten();
            let all = ScreenRect { x: body.x, y: body.y + top, w: body.w, h: shaped.height };
            let at = ScreenRect { y: all.y + shaped.day, h: all.h - shaped.day, ..all };
            if self.mark.as_ref() == Some(ts) {
                out.extend(quad(at, BAND));
                out.extend(quad(ScreenRect { w: MARK_BAR_PX * scale, ..at }, SKY));
            }
            if let Some(color) = tile {
                out.extend(quad(ScreenRect { x: body.x + TILE_LEFT_PX * scale, y: at.y + TILE_TOP_PX * scale, w: TILE_PX * scale, h: TILE_PX * scale }, *color));
            }
            // A divider's pill is its label's run with its padding, a body line tall: a border and the ground inside it, and the
            // rule runs to each side of it.
            for (x, y, w, h) in flagged_runs(&shaped.buffer, |m| m & DAY != 0) {
                let line = BODY_LINE_H * scale;
                let pill = ScreenRect { x: text.x + x, y: all.y + y + (h - line) / 2.0, w, h: line };
                let rule = |from: f32, to: f32| ScreenRect { x: from, y: pill.y + line / 2.0 - scale / 2.0, w: to - from, h: scale };
                out.extend(quad(rule(body.x + TILE_LEFT_PX * scale, pill.x), LOW));
                out.extend(quad(rule(pill.x + w, body.x + body.w - TILE_LEFT_PX * scale), LOW));
                out.extend(boxed(pill, BG));
            }
            // Inline code is a bordered fill around its run, padded left and right, and a mention's slug a fill around its; strike is a rule through the middle of its.
            for (x, y, w, h) in flagged_runs(&shaped.buffer, |m| m & CODE_RUN != 0) {
                let outer = ScreenRect { x: text.x + x - CODE_PAD_PX * scale, y: all.y + y + CODE_INSET_PX * scale, w: w + 2.0 * CODE_PAD_PX * scale, h: h - 2.0 * CODE_INSET_PX * scale };
                out.extend(boxed(outer, BAND));
            }
            for (x, y, w, h) in flagged_runs(&shaped.buffer, |m| m & MENTION_RUN != 0) {
                out.extend(quad(ScreenRect { x: text.x + x - MENTION_PAD_PX * scale, y: all.y + y + CODE_INSET_PX * scale, w: w + 2.0 * MENTION_PAD_PX * scale, h: h - 2.0 * CODE_INSET_PX * scale }, SLUG));
            }
            for (x, y, w, h) in flagged_runs(&shaped.buffer, |m| m & STRIKE_RUN != 0) {
                out.extend(quad(ScreenRect { x: text.x + x, y: all.y + y + (h - scale) / 2.0, w, h: scale }, TEXT));
            }
            // A badge's rectangle is its run of glyphs, which include its padding (`project`), so a wrapped line never cuts it.
            for (x, y, w, h) in flagged_runs(&shaped.buffer, |m| m & BADGE != 0) {
                out.extend(quad(ScreenRect { x: text.x + x, y: all.y + y + h * BADGE_TOP, w, h: h * BADGE_HEIGHT }, LOW));
            }
        }
        out.extend(self.selection_rects().into_iter().filter_map(|r| quad(r, SELECTION)));
        out
    }

    /// Where the glyph of `ts`'s record at buffer `(line, byte)` sits on screen.
    #[cfg(test)]
    pub(in crate::ui) fn point_at(&self, ts: &Ts, line: usize, byte: usize) -> Option<(f32, f32)> {
        let body = self.body?;
        let (key, top, ..) = self.placed.iter().find(|p| &p.2 == ts)?;
        self.cache[key].buffer.layout_runs().filter(|r| r.line_i == line).find_map(|run| {
            let g = run.glyphs.iter().find(|g| g.start == byte)?;
            Some((text_rect(body, self.scale).x + g.x + g.w * 0.15, body.y + top + run.line_top + run.line_height * 0.5))
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
        self.placed.iter().map(|(_, top, ..)| *top).collect()
    }

    /// The placed records' buffers, in placement order.
    #[cfg(test)]
    pub(super) fn placed_buffers(&self) -> Vec<&Buffer> {
        self.placed.iter().map(|(k, ..)| &self.cache[k].buffer).collect()
    }

    /// Whether each placed record is the marked one, in placement order.
    #[cfg(test)]
    pub(super) fn placed_marks(&self) -> Vec<bool> {
        self.placed.iter().map(|p| self.mark.as_ref() == Some(&p.2)).collect()
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
    set_buffer_rich_text(&mut buffer, fonts, spans.iter().map(|s| (s.text.as_str(), styled_attrs(s.kind, s.style, scale))), default, Shaping::Advanced);
    if spans.first().is_some_and(|s| s.kind == Kind::Day) {
        buffer.lines[0].set_align(Some(Align::Center));
    }
    buffer.shape_until_scroll(fonts, false);
    let day = spans.first().is_some_and(|s| s.kind == Kind::Day);
    let lead = usize::from(day) + usize::from(spans.get(usize::from(day)).is_some_and(|s| s.kind == Kind::Name));
    // The divider's height is what its line takes where it is shaped: a narrow drawer wraps the label into more than one.
    let day = if day { buffer.layout_runs().filter(|r| r.line_i == 0).map(|r| r.line_height).sum() } else { 0.0 };
    // A group's first record is at least as tall as its tile, so a header alone (empty text) does not run it into the next.
    let tile = if lead > 0 { day + (TILE_TOP_PX + TILE_PX) * scale } else { 0.0 };
    let height = buffer.layout_runs().map(|r| r.line_height).sum::<f32>().max(tile);
    Shaped { buffer, height, lead, day, at: 0 }
}

/// A continuation's time, small and muted, right-aligned in the gutter.
fn shape_stamp(fonts: &mut FontSystem, text: &str, scale: f32) -> Buffer {
    let mut buffer = Buffer::new(fonts, Metrics::new(BODY_SIZE * scale, BODY_LINE_H * scale));
    buffer.set_size(fonts, Some((GUTTER_PX - STAMP_GAP_PX) * scale), None);
    let default = Attrs::new().family(Family::SansSerif);
    let small = Metrics::new(BODY_SIZE * scale * STAMP_SIZE / 15.0, BODY_LINE_H * scale);
    set_buffer_rich_text(&mut buffer, fonts, [(text, default.color(rgb(MUTED)).metrics(small))], default, Shaping::Advanced);
    for line in buffer.lines.iter_mut() {
        line.set_align(Some(Align::Right));
    }
    buffer.shape_until_scroll(fonts, false);
    buffer
}
