//! Ephemerals started through the daemon's real routes, and their end: a ready row whose REPL has started a tree detached
//! (spinning or returned), the drain that outlasts a forking child and the guard that ends only its own subtree. Every
//! process these cases end is one this test spawned (the launched guard); the tree's processes are watched.

use crate::fixture_owner::Fixture;
use crate::guard::Run;
use crate::routes::{all_ended, julia_bin, ready_row, spin_in_repl, supervisor_in, watch_tree};
use crate::support::{connect_and_hello, Env};
use crate::tree::{session_members, Tree};
use crate::SERIAL;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// A daemon with a ready row whose REPL has started a tree detached and spins; the tree's identities are held.
pub struct Spinning {
    pub run: Run,
    pub state_dir: PathBuf,
    pub ids: Vec<usize>,
    pub task: tokio::task::JoinHandle<()>,
}

pub async fn start_spinning(tag: &str, fx: &mut Fixture, forking: bool) -> Spinning {
    start_spinning_with(tag, fx, forking, &[]).await
}

/// [`start_spinning`] with more daemon variables.
pub async fn start_spinning_with(
    tag: &str,
    fx: &mut Fixture,
    forking: bool,
    extra: &[(&str, &str)],
) -> Spinning {
    start_spinning_at(tag, fx, forking, extra, None, true).await
}

/// [`start_spinning_with`] for the daemon binary at `program` (a copy in an install-shaped folder) when it is given. With
/// `spin` false the REPL's cell starts the tree and returns, so the REPL is idle and no request stays open on the daemon.
pub async fn start_spinning_at(
    tag: &str,
    fx: &mut Fixture,
    forking: bool,
    extra: &[(&str, &str)],
    program: Option<&Path>,
    spin: bool,
) -> Spinning {
    let julia = julia_bin();
    let mut vars = vec![("SOT_JULIA_BIN", julia.as_str())];
    vars.extend_from_slice(extra);
    let run = match program {
        Some(program) => Run::boot_at(Env::new(tag), program, &vars).await,
        None => Run::start(tag, &vars, false).await,
    };
    run.assert_guarded();
    let (mut conn, mut next_id) = connect_and_hello(&run.env.socket_path).await;
    let (workspace_id, state_dir) = ready_row(&run.env, &mut conn, &mut next_id, "repl").await;
    drop(conn);
    let tree = Tree::new(run.env._tmp.path(), "repl-tree");
    let cell = if spin {
        tree.julia_cell(forking)
    } else {
        tree.julia_cell_returning(forking)
    };
    let mut task = spin_in_repl(&run.env.socket_path, &workspace_id, cell).await;
    let ids = watch_tree(fx, &tree, forking, "the REPL's").await;
    if !spin {
        // The cell returns once it has started the tree: the request ends and its connection with it.
        tokio::time::timeout(Duration::from_secs(60), &mut task)
            .await
            .expect("the cell did not return")
            .expect("the cell's task");
    }
    Spinning {
        run,
        state_dir,
        ids,
        task,
    }
}

#[tokio::test]
#[ignore = "needs a Julia 1.12 (SOT_JULIA_BIN, else julia); run by the harness job with --ignored"]
async fn the_drain_outlasts_a_forking_child() {
    let _serial = SERIAL.lock().await;
    let mut fx = Fixture::new("drain_outlasts_a_forking_child");
    let spinning = start_spinning("gdrn", &mut fx, true).await;
    let leader = fx.identity(spinning.ids[0]).pid;
    let alive_before = session_members(leader).len();
    let daemon = fx
        .watch(spinning.run.daemon, None, "the daemon")
        .expect("an identity for the daemon");
    spinning.task.abort();
    spinning.run.daemon_does("raise:9");
    fx.save(
        "leader_gone",
        all_ended(&fx, &spinning.ids, Duration::from_secs(10)),
    );
    // Every process of the leader's session, young children included.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !session_members(leader).is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    fx.save("session_left", format!("{:?}", session_members(leader)));
    let cleanup = fx.cleanup();
    assert!(cleanup.complete(), "{cleanup:?}");
    assert!(alive_before >= 1, "the forking tree never ran");
    assert_eq!(
        fx.saved("leader_gone"),
        Some("true"),
        "the forking tree's leader outlived the daemon"
    );
    assert_eq!(
        fx.saved("session_left"),
        Some("[]"),
        "processes of the forking tree's session outlived the daemon"
    );
}

#[tokio::test]
#[ignore = "needs a Julia 1.12 (SOT_JULIA_BIN, else julia); run by the harness job with --ignored"]
async fn the_guard_ends_only_its_own_subtree() {
    let _serial = SERIAL.lock().await;
    let mut fx = Fixture::new("guard_ends_only_its_own_subtree");
    let mut a = start_spinning("gsua", &mut fx, false).await;
    let b = start_spinning("gsub", &mut fx, false).await;
    // A process the case starts outside both daemons: a child of this test, ended through its own handle.
    let mut outside = std::process::Command::new("sleep")
        .arg("3160")
        .stdin(Stdio::null())
        .spawn()
        .expect("start the outside process");
    // The capsule's supervisor, as daemon A's own lane reports it (the lane is found through A's runtime folder).
    let supervisor = supervisor_in(&a.run.env, &a.state_dir)
        .await
        .expect("daemon A's supervisor answers");
    let capsule = fx
        .watch(
            supervisor.0,
            Some(supervisor.1),
            "daemon A's capsule supervisor",
        )
        .expect("an identity for the reported supervisor");

    let daemon = fx
        .watch(a.run.daemon, None, "daemon A")
        .expect("an identity for daemon A");
    a.task.abort();
    a.run.daemon_does("raise:9");
    assert!(
        fx.identity(daemon).exited(Duration::from_secs(10)),
        "daemon A did not end by its own SIGKILL"
    );
    fx.save(
        "a_tree_ended",
        all_ended(&fx, &a.ids, Duration::from_secs(10)),
    );
    // The guard ends after its drain, so what it was going to end is ended by now.
    fx.save(
        "a_guard_ended",
        a.run.status_within(Duration::from_secs(60)).await.is_some(),
    );
    fx.save(
        "b_tree_alive",
        b.ids
            .iter()
            .all(|i| !fx.identity(*i).exited(Duration::ZERO)),
    );
    // `try_wait` reaps a process that has ended, so a zombie does not read as alive.
    fx.save(
        "outside_alive",
        outside
            .try_wait()
            .expect("try_wait the outside process")
            .is_none(),
    );
    fx.save(
        "capsule_alive",
        !fx.identity(capsule).exited(Duration::from_secs(1)),
    );
    let cleanup = fx.cleanup();
    let _ = outside.kill();
    let _ = outside.wait();
    assert!(cleanup.complete(), "{cleanup:?}");
    assert_eq!(
        fx.saved("a_tree_ended"),
        Some("true"),
        "daemon A's tree outlived it"
    );
    assert_eq!(
        fx.saved("a_guard_ended"),
        Some("true"),
        "daemon A's guard did not end after its drain"
    );
    assert_eq!(
        fx.saved("b_tree_alive"),
        Some("true"),
        "daemon A's guard ended daemon B's tree"
    );
    assert_eq!(
        fx.saved("outside_alive"),
        Some("true"),
        "daemon A's guard ended a process outside its subtree"
    );
    assert_eq!(
        fx.saved("capsule_alive"),
        Some("true"),
        "daemon A's guard ended the capsule it started"
    );
}
