use super::*;

#[test]
fn a_region_write_refuses_what_leaves_the_texture() {
    assert!(region_fits((100, 80), 5 * 3 * 4, 10, 20, 5, 3));
    // a rectangle touching the far edges fits; one pixel past either, an empty one or a short buffer does not
    assert!(region_fits((100, 80), 4 * 4, 96, 79, 4, 1));
    assert!(!region_fits((100, 80), 5 * 4, 96, 79, 5, 1));
    assert!(!region_fits((100, 80), 8 * 4, 99, 78, 1, 3));
    assert!(!region_fits((100, 80), 0, 10, 10, 0, 3));
    assert!(!region_fits((100, 80), 0, 10, 10, 3, 0));
    assert!(!region_fits((100, 80), 5 * 3 * 4 - 1, 10, 20, 5, 3));
    assert!(!region_fits((100, 80), usize::MAX, u32::MAX, 0, 2, 1));
}
