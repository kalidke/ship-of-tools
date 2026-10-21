use super::*;

#[test]
fn a_slot_is_named_for_its_host_and_row_or_refused_whole() {
    assert_eq!(slot_leaf("box1", "ws-a-1").unwrap(), "box1__ws-a-1.txt");
    assert_eq!(slot_leaf("box1", "").unwrap(), "box1__nopane.txt");
    assert_eq!(slot_leaf("box1", "ws/a b").unwrap(), "box1__ws_a_b.txt");
    assert!(slot_leaf("", "ws").is_err());
    assert!(slot_leaf("a/b", "ws").is_err(), "a host is never rewritten to fit");
    assert!(!leaf_valid(&"x".repeat(256), false));
    assert!(leaf_valid("box1__ws.txt", true));
    for bad in ["a:b.txt", "CON.txt", "com1.txt", "x.txt.", "x.txt ", "a|b"] {
        assert!(!leaf_valid(bad, true), "{bad}");
    }
    assert!(leaf_valid("com10.txt", true) && leaf_valid("a:b.txt", false));
}

#[test]
fn a_pinned_slot_names_its_row_and_nopane_names_none() {
    assert_eq!(row_of(Path::new("/c/self/box1__ws-1.txt"), "box1").as_deref(), Some("ws-1"));
    assert_eq!(row_of(Path::new("/c/self/box1__nopane.txt"), "box1"), None);
    assert_eq!(row_of(Path::new("/c/self/a__b__ws-2.txt"), "a__b").as_deref(), Some("ws-2"));
    assert_eq!(row_of(Path::new("/c/self/a__b__ws-2.txt"), "zz").as_deref(), Some("b__ws-2"));
    assert_eq!(row_of(Path::new("/c/self/private.txt"), "box1"), None);
    assert!(is_nopane(Path::new("/c/self/box1__nopane.txt")));
}

#[test]
fn only_a_self_file_for_this_root_names_a_handle() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("box1__ws.txt");
    let mut notes = Vec::new();
    assert_eq!(name_for(&p, "/r", &mut notes), "", "no file names nobody, silently");
    assert!(notes.is_empty());
    write(&p, "h-box1", "r", "/r").unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "h-box1\nrepo=r\nroot=/r\n");
    assert_eq!(name_for(&p, "/r", &mut notes), "h-box1");
    assert_eq!(name_for(&p, "/other", &mut notes), "");
    assert!(notes[0].contains("doesn't match this project"), "{notes:?}");
    for (text, why) in [("h\nrepo=r\n", "no root= line"), ("h\nrepo=r\nrootBROKEN\n", "malformed third line"), ("h\nrepo=r\nroot=\n", "empty/malformed root=")] {
        std::fs::write(&p, text).unwrap();
        let mut notes = Vec::new();
        assert_eq!(name_for(&p, "/r", &mut notes), "", "{text:?}");
        assert!(notes[0].contains(why), "{notes:?}");
    }
}

#[test]
fn a_row_slot_never_comes_to_name_another_projects_session() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("box1__ws.txt");
    write(&p, "a-box1", "a", "/a").unwrap();
    assert_eq!(conflict(&p, "a", "/a"), None);
    assert!(conflict(&p, "b", "/b").unwrap().contains("already names @a-box1, claimed for root='/a'"));
    std::fs::write(&p, "a-box1\nrepo=a\n").unwrap();
    assert!(conflict(&p, "b", "/b").unwrap().contains("legacy slot"));
    assert_eq!(conflict(&p, "a", "/b"), None, "a legacy slot with the same repo is no evidence");
    let shared = dir.path().join("box1__nopane.txt");
    write(&shared, "a-box1", "a", "/a").unwrap();
    assert_eq!(conflict(&shared, "b", "/b"), None, "the shared slot is last-writer-wins");
}
