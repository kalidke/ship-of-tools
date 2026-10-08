//! `handle_workspace_reauth` and `write_accept_then`: the refusal payload, the record, and the accept-frame ordering.

use super::support_tests::*;
use super::*;
use crate::agents::accounts::claude_config_dir;

/// The peer the ordering rule turns on, in its two states: one that
/// accepts what it is handed, one that is already GONE (the dead or
/// non-draining peer `write_frame_to` answers with an `Err`). The bytes
/// are shared rather than owned so the restart closure can see what
/// reached the wire BEFORE it ran.
struct Peer {
    written: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    gone: bool,
}

impl tokio::io::AsyncWrite for Peer {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.gone {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the peer is gone",
            )));
        }
        self.written.lock().unwrap().extend_from_slice(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

// The refusal payload carries the accounts this daemon can see, so no
// caller re-implements discovery to explain one.
#[tokio::test]
async fn an_unknown_row_is_refused_and_the_refusal_names_the_accounts() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    pin_home(home.path(), scratch.path());
    let reg = Workspaces::new();
    let (payload, restart) = reauth(&reg, "sot-ws-nope", "team", "sid").await;
    assert!(restart.is_none(), "a refusal hands back no restart");
    assert_eq!(payload["code"], "unknown_workspace");
    let names: Vec<String> = payload["accounts"]
        .as_array()
        .expect("accounts array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["default".to_string(), "team".to_string()]);
}

// ORDERING 1 (ADR 0046 decision 6): the new account is on the row AND
// in its toml before anything can spawn a replacement — the spawn reads
// `Workspace::account` back off the registry, so this is the one order
// that leaves a single truth. The row is mutated in exactly ONE field:
// id, slug, root and the DECLARED handle all survive.
#[tokio::test]
async fn the_record_carries_the_new_account_before_the_replacement_is_spawned() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    pin_home(home.path(), scratch.path());
    seed_claude_binary(home.path());
    let root = project_root(home.path(), "reauth-row");
    seed_transcript(&claude_config_dir(home.path(), "team"), "sid-7", &[&root]);
    let (reg, id, slug) = seed_capsule_row(&root, "", "row-declared-handle");

    let (payload, restart) = reauth(&reg, &id, "team", "sid-7").await;
    assert!(payload.get("error").is_none(), "must be accepted: {payload:?}");
    let plan = restart.expect("an accept hands the restart back to the caller");

    let after = reg.resolve(Some(&id)).expect("the row is never replaced");
    assert_eq!(after.account(), "team");
    assert_eq!(after.workspace_id, id);
    assert_eq!(after.slug, slug);
    assert_eq!(after.agent_handle(), "row-declared-handle");
    assert_eq!(after.agent(), "claude");

    let toml = std::fs::read_to_string(crate::rows::store::toml_path_for(&slug))
        .expect("the row's toml is persisted, not just held in memory");
    assert!(toml.contains("account       = \"team\""), "{toml}");
    assert!(toml.contains("agent_handle  = \"row-declared-handle\""), "{toml}");

    // Nothing has spawned or been ended yet: that is `restart_blocking`'s
    // job, and it has not run.
    drop(plan);
}

// ORDERING 2: the accept is ANSWERABLE before anything is torn down.
// The accept path never dials the row's lane — its state dir does not
// even exist here — and the only thing that can end the leg needs the
// `ReauthRestart` this call hands BACK. That the frame is physically
// written before the restart is handed that plan is ORDERING 3 below.
#[tokio::test]
async fn the_accept_is_answered_before_the_leg_is_touched() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    pin_home(home.path(), scratch.path());
    seed_claude_binary(home.path());
    let root = project_root(home.path(), "reauth-row");
    seed_transcript(&claude_config_dir(home.path(), "team"), "sid-7", &[&root]);
    let (reg, id, _slug) = seed_capsule_row(&root, "", "row-declared-handle");

    let (payload, restart) = reauth(&reg, &id, "team", "sid-7").await;
    assert_eq!(payload["code"], ACCEPTED_CODE);
    assert_eq!(payload["account"], "team");
    assert_eq!(payload["workspace_id"], id);
    assert!(restart.is_some(), "the effect is deferred to the caller, after the write");

    let state_root = sot_log::host::state_dir::sot_state_dir().expect("pinned state root");
    let state_dir = crate::rows::spawn::state_root::state_dir_for(&state_root, &id);
    assert!(
        !state_dir.exists(),
        "the accept path must not have dialed, created or ended anything: {state_dir:?}"
    );
    drop(restart);
}

// ORDERING 3: the accept frame is PHYSICALLY WRITTEN before anything is
// handed the plan that can end the leg — the dispatcher's whole
// contract, pinned here rather than left to two adjacent statements in
// `server/dispatch.rs`.
#[tokio::test]
async fn the_accept_frame_is_on_the_wire_before_the_restart_is_handed_the_plan() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    let (reg, id, slug) = accept_fixture(home.path(), scratch.path());

    let (out, restart) = reauth_out(&reg, &id, "team", "sid-7").await;
    let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut peer = Peer { written: written.clone(), gone: false };
    let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (seen_then, written_then) = (seen.clone(), written.clone());
    write_accept_then(&mut peer, &out, restart, move |plan| {
        *seen_then.lock().unwrap() = Some(written_then.lock().unwrap().clone());
        // Never end a real leg from a unit test: dropping the plan
        // releases the row's guard and touches nothing.
        drop(plan);
    })
    .await
    .expect("a peer that takes the write leaves the restart to run");

    let seen = seen.lock().unwrap().clone().expect("the restart was handed the plan");
    assert!(
        String::from_utf8_lossy(&seen).contains(ACCEPTED_CODE),
        "the accept must already be on the wire when the restart runs: {:?}",
        String::from_utf8_lossy(&seen)
    );
    // Nothing rolled back: this is the accept that was read.
    assert_eq!(reg.resolve(Some(&id)).unwrap().account(), "team");
    let toml = std::fs::read_to_string(crate::rows::store::toml_path_for(&slug)).unwrap();
    assert!(toml.contains("account       = \"team\""), "{toml}");
}

// ORDERING 4: the THIRD outcome — the accept never reached its reader.
// Nothing was torn down, so the live leg still spends the OLD login and
// the record has to go back to saying so; the error still propagates,
// because that connection is over either way.
#[tokio::test]
async fn an_accept_that_cannot_be_written_rolls_the_record_back() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    let (reg, id, slug) = accept_fixture(home.path(), scratch.path());

    let (out, restart) = reauth_out(&reg, &id, "team", "sid-7").await;
    assert!(restart.is_some(), "the fixture must reach the accept");
    let mut peer = Peer { written: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())), gone: true };
    let err = write_accept_then(&mut peer, &out, restart, |_plan| {
        panic!("the restart must never run when the accept did not reach the caller")
    })
    .await
    .expect_err("a write that fails still ends the connection");
    assert!(format!("{err:#}").contains("the peer is gone"), "{err:#}");

    let after = reg.resolve(Some(&id)).expect("the row itself survives");
    assert_eq!(after.account(), "", "the record is back on the login the live leg spends");
    let toml = std::fs::read_to_string(crate::rows::store::toml_path_for(&slug)).unwrap();
    assert!(toml.contains("account       = \"\""), "{toml}");
    assert!(toml.contains("agent_handle  = \"row-declared-handle\""), "{toml}");
}

// NOTE 7: the record keeps the normalized `""`, but the REPLY names the
// account, or `sot-fe` prints `account=` and the human reads a field
// that failed to fill.
#[tokio::test]
async fn switching_to_the_default_account_answers_with_its_name_not_an_empty_string() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    pin_home(home.path(), scratch.path());
    seed_claude_binary(home.path());
    let root = project_root(home.path(), "reauth-row");
    seed_transcript(&claude_config_dir(home.path(), ""), "sid-7", &[&root]);
    let (reg, id, _slug) = seed_capsule_row(&root, "team", "row-declared-handle");

    let (payload, restart) = reauth(&reg, &id, "default", "sid-7").await;
    assert_eq!(payload["code"], ACCEPTED_CODE);
    assert_eq!(payload["account"], "default", "the reply names the account: {payload:?}");
    assert_eq!(
        reg.resolve(Some(&id)).unwrap().account(),
        "",
        "the record still holds the normalized default"
    );
    drop(restart);
}

// NOTE 8: the accounts ride on refusals decided AFTER the guard too,
// not just on the pre-guard ones — `persist_failed` is the one such
// refusal a test can provoke on demand (a config root that cannot hold
// the row's toml), and it is also the only path that puts the account
// back without a `ReauthRestart` to do it.
#[tokio::test]
async fn a_row_whose_toml_cannot_be_written_is_refused_with_the_accounts_and_the_account_put_back() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    pin_home(home.path(), scratch.path());
    seed_claude_binary(home.path());
    let root = project_root(home.path(), "reauth-row");
    seed_transcript(&claude_config_dir(home.path(), "team"), "sid-7", &[&root]);
    // A config root that is a FILE: nothing can create the row's toml
    // under it, so `save` fails where every other step has succeeded.
    let blocked = scratch.path().join("not-a-directory");
    std::fs::write(&blocked, b"").unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &blocked);
    std::env::set_var("LOCALAPPDATA", &blocked);
    let (reg, id, _slug) = seed_capsule_row(&root, "", "row-declared-handle");

    let (payload, restart) = reauth(&reg, &id, "team", "sid-7").await;
    assert!(restart.is_none(), "a refusal hands back no restart");
    assert_eq!(payload["code"], "persist_failed");
    let names: Vec<String> = payload["accounts"]
        .as_array()
        .expect("a post-guard refusal carries the accounts too")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["default".to_string(), "team".to_string()]);
    assert_eq!(
        reg.resolve(Some(&id)).unwrap().account(),
        "",
        "an unpersisted switch leaves the record on the account the leg spends"
    );
}
