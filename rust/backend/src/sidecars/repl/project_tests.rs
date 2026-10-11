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
/// The WGLMakie setup's bound. The fixture reads the depot list whole (`depot_path`), so the offline add reuses the
/// read depot's caches and compiles only the shim (5 s) when the depots were built on that list, as CI's are
/// (.github/wgl-depot.sh) and a developer's are. A read depot holding other versions recompiles those (25 packages on
/// one host). A depot read on a list it was not built on can recompile most of WGLMakie's environment (420 s on hosted
/// Linux, past 600 s on hosted Windows); this bound does not cover that.
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

    /// As `new`, with the depots of `read_depot` (a `JULIA_DEPOT_PATH` list) behind the owned depot, the list whole
    /// (`depot_path`): packages and their compiled caches are read from them, and every file
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

/// The running child's real argument vector, from the operating system, or `None` once the process has exited. On
/// Windows the OS keeps one command line, returned as a single element. On Windows a Julia child's arguments are not
/// observable this way: Julia's loader splits its own command line in place, so the reading stops at the executable
/// path.
#[cfg(target_os = "linux")]
fn os_argv(pid: u32) -> Option<Vec<String>> {
    let raw = match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(raw) => raw,
        Err(_) if !std::path::Path::new(&format!("/proc/{pid}")).exists() => return None,
        Err(e) => panic!("the command line of process {pid}, still alive, cannot be read: {e}"),
    };
    Some(
        raw.split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect(),
    )
}

#[cfg(target_os = "macos")]
fn os_argv(pid: u32) -> Option<Vec<String>> {
    // SAFETY: signal 0 only checks that the process exists.
    let gone = || unsafe {
        libc::kill(pid as libc::pid_t, 0) == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    };
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
    if first != 0 && gone() {
        return None;
    }
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
    if second != 0 && gone() {
        return None;
    }
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
    Some(
        rest[args_start..]
            .split(|b| *b == 0)
            .take(argc)
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect(),
    )
}

#[cfg(windows)]
fn os_argv(pid: u32) -> Option<Vec<String>> {
    let mut command = std::process::Command::new("powershell");
    command.args([
        "-NoProfile",
        "-Command",
        &format!(
            "Get-CimInstance Win32_Process -Filter 'ProcessId={pid}' | ForEach-Object {{ if ($null -eq $_.CommandLine) {{ 'NULL' }} else {{ 'CMD ' + $_.CommandLine }} }}"
        ),
    ]);
    let out = crate::lifecycle::child_signal::process()
        .output(&mut command)
        .expect("read the child's command line");
    assert!(out.status.success(), "the command-line reader: the CIM query failed");
    let text = String::from_utf8_lossy(&out.stdout);
    let row = text.trim();
    if row.is_empty() {
        return None;
    }
    let line = row
        .strip_prefix("CMD ")
        .unwrap_or_else(|| panic!("the command line of process {pid}, still alive, cannot be read"));
    Some(vec![line.to_owned()])
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

/// The process ids of the child's tree, read the way each OS can. Linux and macOS: its process group (the containment
/// makes the child its leader), from `/proc` and from one `ps -A -o pid=,pgid=` through the test's process seam.
/// Windows: the root and every descendant, walked from the root over one CIM query of `ProcessId, ParentProcessId`; a
/// descendant whose parent has already exited breaks the walk and is not seen (the daemon's job holds it, and the
/// Linux run covers that path).
#[cfg(target_os = "linux")]
fn read_tree(root: u32) -> Vec<u32> {
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

#[cfg(target_os = "macos")]
fn read_tree(root: u32) -> Vec<u32> {
    let mut command = std::process::Command::new("ps");
    command.args(["-A", "-o", "pid=,pgid="]);
    let out = crate::lifecycle::child_signal::process()
        .output(&mut command)
        .expect("read the process table");
    assert!(out.status.success(), "the tree reader: ps failed");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut cols = l.split_whitespace().map(|c| c.parse::<u32>().ok());
            let (pid, group) = (cols.next()??, cols.next()??);
            (group == root).then_some(pid)
        })
        .collect()
}

#[cfg(windows)]
fn read_tree(root: u32) -> Vec<u32> {
    let mut command = std::process::Command::new("powershell");
    command.args([
        "-NoProfile",
        "-Command",
        "Get-CimInstance Win32_Process | ForEach-Object { \"$($_.ProcessId) $($_.ParentProcessId)\" }",
    ]);
    let out = crate::lifecycle::child_signal::process()
        .output(&mut command)
        .expect("read the process table");
    assert!(out.status.success(), "the tree reader: the CIM query failed");
    let edges: Vec<(u32, u32)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut cols = l.split_whitespace().map(|c| c.parse::<u32>().ok());
            Some((cols.next()??, cols.next()??))
        })
        .collect();
    assert!(!edges.is_empty(), "the tree reader: the CIM query printed no process");
    assert!(edges.iter().any(|(pid, _)| *pid == root), "the tree reader did not list the root {root}");
    let mut tree = vec![root];
    let mut next = 0;
    while next < tree.len() {
        let parent = tree[next];
        next += 1;
        for (pid, _) in edges.iter().filter(|(_, up)| *up == parent) {
            if !tree.contains(pid) {
                tree.push(*pid);
            }
        }
    }
    tree
}

/// The tree's process ids, which must include the root: a reading that cannot list the root fails the test.
fn tree_pids(root: u32) -> Vec<u32> {
    let tree = read_tree(root);
    assert!(
        tree.contains(&root),
        "the tree reader did not list the root {root}"
    );
    tree
}

/// Whether any command line in the tree rooted at `root` carries any needle. Only booleans leave this function. A
/// process that has exited is skipped; a command line that cannot be read from a process still alive fails the test.
fn tree_argv_leaks(root: u32, needles: &[&str]) -> bool {
    tree_pids(root).into_iter().any(|pid| {
        os_argv(pid).is_some_and(|argv| {
            argv.iter().any(|arg| {
                needles
                    .iter()
                    .any(|needle| !needle.is_empty() && arg.contains(needle))
            })
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
