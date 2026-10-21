use super::*;

#[test]
fn a_pending_refusal_is_told_apart_from_every_other_error() {
    assert!(is_pending(&pending()));
    assert!(!is_pending(&io::Error::new(
        io::ErrorKind::WouldBlock,
        "something else"
    )));
    assert!(!is_pending(&io::Error::from(io::ErrorKind::NotFound)));
    assert!(is_pending_text(&pending().to_string()));
    assert!(!is_pending_text("capsule supervisor spawn failed: nope"));
}
