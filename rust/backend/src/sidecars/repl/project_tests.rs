//! Real Julia, owned resource and depot roots: a bare workspace is the active project, a package operation writes the
//! workspace and never the installed shim, the child's real arguments and environment follow the selected project
//! on every spawn route, and (Linux) the child's own display ends with it on every route, and it with its display,
//! together with every process that holds the display, a detached one included.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::broadcast;

use super::*;
use crate::rows::Workspace;
use crate::sidecars::contract_tests::{depot_path, isolated, owned_julia_env, within, OwnedRoot};
#[cfg(target_os = "linux")]
use crate::sidecars::contract_tests::{executable, holding_display, x11_answer, x11_cookie, x11_server};
use super::tree_argv_tests::tree_argv_leaks;

/// The longest an isolated body here may take: an empty depot compiles the shim on first start.
pub(super) const BODY: Duration = Duration::from_secs(240);
const EVAL: Duration = Duration::from_secs(180);
/// The WGLMakie setup's bound. The fixture reads the depot list whole (`depot_path`; unset, Julia's default list), so
/// the offline add reuses the read depot's caches and compiles only the shim and WGLMakie, without its precompile
/// workload (`add_wglmakie`; WGLMakie in 5 s on one host), when the depot is named on the list it was built on (CI's:
/// .github/wgl-depot.sh). A read depot holding other versions recompiles those (25 packages on one host). Named on
/// another list, a depot can recompile most of WGLMakie's environment (420 s on hosted Linux, past 600 s on hosted
/// Windows); setup fails fast on that instead (`one_fresh_pkg_cache`).
const SETUP: Duration = Duration::from_secs(600);

/// A bare workspace (no `Project.toml`) whose path holds a space, and the real `Workspace::repl` factory over it.
struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
    shim: PathBuf,
    repl: Repl,
    _frames: broadcast::Receiver<ReplFrameMsg>,
    _owned: OwnedRoot,
}

impl Fixture {
    fn new() -> Self {
        Self::with_read_depot(None)
    }

    /// As `new`, with the depots of `read_depot` (a `JULIA_DEPOT_PATH` list) behind the owned depot, the list whole
    /// (`depot_path`): packages and their compiled caches are read from them, and every file
    /// Julia makes goes to the owned depot. In the read depot Julia does only the
    /// bookkeeping any session does there: Pkg makes and at once removes a lock file beside each package version it
    /// resolves (`packages/<name>/<slug>.pid`), and loading a cache updates its timestamp.
    fn with_read_depot(read_depot: Option<std::ffi::OsString>) -> Self {
        // The process-wide signal fired by `finish` is safe only inside an isolated body.
        debug_assert!(std::env::var_os(crate::sidecars::contract_tests::CHILD_ENV).is_some());
        let dir = tempfile::tempdir().expect("owned fixture root");
        let root = dir.path().to_path_buf();
        let shim = owned_julia_env(&root);
        if let Some(read) = read_depot {
            std::env::set_var("JULIA_DEPOT_PATH", depot_path(&root.join("depot"), Some(&read)));
            crate::sidecars::contract_tests::one_fresh_pkg_cache();
        }
        let workspace = root.join("workspace with spaces");
        std::fs::create_dir(&workspace).expect("create the bare workspace");
        let (frame_tx, frames) = broadcast::channel(64);
        let row = Workspace::meta_only(
            "ws-project-tests".into(),
            "project-tests".into(),
            "Project tests".into(),
            workspace.clone(),
            "sot-be-project-tests".into(),
            0,
            false,
            "none".into(),
            String::new(),
            String::new(),
        );
        let repl = row.repl(frame_tx);
        let root = dir.keep();
        Fixture {
            _owned: OwnedRoot::new(&root, crate::lifecycle::child_signal::process()),
            root,
            workspace,
            shim,
            repl,
            _frames: frames,
        }
    }

    /// The workspace as Julia's `realpath` spells it.
    fn real_workspace(&self) -> String {
        let real = std::fs::canonicalize(&self.workspace).expect("canonical workspace");
        real.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_owned()
    }

    /// Ends every child the process owns, then removes the fixture. Only the owned root is removed.
    async fn finish(self) {
        crate::lifecycle::child_signal::process().fire().expect("fire");
        within(Duration::from_secs(60), "owned children reaped", || {
            crate::lifecycle::child_signal::process().live() == 0
        })
        .await;
        std::fs::remove_dir_all(&self.root).expect("remove the owned fixture root");
    }
}

/// Evaluates `code` in the REPL and returns the frames it produced.
pub(super) async fn eval(repl: &Repl, id: u64, code: &str) -> Vec<Value> {
    eval_within(repl, id, code, EVAL)
        .await
        .expect("the eval must answer")
}

/// As `eval`, waiting at most `bound` for the answer; `None` when none came.
async fn eval_within(repl: &Repl, id: u64, code: &str, bound: Duration) -> Option<Vec<Value>> {
    let (reply, collector) = repl
        .execute("repl.eval", json!({ "code": code, "eval_id": id }))
        .await
        .expect("the REPL must start");
    tokio::time::timeout(bound, reply)
        .await
        .ok()?
        .expect("the supervisor must keep the reply")
        .expect("the eval must succeed");
    let frames = collector.lock().expect("collector").frames.clone();
    Some(frames)
}

pub(super) fn stdout_of(frames: &[Value]) -> String {
    frames
        .iter()
        .filter(|f| f["kind"] == "stdout")
        .filter_map(|f| f["text"].as_str())
        .collect::<String>()
        .trim()
        .to_owned()
}

/// A Julia source literal for `text`.
pub(super) fn raw(text: &str) -> String {
    format!("raw\"{text}\"")
}

/// Whether the child's active project and cwd are the workspace directory.
async fn active_in(repl: &Repl, id: u64, workspace: &str) -> String {
    let code = format!(
        "w = realpath({w}); println(realpath(dirname(Base.active_project())) == w, \",\", realpath(pwd()) == w)",
        w = raw(workspace)
    );
    let frames = eval(repl, id, &code).await;
    stdout_of(&frames)
}

/// A minimal owned registry and offline Pkg settings, so a package operation needs no network.
fn offline_pkg(root: &Path) {
    let registry = root.join("depot/registries/Fixture");
    std::fs::create_dir_all(&registry).expect("create the owned registry");
    std::fs::write(
        registry.join("Registry.toml"),
        "name = \"Fixture\"\nuuid = \"a58b057d-d7df-4e29-a186-405f45c2cafd\"\nrepo = \"\"\n[packages]\n",
    )
    .expect("write the owned registry");
    std::env::set_var("JULIA_PKG_OFFLINE", "true");
}

#[tokio::test]
async fn bare_workspace_is_active() {
    if !isolated("sidecars::repl::project_tests::bare_workspace_is_active", BODY) {
        return;
    }
    let fixture = Fixture::new();
    let real = fixture.real_workspace();
    assert_eq!(
        active_in(&fixture.repl, 1, &real).await,
        "true,true",
        "the bare workspace, not the shim, must be the active project and the cwd"
    );
    fixture.finish().await;
}

/// The owned fixture's first start compiles only the shim into its depot: the stdlibs the shim loads come precompiled
/// from Julia's bundled depot, so no first eval in these tests pays for compiling them.
#[tokio::test]
async fn first_start_compiles_only_the_shim() {
    if !isolated(
        "sidecars::repl::project_tests::first_start_compiles_only_the_shim",
        BODY,
    ) {
        return;
    }
    let fixture = Fixture::new();
    eval(&fixture.repl, 1, "1").await;
    let compiled_root = fixture.root.join("depot").join("compiled");
    let mut compiled = Vec::new();
    for version in std::fs::read_dir(compiled_root).expect("compiled") {
        for package in std::fs::read_dir(version.expect("version").path()).expect("packages") {
            let name = package.expect("package").file_name();
            compiled.push(name.to_string_lossy().into_owned());
        }
    }
    compiled.sort();
    assert_eq!(
        compiled,
        ["ShipToolsRepl"],
        "a first start must compile only the shim into the owned depot"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn pkg_add_does_not_edit_shim() {
    if !isolated(
        "sidecars::repl::project_tests::pkg_add_does_not_edit_shim",
        BODY,
    ) {
        return;
    }
    let fixture = Fixture::new();
    offline_pkg(&fixture.root);
    let shim_project =
        std::fs::read(fixture.shim.join("Project.toml")).expect("read the shim project");
    let frames = eval(
        &fixture.repl,
        1,
        "import Pkg; Pkg.offline(true); Pkg.add(\"LinearAlgebra\"; io=devnull); println(\"added\")",
    )
    .await;
    assert_eq!(
        stdout_of(&frames),
        "added",
        "the package operation must complete"
    );
    let workspace_project =
        std::fs::read_to_string(fixture.workspace.join("Project.toml")).unwrap_or_default();
    assert!(
        workspace_project.contains("LinearAlgebra"),
        "the dependency must land in the workspace project"
    );
    assert!(
        std::fs::read(fixture.shim.join("Project.toml")).expect("read the shim project after")
            == shim_project,
        "the installed shim project must be byte-identical"
    );
    assert!(
        !fixture.shim.join("Manifest.toml").exists(),
        "the installed shim must gain no manifest"
    );
    fixture.finish().await;
}

/// A REPL child the daemon kills leaves none of its temporary files. Julia's copy of SSH's known hosts, written on
/// first use, and a user's `mktemp` file live in the child's own folder, which goes after the child's reap: on a
/// restart, and when an ended row's handle drops.
#[tokio::test]
async fn a_killed_child_leaves_no_temp_files() {
    if !isolated(
        "sidecars::repl::project_tests::a_killed_child_leaves_no_temp_files",
        BODY,
    ) {
        return;
    }
    let fixture = Fixture::new();
    // This process's temporary folder is the fixture's, so a file a child leaves outside its own folder stays here.
    let tmp = fixture.root.join("tmp");
    std::fs::create_dir(&tmp).expect("create the owned temporary folder");
    let tmp_vars: &[&str] = if cfg!(windows) {
        &["TMP", "TEMP"]
    } else {
        &["TMPDIR"]
    };
    for key in tmp_vars {
        std::env::set_var(key, &tmp);
    }
    let make = "import NetworkOptions; println(NetworkOptions.ssh_known_hosts_files()[end]); println(mktemp()[1])";
    let made = |frames: Vec<Value>| -> Vec<PathBuf> {
        stdout_of(&frames).lines().map(PathBuf::from).collect()
    };

    let first = made(eval(&fixture.repl, 1, make).await);
    assert!(
        first.len() == 2 && first.iter().all(|p| p.exists()),
        "setup: the child made two temporary files"
    );
    fixture
        .repl
        .restart_with_project(&fixture.workspace)
        .await
        .expect("restart");
    within(
        Duration::from_secs(30),
        "a restart's killed child leaves no temporary file",
        || first.iter().all(|p| !p.exists()),
    )
    .await;

    let (frame_tx, _frames) = broadcast::channel(64);
    let row = Workspace::meta_only(
        "ws-tmp-tests".into(),
        "tmp-tests".into(),
        "Temporary files".into(),
        fixture.workspace.clone(),
        "sot-be-tmp-tests".into(),
        0,
        false,
        "none".into(),
        String::new(),
        String::new(),
    );
    let other = row.repl(frame_tx);
    let second = made(eval(&other, 2, make).await);
    assert!(
        second.len() == 2 && second.iter().all(|p| p.exists()),
        "setup: the second child made its files"
    );
    drop(other);
    drop(row);
    within(
        Duration::from_secs(30),
        "an ended row's killed child leaves no temporary file",
        || second.iter().all(|p| !p.exists()),
    )
    .await;
    // A folder goes after its files: the removal runs on a blocking thread after the reap, so the folder can outlast them.
    within(Duration::from_secs(30), "only the living child's own folder is left", || {
        std::fs::read_dir(&tmp).map_or(0, |d| d.count()) == 1
    })
    .await;
    fixture.finish().await;
}

/// Every spawn route must leave the same contract inside the real child: the workspace is the active project and
/// the cwd, the shim is second on the load path and answers, the workspace root is exported, and the child's own parse
/// names `--project=<workspace>`, whole, as its last `--project`, and leaves `ARGS` empty.
async fn check_route(fixture: &Fixture, route: &str, id: u64) -> u32 {
    let real = fixture.real_workspace();
    let repl = &fixture.repl;
    assert_eq!(
        active_in(repl, id, &real).await,
        "true,true",
        "{route}: active project and cwd"
    );
    let shim = fixture.shim.to_string_lossy().into_owned();
    let load_path = format!(
        "L = Base.LOAD_PATH; println(L[1] == \"@\" && realpath(L[2]) == realpath({}) && L[end] == \"@stdlib\")",
        raw(&shim)
    );
    assert_eq!(
        stdout_of(&eval(repl, id + 1, &load_path).await),
        "true",
        "{route}: load path order"
    );
    let root = format!(
        "println(realpath(ENV[\"SOT_WORKSPACE_ROOT\"]) == realpath({}))",
        raw(&real)
    );
    assert_eq!(
        stdout_of(&eval(repl, id + 2, &root).await),
        "true",
        "{route}: workspace root exported"
    );
    let pid: u32 = stdout_of(&eval(repl, id + 3, "println(getpid())").await)
        .parse()
        .expect("child pid");
    // The child's own parse of its arguments: on Windows the OS copy of the command line is not evidence, since
    // Julia's loader splits that buffer in place, leaving the executable path alone before its first NUL.
    let parsed = format!(
        "println(unsafe_string(Base.JLOptions().project) == {}, \",\", isempty(ARGS))",
        raw(&fixture.workspace.to_string_lossy())
    );
    assert_eq!(
        stdout_of(&eval(repl, id + 4, &parsed).await),
        "true,true",
        "{route}: the child parsed --project as one intact argument naming the workspace, and no program argument"
    );
    pid
}

#[tokio::test]
async fn repl_child_arguments_and_environment_match_selected_project() {
    if !isolated("sidecars::repl::project_tests::repl_child_arguments_and_environment_match_selected_project", BODY * 2) {
        return;
    }
    let fixture = Fixture::new();
    let initial = check_route(&fixture, "initial start", 10).await;

    // Death respawn: the child ends itself; the next submission starts a new one.
    let (_reply, _collector) = fixture
        .repl
        .execute("repl.eval", json!({ "code": "exit(0)", "eval_id": 20 }))
        .await
        .expect("submit the exit");
    within(
        Duration::from_secs(60),
        "the REPL reports its child dead",
        || fixture.repl.state() == lifecycle::ReplLifecycle::Dead,
    )
    .await;
    let respawned = check_route(&fixture, "death respawn", 30).await;
    assert_ne!(initial, respawned, "a death respawn is a new child");

    // Explicit restart onto the same selected project.
    fixture
        .repl
        .restart_with_project(&fixture.workspace)
        .await
        .expect("restart");
    let restarted = check_route(&fixture, "explicit restart", 40).await;
    assert_ne!(respawned, restarted, "a restart is a new child");
    fixture.finish().await;
}

/// The display a REPL child names in its environment, checked against its server: the child's cookie opens it, no
/// cookie does not, and the session type sends GLFW to it. Returns its number and cookie.
#[cfg(target_os = "linux")]
async fn display_of(repl: &Repl, id: u64) -> (u32, Vec<u8>) {
    let code = r#"for k in ("DISPLAY", "XAUTHORITY", "XDG_SESSION_TYPE") println(get(ENV, k, "-")) end"#;
    let out = stdout_of(&eval(repl, id, code).await);
    let named: Vec<&str> = out.lines().collect();
    assert!(
        named.len() == 3 && named[0].starts_with(':'),
        "the child names a display of its own: {out:?}"
    );
    assert_eq!(named[2], "x11", "the child's session type sends GLFW to that display");
    let number: u32 = named[0][1..].parse().expect("a display number");
    let cookie = x11_cookie(Path::new(named[1]));
    assert_eq!(x11_answer(number, Some(&cookie)), Some(1), "the child's cookie opens its display");
    assert_eq!(x11_answer(number, None), Some(0), "a client with no cookie is refused");
    (number, cookie)
}

/// Waits until display `:number` answers `cookie` no more: nothing listens there, or another server refuses it.
#[cfg(target_os = "linux")]
async fn display_ended(number: u32, cookie: &[u8], what: &str) {
    within(Duration::from_secs(30), what, || x11_answer(number, Some(cookie)) != Some(1)).await;
}

/// A REPL child has a display of its own (Linux), which only its cookie opens, and the display ends with the child
/// however the child ends: a restart (`--fresh` is one), the child's own death, its row's end and the daemon's stop. A
/// process the child started detached, which holds the display, ends with it.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_repl_child_has_its_own_display_which_ends_with_it() {
    if !isolated(
        "sidecars::repl::project_tests::a_repl_child_has_its_own_display_which_ends_with_it",
        BODY * 2,
    ) {
        return;
    }
    executable("Xvfb");
    let fixture = Fixture::new();
    let (first, first_cookie) = display_of(&fixture.repl, 1).await;
    let detached = r#"run(pipeline(detach(`sleep 300`), stdin=devnull, stdout=devnull, stderr=devnull); wait=false); print(ENV["XAUTHORITY"])"#;
    let held = PathBuf::from(stdout_of(&eval(&fixture.repl, 6, detached).await));
    within(Duration::from_secs(30), "setup: the REPL and its detached sleep hold the display", || {
        holding_display(&held).len() == 2
    })
    .await;
    fixture.repl.restart_with_project(&fixture.workspace).await.expect("restart");
    display_ended(first, &first_cookie, "a restart's display ends").await;
    within(Duration::from_secs(30), "a restart ends what holds the old display", || {
        holding_display(&held).is_empty()
    })
    .await;

    let (second, second_cookie) = display_of(&fixture.repl, 2).await;
    assert_ne!(first_cookie, second_cookie, "a restarted child has a display of its own");
    let (_reply, _collector) = fixture
        .repl
        .execute("repl.eval", json!({ "code": "exit(0)", "eval_id": 3 }))
        .await
        .expect("submit the exit");
    display_ended(second, &second_cookie, "a dead child's display ends").await;

    let (frame_tx, _frames) = broadcast::channel(64);
    let row = Workspace::meta_only(
        "ws-display-tests".into(),
        "display-tests".into(),
        "Displays".into(),
        fixture.workspace.clone(),
        "sot-be-display-tests".into(),
        0,
        false,
        "none".into(),
        String::new(),
        String::new(),
    );
    let other = row.repl(frame_tx);
    let (third, third_cookie) = display_of(&other, 4).await;
    drop(other);
    drop(row);
    display_ended(third, &third_cookie, "an ended row's display ends").await;

    let (fourth, fourth_cookie) = display_of(&fixture.repl, 5).await;
    fixture.finish().await;
    display_ended(fourth, &fourth_cookie, "the daemon's stop ends the display").await;
}

/// A REPL whose display ends under it, one that never opened it, ends with everything it started, in its process group
/// or detached, so nothing of it can open that display's name once another account binds it; its next eval starts a
/// new REPL with a new display and first says why the old one ended, once.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_repl_whose_display_ends_ends_with_it() {
    if !isolated("sidecars::repl::project_tests::a_repl_whose_display_ends_ends_with_it", BODY) {
        return;
    }
    executable("Xvfb");
    let fixture = Fixture::new();
    // Processes the REPL started hold the display too: one in its process group, one detached (a session of its own).
    let code = r#"for c in (`sleep 300`, detach(`sleep 300`)) run(pipeline(c, stdin=devnull, stdout=devnull, stderr=devnull); wait=false) end; print(ENV["DISPLAY"], " ", ENV["XAUTHORITY"])"#;
    let named = stdout_of(&eval(&fixture.repl, 1, code).await);
    let (display, cookie) = named.split_once(' ').expect("the REPL names its display");
    let cookie = PathBuf::from(cookie);
    within(Duration::from_secs(30), "setup: the REPL and its two sleeps hold the display", || {
        holding_display(&cookie).len() == 3
    })
    .await;
    // SAFETY: a signal to the X server started for this test's REPL.
    assert_eq!(unsafe { libc::kill(x11_server(&cookie) as libc::pid_t, libc::SIGKILL) }, 0, "kill the display's server");
    let held = format!("nothing of the REPL keeps DISPLAY={display} once its server is gone");
    within(Duration::from_secs(30), &held, || holding_display(&cookie).is_empty()).await;
    within(Duration::from_secs(30), "the REPL is ended", || {
        fixture.repl.state() == lifecycle::ReplLifecycle::Dead
    })
    .await;

    let frames = eval(&fixture.repl, 2, r#"print(ENV["XAUTHORITY"])"#).await;
    assert_eq!(frames[0]["kind"], "stderr", "the new REPL first says why the old one ended: {frames:?}");
    let said = frames[0]["text"].as_str().unwrap_or_default();
    assert!(said.contains("was ended because its own display (Xvfb) ended"), "{said}");
    assert_ne!(PathBuf::from(stdout_of(&frames)), cookie, "the new REPL has a display of its own");
    display_of(&fixture.repl, 3).await;
    let quiet = eval(&fixture.repl, 4, "1").await;
    assert!(!quiet.iter().any(|f| f["kind"] == "stderr"), "it says so once: {quiet:?}");
    fixture.finish().await;
}

const SENTINEL: &str = "sentinel-0f3a9c11-not-a-secret";

/// The page address the eval announced, when it announced one.
fn announced_address(frames: &[Value]) -> Option<String> {
    frames
        .iter()
        .find(|f| f["kind"] == "browser")
        .and_then(|f| f["url"].as_str())
        .map(str::to_owned)
}

/// The page test's leak control alone (no package: the Windows box and hosted legs run it), isolated for its environment.
#[tokio::test]
async fn the_command_line_observer_rejects_a_deliberate_leak() {
    if isolated("sidecars::repl::project_tests::the_command_line_observer_rejects_a_deliberate_leak", BODY) {
        observer_rejects_a_deliberate_leak().await;
    }
}

/// The observer rejects a deliberate leak: a needle carried by a process of an owned probe's tree is found, and a
/// needle nothing carries is passed. Two needles say two things. The root needle is the root's own trailing argument:
/// finding it shows the root's command line is readable. The descendant needle sits in the child's arguments alone, and
/// the root's own text builds it from two pieces: finding it shows the tree reading reaches a descendant. The probe
/// prints its pid, starts the child, then waits. It is Julia, the kind the observation watches, except on Windows:
/// there Julia's loader splits its own command line in place, writing a NUL after each argument
/// (`cli/loader_win_utils.c` in every release from 1.6 to 1.13.1), so a reader sees only its executable path, and the
/// probe is PowerShell, whose command line keeps its arguments.
async fn observer_rejects_a_deliberate_leak() {
    let sig: &'static crate::lifecycle::child_signal::Signal =
        Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
    let mut probe = if cfg!(windows) {
        let mut probe = tokio::process::Command::new("powershell");
        probe.args([
            "-NoProfile",
            "-Command",
            "$n = 'probe-needle-' + '5d1e'; [Console]::Out.WriteLine($PID); [Console]::Out.Flush(); \
             & powershell -NoProfile -Command \"Start-Sleep -Seconds 60 # $n\" # probe-root-needle-5d1e",
        ]);
        probe
    } else {
        let mut probe =
            tokio::process::Command::new(crate::sidecars::contract_tests::executable("julia"));
        probe.args([
            "--startup-file=no",
            "-e",
            "n = \"probe-needle-\" * \"5d1e\"; run(`sh -c \"sleep 60; :\" $n`; wait=false); \
             println(getpid()); flush(stdout); sleep(60)",
            "probe-root-needle-5d1e",
        ]);
        probe
    };
    // Its stderr is its own: Julia leaves an inherited stdio pipe non-blocking (sidecars/repl/CLAUDE.md).
    probe.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    let mut owned = sig.spawn(&mut probe).expect("start the owned leak probe");
    let mut first = tokio::io::BufReader::new(owned.stdout.take().expect("probe stdout"));
    let mut line = String::new();
    tokio::time::timeout(
        Duration::from_secs(60),
        tokio::io::AsyncBufReadExt::read_line(&mut first, &mut line),
    )
    .await
    .expect("the probe reports its pid")
    .expect("read the probe pid");
    let pid: u32 = line.trim().parse().expect("probe pid");
    for needle in ["probe-root-needle-5d1e", "probe-needle-5d1e"] {
        within(
            Duration::from_secs(30),
            &format!("the probe's {needle} is observable"),
            || tree_argv_leaks(pid, &[needle]),
        )
        .await;
    }
    assert!(
        tree_argv_leaks(pid, &["probe-root-needle-5d1e"]),
        "the observer must reject a leaking root command line"
    );
    assert!(
        tree_argv_leaks(pid, &["probe-needle-5d1e"]),
        "the observer must reject a leaking descendant command line"
    );
    assert!(
        !tree_argv_leaks(pid, &["a-needle-it-does-not-carry"]),
        "the observer must pass a clean one"
    );
    let _ = owned.kill().await;
    sig.fire().expect("fire");
}

/// Adds WGLMakie to the fixture's workspace from the read depot, offline; its precompile is setup, bounded by `SETUP`.
/// WGLMakie's precompile workload serves a page from Bonito's default server, on 127.0.0.1:9384, a port the whole
/// host shares: while anything else holds it (another test job compiling WGLMakie, a Bonito session), the build fails
/// (Bonito moves to the next port only on the errors it knows, and HTTP 2.11's `AddressInUseError` is not one), and so
/// does the page's `using WGLMakie`. The page needs WGLMakie loaded, not its workload, so the workspace turns it off.
async fn add_wglmakie(fixture: &Fixture) {
    offline_pkg(&fixture.root);
    std::fs::write(
        fixture.workspace.join("LocalPreferences.toml"),
        "[WGLMakie]\nprecompile_workload = false\n",
    )
    .expect("turn WGLMakie's precompile workload off");
    let added = eval_within(
        &fixture.repl,
        1,
        "import Pkg; Pkg.offline(true); Pkg.add(\"WGLMakie\"; io=devnull); Pkg.precompile(\"WGLMakie\"); println(\"added\")",
        SETUP,
    )
    .await
    .unwrap_or_else(|| {
        panic!(
            "setup: WGLMakie did not install and precompile into the owned depot within {SETUP:?}"
        )
    });
    assert_eq!(
        stdout_of(&added),
        "added",
        "setup: WGLMakie must be installable from the read depot: {}",
        failure_frames(&added)
    );
}

/// The `error` and `stderr` frames of an eval, whole: the reason a child's eval failed (Pkg reports a failed
/// precompile on stderr, and a failed `using` as an `error`).
fn failure_frames(frames: &[Value]) -> String {
    frames
        .iter()
        .filter(|f| f["kind"] == "error" || f["kind"] == "stderr")
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

/// A page served by the real `wglshow` path never puts its secret, or the address that carries it, on a command
/// line of the REPL's tree, before or after the page exists, on every spawn route; a deliberately leaking process is
/// rejected by the same observer, and a sentinel proves the observation sees a value the child really has. It reads
/// every process of the tree; on Windows the Julia root's own command line cannot be read (Julia's loader), so there
/// the check covers the descendants only.
#[tokio::test]
async fn repl_page_secret_never_reaches_command_line() {
    if !isolated(
        "sidecars::repl::project_tests::repl_page_secret_never_reaches_command_line",
        SETUP + BODY,
    ) {
        return;
    }
    std::env::set_var("SOT_TEST_PAGE_SENTINEL", SENTINEL);
    let fixture = Fixture::with_read_depot(Some(crate::sidecars::contract_tests::read_depots()));
    add_wglmakie(&fixture).await;

    observer_rejects_a_deliberate_leak().await;

    for (route, base) in [
        ("initial start", 100u64),
        ("death respawn", 200),
        ("explicit restart", 300),
    ] {
        match route {
            "death respawn" => {
                let _ = fixture
                    .repl
                    .execute(
                        "repl.eval",
                        json!({ "code": "exit(0)", "eval_id": base - 1 }),
                    )
                    .await
                    .expect("submit the exit");
                within(
                    Duration::from_secs(60),
                    "the REPL reports its child dead",
                    || fixture.repl.state() == lifecycle::ReplLifecycle::Dead,
                )
                .await;
            }
            "explicit restart" => fixture
                .repl
                .restart_with_project(&fixture.workspace)
                .await
                .expect("restart"),
            _ => {}
        }
        let pid: u32 = stdout_of(&eval(&fixture.repl, base, "println(getpid())").await)
            .parse()
            .expect("child pid");
        assert_eq!(
            stdout_of(
                &eval(
                    &fixture.repl,
                    base + 1,
                    "println(haskey(ENV, \"SOT_TEST_PAGE_SENTINEL\"))"
                )
                .await
            ),
            "true",
            "{route}: the sentinel is in the child"
        );
        assert!(
            !tree_argv_leaks(pid, &[SENTINEL]),
            "{route}: the sentinel is not on a command line"
        );
        let page = eval(&fixture.repl, base + 2, "using WGLMakie; fig = WGLMakie.Makie.scatter(1:3); println(ShipToolsRepl.wglshow(fig; open=false) isa BrowserView)").await;
        let address = announced_address(&page)
            .unwrap_or_else(|| panic!("{route}: the page was not announced: {}", failure_frames(&page)));
        let secret = address.rsplit('/').next().unwrap_or_default().to_owned();
        assert_eq!(
            secret.len(),
            32,
            "{route}: the announced address ends in the page secret"
        );
        assert!(
            !tree_argv_leaks(pid, &[&secret, &address]),
            "{route}: neither the page secret nor its address is on a command line"
        );
    }
    fixture.finish().await;
}
