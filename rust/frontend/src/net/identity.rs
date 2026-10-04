//! This frontend's one declared identity (ADR 0046 decision 1) and its fe@<host> address.

/// This frontend process's own declared identity (ADR 0046 decision 1):
/// `{host, instance, name, role: "fe"}`, constructed ONCE and shared by
/// every connection, reconnect and input attribution — `instance`
/// replaces `fe_instance_component`'s old per-call recomputation (which
/// literally re-sampled the clock on every call when `SOT_FE_INSTANCE`
/// was unset, so two calls in the same process could mint two different
/// fallback instances).
///
/// `name` is this frontend's ADDRESS, `fe@<host>` (topology plan §A):
/// the value a `--fe <host>` target (`fe.command.send` `target`) matches
/// on, derived from the one declared `host` — no second host resolver.
/// Two frontends on one box share the address and differ by `instance`.
#[derive(Debug, Clone)]
pub(crate) struct FrontendIdentity {
    pub host: String,
    pub instance: String,
    pub name: String,
}

impl FrontendIdentity {
    pub const ROLE: &'static str = "fe";
}

/// The address a frontend on `host` answers to: `fe@<host>`. The same
/// shape `sot-fe --fe <host>` builds its wire `target` from.
pub(crate) fn frontend_address(host: &str) -> String {
    format!("fe@{host}")
}

/// Cached singleton, mirroring the backend's own `declared_host()`
/// (`sot-backend`'s `workspaces.rs`) — one resolver, called once, read
/// everywhere after. `sot_log::state_dir::host_name()` failing means this
/// process has no nameable host at all; fatal, same posture the backend
/// takes at boot, rather than limping on with a guessed address no peer
/// could actually reach it by.
pub(crate) fn frontend_identity() -> &'static FrontendIdentity {
    static IDENTITY: std::sync::OnceLock<FrontendIdentity> = std::sync::OnceLock::new();
    IDENTITY.get_or_init(|| {
        let host = sot_log::state_dir::host_name().unwrap_or_else(|e| {
            panic!("cannot start: no declared host (ADR 0046 decision 1): {e}");
        });
        let env = std::env::var("SOT_FE_INSTANCE").ok();
        let fallback = format!(
            "{:x}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let instance = resolve_fe_instance_component(env.as_deref(), &fallback);
        let name = frontend_address(&host);
        FrontendIdentity { host, instance, name }
    })
}

/// This FE's address (ADR 0025 target filter) — a read of
/// `frontend_identity().name` (`fe@<host>`), kept as a named function
/// since it has call sites all over this file predating that identity.
/// The daemon scopes an `FE_COMMAND`'s `target` to one FE by this
/// address; we self-filter against it. `pub(crate)` because
/// `transport.rs` sends this same value as `HelloReq::name`, so the
/// daemon can name this connection without a second derivation.
pub(crate) fn self_comm_handle() -> String {
    frontend_identity().name.clone()
}

/// ADR 0045 decision 1 (Codex review, lane B5 discharge): pure core of
/// [`fe_instance_component`] — `env` is `std::env::var("SOT_FE_INSTANCE")`'s
/// own `Ok(_)` outcome (empty treated as absent), `fallback` this
/// process's own mint. Unit-tested without touching a real env var.
fn resolve_fe_instance_component(env: Option<&str>, fallback: &str) -> String {
    match env {
        Some(v) if !v.is_empty() => v.to_string(),
        _ => fallback.to_string(),
    }
}

#[cfg(test)]
mod tests {
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
}
