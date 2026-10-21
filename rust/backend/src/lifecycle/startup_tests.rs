use super::*;
use std::ffi::OsString;

use sot_log::identity::challenge::PeerAuthenticated;
use sot_protocol::ops::{FeLeaseReq, LeaseOutcome};

use crate::lifecycle::lease::HeldRecord;

/// The config env this test points at a tempdir, put back on drop.
struct EnvBack(Vec<(&'static str, Option<OsString>)>);

impl Drop for EnvBack {
    fn drop(&mut self) {
        for (key, val) in &self.0 {
            match val {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[tokio::test]
async fn failed_forget_removal_is_kept_for_the_next_start() {
    let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _back = EnvBack(["XDG_CONFIG_HOME", "SOT_SELF_HOST"].map(|k| (k, std::env::var_os(k))).to_vec());
    let config = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_CONFIG_HOME", config.path());
    std::env::set_var("SOT_SELF_HOST", "forget-test");

    let workspaces = Workspaces::new();
    let row = |label: &str| {
        let root = PathBuf::from("/p").join(label);
        workspaces.insert(Workspace::from_label(label, root, false, "none".into(), String::new(), String::new()))
    };
    let stuck = row("stuck");
    let gone = row("gone");
    // A directory where the registration file goes: it will not go.
    std::fs::create_dir_all(crate::rows::store::toml_path_for(&stuck.slug)).unwrap();

    let own = sot_log::identity::challenge::boot_identity().expect("this host's boot");
    let state = tempfile::tempdir().unwrap();
    let path = state.path().join(bounds::HELD_RECORD_FILE);
    let rec = HeldRecord {
        v: 1,
        boot: own.clone(),
        holders: vec![],
        handover_until_ms: None,
        closing: false,
        not_ended: 0,
        forget: vec![stuck.workspace_id.clone(), gone.workspace_id.clone()],
    };
    lease::write_or_delete(&path, &rec).unwrap();

    let (ws_events, _rx) = broadcast::channel(4);
    let leases = begin(Some(state.path().to_path_buf()), &workspaces, &ws_events);
    let req = FeLeaseReq { boot: own, pid: 1, created: 7001, token: None };
    let peer = PeerAuthenticated { pid: 1, created: 7001 };
    assert_eq!(leases.grant(&req, Some(&peer)).0, LeaseOutcome::Granted);
    assert_eq!(
        lease::read_record(&path).unwrap().map(|r| r.forget),
        Some(vec![stuck.workspace_id.clone()]),
        "a forgotten row whose registration would not go was dropped from the record"
    );
}
