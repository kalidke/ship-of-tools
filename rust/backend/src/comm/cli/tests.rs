use super::*;

#[test]
fn an_unknown_verb_is_a_usage_error() {
    assert_eq!(run(&["nope".to_string()]), 2);
    assert_eq!(run(&[]), 2);
}
