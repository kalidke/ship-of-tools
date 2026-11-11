use super::*;
use crate::host::bound::tests::{child_role, run_child, CODE};
use crate::test_isolated::enter;

/// The test binary's allocator, as a real binary's static is; disarmed except in a child that arms it.
#[global_allocator]
static HEAP: HeapCap = HeapCap::new();

const MIB: usize = 1 << 20;

/// A request that stays unread, so the optimizer cannot drop it.
fn request(bytes: usize) -> Vec<u8> {
    std::hint::black_box(Vec::with_capacity(bytes))
}

#[test]
fn an_armed_heap_cap_ends_the_process_with_its_code() {
    const NAME: &str = "host::heap_cap::tests::an_armed_heap_cap_ends_the_process_with_its_code";
    if let Some(role) = child_role() {
        enter(NAME);
        HEAP.arm(64 * MIB, CODE);
        drop(request(if role == "over" { 128 * MIB } else { 32 * MIB }));
        std::process::exit(0);
    }
    assert_eq!(run_child(NAME, "over").0.code(), Some(CODE));
    // The control: a request under the cap is served.
    assert_eq!(run_child(NAME, "under").0.code(), Some(0));
}

#[test]
fn a_heap_cap_counts_realloc_and_zeroed() {
    const NAME: &str = "host::heap_cap::tests::a_heap_cap_counts_realloc_and_zeroed";
    if let Some(role) = child_role() {
        enter(NAME);
        let mut grown = request(MIB);
        HEAP.arm(64 * MIB, CODE);
        if role == "realloc" {
            grown.reserve_exact(128 * MIB);
        } else {
            std::hint::black_box(vec![0u8; 128 * MIB]);
        }
        std::process::exit(0);
    }
    for role in ["realloc", "zeroed"] {
        assert_eq!((role, run_child(NAME, role).0.code()), (role, Some(CODE)));
    }
}

#[test]
fn a_heap_cap_counts_what_is_held_and_reports_its_high_water() {
    const NAME: &str = "host::heap_cap::tests::a_heap_cap_counts_what_is_held_and_reports_its_high_water";
    if let Some(role) = child_role() {
        enter(NAME);
        let before_arming = request(48 * MIB);
        if role == "freed" {
            HEAP.arm(64 * MIB, CODE);
            drop(before_arming);
            // 100 blocks of 32 MiB, one after another, stay under the cap only if each free is counted.
            for _ in 0..100 {
                drop(request(32 * MIB));
            }
        } else {
            drop(before_arming);
            HEAP.arm(1024 * MIB, CODE);
            let (a, b) = (request(16 * MIB), request(16 * MIB));
            let high = HEAP.high_water();
            drop((a, b));
            assert!((32 * MIB..34 * MIB).contains(&high), "high water {high}");
        }
        std::process::exit(0);
    }
    for role in ["freed", "peak"] {
        assert_eq!((role, run_child(NAME, role).0.code()), (role, Some(0)));
    }
}

#[test]
fn one_request_larger_than_the_cap_cannot_switch_the_cap_off() {
    const NAME: &str = "host::heap_cap::tests::one_request_larger_than_the_cap_cannot_switch_the_cap_off";
    if child_role().is_some() {
        enter(NAME);
        HEAP.arm(64 * MIB, CODE);
        let held = request(MIB);
        // A fallible caller's huge request: added to the count it would wrap it, and the cap would be gone.
        let _ = Vec::<u8>::new().try_reserve_exact(isize::MAX as usize - 4096);
        drop(request(128 * MIB));
        drop(held);
        std::process::exit(0);
    }
    assert_eq!(run_child(NAME, "huge").0.code(), Some(CODE));
}

#[test]
fn a_heap_cap_counts_growth_not_size_credits_a_shrink_and_keeps_the_peak() {
    const NAME: &str = "host::heap_cap::tests::a_heap_cap_counts_growth_not_size_credits_a_shrink_and_keeps_the_peak";
    if child_role().is_some() {
        enter(NAME);
        HEAP.arm(64 * MIB, CODE);
        let mut grown = request(40 * MIB);
        grown.reserve_exact(48 * MIB); // 40 -> 48: counted by its 8 MiB growth, not its 48 MiB size (88 would end it)
        grown.shrink_to(8 * MIB); // credited: 8 held
        let other = request(40 * MIB); // 48 held; with the shrink forgotten, 88 would end it
        drop((grown, other));
        let high = HEAP.high_water();
        assert!((48 * MIB..49 * MIB).contains(&high), "high water {high}");
        std::process::exit(0);
    }
    assert_eq!(run_child(NAME, "sizes").0.code(), Some(0));
}
