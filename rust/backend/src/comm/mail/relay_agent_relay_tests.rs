use super::*;

#[tokio::test]
async fn an_empty_target_notification_is_published_and_acknowledged() {
    let (tx, mut rx) = tokio::sync::broadcast::channel(4);
    let out = handle_agent_send(7, json!({"from": "a", "to": "", "text": "{\"sot_ui\":{}}"}), &tx)
        .await
        .expect("handler must not error");
    let published = rx.try_recv().expect("one published notification");
    assert_eq!((published.from.as_str(), published.to.as_str()), ("a", ""));
    assert_eq!(published.text, "{\"sot_ui\":{}}");
    assert_eq!(out[0].0.payload, json!({"ok": true}));
}

#[tokio::test]
async fn directed_mail_is_refused_and_publishes_nothing() {
    let (tx, mut rx) = tokio::sync::broadcast::channel(4);
    let out = handle_agent_send(8, json!({"from": "a", "to": "peer-otherbox", "text": "hi", "id": "x-1"}), &tx)
        .await
        .expect("handler must not error");
    assert_eq!(out[0].0.payload, json!({"error": "mail must use comm.file", "code": "mail_requires_comm_file"}));
    assert!(rx.try_recv().is_err(), "directed mail was published");
}
