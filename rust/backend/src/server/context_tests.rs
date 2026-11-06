use crate::server::conn::{handle_connection, Abandon};
use super::*;
use serde_json::json;

/// A test duplex's write half owes its peer nothing either; there is no linger thread to drop.
impl Abandon for tokio::io::WriteHalf<tokio::io::DuplexStream> {
    fn abandon(&self) {}
}

/// Serves one real accepted connection with `services`, says hello as `client_id`, asks `version.query`, closes, and
/// returns the hello reply's label and the client ids the roster listed.
async fn hello_and_roster(services: Services, client_id: &str) -> (Option<String>, Vec<String>) {
    let (client, server) = tokio::io::duplex(1 << 16);
    let (srx, stx) = tokio::io::split(server);
    let task = tokio::spawn(handle_connection(srx, stx, services, None));
    let (crx, mut ctx) = tokio::io::split(client);
    let mut crx = codec::buffered(crx);

    let hello = json!({
        "client_id": client_id, "protocol": sot_protocol::PROTOCOL_VERSION, "app_version": sot_protocol::app_version(),
        "host": "test-host", "os_user": "test-user", "role": "cli",
    });
    codec::write_frame(&mut ctx, &Frame::req(1, op::HELLO, hello), None).await.unwrap();
    let (reply, _) = codec::read_frame(&mut crx).await.unwrap();
    assert_eq!(reply.op, op::HELLO, "{reply:?}");
    let label = reply.payload["label"].as_str().map(str::to_string);

    codec::write_frame(&mut ctx, &Frame::req(2, op::VERSION_QUERY, json!({})), None).await.unwrap();
    let roster = loop {
        let (frame, _) = codec::read_frame(&mut crx).await.unwrap();
        if frame.kind == Kind::Res && frame.op == op::VERSION_QUERY {
            break frame.payload["clients"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["client_id"].as_str().unwrap().to_string())
                .collect::<Vec<_>>();
        }
    };
    drop((crx, ctx));
    task.await.unwrap().unwrap();
    (label, roster)
}

#[tokio::test(flavor = "multi_thread")]
async fn each_accepted_connection_is_served_the_services_it_was_given() {
    let (root_a, root_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, b) = (Services::for_test("alpha", root_a.path()), Services::for_test("beta", root_b.path()));
    // A client already on each roster, which only that roster may list.
    let _on_a = a.clients.register("on-a", "v", sot_protocol::PROTOCOL_VERSION, "cli".into(), None, None, None);
    let _on_b = b.clients.register("on-b", "v", sot_protocol::PROTOCOL_VERSION, "cli".into(), None, None, None);

    let ((label_a, roster_a), (label_b, roster_b)) =
        tokio::join!(hello_and_roster(a, "conn-a"), hello_and_roster(b, "conn-b"));

    assert_eq!(label_a.as_deref(), Some("alpha"));
    assert_eq!(label_b.as_deref(), Some("beta"));
    for (roster, own, other) in [(&roster_a, ["on-a", "conn-a"], ["on-b", "conn-b"]), (&roster_b, ["on-b", "conn-b"], ["on-a", "conn-a"])] {
        for id in own {
            assert!(roster.iter().any(|c| c == id), "{id} missing from {roster:?}");
        }
        for id in other {
            assert!(!roster.iter().any(|c| c == id), "{id} leaked into {roster:?}");
        }
    }
}
