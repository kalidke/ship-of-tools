use super::spawn_supervisor;
use std::time::Duration;
use {
    super::{bound_pluto_port, seams, Ready, Running, Submission},
    crate::lifecycle::child_signal::Signal,
    crate::sidecars::contract_tests::{executable, isolated, within, OwnedRoot},
    std::path::{Path, PathBuf},
    tokio::sync::oneshot,
};

/// Starts a server and waits for its start to end: the `Running` once it serves, or the reason it failed.
async fn served(julia_bin: &str, project: &Path, script: &Path, sig: &'static Signal) -> anyhow::Result<Running> {
    let running = spawn_supervisor(julia_bin, project, script, sig).await?;
    let mut ready = running.ready.clone();
    let state = ready.wait_for(|state| *state != Ready::Starting).await.map(|state| state.clone());
    match state {
        Ok(Ready::Failed(why)) => Err(anyhow::anyhow!(why)),
        Ok(_) => Ok(running),
        Err(_) => Err(anyhow::anyhow!("the start ended without an answer")),
    }
}

/// The shutdown signal kills a Pluto child that has not yet said READY,
/// and the test waits for its containment registration.
#[cfg(unix)]
#[tokio::test]
async fn shutdown_kills_the_pluto_child_during_the_ready_wait() {
    let dir = tempfile::tempdir().unwrap();
    let stub = dir.path().join("stub-julia");
    let ready = dir.path().join("started");
    sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\necho started > '{}'\nexec sleep 30\n", ready.display()));
    let script = dir.path().join("start.jl");
    std::fs::write(&script, "").unwrap();
    let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
    let (bin, project) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
    let task = tokio::spawn(async move { served(&bin, &project, &script, sig).await.map(|_| ()) });
    let began = std::time::Instant::now();
    while std::fs::read_to_string(&ready).map_or(true, |text| text.trim() != "started") {
        assert!(began.elapsed() < Duration::from_secs(5), "the stub child never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    sig.fire().expect("fire");
    let result = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("the READY wait outlived the shutdown")
        .expect("spawn task");
    assert!(result.is_err());
    assert!(sig.held_groups().is_empty());
}

/// A process the sidecar started dies with it: the stub starts a
/// grandchild, says READY, and the shutdown must take both.
#[cfg(unix)]
#[tokio::test]
async fn pluto_grandchild_dies_with_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let gc_file = dir.path().join("gc");
    let gc = crate::lifecycle::child_signal::tests::Leftover::of_file(gc_file.clone());
    let stub = dir.path().join("stub-julia");
    sot_log::test_exec::write_executable(
        &stub,
        format!("#!/bin/sh\nsleep 3103 &\necho $! > {}\necho \"READY http://127.0.0.1:1/\"\nexec sleep 3103\n", gc_file.display()),
    );
    let script = dir.path().join("start.jl");
    std::fs::write(&script, "").unwrap();
    let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
    let (bin, project) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
    let tx = served(&bin, &project, &script, sig).await.expect("spawn_supervisor");
    let began = std::time::Instant::now();
    while std::fs::read_to_string(&gc_file).map(|s| s.trim().is_empty()).unwrap_or(true) {
        assert!(began.elapsed() < Duration::from_secs(5), "the stub never wrote its grandchild pid");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    sig.fire().expect("fire");
    let gone = gc.gone();
    drop(tx);
    assert!(gone, "the Pluto grandchild survived the shutdown");
}

/// A Pluto whose first boot outlasts the old 180 s bound (a cold precompile) is served, not ended. The paused clock
/// passes 181 s while the stand-in waits, then the stand-in says READY; a running blocking task keeps tokio from
/// moving the clock on its own (`tokio::time::pause`), so every wait here is the test's real-time poll.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_pluto_ready_after_the_old_180s_bound_is_served() {
    let dir = tempfile::tempdir().unwrap();
    let (started, go) = (dir.path().join("started"), dir.path().join("go"));
    let stub = dir.path().join("stub-julia");
    sot_log::test_exec::write_executable(
        &stub,
        format!(
            "#!/bin/sh\necho x > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.05; done\necho 'READY http://pluto.invalid/'\nexec sleep 30\n",
            started.display(),
            go.display()
        ),
    );
    let script = dir.path().join("start.jl");
    std::fs::write(&script, "").unwrap();
    let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
    let (hold, held) = std::sync::mpsc::channel::<()>();
    let still = tokio::task::spawn_blocking(move || held.recv());
    let (bin, project) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
    let task = tokio::spawn(async move { served(&bin, &project, &script, sig).await });
    async fn real_wait(what: &str, mut done: impl FnMut() -> bool) {
        let began = std::time::Instant::now();
        while !done() {
            assert!(began.elapsed() < Duration::from_secs(20), "not within 20 s: {what}");
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    // The stand-in runs only after the supervisor's first poll has set the READY deadline.
    real_wait("the stand-in starts", || started.exists()).await;
    tokio::time::advance(Duration::from_secs(181)).await;
    std::fs::write(&go, "").unwrap();
    real_wait("the start ends", || task.is_finished()).await;
    drop(hold);
    let _ = still.await;
    let running = task.await.expect("the start task").unwrap_or_else(|e| panic!("a Pluto that says READY after 181 s was refused: {e:#}"));
    sig.fire().expect("fire");
    drop(running);
    real_wait("the stand-in is reaped", || sig.live() == 0).await;
}

/// Dropping the Pluto handle during a start ends the child: the supervisor's `ended` arm retires it, so no start
/// outlives its handle until `PLUTO_READY_BOUND`.
#[cfg(unix)]
#[tokio::test]
async fn dropping_the_handle_during_a_start_ends_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let stub = dir.path().join("stub-julia");
    let started = dir.path().join("started");
    sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\necho started > '{}'\nexec sleep 30\n", started.display()));
    let script = dir.path().join("start.jl");
    std::fs::write(&script, "").unwrap();
    let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
    let running = spawn_supervisor(&stub.to_string_lossy(), dir.path(), &script, sig).await.expect("spawn_supervisor");
    within(Duration::from_secs(5), "the stand-in starts", || started.exists()).await;
    assert!(*running.ready.borrow() == Ready::Starting, "the start has not ended");
    drop(running);
    within(Duration::from_secs(5), "the start's child is ended with its handle", || sig.live() == 0).await;
}

/// A Pluto that fails its boot leaves its own words in the log at warn: the stand-in writes Julia's error to stderr
/// and exits before READY.
#[cfg(unix)]
#[tokio::test]
async fn a_pluto_that_fails_its_boot_logs_its_stderr_at_warn() {
    let log = sot_log::test_log::capture();
    let dir = tempfile::tempdir().unwrap();
    let stub = dir.path().join("stub-julia");
    sot_log::test_exec::write_executable(&stub, "#!/bin/sh\necho 'ERROR: LoadError: (pluto-boot-red)' >&2\nexit 1\n");
    let script = dir.path().join("start.jl");
    std::fs::write(&script, "").unwrap();
    let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
    let (bin, project) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
    let err = served(&bin, &project, &script, sig).await.err().expect("a child that exits before READY");
    assert!(format!("{err:#}").contains("before READY"), "{err:#}");
    within(Duration::from_secs(5), "the child's stderr is in the log at warn", || crate::sidecars::warned(&log.text(), "pluto-boot-red")).await;
}

/// A Pluto line may hold any byte (an `ERR` line quotes what failed): the open it answers gets U+FFFD for the byte,
/// and the same server answers the next open. The READY address is no loopback page, so no proxy grant is published.
#[cfg(unix)]
#[tokio::test]
async fn a_pluto_line_holding_an_invalid_byte_keeps_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let stub = dir.path().join("stub-julia");
    sot_log::test_exec::write_executable(
        &stub,
        "#!/bin/sh\necho 'READY http://pluto.invalid/'\nread l\nprintf 'ERR a\\377b\\n'\nread l\necho 'URL http://pluto.invalid/edit'\nexec sleep 600\n".to_string(),
    );
    let script = dir.path().join("start.jl");
    std::fs::write(&script, "").unwrap();
    let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
    let (bin, project) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
    let running = served(&bin, &project, &script, sig).await.expect("spawn_supervisor");
    let mut answers = Vec::new();
    for path in ["/a.jl", "/b.jl"] {
        let (reply, rx) = oneshot::channel();
        let answer = match running.submit.send(Submission { abs_path: path.to_string(), reply }).await {
            Ok(()) => match tokio::time::timeout(Duration::from_secs(10), rx).await.expect("an answer within 10 s") {
                Ok(answer) => answer.map_err(|e| format!("{e:#}")),
                Err(_) => Err("the reply was dropped".to_string()),
            },
            Err(_) => Err("the server ended".to_string()),
        };
        answers.push(answer);
    }
    sig.fire().expect("fire");
    drop(running);
    assert_eq!(answers, [Err("pluto: a\u{fffd}b".to_string()), Ok("http://pluto.invalid/edit".to_string())]);
}

/// The longest an isolated body here may take.
const BODY: Duration = Duration::from_secs(180);
/// How long a fixture waits for the supervisor to react to the child's exit or the Signal.
const REACT: Duration = Duration::from_secs(30);

/// An owned `start.jl` standing in for Pluto's: it binds a real loopback listener, says READY, records its pid,
/// optionally starts a descendant that keeps the child's pipes, and exits when the test creates `gate`.
fn start_script(dir: &Path, ready_url: &str, descendant: &str) -> PathBuf {
    let script = dir.join("start.jl");
    let text = format!(
        r#"using Sockets
dir = raw"{dir}"
server = listen(ip"127.0.0.1", 0)
port = Int(getsockname(server)[2])
write(joinpath(dir, "port"), string(port))
write(joinpath(dir, "pid"), string(getpid()))
println({ready}); flush(stdout)
{descendant}
while !isfile(joinpath(dir, "gate")) sleep(0.05) end
write(joinpath(dir, "exiting"), "x")
exit(0)
"#,
        dir = dir.display(),
        ready = ready_url,
    );
    std::fs::write(&script, text).unwrap();
    script
}

const READY_LOCAL: &str = r#""READY http://127.0.0.1:$port""#;
/// On Windows libuv puts every child it starts in a job that dies with its parent, so the descendant is detached
/// there to outlive the child (it stays in the daemon's containment job); on Unix it stays in the child's group.
/// A descendant that holds only the child's stdout.
const KEEP_STDOUT: &str = r#"cmd = `$(Base.julia_cmd()) --startup-file=no -e "sleep(300)"`; p = run(pipeline(Sys.iswindows() ? detach(cmd) : cmd, stdout=stdout), wait=false); write(joinpath(dir, "desc"), string(getpid(p)))"#;
/// A descendant that holds the read end of the child's stdin and the write end of its stdout, reading nothing.
const KEEP_PIPES: &str = r#"cmd = `$(Base.julia_cmd()) --startup-file=no -e "sleep(300)"`; p = run(pipeline(Sys.iswindows() ? detach(cmd) : cmd, stdin=stdin, stdout=stdout), wait=false); write(joinpath(dir, "desc"), string(getpid(p)))"#;

struct Harness {
    tx: Running,
    owned: OwnedRoot,
}

impl Harness {
    async fn start(ready_url: &str, descendant: &str) -> Harness {
        let owned = OwnedRoot::private();
        let script = start_script(owned.path(), ready_url, descendant);
        let julia = executable("julia").to_string_lossy().into_owned();
        let tx = served(&julia, owned.path(), &script, owned.sig()).await.expect("the fixture says READY");
        Harness { tx, owned }
    }

    fn dir(&self) -> &Path {
        self.owned.path()
    }

    fn sig(&self) -> &'static Signal {
        self.owned.sig()
    }

    fn number(&self, name: &str) -> Option<u32> {
        std::fs::read_to_string(self.dir().join(name)).ok()?.trim().parse().ok()
    }

    async fn wait_number(&self, name: &str) -> u32 {
        within(REACT, name, || self.number(name).is_some()).await;
        self.number(name).unwrap()
    }

    async fn submit(&self, path: String) -> oneshot::Receiver<anyhow::Result<String>> {
        let (reply, rx) = oneshot::channel();
        self.tx.submit.send(Submission { abs_path: path, reply }).await.expect("the supervisor admits a submission");
        rx
    }

    /// Lets the child exit on its own.
    fn open_gate(&self) {
        std::fs::write(self.dir().join("gate"), "x").unwrap();
    }

    async fn finish(self) {
        self.owned.end().await;
    }
}

/// Whether the fixture's child has begun exiting: its script writes `exiting` as its last act before `exit(0)`,
/// so the observation is the same on every platform (the supervisor, not the test, waits for the child).
fn exited(h: &Harness) -> bool {
    h.dir().join("exiting").exists()
}

/// Whether a process of this id is running (a zombie is not).
#[cfg(unix)]
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only probes.
    let probe = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
    #[cfg(target_os = "linux")]
    if std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| stat.rsplit_once(')').map(|(_, rest)| rest.trim_start().starts_with('Z')))
        .unwrap_or(false)
    {
        return false;
    }
    probe
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: a plain query on a process handle opened and closed here.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code == 259
    }
}

async fn errors(rx: oneshot::Receiver<anyhow::Result<String>>, what: &str) {
    let got = tokio::time::timeout(REACT, rx).await.unwrap_or_else(|_| panic!("{what}: no answer"));
    assert!(matches!(got, Ok(Err(_))), "{what}: must end with an error");
}

fn granted(port: u16) -> bool {
    bound_pluto_port() == Some(port) || crate::pages::proxy::allowed_proxy_ports().contains(&port)
}

/// The child's own exit, with no descendant, takes the proxy grant.
#[tokio::test]
async fn death_releases_proxy_grant() {
    if !isolated("sidecars::pluto::port_parse_tests::death_releases_proxy_grant", BODY) {
        return;
    }
    let h = Harness::start(READY_LOCAL, "").await;
    let port = h.wait_number("port").await as u16;
    assert!(granted(port), "setup: READY grants the port");
    h.open_gate();
    within(REACT, "the child exits", || exited(&h)).await;
    within(REACT, "the grant is released", || !granted(port)).await;
    h.finish().await;
}

/// A descendant that keeps stdout open past the child's exit does not keep the grant.
#[tokio::test]
async fn inherited_stdout_does_not_keep_grant() {
    if !isolated("sidecars::pluto::port_parse_tests::inherited_stdout_does_not_keep_grant", BODY) {
        return;
    }
    *seams::CLEANUP_GATE.lock().unwrap() = Some(std::sync::Arc::new(tokio::sync::Semaphore::new(0)));
    let h = Harness::start(READY_LOCAL, KEEP_STDOUT).await;
    let port = h.wait_number("port").await as u16;
    let descendant = h.wait_number("desc").await;
    h.open_gate();
    within(REACT, "the child exits", || exited(&h)).await;
    within(REACT, "the grant is released", || !granted(port)).await;
    assert!(alive(descendant), "the release must not wait for cleanup or EOF");
    let gate = seams::CLEANUP_GATE.lock().unwrap().clone().unwrap();
    gate.add_permits(1);
    within(REACT, "cleanup ends the descendant", || !alive(descendant)).await;
    h.finish().await;
}

/// A READY line that is not a loopback page address grants nothing.
#[tokio::test]
async fn nonloopback_ready_grants_nothing() {
    if !isolated("sidecars::pluto::port_parse_tests::nonloopback_ready_grants_nothing", BODY) {
        return;
    }
    // A documentation-range address: never dialled.
    let h = Harness::start(r#""READY http://192.0.2.7:4000""#, "").await;
    assert_eq!(bound_pluto_port(), None);
    assert!(!crate::pages::proxy::allowed_proxy_ports().contains(&4000));
    h.finish().await;
}

/// A Pluto child the daemon kills leaves no temporary file: what it made in its temporary folder goes with the
/// folder after the supervisor's reap.
#[tokio::test]
async fn a_killed_child_leaves_no_temp_files() {
    if !isolated("sidecars::pluto::port_parse_tests::a_killed_child_leaves_no_temp_files", BODY) {
        return;
    }
    // This process's temporary folder is an owned one, so a file the child leaves outside its own folder stays
    // here.
    let tmp = tempfile::tempdir().expect("owned temporary folder");
    for key in if cfg!(windows) { &["TMP", "TEMP"][..] } else { &["TMPDIR"][..] } {
        std::env::set_var(key, tmp.path());
    }
    let h = Harness::start(READY_LOCAL, r#"write(joinpath(dir, "made"), mktemp()[1])"#).await;
    within(REACT, "the child makes its temporary file", || h.dir().join("made").exists()).await;
    let made = PathBuf::from(std::fs::read_to_string(h.dir().join("made")).unwrap());
    assert!(made.exists(), "setup: the child's temporary file exists");
    h.sig().fire().expect("fire");
    within(REACT, "the killed child's temporary file is gone", || !made.exists()).await;
    h.finish().await;
}

/// Pluto's server has a display of its own (Linux), which its notebook workers inherit with the rest of its
/// environment, and which ends with the server: the child's own exit here, as a crash would end it. A worker the
/// server started in a session of its own (as Distributed starts each notebook's) ends with that display.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn pluto_has_its_own_display_which_ends_with_it() {
    use crate::sidecars::contract_tests::{holding_display, x11_answer, x11_cookie};
    if !isolated("sidecars::pluto::port_parse_tests::pluto_has_its_own_display_which_ends_with_it", BODY) {
        return;
    }
    executable("Xvfb");
    let record = r#"run(pipeline(detach(`sleep 300`), stdin=devnull, stdout=devnull, stderr=devnull); wait=false); write(joinpath(dir, "display.tmp"), get(ENV, "DISPLAY", "-"), "\n", get(ENV, "XAUTHORITY", "-")); mv(joinpath(dir, "display.tmp"), joinpath(dir, "display"))"#;
    let h = Harness::start(READY_LOCAL, record).await;
    within(REACT, "the server records its display", || h.dir().join("display").exists()).await;
    let named = std::fs::read_to_string(h.dir().join("display")).unwrap();
    let (display, path) = named.split_once('\n').expect("two lines");
    let number: u32 = display.strip_prefix(':').and_then(|n| n.parse().ok()).unwrap_or_else(|| {
        panic!("the server names a display of its own: {named:?}")
    });
    let cookie = x11_cookie(Path::new(path));
    assert_eq!(x11_answer(number, Some(&cookie)), Some(1), "the server's cookie opens its display");
    let held = || holding_display(Path::new(path));
    within(REACT, "setup: the server and its worker hold the display", || held().len() == 2).await;
    h.open_gate();
    within(REACT, "the server's display ends with it", || x11_answer(number, Some(&cookie)) != Some(1)).await;
    within(REACT, "the server's worker ends with its display", || held().is_empty()).await;
    h.finish().await;
}

/// A stand-in for Pluto's julia that names its display in the file `named` (before READY, when `ready`), starts a
/// descendant in its group and a worker in a session of its own (as Distributed starts a notebook's) when
/// `descendant`, and reads no request; with the Signal it is started under.
#[cfg(target_os = "linux")]
fn display_stand_in(dir: &Path, named: &Path, ready: bool, descendant: bool) -> (String, PathBuf, &'static Signal) {
    let stub = dir.join("stub-julia");
    sot_log::test_exec::write_executable(
        &stub,
        format!(
            "#!/bin/sh\n{child}printf '%s' \"$XAUTHORITY\" > '{named}.tmp'\nmv '{named}.tmp' '{named}'\n{ready}exec sleep 300\n",
            child = if descendant { "sleep 300 &\nsetsid sleep 300 &\n" } else { "" },
            named = named.display(),
            ready = if ready { "echo 'READY http://pluto.invalid/'\n" } else { "" },
        ),
    );
    let script = dir.join("start.jl");
    std::fs::write(&script, "").unwrap();
    (stub.to_string_lossy().into_owned(), script, Box::leak(Box::new(Signal::new())))
}

/// A Pluto server whose display ends under it ends with what it started, in its group or in a session of its own,
/// so nothing of it can open that display's name once another account binds it, and an open in flight says why.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_pluto_whose_display_ends_ends_with_it() {
    use crate::sidecars::contract_tests::{holding_display, x11_server};
    executable("Xvfb");
    executable("setsid");
    let dir = tempfile::tempdir().unwrap();
    let named = dir.path().join("named");
    let (bin, script, sig) = display_stand_in(dir.path(), &named, true, true);
    let running = served(&bin, dir.path(), &script, sig).await.expect("the stand-in says READY");
    let cookie = PathBuf::from(std::fs::read_to_string(&named).expect("the stand-in names its display"));
    within(REACT, "setup: the server, its sleep and its worker hold the display", || {
        holding_display(&cookie).len() == 3
    })
    .await;
    let (reply, open) = oneshot::channel();
    running.submit.send(Submission { abs_path: "/nowhere.jl".into(), reply }).await.expect("the supervisor admits an open");
    // SAFETY: a signal to the X server started for this test's stand-in.
    assert_eq!(unsafe { libc::kill(x11_server(&cookie) as libc::pid_t, libc::SIGKILL) }, 0, "kill the display's server");
    within(REACT, "nothing of the server keeps its ended display", || holding_display(&cookie).is_empty()).await;
    let answer = tokio::time::timeout(REACT, open).await.expect("the open is answered").expect("the supervisor answers");
    let why = format!("{:#}", answer.expect_err("an open the display's end cut short fails"));
    assert!(why.contains("its display ended"), "{why}");
    within(REACT, "the server and its display are reaped", || sig.live() == 0).await;
    drop(running);
}

/// A display that ends while its server starts ends the start: the server is ended, and the start fails saying why.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_pluto_whose_display_ends_before_ready_is_not_served() {
    use crate::sidecars::contract_tests::{holding_display, x11_server};
    executable("Xvfb");
    let dir = tempfile::tempdir().unwrap();
    let named = dir.path().join("named");
    let (bin, script, sig) = display_stand_in(dir.path(), &named, false, false);
    let project = dir.path().to_path_buf();
    let start = tokio::spawn(async move { served(&bin, &project, &script, sig).await.map(|_| ()) });
    within(REACT, "the stand-in names its display", || named.exists()).await;
    let cookie = PathBuf::from(std::fs::read_to_string(&named).unwrap());
    // SAFETY: a signal to the X server started for this test's stand-in.
    assert_eq!(unsafe { libc::kill(x11_server(&cookie) as libc::pid_t, libc::SIGKILL) }, 0, "kill the display's server");
    let started = tokio::time::timeout(REACT, start).await.expect("the start ends with its display").expect("the start task");
    let why = format!("{:#}", started.expect_err("a server whose display ended is not served"));
    assert!(why.contains("display ended before READY"), "{why}");
    assert!(holding_display(&cookie).is_empty(), "nothing of the server keeps its ended display");
    assert_eq!(sig.live(), 0, "the server and its display are reaped");
}

/// A stdin write that cannot complete: three requests are in flight (one answered by nobody, one mid-write, one
/// queued) when the child exits with a descendant holding both pipes.
async fn blocked_write_scenario(h: &Harness) -> (u16, u32, Vec<oneshot::Receiver<anyhow::Result<String>>>) {
    let port = h.wait_number("port").await as u16;
    let descendant = h.wait_number("desc").await;
    let first = h.submit("a".to_string()).await;
    let before = seams::WRITE_PENDING_POLLS.load(std::sync::atomic::Ordering::SeqCst);
    let big = h.submit("x".repeat(4 * 1024 * 1024)).await;
    within(REACT, "the large write is observed blocked", || {
        seams::WRITE_PENDING_POLLS.load(std::sync::atomic::Ordering::SeqCst) > before
    })
    .await;
    let queued = h.submit("c".to_string()).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(alive(h.number("pid").unwrap()) && alive(descendant), "setup: child and descendant are running");
    assert!(granted(port), "setup: the grant is held");
    (port, descendant, vec![first, big, queued])
}

#[tokio::test]
async fn blocked_write_death_releases_grant_and_closes_requests() {
    if !isolated("sidecars::pluto::port_parse_tests::blocked_write_death_releases_grant_and_closes_requests", BODY) {
        return;
    }
    let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    *seams::CLEANUP_GATE.lock().unwrap() = Some(gate.clone());
    let h = Harness::start(READY_LOCAL, KEEP_PIPES).await;
    let (port, descendant, replies) = blocked_write_scenario(&h).await;
    h.open_gate();
    within(REACT, "the child exits", || exited(&h)).await;
    within(REACT, "the grant is released", || !granted(port)).await;
    assert!(alive(descendant), "revocation precedes cleanup: the descendant still holds the pipes");
    for (i, rx) in replies.into_iter().enumerate() {
        errors(rx, &format!("request {i}")).await;
    }
    assert!(alive(descendant), "closeout precedes cleanup");
    gate.add_permits(1);
    within(REACT, "cleanup ends the descendant", || !alive(descendant)).await;
    h.finish().await;
}

/// A request whose permit was taken before the close-out's close and pushed after it is answered by the
/// close-out, not dropped with the receiver.
#[tokio::test]
async fn a_request_pushed_after_the_close_is_answered() {
    if !isolated("sidecars::pluto::port_parse_tests::a_request_pushed_after_the_close_is_answered", BODY) {
        return;
    }
    let h = Harness::start(READY_LOCAL, "").await;
    let permit = h.tx.submit.reserve().await.expect("a permit before the close");
    h.open_gate();
    within(REACT, "the close-out closes the queue", || h.tx.submit.is_closed()).await;
    let (reply, rx) = oneshot::channel();
    permit.send(Submission { abs_path: "x".into(), reply });
    let answered = tokio::time::timeout(REACT, rx).await.expect("an answer in time");
    assert!(matches!(answered, Ok(Err(_))), "the close-out answers it with the sidecar's end: {answered:?}");
    h.finish().await;
}

#[tokio::test]
async fn signal_cancels_blocked_pluto_write() {
    if !isolated("sidecars::pluto::port_parse_tests::signal_cancels_blocked_pluto_write", BODY) {
        return;
    }
    let h = Harness::start(READY_LOCAL, KEEP_PIPES).await;
    let (port, _descendant, replies) = blocked_write_scenario(&h).await;
    h.sig().fire().expect("fire");
    within(REACT, "the grant is released", || !granted(port)).await;
    for (i, rx) in replies.into_iter().enumerate() {
        errors(rx, &format!("request {i}")).await;
    }
    h.finish().await;
}

/// A flush that cannot finish gives way to the child's exit like a write does: the grant is released and the
/// current and pending requests end with errors.
#[tokio::test]
async fn flush_blocked_death_releases_grant_and_closes_requests() {
    if !isolated("sidecars::pluto::port_parse_tests::flush_blocked_death_releases_grant_and_closes_requests", BODY) {
        return;
    }
    let h = Harness::start(READY_LOCAL, "").await;
    let port = h.wait_number("port").await as u16;
    let first = h.submit("a".to_string()).await;
    let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    *seams::FLUSH_GATE.lock().unwrap() = Some(gate.clone());
    let second = h.submit("b".to_string()).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(granted(port), "setup: the grant is held while the flush waits");
    h.open_gate();
    within(REACT, "the child exits", || exited(&h)).await;
    within(REACT, "the grant is released", || !granted(port)).await;
    errors(first, "first").await;
    errors(second, "second").await;
    h.finish().await;
}

/// A flush that fails ends the supervisor the same way: grant released, requests failed.
#[tokio::test]
async fn flush_error_releases_grant_and_closes_requests() {
    if !isolated("sidecars::pluto::port_parse_tests::flush_error_releases_grant_and_closes_requests", BODY) {
        return;
    }
    let h = Harness::start(READY_LOCAL, "").await;
    let port = h.wait_number("port").await as u16;
    seams::FAIL_FLUSH.store(true, std::sync::atomic::Ordering::SeqCst);
    let only = h.submit("a".to_string()).await;
    errors(only, "the request whose flush failed").await;
    within(REACT, "the grant is released", || !granted(port)).await;
    h.finish().await;
}
