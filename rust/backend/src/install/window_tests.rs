use super::*;

// The shared check runs on Windows too; this wire fixture needs a Unix socket (Linux or macOS).
#[cfg(unix)]
#[test]
fn a_repeat_install_judges_the_connected_local_window_not_the_record() {
    use sot_protocol::{codec, op, Frame};
    use std::io::BufReader;
    use std::os::unix::net::UnixListener;
    use std::time::{Duration, Instant};

    let client = |role: &str, host: Option<&str>, version: &str| serde_json::json!({
        "client_id": "fixture", "role": role, "host": host,
        "app_version": version, "protocol": sot_protocol::PROTOCOL_VERSION,
    });
    let local_old = client("fe", Some("desk"), "0.7.0-rc.1");
    for (clients, answer, pending) in [
        (vec![local_old.clone()], "roster", true),
        (vec![local_old.clone(), local_old.clone()], "roster", true),
        (vec![client("fe", Some("desk"), "0.7.0-rc.2")], "roster", false),
        (vec![client("fe", Some("elsewhere"), "0.7.0-rc.1")], "roster", false),
        (vec![client("cli", Some("desk"), "0.7.0-rc.1")], "roster", false),
        (vec![client("fe", None, "0.7.0-rc.1")], "roster", false),
        (vec![local_old.clone()], "unknown", false),
        (vec![local_old.clone()], "closed", false),
        (vec![local_old], "silent", false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "installer never queried its own daemon");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            };
            // macOS hands an accepted socket the listener's nonblocking mode.
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let hello = codec::read_frame_blocking(&mut reader).unwrap();
            assert_eq!(hello.op, op::HELLO);
            codec::write_frame_blocking(&mut stream, &Frame::res(hello.id, op::HELLO, serde_json::json!({}))).unwrap();
            let query = codec::read_frame_blocking(&mut reader).unwrap();
            assert_eq!(query.op, op::VERSION_QUERY);
            if answer == "silent" {
                let _ = wait.recv_timeout(Duration::from_secs(5));
            } else if answer != "closed" {
                let payload = if answer == "unknown" { serde_json::json!({"error": "unknown op"}) } else { serde_json::json!({
                    "daemon": {"app_version": "0.7.0-rc.2", "protocol": sot_protocol::PROTOCOL_VERSION},
                    "clients": clients,
                }) };
                codec::write_frame_blocking(&mut stream, &Frame::res(query.id, op::VERSION_QUERY, payload)).unwrap();
            }
        });
        let mut found = super::super::verdict::Found { pass: "repeat install".into(), ..Default::default() };
        let began = Instant::now();
        let step = restart_step(&format!("unix:{}", path.display()), "desk", "0.7.0-rc.2");
        assert!(began.elapsed() < Duration::from_secs(5), "the optional query exceeded its bound");
        let _ = release.send(());
        if let Some(step) = step { found.step(step); }
        server.join().unwrap();
        let (line, code) = super::super::verdict::line(&found, "");
        assert_eq!(code, 0, "{line}");
        assert_eq!(found.steps.len(), usize::from(pending), "{line}");
        if pending {
            assert!(line.starts_with("ONE STEP LEFT:") && line.contains("0.7.0-rc.1") && line.contains("0.7.0-rc.2"), "{line}");
        } else {
            assert!(line.starts_with("PASS:"), "{line}");
        }
    }
    let absent = tempfile::tempdir().unwrap();
    assert!(restart_step(&format!("unix:{}", absent.path().join("absent.sock").display()), "desk", "0.7.0-rc.2").is_none());
}
