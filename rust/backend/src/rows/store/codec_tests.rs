use super::*;

#[test]
fn parse_kv_top_level_only() {
    let text = r#"
workspace_id = "x"
slug         = "alpha"
[kernel]
status = "stopped"
"#;
    let kv = parse_kv(text);
    assert_eq!(kv.get("workspace_id").map(String::as_str), Some("x"));
    assert!(kv.get("status").is_none()); // inside [kernel], not top
}

#[test]
fn parse_section_scoped() {
    let text = r#"
workspace_id = "x"

[backend]
session_id = "y"
label = "MyPkg"
project_dir = "/p"
"#;
    let b = parse_section(text, "backend");
    assert_eq!(b.get("session_id").map(String::as_str), Some("y"));
    assert_eq!(b.get("label").map(String::as_str), Some("MyPkg"));
    assert!(b.get("workspace_id").is_none());
}

#[test]
fn strip_canonical_keeps_other_sections() {
    let input = r#"workspace_id = "old"
slug         = "alpha"
label        = "Alpha"
project_root = "/p"
session_name = "sot-be-alpha"
created      = 1700000000

[kernel]
status = "stopped"

[nav_state]
mode = "files"
cursor_path = "src/lib.jl"
"#;
    let stripped = strip_canonical_top_and_kernel(input);
    assert!(!stripped.contains("workspace_id"));
    assert!(!stripped.contains("[kernel]"));
    assert!(!stripped.contains("status = \"stopped\""));
    assert!(stripped.contains("[nav_state]"));
    assert!(stripped.contains("cursor_path = \"src/lib.jl\""));
}
