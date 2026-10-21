// unique.rs — how `sot-updater` and its consumers make a name unique.
//
// NOT "the one place in the workspace", and the scope is worth stating exactly
// rather than claiming more than is delivered:
//
//   - `sot_log::host`'s `preflight_nonce` is an independent `<pid>-<seq>`
//     generator with its own counter and the same reasoning. It CANNOT share
//     this one: `sot-log` is the LOWER crate and must not depend on
//     `sot-updater`. Two, deliberately.
//   - Everything whose name was the clock ALONE, or the clock plus a constant
//     label, now draws from here: `lock`'s owner nonce and its graveyard path,
//     the frontend download fallback and its test helper,
//     `backend::topology_store`'s argument-less test `tempdir` (six callers on
//     parallel threads — `fetch::tempdir`'s defect verbatim) and
//     `backend::site_serve`'s test base (the clock with no pid at all).
//   - Still outside: roughly fifteen helpers that put a PER-CALL TAG in the name
//     alongside a clock reading. A collision there needs two callers with the
//     same tag inside one tick, so the tag is doing real work and they are a
//     different risk class — but the same false premise, and they are queued
//     rather than claimed as done.
//
// A clock reading is not a uniqueness source. Treating one as such was a defect
// with five instances in this tree, and the one that bit shows why the class is
// worth a module of its own rather than five local fixes: `fetch::tempdir`
// named its scratch dir `<label>-<pid>-<nanos>` while `fetch::latest` passes a
// CONSTANT label, so for two concurrent callers in one process the clock was
// the only separator. On a platform whose clock ticks coarser than a nanosecond
// two callers drew the SAME directory, and the first to finish deleted the
// other's `SHA256SUMS` between its download and its read. It reached CI as a
// macOS-only red because Linux's nanosecond clock almost never collides — which
// is exactly what makes this class dangerous: it hides on the platform the code
// is written on, and `latest` runs concurrently in production too (the daemon's
// periodic check and an `update.check` op can overlap). That scratch folder no
// longer draws a name from here: it is a held folder (`sot_log::host::HeldDir`),
// whose random name is taken by an exclusive create.
//
// This lives in `sot-updater` because every consumer of it already depends on
// this crate, so the helper needed no new edge in either direction.

use std::sync::atomic::{AtomicU64, Ordering};

/// The in-process half of every unique name here, and the only part that is
/// actually a guarantee. `Relaxed` is right: nothing orders memory against
/// this, the single requirement is that no two draws return the same value.
static SEQ: AtomicU64 = AtomicU64::new(0);

fn next() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

fn clock_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// `<pid>-<nanos>-<seq>`: unique BY CONSTRUCTION within this process, and clear
/// of another process's names and of a stale name left by an earlier process
/// that happened to reuse the pid.
///
/// The counter is the guarantee. `pid` and the timestamp are diagnostics and
/// are kept deliberately: a leftover scratch directory is something a person
/// has to identify later, and "whose, and when" is the whole of that. Do not
/// drop the counter and lean on the timestamp again — that is the defect this
/// module exists to delete, and `suffix_is_unique_without_the_clocks_help`
/// fails if anyone tries.
pub fn suffix() -> String {
    format!("{}-{}-{}", std::process::id(), clock_nanos(), next())
}

/// The same uniqueness as [`suffix`], as one number, for a caller that writes
/// it into a file rather than showing it to a person — `lock.rs`'s owner line
/// `"<pid>@<host>#<nonce>"`, and the graveyard directory a lock breaker renames
/// a stale lock into.
///
/// Disjoint fields rather than a mix, so no field can mask another: the clock
/// in the low 64 bits, `pid` in the next 32, the counter in the top 32. Past
/// 2^32 draws the counter field wraps *within the nonce* and the clock is again
/// all that separates two draws — a degradation to the old behaviour, not a
/// wrong answer.
///
/// That budget is 2^32 draws of `SEQ` in TOTAL, shared with [`suffix`], not 2^32
/// lock acquisitions: every scratch dir spends from the same counter. Still not
/// a case worth buying anything for, but the distinction is the doc's to make
/// rather than the reader's to discover.
///
/// Nothing parses the fields back out: `lock.rs` compares the owner line whole.
/// The layout is for a person reading a stale lock file.
pub fn nonce() -> u128 {
    const LOW64: u128 = u64::MAX as u128;
    const LOW32: u128 = u32::MAX as u128;
    ((next() as u128 & LOW32) << 96)
        | ((std::process::id() as u128 & LOW32) << 64)
        | (clock_nanos() & LOW64)
}

#[cfg(test)]
#[path = "unique_tests.rs"]
mod tests;
