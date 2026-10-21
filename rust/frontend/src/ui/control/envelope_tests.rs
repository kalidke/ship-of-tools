use super::*;

#[test]
fn nav_envelope_parses_valid_v1() {
    let txt = r#"{"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"mypackage","mode":"files","path":"src/edge.jl"}}"#;
    assert_eq!(
        parse_nav_envelope(txt),
        Some(NavEnvelope {
            workspace: "mypackage".to_string(),
            path: "src/edge.jl".to_string(),
        })
    );
}

#[test]
fn nav_envelope_tolerates_surrounding_whitespace() {
    let txt = "  \n{\"sot_ui\":{\"v\":1,\"cmd\":\"nav.preview\",\"workspace\":\"w\",\"path\":\"a/b.md\"}}\n ";
    assert_eq!(
        parse_nav_envelope(txt),
        Some(NavEnvelope {
            workspace: "w".to_string(),
            path: "a/b.md".to_string()
        })
    );
}

#[test]
fn nav_envelope_rejects_ordinary_chat() {
    // Ordinary prose, empty, malformed JSON, and valid-but-foreign JSON
    // all fall through to normal inbox rendering.
    assert_eq!(parse_nav_envelope("hey, look at src/edge.jl?"), None);
    assert_eq!(parse_nav_envelope(""), None);
    assert_eq!(parse_nav_envelope("{not json"), None);
    assert_eq!(parse_nav_envelope(r#"{"hello":"world"}"#), None);
}

#[test]
fn nav_envelope_rejects_wrong_version_cmd_or_missing_fields() {
    // Wrong version.
    assert_eq!(
        parse_nav_envelope(
            r#"{"sot_ui":{"v":2,"cmd":"nav.preview","workspace":"w","path":"p"}}"#
        ),
        None
    );
    // Unknown cmd (v1 only handles nav.preview).
    assert_eq!(
        parse_nav_envelope(r#"{"sot_ui":{"v":1,"cmd":"nav.jump","workspace":"w","path":"p"}}"#),
        None
    );
    // Missing path / missing workspace.
    assert_eq!(
        parse_nav_envelope(r#"{"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"w"}}"#),
        None
    );
    assert_eq!(
        parse_nav_envelope(r#"{"sot_ui":{"v":1,"cmd":"nav.preview","path":"p"}}"#),
        None
    );
    // Empty workspace / path.
    assert_eq!(
        parse_nav_envelope(
            r#"{"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"","path":"p"}}"#
        ),
        None
    );
    assert_eq!(
        parse_nav_envelope(
            r#"{"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"w","path":""}}"#
        ),
        None
    );
}
