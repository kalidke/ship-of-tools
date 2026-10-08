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
    let child = command.spawn().expect("start isolated test body");
    let pid = child.id();
    let (status, output, errors) = sot_log::test_isolated::drain(child).wait_within(bound);
    print!("{output}{errors}");
    entry.assert_once(pid);
    assert!(status.success(), "isolated test body {name} failed");
    false
}

/// A verified native executable on the search path; a missing one is a setup failure, never a skip.
pub(crate) fn executable(name: &str) -> PathBuf {
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
/// and `JULIA_DEPOT_PATH` at them: call only in an isolated body.
pub(crate) fn owned_julia_env(root: &Path) -> PathBuf {
    let resources = root.join("resources");
    let shim = resources.join("julia").join("repl");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../julia/repl");
    copy_folder(&source.join("src"), &shim.join("src"));
    std::fs::copy(source.join("Project.toml"), shim.join("Project.toml"))
        .expect("copy shim project");
    std::env::set_var("SOT_RESOURCE_ROOT", &resources);
    std::env::set_var("SOT_JULIA_BIN", executable("julia"));
    std::env::set_var("JULIA_DEPOT_PATH", root.join("depot"));
    shim
}

/// Polls `condition` until it holds; the failure names `what`.
pub(crate) async fn within(bound: Duration, what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + bound;
    while !condition() {
        assert!(Instant::now() < deadline, "not within {bound:?}: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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
    let repl = Repl::new(frames, Some("ws".into()), None, sig);

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

    sig.fire();
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

    sig.fire();
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
