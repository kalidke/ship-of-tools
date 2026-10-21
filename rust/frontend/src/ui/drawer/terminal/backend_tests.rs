use super::*;

#[test]
fn attach_only_requires_matching_state_root() {
    let roots = vec!["r".to_string()];
    // (what, setting, attach_live, local_live, roots, own_root, want)
    let cases: &[(&str, bool, bool, bool, &[String], Option<&str>, bool)] = &[
        ("setting on, matching root, no terminals", true, false, false, &roots, Some("r"), true),
        ("setting off", false, false, false, &roots, Some("r"), false),
        ("no granted lease", true, false, false, &[], Some("r"), false),
        ("a root that differs", true, false, false, &roots, Some("x"), false),
        ("own root unknown", true, false, false, &roots, None, false),
        ("attach already live, setting off", false, true, false, &[], None, true),
        ("plain terminal already live", true, false, true, &roots, Some("r"), false),
    ];
    for (what, setting, attach, local, granted, own, want) in cases {
        assert_eq!(drawer_uses_attach(*setting, *attach, *local, granted, *own), *want, "{what}");
    }
}
