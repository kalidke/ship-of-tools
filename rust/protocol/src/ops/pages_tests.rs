use super::*;

#[test]
fn docs_open_grows_by_a_defaulted_scope_and_old_payloads_grant_none() {
    let old = serde_json::json!({"path": "/site/index.html"});
    let request: DocsOpenReq = serde_json::from_value(old).unwrap();
    assert_eq!(serde_json::to_value(request).unwrap()["direct"], serde_json::json!({"scope": "none"}));
    for scope in [serde_json::json!({"scope": "none"}),
        serde_json::json!({"scope": "row", "workspace_id": "row-id"}), serde_json::json!({"scope": "all"})] {
        let request: DocsOpenReq = serde_json::from_value(serde_json::json!({"path": "/site/index.html", "direct": scope})).unwrap();
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["direct"], scope);
        #[derive(serde::Deserialize)]
        struct OldDocsOpen { path: String }
        let old_reader: OldDocsOpen = serde_json::from_value(encoded).unwrap();
        assert_eq!(old_reader.path, "/site/index.html");
    }
}
