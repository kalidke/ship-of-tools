//! The parts of the lifetime guard's done test (`workers::every_descendant_of_a_killed_daemon_ends`): the case's inputs, the
//! trees it starts in a guarded daemon through each product route, and the oracle it reads after the daemon is killed.

use crate::fixture_owner::Fixture;
use crate::guard::Run;
use crate::routes::{
    all_ended, call_long, children_of, command_line, julia_bin, nonce_round_trip, ready_row,
    spin_in_repl, supervisor_in, watch_tree,
};
use crate::support::{call, connect_and_hello, poll_until, Conn, BOUND};
use crate::tree::Tree;
use sot_protocol::op;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn repo_file(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(rel)
}

/// The case's own copy of `julia/pluto`, where the daemon finds it (`SOT_RESOURCE_ROOT`): `session_options.jl` carries one
/// more line, the worker's startup expression from the environment, so a worker starts the case's tree without a cell being
/// run. The environment is instantiated here unless `SOT_L2_PLUTO_MANIFEST` names a manifest to use.
pub fn pluto_resource_root(case: &Path, julia: &str) -> PathBuf {
    let root = case.join("resources");
    let dir = root.join("julia").join("pluto");
    std::fs::create_dir_all(&dir).expect("create the resource folder");
    for file in ["Project.toml", "start.jl"] {
        std::fs::copy(repo_file("julia/pluto").join(file), dir.join(file))
            .expect("copy a Pluto file");
    }
    let options = std::fs::read_to_string(repo_file("julia/pluto/session_options.jl"))
        .expect("read session_options.jl");
    let marker = "    session.options.evaluation.workspace_use_distributed_stdlib = true\n";
    assert!(
        options.contains(marker),
        "session_options.jl no longer has the line the case extends"
    );
    let patched = options.replacen(
        marker,
        &format!("{marker}    let s = get(ENV, \"SOT_L2_WORKER_STARTUP\", \"\"); isempty(s) || (session.options.evaluation.workspace_custom_startup_expr = s); end\n"),
        1,
    );
    std::fs::write(dir.join("session_options.jl"), patched)
        .expect("write the patched session_options.jl");
    match std::env::var_os("SOT_L2_PLUTO_MANIFEST") {
        Some(manifest) => {
            std::fs::copy(manifest, dir.join("Manifest.toml")).expect("copy the Pluto manifest");
        }
        None => {
            let out = std::process::Command::new(julia)
                .arg(format!("--project={}", dir.display()))
                .args(["-e", "using Pkg; Pkg.instantiate()"])
                .env(
                    "JULIA_DEPOT_PATH",
                    crate::julia_depot_path(&case.join("julia-depot")),
                )
                .output()
                .expect("run julia to instantiate Pluto");
            assert!(
                out.status.success(),
                "instantiating Pluto failed:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    root
}

/// The Julia a worker or a chunk runs to ignore TERM and HUP, report its pid and start `tree` detached.
pub fn worker_expression(tree: &Tree, report: &Path, spin: bool) -> String {
    format!(
        "ccall(:signal, Ptr{{Cvoid}}, (Cint, Ptr{{Cvoid}}), 15, Ptr{{Cvoid}}(1)); ccall(:signal, Ptr{{Cvoid}}, (Cint, Ptr{{Cvoid}}), 1, Ptr{{Cvoid}}(1)); write(\"{}\", string(getpid())); run(detach(`sh {}`); wait = false){}",
        report.display(),
        tree.start_words(false),
        if spin { "; while true end" } else { "" }
    )
}

pub fn have_quarto() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let quarto = std::env::split_paths(&path)
        .map(|d| d.join("quarto"))
        .find(|p| p.is_file())?;
    std::process::Command::new(&quarto)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(quarto)
}

/// Everything the case gives the daemon before it starts: its folder, the trees, the environment.
pub struct Inputs {
    pub _case: tempfile::TempDir,
    pub pluto_tree: Tree,
    pub quarto_tree: Tree,
    pub capsule_tree: Tree,
    pub repl_tree: Tree,
    pub quarto: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

impl Inputs {
    pub fn new() -> Inputs {
        let julia = julia_bin();
        let case = tempfile::tempdir().expect("the case's folder");
        let pluto_tree = Tree::new(case.path(), "pluto-tree");
        let quarto_tree = Tree::new(case.path(), "quarto-tree");
        let capsule_tree = Tree::new(case.path(), "capsule-tree");
        let repl_tree = Tree::new(case.path(), "repl-tree");
        let quarto = have_quarto();
        let resources = pluto_resource_root(case.path(), &julia);
        let worker_startup =
            worker_expression(&pluto_tree, &pluto_tree.report_path("worker"), true);
        let mut path = std::env::var("PATH").unwrap_or_default();
        if let Some(real) = &quarto {
            // Quarto's transport files live under XDG_RUNTIME_DIR: the case's own, so no case can reach a live user's engine
            // server. Only this wrapper sets it; the daemon keeps the real one, which `systemd-run --user` needs.
            let (bin, runtime) = (case.path().join("bin"), case.path().join("quarto-runtime"));
            std::fs::create_dir_all(&bin).expect("create the wrapper's folder");
            std::fs::create_dir_all(&runtime).expect("create the quarto runtime folder");
            sot_log::test_exec::write_executable(
                &bin.join("quarto"),
                format!(
                    "#!/bin/sh\nexec env XDG_RUNTIME_DIR='{}' '{}' \"$@\"\n",
                    runtime.display(),
                    real.display()
                ),
            );
            path = format!("{}:{path}", bin.display());
        }
        let env = vec![
            ("SOT_JULIA_BIN".to_string(), julia),
            (
                "SOT_RESOURCE_ROOT".to_string(),
                resources.to_string_lossy().into_owned(),
            ),
            ("SOT_L2_WORKER_STARTUP".to_string(), worker_startup),
            ("PATH".to_string(), path),
        ];
        Inputs {
            _case: case,
            pluto_tree,
            quarto_tree,
            capsule_tree,
            repl_tree,
            quarto,
            env,
        }
    }

    pub fn env_pairs(&self) -> Vec<(&str, &str)> {
        self.env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }
}

/// What the case holds once its trees are up.
#[derive(Default)]
pub struct Held {
    pub workspace_id: String,
    pub state_dir: PathBuf,
    pub capsule_ids: Vec<usize>,
    pub repl_ids: Vec<usize>,
    pub repl_spin: Option<tokio::task::JoinHandle<()>>,
    pub pluto_ids: Vec<usize>,
    pub quarto_ids: Vec<usize>,
    pub quarto_seen: Vec<usize>,
}

pub struct Wire {
    pub conn: Conn,
    pub next_id: u64,
}

impl Wire {
    pub async fn connect(run: &Run) -> Wire {
        let (conn, next_id) = connect_and_hello(&run.env.socket_path).await;
        Wire { conn, next_id }
    }

    async fn long(&mut self, op: &str, payload: serde_json::Value) -> serde_json::Value {
        let reply = call_long(
            &mut self.conn,
            self.next_id,
            op,
            payload,
            Duration::from_secs(600),
        )
        .await;
        self.next_id += 1;
        assert!(
            reply.payload.get("error").is_none(),
            "{op} failed: {:?}",
            reply.payload
        );
        reply.payload
    }
}

pub fn read_pid(path: PathBuf) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// A capsule row whose agent runs a tree, and a REPL cell that starts its own tree and spins.
pub async fn hold_row(
    fx: &mut Fixture,
    run: &Run,
    wire: &mut Wire,
    inputs: &Inputs,
    held: &mut Held,
) {
    let (workspace_id, state_dir) =
        ready_row(&run.env, &mut wire.conn, &mut wire.next_id, "done").await;
    let typed = call(
        &mut wire.conn,
        wire.next_id,
        op::PTY_INPUT,
        serde_json::json!({ "workspace_id": workspace_id, "data_b64": base64_of(&format!("{} &", inputs.capsule_tree.shell_command(false))), "enter": true, "origin": "l2-done-test" }),
    )
    .await;
    wire.next_id += 1;
    assert!(typed.payload.get("error").is_none(), "{:?}", typed.payload);
    held.repl_spin = Some(
        spin_in_repl(
            &run.env.socket_path,
            &workspace_id,
            inputs.repl_tree.julia_cell(false),
        )
        .await,
    );
    held.capsule_ids = watch_tree(fx, &inputs.capsule_tree, false, "the capsule's").await;
    held.repl_ids = watch_tree(fx, &inputs.repl_tree, false, "the REPL's").await;
    held.workspace_id = workspace_id;
    held.state_dir = state_dir;
}

/// One notebook, in `folder` (a row's root), opened through the daemon's real Pluto supervisor (`pluto.open`).
pub async fn open_notebook(folder: &Path, wire: &mut Wire) {
    let notebook = folder.join("notebook.jl");
    std::fs::copy(
        repo_file("rust/backend/tests/daemon_lifetime/fixtures/notebook.jl"),
        &notebook,
    )
    .expect("copy the notebook");
    wire.long(
        op::PLUTO_OPEN,
        serde_json::json!({ "path": notebook.to_string_lossy() }),
    )
    .await;
}

/// Pluto: a notebook opened through the daemon; its worker starts the case's tree from its startup expression. Pluto's
/// server is a child of the daemon (its children list).
pub async fn hold_pluto(
    fx: &mut Fixture,
    run: &Run,
    wire: &mut Wire,
    inputs: &Inputs,
    held: &mut Held,
) {
    open_notebook(&run.env.workspace_project_root, wire).await;
    held.pluto_ids = watch_tree(fx, &inputs.pluto_tree, false, "Pluto's").await;
    let worker = poll_until(
        || async { read_pid(inputs.pluto_tree.report_path("worker")) },
        Duration::from_secs(120),
        "Pluto's worker to report",
    )
    .await;
    held.pluto_ids.push(
        fx.watch(worker, None, "Pluto's worker")
            .expect("an identity for the worker that reported itself"),
    );
    let server = children_of(run.daemon)
        .into_iter()
        .find(|pid| command_line(*pid).contains("start.jl"))
        .expect("the Pluto server is a child of the daemon");
    // The daemon's own child, found in the children list of the process the case holds, so the case may end it.
    held.pluto_ids.push(
        fx.watch(server, None, "Pluto's server")
            .expect("an identity for the daemon's child"),
    );
}

/// Quarto: a document rendered with execution through the daemon. The chunk runs in the engine's worker, which a render may
/// end, and reports its pid and its parent's, the engine server that stays; both are looked at and never signalled.
pub async fn hold_quarto(
    fx: &mut Fixture,
    run: &Run,
    wire: &mut Wire,
    inputs: &Inputs,
    held: &mut Held,
) {
    if inputs.quarto.is_none() {
        eprintln!("Quarto 1.7.31 is not installed here: the Quarto half is NOT CHECKED");
        return;
    }
    let doc = run.env.workspace_project_root.join("render.qmd");
    let text = std::fs::read_to_string(repo_file(
        "rust/backend/tests/daemon_lifetime/fixtures/render.qmd",
    ))
    .expect("read the document");
    let tree = &inputs.quarto_tree;
    std::fs::write(
        &doc,
        text.replace("@DIR@", &tree.dir_str())
            .replace("@TREE@", &tree.script_path(false)),
    )
    .expect("write the document");
    wire.long(
        op::QUARTO_OPEN,
        serde_json::json!({ "path": doc.to_string_lossy(), "execute": true }),
    )
    .await;
    held.quarto_ids = watch_tree(fx, tree, false, "Quarto's").await;
    let server = poll_until(
        || async { read_pid(tree.report_path("server")) },
        Duration::from_secs(60),
        "Quarto's chunk to report",
    )
    .await;
    held.quarto_seen.push(
        fx.watch(server, None, "Quarto's engine server")
            .expect("observe the engine server"),
    );
    if let Some(worker) = read_pid(tree.report_path("worker")) {
        held.quarto_seen
            .extend(fx.watch(worker, None, "Quarto's worker").ok());
    }
}

/// The row's supervisor as the product's lane reports it: its pid and creation time.
pub async fn supervisor_of(run: &Run, state_dir: &Path) -> Option<(i32, u64)> {
    supervisor_in(&run.env, state_dir).await
}

/// Save what the killed daemon left, before any cleanup.
pub fn save_oracle(fx: &mut Fixture, held: &Held, supervisor_id: usize) {
    let within = Duration::from_secs(10);
    fx.save("repl_ended", all_ended(fx, &held.repl_ids, within));
    fx.save("pluto_ended", all_ended(fx, &held.pluto_ids, within));
    let quarto = all_ended(fx, &held.quarto_ids, within)
        && held
            .quarto_seen
            .iter()
            .all(|i| fx.identity(*i).exited(within));
    fx.save("quarto_ended", quarto);
    let alive = held
        .capsule_ids
        .iter()
        .chain([&supervisor_id])
        .all(|i| !fx.identity(*i).exited(Duration::ZERO));
    fx.save("capsule_alive", alive);
}

/// A successor on the same roots reaches the same supervisor and a fresh nonce comes back through its daemon.
pub async fn save_successor(
    fx: &mut Fixture,
    run: &mut Run,
    env: &[(&str, &str)],
    held: &Held,
    supervisor: (i32, u64),
) {
    run.successor(env).await;
    let mut wire = Wire::connect(run).await;
    let adopted = poll_until(
        || supervisor_of(run, &held.state_dir),
        BOUND,
        "the successor to reach the supervisor",
    )
    .await;
    fx.save(
        "successor_adopts_the_same_supervisor",
        adopted == supervisor,
    );
    let nonce = nonce_round_trip(
        &mut wire.conn,
        &mut wire.next_id,
        &held.workspace_id,
        "l2-done-test",
    )
    .await;
    fx.save("nonce", nonce);
}

/// The asserts, after cleanup: the launched process ended by the daemon's signal, nothing ephemeral survives, the capsule
/// stays and the successor adopts it.
pub fn assert_oracle(fx: &Fixture, status: Option<std::process::ExitStatus>, said: &str) {
    use std::os::unix::process::ExitStatusExt;
    let status = status.unwrap_or_else(|| {
        panic!(
            "the launched process did not end:
{said}"
        )
    });
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "the launched process did not end by the daemon's signal: {status:?}"
    );
    let survivors: Vec<&str> = [
        ("the REPL's tree", "repl_ended"),
        ("Pluto's server, worker and tree", "pluto_ended"),
        ("Quarto's server, worker and tree", "quarto_ended"),
    ]
    .into_iter()
    .filter(|(_, key)| fx.saved(key) != Some("true"))
    .map(|(what, _)| what)
    .collect();
    assert!(
        survivors.is_empty(),
        "outlived the killed daemon: {survivors:?}"
    );
    assert_eq!(
        fx.saved("capsule_alive"),
        Some("true"),
        "the capsule did not stay alive"
    );
    assert_eq!(
        fx.saved("successor_adopts_the_same_supervisor"),
        Some("true"),
        "the successor did not adopt the same supervisor"
    );
    assert!(
        fx.saved("nonce").is_some_and(|n| n.starts_with("true")),
        "the fresh nonce did not come back: {:?}",
        fx.saved("nonce")
    );
}

fn base64_of(text: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(text)
}
