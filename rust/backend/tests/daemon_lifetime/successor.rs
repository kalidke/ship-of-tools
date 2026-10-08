//! A successor daemon and a capsule birth that is still in flight. The invariant (R4): the claim on a row's
//! supervisor fence is taken before a capsule birth is accepted and is carried unbroken to the supervisor's first
//! act, so a successor that activates the row while the original is held finds the claim, starts no second
//! supervisor, and later attaches to the original. Real `sotd` and `sot-capsule` on real roots; the original is held
//! by a phase barrier (`sot_log::test_barrier`) with the fixture's authority over the held process acknowledged
//! first, the predecessor is ended by SIGKILL, and nothing the product does is used to clean up.
//!
//! The barrier is one of five places on the birth's way to its first act: in the durable parent after the claim and
//! before the fork (`parent_accepted`), with the supervisor forked and set up (`parent_ready`), before the gate
//! opens (`parent_release`), in the new supervisor before it adopts the claim (`pre_fence`), and after it adopted
//! the claim and before it told the parent (`claim_adopted`).
//!
//! Needs the barrier build of the capsule binary (see main.rs).

use crate::fixture_owner::Fixture;
use crate::observations::BarrierDir;
use crate::support::{
    call, connect_and_hello, find_row, poll_until, sot_capsule_exe, Conn, Env, BOUND,
    CAPSULE_EXE_NAME,
};
use sot_protocol::op;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::SERIAL;

/// How long the successor's activation of the held row is watched for a second birth after it has answered.
const ACTIVATION_WINDOW: Duration = Duration::from_secs(3);

/// The phases where the held process is the supervisor itself (the original), so one birth has been seen.
fn original_is_born(phase: &str) -> bool {
    matches!(phase, "pre_fence" | "claim_adopted")
}

/// The case's state between its stages: the roots, the barrier folder, the fixture and the held process.
struct Case {
    env: Env,
    barriers: BarrierDir,
    barrier_dir: String,
    fx: Fixture,
    phase: &'static str,
    held_index: usize,
    create: Option<tokio::task::JoinHandle<()>>,
}

impl Case {
    /// 1. The predecessor is asked to create a real capsule row; the birth is held at `phase`.
    async fn accept_and_hold(phase: &'static str) -> Case {
        assert!(
            sot_capsule_exe().is_file(),
            "{CAPSULE_EXE_NAME} not found at {:?}: build it with --features native-barrier into this target first",
            sot_capsule_exe()
        );
        let env = Env::new("succ");
        let barriers = BarrierDir::new(env._tmp.path());
        barriers.hold(phase, 1);
        let barrier_dir = barriers.path().to_string_lossy().into_owned();
        let mut fx = Fixture::new(&format!("successor_activation_with_original_held::{phase}"));
        env.spawn_sotd_with_env(&[("SOT_TEST_BARRIER_DIR", &barrier_dir)]);
        let (mut conn, next_id) = connect_and_hello(&env.socket_path).await;
        let project = env.workspace_project_root.to_string_lossy().into_owned();
        // The create does not answer while the birth is held in the parent: it runs in a task the case ends with.
        let create = tokio::spawn(async move {
            let _ = call(
                &mut conn,
                next_id,
                op::WORKSPACE_CREATE,
                serde_json::json!({ "label": "held-row", "project_root": project, "runtime": "capsule" }),
            )
            .await;
        });
        let began = Instant::now();
        while !barriers.has_reached(phase, 0) {
            assert!(
                began.elapsed() < BOUND,
                "nothing reached the {phase} barrier within {BOUND:?}; the daemon's log:\n{}",
                daemon_log(&env)
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let held = barriers.reached(phase, 0, BOUND);
        let held_index = fx
            .adopt(held.pid, Some(held.created), "held process")
            .expect(
                "the fixture's authority over the held process, before anything else happens to it",
            );
        fx.save(
            "held_source_domain",
            format!("pgid {} sid {} ppid {}", held.pgid, held.sid, held.ppid),
        );
        Case {
            env,
            barriers,
            barrier_dir,
            fx,
            phase,
            held_index,
            create: Some(create),
        }
    }

    /// 2. The predecessor dies by SIGKILL; the held process stays held.
    fn end_predecessor(&mut self) {
        if let Some(create) = self.create.take() {
            create.abort();
        }
        let mut predecessor = self
            .env
            .daemon
            .borrow_mut()
            .take()
            .expect("the predecessor");
        predecessor.kill().expect("SIGKILL the predecessor");
        predecessor.wait().expect("reap the predecessor");
        let alive = !self
            .fx
            .identity(self.held_index)
            .exited(Duration::from_millis(300));
        self.fx.save("held_alive_after_predecessor_death", alive);
    }

    /// How many supervisors have reached their first act so far.
    fn births(&self) -> usize {
        (0..)
            .take_while(|n| self.barriers.has_reached("pre_fence", *n))
            .count()
    }

    /// 3. The successor boots on the same roots and the held row is activated; what it does about a second
    /// supervisor, and whether any authority answers while the birth is held, is saved.
    async fn activate_successor(&mut self) -> (Conn, u64, PathBuf, String) {
        self.env
            .spawn_sotd_with_env(&[("SOT_TEST_BARRIER_DIR", &self.barrier_dir)]);
        let (mut conn, mut next_id) = connect_and_hello(&self.env.socket_path).await;
        let listed = call(
            &mut conn,
            next_id,
            op::WORKSPACE_LIST,
            serde_json::json!({}),
        )
        .await
        .payload;
        next_id += 1;
        let row = listed["workspaces"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["label"] == "held-row"))
            .unwrap_or_else(|| panic!("the held row was not persisted: {listed:?}"))
            .clone();
        let workspace_id = row["workspace_id"]
            .as_str()
            .expect("workspace_id")
            .to_string();
        let target = row["session_name"]
            .as_str()
            .expect("session_name")
            .to_string();
        let state_dir = PathBuf::from(
            find_row(&listed, &workspace_id)
                .and_then(|r| r["state_dir"].as_str().map(str::to_owned))
                .expect("the row's state dir"),
        );
        let opened = call(
            &mut conn,
            next_id,
            op::PTY_OPEN,
            serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": target }),
        )
        .await;
        next_id += 1;
        self.fx.save(
            "pty_open_code",
            opened.payload["code"].as_str().unwrap_or("none"),
        );
        tokio::time::sleep(ACTIVATION_WINDOW).await;
        let births = self.births();
        self.fx.save("births_while_held", births);
        // A birth beyond the original is the product's failure; it is adopted so that cleanup can end it.
        for n in usize::from(original_is_born(self.phase))..births {
            let second = self.barriers.reached("pre_fence", n, BOUND);
            let _ = self
                .fx
                .adopt(second.pid, Some(second.created), "an extra supervisor");
        }
        let answering = status_of(&state_dir).await;
        self.fx
            .save("authority_answers_while_held", answering.is_some());
        let alive = !self.fx.identity(self.held_index).exited(Duration::ZERO);
        self.fx.save("held_alive_during_activation", alive);
        (conn, next_id, state_dir, workspace_id)
    }

    /// 4. Only now the held process is released. The birth completes; the successor must reach the supervisor the
    /// first birth forked, and a fresh nonce must make the round trip through it.
    async fn release_and_attach(
        &mut self,
        conn: &mut Conn,
        mut next_id: u64,
        state_dir: &PathBuf,
        workspace_id: &str,
    ) {
        self.barriers.open(self.phase, 0);
        let dir = state_dir.clone();
        let (pid, created, _) = poll_until(
            || {
                let dir = dir.clone();
                async move { status_of(&dir).await.filter(|status| status.2) }
            },
            BOUND,
            "an authority to be ready once the held process is released",
        )
        .await;
        let original = self.barriers.reached("pre_fence", 0, BOUND);
        if !original_is_born(self.phase) {
            // The original was born only now: the fixture takes authority over it for the cleanup.
            let _ = self.fx.adopt(
                original.pid,
                Some(original.created),
                "the original supervisor",
            );
        }
        self.fx.save(
            "authority_after_release_is_the_original",
            pid == original.pid && created == original.created,
        );
        let births = self.births();
        self.fx.save("births_in_all", births);

        let outcome =
            crate::guard::nonce_round_trip(conn, &mut next_id, workspace_id, "l2-successor-test")
                .await;
        self.fx.save("nonce_round_trip", outcome);
    }

    /// Cleanup comes after everything the product did is saved, and uses only the fixture's identities.
    async fn finish(mut self) {
        let cleanup = self.fx.cleanup();
        self.env.kill_daemon_bounded().await;
        assert!(
            cleanup.complete(),
            "fixture cleanup left survivors: {cleanup:?}"
        );
        let saved = |key: &str| self.fx.saved(key).unwrap_or("not recorded").to_string();
        let phase = self.phase;
        let expected_births = usize::from(original_is_born(phase));
        assert_eq!(
            saved("held_alive_after_predecessor_death"),
            "true",
            "{phase}: the held process must outlive the predecessor"
        );
        assert_eq!(
            saved("births_while_held"),
            expected_births.to_string(),
            "{phase}: the successor started a supervisor while the birth was held"
        );
        assert_eq!(
            saved("authority_answers_while_held"),
            "false",
            "{phase}: an authority answered while the birth was held"
        );
        assert_eq!(
            saved("held_alive_during_activation"),
            "true",
            "{phase}: the held process ended during the successor's activation"
        );
        assert_eq!(
            saved("authority_after_release_is_the_original"),
            "true",
            "{phase}: the authority after the release is not the original"
        );
        assert_eq!(
            saved("births_in_all"),
            "1",
            "{phase}: more than one supervisor was born"
        );
        assert!(
            saved("nonce_round_trip").starts_with("true"),
            "{phase}: the fresh nonce did not come back through the successor: {}",
            saved("nonce_round_trip")
        );
    }
}

/// The tail of the daemon's own log, for a failure message.
fn daemon_log(env: &Env) -> String {
    let log = std::fs::read_to_string(env.state_root.join("sot").join("sotd.log"))
        .unwrap_or_else(|e| format!("(no log: {e})"));
    log.lines()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

/// The (pid, start identity, ready) of the authority that answers on `state_dir`'s supervisor lane, if one does.
async fn status_of(state_dir: &std::path::Path) -> Option<(i32, u64, bool)> {
    let dir = state_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        sot_log::attach_client::supervisor_client::query_status(&dir)
            .ok()
            .map(|(report, _)| {
                (
                    report.pid as i32,
                    report.created,
                    report.phase == sot_log::lane::wire::SupervisorPhase::Ready,
                )
            })
    })
    .await
    .unwrap()
}

async fn run(phase: &'static str) {
    let _serial = SERIAL.lock().await;
    let mut case = Case::accept_and_hold(phase).await;
    case.end_predecessor();
    let (mut conn, next_id, state_dir, workspace_id) = case.activate_successor().await;
    case.release_and_attach(&mut conn, next_id, &state_dir, &workspace_id)
        .await;
    case.finish().await;
}

#[tokio::test]
async fn successor_activation_with_the_birth_accepted_before_the_fork() {
    run("parent_accepted").await;
}

#[tokio::test]
async fn successor_activation_with_the_supervisor_forked_and_gated() {
    run("parent_ready").await;
}

#[tokio::test]
async fn successor_activation_with_the_gate_about_to_open() {
    run("parent_release").await;
}

#[tokio::test]
async fn successor_activation_with_original_held() {
    run("pre_fence").await;
}

#[tokio::test]
async fn successor_activation_with_the_claim_adopted_and_not_yet_acknowledged() {
    run("claim_adopted").await;
}
