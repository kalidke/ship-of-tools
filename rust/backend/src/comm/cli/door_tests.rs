use super::*;
use crate::comm::test_home::TestHome;
use serde_json::json;

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// The shell door answers as the 0.6.6 functions answered: 0 present, 1 absent, 2 unreadable.
#[test]
fn a_registry_read_tells_present_absent_and_unreadable_apart() {
    let t = TestHome::new();
    t.put("a", json!({"host": "box1"}));
    let o = registry(&t.env, &args(&["read", "a"]));
    assert_eq!((o.code, o.stdout.as_str()), (0, "{\"host\":\"box1\"}\n"));
    assert_eq!(registry(&t.env, &args(&["read", "b"])).code, 1);
    std::fs::write(t.registry(), "not json").unwrap();
    assert_eq!(registry(&t.env, &args(&["read", "a"])).code, 2, "unreadable is never absent");
    assert_eq!(registry(&t.env, &args(&["read"])).code, 2);
}

#[test]
fn every_door_write_is_one_locked_step() {
    let mut t = TestHome::exclusive();
    assert_eq!(registry(&t.env, &args(&["del", "x"])).code, 0, "nothing to delete is no failure");
    t.put("prov", json!({"status": "spawning", "root": "/r", "nonce": "n1"}));
    assert_eq!(registry(&t.env, &args(&["del-provisional", "prov", "/r", "other"])).code, 2, "not ours any more");
    assert_eq!(registry(&t.env, &args(&["del-provisional", "prov", "/r", "n1"])).code, 0);
    assert!(t.agents().get("prov").is_none());
    t.put("live", json!({"host": "box1", "last_seen": "2020-01-01T00:00:00Z"}));
    assert_eq!(registry(&t.env, &args(&["touch", "live"])).code, 0);
    assert_ne!(t.agents()["live"]["last_seen"], "2020-01-01T00:00:00Z");
    assert_eq!(stop_mark(&t.env, &args(&["live"])).code, 0);
    assert!(t.agents()["live"]["stop_at"].is_string());
    std::fs::write(t.home.join(".registry.lock"), "elsewhere:-:-:-:1:-\n").unwrap();
    t.set("SOT_LOCK_WAIT_SECS", "0");
    let o = registry(&t.env, &args(&["del", "live"]));
    assert_eq!(o.code, 1);
    assert!(o.stderr.starts_with("FAILED: registry lock ") && o.stderr.contains("is held by elsewhere"), "{}", o.stderr);
    assert!(t.agents().get("live").is_some(), "a held lock writes nothing");
}

#[test]
fn derive_prints_the_three_lines_the_spawn_reads() {
    let t = TestHome::new();
    t.put("repo-box1", json!({"root": "/elsewhere"}));
    let o = derive(&t.env, &args(&["fresh", "/home/u/repo", "box1"]));
    assert_eq!((o.code, o.stdout.as_str()), (0, "repo-u-box1\nu\nrepo-box1\n"), "{}", o.stderr);
    assert!(t.agents().get("repo-u-box1").is_none(), "a derive alone writes nothing");
    assert_eq!(self_slot(&t.env, &args(&["box1", "ws/1"])).stdout, "box1__ws_1.txt");
    assert_eq!(slug(&t.env, &args(&["My Row"])).stdout, format!("{}\n", sot_topology::endpoint::slug("My Row")));
    assert_eq!(routable(&t.env, &args(&["", "/r"])).code, 1);
}

#[test]
fn derive_fresh_uses_the_childs_kind_and_defaults_to_claude() {
    let mut t = TestHome::new();
    t.set("SOT_AGENT_KIND", "codex");
    for (kind, name) in [(Some("codex"), "repo-cx-box1"), (None, "repo-box1"), (Some("claude"), "repo-box1")] {
        let mut argv = args(&["fresh", "/home/u/repo", "box1"]);
        if let Some(kind) = kind {
            argv.extend(args(&["--agent", kind]));
        }
        let o = derive(&t.env, &argv);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(o.stdout, format!("{name}\n\n{name}\n"));
    }
}

/// The kind is the argument's alone (the decoy environment says Codex), on the claim path as on the read-only one, and
/// any other form of the arguments is usage that derives and writes nothing.
#[test]
fn derive_arguments_are_one_table() {
    let mut t = TestHome::new();
    t.set("SOT_AGENT_KIND", "codex");
    let base = ["fresh", "/home/u/repo", "box1"];
    let run = |extra: &[&str]| {
        let mut argv = args(&base);
        argv.extend(args(extra));
        derive_from(&t.env, &argv, || r#"{"host":"box1"}"#.to_string())
    };
    let keys = || t.agents().as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    t.put("keep", json!({"host": "box1"}));
    for extra in [
        &["--agent", "bad"][..],
        &["--agent"],
        &["--agent", "codex", "--agent", "codex"],
        &["--claim", "--agent", "codex"],
        &["--agent", "--claim"],
        &["--agent", "codex", "--bogus"],
        &["--bogus"],
        &["--claim", "--claim"],
    ] {
        let o = run(extra);
        assert_eq!((o.code, o.stdout.as_str()), (2, ""), "{extra:?}");
        assert!(o.stderr.starts_with("usage: ") && o.stderr.contains("[--agent claude|codex]"), "{extra:?}: {}", o.stderr);
        assert_eq!(keys(), ["keep"], "{extra:?} wrote");
    }
    // The two old forms.
    assert_eq!(run(&[]).stdout, "repo-box1\n\nrepo-box1\n");
    assert_eq!(keys(), ["keep"]);
    // The claim path takes the argument's kind and writes under that handle in the same step.
    let o = run(&["--agent", "codex", "--claim"]);
    assert_eq!((o.code, o.stdout.as_str()), (0, "repo-cx-box1\n\nrepo-cx-box1\n"), "{}", o.stderr);
    assert!(t.agents().get("repo-cx-box1").is_some() && t.agents().get("repo-box1").is_none());
}
