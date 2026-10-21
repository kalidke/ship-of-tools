use super::*;
use serde_json::json;

/// What the 0.6.6 scripts give for the same input (`sot_sanitize_component`, `_sot_handle_host`, `_sot_tier`), so a
/// session keeps its handle across the install.
#[test]
fn a_derived_handle_is_the_one_the_0_6_6_scripts_derive() {
    assert_eq!(sanitize_component("My Repo!!", 20), "My-Repo");
    assert_eq!(sanitize_component("--a--b--", 20), "a-b");
    assert_eq!(sanitize_component("\u{fc}n\u{ef}code", 20), "n-code");
    assert_eq!(sanitize_component("abcdefghijklmnopqrstuvwxyz-", 20), "abcdefghijklmnopqrst");
    assert_eq!(sanitize_component("", 20), "x");
    assert_eq!(sanitize_component("....", 20), "....");
    assert_eq!(hash6("/home/u/repo"), "955a1c");
    assert_eq!(handle_host("box1"), "box1");
    assert_eq!(handle_host("Box1.Lab"), "Box1.Lab", "a host the clamp leaves alone keeps its case and gets no digest");
    assert_eq!(handle_host("averyveryverylonghostname"), "averyveryver-5b2c20");
    assert_eq!(tier(1, "/home/u/My.Repo", "box1", Kind::Claude), (String::new(), "My.Repo-box1".to_string()));
    assert_eq!(tier(2, "/home/u/My.Repo", "box1", Kind::Claude), ("u".to_string(), "My.Repo-u-box1".to_string()));
    assert_eq!(tier(3, "/home/u/My.Repo", "box1", Kind::Claude), ("aa359d".to_string(), "My.Repo-aa359d-box1".to_string()));
}

#[test]
fn a_handle_is_an_inbox_file_name_and_nothing_else() {
    for ok in ["a", "ship-of-tools-host", "A.b_c-9", &"x".repeat(64)] {
        assert!(valid_handle(ok), "{ok}");
    }
    for bad in ["", "-a", "a..b", "a/b", "a b", "\u{e9}", &"x".repeat(65), ".."] {
        assert!(!valid_handle(bad), "{bad}");
    }
}

fn agents(v: serde_json::Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[test]
fn a_join_takes_its_own_root_again_and_steps_past_another_projects() {
    let mut notes = Vec::new();
    let none = agents(json!({}));
    let d = derive(Mode::Reclaim, "/home/u/repo", "box1", Kind::Claude, &none, None, &mut notes).unwrap();
    assert_eq!((d.name.as_str(), d.qualifier.as_str(), d.tier1.as_str()), ("repo-box1", "", "repo-box1"));
    let mine = agents(json!({"repo-box1": {"root": "/home/u/repo"}}));
    assert_eq!(derive(Mode::Reclaim, "/home/u/repo", "box1", Kind::Claude, &mine, None, &mut notes).unwrap().name, "repo-box1");
    assert!(notes.is_empty());
    let theirs = agents(json!({"repo-box1": {"root": "/srv/repo"}}));
    let d = derive(Mode::Reclaim, "/home/u/repo", "box1", Kind::Claude, &theirs, None, &mut notes).unwrap();
    assert_eq!((d.name.as_str(), d.qualifier.as_str()), ("repo-u-box1", "u"));
    assert_eq!(notes, ["comm: '@repo-box1' is already held by /srv/repo \u{2014} joining as '@repo-u-box1' instead"]);
}

#[test]
fn a_spawn_takes_only_a_handle_nobody_holds() {
    let mut notes = Vec::new();
    let mine = agents(json!({"repo-box1": {"root": "/home/u/repo"}}));
    let d = derive(Mode::Fresh, "/home/u/repo", "box1", Kind::Claude, &mine, None, &mut notes).unwrap();
    assert_eq!(d.name, "repo-u-box1", "a fresh spawn never absorbs a live row of the same root");
}

#[test]
fn every_tier_taken_is_a_refusal_that_writes_nothing() {
    let mut notes = Vec::new();
    let root = "/home/u/repo";
    let (_, t3) = tier(3, root, "box1", Kind::Claude);
    let mut full = agents(json!({"repo-box1": {"root": "/a"}, "repo-u-box1": {}}));
    full.insert(t3, json!({"root": "/c"}));
    let err = derive(Mode::Reclaim, root, "box1", Kind::Claude, &full, None, &mut notes).unwrap_err();
    assert!(err.contains("every derived handle") && err.contains("an unknown project") && err.contains("--name"), "{err}");
}

#[test]
fn a_join_from_outside_any_row_takes_only_what_the_daemon_cleared() {
    let mut notes = Vec::new();
    let root = "/home/u/repo";
    // Tier 1 is unheld but the daemon did not clear it (a row holds it): the join steps past it, saying so.
    let reg = agents(json!({}));
    let cleared = [("repo-u-box1".to_string(), String::new())];
    let d = derive(Mode::Reclaim, root, "box1", Kind::Claude, &reg, Some(&cleared), &mut notes).unwrap();
    assert_eq!(d.name, "repo-u-box1");
    assert_eq!(notes, ["comm: @repo-box1 is held by a row; a session outside any row joins as @repo-u-box1"]);
    // A row that bound the cleared name since the daemon answered is not cleared any more.
    let bound = agents(json!({"repo-u-box1": {"workspace_id": "ws-x", "root": root}}));
    let mut notes = Vec::new();
    let r = derive(Mode::Reclaim, root, "box1", Kind::Claude, &bound, Some(&cleared), &mut notes);
    assert_eq!(r.unwrap_err().matches("held by").count(), 3);
}

#[test]
fn a_codex_row_and_a_claude_row_in_one_folder_derive_two_handles() {
    let root = "/home/u/repo";
    let mut notes = Vec::new();
    let mut reg = agents(json!({}));
    let claude = derive(Mode::Reclaim, root, "box1", Kind::Claude, &reg, None, &mut notes).unwrap();
    assert_eq!(claude.name, "repo-box1");
    reg.insert(claude.name.clone(), json!({"root": root, "workspace_id": "ws-claude"}));
    let codex = derive(Mode::Reclaim, root, "box1", Kind::Codex, &reg, None, &mut notes).unwrap();
    assert_eq!((codex.name.as_str(), codex.qualifier.as_str()), ("repo-cx-box1", ""));
    reg.insert(codex.name.clone(), json!({"root": root, "workspace_id": "ws-codex"}));
    // Each joins again and keeps its own: the other's entry is never the one it moves.
    assert_eq!(derive(Mode::Reclaim, root, "box1", Kind::Claude, &reg, None, &mut notes).unwrap().name, "repo-box1");
    assert_eq!(derive(Mode::Reclaim, root, "box1", Kind::Codex, &reg, None, &mut notes).unwrap().name, "repo-cx-box1");
    assert!(notes.is_empty());
    // The tiers after tier 1 are the same for both kinds.
    assert_eq!(tier(2, root, "box1", Kind::Codex), ("u".to_string(), "repo-u-cx-box1".to_string()));
    assert_eq!(tier(3, root, "box1", Kind::Codex).1, format!("repo-{}-cx-box1", hash6(root)));
}
