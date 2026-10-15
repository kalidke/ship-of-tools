//! Actual endpoint fixtures and spare ownership, consumption and retry.
use super::*;
use std::io::Read;
use std::sync::atomic::Ordering;
use std::time::Duration;
use super::lane_child::tests::spawn_stub_child;

fn spare_endpoint() -> DaemonLaneEndpoint {
    let recipe = crate::topology::ssh_bridge::SshRecipe::new("teststub", None).unwrap();
    DaemonLaneEndpoint::new(LaneDial::Ssh(recipe, Default::default()), None)
}

#[test]
fn a_first_voyage_dial_takes_the_spare_the_supervisor_dial_started() {
    let ep = spare_endpoint();
    ep.start_spare(|| BridgedClient::wrap(spawn_stub_child()).map_err(TransportError::Unreachable));
    assert!(matches!(*ep.spare.lock().unwrap(), VoyageSpare::Parked(..)), "the supervisor must park its voyage login");
    let (spare, _) = ep.take_spare().expect("the first voyage consumes the live spare");
    assert!(!spare.exited());
    assert!(matches!(*ep.spare.lock().unwrap(), VoyageSpare::Spent));
    ep.start_spare(|| panic!("a spent endpoint must start no spare"));
    assert!(ep.take_spare().is_none());
}

#[test]
fn a_second_supervisor_dial_starts_no_second_spare() {
    let ep = spare_endpoint();
    let mut calls = 0;
    for _ in 0..2 {
        ep.start_spare(|| {
            calls += 1;
            BridgedClient::wrap(spawn_stub_child()).map_err(TransportError::Unreachable)
        });
    }
    assert_eq!(calls, 1, "a parked endpoint starts just one spare");
}

#[test]
fn a_dead_spare_is_dropped_not_used() {
    let ep = spare_endpoint();
    ep.start_spare(|| BridgedClient::wrap(spawn_stub_child()).map_err(TransportError::Unreachable));
    if let VoyageSpare::Parked(c, _) = &*ep.spare.lock().unwrap() {
        c.cancel();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !c.exited() {
            assert!(Instant::now() < deadline, "the owned spare child never exited");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(c.child.lock().unwrap().try_wait().unwrap().is_some(), "the owned child is reaped before fixture teardown");
    } else {
        panic!("spare not parked");
    }
    assert!(ep.take_spare().is_none());
    assert!(matches!(*ep.spare.lock().unwrap(), VoyageSpare::Spent));
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
    sot_log::test_exec::write_executable(&script.0.join("peer.py"), r#"import json, os, sys
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
    let ep = spare_endpoint().with_test_ssh_spawner(std::sync::Arc::new(move |command| {
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

fn ssh_step(ep: &DaemonLaneEndpoint, kind: &str) -> Result<DaemonLaneClient, TransportError> {
    ep.dial("row-owned-peer", kind, (kind == "voyage").then(|| "voyage".to_string()))
}

fn parked_pid(ep: &DaemonLaneEndpoint) -> u32 {
    match &*ep.spare.lock().unwrap() {
        VoyageSpare::Parked(child, _) => child.child.lock().unwrap().id(),
        _ => panic!("the supervisor must park a spare"),
    }
}

/// The owned child `pid` has been reaped: waitpid finds no child by that pid (ECHILD), so no waitable zombie remains.
#[cfg(unix)]
fn assert_reaped(pid: u32, why: &str) {
    unsafe extern "C" { fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32; }
    assert_eq!(unsafe { waitpid(pid as i32, std::ptr::null_mut(), 1) }, -1, "{why}");
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(10), "{why}: no child remains to reap");
}

fn child_is_live(client: &DaemonLaneClient) -> bool {
    let LaneStream::Bridged(child) = &client.stream else { panic!("test peer must be bridged") };
    !child.exited()
}

#[test]
fn ssh_spare_spawn_and_consume_counts() {
    for (failed_spawn, counts) in [(None, [2, 3, 3, 4, 5]), (Some(1), [2, 4, 4, 5, 6])] {
        let (ep, calls) = counted_endpoint(failed_spawn, None);
        for (kind, expected) in ["supervisor", "supervisor", "voyage", "supervisor", "voyage"].into_iter().zip(counts) {
            let parked_pid = match &*ep.spare.lock().unwrap() {
                VoyageSpare::Parked(child, _) => Some(child.child.lock().unwrap().id()),
                _ => None,
            };
            let client = ssh_step(&ep, kind).expect("ordinary login succeeds");
            if kind == "voyage" && parked_pid.is_some() {
                assert_eq!(Some(client.peer.pid), parked_pid, "the first voyage uses the parked child identity");
            }
            assert!(child_is_live(&client), "the selected owned child is live");
            assert_eq!(calls.load(Ordering::SeqCst), expected, "spawn count after {kind}");
            drop(client);
        }
    }
    let (ep, calls) = counted_endpoint(None, None);
    drop(ssh_step(&ep, "voyage").unwrap());
    drop(ssh_step(&ep, "supervisor").unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 2, "first voyage without a spare spends the endpoint");
    let (ep, calls) = counted_endpoint(Some(1), None);
    drop(ssh_step(&ep, "supervisor").unwrap());
    drop(ssh_step(&ep, "voyage").unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 3, "a failed spare spawn falls back to one ordinary voyage login");
}

#[test]
fn a_dead_spare_uses_one_fresh_voyage_login() {
    let (ep, calls) = counted_endpoint(None, None);
    drop(ssh_step(&ep, "supervisor").unwrap());
    if let VoyageSpare::Parked(client, _) = &*ep.spare.lock().unwrap() {
        client.cancel();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !client.exited() {
            assert!(Instant::now() < deadline, "the owned spare child never exited");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(client.child.lock().unwrap().try_wait().unwrap().is_some(), "the dead spare is reaped before fallback");
    } else {
        panic!("the supervisor must park a spare");
    }
    let fresh = ssh_step(&ep, "voyage").unwrap();
    assert!(child_is_live(&fresh), "the fallback voyage login is live");
    assert_eq!(calls.load(Ordering::SeqCst), 3, "the dead spare needs one ordinary fresh voyage login");
    assert!(matches!(*ep.spare.lock().unwrap(), VoyageSpare::Spent));
}

#[test]
fn a_spare_that_dies_in_its_handshake_uses_one_fresh_voyage_login() {
    let (ep, calls) = counted_endpoint(None, Some(1));
    drop(ssh_step(&ep, "supervisor").unwrap());
    let spare = parked_pid(&ep);
    let voyage = ssh_step(&ep, "voyage");
    assert_eq!(calls.load(Ordering::SeqCst), 3, "a spare that dies after the check needs one ordinary fresh voyage login: {:?}", voyage.as_ref().err());
    #[cfg(unix)]
    assert_reaped(spare, "the failed spare is reaped before the voyage returns");
    #[cfg(windows)]
    let _ = spare;
    let fresh = voyage.expect("the ordinary voyage login answers");
    assert!(child_is_live(&fresh), "the fallback voyage login is live");
    assert!(matches!(*ep.spare.lock().unwrap(), VoyageSpare::Spent));
}

#[test]
fn a_spare_and_a_fresh_login_that_both_fail_return_the_fresh_error() {
    let (ep, calls) = counted_endpoint(Some(2), Some(1));
    drop(ssh_step(&ep, "supervisor").unwrap());
    let spare = parked_pid(&ep);
    let voyage = ssh_step(&ep, "voyage");
    assert_eq!(calls.load(Ordering::SeqCst), 3, "one fresh login follows the failed spare, and no other: {:?}", voyage.as_ref().err());
    match voyage {
        Err(TransportError::Unreachable(error)) => assert_eq!(error.to_string(), "injected spawn failure", "the voyage's error is the fresh login's, not the spare's"),
        other => panic!("the voyage's error is the fresh login's, got {:?}", other.err()),
    }
    #[cfg(unix)]
    assert_reaped(spare, "the failed spare is reaped although the fresh login failed too");
    #[cfg(windows)]
    let _ = spare;
}

#[test]
fn a_down_gate_never_invokes_the_test_spawner() {
    let (ep, calls) = counted_endpoint(None, None);
    let LaneDial::Ssh(_, gate) = &ep.dial else { unreachable!() };
    gate.set_up(false);
    assert!(matches!(ssh_step(&ep, "supervisor"), Err(TransportError::LinkDown)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    gate.set_up(true);
    drop(ssh_step(&ep, "supervisor").unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    gate.set_up(false);
    assert!(matches!(ssh_step(&ep, "voyage"), Err(TransportError::LinkDown)));
    assert_eq!(calls.load(Ordering::SeqCst), 2, "a down voyage drops its spare without another spawn");
    assert!(matches!(*ep.spare.lock().unwrap(), VoyageSpare::Spent));
}

#[test]
fn endpoint_destruction_reaps_the_parked_spare() {
    let (ep, _) = counted_endpoint(None, None);
    drop(ssh_step(&ep, "supervisor").unwrap());
    #[cfg(windows)]
    let process = match &*ep.spare.lock().unwrap() {
        VoyageSpare::Parked(c, _) => {
            use std::os::windows::io::AsHandle;
            c.child.lock().unwrap().as_handle().try_clone_to_owned().unwrap()
        }
        _ => panic!("no parked spare"),
    };
    let (mut output, pid) = match &*ep.spare.lock().unwrap() {
        VoyageSpare::Parked(c, _) => {
            assert!(c.child.lock().unwrap().try_wait().unwrap().is_none(), "the owned spare is alive before destruction");
            (c.out.try_clone().unwrap(), c.child.lock().unwrap().id())
        }
        _ => panic!("no parked spare"),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || { let mut byte = [0]; tx.send(output.read(&mut byte)).unwrap(); });
    let teardown_started = Instant::now();
    drop(ep);
    assert!(teardown_started.elapsed() < Duration::from_secs(2), "endpoint destruction stays within child teardown bound");
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).expect("owned child stdout must close").unwrap(), 0);
    reader.join().unwrap();
    #[cfg(unix)]
    assert_reaped(pid, "the owned child must already be reaped");
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        unsafe extern "system" { fn WaitForSingleObject(handle: *mut std::ffi::c_void, millis: u32) -> u32; }
        assert_eq!(unsafe { WaitForSingleObject(process.as_raw_handle(), 0) }, 0, "the owned child has exited before teardown");
        let _ = pid;
    }
}

#[test]
fn abandonment_rearms_only_before_the_first_voyage() {
    let (ep, calls) = counted_endpoint(None, None);
    drop(ssh_step(&ep, "supervisor").unwrap());
    let (mut output, pid) = match &*ep.spare.lock().unwrap() {
        VoyageSpare::Parked(child, _) => (child.out.try_clone().unwrap(), child.child.lock().unwrap().id()),
        _ => panic!("the initial supervisor parks its spare"),
    };
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let teardown_started = Instant::now();
    ep.drop_spare();
    assert!(teardown_started.elapsed() < Duration::from_secs(2), "abandonment stays within child teardown bound");
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || { let _ = tx.send(output.read(&mut [0])); });
    assert_eq!(rx.recv_timeout(Duration::from_secs(2)).expect("abandonment closes the owned spare").unwrap(), 0);
    reader.join().unwrap();
    #[cfg(unix)]
    assert_reaped(pid, "abandonment reaps its spare before retry");
    #[cfg(windows)]
    let _ = pid;
    drop(ssh_step(&ep, "supervisor").unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 4, "a pre-voyage retry starts a replacement pair");
    drop(ssh_step(&ep, "voyage").unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 4, "first voyage consumes the replacement without a fifth spawn");
    ep.drop_spare();
    for kind in ["supervisor", "voyage"] { drop(ssh_step(&ep, kind).unwrap()); }
    assert_eq!(calls.load(Ordering::SeqCst), 6, "cleanup after Spent never rearms a spare");
}

/// A spare parked before its host's link closed is never used after the link returns: on the dead connection its
/// handshake would hold the first voyage until `SSH_LOGIN_BOUND`. That voyage makes one fresh login instead, and the
/// spare is reaped by the time it returns.
#[test]
fn a_spare_parked_before_a_link_close_is_not_used_after_it() {
    let (ep, calls) = counted_endpoint(None, None);
    let LaneDial::Ssh(_, gate) = &ep.dial else { unreachable!() };
    drop(ssh_step(&ep, "supervisor").unwrap());
    let spare = parked_pid(&ep);
    gate.set_up(false);
    gate.set_up(true);
    let voyage = ssh_step(&ep, "voyage").expect("one fresh voyage login answers");
    assert_ne!(voyage.peer.pid, spare, "the first voyage used the spare parked before the link closed");
    assert_eq!(calls.load(Ordering::SeqCst), 3, "the first voyage makes one fresh login");
    #[cfg(unix)]
    assert_reaped(spare, "the spare parked before the link closed is reaped before the voyage returns");
    assert!(matches!(*ep.spare.lock().unwrap(), VoyageSpare::Spent));
}

/// The host's link closes while the first voyage's handshake with the parked spare is in flight (the spare is spent,
/// answers nothing): the dial ends `LinkDown` at once, not after `SSH_LOGIN_BOUND` on a dead connection.
#[test]
fn a_spare_handshake_in_flight_ends_when_its_link_closes() {
    let ep = std::sync::Arc::new(spare_endpoint());
    let LaneDial::Ssh(_, gate) = &ep.dial else { unreachable!() };
    let gate = gate.clone();
    ep.start_spare(|| BridgedClient::wrap(super::tests::silent_login().expect("silent login")).map_err(TransportError::Unreachable));
    assert!(matches!(*ep.spare.lock().unwrap(), VoyageSpare::Parked(..)), "the supervisor must park its voyage login");
    let (sent, ended) = std::sync::mpsc::channel();
    let dial = {
        let ep = ep.clone();
        std::thread::spawn(move || {
            let result = ep.dial("row-1", "voyage", Some("v".to_string())).map(|_| ());
            sent.send((result, std::time::Instant::now())).unwrap();
        })
    };
    let spent = std::time::Instant::now();
    while !matches!(*ep.spare.lock().unwrap(), VoyageSpare::Spent) {
        assert!(spent.elapsed() < Duration::from_secs(30), "the first voyage never took the spare");
        std::thread::sleep(Duration::from_millis(10));
    }
    let closed = std::time::Instant::now();
    gate.set_up(false);
    std::thread::sleep(Duration::from_secs(1));
    gate.set_up(true);
    let (result, at) = ended.recv_timeout(crate::topology::ssh_bridge::SSH_LOGIN_BOUND + Duration::from_secs(30)).expect("the dial ended");
    dial.join().unwrap();
    let after = at.duration_since(closed);
    println!("spare handshake in flight at a link close: result={result:?} after_close_ms={}", after.as_millis());
    assert!(
        matches!(result, Err(TransportError::LinkDown)) && after < Duration::from_secs(5),
        "the spare's handshake outlived its link: {result:?}, {after:?} after the close"
    );
}
