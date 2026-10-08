//! A successor daemon and a capsule birth that is still in flight. The invariant (R4): the claim on a row's
//! supervisor fence is taken before a capsule birth is accepted and is carried unbroken to the supervisor's first
//! act, so a successor that activates the row while the original is held finds the claim, starts no second
//! supervisor, and later attaches to the original. Real `sotd` and `sot-capsule` on real roots; the original is held
//! by a phase barrier (`sot_log::test_barrier`) with the fixture's authority over it acknowledged first, the
//! predecessor is ended by SIGKILL, and nothing the product does is used to clean up.
//!
//! Needs the barrier build of the capsule binary (see main.rs). With the ordering of the parent commit the case is
//! red by design: the original has not yet claimed anything when the successor looks, so the successor starts a
//! second supervisor that takes the fence, and the original, once released, loses it. The case is `#[ignore]` until
//! the claim lands in the next commit; `cargo test ... -- --ignored` runs it and shows that red.

use crate::fixture_owner::Fixture;
use crate::observations::{BarrierDir, Report};
use crate::support::{
    call, connect_and_hello, find_row, poll_until, sot_capsule_exe, Conn, Env, BOUND,
    CAPSULE_EXE_NAME,
};
use base64::Engine as _;
use sot_protocol::op;
use std::path::PathBuf;
use std::time::{Duration, Instant};

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// How long the successor's activation of the held row is watched for a second birth.
const ACTIVATION_WINDOW: Duration = Duration::from_secs(20);

/// The case's state between its stages: the roots, the barrier folder, the fixture and the held original.
struct Case {
    env: Env,
    barriers: BarrierDir,
    barrier_dir: String,
    fx: Fixture,
    workspace_id: String,
    target: String,
    original: Report,
    original_index: usize,
}

impl Case {
    /// 1. The predecessor accepts a real capsule row; the original supervisor is held before it claims anything.
    async fn accept_and_hold() -> Case {
        assert!(
            sot_capsule_exe().is_file(),
            "{CAPSULE_EXE_NAME} not found at {:?}: build it with --features native-barrier into this target first",
            sot_capsule_exe()
        );
        let env = Env::new("succ");
        let barriers = BarrierDir::new(env._tmp.path());
        barriers.hold("pre_fence", 1);
        let barrier_dir = barriers.path().to_string_lossy().into_owned();
        let mut fx = Fixture::new("successor_activation_with_original_held");
        env.spawn_sotd_with_env(&[("SOT_TEST_BARRIER_DIR", &barrier_dir)]);
        let (mut conn, next_id) = connect_and_hello(&env.socket_path).await;
        let created = call(
            &mut conn,
            next_id,
            op::WORKSPACE_CREATE,
            serde_json::json!({ "label": "held-row", "project_root": env.workspace_project_root.to_string_lossy(), "runtime": "capsule" }),
        )
        .await
        .payload;
        assert!(
            created.get("error").is_none(),
            "workspace.create failed: {created:?}"
        );
        let workspace_id = created["workspace_id"]
            .as_str()
            .expect("workspace_id")
            .to_string();
        let target = created["session_name"]
            .as_str()
            .expect("session_name")
            .to_string();
        let original = barriers.reached("pre_fence", 0, BOUND);
        let original_index = fx
            .adopt(original.pid, Some(original.created), "original supervisor")
            .expect("the fixture's authority over the held original, before anything else happens to it");
        fx.save(
            "original_source_domain",
            format!(
                "pgid {} sid {} ppid {}",
                original.pgid, original.sid, original.ppid
            ),
        );
        Case {
            env,
            barriers,
            barrier_dir,
            fx,
            workspace_id,
            target,
            original,
            original_index,
        }
    }

    /// 2. The predecessor dies by SIGKILL; the original stays held.
    fn end_predecessor(&mut self) {
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
            .identity(self.original_index)
            .exited(Duration::from_millis(300));
        self.fx
            .save("original_alive_after_predecessor_death", alive);
    }

    /// 3. The successor boots on the same roots and the held row is activated; what it does about a second
    /// supervisor, and whether any authority answers while the original is held, is saved. Returns its connection.
    async fn activate_successor(&mut self) -> (Conn, u64, PathBuf) {
        self.env
            .spawn_sotd_with_env(&[("SOT_TEST_BARRIER_DIR", &self.barrier_dir)]);
        let (mut conn, mut next_id) = connect_and_hello(&self.env.socket_path).await;
        let opened = call(&mut conn, next_id, op::PTY_OPEN, serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": self.target })).await;
        next_id += 1;
        self.fx.save(
            "pty_open_code",
            opened.payload["code"].as_str().unwrap_or("none"),
        );
        let listed = call(
            &mut conn,
            next_id,
            op::WORKSPACE_LIST,
            serde_json::json!({}),
        )
        .await
        .payload;
        next_id += 1;
        let state_dir = PathBuf::from(
            find_row(&listed, &self.workspace_id)
                .and_then(|row| row["state_dir"].as_str().map(str::to_owned))
                .expect("the row's state dir"),
        );
        let deadline = Instant::now() + ACTIVATION_WINDOW;
        let mut second_birth = None;
        while Instant::now() < deadline && second_birth.is_none() {
            if self.barriers.has_reached("pre_fence", 1) {
                second_birth = Some(self.barriers.reached("pre_fence", 1, BOUND));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.fx.save("second_birth", second_birth.is_some());
        if let Some(second) = second_birth {
            let _ = self
                .fx
                .adopt(second.pid, Some(second.created), "second supervisor");
        }
        let answering = status_of(&state_dir).await;
        self.fx
            .save("authority_answers_while_original_held", answering.is_some());
        if let Some((pid, created)) = answering {
            self.fx.save(
                "answering_authority_is_the_original",
                pid == self.original.pid && created == self.original.created,
            );
        }
        let alive = !self.fx.identity(self.original_index).exited(Duration::ZERO);
        self.fx.save("original_alive_during_activation", alive);
        (conn, next_id, state_dir)
    }

    /// 4. Only now the original is released. It completes its takeover; the successor must reach that same original,
    /// and a fresh nonce must make the round trip through it.
    async fn release_and_attach(&mut self, conn: &mut Conn, mut next_id: u64, state_dir: &PathBuf) {
        self.barriers.open("pre_fence", 0);
        let dir = state_dir.clone();
        let (pid, created) = poll_until(
            || {
                let dir = dir.clone();
                async move { status_of(&dir).await }
            },
            BOUND,
            "an authority to answer once the original is released",
        )
        .await;
        self.fx.save(
            "authority_after_release_is_the_original",
            pid == self.original.pid && created == self.original.created,
        );
        let alive = !self
            .fx
            .identity(self.original_index)
            .exited(Duration::from_millis(300));
        self.fx.save("original_alive_after_release", alive);

        let nonce = format!("l2-nonce-{}", std::process::id());
        let input = call(
            conn,
            next_id,
            op::PTY_INPUT,
            serde_json::json!({
                "workspace_id": self.workspace_id,
                "data_b64": base64::engine::general_purpose::STANDARD.encode(format!("echo {nonce}")),
                "enter": true,
                "origin": "l2-successor-test",
            }),
        )
        .await;
        next_id += 1;
        let echo_deadline = Instant::now() + BOUND;
        let echoed = loop {
            let id = next_id;
            next_id += 1;
            let screen = call(
                conn,
                id,
                op::PTY_SCREEN,
                serde_json::json!({ "workspace_id": self.workspace_id }),
            )
            .await
            .payload;
            let seen = screen["lines"].as_array().is_some_and(|lines| {
                lines
                    .iter()
                    .any(|line| line.as_str().is_some_and(|l| l.trim_end() == nonce))
            });
            if seen || Instant::now() >= echo_deadline {
                break seen;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        self.fx.save(
            "nonce_round_trip",
            format!("{echoed} (input ok: {})", input.payload["ok"]),
        );
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
        assert_eq!(
            saved("original_alive_after_predecessor_death"),
            "true",
            "the original must outlive the predecessor"
        );
        assert_eq!(
            saved("second_birth"),
            "false",
            "the successor started a second supervisor while the original was held"
        );
        assert_eq!(
            saved("authority_answers_while_original_held"),
            "false",
            "an authority answered while the original was held: {}",
            saved("answering_authority_is_the_original")
        );
        assert_eq!(
            saved("original_alive_during_activation"),
            "true",
            "the original ended during the successor's activation"
        );
        assert_eq!(
            saved("authority_after_release_is_the_original"),
            "true",
            "the authority after the release is not the original"
        );
        assert_eq!(
            saved("original_alive_after_release"),
            "true",
            "the original ended after it was released"
        );
        assert!(
            saved("nonce_round_trip").starts_with("true"),
            "the fresh nonce did not come back through the successor: {}",
            saved("nonce_round_trip")
        );
    }
}

/// The (pid, start identity) of the authority that answers on `state_dir`'s supervisor lane, if one does.
async fn status_of(state_dir: &std::path::Path) -> Option<(i32, u64)> {
    let dir = state_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        sot_log::attach_client::supervisor_client::query_status(&dir)
            .ok()
            .map(|(report, _)| (report.pid as i32, report.created))
    })
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "red by design until the claim-before-acceptance ordering lands (R4.2); run with --ignored to see the old-ordering red"]
async fn successor_activation_with_original_held() {
    let _serial = SERIAL.lock().await;
    let mut case = Case::accept_and_hold().await;
    case.end_predecessor();
    let (mut conn, next_id, state_dir) = case.activate_successor().await;
    case.release_and_attach(&mut conn, next_id, &state_dir)
        .await;
    case.finish().await;
}
