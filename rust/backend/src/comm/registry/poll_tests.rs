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
