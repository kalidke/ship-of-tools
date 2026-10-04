//! The registry poll's change detection; the poll task runs in the server's `run` until cut-run moves it here.

/// Canonical projection of just the state-relevant fields per sot-comm
/// registry agent (state/summary/status_at, plus `host` — LU5d2:
/// `workspace.list`'s registry reads are now host-filtered, so a `host`
/// edit alone, e.g. a stale row's
/// owner changing, is a real change even when every other field is
/// unchanged). `last_seen` is deliberately excluded. Used by the
/// registry-watch task in `run` (below) to detect a real change between
/// polls; hoisted to module scope (out of that task's async block) so it's
/// unit-testable on its own. Deliberately NOT host-filtered (unlike the
/// list itself): this has no per-workspace context to filter against, only
/// a flat agent map, so a write on another host still costs one extra
/// `workspace.list` broadcast — cheaper than threading `declared_host()`
/// through a projection whose only job is "did anything change".
pub(crate) fn project_comm_registry(bytes: &[u8]) -> String {
    let root: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let agents = match root.get("agents").and_then(|a| a.as_object()) {
        Some(o) => o,
        None => return String::new(),
    };
    let mut keys: Vec<&String> = agents.keys().collect();
    keys.sort();
    let mut s = String::new();
    for k in keys {
        let e = &agents[k];
        s.push_str(k);
        for field in ["state", "summary", "status_at", "host"] {
            s.push('\u{1}');
            s.push_str(e.get(field).and_then(|v| v.as_str()).unwrap_or(""));
        }
        s.push('\u{2}');
    }
    s
}

#[cfg(test)]
mod tests {
    mod project_comm_registry_tests {
        // LU5d2: the registry-watch task's own change detection must see a
        // `host` edit as a real change (a stale row changing owner is not a
        // no-op) even though every other field stayed the same.
        use super::super::project_comm_registry;

        fn registry(host: &str) -> Vec<u8> {
            serde_json::to_vec(&serde_json::json!({
                "agents": {
                    "host-4-be-x": {
                        "state": "idle",
                        "summary": "",
                        "status_at": "",
                        "tmux": "sot-be-x:0.0",
                        "host": host
                    }
                }
            }))
            .unwrap()
        }

        #[test]
        fn a_host_change_alone_changes_the_projection() {
            let before = project_comm_registry(&registry("hostA"));
            let after = project_comm_registry(&registry("hostB"));
            assert_ne!(before, after);
        }

        #[test]
        fn an_unchanged_registry_projects_identically() {
            let a = project_comm_registry(&registry("hostA"));
            let b = project_comm_registry(&registry("hostA"));
            assert_eq!(a, b);
        }
    }
}
