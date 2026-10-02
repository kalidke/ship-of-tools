// row_scope_aim.rs — A4b: the one gate before any write to a row scope's
// `cgroup.kill`. Dependency-free on purpose: `capsule_workspace::row_scope`
// uses it through `mod`, and `tests/support/mod.rs` includes this same file
// by `#[path]`, so a test's kill guard can never aim wider than production.

/// The leaf prefix of every systemd scope a row's supervisor is spawned
/// into (`systemd-run --unit`); `hash` is the row's `state_dir_hash`.
pub(crate) fn prefix(hash: &str) -> String {
    format!("sot-row-{hash}-")
}

/// Accept `rel` (a cgroup path relative to the cgroup2 root) only when it
/// is a scope of the row whose hash is `hash`, and neither `own_rel` (the
/// caller's own cgroup) nor an ancestor of it. An unknown (empty) `own_rel`
/// refuses every aim, since that check cannot then be made.
pub(crate) fn aim(rel: &str, own_rel: &str, hash: &str) -> Result<(), String> {
    if own_rel.is_empty() {
        return Err(format!("the caller's own cgroup is unknown: {rel:?}"));
    }
    if !rel.starts_with('/') {
        return Err(format!("not an absolute cgroup path: {rel:?}"));
    }
    if rel.split('/').any(|s| s == "." || s == "..") {
        return Err(format!("a `.` or `..` segment: {rel:?}"));
    }
    let leaf = rel.rsplit('/').next().unwrap_or("");
    if !(leaf.starts_with(&prefix(hash)) && leaf.ends_with(".scope")) {
        return Err(format!("not a scope of this row ({}*.scope): {rel:?}", prefix(hash)));
    }
    let base = rel.trim_end_matches('/');
    if own_rel == rel || own_rel == base || own_rel.starts_with(&format!("{base}/")) {
        return Err(format!("the caller's own cgroup or an ancestor of it ({own_rel:?}): {rel:?}"));
    }
    Ok(())
}

/// The aim rule's one table, `(target, own, accepted)`: `row_scope`'s unit
/// test runs it against [`aim`], and `tests/capsule_workspaces.rs` runs it
/// against the test scope guard through `tests/support`'s copy of this file.
#[cfg(test)]
pub(crate) fn aim_table(h: &str) -> Vec<(String, String, bool)> {
    let other = if h == "0123456789abcdef" { "fedcba9876543210" } else { "0123456789abcdef" };
    let ours = format!("/a/app.slice/sot-row-{h}-x.scope");
    let own = "/a/app.slice/run-u1.scope".to_string();
    vec![
        (ours.clone(), own.clone(), true),
        (ours.clone(), ours.clone(), false),
        (ours.clone(), String::new(), false),
        ("/a/app.slice".to_string(), own.clone(), false),
        (format!("/a/sot-row-{h}-y.scope"), format!("/a/sot-row-{h}-y.scope/app.slice/run-u1.scope"), false),
        ("/".to_string(), own.clone(), false),
        (format!("/a/app.slice/sot-row-{other}-x.scope"), own.clone(), false),
        ("run-u5.scope".to_string(), own.clone(), false),
        ("/a/app.slice/run-u5.scope".to_string(), own.clone(), false),
        ("/a/app.slice/run-u123.scope".to_string(), own.clone(), false),
        (format!("a/app.slice/sot-row-{h}-x.scope"), own.clone(), false),
        (format!("/a/app.slice/../app.slice/sot-row-{h}-x.scope"), own.clone(), false),
        (format!("/a/./app.slice/sot-row-{h}-x.scope"), own.clone(), false),
        (format!("/a/app.slice/sot-row-{h}-x.service"), own, false),
    ]
}
