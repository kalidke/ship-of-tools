//! Absolute command fixtures for the bounded, recoverable trash path.
use super::*;
#[cfg(unix)]
use crate::lifecycle::start_tests::unix::Watched;
#[cfg(windows)]
use crate::lifecycle::start_tests::windows::Watched;
use crate::lifecycle::{child_signal::Signal, contain};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Fixture {
    dir: tempfile::TempDir,
    program: std::path::PathBuf,
}

impl Fixture {
    fn new(body: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let program = dir.path().join("trash");
        #[cfg(windows)]
        let program = dir.path().join("trash.ps1");
        sot_log::test_exec::write_executable(&program, body);
        std::fs::write(dir.path().join("source.txt"), b"recoverable bytes").unwrap();
        Self { dir, program }
    }

    fn live() -> Self {
        #[cfg(unix)]
        let body = "#!/bin/sh\n/bin/sh -c 'echo $$ > \"$1\"; while [ ! -e \"$2\" ]; do sleep 0.02; done' sh \"$1\" \"$2\" &\nwait\n";
        #[cfg(windows)]
        let body = r#"param($ready, $cleanup)
$info = New-Object System.Diagnostics.ProcessStartInfo
$info.FileName = 'powershell'
$info.Arguments = '-NoProfile -File "' + (Join-Path $PSScriptRoot 'descendant.ps1') + '" "' + $ready + '" "' + $cleanup + '"'
$info.UseShellExecute = $false
$info.CreateNoWindow = $true
$p = [System.Diagnostics.Process]::Start($info)
$p.WaitForExit()
"#;
        let fixture = Self::new(body);
        #[cfg(windows)]
        sot_log::test_exec::write_executable(&fixture.dir.path().join("descendant.ps1"),
            "param($ready, $cleanup)\n[IO.File]::WriteAllText($ready, [string]$PID); while (!(Test-Path -LiteralPath $cleanup)) { Start-Sleep -Milliseconds 20 }\n");
        fixture
    }

    fn exit(code: u8) -> Self {
        #[cfg(unix)]
        let body = format!("#!/bin/sh\nexit {code}\n");
        #[cfg(windows)]
        let body = format!("exit {code}\n");
        Self::new(&body)
    }

    fn source(&self) -> std::path::PathBuf {
        self.dir.path().join("source.txt")
    }

    fn command(&self) -> std::process::Command {
        #[cfg(unix)]
        let mut cmd = std::process::Command::new(&self.program);
        #[cfg(windows)]
        let mut cmd = {
            let mut cmd = std::process::Command::new("powershell");
            cmd.args(["-NoProfile", "-File"]).arg(&self.program);
            cmd
        };
        cmd.arg(self.dir.path().join("ready"))
            .arg(self.dir.path().join("cleanup"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        cmd
    }

    fn ready_hook(&self, signal: &Signal) -> mpsc::Receiver<(Watched, Watched)> {
        let ready = self.dir.path().join("ready");
        let (send, recv) = mpsc::channel();
        *signal.after_adopt.lock().unwrap() = Some(Box::new(move |leader| {
            let deadline = Instant::now() + Duration::from_secs(15);
            while std::fs::read_to_string(&ready).map_or(true, |s| s.trim().is_empty()) {
                assert!(
                    Instant::now() < deadline,
                    "trash fixture descendant never ready"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let descendant = std::fs::read_to_string(&ready)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            send.send((Watched::open(leader), Watched::open(descendant)))
                .unwrap();
        }));
        recv
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // A fixture-owned readiness protocol, never signaling a reusable PID.
        std::fs::write(self.dir.path().join("cleanup"), b"done").unwrap();
    }
}

fn dead(watched: &Watched) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if watched.dead() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
    }
}

fn signal() -> &'static Signal {
    Box::leak(Box::new(Signal::new()))
}

fn assert_fallback(result: Result<Option<std::path::PathBuf>>, source: &Path, root: &Path) {
    let destination = result.unwrap().expect("fallback not reported");
    assert!(!source.exists());
    assert!(destination.starts_with(root.join(".sot-trash")));
    assert_eq!(std::fs::read(destination).unwrap(), b"recoverable bytes");
}

#[test]
fn trash_timeout_reports_cleanup_before_fallback() {
    let fixture = Fixture::live();
    let signal = signal();
    let ready = fixture.ready_hook(signal);
    let source = fixture.source();
    let root = fixture.dir.path().to_path_buf();
    let mut command = fixture.command();
    let (send, recv) = mpsc::channel();
    let cleanup = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_cleanup = cleanup.clone();
    let (worker_source, worker_root) = (source.clone(), root.clone());
    let worker = std::thread::spawn(move || {
        let log = sot_log::test_log::capture();
        contain::REQUEST_EVENTS.with(|events| events.borrow_mut().clear());
        let result = trash_with_command(
            signal,
            &mut command,
            &worker_source,
            &worker_root,
            Duration::from_millis(100),
            |child| {
                if worker_cleanup.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                assert!(
                    child.confirmed_reaped(),
                    "fallback began before direct-child reap"
                );
                assert!(
                    worker_source.exists(),
                    "fallback ran before cleanup observation"
                );
                #[cfg(unix)]
                let expected = vec!["group", "leader"];
                #[cfg(windows)]
                let expected = vec!["job"];
                assert_eq!(
                    contain::REQUEST_EVENTS.with(|events| events.borrow().clone()),
                    expected
                );
            },
        );
        send.send((result, log.text())).unwrap();
    });
    let (leader, descendant) = ready.recv_timeout(Duration::from_secs(20)).unwrap();
    let observed = recv.recv_timeout(Duration::from_secs(1));
    let timely = observed.is_ok();
    let (result, log) = match observed {
        Ok(result) => result,
        Err(_) => {
            cleanup.store(true, std::sync::atomic::Ordering::Release);
            signal.fire().expect("private fixture cleanup fire");
            recv.recv_timeout(Duration::from_secs(10)).unwrap()
        }
    };
    worker.join().unwrap();
    assert!(dead(&leader), "trash direct child survived cleanup");
    // This is a separate eventual-death observation, after fallback completed.
    assert!(dead(&descendant), "trash descendant survived cleanup");
    assert!(timely, "trash wait missed fallback observation deadline");
    assert_fallback(result, &source, &root);
    assert!(log.contains("system trash timed out"), "{log}");
    assert!(
        log.contains("termination requests succeeded and direct child reaped"),
        "{log}"
    );
    assert!(
        log.contains("recoverable trash fallback succeeded"),
        "{log}"
    );
}

#[test]
fn confirmed_zero_is_system_trash() {
    let fixture = Fixture::exit(0);
    let result = trash_with_command(
        signal(),
        &mut fixture.command(),
        &fixture.source(),
        fixture.dir.path(),
        Duration::from_secs(5),
        |child| assert!(child.confirmed_reaped()),
    );
    assert!(result.unwrap().is_none());
    assert!(
        fixture.source().exists(),
        "synthetic zero fixture must not move bytes"
    );
}

#[test]
fn nonzero_and_missing_commands_take_recoverable_fallback() {
    for missing in [false, true] {
        let fixture = Fixture::exit(7);
        let log = sot_log::test_log::capture();
        let mut command = if missing {
            std::process::Command::new(fixture.dir.path().join("absent"))
        } else {
            fixture.command()
        };
        let result = trash_with_command(
            signal(),
            &mut command,
            &fixture.source(),
            fixture.dir.path(),
            Duration::from_secs(5),
            |_| {},
        );
        assert_fallback(result, &fixture.source(), fixture.dir.path());
        assert!(log.text().contains(if missing {
            "system trash spawn failed"
        } else {
            "system trash exited unsuccessfully"
        }));
    }
}

#[test]
fn failed_fallback_remains_an_error() {
    let fixture = Fixture::exit(7);
    std::fs::write(
        fixture.dir.path().join(".sot-trash"),
        b"blocks the destination directory",
    )
    .unwrap();
    let log = sot_log::test_log::capture();
    let error = trash_with_command(
        signal(),
        &mut fixture.command(),
        &fixture.source(),
        fixture.dir.path(),
        Duration::from_secs(5),
        |_| {},
    )
    .unwrap_err();
    assert!(error.to_string().contains("create_dir_all"));
    assert_eq!(
        std::fs::read(fixture.source()).unwrap(),
        b"recoverable bytes"
    );
    assert!(log.text().contains("recoverable trash fallback failed"));
}

fn cleanup_failure(kind: &str, reason: &str) {
    let fixture = Fixture::live();
    let signal = signal();
    let ready = fixture.ready_hook(signal);
    let log = sot_log::test_log::capture();
    #[cfg(unix)]
    let request = 3;
    #[cfg(windows)]
    let request = 4;
    contain::PROBE_FAILURE.with(|flag| flag.set(kind == "probe"));
    contain::REQUEST_FAILURE.with(|flag| flag.set(if kind == "request" { request } else { 0 }));
    contain::REAP_FAILURE.with(|flag| flag.set(kind == "reap"));
    let result = trash_with_command(
        signal,
        &mut fixture.command(),
        &fixture.source(),
        fixture.dir.path(),
        Duration::from_millis(100),
        |child| {
            assert_eq!(child.confirmed_reaped(), kind == "probe");
            // Let Drop perform its ordinary checked cleanup after the injected explicit error. No leaked fixture.
            contain::PROBE_FAILURE.with(|flag| flag.set(false));
            contain::REQUEST_FAILURE.with(|flag| flag.set(0));
            contain::REAP_FAILURE.with(|flag| flag.set(false));
        },
    );
    let (leader, descendant) = ready.recv_timeout(Duration::from_secs(20)).unwrap();
    assert!(dead(&leader));
    assert!(dead(&descendant));
    assert_fallback(result, &fixture.source(), fixture.dir.path());
    assert!(log.text().contains(reason), "{}", log.text());
    assert!(
        log.text().contains("cleanup success unconfirmed"),
        "{}",
        log.text()
    );
    assert!(!log
        .text()
        .contains("termination requests succeeded and direct child reaped"));
}

#[test]
fn probe_error_diagnoses_cleanup_and_falls_back() {
    cleanup_failure("probe", "injected exit probe failure");
}

#[test]
fn request_error_diagnoses_cleanup_and_falls_back() {
    #[cfg(unix)]
    let reason = "injected group request failure";
    #[cfg(windows)]
    let reason = "injected job request failure";
    cleanup_failure("request", reason);
}

#[test]
fn reap_error_diagnoses_cleanup_and_falls_back() {
    cleanup_failure("reap", "injected direct-child reap failure");
}
