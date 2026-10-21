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
    let o = registry(&args(&["read", "a"]));
    assert_eq!((o.code, o.stdout.as_str()), (0, "{\"host\":\"box1\"}\n"));
    assert_eq!(registry(&args(&["read", "b"])).code, 1);
    std::fs::write(t.registry(), "not json").unwrap();
    assert_eq!(registry(&args(&["read", "a"])).code, 2, "unreadable is never absent");
    assert_eq!(registry(&args(&["read"])).code, 2);
}

#[test]
fn every_door_write_is_one_locked_step() {
    let t = TestHome::new();
    assert_eq!(registry(&args(&["del", "x"])).code, 0, "nothing to delete is no failure");
    t.put("prov", json!({"status": "spawning", "root": "/r", "nonce": "n1"}));
    assert_eq!(registry(&args(&["del-provisional", "prov", "/r", "other"])).code, 2, "not ours any more");
    assert_eq!(registry(&args(&["del-provisional", "prov", "/r", "n1"])).code, 0);
    assert!(t.agents().get("prov").is_none());
    t.put("live", json!({"host": "box1", "last_seen": "2020-01-01T00:00:00Z"}));
    assert_eq!(registry(&args(&["touch", "live"])).code, 0);
    assert_ne!(t.agents()["live"]["last_seen"], "2020-01-01T00:00:00Z");
    assert_eq!(stop_mark(&args(&["live"])).code, 0);
    assert!(t.agents()["live"]["stop_at"].is_string());
    std::fs::write(t.home.join(".registry.lock"), "elsewhere:-:-:-:1:-\n").unwrap();
    std::env::set_var("SOT_LOCK_WAIT_SECS", "0");
    let o = registry(&args(&["del", "live"]));
    assert_eq!(o.code, 1);
    assert!(o.stderr.starts_with("FAILED: registry lock ") && o.stderr.contains("is held by elsewhere"), "{}", o.stderr);
    assert!(t.agents().get("live").is_some(), "a held lock writes nothing");
}

#[test]
fn derive_prints_the_three_lines_the_spawn_reads() {
    let t = TestHome::new();
    t.put("repo-box1", json!({"root": "/elsewhere"}));
    let o = derive(&args(&["fresh", "/home/u/repo", "box1"]));
    assert_eq!((o.code, o.stdout.as_str()), (0, "repo-u-box1\nu\nrepo-box1\n"), "{}", o.stderr);
    assert!(t.agents().get("repo-u-box1").is_none(), "a derive alone writes nothing");
    assert_eq!(self_slot(&args(&["box1", "ws/1"])).stdout, "box1__ws_1.txt");
    assert_eq!(name_part(&args(&["component", "My Repo!!"])).stdout, "My-Repo");
    assert_eq!(name_part(&args(&["handle-host", "averyveryverylonghostname"])).stdout, "averyveryver-5b2c20");
    assert_eq!(routable(&args(&["", "/r"])).code, 1);
}
