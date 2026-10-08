//! Supervisors on a full volume: real `sot-capsule supervise` processes hold their rows and resume them.
use super::*;
use sot_log::host::state_dir::state_dir_hash;
use sot_log::lane::wire::{SupervisorPhase, SupervisorReply, SupervisorRequest};
use sot_log::supervisor::{connect_and_challenge_for_test, request_for_test, voyage_root_path};
use std::process::{Command, Stdio};
use std::time::Instant;

#[path = "../support/capsule_guard.rs"]
mod capsule_guard;
use capsule_guard::CapsuleGuard;

/// The agent: a producer that writes a short line every 200 ms, forever.
fn agent() -> Vec<String> {
    #[cfg(target_os = "linux")]
    let helper = env!("CARGO_BIN_EXE_sot-pty-helper");
    #[cfg(windows)]
    let helper = env!("CARGO_BIN_EXE_sot-conpty-helper");
    [helper, "--script", "1", "--drip"]
        .map(String::from)
        .to_vec()
}

/// Points the supervisors' sockets at a private folder outside the volume
/// (Unix); Windows pipes have no such folder.
struct RuntimeDir {
    #[cfg(target_os = "linux")]
    _dir: tempfile::TempDir,
}

fn runtime_dir() -> RuntimeDir {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::Builder::new()
            .prefix("sot-l3s")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::env::set_var("SOT_RUNTIME_DIR", dir.path());
        RuntimeDir { _dir: dir }
    }
    #[cfg(windows)]
    RuntimeDir {}
}

/// One row: a supervisor process on a state dir of the volume.
struct Row {
    guard: Option<CapsuleGuard>,
    state_dir: PathBuf,
    voyage: Option<String>,
}

impl Row {
    fn start(state_dir: PathBuf) -> Row {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sot-capsule"));
        command
            .arg("supervise")
            .arg(&state_dir)
            .args(["--start", "--assume-no-rollback-target", "--"])
            .args(agent())
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let child = command.spawn().expect("start sot-capsule supervise");
        Row {
            guard: Some(CapsuleGuard::new(child, &state_dir)),
            state_dir,
            voyage: None,
        }
    }

    fn lane_name(&self) -> String {
        state_dir_hash(&self.state_dir)
    }

    /// The supervisor's answer to `status`: its pid, voyage and phase; `None`
    /// when it did not answer. A new connection each time: the lane closes
    /// an idle one after 5 s.
    fn status(&self) -> Option<(u32, Option<String>, SupervisorPhase)> {
        let (conn, _) = connect_and_challenge_for_test(&self.lane_name()).ok()?;
        match request_for_test(
            &conn,
            &SupervisorRequest::Status,
            Instant::now() + Duration::from_secs(5),
        ) {
            Ok(SupervisorReply::StatusOk {
                pid, voyage, phase, ..
            }) => Some((pid, voyage, phase)),
            _ => None,
        }
    }

    /// As [`Self::status`], asked up to three times.
    fn answer(&self) -> Option<(u32, Option<String>, SupervisorPhase)> {
        for _ in 0..3 {
            if let Some(answer) = self.status() {
                return Some(answer);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        None
    }

    /// Waits until the row is `Ready`, on a voyage: returns its pid and voyage.
    fn ready(&mut self, within: Duration, what: &str) -> (u32, String) {
        let deadline = Instant::now() + within;
        loop {
            if let Some((pid, Some(voyage), SupervisorPhase::Ready)) = self.status() {
                self.voyage = Some(voyage.clone());
                return (pid, voyage);
            }
            assert!(
                Instant::now() < deadline,
                "{what}: not Ready within {within:?}"
            );
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Bytes under the voyage's `seg/` folder: the voyage grows while its leg writes.
    fn voyage_bytes(&self, voyage: &str) -> u64 {
        let seg = voyage_root_path(&self.state_dir, voyage).join("seg");
        std::fs::read_dir(seg)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok()?.metadata().ok())
                    .map(|m| m.len())
                    .sum()
            })
            .unwrap_or(0)
    }
}

impl Drop for Row {
    /// Ends the supervisor and its leg, then waits for the voyage's writer
    /// lock to be free: nothing of the row may hold the volume when the
    /// fixture detaches it.
    fn drop(&mut self) {
        self.guard.take();
        let Some(voyage) = self.voyage.take() else {
            return;
        };
        let lock = voyage_root_path(&self.state_dir, &voyage).join("writer.lock");
        let deadline = Instant::now() + Duration::from_secs(60);
        while sot_log::lock_writer(&lock).is_err() {
            if Instant::now() > deadline {
                eprintln!("L3 HARNESS FAILURE: a row's leg still holds its voyage after 60 s");
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "needs the bounded ext4 volume of rust.yml's L3 step"
)]
fn three_rows_hold_and_resume_on_a_full_volume() {
    volume::on_volume(|root| {
        let _runtime = runtime_dir();
        let mut rows: Vec<Row> = (0..3)
            .map(|i| Row::start(root.join(format!("row-{i}"))))
            .collect();
        let before: Vec<(u32, String)> = rows
            .iter_mut()
            .map(|r| r.ready(Duration::from_secs(90), "start"))
            .collect();
        let grown: Vec<u64> = rows
            .iter()
            .zip(&before)
            .map(|(r, (_, v))| r.voyage_bytes(v))
            .collect();

        let filled = volume::fill(root);

        // Within 30 s each leg is gone: the row is no longer Ready.
        for row in &rows {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                let Some((_, _, phase)) = row.answer() else {
                    panic!("L3 a row's supervisor stopped answering when the volume filled");
                };
                if phase != SupervisorPhase::Ready {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "L3 a row's leg was still Ready 30 s after the volume filled"
                );
                std::thread::sleep(Duration::from_millis(250));
            }
        }

        // For 65 s each authority answers with the same pid and voyage and is
        // neither terminal nor ended: it holds, it does not give up.
        let until = Instant::now() + Duration::from_secs(65);
        while Instant::now() < until {
            for (row, (pid, voyage)) in rows.iter().zip(&before) {
                let (now_pid, now_voyage, phase) = row
                    .answer()
                    .expect("L3 a holding supervisor must keep answering status");
                assert_eq!(now_pid, *pid, "the supervisor must stay the same process");
                assert_eq!(
                    now_voyage.as_ref(),
                    Some(voyage),
                    "the row must stay on its voyage"
                );
                assert!(
                    !matches!(
                        phase,
                        SupervisorPhase::Terminal | SupervisorPhase::EndedNoRespawn
                    ),
                    "L3 a row left its voyage for {phase:?} while storage was full"
                );
            }
            std::thread::sleep(Duration::from_secs(1));
        }

        volume::free_and_sync(root);

        // Within 120 s each is Ready again on the same voyage, and it writes.
        for ((row, (pid, voyage)), bytes) in rows.iter_mut().zip(&before).zip(&grown) {
            let (now_pid, now_voyage) =
                row.ready(Duration::from_secs(120), "after storage cleared");
            assert_eq!(
                (&now_pid, &now_voyage),
                (pid, voyage),
                "the same supervisor on the same voyage resumed"
            );
            let deadline = Instant::now() + Duration::from_secs(30);
            while row.voyage_bytes(voyage) <= *bytes {
                assert!(
                    Instant::now() < deadline,
                    "L3 a resumed row's voyage did not grow"
                );
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        println!("L3 scenario three-rows fill-code={filled} held=ok resumed=ok");
    });
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "needs the bounded ext4 volume of rust.yml's L3 step"
)]
fn a_new_row_on_a_full_volume_waits_then_starts() {
    volume::on_volume(|root| {
        let _runtime = runtime_dir();
        let filled = volume::fill(root);

        let mut row = Row::start(root.join("row-new"));
        // Alive and not exited for 15 s: it waits, it does not end.
        let until = Instant::now() + Duration::from_secs(15);
        while Instant::now() < until {
            let child = row.guard.as_mut().unwrap().child_mut();
            assert!(
                child.try_wait().unwrap().is_none(),
                "L3 a new row's supervisor ended while the volume was full"
            );
            std::thread::sleep(Duration::from_millis(250));
        }

        volume::free_and_sync(root);
        row.ready(Duration::from_secs(120), "after storage cleared");
        println!("L3 scenario new-row fill-code={filled} waited=ok started=ok");
    });
}
