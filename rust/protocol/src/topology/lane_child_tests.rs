//! Observations of the real owned child: cancellation, teardown and reaping.
use super::*;
use std::time::{Duration, Instant};

#[cfg(unix)]
pub(crate) fn spawn_stub_child() -> std::process::Child {
    std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("`cat` must be on PATH for this test")
}

#[cfg(windows)]
pub(crate) fn spawn_stub_child() -> std::process::Child {
    // `more` with no filename argument reads stdin and copies it to
    // stdout, the same echo shape `cat` gives on Unix — no unix-only
    // tool required. It is `more.com`, and Command looks up only `.exe`
    // without an extension.
    std::process::Command::new("more.com")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("`more.com` must be on PATH for this test")
}

/// `BridgedClient::cancel()`'s own contract: the kill it issues must
/// be what unblocks a read already parked on the child's stdout, not
/// an eventual natural exit racing ahead of it — same property
/// `cancel_unblocks_a_pending_read` proves for the local-socket client above, one
/// mechanism for both.
#[test]
fn bridged_cancel_unblocks_a_parked_read_via_kill() {
    let client = std::sync::Arc::new(BridgedClient::wrap(spawn_stub_child()).expect("wrap"));
    let reader = std::sync::Arc::clone(&client);
    let read_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        let started = Instant::now();
        let result = reader.read(&mut buf);
        (result, started.elapsed())
    });
    // Give the read a moment to actually park before cancelling —
    // `cat`/`more` never write anything unprompted, so the read has
    // nothing to return until either bytes arrive or the child dies.
    std::thread::sleep(std::time::Duration::from_millis(100));
    client.cancel();
    let (result, elapsed) = read_thread.join().unwrap();
    assert!(elapsed < std::time::Duration::from_secs(2), "cancel() must unblock the read promptly, took {elapsed:?}");
    match result {
        Ok(0) | Err(TransportError::Cancelled) | Err(TransportError::Io { .. }) => {}
        other => panic!("expected cancel to unblock the read as EOF or an error, got {other:?}"),
    }
}


struct ChildFixture(std::path::PathBuf);
impl Drop for ChildFixture {
    fn drop(&mut self) { std::fs::remove_dir_all(&self.0).expect("remove owned child fixture"); }
}

fn controlled_child() -> (ChildFixture, BridgedClient) {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!("sot-child-owner-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir(&path).unwrap();
    let program = path.join("child.py");
    sot_log::test_exec::write_executable(&program, "import sys\nsys.stdin.readline()\n");
    let child = std::process::Command::new("python3").arg("-u").arg(program)
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .spawn().expect("spawn fixture-owned child");
    (ChildFixture(path), BridgedClient::wrap(child).unwrap())
}

fn rescue_child(client: &BridgedClient) {
    *client.faults.lock().unwrap() = ChildFaults::default();
    let mut child = client.child.lock().unwrap();
    if child.try_wait().unwrap().is_none() { child.kill().expect("terminate fixture-owned child"); }
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "fixture-owned child must be reaped");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[derive(Clone, Copy)]
struct OwnerClock { started: Instant, deadline: Instant }

pub(super) struct TeardownRendezvous {
    entry: std::sync::mpsc::Sender<OwnerClock>,
    armed: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    pub(super) completed: std::sync::mpsc::Sender<Instant>,
}

impl TeardownRendezvous {
    pub(super) fn enter(&self, started: Instant, deadline: Instant) {
        self.entry.send(OwnerClock { started, deadline }).unwrap();
        self.armed.lock().unwrap().recv_timeout(Duration::from_secs(5)).expect("owner entry arming watchdog");
    }
}

struct ReleaseMonitor {
    entry: std::sync::mpsc::Receiver<OwnerClock>,
    armed: std::sync::mpsc::Sender<()>,
    released: std::sync::mpsc::Sender<(OwnerClock, Instant, Instant)>,
}

impl ReleaseMonitor {
    fn await_release_time(&self) -> OwnerClock {
        let clock = self.entry.recv_timeout(Duration::from_secs(5)).expect("monitor owner-entry watchdog");
        let earliest = clock.deadline + Duration::from_millis(400);
        self.armed.send(()).unwrap();
        // Scheduling delays can only lengthen the hold past the owner's deadline.
        while Instant::now() < earliest {
            std::thread::sleep(earliest.saturating_duration_since(Instant::now()));
        }
        clock
    }
}

fn teardown_watch(client: &BridgedClient) -> (ReleaseMonitor, std::sync::mpsc::Receiver<(OwnerClock, Instant, Instant)>, std::sync::mpsc::Receiver<Instant>) {
    let (entry_tx, entry) = std::sync::mpsc::channel();
    let (armed, armed_rx) = std::sync::mpsc::channel();
    let (completed, completion) = std::sync::mpsc::channel();
    let (released, release) = std::sync::mpsc::channel();
    *client.teardown_observer.lock().unwrap() = Some(TeardownRendezvous { entry: entry_tx, armed: std::sync::Mutex::new(armed_rx), completed });
    (ReleaseMonitor { entry, armed, released }, release, completion)
}

fn finish_deadline_witness(client: &BridgedClient, result: std::io::Result<()>, monitor: std::thread::JoinHandle<()>, release: std::sync::mpsc::Receiver<(OwnerClock, Instant, Instant)>, completion: std::sync::mpsc::Receiver<Instant>, elapsed_assertion: &str, required_error: &str) {
    let completed = completion.recv_timeout(Duration::from_secs(5)).expect("owner completion watchdog");
    let (clock, held, released) = release.recv_timeout(Duration::from_secs(5)).expect("fixture release watchdog");
    let join_deadline = Instant::now() + Duration::from_secs(5);
    while !monitor.is_finished() {
        assert!(Instant::now() < join_deadline, "fixture monitor join watchdog");
        std::thread::sleep(Duration::from_millis(5));
    }
    monitor.join().unwrap();
    rescue_child(client);
    let elapsed = completed.duration_since(clock.started);
    println!("teardown witness {required_error}: owned child {}; hold before entry {:?}; owner deadline {:?}; owner completion {:?}; actual release {:?}; result {result:?}; child reaped and monitor joined", client.id, clock.started.saturating_duration_since(held), clock.deadline.duration_since(clock.started), elapsed, released.duration_since(clock.started));
    // Cleanup precedes every verdict; the elapsed failure is distinct from diagnostics.
    assert!(elapsed < Duration::from_millis(2250), "{elapsed_assertion}: {elapsed:?}");
    assert!(result.as_ref().is_err_and(|error| error.to_string().contains(required_error)), "the owner must report its relevant deadline error: {result:?}");
    assert_eq!(clock.deadline.duration_since(clock.started), Duration::from_secs(2));
    assert!(held <= clock.started, "the actual fixture hold precedes teardown entry");
    assert!(released >= clock.deadline + Duration::from_millis(400), "release cannot precede the owner deadline plus 400 ms");
    assert!(completed < released, "owner completion must precede fixture release");
}

#[test]
fn lane_child_teardown_finishes_within_its_bound() {
    let (_fixture, client) = controlled_child();
    client.faults.lock().unwrap().hold_termination = true;
    assert!(client.child.lock().unwrap().try_wait().unwrap().is_none(), "the recorded child is held alive");
    let held = Instant::now();
    let (watch, release, completion) = teardown_watch(&client);
    let mut input = client.inp.try_clone().unwrap();
    let monitor = std::thread::spawn(move || {
        use std::io::Write;
        let clock = watch.await_release_time();
        input.write_all(b"exit\n").expect("release only the recorded child's input");
        watch.released.send((clock, held, Instant::now())).unwrap();
    });
    let result = client.teardown();
    finish_deadline_witness(&client, result, monitor, release, completion, "owned child teardown exceeded its 2 s budget", "2 s teardown budget expired");
}

#[test]
fn lane_child_teardown_reports_termination_reap_and_timeout_errors() {
    for operation in ["terminate", "reap", "deadline"] {
        let (_fixture, client) = controlled_child();
        {
            let mut faults = client.faults.lock().unwrap();
            faults.fail_terminate = operation == "terminate";
            faults.fail_reap = operation == "reap";
            faults.hold_termination = operation == "deadline";
        }
        let result = if operation == "terminate" { client.cancel(); None } else { Some(client.teardown()) };
        let diagnostics = client.diagnostics.lock().unwrap().clone();
        let id = client.id;
        rescue_child(&client);
        assert!(diagnostics.iter().any(|line| line.contains("lane child teardown failed") && line.contains(operation) && line.contains(&id.to_string())), "owned child {operation} failure must reach the actual reporting sink");
        if let Some(result) = result { assert!(result.is_err(), "explicit teardown retains its error"); }
    }
}

#[test]
fn normal_lane_child_teardown_confirms_reaping() {
    let (_fixture, client) = controlled_child();
    let result = client.teardown();
    let observed = client.child.lock().unwrap().try_wait().unwrap().is_some();
    rescue_child(&client);
    assert!(result.is_ok() && observed, "successful teardown must terminate and reap its owned child");
}

#[test]
fn an_exited_child_and_repeated_cancel_are_successful() {
    use std::io::Write;
    let (_fixture, client) = controlled_child();
    (&client.inp).write_all(b"exit\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !client.exited() {
        assert!(Instant::now() < deadline, "fixture-owned child exit must be observed");
        std::thread::sleep(Duration::from_millis(5));
    }
    client.cancel();
    client.cancel();
    let result = client.teardown();
    let diagnostics = client.diagnostics.lock().unwrap().clone();
    rescue_child(&client);
    assert!(result.is_ok() && diagnostics.is_empty(), "confirmed exit and repeated cancel are success");
}

#[test]
fn the_teardown_budget_includes_child_lock_acquisition() {
    let (_fixture, client) = controlled_child();
    let client = std::sync::Arc::new(client);
    let holder = client.clone();
    let (ready_tx, ready) = std::sync::mpsc::channel();
    let (watch, release, completion) = teardown_watch(&client);
    let monitor = std::thread::spawn(move || {
        let guard = holder.child.lock().unwrap();
        let held = Instant::now();
        ready_tx.send(()).unwrap();
        let clock = watch.await_release_time();
        drop(guard);
        watch.released.send((clock, held, Instant::now())).unwrap();
    });
    ready.recv_timeout(Duration::from_secs(5)).expect("holder rendezvous watchdog");
    let result = client.teardown();
    finish_deadline_witness(&client, result, monitor, release, completion, "child lock acquisition exceeded its 2 s teardown budget", "child lock acquisition expired");
}
