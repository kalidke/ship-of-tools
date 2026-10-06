//! The session's environment: a session the daemon spawns carries the daemon's own binary in `SOTD_BIN`.

use super::*;
use sot_log::attach_client::supervisor_client::{end_run, query_status, stop, EndRunOutcome};

/// What a test has made that it must end: nothing yet, a row `workspace.create` made whose state folder the daemon has
/// not yet reported, or that row's state folder as the daemon reports it.
enum Made {
    Nothing,
    Created,
    Row(PathBuf),
}

/// A test's capsule row and the folders it runs from: the test's `Env` and, in T4, the stage holding the daemon's
/// binaries. On every exit path the row is ended through its own owners, and the folders go only once its run and its
/// supervisor are confirmed ended. Otherwise they are kept, and stderr says why, with the row's recorded supervisor and
/// leg. No process is found by name, path or pattern.
struct OwnedRow {
    env: Option<Env>,
    stage: Option<tempfile::TempDir>,
    made: Made,
    /// The supervisor (pid and creation time) and leg number the row's supervisor last reported, for the report of a
    /// row not confirmed ended.
    seen: Option<String>,
    /// `finish`'s outcome, so that `Drop` ends the row only when the test body did not.
    outcome: Option<Result<(), String>>,
}

impl OwnedRow {
    fn new(env: Env, stage: Option<tempfile::TempDir>) -> Self {
        OwnedRow {
            env: Some(env),
            stage,
            made: Made::Nothing,
            seen: None,
            outcome: None,
        }
    }

    fn env(&self) -> &Env {
        self.env
            .as_ref()
            .expect("the row's Env, held until the row is ended")
    }

    /// `workspace.create` for a capsule row, owned from the moment it succeeds. Then the row's state folder as the
    /// daemon itself resolves and reports it in `workspace.list` (`<state root>/sot/workspaces/<id>`), so the test
    /// derives no path of its own.
    async fn create(&mut self, conn: &mut Conn, next_id: &mut u64, label: &str) -> String {
        let req = serde_json::json!({
            "label": label,
            "project_root": self.env().workspace_project_root.to_string_lossy(),
            "runtime": "capsule",
        });
        let res = call(conn, *next_id, op::WORKSPACE_CREATE, req).await;
        *next_id += 1;
        assert!(
            res.payload.get("error").is_none(),
            "workspace.create failed: {:?}",
            res.payload
        );
        self.made = Made::Created;
        let workspace_id = res.payload["workspace_id"]
            .as_str()
            .expect("workspace_id")
            .to_string();
        self.made = Made::Row(state_dir_from_list(conn, next_id, &workspace_id).await);
        workspace_id
    }

    /// Ends the row, and is `Ok` only when it is confirmed ended. Each step runs whatever an earlier one returned:
    /// 1. the daemon, by `Env`'s tracked child, so nothing restarts the row: killed, then polled for its exit within
    ///    `BOUND` (not `wait_within`, which panics, since this also runs while a test unwinds);
    /// 2. the run, through the row's supervisor: `end_run`, which ends the leg on the identity it challenged and checks
    ///    the leg's marker;
    /// 3. the supervisor: `stop`, which waits for its exit on the identity its lane's challenge retained.
    /// The row is confirmed ended when `end_run` reports its record closed or verified and `stop` returns: the pair the
    /// daemon itself ends a row with (rows/run/end_run.rs). A supervisor that does not answer leaves both unconfirmed.
    /// `Err` lists every step that failed.
    fn end(&mut self) -> Result<(), String> {
        let mut failed = Vec::new();
        if let Some(mut daemon) = self
            .env
            .as_ref()
            .and_then(|env| env.daemon.borrow_mut().take())
        {
            let _ = daemon.kill();
            let deadline = Instant::now() + BOUND;
            while !matches!(daemon.try_wait(), Ok(Some(_))) {
                if Instant::now() >= deadline {
                    failed.push(format!(
                        "the daemon (pid {}) did not exit within {BOUND:?} of its kill",
                        daemon.id()
                    ));
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        match &self.made {
            Made::Nothing => {}
            Made::Created => failed
                .push("a row was created whose state folder the daemon never reported".to_string()),
            Made::Row(dir) => {
                match query_status(dir) {
                    Ok((report, _)) => {
                        self.seen = Some(format!(
                            "its supervisor was pid {} (created {}), at leg {:?}",
                            report.pid, report.created, report.leg
                        ));
                        match report
                            .voyage
                            .as_deref()
                            .map(|voyage| end_run(dir, voyage, "sotd-bin test teardown"))
                        {
                            Some(Ok(
                                EndRunOutcome::RecordVerified | EndRunOutcome::RecordClosed,
                            )) => {}
                            other => failed.push(format!("the row's run did not end: {other:?}")),
                        }
                    }
                    Err(e) => failed.push(format!(
                        "the row's supervisor did not answer, so its run's end is unconfirmed: {e}"
                    )),
                }
                if let Err(e) = stop(dir) {
                    failed.push(format!("the row's supervisor did not stop: {e}"));
                }
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }

    /// The test body's end: ends the row once and returns the outcome, which the test asserts.
    fn finish(&mut self) -> Result<(), String> {
        let outcome = self.end();
        self.outcome = Some(outcome.clone());
        outcome
    }
}

impl Drop for OwnedRow {
    /// On every exit path, a panic included: the row is ended if the test body did not end it. The folders go only
    /// when the row is confirmed ended (`Env` first, whose own teardown then runs, then the stage). Otherwise they are
    /// kept, since a process of the row may still run from them, and stderr says which and why.
    fn drop(&mut self) {
        let outcome = match self.outcome.take() {
            Some(outcome) => outcome,
            None => self.end(),
        };
        match outcome {
            Ok(()) => {
                drop(self.env.take());
                drop(self.stage.take());
            }
            Err(why) => {
                let env = self.env.take();
                let stage = self.stage.take();
                // The row's state folder and the Env's runtime folder are what ending it later through its own
                // supervisor needs (`SOT_RUNTIME_DIR` names where its lane listens).
                let folders: Vec<String> = [
                    match &self.made {
                        Made::Row(dir) => Some(dir.display().to_string()),
                        _ => None,
                    },
                    env.as_ref()
                        .map(|env| env._tmp.path().display().to_string()),
                    env.as_ref()
                        .map(|env| env._runtime_tmp.path().display().to_string()),
                    stage
                        .as_ref()
                        .map(|stage| stage.path().display().to_string()),
                ]
                .into_iter()
                .flatten()
                .collect();
                eprintln!(
                    "sotd-bin test: the row is not confirmed ended ({why}); {}; keeping its folders: {}",
                    self.seen.as_deref().unwrap_or("its supervisor never answered"),
                    folders.join(" ")
                );
                std::mem::forget(env);
                std::mem::forget(stage);
            }
        }
    }
}

/// `capsule_supervisor_env`: a session's `SOTD_BIN` is the path the daemon was started by (forward slashes on
/// Windows), over the value the daemon's own environment carries, so the comm shell in the session bridges with the
/// daemon's own binary. The session's platform shell (`$SHELL` here, `cmd.exe` on Windows) writes the value to a file.
#[tokio::test]
async fn a_session_carries_the_daemons_own_binary_over_an_inherited_sotd_bin() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] — build it first"
    );

    let mut row = OwnedRow::new(Env::new("sbin"), None);
    row.env()
        .spawn_sotd_with_env(&[("SOTD_BIN", "/sentinel/sotd")]);
    let (mut conn, mut next_id) = connect_and_hello(&row.env().socket_path).await;
    let workspace_id = row.create(&mut conn, &mut next_id, "sbin-workspace").await;
    poll_for_phase(
        &mut conn,
        &mut next_id,
        &workspace_id,
        "ready",
        BOUND.max(Duration::from_secs(90)),
    )
    .await;

    let out = row.env().workspace_project_root.join("sotd-bin.txt");
    let line = if cfg!(windows) {
        format!("echo %SOTD_BIN%>\"{}\"", out.display())
    } else {
        format!("printf '%s\\n' \"$SOTD_BIN\" > '{}'", out.display())
    };
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let input_req = serde_json::json!({
        "workspace_id": workspace_id,
        "data_b64": STANDARD.encode(line.as_bytes()),
        "enter": true,
        "origin": "sotd-bin-test",
    });
    let input_res = call(&mut conn, next_id, op::PTY_INPUT, input_req).await;
    assert!(
        input_res.payload.get("error").is_none(),
        "pty.input failed: {:?}",
        input_res.payload
    );

    let deadline = Instant::now() + BOUND;
    let got = loop {
        if let Ok(text) = std::fs::read_to_string(&out) {
            if text.ends_with('\n') {
                break text.trim().to_string();
            }
        }
        assert!(
            Instant::now() < deadline,
            "the session never wrote {}",
            out.display()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let started = sotd_program().to_string_lossy().into_owned();
    let want = if cfg!(windows) {
        started.replace('\\', "/")
    } else {
        started
    };
    assert_eq!(
        got, want,
        "a session's SOTD_BIN is the daemon's own binary, not the value the daemon inherited"
    );

    if let Err(e) = row.finish() {
        panic!("the row did not end through its owners: {e}");
    }
}

/// The stable start path, never the resolved one: a daemon started through a symlink, whose target an update then
/// replaces in place (a new file renamed over it), gives a session spawned afterwards the symlink's path. Not the
/// target, and never `<target> (deleted)`, which is what `/proc/<pid>/exe` reads by then. Linux only: Windows cannot
/// rename over a running image, and `/proc` is Linux's.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_session_after_an_in_place_update_carries_the_stable_start_path() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd — build it first"
    );

    // The daemon's binary and its capsule sibling in a folder of this test's own, and a link to the binary.
    let stage = tempfile::tempdir().expect("a stage folder");
    let bin = stage.path().join("bin");
    let link = stage.path().join("link");
    std::fs::create_dir_all(&bin).expect("mkdir bin");
    std::fs::create_dir_all(&link).expect("mkdir link");
    for (from, to) in [
        (sotd_program(), bin.join("sotd")),
        (sot_capsule_exe(), bin.join(CAPSULE_EXE_NAME)),
    ] {
        if std::fs::hard_link(&from, &to).is_err() {
            sot_log::test_exec::write_executable(
                &to,
                std::fs::read(&from).expect("read the binary to stage"),
            );
        }
    }
    std::os::unix::fs::symlink(bin.join("sotd"), link.join("sotd")).expect("link the binary");

    let mut row = OwnedRow::new(Env::new("sbup"), Some(stage));
    row.env()
        .spawn_sotd_at(&link.join("sotd"), &[("SOTD_BIN", "/sentinel/sotd")]);
    let (mut conn, mut next_id) = connect_and_hello(&row.env().socket_path).await;

    // An update replaces the file in place: a new file written beside it and renamed over it.
    let fresh = bin.join("sotd.new");
    sot_log::test_exec::write_executable(&fresh, "#!/bin/sh\nexit 0\n");
    std::fs::rename(&fresh, bin.join("sotd")).expect("rename the replacement over the binary");
    let pid = row
        .env()
        .daemon
        .borrow()
        .as_ref()
        .expect("the daemon is tracked")
        .id();
    let exe =
        std::fs::read_link(format!("/proc/{pid}/exe")).expect("read the daemon's /proc exe link");
    assert!(
        exe.to_string_lossy().ends_with(" (deleted)"),
        "the in-place replacement did not take: /proc/{pid}/exe reads {}",
        exe.display()
    );

    let workspace_id = row.create(&mut conn, &mut next_id, "sbup-workspace").await;
    poll_for_phase(
        &mut conn,
        &mut next_id,
        &workspace_id,
        "ready",
        BOUND.max(Duration::from_secs(90)),
    )
    .await;

    let out = row.env().workspace_project_root.join("sotd-bin.txt");
    let line = format!("printf '%s\\n' \"$SOTD_BIN\" > '{}'", out.display());
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let input_req = serde_json::json!({
        "workspace_id": workspace_id,
        "data_b64": STANDARD.encode(line.as_bytes()),
        "enter": true,
        "origin": "sotd-bin-update-test",
    });
    let input_res = call(&mut conn, next_id, op::PTY_INPUT, input_req).await;
    assert!(
        input_res.payload.get("error").is_none(),
        "pty.input failed: {:?}",
        input_res.payload
    );
    let deadline = Instant::now() + BOUND;
    let got = loop {
        if let Ok(text) = std::fs::read_to_string(&out) {
            if text.ends_with('\n') {
                break text.trim().to_string();
            }
        }
        assert!(
            Instant::now() < deadline,
            "the session never wrote {}",
            out.display()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        got,
        link.join("sotd").to_string_lossy(),
        "a session spawned after an in-place update carries the daemon's stable start path"
    );

    if let Err(e) = row.finish() {
        panic!("the row did not end through its owners: {e}");
    }
}
