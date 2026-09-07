//! Shared script-block bytes + the flood pattern generator, `#[path]`-
//! included by BOTH platform helper binaries (`sot-conpty-helper.rs`,
//! `sot-pty-helper.rs`, ADR 0043 "Decisions for LU2" LU2b) so the exact
//! bytes each `--script`/`--flood` producer emits are ONE constant, not
//! two independently-drifting copies. Neither binary is a library target
//! another crate could `use` (a `src/bin/*.rs` file has none), so
//! `#[path = "support/helper_common.rs"] mod helper_common;` is the
//! ordinary way two sibling binaries in one package share source without
//! a real dependency edge — unlike `capsule.rs`/`capsule_legacy.rs`
//! (which deliberately shared NOTHING, see that module's own doc),
//! byte-for-byte identity is the whole point here, so sharing is
//! required, not merely convenient. Lives in a `support/` subdirectory,
//! not directly under `src/bin/`, for the same reason
//! `tests/support/transports.rs` does: Cargo auto-discovers every loose
//! `.rs` file directly under `src/bin/` as its OWN binary target (needing
//! its own `fn main`), but never a file inside a subdirectory that isn't
//! itself named `main.rs`.
#![allow(dead_code)] // either platform's own helper may not use every item here

/// The fixed byte sequence `--script` emits, repeated: plain text; a CSI
/// SGR pair (color on, "red", color off); an OSC title set, BEL-
/// terminated; a 3-byte UTF-8 codepoint (★ U+2605) immediately followed by
/// a 4-byte one (😀 U+1F600); a DCS payload, ST-terminated; a trailing
/// newline. Byte-identical on both platforms — the attach-fidelity
/// property this drives is platform-neutral, and a diverging fixture
/// between the two helpers would silently prove two different things.
pub const SCRIPT_BLOCK: &[u8] =
    b"plain text\x1b[31mred\x1b[0m\x1b]0;title\x07\xe2\x98\x85\xf0\x9f\x98\x80\x1bPdcs-payload\x1b\\done\n";

/// One `chunk`-sized block of the cheap, position-independent `0..=9`
/// repeating pattern `--flood` writes — a decoder verifies total byte
/// COUNT, never content, so the exact digits carry no meaning of their
/// own (see each binary's own `--flood` doc).
pub fn flood_pattern(chunk: usize) -> Vec<u8> {
    (0..chunk).map(|i| b'0' + (i % 10) as u8).collect()
}
