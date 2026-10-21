use super::*;

#[test]
fn self_comm_handle_is_fe_at_the_declared_host() {
    // Topology plan §A: the address is derived from the ONE declared
    // host, `fe@<host>` — no second resolver, no platform prefix.
    let h = self_comm_handle();
    assert_eq!(h, format!("fe@{}", frontend_identity().host), "got {h:?}");
    assert_eq!(frontend_address("host-a"), "fe@host-a");
}

#[test]
fn frontend_identity_is_constructed_once_per_process() {
    // ADR 0046 decision 1: every caller shares ONE identity -- in
    // particular `instance` must never re-mint across calls the way
    // the old bare `fe_instance_component()` could when
    // `SOT_FE_INSTANCE` was unset (two calls could disagree).
    let a = frontend_identity();
    let b = frontend_identity();
    assert_eq!(a.host, b.host);
    assert_eq!(a.instance, b.instance);
    assert_eq!(a.name, b.name);
    assert!(!a.instance.is_empty());
    assert!(!a.name.is_empty());
    assert_eq!(FrontendIdentity::ROLE, "fe");
}

/// BLOCKER (Codex review, lane B5 discharge): controller-instance
/// identity — `SOT_FE_INSTANCE` (set once per `launch-sot.ps1`
/// supervisor invocation, inherited across every managed relaunch it
/// spawns) wins whenever present and non-empty; an absent or empty
/// value (a dev/manual run, or a launcher build that predates this)
/// falls back to this process's own mint.
#[test]
fn resolve_fe_instance_component_prefers_the_supervisors_env_var() {
    assert_eq!(
        resolve_fe_instance_component(Some("supervisor-abc123"), "fallback-mint"),
        "supervisor-abc123"
    );
    assert_eq!(resolve_fe_instance_component(Some(""), "fallback-mint"), "fallback-mint");
    assert_eq!(resolve_fe_instance_component(None, "fallback-mint"), "fallback-mint");
}
