//! Real children, owned fixtures and a private Signal: a restart retires and joins the old supervisor before the new
//! child opens, a death respawn keeps the selected project, and every submission sender closing still closes the
//! streamed runs out.

use std::path::PathBuf;
#[cfg(unix)]
use std::path::Path;
use std::time::Duration;

#[cfg(unix)]
use serde_json::json;
use tokio::sync::broadcast;

#[cfg(unix)]
use super::project_tests::{eval, raw, stdout_of};
use super::*;
use crate::lifecycle::child_signal::Signal;
use crate::sidecars::contract_tests::{executable, isolated, owned_julia_env, within};

/// A first start compiles the shim in an empty depot, which several parallel tests slow further.
#[cfg_attr(windows, allow(dead_code, reason = "used by the Unix-only tests of this file"))]
const START: Duration = Duration::from_secs(240);
const BODY: Duration = Duration::from_secs(600);

/// A shim that announces itself and then reads requests forever, answering none.
const IDLE_SHIM: &str = r#"module ShipToolsRepl
function serve(i, o)
    println(o, "{\"v\":1,\"id\":0,\"kind\":\"evt\",\"op\":\"repl.ready\",\"payload\":{}}")
    flush(o)
    while !eof(i)
        readline(i)
    end
end
end
"#;

fn private_signal() -> &'static Signal {
    Box::leak(Box::new(Signal::new()))
}

/// Whether a process with this id still exists; a zombie counts, so false means the child was reaped.
#[cfg(unix)]
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only probes.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg_attr(windows, allow(dead_code, reason = "used by the Unix-only tests of this file"))]
struct Fixture {
    root: PathBuf,
    first: PathBuf,
    second: PathBuf,
    sig: &'static Signal,
    repl: Repl,
    frames: broadcast::Receiver<ReplFrameMsg>,
}

impl Fixture {
    /// A fixture whose REPL child is a minimal owned shim that says ready and then idles: it starts in well under a
    /// second on every platform, and its spawns are counted by the lifecycle generation.
    fn idle() -> Self {
        let fx = Fixture::new(false);
        let shim = fx.root.join("resources").join("julia").join("repl");
        std::fs::create_dir_all(shim.join("src")).unwrap();
        std::fs::write(
            shim.join("Project.toml"),
            "name = \"ShipToolsRepl\"\nuuid = \"5c3a7a6e-5b0a-4b44-9d0e-2f2a6b0f7c11\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(shim.join("src").join("ShipToolsRepl.jl"), IDLE_SHIM).unwrap();
        std::env::set_var("SOT_RESOURCE_ROOT", fx.root.join("resources"));
        std::env::set_var("SOT_JULIA_BIN", executable("julia"));
        std::env::set_var("JULIA_DEPOT_PATH", fx.root.join("depot"));
        fx
    }

    /// How many children this REPL has started.
    fn spawns(&self) -> u64 {
        self.repl.inner.lifecycle.lock().unwrap().gen
    }

    fn new(julia: bool) -> Self {
        let root = tempfile::tempdir().expect("owned fixture root").keep();
        if julia {
            owned_julia_env(&root);
        }
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir(&first).expect("create first project");
        std::fs::create_dir(&second).expect("create second project");
        let sig = private_signal();
        let (tx, frames) = broadcast::channel(256);
        let repl = Repl::new(tx, Some("ws-restart".into()), first.clone(), sig);
        Fixture {
            root,
            first,
            second,
            sig,
            repl,
            frames,
        }
    }

    /// Ends every child this fixture started, then removes only its own root.
    async fn finish(self) {
        self.sig.fire().expect("fire");
        within(Duration::from_secs(60), "owned children reaped", || {
            self.sig.live() == 0
        })
        .await;
        std::fs::remove_dir_all(&self.root).expect("remove the owned fixture root");
    }
}

#[cfg(unix)]
async fn pid_of(repl: &Repl, id: u64) -> u32 {
    stdout_of(&eval(repl, id, "println(getpid())").await)
        .parse()
        .expect("child pid")
}

#[cfg(unix)]
async fn real_cwd(repl: &Repl, id: u64) -> String {
    stdout_of(&eval(repl, id, "println(realpath(pwd()))").await)
}

#[cfg(unix)]
fn real(path: &Path) -> String {
    std::fs::canonicalize(path)
        .expect("canonical path")
        .to_string_lossy()
        .into_owned()
}

/// A restart stops the old supervisor whatever clones of its sender exist and however busy Julia is, and the old
/// child is reaped before the call returns and before the new child answers.
#[cfg(unix)]
#[tokio::test]
async fn restart_reaps_before_replacement() {
    if !isolated("sidecars::repl::restart_tests::restart_reaps_before_replacement", BODY) {
        return;
    }
    let fx = Fixture::new(true);
    let held = fx.repl.ensure_supervisor().await.expect("start");
    let old = pid_of(&fx.repl, 1).await;
    let marker = fx.root.join("spinning");
    let spin = format!("write({}, \"x\"); while true end", raw(&marker.to_string_lossy()));
    fx.repl
        .submit("repl.eval", json!({ "code": spin, "eval_id": 2 }))
        .await
        .expect("submit the spin");
    within(START, "the spin starts", || marker.exists()).await;
    assert!(alive(old), "setup: the old child is running");

    fx.repl
        .restart_with_project(&fx.second)
        .await
        .expect("restart");
    assert!(!alive(old), "the old child must be reaped before restart returns");
    assert!(held.is_closed(), "the old supervisor must be gone with its sender clone still held");
    let new = pid_of(&fx.repl, 3).await;
    assert_ne!(old, new, "the replacement is a new child");
    assert_eq!(real_cwd(&fx.repl, 4).await, real(&fx.second));
    fx.finish().await;
}

/// The project a restart selected is the one a death respawn starts in.
#[cfg(unix)]
#[tokio::test]
async fn selected_project_survives_death() {
    if !isolated("sidecars::repl::restart_tests::selected_project_survives_death", BODY) {
        return;
    }
    let fx = Fixture::new(true);
    assert_eq!(real_cwd(&fx.repl, 1).await, real(&fx.first));
    fx.repl
        .restart_with_project(&fx.second)
        .await
        .expect("restart");
    assert_eq!(real_cwd(&fx.repl, 2).await, real(&fx.second));
    let _ = fx
        .repl
        .execute("repl.eval", json!({ "code": "exit(0)", "eval_id": 3 }))
        .await
        .expect("submit the exit");
    within(START, "the child dies", || {
        fx.repl.state() == lifecycle::ReplLifecycle::Dead
    })
    .await;
    assert_eq!(
        real_cwd(&fx.repl, 4).await,
        real(&fx.second),
        "the respawn must keep the selected project"
    );
    fx.finish().await;
}

/// Every sender gone while a streamed eval is outstanding still ends the run: terminal error, then done.
#[cfg(unix)]
#[tokio::test]
async fn channel_close_finishes_streamed_runs() {
    if !isolated("sidecars::repl::restart_tests::channel_close_finishes_streamed_runs", BODY) {
        return;
    }
    let mut fx = Fixture::new(true);
    let held = fx.repl.ensure_supervisor().await.expect("start");
    fx.repl
        .submit(
            "repl.eval",
            json!({ "code": "println(\"waiting\"); flush(stdout); sleep(120)", "eval_id": 5 }),
        )
        .await
        .expect("submit");
    loop {
        let msg = tokio::time::timeout(START, fx.frames.recv())
            .await
            .expect("the eval starts")
            .expect("frame bus");
        if msg.eval_id == 5 && msg.frame["kind"] == "stdout" {
            break;
        }
    }
    let sig = fx.sig;
    let root = fx.root.clone();
    let mut frames = fx.frames;
    drop(held);
    drop(fx.repl);
    let mut kinds = Vec::new();
    loop {
        let msg = tokio::time::timeout(START, frames.recv())
            .await
            .expect("the run must be closed out")
            .expect("frame bus");
        if msg.eval_id == 5 {
            let kind = msg.frame["kind"].as_str().unwrap_or("").to_owned();
            kinds.push(kind.clone());
            if kind == "done" {
                break;
            }
        }
    }
    assert_eq!(kinds.last().map(String::as_str), Some("done"));
    assert!(kinds.contains(&"error".to_owned()), "an error precedes the done: {kinds:?}");
    sig.fire().expect("fire");
    within(Duration::from_secs(60), "owned children reaped", || sig.live() == 0).await;
    std::fs::remove_dir_all(&root).expect("remove the owned fixture root");
}

/// A stand-in `julia` whose first request is answered, two seconds later, by a browser frame.
#[cfg(unix)]
const LATE_BROWSER_STUB: &str = "#!/bin/sh\nwhile IFS= read -r line; do\n  sleep 2\n  printf '%s\\n' '{\"id\":0,\"kind\":\"evt\",\"op\":\"repl.frame\",\"payload\":{\"eval_id\":7,\"frame\":{\"kind\":\"browser\",\"url\":\"http://127.0.0.1:45611/page\"}}}'\ndone\n";

/// A generation retired by a restart cannot grant a browser port after its replacement has opened.
#[cfg(unix)]
#[tokio::test]
async fn retired_generation_cannot_grant() {
    if !isolated("sidecars::repl::restart_tests::retired_generation_cannot_grant", BODY) {
        return;
    }
    let fx = Fixture::new(false);
    let stub = fx.root.join("julia");
    sot_log::test_exec::write_executable(&stub, LATE_BROWSER_STUB.to_owned());
    let resources = fx.root.join("resources");
    std::fs::create_dir_all(resources.join("julia").join("repl")).expect("create resources");
    std::env::set_var("SOT_RESOURCE_ROOT", &resources);
    std::env::set_var("SOT_JULIA_BIN", &stub);

    let held = fx.repl.ensure_supervisor().await.expect("start");
    fx.repl
        .submit("repl.eval", json!({ "code": "x", "eval_id": 7 }))
        .await
        .expect("submit");
    fx.repl
        .restart_with_project(&fx.second)
        .await
        .expect("restart");
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        !crate::pages::proxy::allowed_proxy_ports().contains(&45611),
        "a retired generation's browser port must not be granted"
    );
    drop(held);
    fx.finish().await;
}

/// A retirement that does not finish within `REPL_RESTART_WAIT` is returned as an error, stays owned and starts no
/// replacement; once it finishes, the next restart reconciles it and starts the replacement.
#[tokio::test]
async fn restart_timeout_retains_owner_and_starts_no_replacement() {
    if !isolated("sidecars::repl::restart_tests::restart_timeout_retains_owner_and_starts_no_replacement", BODY) {
        return;
    }
    let fx = Fixture::idle();
    fx.repl.ensure_supervisor().await.expect("start");
    assert_eq!(fx.spawns(), 1);
    let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    *super::supervisor::seams::CLOSEOUT_GATE.lock().unwrap() = Some(gate.clone());

    let err = fx.repl.restart_with_project(&fx.second).await.expect_err("the join is delayed past the wait");
    assert!(err.to_string().contains("not finished"), "{err:#}");
    assert_eq!(fx.spawns(), 1, "a timed-out retirement must start no replacement");

    gate.add_permits(1);
    fx.repl.restart_with_project(&fx.second).await.expect("the retained owner is reconciled");
    assert_eq!(fx.spawns(), 2, "the replacement starts once the retirement finished");
    fx.finish().await;
}

/// A retirement whose termination check fails is returned as an error and starts no replacement; the failure is
/// reported once, and the next restart goes on.
#[tokio::test]
async fn cleanup_error_prevents_replacement() {
    if !isolated("sidecars::repl::restart_tests::cleanup_error_prevents_replacement", BODY) {
        return;
    }
    let fx = Fixture::idle();
    fx.repl.ensure_supervisor().await.expect("start");
    super::supervisor::seams::FAIL_RETIREMENT.store(true, std::sync::atomic::Ordering::SeqCst);

    let err = fx.repl.restart_with_project(&fx.second).await.expect_err("the retirement failed");
    assert!(err.to_string().contains("retirement failed"), "{err:#}");
    assert_eq!(fx.spawns(), 1, "a failed retirement must start no replacement");

    fx.repl.restart_with_project(&fx.second).await.expect("the failure was reported once");
    assert_eq!(fx.spawns(), 2);
    fx.finish().await;
}
