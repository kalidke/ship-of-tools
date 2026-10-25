use super::*;

#[test]
fn default_workspace_names() {
    for name in ["", "default", "<default>"] {
        assert!(is_default_workspace_name(name), "{name:?}");
    }
    for name in ["Default", " default", "default ", "<Default>", "defaults", "x"] {
        assert!(!is_default_workspace_name(name), "{name:?}");
    }
}
