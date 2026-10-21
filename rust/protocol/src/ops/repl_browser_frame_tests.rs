use super::*;

#[test]
fn a_browser_frame_names_its_one_frontend_only_when_aimed() {
    let v = serde_json::json!({"kind": "browser", "url": "http://127.0.0.1:1/x"});
    let f: ReplFrame = serde_json::from_value(v).unwrap();
    match &f {
        ReplFrame::Browser { open, fe, .. } => {
            assert!(*open);
            assert_eq!(*fe, None);
        }
        other => panic!("not a browser frame: {other:?}"),
    }
    let back = serde_json::to_value(&f).unwrap();
    assert!(back.get("fe").is_none(), "{back}");

    let v = serde_json::json!({"kind": "browser", "url": "http://127.0.0.1:1/x", "open": false, "fe": "fe@a"});
    match serde_json::from_value::<ReplFrame>(v).unwrap() {
        ReplFrame::Browser { open, fe, .. } => {
            assert!(!open);
            assert_eq!(fe.as_deref(), Some("fe@a"));
        }
        other => panic!("not a browser frame: {other:?}"),
    }
}
