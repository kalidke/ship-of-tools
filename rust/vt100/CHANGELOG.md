# Provenance

This is a vendored fork of **vt100-ctt 0.17.1**
(`ChrisTitusTech/vt100-rust`, itself a fork of `doy/vt100-rust`), MIT.
Upstream's own changelog lives with those projects; it is not reproduced
here, because most of what it documents is API this fork has removed and a
changelog describing deleted functions is worse than no changelog.

## What this fork changes

**Added — the reason the fork exists.** `Screen::checkpoint` and
`Parser::restore_screen`: an exact, versioned, binary hand-off of terminal
state covering both grids, alternate-screen identity, the current and saved
cursor and attributes, the scroll region, origin mode, per-row wrap flags,
and the input modes. See `src/checkpoint.rs` for the format and
`docs/adr/0041-fe-local-capsules-windows.md` for why it exists: a capsule
outlives the frontend that renders it, and the frontend that attaches must
be handed the state rather than a reconstruction of it.

**Removed — what the addition replaces.** The escape-sequence *writing*
stack: `Screen::contents_formatted`, `contents_diff`, `rows_formatted`,
`rows_diff`, `state_formatted`, `state_diff`, `input_mode_formatted`,
`input_mode_diff`, `attributes_formatted`, `cursor_state_formatted`, the
`term` module implementing them, and the grid/row/attribute writers behind
it. ADR 0041 rejects that approach outright: replayed escape sequences
cannot express the inactive grid or alternate-screen identity. Also removed:
the `tui-term` feature and its ratatui glue, which the only consumer had
already switched off.

**Changed — three parser behaviors, all to stop a panic.**

`Row::resize` now clears a wide character's leading cell when shrinking cuts
off its continuation, which `Row::truncate` already did. Without it, shrinking
a terminal through a wide glyph left a lead with no continuation, and drawing
over that cell reached for the missing half and panicked. (`Row::truncate`
also no longer indexes past the end when resized to zero.)

A terminal now has a MINIMUM size of 2x2 (`grid::MIN_ROWS`). Below it, two
inherited panics were reachable from ordinary traffic: a width-two glyph in a
one-column terminal underflowed `size.cols - width` and, once that was fixed,
still had nowhere to put its continuation half; and a wrap in a one-row
terminal underflowed `prev_pos.row -= scrolled` at row 0. Neither geometry has
a rendering worth inventing, so it is refused instead — constructors and
`set_size` raise a smaller request, and `Parser::restore_screen` rejects a
checkpoint announcing one rather than clamping it into a screen the payload
does not describe. Reasoning at the constant; both former panics are pinned in
`tests/geometry.rs`.

A glyph's width is CLAMPED to two cells (`grid::MAX_GLYPH_WIDTH`). Upstream's
`Screen::text` carried the comment "width() can only return 0, 1, or 2", which
was true of the `unicode-width` table it was written against; 0.2 reports three
for a handful of characters. This crate cannot represent a three-cell glyph —
a wide character is a lead plus exactly one continuation — and `cols - width`
underflowed for any terminal narrower than the glyph, so an ordinary
two-character input panicked on a two-column screen. Such a character now draws
as an ordinary wide glyph. `MIN_COLS >= MAX_GLYPH_WIDTH` is asserted at compile
time, because that is the relationship the column arithmetic depends on and
raising one without the other would restore the underflow in silence.

Nothing else about parsing differs from the release this vendors — and that
sentence is now checked rather than asserted; see below.

**Added, ADR 0041 step 5 — `Parser::is_ground`.** `src/vte/` vendors vte
0.13.1's core (minus the `ansi` feature module) and utf8parse 0.2.2 in full,
both MIT/Apache-2.0 with their license files copied in alongside. No
released `vte`, checked through 0.15.0, exposes the escape-sequence state
machine's state — it is a private field — and the capsule can only cut an
attach stream at a boundary where that state is `Ground` and no partial
UTF-8 codepoint is buffered (see `src/checkpoint.rs`). Vendoring is what
makes the accessor possible: `Parser::is_ground` reads both the vendored
vte state and the vendored utf8parse state directly, rather than inferring
ground-ness from which `Perform` callback last fired — unreliable, since DEL
is `Action::Print` from `Ground` and `Action::Ignore` elsewhere, so the
shape of the callback traffic alone does not say which state produced it
(see `tests/ground.rs::del_in_ground_is_still_ground`). One divergence from
straight vendoring: `src/vte/table.rs`'s state-transition table is normally
generated at compile time by the `vte_generate_state_changes` proc macro;
vendoring that macro too would add a dependency for a single generated
constant, so its expansion is precomputed once and pinned as a literal
instead, with the transition list that produced it kept alongside as a
comment so the literal stays auditable against it. `tests/ground.rs` pins
the ground/not-ground boundary across escape, CSI, OSC, DCS, UTF-8, and
cancel-byte (CAN/SUB) cases. A second divergence: upstream caps the OSC
accumulation buffer at 1024 bytes only on its `no_std`/arrayvec arm — which
is the arm vte's DEFAULT features select, so it was the behavior actually
shipping before the vendoring. The vendored copy keeps `Vec` storage (no
arrayvec dependency) but keeps the cap unconditionally: the parser sits in
front of an untrusted producer, and an unterminated OSC must not grow
memory without bound (`tests/osc_cap.rs`). A third divergence: upstream's
`pub use params::{Params, ParamsIter};` also re-exports `ParamsIter`, which
this crate never names — pruned rather than kept as a dead re-export,
since nothing outside the crate can reach it through the `pub(crate) mod
vte` boundary anyway.

## What this fork's tests are

`src/` was vendored alone at first, so the 54 checkpoint tests were the whole
suite and every parser behavior the fork inherited was untested here. Upstream's
suite is now vendored too, because ADR 0041 steps 4-5 change parser behavior and
a change wants something to regress against.

**Vendored close to verbatim:** the fixture corpus — 34 recordings under
`tests/data/fixtures/`, each a stream of input chunks with the exact screen it
must produce — and the tests that drive it (`attr`, `control`, `csi`, `escape`,
`mode`, `osc`, `processing`, `scroll`, `text`, `weird`), plus `basic`, `init`,
`write`, and `split-escapes`. This is what turns "nothing else about parsing
differs" from a claim into a check.

**Vendored with the oracle swapped.** Upstream proved a screen's state was
*complete* by re-emitting it as escape sequences and re-parsing them. That
stack is gone, so the oracle is the checkpoint round trip — the same shape of
proof over a strictly larger surface, since it also carries the grid that is
not on screen. `tests/helpers/mod.rs` holds it and explains the one property
that could not come along: restoring mid-stream and continuing. The corpus
splits its input *inside* escape sequences and UTF-8 characters on purpose, and
a checkpoint cannot carry a half-read sequence — the state machine lives in
`vte`, private. That is precisely why the capsule cuts only at ground-state
boundaries, and the property is tested where the cut can be placed on purpose
(`checkpoint::a_restored_parser_continues_the_stream_identically`).

**Not vendored, deliberately:** the tests of the removed writing stack
(`window_contents`'s `formatted`/`diff_*`, `attr::attributes_formatted`) and
their 31 MB crawl corpus; `linutil_integration.rs`, which tests the removed
ratatui glue; the `regen-fixtures` tool, because a one-command way to rewrite
the evidence from the behavior under test is the wrong thing to keep within
reach; and `quickcheck`, whose property died with the stack it tested — its
input generator survives in `tests/roundtrip_fuzz.rs` on a fixed-seed PRNG, so
a CI failure reproduces exactly.
