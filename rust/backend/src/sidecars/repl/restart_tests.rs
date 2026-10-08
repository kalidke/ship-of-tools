//! Real children, owned fixtures and a private Signal: a restart retires and joins the old supervisor before the new
//! child opens, a death respawn keeps the selected project, and every submission sender closing still closes the
//! streamed runs out.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use tokio::sync::broadcast;

use super::project_tests::{eval, raw, stdout_of};
use super::*;
use crate::lifecycle::child_signal::Signal;
use crate::sidecars::contract_tests::{isolated, owned_julia_env, within};

/// A first start compiles the shim in an empty depot, which several parallel tests slow further.
const START: Duration = Duration::from_secs(240);
const BODY: Duration = Duration::from_secs(600);

fn private_signal() -> &'static Signal {
    Box::leak(Box::new(Signal::new()))
}

/// Whether a process with this id still exists; a zombie counts, so false means the child was reaped.
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only probes.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

struct Fixture {
    root: PathBuf,
    first: PathBuf,
    second: PathBuf,
    sig: &'static Signal,
    repl: Repl,
    frames: broadcast::Receiver<ReplFrameMsg>,
}

impl Fixture {
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
        self.sig.fire();
        within(Duration::from_secs(60), "owned children reaped", || {
            self.sig.live() == 0
        })
        .await;
        std::fs::remove_dir_all(&self.root).expect("remove the owned fixture root");
    }
}

async fn pid_of(repl: &Repl, id: u64) -> u32 {
    stdout_of(&eval(repl, id, "println(getpid())").await)
        .parse()
        .expect("child pid")
}

async fn real_cwd(repl: &Repl, id: u64) -> String {
    stdout_of(&eval(repl, id, "println(realpath(pwd()))").await)
}

fn real(path: &Path) -> String {
    std::fs::canonicalize(path)
        .expect("canonical path")
        .to_string_lossy()
        .into_owned()
}

/// A restart stops the old supervisor whatever clones of its sender exist and however busy Julia is, and the old
/// child is reaped before the call returns and before the new child answers.
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
    sig.fire();
    within(Duration::from_secs(60), "owned children reaped", || sig.live() == 0).await;
    std::fs::remove_dir_all(&root).expect("remove the owned fixture root");
}

/// A stand-in `julia` whose first request is answered, two seconds later, by a browser frame.
const LATE_BROWSER_STUB: &str = "#!/bin/sh\nwhile IFS= read -r line; do\n  sleep 2\n  printf '%s\\n' '{\"id\":0,\"kind\":\"evt\",\"op\":\"repl.frame\",\"payload\":{\"eval_id\":7,\"frame\":{\"kind\":\"browser\",\"url\":\"http://127.0.0.1:45611/page\"}}}'\ndone\n";

/// A generation retired by a restart cannot grant a browser port after its replacement has opened.
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
