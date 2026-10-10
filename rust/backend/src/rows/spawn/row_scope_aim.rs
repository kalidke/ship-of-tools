// row_scope_aim.rs — A4b: the one gate before any write to a row scope's
// `cgroup.kill`, and the root it is written under. Dependency-free on purpose:
// `rows::spawn::row_scope` uses it through `mod`, and `tests/support/mod.rs`
// includes this same file by `#[path]`, so a test's kill guard can never aim
// wider than production, and a test reads a scope where production does.

use std::path::{Path, PathBuf};

/// Where this host mounts its cgroup v2 hierarchy, in the two layouts systemd
/// makes: `/sys/fs/cgroup` when it is the v2 mount (a unified host), else
/// `/sys/fs/cgroup/unified` (a hybrid host: v1 controllers on a tmpfs, as under
/// systemd 245). A v2 mount's root holds `cgroup.controllers`. A host with no v2
/// hierarchy gives its processes no `0::` path, so no row scope is captured.
pub(crate) fn v2_root() -> PathBuf {
    v2_root_in(Path::new("/sys/fs/cgroup"))
}

/// [`v2_root`] with `base` for `/sys/fs/cgroup`, for a test's fake tree.
pub(crate) fn v2_root_in(base: &Path) -> PathBuf {
    if base.join("cgroup.controllers").exists() {
        base.to_path_buf()
    } else {
        base.join("unified")
    }
}

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
/// test runs it against [`aim`], and `tests/capsule_workspaces/main.rs` runs it
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
