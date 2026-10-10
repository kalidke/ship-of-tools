//! Real Julia, owned resource and depot roots: a bare workspace is the active project, a package operation writes the
//! workspace and never the installed shim, and the child's real arguments and environment follow the selected project
//! on every spawn route.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::broadcast;

use super::*;
use crate::rows::Workspace;
use crate::sidecars::contract_tests::{depot_path, isolated, owned_julia_env, within};

/// The longest an isolated body here may take: an empty depot compiles the shim on first start.
pub(super) const BODY: Duration = Duration::from_secs(240);
const EVAL: Duration = Duration::from_secs(180);
/// The WGLMakie setup's bound. Each run's owned depot starts empty and the read depot's compile caches need not match
/// the versions an offline add resolves, so the add can precompile WGLMakie's whole environment, Makie then WGLMakie in
/// series; about twice the slowest measured (303 s, on two CPUs).
const SETUP: Duration = Duration::from_secs(600);

/// A bare workspace (no `Project.toml`) whose path holds a space, and the real `Workspace::repl` factory over it.
struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
    shim: PathBuf,
    repl: Repl,
    _frames: broadcast::Receiver<ReplFrameMsg>,
}

impl Fixture {
    fn new() -> Self {
        Self::with_read_depot(None)
    }

    /// As `new`, with the depots of `read_depot` (a `JULIA_DEPOT_PATH` list) behind the owned depot and Julia's own
    /// bundled depots after them (`depot_path`): packages and their compiled caches are read from them, and every file
    /// Julia makes goes to the owned depot. In the read depot Julia does only the
    /// bookkeeping any session does there: Pkg makes and at once removes a lock file beside each package version it
    /// resolves (`packages/<name>/<slug>.pid`), and loading a cache updates its timestamp.
    fn with_read_depot(read_depot: Option<std::ffi::OsString>) -> Self {
        let root = tempfile::tempdir().expect("owned fixture root").keep();
        let shim = owned_julia_env(&root);
        if let Some(read) = read_depot {
            std::env::set_var("JULIA_DEPOT_PATH", depot_path(&root.join("depot"), Some(&read)));
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
        Fixture {
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
    if !isolated(
        "sidecars::repl::project_tests::bare_workspace_is_active",
        BODY,
    ) {
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

/// The running child's real argument vector, from the operating system. On Windows the OS keeps one command line,
/// returned as a single element.
#[cfg(target_os = "linux")]
fn os_argv(pid: u32) -> Vec<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).expect("read the child's command line");
    raw.split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

#[cfg(target_os = "macos")]
fn os_argv(pid: u32) -> Vec<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    // SAFETY: the sizes and buffers passed are the ones the calls document.
    let first = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(first, 0, "size the child's argument block");
    let mut buf = vec![0u8; size];
    // SAFETY: `buf` is `size` bytes.
    let second = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(second, 0, "read the child's argument block");
    buf.truncate(size);
    let argc = i32::from_ne_bytes(buf[..4].try_into().expect("argc")) as usize;
    // After the executable path and its padding come argc arguments.
    let rest = &buf[4..];
    let path_end = rest
        .iter()
        .position(|b| *b == 0)
        .expect("executable path end");
    let args_start = path_end
        + rest[path_end..]
            .iter()
            .position(|b| *b != 0)
            .expect("argument start");
    rest[args_start..]
        .split(|b| *b == 0)
        .take(argc)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

#[cfg(windows)]
fn os_argv(pid: u32) -> Vec<String> {
    let mut command = std::process::Command::new("powershell");
    command.args([
        "-NoProfile",
        "-Command",
        &format!("(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CommandLine"),
    ]);
    let out = crate::lifecycle::child_signal::process()
        .output(&mut command)
        .expect("read the child's command line");
    vec![String::from_utf8_lossy(&out.stdout).trim().to_owned()]
}

/// Whether the vector carries `--project=<workspace>` as one intact argument.
fn has_intact_project(argv: &[String], workspace: &Path) -> bool {
    let wanted = format!("--project={}", workspace.display());
    if cfg!(windows) {
        argv.iter()
            .any(|line| line.contains(&format!("\"{wanted}\"")))
    } else {
        argv.iter().any(|arg| *arg == wanted)
    }
}

/// Every spawn route must leave the same contract inside the real child: the workspace is the active project and
/// the cwd, the shim is second on the load path and answers, the workspace root is exported, and the OS argument
/// vector names the workspace as one intact `--project` argument and does not name the shim.
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
    let argv = os_argv(pid);
    assert!(
        has_intact_project(&argv, &fixture.workspace),
        "{route}: --project is one intact argument"
    );
    assert!(
        !argv.iter().any(|a| a.contains(&shim)),
        "{route}: the shim is not an argument"
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

/// The process ids of the child's tree: its process group, on Unix (the containment makes the child its leader).
#[cfg(target_os = "linux")]
fn tree_pids(root: u32) -> Vec<u32> {
    let group = |pid: u32| -> Option<u32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after = &stat[stat.rfind(')')? + 2..];
        after.split_whitespace().nth(2)?.parse().ok()
    };
    std::fs::read_dir("/proc")
        .expect("read the process table")
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| group(*pid) == Some(root))
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn tree_pids(root: u32) -> Vec<u32> {
    vec![root]
}

/// Whether any command line in the tree rooted at `root` carries any needle. Only booleans leave this function.
fn tree_argv_leaks(root: u32, needles: &[&str]) -> bool {
    tree_pids(root).into_iter().any(|pid| {
        let argv = std::panic::catch_unwind(|| os_argv(pid)).unwrap_or_default();
        argv.iter().any(|arg| {
            needles
                .iter()
                .any(|needle| !needle.is_empty() && arg.contains(needle))
        })
    })
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

/// The page test's leak control alone: it needs no package, so the Windows box and every hosted leg can run it.
#[tokio::test]
async fn the_command_line_observer_rejects_a_deliberate_leak() {
    observer_rejects_a_deliberate_leak().await;
}

/// The observer rejects a deliberate leak: an owned process whose command line carries the needle is found, and one
/// that does not carry it is passed. The probe prints its pid, then sleeps. It is Julia, the kind the observation
/// watches, except on Windows: there Julia's loader splits its own command line in place, writing a NUL after each
/// argument (`cli/loader_win_utils.c` in every release from 1.6 to 1.13.1), so a reader sees only its executable
/// path, and the probe is PowerShell, whose command line keeps its arguments.
async fn observer_rejects_a_deliberate_leak() {
    let sig: &'static crate::lifecycle::child_signal::Signal =
        Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
    let mut probe = if cfg!(windows) {
        let mut probe = tokio::process::Command::new("powershell");
        probe.args([
            "-NoProfile",
            "-Command",
            "[Console]::Out.WriteLine($PID); [Console]::Out.Flush(); Start-Sleep -Seconds 60 # probe-needle-5d1e",
        ]);
        probe
    } else {
        let mut probe =
            tokio::process::Command::new(crate::sidecars::contract_tests::executable("julia"));
        probe.args([
            "--startup-file=no",
            "-e",
            "println(getpid()); flush(stdout); sleep(60)",
            "probe-needle-5d1e",
        ]);
        probe
    };
    probe.stdout(std::process::Stdio::piped());
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
    within(
        Duration::from_secs(30),
        "the probe's command line is observable",
        || tree_argv_leaks(pid, &["probe-needle-5d1e"]),
    )
    .await;
    assert!(
        tree_argv_leaks(pid, &["probe-needle-5d1e"]),
        "the observer must reject a leaking command line"
    );
    assert!(
        !tree_argv_leaks(pid, &["a-needle-it-does-not-carry"]),
        "the observer must pass a clean one"
    );
    let _ = owned.kill().await;
    sig.fire().expect("fire");
}

/// Adds WGLMakie to the fixture's workspace from the read depot, offline; its precompile is setup, bounded by `SETUP`.
async fn add_wglmakie(fixture: &Fixture) {
    offline_pkg(&fixture.root);
    let added = eval_within(
        &fixture.repl,
        1,
        "import Pkg; Pkg.offline(true); Pkg.add(\"WGLMakie\"; io=devnull); println(\"added\")",
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
        added
            .iter()
            .filter(|f| f["kind"] == "error")
            .map(|f| f["message"].to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
}

/// A page served by the real `wglshow` path never puts its secret, or the address that carries it, on a command
/// line of the REPL's tree, before or after the page exists, on every spawn route; a deliberately leaking process is
/// rejected by the same observer, and a sentinel proves the observation sees a value the child really has.
#[tokio::test]
async fn repl_page_secret_never_reaches_command_line() {
    if !isolated(
        "sidecars::repl::project_tests::repl_page_secret_never_reaches_command_line",
        SETUP + BODY,
    ) {
        return;
    }
    let read_depot =
        std::env::var_os("JULIA_DEPOT_PATH").expect("setup: the packages' depot is not named");
    std::env::set_var("SOT_TEST_PAGE_SENTINEL", SENTINEL);
    let fixture = Fixture::with_read_depot(Some(read_depot));
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
            .unwrap_or_else(|| panic!("{route}: the page was not announced"));
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
