//! Shared fixtures for the reauth tests: the env guard and the seeded homes and rows.

use super::*;
use crate::agents::accounts::claude_config_dir;
use crate::rows::Workspace;

/// Restores every variable these tests pin, under the crate-wide
/// serialization every env-mutating test module here shares
/// (`paths::ENV_TEST_LOCK`) — `HOME`/`USERPROFILE` because
/// `accounts::account_home` reads them, `XDG_CONFIG_HOME` because
/// `rows::store::save` writes under it, and `XDG_STATE_HOME`/
/// `LOCALAPPDATA` because the accept path resolves this machine's
/// state root.
pub(super) struct EnvGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
    home: Option<std::ffi::OsString>,
    userprofile: Option<std::ffi::OsString>,
    xdg_config_home: Option<std::ffi::OsString>,
    xdg_state_home: Option<std::ffi::OsString>,
    localappdata: Option<std::ffi::OsString>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, val) in [
            ("HOME", &self.home),
            ("USERPROFILE", &self.userprofile),
            ("XDG_CONFIG_HOME", &self.xdg_config_home),
            ("XDG_STATE_HOME", &self.xdg_state_home),
            ("LOCALAPPDATA", &self.localappdata),
        ] {
            match val {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

pub(super) fn env_guarded() -> EnvGuard {
    let serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    EnvGuard {
        _serial: serial,
        home: std::env::var_os("HOME"),
        userprofile: std::env::var_os("USERPROFILE"),
        xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
        xdg_state_home: std::env::var_os("XDG_STATE_HOME"),
        localappdata: std::env::var_os("LOCALAPPDATA"),
    }
}

/// A home with a default `.claude` folder and one `.claude-auth`
/// subdirectory per named account — `logged_in` decides whether its
/// `.credentials.json` exists, which is the whole difference between
/// an account this op accepts and one it refuses.
pub(super) fn home_with(default_logged_in: bool, named: &[(&str, bool)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let claude = dir.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    if default_logged_in {
        std::fs::write(claude.join(".credentials.json"), b"{}").unwrap();
    }
    for (name, logged_in) in named {
        let acct = dir.path().join(".claude-auth").join(name);
        std::fs::create_dir_all(&acct).unwrap();
        if *logged_in {
            std::fs::write(acct.join(".credentials.json"), b"{}").unwrap();
        }
    }
    dir
}

/// `claude_resume_argv` resolves a real binary on Unix, so the accept
/// path needs one under the pinned home; the Windows arm resolves
/// nothing and needs no fixture.
#[cfg(unix)]
pub(super) fn seed_claude_binary(home: &Path) {
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    sot_log::test_exec::write_executable(&claude, b"#!/bin/sh\nexit 0\n");
}
#[cfg(windows)]
pub(super) fn seed_claude_binary(_home: &Path) {}

/// One transcript the account owning `config_dir` can open:
/// `projects/<project>/<id>.jsonl`, the shape `check` globs for. In the
/// real tree a named account reaches the very same file through the
/// shared `projects` symlink (`accounts::SHARED_ENTRIES`); these tests
/// seed the folder being asked about directly, which is what the glob
/// resolves to either way.
pub(super) fn seed_transcript(config_dir: &Path, id: &str) {
    let project = config_dir.join("projects").join("-a-project-root");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join(format!("{id}.jsonl")), b"{}\n").unwrap();
}

/// Pins HOME, the config root and the state root at scratch dirs and
/// returns the home. Caller holds the `EnvGuard` FIRST.
pub(super) fn pin_home(home: &Path, scratch: &Path) {
    std::env::set_var("HOME", home);
    std::env::set_var("USERPROFILE", home);
    std::env::set_var("XDG_CONFIG_HOME", scratch);
    std::env::set_var("XDG_STATE_HOME", scratch);
    std::env::set_var("LOCALAPPDATA", scratch);
}

pub(super) fn seed_capsule_row(account: &str, handle: &str) -> (Workspaces, String, String) {
    let reg = Workspaces::new();
    let mut ws = Workspace::from_label(
        "reauth-row",
        std::path::PathBuf::from("/p/reauth-row"),
        true,
        "claude".into(),
        "row-agent".into(),
        String::new(),
    );
    ws.runtime = "capsule".to_string();
    ws.account = std::sync::Mutex::new(account.to_string());
    let id = ws.workspace_id.clone();
    let slug = ws.slug.clone();
    reg.insert(ws);
    reg.set_agent_handle(&id, handle);
    (reg, id, slug)
}

pub(super) async fn reauth_out(reg: &Workspaces, id: &str, account: &str, resume: &str) -> (HandlerOutput, Option<ReauthRestart>) {
    handle_workspace_reauth(
        1,
        serde_json::json!({"workspace_id": id, "account": account, "resume": resume}),
        reg,
    )
    .await
    .expect("the handler itself never errors; it refuses")
}

pub(super) async fn reauth(reg: &Workspaces, id: &str, account: &str, resume: &str) -> (serde_json::Value, Option<ReauthRestart>) {
    let (out, restart) = reauth_out(reg, id, account, resume).await;
    (out[0].0.payload.clone(), restart)
}

/// The accept path with a transcript the target can open and a row on
/// the default login — the fixture both ordering tests start from.
/// Returns the registry, the row's id and slug.
pub(super) fn accept_fixture(home: &Path, scratch: &Path) -> (Workspaces, String, String) {
    pin_home(home, scratch);
    seed_claude_binary(home);
    seed_transcript(&claude_config_dir(home, "team"), "sid-7");
    seed_capsule_row("", "row-declared-handle")
}
