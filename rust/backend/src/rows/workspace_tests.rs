use super::*;

#[test]
fn workspace_from_label_uses_slug_and_tmux_convention() {
    let ws = Workspace::from_label("MyPkg.jl", PathBuf::from("/home/u/MyPkg.jl"), false, "none".into(), String::new(), String::new());
    assert_eq!(ws.slug, "mypkg_jl");
    assert_eq!(ws.session_name, "sot-be-mypkg_jl");
    assert_eq!(ws.label, "MyPkg.jl");
    assert_eq!(ws.project_root, PathBuf::from("/home/u/MyPkg.jl"));
}

#[test]
fn workspace_kernel_built_reflects_lazy_construction() {
    // Fresh workspace: handle not yet constructed.
    let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
    assert!(!ws.kernel_built());
    // Calling .kernel() constructs the handle (no child spawned yet).
    let _ = ws.kernel();
    assert!(ws.kernel_built());
}

#[test]
fn every_phase_has_a_distinct_wire_string() {
    let all = [
        Phase::Stopped,
        Phase::Starting,
        Phase::Ready,
        Phase::Ending,
        Phase::EndedNoRespawn,
        Phase::Terminal,
        Phase::Unreachable,
        Phase::Foreign,
    ];
    let strings: std::collections::HashSet<&str> = all.iter().map(|p| p.as_wire_str()).collect();
    assert_eq!(strings.len(), all.len(), "every Phase must have its own distinct wire string");
}
