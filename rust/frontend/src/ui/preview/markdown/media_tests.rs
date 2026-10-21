use super::*;

#[test]
fn whole_row_bottom_stops_at_the_last_whole_row() {
    assert_eq!(whole_row_bottom(100.0, 20.0, 175.0), 160.0);
    assert_eq!(whole_row_bottom(100.0, 20.0, 180.0), 180.0);
    assert_eq!(whole_row_bottom(100.0, 20.0, 179.6), 180.0);
    assert_eq!(whole_row_bottom(100.0, 20.0, 115.0), 100.0);
    assert_eq!(whole_row_bottom(40.0, 20.0, 175.0), 160.0);
    assert_eq!(whole_row_bottom(100.0, 0.0, 175.0), 175.0);
}
