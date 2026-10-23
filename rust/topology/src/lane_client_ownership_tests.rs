//! An ssh route's login: one dial starts one, a failed start is the dial's error, and none is left behind.
use super::*;
use std::sync::atomic::Ordering;

fn ssh_endpoint() -> DaemonLaneEndpoint {
    let recipe = crate::ssh_bridge::SshRecipe::new("teststub", None).unwrap();
    DaemonLaneEndpoint::new(LaneDial::Ssh(recipe, Default::default()), None)
}

struct PeerScript(std::path::PathBuf);
impl Drop for PeerScript {
    fn drop(&mut self) { std::fs::remove_dir_all(&self.0).expect("remove owned peer folder"); }
}

/// `failed_spawn` names the start that fails; `dying_spawn` the one whose peer reads the hello and exits unanswered.
fn counted_endpoint(failed_spawn: Option<usize>, dying_spawn: Option<usize>) -> (DaemonLaneEndpoint, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!("sot-spare-peer-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir(&path).unwrap();
    let script = std::sync::Arc::new(PeerScript(path));
    sot_platform::test_exec::write_executable(&script.0.join("peer.py"), r#"import json, os, sys
hello = json.loads(sys.stdin.readline())
if sys.argv[1:] == ['dies-in-handshake']:
    sys.exit(0)
request = json.loads(sys.stdin.readline())
assert hello['kind'] == 'req' and hello['op'] == 'hello'
assert hello['payload']['role'] == 'handoff'
assert request['kind'] == 'req' and request['op'] == 'lane.connect'
for frame, payload in [(hello, {'ok': True}), (request, {'ok': True, 'pid': os.getpid(), 'created': 1})]:
    print(json.dumps(dict(v=frame['v'], id=frame['id'], kind='res', op=frame['op'], payload=payload)), flush=True)
sys.stdin.buffer.read()
"#);
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = calls.clone();
    let ep = ssh_endpoint().with_test_ssh_spawner(std::sync::Arc::new(move |command| {
        let n = counted.fetch_add(1, Ordering::SeqCst);
        assert!(command.get_args().any(|arg| arg == "ControlMaster=no"), "the seam uses the gated argv authority");
        if failed_spawn == Some(n) {
            return Err(std::io::Error::other("injected spawn failure"));
        }
        std::process::Command::new("python3").arg("-u").arg(script.0.join("peer.py"))
            .args((dying_spawn == Some(n)).then_some("dies-in-handshake"))
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .spawn()
    }));
    (ep, calls)
}

fn ssh_step(ep: &DaemonLaneEndpoint) -> Result<DaemonLaneClient, TransportError> {
    ep.dial("row-owned-peer")
}

/// An ssh-route attach is one dial, so one login: nothing is parked for a second dial that no longer follows.
#[test]
fn a_remote_attach_uses_one_login() {
    let (ep, calls) = counted_endpoint(None, None);
    let client = ssh_step(&ep).expect("the one login answers");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "a remote attach starts exactly one login");
    drop(client);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "and no login is started behind it");
}

/// A login that cannot start is the dial's error, and the dial does not try again by itself.
#[test]
fn a_failed_login_is_the_dials_error_with_no_second_try() {
    let (ep, calls) = counted_endpoint(Some(0), None);
    match ssh_step(&ep) {
        Err(TransportError::Unreachable(error)) => assert_eq!(error.to_string(), "injected spawn failure"),
        other => panic!("expected the spawn failure, got {:?}", other.err()),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
