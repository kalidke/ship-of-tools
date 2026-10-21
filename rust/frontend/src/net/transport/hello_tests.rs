#[test]
fn protocol_mismatch_message_names_both_sides_of_the_skew() {
    // A NEW frontend against an OLD daemon: the daemon's gate answers
    // with its own version; this build adds its own. Both must appear.
    let payload = serde_json::json!({
        "code": "protocol_mismatch",
        "backend_protocol": sot_protocol::PROTOCOL_VERSION - 1,
        "backend_version": "0.5.9",
    });
    let msg = super::protocol_mismatch_message(&payload, "protocol mismatch");
    assert!(msg.contains(&format!("backend:  0.5.9  (protocol {})", sot_protocol::PROTOCOL_VERSION - 1)), "{msg}");
    assert!(
        msg.contains(&format!("frontend: {}  (protocol {})", sot_protocol::app_version(), sot_protocol::PROTOCOL_VERSION)),
        "{msg}"
    );
}

#[test]
fn protocol_mismatch_headline_names_the_side_that_is_behind() {
    let headline = |theirs: u32, version: &str| {
        let payload = serde_json::json!({
            "code": "protocol_mismatch",
            "backend_protocol": theirs,
            "backend_version": version,
        });
        let msg = super::protocol_mismatch_message(&payload, "protocol mismatch");
        msg.lines().next().unwrap().to_string()
    };
    let ahead = sot_protocol::PROTOCOL_VERSION + 1;
    let first = headline(ahead, "9.9.9");
    assert!(first.starts_with("frontend out of date"), "{first}");
    assert!(first.contains(&format!("protocol {ahead}")), "{first}");
    let first = headline(sot_protocol::PROTOCOL_VERSION - 1, "0.5.9");
    assert!(first.starts_with("daemon out of date"), "{first}");
}
