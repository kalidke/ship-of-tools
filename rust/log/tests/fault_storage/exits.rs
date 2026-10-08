//! A leg's exit code: 71 means one thing, a run that failed with storage exhaustion.
use super::*;
use std::process::{Command, Stdio};
use std::time::Instant;

/// A private runtime folder for the capsule's socket, outside any bounded volume.
#[cfg(unix)]
fn runtime_dir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::Builder::new()
        .prefix("sot-l3")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// A producer that exits with `code` the moment it starts.
fn exit_with(code: i32) -> Vec<String> {
    #[cfg(unix)]
    let argv = ["/bin/sh", "-c", &format!("exit {code}")].map(String::from);
    #[cfg(windows)]
    let argv = ["cmd.exe", "/d", "/c", &format!("exit {code}")].map(String::from);
    argv.to_vec()
}

/// Runs `sot-capsule run` on `voyage_root` to its end and returns its exit code.
fn run_leg(voyage_root: &Path, voyage_id: &str, producer: &[String]) -> i32 {
    #[cfg(unix)]
    let runtime = runtime_dir();
    let mut command = Command::new(env!("CARGO_BIN_EXE_sot-capsule"));
    command
        .arg("run")
        .arg(voyage_root)
        .arg(voyage_id)
        .args(["--assume-no-rollback-target", "--"])
        .args(producer)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    command.env("SOT_RUNTIME_DIR", runtime.path());
    let mut child = command.spawn().expect("start sot-capsule run");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code().expect("the leg ended by an exit code");
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the leg did not end within 120 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_producer_exit_71_leaves_its_leg_exit_1() {
    let dir = tempfile::tempdir().unwrap();
    let voyage = uuid::Uuid::now_v7().to_string();
    let code = run_leg(&dir.path().join("voyage"), &voyage, &exit_with(71));
    assert_eq!(
        code, 1,
        "a producer's own 71 must not become the storage code"
    );
    println!("L3 exits producer-71 leg-exit={code}");
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "needs the bounded ext4 volume of rust.yml's L3 step"
)]
fn a_full_volume_ends_the_leg_with_71() {
    volume::on_volume(|root| {
        let voyage = uuid::Uuid::now_v7().to_string();
        let voyage_root = root.join("voyage");
        let healthy = run_leg(&voyage_root, &voyage, &exit_with(0));
        assert_eq!(
            healthy, 0,
            "a leg on a healthy volume ends with its producer's code"
        );
        let filled = volume::fill(root);
        let code = run_leg(&voyage_root, &voyage, &exit_with(0));
        volume::free_and_sync(root);
        assert_eq!(code, 71, "a leg whose volume is full exits 71");
        println!("L3 exits full-volume fill-code={filled} leg-exit={code}");
    });
}

/// A leg whose diagnostics cannot be written (its stderr is a full volume,
/// here `/dev/full`) still ends with its producer's exit code: a diagnostic is
/// one write that never panics. The diagnostic is the leg's refusal of a
/// connection past `NON_WATCHER_CAP`, provoked by five plain connections to
/// its voyage socket that never attach.
#[cfg(target_os = "linux")]
#[test]
fn a_leg_whose_stderr_is_full_keeps_its_exit_code() {
    use std::os::unix::net::UnixStream;
    let dir = tempfile::tempdir().unwrap();
    let voyage = uuid::Uuid::now_v7().to_string();
    let runtime = runtime_dir();
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sot-capsule"))
        .arg("run")
        .arg(dir.path().join("voyage"))
        .arg(&voyage)
        .args([
            "--assume-no-rollback-target",
            "--",
            "/bin/sh",
            "-c",
            "sleep 3; exit 7",
        ])
        .env("SOT_RUNTIME_DIR", runtime.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(full))
        .spawn()
        .expect("start sot-capsule run");

    // The first connection waits for the socket to exist; four more fill the
    // pre-admission cap and the fifth is refused.
    let socket = runtime.path().join(format!("voyage-{voyage}.sock"));
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut held = Vec::new();
    while held.is_empty() {
        match UnixStream::connect(&socket) {
            Ok(stream) => held.push(stream),
            Err(_) => {
                assert!(
                    Instant::now() < deadline,
                    "the leg never bound its voyage socket"
                );
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "the leg ended before binding"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    for _ in 0..4 {
        held.push(UnixStream::connect(&socket).expect("a plain connection"));
    }

    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the leg did not end within 60 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(held);
    assert_eq!(
        status.code(),
        Some(7),
        "a diagnostic that cannot be written must not change the exit code"
    );
    println!("L3 exits stderr-full leg-exit=7");
}
