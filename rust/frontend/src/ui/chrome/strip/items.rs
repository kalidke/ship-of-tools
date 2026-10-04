//! Strip items, labels, widths and cursor positions: what the band lays out, left to right.

use super::*;

/// Max characters shown for one session label in the bottom strip before
/// truncating with an ellipsis — keeps a long workspace name from dominating.
const STRIP_MAX_LABEL: usize = 24;
/// Gap between adjacent session labels in the strip, in cell-widths.
pub(in crate::ui) const STRIP_GAP_CELLS: f32 = 3.0;
/// Cells of clear water between one ship's stern and the next ship's rake —
/// the FLEET's word space, against the three cells between session names. A
/// host boundary equal to one word space did no grouping work at all (22–23 px
/// of it against a 24 px word gap), so a ship reads as a ship only when the
/// water around it is wider than the space inside it. `strip_gap_before` spends
/// it, and it is what keeps a rake from ever touching the stern ahead of it.
pub(in crate::ui) const STRIP_SHIP_GAP_CELLS: f32 = 5.0;
/// Cells between a ship's LAST session name and its stern riser, so the riser
/// never crowds the name it closes behind — 5 px of clearance against a
/// 20–24 px word rhythm read as a collision. `ship_marks` places the stern
/// here; no item width reserves it, so `strip_gap_before` is where it is paid
/// for across a ship boundary.
pub(in crate::ui) const STRIP_STERN_CLEAR_CELLS: f32 = 2.0;

/// Truncate a session label to `STRIP_MAX_LABEL` chars, ellipsizing if longer.
pub(in crate::ui) fn strip_truncate(label: &str) -> String {
    let n = label.chars().count();
    if n <= STRIP_MAX_LABEL {
        label.to_string()
    } else {
        let mut s: String = label
            .chars()
            .take(STRIP_MAX_LABEL.saturating_sub(1))
            .collect();
        s.push('…');
        s
    }
}

/// The text the strip draws for one session, and so the text every width the
/// strip reserves for it is measured from: the badge-floor sigil (ADR 0025 §1)
/// when a nav.preview result is waiting, then the label, truncated as ONE
/// string so a badged name is never wider than `STRIP_MAX_LABEL`. The hull,
/// the cursor walk, the centring target and the drawn line all read this one
/// string; a badge added anywhere else grows the name past the hull reserved
/// for it (the name hung over the stern).
pub(in crate::ui) fn strip_label(label: &str, pending: bool) -> String {
    if pending {
        strip_truncate(&format!("●{label}"))
    } else {
        strip_truncate(label)
    }
}

/// One entry in the bottom session strip's layout: a session badge (its
/// `WsKey`, carried for symmetry with `workspace_slugs` even though today's
/// only consumer just needs the slot), or a `Bow` — the head of one HOST
/// GROUP, which the strip draws as a ship: the full-size brand wheel
/// (mirroring how the Sessions TREE separates hosts with
/// `HOST_DIVIDER_GLYPH`; ADR 0042 L2a, "local is just another host", so the
/// boundary next to `local` gets no special case), and — a row below, so it
/// costs the layout no width at all — that group's host as `host_label`
/// renders it, `BOW_AIR_CELLS` past the wheel. The wheel and the name are ONE
/// item because the owner's design treats them as one: they always co-occur,
/// and the hull's whole extent is measured from this one slot. A bow is never
/// selectable or the active item — `cycle_workspace`
/// (Shift+←/→) keeps walking `workspace_slugs` only, never `StripItem`.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::ui) enum StripItem {
    Session(WsKey),
    Bow { host: HostKey, name: String },
}

/// Lay `slugs` (== `State::workspace_slugs`, already in strip/display order
/// — ADR 0042 L2a union-of-hosts order) out as strip items: a `Bow` (name
/// text from `tag_for`, the caller's `host_label` projection — the one place
/// a host's display name is decided) at the head of EVERY host group, and a
/// `Session` per slug. A lone host is a ship too (owner, 2026-09-27): its
/// group gets a bow like every other, so the strip has no state in which
/// session names float with no hull under them. A bow needs no asset check: with no decoded logo
/// its wheel simply measures 0 (`strip_item_widths`' `wheel_w`) and the NAME
/// still marks the group, matching the logo asset's fail-soft contract. A
/// host with no visible rows never appears in `slugs` to begin with
/// (`fresh_workspace_caches` already filters the inert anchor out), so it
/// can't produce an empty group.
pub(in crate::ui) fn strip_items(slugs: &[WsKey], tag_for: impl Fn(&HostKey) -> String) -> Vec<StripItem> {
    let mut out = Vec::with_capacity(slugs.len());
    for (i, key) in slugs.iter().enumerate() {
        if i == 0 || slugs[i - 1].0 != key.0 {
            out.push(StripItem::Bow {
                host: key.0.clone(),
                name: tag_for(&key.0),
            });
        }
        out.push(StripItem::Session(key.clone()));
    }
    out
}

/// Index span of every ship in `items`: `(bow index, stern index)`, the stern
/// being the group's LAST item — the entry before the next bow, or the last
/// item of all. Pure so the hull's horizontal extent is testable without a
/// draw pass; it replaces chasing `k+1` and `items.len()-1` at the draw site.
pub(in crate::ui) fn ship_spans(items: &[StripItem]) -> Vec<(usize, usize)> {
    let bows: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, it)| matches!(it, StripItem::Bow { .. }))
        .map(|(i, _)| i)
        .collect();
    bows.iter()
        .enumerate()
        .map(|(n, &bow)| {
            let stern = bows
                .get(n + 1)
                .map(|&next| next - 1)
                .unwrap_or(items.len() - 1);
            (bow, stern)
        })
        .collect()
}

/// Left-edge cursor position (pixels, strip-local — before the `win_w/2 +
/// (center - scroll)` screen-centering every strip element applies) of
/// each entry in `widths`, laid out left→right with `gap_before(i)` of space
/// ahead of entry `i` (the first entry sits at 0 — the strip is centred on the
/// active session, so its own left edge is not a coordinate anyone reads). The
/// one cursor-walk primitive `session_strip_target`, `session_strip_lines`, and
/// `strip_divider_offsets` all build on, so inserting an item — a `Bow`, or
/// hypothetically another session — shifts everything after it by exactly that
/// item's own width plus the gap it opens; there's only one place that
/// arithmetic lives. A uniform-gap caller passes `|_| gap`.
pub(in crate::ui) fn strip_cursor_positions(widths: &[f32], gap_before: impl Fn(usize) -> f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(widths.len());
    let mut cursor = 0.0;
    for (i, &w) in widths.iter().enumerate() {
        if i > 0 {
            cursor += gap_before(i);
        }
        out.push(cursor);
        cursor += w;
    }
    out
}

/// Space the cursor walk opens BEFORE one strip item. A `Session` gets one word
/// space (`STRIP_GAP_CELLS`). A `Bow` opens a whole new ship, so it carries the
/// entire fleet boundary: the previous ship's stern clearance
/// (`STRIP_STERN_CLEAR_CELLS` — drawn by `ship_marks`, reserved by no item
/// width), then `STRIP_SHIP_GAP_CELLS` of clear water, then the
/// `HULL_BOW_RUN_CELLS` the rake needs to the LEFT of the wheel this item
/// measures. That sum is what puts five cells of navy between one stern and the
/// next rake's tip, as the locked rasteriser draws it; spending the water alone
/// would leave the rake landing on the stern ahead of it, which is what the
/// uniform gap did.
pub(in crate::ui) fn strip_gap_before(item: &StripItem, cell_w: f32) -> f32 {
    match item {
        StripItem::Session(_) => STRIP_GAP_CELLS * cell_w,
        StripItem::Bow { .. } => {
            (STRIP_STERN_CLEAR_CELLS + STRIP_SHIP_GAP_CELLS + HULL_BOW_RUN_CELLS) * cell_w
        }
    }
}

/// Pixel width for every entry in `items`: a `Session` gets its matching
/// entry from `label_widths` (same order — `items`'s `Session` entries are
/// built, via `strip_items`, from the very slug list `label_widths` is
/// keyed off, so the i-th `Session` in `items` IS `label_widths[i]`); a
/// `Bow` reserves its WHEEL and nothing else.
///
/// The bow reserves no box name (owner, 2026-09-27). Reserving wheel + air +
/// name pushed the first session of a group right by the host's own name length
/// — 8 cells for a 4-letter host, 11.6 for an 8-letter one — so the gap after a
/// bow grew with a fact nobody reads it for. The name runs BENEATH the session
/// names on the hull row, a row of its own, so nothing collides and the
/// sessions start `STRIP_GAP_CELLS` past the wheel whatever the box is called.
pub(in crate::ui) fn strip_item_widths(items: &[StripItem], label_widths: &[f32], wheel_w: f32) -> Vec<f32> {
    let mut li = 0usize;
    items
        .iter()
        .map(|it| match it {
            StripItem::Session(_) => {
                let w = label_widths.get(li).copied().unwrap_or(0.0);
                li += 1;
                w
            }
            StripItem::Bow { .. } => wheel_w,
        })
        .collect()
}

/// Per-SESSION cumulative pixel offset injected by every preceding
/// NON-session item (the `Bow`s): the difference between
/// where a session sits in the FULL layout (sessions interleaved with the
/// rest, `item_widths`) and where it would sit among sessions alone
/// (`label_widths`). Derived from the two cursor walks rather than from any
/// item kind, so a new kind of interleaved item shifts the sessions after
/// it with no change here. Parallel
/// to `label_widths` (one entry per `Session` item, in `items` order), so
/// `session_strip_target`/`session_strip_lines` — which already do their
/// own dividers-oblivious cursor walk over `labels` — can just add
/// `offsets[i]` to session i's position instead of learning about
/// `StripItem` at all. A caller with no dividers (or no divider asset to
/// draw) can pass an empty slice; both callers already treat a short
/// `divider_offsets` as "0.0 past the end".
///
/// The item walk spends the per-item gap (`strip_gap_before` — a ship boundary
/// is wider than a word space), the label walk the uniform `STRIP_GAP_CELLS`
/// the two session-only walks use, and the difference is precisely the offset.
/// So a wider ship boundary needs no change in either of those two callers.
pub(in crate::ui) fn strip_divider_offsets(
    items: &[StripItem],
    item_widths: &[f32],
    label_widths: &[f32],
    cell_w: f32,
) -> Vec<f32> {
    let item_positions = strip_cursor_positions(item_widths, |i| strip_gap_before(&items[i], cell_w));
    let label_positions = strip_cursor_positions(label_widths, |_| STRIP_GAP_CELLS * cell_w);
    let mut out = Vec::with_capacity(label_widths.len());
    let mut li = 0usize;
    for (item, &pos) in items.iter().zip(item_positions.iter()) {
        if matches!(item, StripItem::Session(_)) {
            out.push(pos - label_positions.get(li).copied().unwrap_or(0.0));
            li += 1;
        }
    }
    out
}

#[cfg(test)]
#[path = "items_tests.rs"]
mod tests;
