use super::*;

#[test]
fn a_region_write_covers_exactly_its_rectangle() {
    let copy = region_copy((100, 80), 5 * 3 * 4, 10, 20, 5, 3).unwrap();
    assert_eq!(copy, RegionCopy { origin: (10, 20), extent: (5, 3), bytes_per_row: 20 });
    // a rectangle touching the far edges fits; one pixel past either, an empty one or a short buffer does not
    assert!(region_copy((100, 80), 4 * 4, 96, 79, 4, 1).is_some());
    assert!(region_copy((100, 80), 5 * 4, 96, 79, 5, 1).is_none());
    assert!(region_copy((100, 80), 8 * 4, 99, 78, 1, 3).is_none());
    assert!(region_copy((100, 80), 0, 10, 10, 0, 3).is_none());
    assert!(region_copy((100, 80), 0, 10, 10, 3, 0).is_none());
    assert!(region_copy((100, 80), 5 * 3 * 4 - 1, 10, 20, 5, 3).is_none());
    assert!(region_copy((100, 80), usize::MAX, u32::MAX, 0, 2, 1).is_none());
}
