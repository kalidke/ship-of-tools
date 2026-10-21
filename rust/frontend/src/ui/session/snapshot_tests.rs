use super::*;

#[test]
fn same_slug_across_hosts_produces_distinct_ws_keys_and_snapshot_slots() {
    // ADR 0042 L2a codex review, item B: workspace_ui_snapshots (and
    // every sibling map) used to key by bare slug, so switching from
    // "sot" on host alpha to "sot" on host beta was the SAME map key
    // — restore_workspace_ui found alpha's stashed snapshot and
    // repainted it under beta. WsKey = (host, slug) makes the two
    // entries distinct slots, so a same-slug cross-host switch can
    // only ever hit its own host's entry.
    let key_alpha: WsKey = ("alpha".to_string(), "sot".to_string());
    let key_beta: WsKey = ("beta".to_string(), "sot".to_string());
    assert_ne!(
        key_alpha, key_beta,
        "same slug on two different hosts must not collide"
    );

    let mut snaps: HashMap<WsKey, &'static str> = HashMap::new();
    snaps.insert(key_alpha.clone(), "alpha's snapshot");
    snaps.insert(key_beta.clone(), "beta's snapshot");
    assert_eq!(snaps.get(&key_alpha), Some(&"alpha's snapshot"));
    assert_eq!(
        snaps.get(&key_beta),
        Some(&"beta's snapshot"),
        "beta's own snapshot must survive alpha's insert under the same slug"
    );
}
