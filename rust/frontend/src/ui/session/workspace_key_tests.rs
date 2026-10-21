use super::*;

#[test]
fn default_workspace_names() {
    for name in ["", "default", "<default>"] {
        assert!(is_default_workspace_name(name), "{name:?}");
    }
    for name in ["Default", " default", "default ", "<Default>", "defaults", "x"] {
        assert!(!is_default_workspace_name(name), "{name:?}");
    }
}

#[test]
fn lifecycle_key_translates_canonical_id_to_slug() {
    // Lifecycle frames stamp the CANONICAL workspace id; every FE surface
    // keys by slug (the `started`-frame lesson: a canonical-id key
    // silently never matches). The map from the last workspace.list is
    // the translation. ADR 0042 L2a: the returned `WsKey` carries the
    // HOST that `workspace_id_slugs` recorded for the id, which can
    // differ from the frame's own connection (`host` below) — the
    // lookup wins over the fallback whenever the id IS known.
    let mut ids: HashMap<(HostKey, String), WsKey> = HashMap::new();
    ids.insert(
        ("myhost".to_string(), "ws-alpha-1a2b".to_string()),
        ("myhost".to_string(), "alpha".to_string()),
    );
    assert_eq!(
        lifecycle_key_of("myhost", Some("ws-alpha-1a2b"), &ids, Some("home")),
        ("myhost".to_string(), "alpha".to_string())
    );
    // Already-a-slug (or unknown) hints store as-is, under the frame's
    // OWN host (the only host information available for an unknown id).
    assert_eq!(
        lifecycle_key_of("myhost", Some("beta"), &ids, Some("home")),
        ("myhost".to_string(), "beta".to_string())
    );
    // ADR 0042 L2a codex review, item J: the SAME canonical id under a
    // DIFFERENT host must not resolve — otherwise ws-alpha-1a2b known
    // only to "myhost" would leak into "otherhost"'s lookup.
    assert_eq!(
        lifecycle_key_of("otherhost", Some("ws-alpha-1a2b"), &ids, Some("home")),
        ("otherhost".to_string(), "ws-alpha-1a2b".to_string()),
        "an id known only to a different host falls through to the unknown-hint case"
    );
}

#[test]
fn lifecycle_key_none_hint_is_the_default_workspace() {
    // A frame with no workspace id (only an older daemon sends one) IS
    // the default workspace. Resolve to its slug once known, "<default>" before the
    // first workspace.list reply (transient; the list's repl_state
    // catch-up re-keys it). Always under the frame's own host.
    let ids: HashMap<(HostKey, String), WsKey> = HashMap::new();
    assert_eq!(
        lifecycle_key_of("myhost", None, &ids, Some("home")),
        ("myhost".to_string(), "home".to_string())
    );
    assert_eq!(
        lifecycle_key_of("myhost", None, &ids, None),
        ("myhost".to_string(), "<default>".to_string())
    );
}
