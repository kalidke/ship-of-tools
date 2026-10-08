//! The relay's log for `fe.command.send`: the forwarded result's workspace and path, beside the target and the audience.

use super::handle_fe_command_send;
use crate::clients::Clients;
use sot_protocol::FeCommandEvt;
use tokio::sync::broadcast;

/// The relay's log names the forwarded workspace and path beside the target and the audience, and says nothing
/// invented when a command carries neither; every line still passes the standard redactor.
#[tokio::test]
async fn relay_log_names_the_forwarded_result() {
    let log = sot_log::test_log::capture();
    let clients = Clients::new();
    let _fe = clients.register(
        "c-fe",
        "0.6.0",
        1,
        "fe".to_string(),
        None,
        None,
        Some("fe@host-a".into()),
    );
    let (tx, _rx) = broadcast::channel::<FeCommandEvt>(8);
    let secret = "0123456789abcdef0123456789abcdef";
    let send = |cmd: &str, args: serde_json::Value, target: Option<&str>| {
        let req = serde_json::json!({ "cmd": cmd, "args": args, "target": target });
        handle_fe_command_send(1, req, &tx, &clients)
    };
    let args = serde_json::json!({ "ws": format!("w\"1 {secret}"), "path": "/data/run 1/a.png", "text": "kept out" });
    send("preview", args, Some("fe@host-a")).await.unwrap();
    send(
        "notify",
        serde_json::json!({ "text": "hi" }),
        Some("fe@host-a"),
    )
    .await
    .unwrap();
    send(
        "relaunch",
        serde_json::json!({ "path": "/x/relaunch" }),
        None,
    )
    .await
    .unwrap();
    let text = log.text();
    let line = |needle: &str| {
        text.lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no log line {needle}: {text}"))
    };
    let preview = line("cmd=preview");
    for want in [
        r#"workspace="w\"1 "#,
        r#"path="/data/run 1/a.png""#,
        r#"target=Some("fe@host-a")"#,
        "delivered_to=1",
    ] {
        assert!(preview.contains(want), "{want} missing: {preview}");
    }
    assert!(
        !preview.contains("kept out"),
        "arbitrary args stay out of the log: {preview}"
    );
    let notify = line("cmd=notify");
    assert!(
        !notify.contains("workspace=") && !notify.contains("path="),
        "{notify}"
    );
    let relaunch = line("cmd=relaunch");
    assert!(
        relaunch.contains(r#"path="/x/relaunch""#) && relaunch.contains("delivered_to=0"),
        "{relaunch}"
    );
    let masked = sot_log::secret::redact(text.as_bytes());
    assert!(
        !String::from_utf8_lossy(&masked).contains(secret),
        "the redactor masks the sentinel"
    );
}
