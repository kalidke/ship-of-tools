use super::*;

#[test]
fn awareness_env_shapes() {
    // SOT_SESSION is always present and first; slug/cwd/workspace_id add
    // their vars. (SOT_MANUAL is checkout-dependent and SOT_SOCKET
    // depends on whether this process ever called set_own_endpoint --
    // no exact-value assertion on either here; see the dedicated test
    // below. No SOT_SELF_HOST pin at all: an
    // override set on the daemon's own process already reaches every
    // child through ordinary env inheritance.)
    let find = |env: &[(String, String)], k: &str| -> Option<String> {
        env.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.clone())
    };
    let bare = awareness_env(None, None, None);
    assert_eq!(bare[0], ("SOT_SESSION".to_string(), "1".to_string()));
    assert_eq!(find(&bare, "SOT_WORKSPACE"), None);
    assert_eq!(find(&bare, "SOT_WORKSPACE_ID"), None);
    assert_eq!(find(&bare, "SOT_WORKSPACE_ROOT"), None);

    let full = awareness_env(Some("alpha"), Some(Path::new("/proj/alpha")), Some("ws-alpha-1"));
    assert_eq!(find(&full, "SOT_WORKSPACE").as_deref(), Some("alpha"));
    assert_eq!(find(&full, "SOT_WORKSPACE_ID").as_deref(), Some("ws-alpha-1"));
    assert_eq!(find(&full, "SOT_WORKSPACE_ROOT").as_deref(), Some("/proj/alpha"));
}

#[test]
#[cfg(unix)]
fn awareness_env_carries_own_endpoint_once_set() {
    // set_own_endpoint is idempotent by construction (the daemon binds
    // exactly one listener per process) and process-global (`OnceLock`),
    // so this asserts the BARE SHAPE, never an exact path another test
    // in this binary may have already pinned it to. Manager review
    // (S4): SOT_SOCKET keeps its pre-existing meaning, a bare Unix
    // socket path -- no typed unix:/pipe: prefix, pinned on Unix only.
    set_own_endpoint(Path::new("/tmp/sot-test/sot.sock"));
    let env = awareness_env(None, None, None);
    let socket = env.iter().find(|(k, _)| k == "SOT_SOCKET").map(|(_, v)| v.clone());
    assert!(
        socket.as_deref().is_some_and(|s| !s.starts_with("unix:") && !s.starts_with("pipe:")),
        "SOT_SOCKET must be a bare path, got {socket:?}"
    );
}
