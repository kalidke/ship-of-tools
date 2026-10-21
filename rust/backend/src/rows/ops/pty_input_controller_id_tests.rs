use super::*;

#[test]
fn absent_origin_falls_back_to_the_connection_client_id() {
    let id = resolve_pty_input_controller_id(1, op::PTY_INPUT, None, "conn-client").unwrap();
    assert_eq!(id, "conn-client");
}

#[test]
fn present_origin_wins_over_the_connection_client_id() {
    let id = resolve_pty_input_controller_id(1, op::PTY_INPUT, Some("host-4-dev"), "conn-client").unwrap();
    assert_eq!(id, "host-4-dev");
}

#[test]
fn origin_at_exactly_the_bound_is_accepted() {
    let origin = "x".repeat(MAX_PTY_INPUT_ORIGIN_LEN);
    let id = resolve_pty_input_controller_id(1, op::PTY_INPUT, Some(&origin), "conn-client").unwrap();
    assert_eq!(id, origin);
}

#[test]
fn origin_one_over_the_bound_is_bad_origin() {
    let origin = "x".repeat(MAX_PTY_INPUT_ORIGIN_LEN + 1);
    let frame = resolve_pty_input_controller_id(1, op::PTY_INPUT, Some(&origin), "conn-client")
        .expect_err("an over-length origin must be refused");
    assert_eq!(frame.payload["code"], "bad_origin");
}
