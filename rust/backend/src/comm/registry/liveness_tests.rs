use super::*;

#[test]
fn held_is_the_handles_of_starting_and_ready_rows() {
    let rows: [(Phase, fn() -> String); 5] = [
        (Phase::Ready, || "a".to_string()),
        (Phase::Starting, || "b".to_string()),
        (Phase::Stopped, || "c".to_string()),
        (Phase::Terminal, || "d".to_string()),
        (Phase::Ready, String::new),
    ];
    assert_eq!(held(rows), ["a".to_string(), "b".to_string()].into_iter().collect());
}

#[test]
fn unended_is_the_handles_of_every_row_not_latched_ended() {
    let rows: [(Phase, fn() -> String); 9] = [
        (Phase::Stopped, || "stopped".to_string()),
        (Phase::Starting, || "starting".to_string()),
        (Phase::Ready, || "ready".to_string()),
        (Phase::Ending, || "ending".to_string()),
        (Phase::Unreachable, || "unreachable".to_string()),
        (Phase::Foreign, || "foreign".to_string()),
        (Phase::Terminal, || "terminal".to_string()),
        (Phase::EndedNoRespawn, || "ended".to_string()),
        (Phase::Stopped, String::new),
    ];
    let want = ["stopped", "starting", "ready", "ending", "unreachable", "foreign"].map(String::from);
    assert_eq!(unended(rows), want.into_iter().collect());
}

#[test]
fn held_resolves_a_handle_only_for_a_running_row() {
    let rows: [(Phase, Box<dyn FnOnce() -> String>); 2] =
        [(Phase::Ready, Box::new(|| "a".to_string())), (Phase::Stopped, Box::new(|| panic!("resolved a stopped row")))];
    assert_eq!(held(rows), ["a".to_string()].into_iter().collect());
}

#[test]
fn a_pass_is_due_for_a_changed_set_or_a_minute() {
    let a: BTreeSet<String> = ["a".to_string()].into_iter().collect();
    let b: BTreeSet<String> = ["b".to_string()].into_iter().collect();
    assert!(due(&a, &b, Duration::from_secs(1)));
    assert!(due(&a, &a, STAMP_EVERY));
    assert!(!due(&a, &a, STAMP_EVERY - Duration::from_secs(1)));
}

// ADR 0049: a running row whose self-file names a handle another row
// declares does not hold it; with no declaring row the fallback binds.
#[test]
fn held_handles_skips_a_handle_another_row_declares() {
    use crate::rows::workspace::{Observation, SupervisorIdentity};
    // Restores both vars when dropped, so a failed assertion leaks nothing.
    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        saved: [(&'static str, Option<std::ffi::OsString>); 2],
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
    let _guard = EnvGuard {
        saved: [("SOT_COMM_HOME", std::env::var_os("SOT_COMM_HOME")), ("SOT_SELF_HOST", std::env::var_os("SOT_SELF_HOST"))],
        _serial: crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner()),
    };
    let dir = std::env::temp_dir().join(format!(
        "sot-filer-running-row-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(dir.join("self")).unwrap();
    std::env::set_var("SOT_COMM_HOME", &dir);
    std::env::set_var("SOT_SELF_HOST", "host-4");

    let row = |label: &str, declared: &str, ready: bool| {
        let mut w = Workspace::from_label(
            label,
            std::env::temp_dir(),
            false,
            "none".into(),
            String::new(),
            String::new(),
        );
        w.runtime = "capsule".to_string();
        w.agent_handle = std::sync::Mutex::new(declared.to_string());
        if ready {
            assert!(w.apply_phase_observation(Observation::Phase {
                phase: Phase::Ready,
                supervisor: SupervisorIdentity { pid: 1, created: 1 },
                voyage: Some(uuid::Uuid::from_u128(1)),
            }));
        }
        Arc::new(w)
    };
    let older = row("m5-older", "", true);
    std::fs::write(
        dir.join("self").join(format!("host-4__{}.txt", older.workspace_id)),
        "m5-shared\nrepo=m5\nroot=/p/x\n",
    )
    .unwrap();
    let newer = row("m5-newer", "m5-shared", false);
    let both = [older.clone(), newer];
    assert!(!held_handles(&both).contains("m5-shared"));
    assert!(held_handles(&[older.clone()]).contains("m5-shared"), "no declaring row: the fallback still binds");
    let newer_ready = row("m5-newer-ready", "m5-shared", true);
    assert!(held_handles(&[older, newer_ready]).contains("m5-shared"));

    let _ = std::fs::remove_dir_all(&dir);
}
