use super::*;

#[test]
fn an_error_is_its_first_line() {
    assert_eq!(first_line("svg parse failed\ncaused by: a second line", ERROR_BYTES), "svg parse failed");
    assert_eq!(first_line("", ERROR_BYTES), "");
}

#[test]
fn an_error_is_cut_at_its_limit_and_never_inside_a_character() {
    assert_eq!(first_line(&"a".repeat(ERROR_BYTES + 5), ERROR_BYTES).len(), ERROR_BYTES);
    // 1024 is one byte into the 342nd three-byte character, so the cut falls back to the one before it.
    let text = "\u{20ac}".repeat(500);
    let wide = first_line(&text, ERROR_BYTES);
    assert_eq!(wide.len(), 1023);
    assert!(wide.chars().all(|c| c == '\u{20ac}'));
}
