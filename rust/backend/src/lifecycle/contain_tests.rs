use crate::lifecycle::child_signal::Signal;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};

/// A retained observation handle; a reported PID never becomes a termination target.
struct Watched(windows_sys::Win32::Foundation::HANDLE);

impl Watched {
    fn open(pid: u32) -> Result<Self, String> {
        // SAFETY: an observation-only process handle, closed by Watched.
        let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            Err(format!(
                "process {pid} could not be opened to watch: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(Self(handle))
        }
    }

    fn exits_within(&self, ms: u32) -> Result<bool, String> {
        // SAFETY: a wait on the retained observation handle; no process is signalled.
        match unsafe { WaitForSingleObject(self.0, ms) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            WAIT_FAILED => Err(format!(
                "native process wait failed: {}",
                std::io::Error::last_os_error()
            )),
            result => Err(format!("unexpected native process wait result: {result}")),
        }
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        // SAFETY: the handle is this value's own.
        unsafe { CloseHandle(self.0) };
    }
}

/// One tree fixture's output: stdout and stderr are files in the fixture's own folder, so no reader exists to
/// block the tree or to supply its death. Readiness is stdout's first complete numeric line (a grandchild that
/// shares stdout, like `ping`, may write first); a failure carries a bounded excerpt of both files.
struct TreeFiles {
    dir: tempfile::TempDir,
    leader: Arc<AtomicU32>,
}

impl TreeFiles {
    /// Points `cmd`'s standard handles at this fixture's files and records the root that `sig` creates.
    fn new(sig: &'static Signal, cmd: &mut std::process::Command) -> Result<Self, String> {
        let dir = tempfile::tempdir().map_err(|error| format!("fixture folder: {error}"))?;
        let file = |name: &str| {
            std::fs::File::create(dir.path().join(name))
                .map_err(|error| format!("fixture {name} file: {error}"))
        };
        cmd.stdin(std::process::Stdio::null())
            .stdout(file("stdout")?)
            .stderr(file("stderr")?);
        let leader = Arc::new(AtomicU32::new(0));
        let recorded = leader.clone();
        // The private signal creates exactly one tree, so its hook is never reset.
        *sig.after_create.lock().unwrap() = Some(Box::new(move |pid| {
            recorded.store(pid, Ordering::SeqCst);
        }));
        Ok(Self { dir, leader })
    }

    /// The recorded root, opened while its owner still holds it.
    fn leader(&self) -> Result<Watched, String> {
        Watched::open(self.leader.load(Ordering::SeqCst))
    }

    /// The descendant's PID from stdout's first complete numeric line. The leader is checked before stdout is
    /// read, so a line written before it exited is still found; an exited leader or 30 s without one is an error.
    fn ready(&self, leader: &Watched) -> Result<u32, String> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let exited = leader.exits_within(0)?;
            let stdout = std::fs::read(self.dir.path().join("stdout"))
                .map_err(|error| format!("read fixture stdout: {error}"))?;
            let complete = stdout
                .iter()
                .rposition(|&byte| byte == b'\n')
                .map_or(0, |end| end + 1);
            if let Some(pid) = String::from_utf8_lossy(&stdout[..complete])
                .lines()
                .find_map(|line| line.trim().parse().ok())
            {
                return Ok(pid);
            }
            if exited {
                return Err("fixture leader exited before descendant readiness".into());
            }
            if Instant::now() >= deadline {
                return Err("descendant readiness exceeded 30 s".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The observation, with both files' excerpts appended to an error.
    fn result(&self, observation: Result<(), String>) -> Result<(), String> {
        observation.map_err(|reason| {
            format!(
                "{reason}\n{}\n{}",
                self.excerpt("stdout"),
                self.excerpt("stderr")
            )
        })
    }

    /// A stream's first 4 KiB and last 12 KiB, with the count of bytes left out between them.
    fn excerpt(&self, name: &str) -> String {
        let bytes = match std::fs::read(self.dir.path().join(name)) {
            Ok(bytes) => bytes,
            Err(error) => return format!("{name}: unreadable: {error}"),
        };
        let (head, tail) = if bytes.len() > 16 * 1024 {
            (&bytes[..4 * 1024], &bytes[bytes.len() - 12 * 1024..])
        } else {
            (&bytes[..], &bytes[..0])
        };
        format!(
            "{name}: {} bytes, {} omitted between head and tail\n{}\n{}",
            bytes.len(),
            bytes.len() - head.len() - tail.len(),
            String::from_utf8_lossy(head),
            String::from_utf8_lossy(tail)
        )
    }
}

/// Waits for readiness, applies `end` (the one termination under test), then requires the leader and the
/// descendant to exit within one shared 3 s deadline. An owner moved into `end` drops with it on every path.
fn tree_ends(
    files: &TreeFiles,
    end: impl FnOnce(&Watched, &Watched) -> Result<(), String>,
) -> Result<(), String> {
    let leader = files.leader()?;
    let descendant = Watched::open(files.ready(&leader)?)?;
    end(&leader, &descendant)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    for (label, watched) in [("leader", &leader), ("descendant", &descendant)] {
        let millis = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u32;
        if !watched.exits_within(millis)? {
            return Err(format!("{label} survived past the shared 3 s deadline"));
        }
    }
    Ok(())
}

/// Fire alone supplies the death: the owner outlives the observation.
fn fire_takes_the_tree(mut cmd: tokio::process::Command) -> Result<(), String> {
    let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
    let files = TreeFiles::new(sig, cmd.as_std_mut())?;
    let owner = sig
        .spawn(&mut cmd)
        .map_err(|error| format!("spawn through the signal: {error}"))?;
    let observation = tree_ends(&files, |_, _| {
        sig.fire().map_err(|error| format!("fire: {error}"))
    });
    drop(owner);
    files.result(observation)
}

fn std_drop_takes_the_tree(mut cmd: std::process::Command) -> Result<(), String> {
    let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
    let files = TreeFiles::new(sig, &mut cmd)?;
    let owner = sig
        .spawn_std(&mut cmd)
        .map_err(|error| format!("spawn_std: {error}"))?;
    files.result(tree_ends(&files, move |_, _| {
        drop(owner);
        Ok(())
    }))
}

/// Retaining the owner keeps the tree live; dropping it, with no fire, is the only termination.
fn async_drop_takes_the_tree(mut cmd: tokio::process::Command) -> Result<(), String> {
    let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
    let files = TreeFiles::new(sig, cmd.as_std_mut())?;
    let owner = sig
        .spawn(&mut cmd)
        .map_err(|error| format!("spawn through a private signal: {error}"))?;
    files.result(tree_ends(&files, move |leader, descendant| {
        if leader.exits_within(100)? || descendant.exits_within(0)? {
            return Err("retained-owner control observed tree death".into());
        }
        drop(owner);
        Ok(())
    }))
}

fn ping_tree() -> std::process::Command {
    let mut cmd = std::process::Command::new("powershell");
    cmd.args([
        "-NoProfile", "-Command",
        "$p=Start-Process -PassThru -NoNewWindow ping -ArgumentList '-n','600','127.0.0.1'; $p.Id; Start-Sleep 600",
    ]);
    cmd
}

#[tokio::test]
async fn fire_kills_a_windows_tree() {
    fire_takes_the_tree(ping_tree().into()).expect("fire fixture");
}

#[test]
fn a_dropped_std_child_takes_its_windows_tree() {
    std_drop_takes_the_tree(ping_tree()).expect("std Drop fixture");
}

#[tokio::test]
async fn a_dropped_async_child_takes_its_windows_tree() {
    async_drop_takes_the_tree(ping_tree().into()).expect("async Drop fixture");
    println!("WINDOWS async Drop PASS: live leader and descendant exited after owner drop; signal not fired");
}

/// 128 KiB on each stream between a prefix and a suffix line, then exit with no numeric line.
fn pressure_command() -> std::process::Command {
    let mut cmd = std::process::Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-Command",
        concat!(
            "$data='x' * 131072; ",
            "[Console]::Out.WriteLine('STDOUT-PREFIX'); [Console]::Out.Write($data); ",
            "[Console]::Out.WriteLine('STDOUT-SUFFIX'); [Console]::Out.Flush(); ",
            "[Console]::Error.WriteLine('STDERR-PREFIX'); [Console]::Error.Write($data); ",
            "[Console]::Error.WriteLine('STDERR-SUFFIX'); [Console]::Error.Flush(); exit 0"
        ),
    ]);
    cmd
}

#[tokio::test]
async fn all_windows_tree_fixtures_keep_bounded_diagnostics() {
    for mode in ["std", "async", "fire"] {
        let cmd = pressure_command();
        let result = match mode {
            "std" => std_drop_takes_the_tree(cmd),
            "async" => async_drop_takes_the_tree(cmd.into()),
            _ => fire_takes_the_tree(cmd.into()),
        };
        let text = result.expect_err("a fixture that printed no PID became ready");
        let missing: Vec<_> = [
            "fixture leader exited before descendant readiness",
            "STDOUT-PREFIX",
            "STDOUT-SUFFIX",
            "STDERR-PREFIX",
            "STDERR-SUFFIX",
        ]
        .into_iter()
        .filter(|item| !text.contains(item))
        .collect();
        assert!(
            missing.is_empty() && text.len() < 34 * 1024,
            "{mode} fixture: missing {missing:?} or longer than 34 KiB ({} bytes): {text}",
            text.len()
        );
    }
    println!(
        "WINDOWS shared fixture diagnostics PASS: std, async and fire retain both streams"
    );
}

#[test]
fn contain_job_permits_no_breakaway() {
    use windows_sys::Win32::System::JobObjects::{
        JobObjectExtendedLimitInformation, QueryInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    let job = sot_log::capsule::producer::conpty::AnonymousJob::create().unwrap();
    // SAFETY: a plain query into a zeroed, correctly sized struct.
    let flags = unsafe {
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        let ok = QueryInformationJobObject(
            job.raw(),
            JobObjectExtendedLimitInformation,
            &mut info as *mut _ as *mut _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            std::ptr::null_mut(),
        );
        assert!(
            ok != 0,
            "QueryInformationJobObject: {}",
            std::io::Error::last_os_error()
        );
        info.BasicLimitInformation.LimitFlags
    };
    assert_eq!(
        flags, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        "the containment job permits a breakaway"
    );
}

/// Needs julia; started the way the daemon starts every julia.
#[tokio::test]
#[ignore = "needs julia"]
async fn fire_kills_a_julia_detached_grandchild_windows() {
    let (julia, _) = crate::sidecars::julia::resolve_bin().expect("resolve julia");
    let mut cmd = tokio::process::Command::new(julia);
    cmd.args([
        "-e",
        "run(`cmd /c exit 0`); p=run(detach(`ping -n 600 127.0.0.1`);wait=false);println(getpid(p));flush(stdout);sleep(600)",
    ]);
    fire_takes_the_tree(cmd).expect("julia fire fixture");
}

#[tokio::test]
#[ignore = "needs Git for Windows bash"]
async fn fire_kills_an_msys_launched_grandchild_windows() {
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".to_string());
    let bash = std::path::Path::new(&pf).join(r"Git\usr\bin\bash.exe");
    assert!(bash.exists(), "Git for Windows bash not found");
    let mut cmd = tokio::process::Command::new(bash);
    cmd.args([
        "-c",
        "ping -n 600 127.0.0.1 >/dev/null & cat /proc/$!/winpid; exec sleep 600",
    ]);
    fire_takes_the_tree(cmd).expect("MSYS fire fixture");
}

/// The daemon itself inside a job: a nested job must still contain.
#[tokio::test]
#[ignore = "puts the test process itself in a job"]
async fn contain_from_a_jobbed_daemon_windows() {
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let outer = sot_log::capsule::producer::conpty::AnonymousJob::create().unwrap();
    // SAFETY: assigns this process to a job we own; the job is leaked so it never ends the test run.
    let ok = unsafe { AssignProcessToJobObject(outer.raw(), GetCurrentProcess()) };
    assert!(
        ok != 0,
        "AssignProcessToJobObject: {}",
        std::io::Error::last_os_error()
    );
    std::mem::forget(outer);
    fire_takes_the_tree(ping_tree().into()).expect("fire fixture");
}
