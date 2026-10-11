//! Native private-Signal behavior of the REPL and MathJax supervisors, and the isolated-fixture helpers the sidecar
//! tests with real children share.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::lifecycle::child_signal::Signal;
use crate::sidecars::mathjax::MathJax;
use crate::sidecars::repl::{lifecycle::ReplLifecycle, Repl};
use tokio::sync::broadcast;

const CHILD_ENV: &str = "SOT_TEST_SIDECAR_ISOLATED";

/// Runs `name` in a child of this test binary (own process environment, own working directory) within `bound`, and
/// returns `false` in the parent once the child exited 0 having entered the body exactly once. In the child it
/// records the entry and returns `true`.
pub(crate) fn isolated(name: &str, bound: Duration) -> bool {
    if std::env::var(CHILD_ENV).as_deref() == Ok(name) {
        sot_log::test_isolated::enter(name);
        return true;
    }
    let (mut command, entry) = sot_log::test_isolated::test_command(name);
    command.env(CHILD_ENV, name).stdin(Stdio::null());
    #[allow(
        clippy::disallowed_methods,
        reason = "bounded fixture child through test_isolated::test_command, enter, drain and assert_once"
    )]
    // The child inherits this process's environment at the moment of the spawn. Sibling tests change it under
    // `ENV_TEST_LOCK` and restore it before releasing the lock, so the spawn takes the lock: it sees only restored
    // values, never another test's temporary HOME or julia override.
    let child = {
        let _env = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        command.spawn().expect("start isolated test body")
    };
    let pid = child.id();
    let (status, output, errors) = sot_log::test_isolated::drain(child).wait_within(bound);
    print!("{output}{errors}");
    entry.assert_once(pid);
    assert!(status.success(), "isolated test body {name} failed");
    false
}

/// A verified native executable on the search path; a missing one is a setup failure, never a skip. `julia` is the
/// one the daemon itself would run (`julia::resolve_bin`), never a bare search hit: on Windows the first `julia.exe`
/// on the search path can be an app-execution alias, which runs outside the fixture's containment.
pub(crate) fn executable(name: &str) -> PathBuf {
    if name == "julia" {
        let (bin, _) = crate::sidecars::julia::resolve_bin().unwrap_or_else(|e| panic!("setup: no real julia: {e}"));
        return PathBuf::from(bin);
    }
    let search = std::env::var_os("PATH").expect("setup: executable search unavailable");
    let filename = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    std::env::split_paths(&search)
        .map(|entry| entry.join(&filename))
        .find(|candidate| candidate.is_absolute() && candidate.is_file())
        .unwrap_or_else(|| panic!("setup: required native executable {name} missing"))
}

/// The MathJax helper's npm modules must be installed (`npm ci` in `rust/backend/sidecars/mathjax`); their absence is
/// a setup failure, never a skip.
pub(crate) fn require_mathjax_modules() {
    let modules = MathJax::default_script_path().with_file_name("node_modules").join("mathjax-full");
    assert!(
        modules.is_dir(),
        "setup: the MathJax helper's modules are missing at {}; run `npm ci` in rust/backend/sidecars/mathjax",
        modules.display()
    );
}

pub(crate) fn copy_folder(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("create owned fixture folder");
    for entry in std::fs::read_dir(source).expect("read fixture source") {
        let entry = entry.expect("fixture source entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("fixture source kind").is_dir() {
            copy_folder(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy fixture source");
        }
    }
}

/// An owned resource root holding a copy of the real `julia/repl` shim, so a package operation or a precompile
/// cannot touch the installed shim, and an owned depot. Points the process's `SOT_RESOURCE_ROOT`, `SOT_JULIA_BIN`
/// and `JULIA_DEPOT_PATH` at them: call only in an isolated body. The depot path ends in an empty entry, which Julia
/// expands to its own bundled depots, so the stdlibs the shim loads come precompiled from there; Julia writes to the
/// owned depot, the first entry, and of a bundled cache it loads updates only the timestamp.
pub(crate) fn owned_julia_env(root: &Path) -> PathBuf {
    let resources = root.join("resources");
    let shim = resources.join("julia").join("repl");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../julia/repl");
    copy_folder(&source.join("src"), &shim.join("src"));
    std::fs::copy(source.join("Project.toml"), shim.join("Project.toml"))
        .expect("copy shim project");
    std::env::set_var("SOT_RESOURCE_ROOT", &resources);
    std::env::set_var("SOT_JULIA_BIN", executable("julia"));
    std::env::set_var("JULIA_DEPOT_PATH", depot_path(&root.join("depot"), None));
    shim
}

/// The depot path of a fixture's Julia, the one builder every sidecar fixture uses: `owned` first (Julia writes only
/// there), then `read`, a `JULIA_DEPOT_PATH` list, whole and in its own order, as the daemon's children read it; with
/// no list, one empty entry, which Julia expands to its own bundled depots. A list is never extended: its depots were
/// built on it and hold the stdlib caches their caches need, and adding Julia's bundled depots would put a second copy
/// of each stdlib on the path, so which copy loads would turn on which one a sibling test touched last.
pub(crate) fn depot_path(owned: &Path, read: Option<&std::ffi::OsStr>) -> std::ffi::OsString {
    let entries: Vec<PathBuf> = match read {
        Some(list) => std::iter::once(owned.to_path_buf()).chain(std::env::split_paths(list)).collect(),
        None => vec![owned.to_path_buf(), PathBuf::new()],
    };
    std::env::join_paths(entries).expect("depot path")
}

#[test]
fn a_fixture_depot_path_keeps_a_read_list_whole() {
    let owned = PathBuf::from("owned");
    let entries = |path: std::ffi::OsString| std::env::split_paths(&path).collect::<Vec<_>>();
    let none = PathBuf::new();
    assert_eq!(entries(depot_path(&owned, None)), [owned.clone(), none.clone()]);
    // A bare list (a developer's depot) gains no bundled depots; a list that keeps them (CI's) keeps its one entry.
    let one = std::env::join_paths(["read"]).unwrap();
    assert_eq!(entries(depot_path(&owned, Some(&one))), [owned.clone(), "read".into()]);
    let list = std::env::join_paths(["a", "b", ""]).unwrap();
    assert_eq!(entries(depot_path(&owned, Some(&list))), [owned, "a".into(), "b".into(), none]);
}

/// Polls `condition` until it holds; the failure names `what`.
pub(crate) async fn within(bound: Duration, what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + bound;
    while !condition() {
        assert!(Instant::now() < deadline, "not within {bound:?}: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `SOT_JULIA_BIN` and `SOT_RESOURCE_ROOT` are process-global and read on every spawn attempt, so pinning them takes
/// the crate-wide env serialization lock (`paths::ENV_TEST_LOCK`); the previous values come back when the pin drops.
/// Unix only, as every test that pins them is.
#[cfg(unix)]
pub(crate) struct EnvPin {
    _serial: std::sync::MutexGuard<'static, ()>,
    julia_bin: Option<std::ffi::OsString>,
    resource_root: Option<std::ffi::OsString>,
}

#[cfg(unix)]
impl Drop for EnvPin {
    fn drop(&mut self) {
        for (key, val) in [("SOT_JULIA_BIN", &self.julia_bin), ("SOT_RESOURCE_ROOT", &self.resource_root)] {
            match val {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[cfg(unix)]
pub(crate) fn pin_env(julia_bin: &Path, resource_root: &Path) -> EnvPin {
    let serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pin = EnvPin {
        _serial: serial,
        julia_bin: std::env::var_os("SOT_JULIA_BIN"),
        resource_root: std::env::var_os("SOT_RESOURCE_ROOT"),
    };
    std::env::set_var("SOT_JULIA_BIN", julia_bin);
    std::env::set_var("SOT_RESOURCE_ROOT", resource_root);
    pin
}

fn private_signal() -> &'static Signal {
    Box::leak(Box::new(Signal::new()))
}

/// A real eval completes under the REPL's own Signal; firing only that Signal closes the supervisor out and reaps
/// its child, and the REPL then refuses to start another.
#[tokio::test]
async fn repl_uses_private_signal() {
    if !isolated(
        "sidecars::contract_tests::repl_uses_private_signal",
        Duration::from_secs(150),
    ) {
        return;
    }
    let root = tempfile::tempdir().expect("owned fixture root").keep();
    owned_julia_env(&root);
    let sig = private_signal();
    let (frames, _keep) = broadcast::channel(64);
    let workspace = root.join("workspace");
    std::fs::create_dir(&workspace).expect("create the workspace");
    let repl = Repl::new(frames, Some("ws".into()), workspace, sig);

    let (reply, collector) = repl
        .execute(
            "repl.eval",
            serde_json::json!({ "code": "40 + 2", "eval_id": 71 }),
        )
        .await
        .expect("the first start must succeed under the supplied Signal");
    tokio::time::timeout(Duration::from_secs(120), reply)
        .await
        .expect("the eval must answer")
        .expect("the supervisor must not drop the reply")
        .expect("the eval must succeed");
    let value = collector
        .lock()
        .expect("collector")
        .frames
        .iter()
        .any(|f| f["kind"] == "value" && f["text"] == "42");
    assert!(value, "the real eval must return its value");
    assert_eq!(sig.live(), 1, "the supplied Signal owns the child");

    sig.fire().expect("fire");
    within(
        Duration::from_secs(60),
        "the fired Signal's child is reaped and released",
        || sig.live() == 0,
    )
    .await;
    within(
        Duration::from_secs(10),
        "the REPL reports its child dead",
        || repl.state() == ReplLifecycle::Dead,
    )
    .await;
    let refused = repl
        .execute(
            "repl.eval",
            serde_json::json!({ "code": "1", "eval_id": 72 }),
        )
        .await;
    assert!(refused.is_err(), "a fired Signal must refuse another start");
    assert_eq!(sig.live(), 0, "the refused start created no child");
    println!("D-L repl private Signal PASS");
    std::fs::remove_dir_all(&root).expect("remove fixture only after the child is reaped");
}

/// The same for MathJax.
#[tokio::test]
async fn mathjax_uses_private_signal() {
    if !isolated(
        "sidecars::contract_tests::mathjax_uses_private_signal",
        Duration::from_secs(60),
    ) {
        return;
    }
    let sig = private_signal();
    // The node binary is read at construction.
    require_mathjax_modules();
    std::env::set_var("SOT_NODE_BIN", executable("node"));
    let mathjax = MathJax::new(MathJax::default_script_path(), sig);
    let svg = tokio::time::timeout(Duration::from_secs(30), mathjax.render("x^2", false))
        .await
        .expect("the render must answer")
        .expect("the render must succeed");
    assert!(
        String::from_utf8_lossy(&svg.svg).contains("<svg"),
        "a real SVG comes back"
    );
    assert_eq!(sig.live(), 1, "the supplied Signal owns the child");

    sig.fire().expect("fire");
    within(
        Duration::from_secs(30),
        "the fired Signal's child is reaped and released",
        || sig.live() == 0,
    )
    .await;
    let refused = mathjax.render("y", false).await;
    assert!(refused.is_err(), "a fired Signal must refuse another start");
    assert_eq!(sig.live(), 0, "the refused start created no child");
    println!("D-L mathjax private Signal PASS");
}

/// Linux: the listening TCP and bound UDP sockets, as `(protocol, port)`, of every process in process group `group`
/// (a contained child leads its group, so this is its whole tree). A socket belongs to a process when the process
/// holds its inode open.
#[cfg(target_os = "linux")]
pub(crate) fn group_listeners(group: i32) -> (usize, std::collections::BTreeSet<(&'static str, u16)>) {
    let pgrp = |pid: &str| -> Option<i32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat[stat.rfind(')')? + 2..].split_whitespace().nth(2)?.parse().ok()
    };
    let mut inodes = std::collections::BTreeSet::new();
    let mut processes = 0;
    for entry in std::fs::read_dir("/proc").expect("read the process table").flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.bytes().all(|b| b.is_ascii_digit()) || pgrp(&name) != Some(group) {
            continue;
        }
        processes += 1;
        for fd in std::fs::read_dir(format!("/proc/{name}/fd")).into_iter().flatten().flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                if let Some(inode) = target.to_string_lossy().strip_prefix("socket:[").and_then(|t| t.strip_suffix(']')) {
                    inodes.insert(inode.to_owned());
                }
            }
        }
    }
    let mut found = std::collections::BTreeSet::new();
    for (file, protocol, state) in [
        ("tcp", "tcp", "0A"),
        ("tcp6", "tcp", "0A"),
        ("udp", "udp", "07"),
        ("udp6", "udp", "07"),
    ] {
        let table = std::fs::read_to_string(format!("/proc/net/{file}")).unwrap_or_default();
        for line in table.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() > 9 && f[3] == state && inodes.contains(f[9]) {
                if let Some(port) = f[1].rsplit(':').next().and_then(|p| u16::from_str_radix(p, 16).ok()) {
                    found.insert((protocol, port));
                }
            }
        }
    }
    (processes, found)
}

/// The MathJax helper, started and put to work through its production path, listens nowhere in its whole tree.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn mathjax_helper_tree_listens_nowhere() {
    if !isolated(
        "sidecars::contract_tests::mathjax_helper_tree_listens_nowhere",
        Duration::from_secs(60),
    ) {
        return;
    }
    let sig = private_signal();
    require_mathjax_modules();
    std::env::set_var("SOT_NODE_BIN", executable("node"));
    let mathjax = MathJax::new(MathJax::default_script_path(), sig);
    tokio::time::timeout(Duration::from_secs(30), mathjax.render("x^2", false))
        .await
        .expect("the render must answer")
        .expect("the render must succeed");
    let groups = sig.held_groups();
    assert_eq!(groups.len(), 1, "the helper is alive and owned");
    let (processes, listeners) = group_listeners(groups[0]);
    assert!(processes >= 1, "setup: the observer sees the helper's process");
    assert!(listeners.is_empty(), "the helper's tree must listen nowhere: {listeners:?}");
    sig.fire().expect("fire");
    within(Duration::from_secs(30), "the helper is reaped", || sig.live() == 0).await;
}

/// The same observer and the same empty-set comparison reject a node tree that does listen: the listener is
/// functional, and the comparison fails on it.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn listener_observer_rejects_a_listening_node_tree() {
    if !isolated(
        "sidecars::contract_tests::listener_observer_rejects_a_listening_node_tree",
        Duration::from_secs(60),
    ) {
        return;
    }
    let sig = private_signal();
    let mut command = tokio::process::Command::new(executable("node"));
    command
        .args([
            "-e",
            "const s = require('net').createServer().listen(0, '127.0.0.1', () => { console.log(s.address().port); setTimeout(() => {}, 60000); });",
        ])
        .stdout(Stdio::piped());
    let mut owned = sig.spawn(&mut command).expect("start the owned listener");
    let mut out = tokio::io::BufReader::new(owned.stdout.take().expect("stdout"));
    let mut line = String::new();
    tokio::time::timeout(
        Duration::from_secs(30),
        tokio::io::AsyncBufReadExt::read_line(&mut out, &mut line),
    )
    .await
    .expect("the listener reports its port")
    .expect("read the port");
    let port: u16 = line.trim().parse().expect("port");
    let group = sig.held_groups()[0];
    let (_, listeners) = group_listeners(group);
    assert!(
        listeners != std::collections::BTreeSet::new(),
        "the empty-set comparison must reject a listening tree"
    );
    assert!(listeners.contains(&("tcp", port)), "the observer sees the declared listener: {listeners:?}");
    sig.fire().expect("fire");
    let _ = owned.kill().await;
    drop(owned);
    within(Duration::from_secs(30), "the listener is reaped", || sig.live() == 0).await;
}
