//! The product worker factories, read where they stand and not from their source: a Julia `Distributed` worker (the
//! worker Pluto's notebooks run in, here by the same `addprocs` call Malt's `DistributedStdlibWorker` makes) against
//! the process that started it. Pluto reaches it through `Malt.DistributedStdlibWorker`, whose constructor evaluates
//! `Distributed.addprocs(1; exeflags=...)`; `LocalManager` starts the worker with `open(detach(cmd), "r+")`, and a
//! detached process is started in a session of its own. The case records that, so the lane's ephemeral containment
//! does not assume a worker stays in the group of the process that launched it.
//!
//! Needs a Julia 1.12 (`SOT_JULIA_BIN`, else `julia` on the PATH), so it is `#[ignore]` in a plain `cargo test` and
//! run by the harness job (`-- --ignored`); with no Julia it fails and never passes by skipping.

use crate::fixture_owner::Fixture;
use std::process::{Command, Stdio};

/// What the script prints: the master's and the worker's pid, session and group, one `who pid sid pgid` line each.
const SCRIPT: &str = r#"
using Distributed
sid(p) = ccall(:getsid, Cint, (Cint,), p)
pgid(p) = ccall(:getpgid, Cint, (Cint,), p)
worker = addprocs(1)[1]
wpid = remotecall_fetch(getpid, worker)
println("master ", getpid(), " ", sid(0), " ", pgid(0))
println("worker ", wpid, " ", sid(wpid), " ", pgid(wpid))
rmprocs(worker)
"#;

#[test]
#[ignore = "needs a Julia 1.12 (SOT_JULIA_BIN, else julia); run by the harness job with --ignored"]
fn a_distributed_worker_starts_in_a_session_of_its_own() {
    let julia = std::env::var_os("SOT_JULIA_BIN").unwrap_or_else(|| "julia".into());
    let mut fx = Fixture::new("distributed_worker_session");
    let master = Command::new(julia)
        .args(["--startup-file=no", "-e", SCRIPT])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start julia");
    // Authority over the master first; the worker is learned from what the master reports and only watched.
    fx.own(master.id() as i32, "julia master")
        .expect("an identity for the master");
    let out = master.wait_with_output().expect("the script ends");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "the script failed: {}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let row = |who: &str| -> Vec<i32> {
        text.lines()
            .find_map(|line| line.strip_prefix(who)?.strip_prefix(' '))
            .unwrap_or_else(|| panic!("no {who} line in:\n{text}"))
            .split_whitespace()
            .map(|n| n.parse().unwrap())
            .collect()
    };
    let (master_row, worker_row) = (row("master"), row("worker"));
    fx.save("master", format!("{master_row:?}"));
    fx.save("worker", format!("{worker_row:?}"));
    let cleanup = fx.cleanup();
    assert!(cleanup.complete(), "{cleanup:?}");
    let (wpid, wsid, wpgid) = (worker_row[0], worker_row[1], worker_row[2]);
    assert_eq!(
        (wsid, wpgid),
        (wpid, wpid),
        "the worker is not its own session leader: {worker_row:?} (master {master_row:?})"
    );
    assert_ne!(
        wsid, master_row[1],
        "the worker shares the master's session: {worker_row:?} (master {master_row:?})"
    );
}

/// Pluto's notebook worker, started detached in a session of its own, is outside the process group the fire kills; on
/// Linux the guard's drain ends it. One notebook opened through the daemon's real Pluto supervisor, an ordinary worker
/// that only reports its pid, then the window's close: the daemon exits 0 and the worker has ended by the time the guard
/// has.
#[cfg(all(target_os = "linux", feature = "daemon-lifetime-faults"))]
#[tokio::test]
#[ignore = "needs Julia 1.12 and Pluto's environment (SOT_JULIA_BIN, SOT_L2_PLUTO_MANIFEST); run by the harness job with --ignored"]
async fn a_closed_daemon_ends_plutos_notebook_worker() {
    use crate::done::{open_notebook, pluto_resource_root, read_pid, Wire};
    use crate::guard::{close_by_lease, Run};
    use std::time::Duration;
    let _serial = crate::SERIAL.lock().await;
    let mut fx = Fixture::new("a_closed_daemon_ends_plutos_notebook_worker");
    let case = tempfile::tempdir().expect("the case's folder");
    let julia = crate::routes::julia_bin();
    let resources = pluto_resource_root(case.path(), &julia);
    let report = case.path().join("worker.pid");
    let startup = format!("write(\"{}\", string(getpid()))", report.display());
    let env = [
        ("SOT_JULIA_BIN", julia.as_str()),
        (
            "SOT_RESOURCE_ROOT",
            resources.to_str().expect("a UTF-8 case folder"),
        ),
        ("SOT_L2_WORKER_STARTUP", startup.as_str()),
    ];
    let mut run = Run::start("gplworker", &env, false).await;
    let mut wire = Wire::connect(&run).await;
    // The default row's root, where the daemon was started: the one row this case needs.
    open_notebook(&run.env.daemon_project_root, &mut wire).await;
    let worker = crate::support::poll_until(
        || async { read_pid(report.clone()) },
        Duration::from_secs(120),
        "Pluto's worker to report",
    )
    .await;
    let id = fx
        .watch(worker, None, "Pluto's worker")
        .expect("an identity for the worker that reported itself");
    // SAFETY: plain reads of a process's session and group; nothing is signalled.
    let (sid, pgid) = unsafe { (libc::getsid(worker), libc::getpgid(worker)) };
    close_by_lease(&run.env).await;
    let status = run.status_within(Duration::from_secs(60)).await;
    let ended = fx.identity(id).exited(Duration::from_secs(1));
    let said = run.said();
    fx.save("worker_ended", ended);
    let cleanup = fx.cleanup();
    assert!(cleanup.complete(), "{cleanup:?}");
    assert_eq!(
        (sid, pgid),
        (worker, worker),
        "the worker is not in a session and group of its own, so the case tests nothing"
    );
    assert_eq!(
        status.and_then(|s| s.code()),
        Some(0),
        "the close: {status:?}\n{said}"
    );
    assert!(
        ended,
        "Pluto's notebook worker outlived its closed daemon and the guard:\n{said}"
    );
}

#[cfg(all(target_os = "linux", feature = "daemon-lifetime-faults"))]
#[tokio::test]
#[ignore = "needs Julia 1.12 and Pluto's environment (SOT_JULIA_BIN, SOT_L2_PLUTO_MANIFEST) and, for its Quarto half, Quarto 1.7.31; run by the harness job with --ignored"]
async fn every_descendant_of_a_killed_daemon_ends() {
    use crate::done::{
        assert_oracle, hold_pluto, hold_quarto, hold_row, save_oracle, save_successor,
        supervisor_of, Held, Inputs, Wire,
    };
    use crate::guard::Run;
    let _serial = crate::SERIAL.lock().await;
    let mut fx = Fixture::new("every_descendant_of_a_killed_daemon_ends");
    let inputs = Inputs::new();
    let env = inputs.env_pairs();
    let mut run = Run::start("gdone", &env, false).await;
    let mut wire = Wire::connect(&run).await;
    let mut held = Held::default();

    hold_row(&mut fx, &run, &mut wire, &inputs, &mut held).await;
    hold_pluto(&mut fx, &run, &mut wire, &inputs, &mut held).await;
    hold_quarto(&mut fx, &run, &mut wire, &inputs, &mut held).await;
    let supervisor = supervisor_of(&run, &held.state_dir)
        .await
        .expect("the capsule's supervisor answers");
    let supervisor_id = fx
        .watch(supervisor.0, Some(supervisor.1), "the capsule's supervisor")
        .expect("an identity for the reported supervisor");

    // The stimulus: SIGKILL, which the daemon sends itself on the case's word (the case never signals a pid it did not
    // get back from its own spawn).
    if let Some(spin) = held.repl_spin.take() {
        spin.abort();
    }
    run.daemon_does("raise:9");
    let status = run.status_within(std::time::Duration::from_secs(60)).await;

    fx.save("launched_status", format!("{status:?}"));
    save_oracle(&mut fx, &held, supervisor_id);
    save_successor(&mut fx, &mut run, &env, &held, supervisor).await;
    let said = run.said();
    // The capsule's agent tree stays by design (it ignores TERM and HUP and runs in the row's scope): the successor's
    // close, through the product, ends the row and with it the tree.
    run.end_capsules().await;
    let cleanup = fx.cleanup();
    assert!(cleanup.complete(), "{cleanup:?}");
    assert_oracle(&fx, status, &said);
}
