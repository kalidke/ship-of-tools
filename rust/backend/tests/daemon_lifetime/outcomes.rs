//! The serving daemon's outcomes on real processes: the main future's result becomes a status while the runtime still exists
//! (0, 1, 101), INT and TERM end a daemon whose runtime is stalled and whose inherited mask blocks them (by the signal itself,
//! after the fire) and a second TERM changes nothing, a service manager counts that end as a clean stop, a close that
//! outlasts its bound ends the daemon with 1 and takes its ephemeral trees with it, and an early command keeps its own
//! status. The status is read at the launched process: the guard exits as the daemon did. The capsule a case starts is
//! outside the daemon's lifetime and stays alive through every outcome.

use crate::ephemerals::start_spinning_with;
use crate::fixture_owner::Fixture;
use crate::guard::Run;
use crate::routes::{all_ended, ready_row, supervisor_in};
use crate::support::{connect_and_hello, Env};
use crate::SERIAL;
use std::os::unix::process::ExitStatusExt;
use std::time::Duration;

/// A guarded daemon with a ready capsule row whose supervisor the fixture watches.
struct Ready {
    run: Run,
    supervisor: usize,
}

async fn ready(tag: &str, fx: &mut Fixture, masked: bool, extra: &[(&str, &str)]) -> Ready {
    let env = Env::new(tag);
    let run = Run::boot(env, extra, false, masked).await;
    run.assert_guarded();
    let (mut conn, mut next_id) = connect_and_hello(&run.env.socket_path).await;
    let (_, state_dir) = ready_row(&run.env, &mut conn, &mut next_id, "outcome").await;
    drop(conn);
    let (pid, created) = supervisor_in(&run.env, &state_dir)
        .await
        .expect("the capsule's supervisor answers");
    let supervisor = fx
        .watch(pid, Some(created), "the capsule's supervisor")
        .expect("an identity for the reported supervisor");
    Ready { run, supervisor }
}

#[tokio::test]
async fn a_main_outcome_becomes_the_launched_status() {
    let _serial = SERIAL.lock().await;
    for (outcome, code, said) in [
        ("ok", 0, None),
        ("err", 1, Some("injected main error")),
        ("panic", 101, Some("injected main panic")),
    ] {
        let mut fx = Fixture::new(&format!("main_outcome::{outcome}"));
        let mut case = ready("mout", &mut fx, false, &[]).await;
        case.run.daemon_does(outcome);
        let status = case.run.status_within(Duration::from_secs(60)).await;
        fx.save("status", format!("{status:?}"));
        fx.save(
            "capsule_alive",
            !fx.identity(case.supervisor).exited(Duration::from_secs(1)),
        );
        let log = case.run.said();
        let cleanup = fx.cleanup();
        drop(case);
        assert!(cleanup.complete(), "{cleanup:?}");
        let status =
            status.unwrap_or_else(|| panic!("{outcome}: the launched process did not end:\n{log}"));
        assert_eq!(
            status.code(),
            Some(code),
            "{outcome}: {status:?} signal {:?}\n{log}",
            status.signal()
        );
        if let Some(said) = said {
            assert!(
                log.contains(said),
                "{outcome}: the log does not carry the reason:\n{log}"
            );
        }
        assert_eq!(
            fx.saved("capsule_alive"),
            Some("true"),
            "{outcome}: the daemon's end took the capsule with it"
        );
    }
}

#[tokio::test]
async fn int_and_term_end_a_stalled_daemon_whatever_mask_it_inherited() {
    let _serial = SERIAL.lock().await;
    for (signal, masked) in [
        (libc::SIGTERM, false),
        (libc::SIGINT, false),
        (libc::SIGTERM, true),
        (libc::SIGINT, true),
    ] {
        let mut fx = Fixture::new(&format!("stalled_signal::{signal}::{masked}"));
        let mut case = ready("msig", &mut fx, masked, &[]).await;
        case.run.daemon_does("stall");
        // The injected stall takes effect within a poll of the outcome file: it has taken effect once a new connection's
        // hello goes unanswered.
        let mut stalled = false;
        for _ in 0..100 {
            let answered = tokio::time::timeout(
                Duration::from_secs(1),
                connect_and_hello(&case.run.env.socket_path),
            )
            .await
            .is_ok();
            if !answered {
                stalled = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        fx.save("runtime_stalled", stalled);
        // SAFETY: signals to the guard this test spawned; a second TERM is the case where the unit's stop delivers it twice.
        unsafe {
            libc::kill(case.run.guard_pid(), signal);
            if signal == libc::SIGTERM {
                std::thread::sleep(Duration::from_millis(100));
                libc::kill(case.run.guard_pid(), signal);
            }
        }
        let status = case.run.status_within(Duration::from_secs(60)).await;
        fx.save("status", format!("{status:?}"));
        fx.save(
            "capsule_alive",
            !fx.identity(case.supervisor).exited(Duration::from_secs(1)),
        );
        let log = case.run.said();
        let cleanup = fx.cleanup();
        drop(case);
        assert!(cleanup.complete(), "{cleanup:?}");
        assert_eq!(
            fx.saved("runtime_stalled"),
            Some("true"),
            "signal {signal}: the runtime was not stalled when the signal arrived"
        );
        let status = status
            .unwrap_or_else(|| panic!("signal {signal}: the stalled daemon did not end:\n{log}"));
        assert_eq!(
            status.signal(),
            Some(signal),
            "signal {signal} masked {masked}: {status:?} code {:?}\n{log}",
            status.code()
        );
        assert_eq!(
            fx.saved("capsule_alive"),
            Some("true"),
            "signal {signal}: the daemon's end took the capsule with it"
        );
    }
}

/// `systemctl --user` for one of this case's own units; its status and its output.
fn systemctl(args: &[&str]) -> (bool, String) {
    let out = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .expect("run systemctl --user");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// Whether a daemon answers on `socket` within `bound`.
async fn answers_within(socket: &std::path::Path, bound: Duration) -> bool {
    let deadline = std::time::Instant::now() + bound;
    while std::time::Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Under a service manager INT and TERM are a clean stop, as for a daemon with no handler: `systemctl stop` leaves a unit
/// inactive, not failed, and a TERM to its main process (the guard) is not restarted under the deploy unit's
/// `Restart=on-failure`. The units are this case's own, named for it, and are stopped and reset by name.
#[tokio::test]
async fn a_service_manager_counts_int_and_term_as_a_clean_stop() {
    let _serial = SERIAL.lock().await;
    if let Err(e) = crate::support::user_manager_available_for_test() {
        assert_ne!(
            std::env::var("SOT_TEST_REQUIRE_USER_MANAGER").as_deref(),
            Ok("1"),
            "SOT_TEST_REQUIRE_USER_MANAGER=1 but no user manager answers: {e}"
        );
        eprintln!("SKIPPED: no user manager: {e}");
        return;
    }
    for how in ["stop", "term-to-main"] {
        let env = Env::new("msvc");
        let unit = format!("sot-l2-term-{how}-{}.service", std::process::id());
        let set = |k: &str, v: &std::ffi::OsStr| format!("--setenv={k}={}", v.to_string_lossy());
        let started = std::process::Command::new("systemd-run")
            .args(["--user", "--unit", &unit, "--service-type=exec"])
            .args(["--property=Restart=on-failure", "--property=RestartSec=1"])
            .arg(set("XDG_STATE_HOME", env.state_root.as_os_str()))
            .arg(set("XDG_CONFIG_HOME", env.config_root.as_os_str()))
            .arg(set(
                "SOT_SELF_HOST",
                std::ffi::OsStr::new(crate::support::TEST_STATE_HOST),
            ))
            .arg(set("SOT_RUNTIME_DIR", env._runtime_tmp.path().as_os_str()))
            .arg(set("HOME", env.home_root.as_os_str()))
            .arg(set("SOT_COMM_HOME", env.comm_root.as_os_str()))
            .arg(set("PATH", &std::env::var_os("PATH").unwrap_or_default()))
            .arg("--")
            .arg(crate::support::sotd_program())
            .arg("--socket")
            .arg(&env.socket_path)
            .arg("--project-root")
            .arg(&env.daemon_project_root)
            .status()
            .expect("systemd-run the case's unit");
        assert!(
            started.success(),
            "{how}: systemd-run --user --unit {unit} failed ({started})"
        );
        let answered = answers_within(&env.socket_path, crate::support::BOUND).await;
        let stimulus = match how {
            "stop" => systemctl(&["stop", &unit]).0,
            _ => systemctl(&["kill", "--kill-who=main", "--signal=TERM", &unit]).0,
        };
        // Long enough for the end, then for a restart a failed end would bring (RestartSec=1).
        let mut ended = false;
        for _ in 0..150 {
            if std::os::unix::net::UnixStream::connect(&env.socket_path).is_err() {
                ended = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let restarted = answers_within(&env.socket_path, Duration::from_secs(5)).await;
        let (_, shown) = systemctl(&["show", &unit, "--property=ActiveState,Result,NRestarts"]);
        systemctl(&["stop", &unit]);
        systemctl(&["reset-failed", &unit]);
        assert!(answered, "{how}: the unit's daemon never answered");
        assert!(stimulus, "{how}: systemctl refused the stimulus");
        assert!(ended, "{how}: the unit's daemon did not end");
        assert!(
            !restarted,
            "{how}: the unit was restarted after a clean INT/TERM end:\n{shown}"
        );
        for line in ["ActiveState=inactive", "Result=success", "NRestarts=0"] {
            assert!(
                shown.lines().any(|l| l == line),
                "{how}: {line} missing:\n{shown}"
            );
        }
    }
}

#[tokio::test]
#[ignore = "needs a Julia 1.12 (SOT_JULIA_BIN, else julia); run by the harness job with --ignored"]
async fn a_close_that_outlasts_its_bound_ends_with_1_and_takes_the_ephemeral_trees() {
    let _serial = SERIAL.lock().await;
    let mut fx = Fixture::new("close_backstop_with_live_birth");
    let spinning = start_spinning_with(
        "obk",
        &mut fx,
        false,
        &[("SOT_TEST_SHUTDOWN_BOUND_MS", "1")],
    )
    .await;
    let mut run = spinning.run;
    spinning.task.abort();
    crate::guard::close_by_lease(&run.env).await;
    let status = run.status_within(Duration::from_secs(60)).await;
    fx.save("status", format!("{status:?}"));
    fx.save(
        "tree_ended",
        all_ended(&fx, &spinning.ids, Duration::from_secs(10)),
    );
    let log = run.said();
    let cleanup = fx.cleanup();
    drop(run);
    assert!(cleanup.complete(), "{cleanup:?}");
    let status = status.unwrap_or_else(|| panic!("the backstop did not end the daemon:\n{log}"));
    assert_eq!(status.code(), Some(1), "{status:?}\n{log}");
    assert_eq!(
        fx.saved("tree_ended"),
        Some("true"),
        "the REPL's tree outlived the daemon's backstop exit"
    );
}

/// The early commands run before any daemon exists and keep their own statuses: a bad agent-exec recipe is 2, a sotd with
/// no sot-capsule beside it refuses the boot with 1, and a process with no derivable config directory refuses with 78.
#[tokio::test]
async fn early_commands_and_boot_refusals_keep_their_statuses() {
    let _serial = SERIAL.lock().await;
    let output = crate::support::sotd_command()
        .args(["agent-exec", "no-such-agent-kind"])
        .output()
        .expect("run sotd agent-exec");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("sotd agent-exec:"),
        "{output:?}"
    );

    let dir = tempfile::tempdir().expect("a temporary folder");
    let alone = dir.path().join("sotd");
    std::fs::copy(crate::support::sotd_program(), &alone).expect("copy sotd alone into a folder");
    let env = Env::new("early");
    let output = crate::support::sotd_command_at(&alone)
        .arg("--socket")
        .arg(&env.socket_path)
        .arg("--project-root")
        .arg(&env.daemon_project_root)
        .env("XDG_STATE_HOME", &env.state_root)
        .env("XDG_CONFIG_HOME", &env.config_root)
        .env("HOME", &env.home_root)
        .env("SOT_SELF_HOST", crate::support::TEST_STATE_HOST)
        .env("SOT_RUNTIME_DIR", env._runtime_tmp.path())
        .output()
        .expect("run a sotd with no capsule beside it");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("sot-capsule is missing"),
        "{output:?}"
    );

    let output = crate::support::sotd_command()
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .arg("--socket")
        .arg(&env.socket_path)
        .output()
        .expect("run sotd with no home");
    assert_eq!(output.status.code(), Some(78), "{output:?}");
}
