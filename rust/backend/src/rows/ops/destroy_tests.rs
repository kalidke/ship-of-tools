//! Tests of destroy.rs: workspace.destroy on the default row, with the state-root fixtures they share.

// Default-row end-run: a default TMUX row keeps the flat refusal
// (no run to end). A default CAPSULE row (ADR 0043 decision 22: on
// any host the capsule runtime compiles for, not just Windows) ends
// its run and keeps the row, reporting the outcome in
// `WorkspaceDestroyRes::kept` — but only when CONFIRMED
// (`Removable`); `Kept` still returns the typed
// `capsule_end_not_reached` error, never the flat tmux-style refusal.
use super::*;
use crate::rows::Workspace;

// Isolates `crate::rows::store::save`'s config dir -- `pin_local_
// state_root` below now pins `XDG_CONFIG_HOME` unconditionally
// alongside state, so this is no longer "the one test below" that
// reaches the reset+persist path for real: an `Orphaned` outcome
// (this fix) can make ANY of them reach a real toml write or removal,
// and every one now runs through the same scratch config root.
// Same technique as `rows/store/support_tests.rs`'s own `env_guarded`, serialized
// under the crate-wide lock so this never races another module's
// env-mutating test. Ungated since the macOS wiring lane: the
// absence proof `seed_provably_unheld_state_dir` builds means
// something on every host this daemon builds for, so this cluster
// has real callers everywhere and needs no dead-code suppression.
struct EnvGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
    xdg_config_home: Option<std::ffi::OsString>,
    // Added alongside `seed_provably_unheld_state_dir` below (ADR
    // 0043 decision 33, Codex review, 2026-09-11): the tests that used
    // to lean on `mark_capsule_terminal`'s now-deleted unguarded fast
    // path instead point `sot_log::host::state_dir::sot_state_dir()` at a
    // scratch root so `destroy_capsule_workspace`'s real guarded path
    // finds a hermetic, provably-absent state dir there. Both vars are
    // saved/restored on every platform even though `sot_state_dir()`
    // only ever reads ONE of them per platform (`XDG_STATE_HOME` on
    // Unix, `LOCALAPPDATA` on Windows — see `pin_local_state_root`
    // below): a fixture that pinned only `XDG_STATE_HOME` used to be
    // silently ignored by the resolver on Windows CI, which is exactly
    // how the terminal/confirmed-end tests below used to fail there —
    // the fixture built a state dir nobody ever looked at.
    xdg_state_home: Option<std::ffi::OsString>,
    localappdata: Option<std::ffi::OsString>,
    sot_self_host: Option<std::ffi::OsString>,
    sot_comm_home: Option<std::ffi::OsString>,
    // Added for the real-listener refusal proof below (`sot_log::
    // state_dir::runtime_dir` trusts this once it is absolute and
    // private) -- captured/restored exactly like the other five so a
    // test that pins it never leaks the override past its own scope.
    sot_runtime_dir: Option<std::ffi::OsString>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, val) in [
            ("XDG_CONFIG_HOME", &self.xdg_config_home),
            ("XDG_STATE_HOME", &self.xdg_state_home),
            ("LOCALAPPDATA", &self.localappdata),
            ("SOT_SELF_HOST", &self.sot_self_host),
            ("SOT_COMM_HOME", &self.sot_comm_home),
            ("SOT_RUNTIME_DIR", &self.sot_runtime_dir),
        ] {
            match val {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn env_guarded() -> EnvGuard {
    let serial = crate::paths::ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    EnvGuard {
        xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
        xdg_state_home: std::env::var_os("XDG_STATE_HOME"),
        localappdata: std::env::var_os("LOCALAPPDATA"),
        sot_self_host: std::env::var_os("SOT_SELF_HOST"),
        sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
        sot_runtime_dir: std::env::var_os("SOT_RUNTIME_DIR"),
        _serial: serial,
    }
}

/// Points wherever `sot_log::host::state_dir::sot_state_dir()` ACTUALLY reads
/// on this platform (`LOCALAPPDATA` on Windows, `XDG_STATE_HOME`
/// elsewhere — that function's own doc has the precedence) at `dir`,
/// then returns the root by calling that SAME resolver rather than
/// hand-building `dir.join("sot")` here — the one seam every fixture
/// below must agree with `destroy_capsule_workspace` about. `dir`
/// need not exist yet — nothing here creates it; `state_dir_missing`
/// fixtures rely on exactly that. The caller must hold an `EnvGuard`
/// (`env_guarded()`) FIRST, captured before this call touches
/// anything — its `Drop` puts `XDG_CONFIG_HOME`, `XDG_STATE_HOME`,
/// `LOCALAPPDATA`, `SOT_SELF_HOST` and `SOT_COMM_HOME` back to
/// whatever `env_guarded()` observed at that moment, which is every
/// var this function or any of its callers may set — but that
/// restore is only as good as the crate-wide serialization every one
/// of these fixtures shares (`paths::ENV_TEST_LOCK`): it is NOT a
/// claim that `XDG_STATE_HOME` (or any of the five) is protected
/// from a test elsewhere in this binary that mutates it without
/// taking the same lock.
///
/// Also isolates `XDG_CONFIG_HOME` on every non-Windows platform (the
/// field defect this closes): `sot_config_dir()` reads a SEPARATE var
/// there, entirely independent of `XDG_STATE_HOME`
/// (`state_dir.rs`'s own doc — only Windows derives config from the
/// same `LOCALAPPDATA` root), so pinning state alone left config free
/// to resolve to the real `$HOME/.config/sot` the instant any test
/// through this fixture reached `crate::rows::store::save` or a toml
/// removal — exactly the shape of the leaked row this whole fix
/// exists to close: a scratch daemon whose state root was isolated
/// but whose config directory was not, writing (and then, once a row
/// with no state dir can be proven `Orphaned`, DELETING) a real
/// per-host workspace registry entry. Pinned to the SAME `dir` as
/// state (Fable review: no separate sibling path to invent or keep
/// in sync) — `sot_state_dir()` and `sot_config_dir()` both append
/// their own distinct subtree name under it (`state_dir.rs`'s own
/// doc), so the two never collide even sharing one root; "nothing
/// exists under `dir`" fixtures stay literally true either way.
fn pin_local_state_root(dir: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    std::env::set_var("LOCALAPPDATA", dir);
    #[cfg(not(windows))]
    {
        std::env::set_var("XDG_STATE_HOME", dir);
        std::env::set_var("XDG_CONFIG_HOME", dir);
    }
    sot_log::host::state_dir::sot_state_dir()
        .expect("state root must resolve once pinned to a scratch dir")
}

/// Builds a hermetic on-disk state dir that `destroy_capsule_
/// workspace`'s real guarded path (ADR 0043 decision 33) will
/// independently prove BOTH halves of the destroy proof absent for —
/// nothing ever holds `supervisor.lock`, and a published pointer
/// names a voyage whose own `writer.lock` exists and is free — so
/// `end_run`'s `Unheld` arm reports `Removable` with no live process
/// anywhere. Caller must first call `pin_local_state_root` (under
/// `env_guarded`) and pass ITS return value as `state_root` — the
/// resolved root `sot_log::host::state_dir::sot_state_dir()` itself reports,
/// never a hand-built path, so this fixture lands exactly where
/// `destroy_capsule_workspace` (via `state_dir_for`) actually looks.
/// Replaces this module's old reliance on `mark_capsule_terminal`'s
/// deleted unguarded fast path (Codex review, 2026-09-11: that path
/// returned `Removable` on the daemon's own say-so alone, with no
/// proof at all) — same technique `rows/run/end_run.rs`'s own
/// absence-proof unit tests use. Ungated since the macOS wiring
/// lane: the body reaches the capsule runtime in `rows/run/` and `rows/spawn/`, which no
/// longer carries a platform gate at its own root, so this fixture
/// exists wherever the daemon does.
fn seed_provably_unheld_state_dir(state_root: &std::path::Path, workspace_id: &str) {
    let state_dir = crate::rows::spawn::state_root::state_dir_for(state_root, workspace_id);
    std::fs::create_dir_all(&state_dir).expect("create the fake state dir");
    let voyage_id = "a1b2c3d4-e5f6-4890-9abc-def012345678";
    sot_log::supervisor::journal::pointer::publish(&state_dir, voyage_id).expect("publish the pointer");
    let voyage_root = sot_log::supervisor::voyage_root_path(&state_dir, voyage_id);
    std::fs::create_dir_all(&voyage_root).expect("voyage root");
    std::fs::write(voyage_root.join("writer.lock"), b"").expect("writer.lock file");
}

fn seed_default(runtime: &str) -> (Workspaces, String) {
    let reg = Workspaces::new();
    let mut ws = Workspace::from_label(
        "local",
        std::path::PathBuf::from("/p/local"),
        false,
        "none".into(),
        String::new(),
        String::new(),
    );
    ws.runtime = runtime.to_string();
    let id = ws.workspace_id.clone();
    reg.insert(ws);
    reg.set_default(&id);
    (reg, id)
}

/// Same as `seed_default("capsule")` but with a carried-over agent —
/// the field shape (owner once started an agent in this row before
/// the "nothing runs in the anchor" rule existed) that the reset in
/// `end_default_row_run` exists to unstick.
fn seed_default_with_agent(agent: &str, agent_name: &str) -> (Workspaces, String, String) {
    let reg = Workspaces::new();
    let mut ws = Workspace::from_label(
        "local",
        std::path::PathBuf::from("/p/local"),
        true,
        agent.to_string(),
        agent_name.to_string(),
        String::new(),
    );
    ws.runtime = "capsule".to_string();
    let id = ws.workspace_id.clone();
    let slug = ws.slug.clone();
    reg.insert(ws);
    reg.set_default(&id);
    (reg, id, slug)
}

async fn destroy(workspaces: &Workspaces, workspace_id: &str) -> serde_json::Value {
    let session = Session::new();
    let (tx, _rx) = broadcast::channel(16);
    let payload = json!({ "workspace_id": workspace_id });
    let out = handle_workspace_destroy(1, payload, &session, workspaces, &tx)
        .await
        .expect("handler must not error");
    assert_eq!(
        out.len(),
        1,
        "workspace.destroy always answers with exactly one frame"
    );
    out[0].0.payload.clone()
}

// ADR 0043 decision 22: capsule support is no longer Windows-only, so
// a default row explicitly marked "capsule" (a hand-edited toml, or
// later the bridge) now takes the SAME real end-run path a Windows
// one always did — never the flat tmux-style refusal
// (`default_workspace_not_destroyable`). Nothing is actually running
// behind this row in-process, so the real attempt cannot reach a
// live lane. The exact outcome is platform-dependent: on Windows and
// Linux, `destroy_capsule_workspace`'s real path finds no state dir
// at all on disk for this synthetic, never-spawned row AND
// `query_status`'s own connect fails with decision 27's "no listener
// at all" shape (nothing was ever bound at this row's lane address
// either) — the orphan-removal fix this test now covers: proven,
// not merely refused, so the row's run is confirmed ended
// (`orphan_removed`) exactly as a real end would be, never the
// flat refusal AND never a bare `Kept`. One outcome on every host
// since the macOS wiring lane: there is no platform-shaped fallback
// arm left for this to mean something different on.
// Pinned hermetic (Codex review, 2026-09-11): this test used to read
// `sot_log::host::state_dir::sot_state_dir()`'s REAL, unpinned environment —
// fine on a dev box whose shell always exports a stable, qualified
// `XDG_STATE_HOME`, but on CI (nothing exported) it read whatever the
// ambient state root happened to resolve to, unguarded against every
// OTHER test in this module that mutates the SAME process-global vars
// under `env_guarded()`'s lock. Pinning to a fresh, never-created
// scratch root — same resolver, same lock — makes "no state dir on
// disk for this workspace" true by construction, not by luck.
// `pin_local_state_root` now also isolates `XDG_CONFIG_HOME` (the
// harness fix this same effort closes): once this scenario proves
// `Orphaned` instead of merely refusing, the response path really
// does reach `crate::rows::store::save`'s reset-persist write, which
// must never land under a real `~/.config/sot`.
#[tokio::test]
async fn default_capsule_workspace_with_no_state_dir_is_proven_orphaned_not_a_flat_refusal() {
    let _guard = env_guarded();
    let scratch = std::env::temp_dir().join(format!(
        "sot-ws-destroy-missing-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    // The resolved STATE ROOT is created (Fable review, safety: the
    // orphan proof now refuses outright unless the root itself
    // canonicalizes -- `destroy_capsule_workspace`'s own doc) but
    // nothing under it is: the point of this test is that THIS ROW's
    // own state dir does not exist on disk at all, and nothing was
    // ever bound at its lane address either.
    {
        let root = pin_local_state_root(&scratch);
        std::fs::create_dir_all(&root).unwrap();
    }

    let (reg, id) = seed_default("capsule");
    let payload = destroy(&reg, &id).await;
    assert_ne!(
        payload.get("code").and_then(|v| v.as_str()),
        Some("default_workspace_not_destroyable"),
        "a capsule default row must not get the flat tmux-style refusal: {payload:?}"
    );
    {
        assert_eq!(
            payload.get("code").and_then(|v| v.as_str()),
            None,
            "a proven orphan is a CONFIRMED end, not a `Kept` error: {payload:?}"
        );
        let kept = payload.get("kept").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            kept.contains("orphan_removed"),
            "the orphan proof's own distinct outcome must be visible: {payload:?}"
        );
    }
    assert!(reg.resolve(Some(&id)).is_some(), "the default row is never removed either way");

    let _ = std::fs::remove_dir_all(&scratch);
}

/// The refusing half of the same proof, for real (Fable review, item
/// 6): a row with no state dir whose lane IS actually answered by
/// something — a bare listener, bound at exactly this row's own
/// `supervisor-<h>.sock`, that never speaks the protocol back — must
/// never be classified `is_definitely_orphaned`. `connect(2)` itself
/// succeeds against a bound-and-listening socket even with nothing
/// ever `accept`ing it, so `query_status`'s connect step does NOT
/// return decision 27's absent shape (`ENOENT`/`ECONNREFUSED`); it
/// times out instead, waiting on a hello nobody answers — exactly the
/// "something is there, but unresponsive" case that must keep
/// refusing. `SOT_RUNTIME_DIR` is pinned to a fresh, private (owner-
/// only) scratch dir so the real socket path (`sot_log::lane::socket_unix::
/// supervisor_socket_path`) never collides with a real session.
/// `cfg(unix)`: the assertion target is `rows::run::end_run::
/// is_definitely_orphaned`'s refusing half, and the capsule runtime lost
/// its platform gate in the macOS wiring lane — exactly the change
/// this gate's predecessor said it would widen with. The socket half
/// was never the constraint (`sot_log::lane::socket_unix` and
/// `supervisor_client` both compile for Darwin).
#[tokio::test]
#[cfg(unix)]
async fn a_reachable_listener_with_no_state_dir_still_refuses() {
    let _guard = env_guarded();
    let stamp = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    );
    let scratch = std::env::temp_dir().join(format!("sot-ws-destroy-listener-test-state-{stamp}"));
    let root = pin_local_state_root(&scratch);
    std::fs::create_dir_all(&root).unwrap();
    let canonical_root = root.canonicalize().expect("the scratch root was just created");

    // NOT `temp_dir()`: a unix socket path is capped at `sun_path`
    // (104 bytes on macOS, 108 on Linux) and macOS's temp dir is a
    // deep `/var/folders/<..>/<..>/T/` path, so the supervisor socket
    // built under it overflows and this test dies `PathTooLong` before
    // it can assert anything. `/tmp` is also what `runtime_sot_dir`
    // itself falls back to when no private runtime dir exists, which
    // is the production shape on macOS -- so this keeps the test on
    // the same path length the real thing gets. Kept short for the
    // same reason: the name below plus `supervisor-<16 hex>.sock`
    // must still fit.
    let runtime_dir = std::path::PathBuf::from("/tmp").join(format!("sot-wsdl-rt-{stamp}"));
    std::fs::create_dir_all(&runtime_dir).unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::env::set_var("SOT_RUNTIME_DIR", &runtime_dir);

    let (reg, id) = seed_default("capsule");
    // The EXACT path `destroy_capsule_workspace` will dial: the same
    // canonical-root-then-join this fix's own destroy site uses, fed
    // to the SAME hash the production lane address is built from.
    let state_dir = crate::rows::spawn::state_root::state_dir_for(&canonical_root, &id);
    let h = sot_log::host::state_dir::state_dir_hash(&state_dir);
    let sock_path =
        sot_log::lane::socket_unix::supervisor_socket_path(&h).expect("runtime dir was just pinned");
    let _listener = std::os::unix::net::UnixListener::bind(&sock_path)
        .unwrap_or_else(|e| panic!("bind the stand-in listener at {sock_path:?}: {e}"));
    // Never `accept()`s -- proves a merely-unresponsive lane, not an
    // absent one.

    let payload = destroy(&reg, &id).await;
    assert_eq!(
        payload.get("code").and_then(|v| v.as_str()),
        Some("state_dir_missing"),
        "a lane that answers (even silently) must never be treated as orphaned: {payload:?}"
    );
    assert!(reg.resolve(Some(&id)).is_some(), "a refused destroy never removes the row");
    assert!(!state_dir.exists(), "destroy on a missing state dir must never recreate it");

    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_dir_all(&runtime_dir);
}

// ADR 0043 decision 33 (BLOCKER, Codex review, 2026-09-11): a row the
// watchdog already marked `capsule_terminal` no longer takes an
// unguarded shortcut straight to `Removable` -- that deleted fast
// path returned "removable" on the daemon's own say-so alone,
// bypassing the guard AND the fence/leg absence proof, so a leg the
// watchdog's own exhausted restart budget (or a failed adoption) left
// running behind a `Terminal` authority could have been orphaned. A
// terminal row now goes through the SAME guarded resume/end_run path
// as every other row: `resume_locked`'s own internal `is_capsule_
// terminal` check still reports that phase without a live round trip
// (no wasted probe against an authority that is almost always
// already gone), but `end_run`'s fresh `query_status` -- naturally
// unreachable here, nothing is listening -- then reaches the SAME
// independent absence proof every other row does, hermetically
// reproduced via `seed_provably_unheld_state_dir`. Called directly
// (not through `handle_workspace_destroy`) to stay hermetic -- the
// full wire path also removes on-disk tomls under the real config
// dir, which is not safe to exercise from an in-process unit test.
//
// Gated (unlike the deleted portable shortcut this replaces): the
// absence proof this now exercises lives entirely inside
// `destroy_capsule_workspace`'s `#[cfg(any(windows, target_os =
// "linux"))]` arm -- every other host takes the unconditional `Kept`
// fallback regardless of any on-disk fixture.
#[tokio::test]
async fn a_capsule_workspace_marked_terminal_still_needs_the_absence_proof() {
    let _guard = env_guarded();
    let scratch = std::env::temp_dir().join(format!(
        "sot-ws-destroy-terminal-proof-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let state_root = pin_local_state_root(&scratch);

    let (reg, id) = seed_default("capsule");
    // An epoch must begin before an observation about it is accepted,
    // so seed one before forcing the phase cell to Terminal.
    let ws = reg.resolve(Some(id.as_str())).expect("row just seeded");
    let identity = crate::rows::workspace::SupervisorIdentity { pid: 1, created: 1 };
    ws.begin_supervisor_epoch(identity);
    ws.apply_phase_observation(crate::rows::workspace::Observation::Phase {
        phase: crate::rows::workspace::Phase::Terminal,
        supervisor: identity,
        voyage: None,
    });
    seed_provably_unheld_state_dir(&state_root, &id);

    // `/p/local`/`"local"` are placeholders (`resume_locked`'s own
    // `Phase::Terminal` check returns before any agent argv is
    // resolved) -- only `state_root`'s on-disk fixture is real.
    let (outcome, held) = destroy_capsule_workspace(
        &id,
        "test reason",
        "none",
        "",
        "local",
        std::path::Path::new("/p/local"),
        &reg,
        true,
    )
    .await;
    // The guard IS taken now (Codex review: the deleted fast path's
    // `None` bypassed it) -- dropped once this proof has run.
    assert!(held.is_some(), "a terminal row must take the same row guard every other row does");
    match outcome {
        CapsuleDestroyOutcome::AlreadyRemoved => unreachable!("never an end_run mapping"),
        CapsuleDestroyOutcome::Removable(_) => {}
        CapsuleDestroyOutcome::Kept { detail } => {
            panic!(
                "a terminal row with both halves of the absence proof independently absent \
                 must be Removable: {detail}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&scratch);
}

// The full field defect this lane fixes: a default row carrying an
// agent from before the anchor rule, whose run is CONFIRMED ended,
// must have its `agent`/`agent_name` reset to the inert-anchor
// shape, that reset persisted to its toml, and the existing
// `run_ended` broadcast still fired -- all through the real
// `handle_workspace_destroy` wire path. Hermetic despite going
// through the full handler: `seed_provably_unheld_state_dir` (ADR
// 0043 decision 33's own absence proof, reproduced on disk -- the
// technique the test above also uses) makes the outcome
// deterministic with no live supervisor at all, and
// `XDG_CONFIG_HOME`/`XDG_STATE_HOME`/`SOT_SELF_HOST` are pinned to a
// scratch dir so neither the toml write nor the state dir ever
// touches a real `~/.config/sot` or `~/.local/state/sot`.
//
// The absence proof `seed_provably_unheld_state_dir` targets is
// `destroy_capsule_workspace`'s one, ungated path (macOS wiring
// lane), so this runs on every host.
#[tokio::test]
async fn default_row_confirmed_ended_resets_agent_persists_toml_and_broadcasts() {
    let _guard = env_guarded();
    let dir = std::env::temp_dir().join(format!(
        "sot-ws-destroy-default-reset-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    std::env::set_var("SOT_SELF_HOST", "reset-test-host");
    let state_root = pin_local_state_root(&dir.join("state"));

    let (reg, id, slug) = seed_default_with_agent("claude", "kal-local");
    assert!(
        !reg.is_inert_default_anchor(&reg.resolve(Some(&id)).unwrap()),
        "a default row carrying an agent is a real session, not the anchor, before the fix runs"
    );
    seed_provably_unheld_state_dir(&state_root, &id);

    let session = Session::new();
    let (tx, mut rx) = broadcast::channel(16);
    let payload = json!({ "workspace_id": id });
    let out = handle_workspace_destroy(1, payload, &session, &reg, &tx)
        .await
        .expect("handler must not error");
    let response = out[0].0.payload.clone();
    assert!(
        response.get("error").is_none(),
        "a confirmed end must not error: {response:?}"
    );
    assert!(
        response.get("kept").is_some(),
        "a confirmed end reports the success shape: {response:?}"
    );

    // The row: agent reset, inert again, id unchanged.
    let after = reg
        .resolve(Some(&id))
        .expect("the default row is never removed");
    assert_eq!(after.workspace_id, id);
    assert_eq!(after.agent(), "none");
    assert_eq!(after.agent_name(), "");
    assert!(
        reg.is_inert_default_anchor(&after),
        "with agent reset to none, the default row must be inert again"
    );

    // The broadcast: the existing `run_ended` WorkspaceChanged, unchanged.
    let evt = rx
        .try_recv()
        .expect("run_ended must still be broadcast on a confirmed end");
    assert_eq!(evt.action, "run_ended");
    assert_eq!(evt.workspace_id, id);
    assert_eq!(evt.slug, slug);

    // The toml: the reset was persisted, not just held in memory.
    let toml_path = crate::rows::store::toml_path_for(&slug);
    let contents = std::fs::read_to_string(&toml_path)
        .unwrap_or_else(|e| panic!("toml must be persisted at {toml_path:?}: {e}"));
    assert!(
        contents.contains("agent         = \"none\""),
        "agent must persist as none:\n{contents}"
    );
    assert!(
        contents.contains("agent_name    = \"\""),
        "agent_name must persist as empty:\n{contents}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ADR 0043 decision 35: the default row's end prunes its sot-comm
// registry row the same way `workspace.destroy`'s non-default path
// already does (`remove_comm_agents_for_workspace_host_tests` proves
// that path in isolation) -- before this lane, only the destroy path
// pruned, so a Windows default-row end left a ghost row that
// `workspace.list` merged back in. Two rows share the agent's handle
// string on the shared registry, one on this test's host and one on
// another, to prove the prune is host-scoped exactly like the
// destroy-path prune it mirrors.
//
// Same reason as the reset test above -- the confirmed-end outcome
// `seed_provably_unheld_state_dir` produces reaches `Removable`
// through `destroy_capsule_workspace`'s one, ungated path.
#[tokio::test]
async fn default_row_end_prunes_the_rows_registry_row() {
    let _guard = env_guarded();
    let stamp = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let config_dir =
        std::env::temp_dir().join(format!("sot-ws-destroy-default-leave-cfg-{stamp}"));
    let comm_dir =
        std::env::temp_dir().join(format!("sot-ws-destroy-default-leave-comm-{stamp}"));
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&comm_dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &config_dir);
    std::env::set_var("SOT_COMM_HOME", &comm_dir);
    std::env::set_var("SOT_SELF_HOST", "leave-test-host");
    let scratch_state =
        std::env::temp_dir().join(format!("sot-ws-destroy-default-leave-state-{stamp}"));
    let state_root = pin_local_state_root(&scratch_state);

    let handle = "default-row-leave-handle";
    std::fs::write(
        comm_dir.join("registry.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "agents": {
                handle: {"host": "leave-test-host"},
                "other-host-handle": {"host": "another-host"},
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let (reg, id, _slug) = seed_default_with_agent("claude", handle);
    seed_provably_unheld_state_dir(&state_root, &id);
    let payload = destroy(&reg, &id).await;
    assert!(
        payload.get("error").is_none(),
        "a confirmed end must not error: {payload:?}"
    );

    let after: serde_json::Value = serde_json::from_slice(
        &std::fs::read(comm_dir.join("registry.json")).unwrap(),
    )
    .unwrap();
    let agents = after.get("agents").unwrap().as_object().unwrap();
    assert!(
        !agents.contains_key(handle),
        "the default row's own handle must be pruned on a confirmed end: {agents:?}"
    );
    assert!(
        agents.contains_key("other-host-handle"),
        "another host's same-named-session row must survive: {agents:?}"
    );

    let _ = std::fs::remove_dir_all(&config_dir);
    let _ = std::fs::remove_dir_all(&comm_dir);
    let _ = std::fs::remove_dir_all(&scratch_state);
}
