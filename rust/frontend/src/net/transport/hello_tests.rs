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

/// The window's hello says it reads terminal escapes (the REPL drawer's colors); the daemon sends text with its escape
/// sequences to a window that says so and without them to every other.
#[tokio::test]
async fn the_windows_hello_says_it_reads_terminal_color() {
    let _env = crate::net::state::test_env::set_test_env();
    let session = super::SessionState {
        host: "h".to_string(),
        memory: crate::net::state::SessionMemory::fresh(),
        gate: super::StateSaveGate::new(),
        takes_drawing: false,
        draws_pages: false,
        posts_plain_text: false,
        serves_page_words: false,
    };
    let mut wire = Vec::new();
    super::send_hello(&mut wire, 1, &session, None).await.unwrap();
    let frame = sot_protocol::codec::read_frame_blocking(&mut std::io::BufReader::new(&wire[..])).unwrap();
    assert_eq!(frame.payload["reads_terminal_color"], true, "{}", frame.payload);
}
