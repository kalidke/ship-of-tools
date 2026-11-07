//! The hold tool, in one process. Each test names its own point: an arm is exclusive per point, and the tests run
//! beside each other. Every wait here is a condition with a stated bound that fails with what it saw.

use super::*;
use crate::hold::{arrive, waiting_arms};
use std::fmt::Display;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const SEEN: Duration = Duration::from_secs(10);

/// An arrival on its own thread; the receiver hears when it has gone on.
fn arrives(point: &'static str, key: Option<&str>) -> mpsc::Receiver<()> {
    let key = key.map(str::to_string);
    let (done, gone) = mpsc::channel();
    std::thread::spawn(move || {
        arrive(point, key.as_ref().map(|key| key as &dyn Display));
        done.send(()).ok();
    });
    gone
}

fn goes_on(gone: &mpsc::Receiver<()>, what: &str) {
    gone.recv_timeout(SEEN).unwrap_or_else(|_| panic!("{what}: the arrival did not go on within {SEEN:?}"));
}

fn panic_text(result: std::thread::Result<()>) -> String {
    let payload = result.expect_err("expected a panic");
    payload.downcast_ref::<String>().cloned().or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()
}

#[test]
fn a_held_arrival_waits_at_its_line_until_released() {
    let arm = Holds::here().arm("t.waits").hold(1);
    let gone = arrives("t.waits", None);
    arm.reached(0);
    assert!(matches!(gone.try_recv(), Err(mpsc::TryRecvError::Empty)));
    arm.release(0);
    goes_on(&gone, "after release");
}

#[test]
fn arrivals_take_tickets_in_order_and_only_the_held_ones_wait() {
    let arm = Holds::here().arm("t.tickets").hold(2);
    let first = arrives("t.tickets", None);
    arm.reached(0);
    let second = arrives("t.tickets", None);
    arm.reached(1);
    goes_on(&arrives("t.tickets", None), "the third, past the hold count");
    assert_eq!(arm.arrivals(), 3);
    assert!(panic_text(catch_unwind(AssertUnwindSafe(|| arm.release(2)))).contains("Passed"), "a passed ticket is not held");
    arm.release(1);
    goes_on(&second, "ticket 1");
    arm.release(0);
    goes_on(&first, "ticket 0");
}

#[test]
fn an_unarmed_point_holds_and_counts_nothing() {
    let started = std::time::Instant::now();
    crate::hold_point!("t.unarmed");
    crate::hold_point!("t.unarmed", "key");
    assert!(started.elapsed() < WAIT, "an unarmed point must not wait");
    let arm = Holds::here().arm("t.unarmed").hold(0);
    assert_eq!(arm.arrivals(), 0, "arrivals before the arm are not seen");
}

#[test]
fn a_keyed_arm_sees_only_its_key() {
    let arm = Holds::here().arm("t.keyed").key("mine").hold(1);
    goes_on(&arrives("t.keyed", Some("theirs")), "another key");
    goes_on(&arrives("t.keyed", None), "no key");
    assert_eq!(arm.arrivals(), 0);
    let held = arrives("t.keyed", Some("mine"));
    arm.reached(0);
    arm.release(0);
    goes_on(&held, "the matching key");
    assert_eq!(arm.arrivals(), 1);
}

#[test]
fn reached_fails_by_name_within_its_bound() {
    let arm = Holds::here().arm("t.never").hold(1);
    let started = std::time::Instant::now();
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| arm.reached_within(0, Duration::from_millis(200)))));
    assert!(text.contains("t.never") && text.contains("arrival 0") && text.contains("0 arrivals seen"), "{text}");
    assert!(text.contains("reached_async"), "the failure names its usual fix: {text}");
    assert!(started.elapsed() < SEEN);
}

#[test]
fn release_before_the_arrival_is_refused() {
    let arm = Holds::here().arm("t.early").hold(1);
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| arm.release(0))));
    assert!(text.contains("has not arrived"), "{text}");
}

#[test]
fn dropping_an_arm_releases_every_held_arrival() {
    let arm = Holds::here().arm("t.drop").hold(2);
    let first = arrives("t.drop", None);
    let second = arrives("t.drop", None);
    arm.reached(1);
    drop(arm);
    goes_on(&first, "ticket 0 after the drop");
    goes_on(&second, "ticket 1 after the drop");
    goes_on(&arrives("t.drop", None), "an arrival after the drop");
}

/// The test panics while its arm has a give-up: the arm's drop prints its failure (it may not panic a second time),
/// which only a child's stderr shows, so the test reruns itself as a child and the parent reads that stderr.
#[test]
fn a_panicking_test_still_prints_the_arms_failure_and_does_not_abort() {
    use crate::test_isolated::{drain, enter, test_command, ISOLATION_TIMEOUT};
    const NAME: &str = "test_hold::tests::a_panicking_test_still_prints_the_arms_failure_and_does_not_abort";
    const CHILD: &str = "SOT_HOLD_TEST_PRINT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        enter(NAME);
        let result = catch_unwind(|| {
            let _arm = Holds::here().arm("t.print").give_up_after(Duration::from_millis(100)).hold(1);
            goes_on(&arrives("t.print", None), "after its give-up");
            panic!("the test's own failure");
        });
        assert_eq!(panic_text(result), "the test's own failure");
        return;
    }
    let (mut command, entry) = test_command(NAME);
    command.env(CHILD, "1").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let child = command.spawn().expect("spawn the child");
    let pid = child.id();
    let (status, _out, err) = drain(child).wait_within(ISOLATION_TIMEOUT);
    entry.assert_once(pid);
    assert!(status.success(), "the child did not end cleanly (an abort is a second panic): {status}\n{err}");
    assert_eq!(err.matches("panicked at").count(), 1, "one panic, the test's own: {err}");
    assert!(err.contains("t.print") && err.contains("arrival 0") && err.contains("hold_point_async!"), "{err}");
}

#[test]
fn a_second_arm_of_a_point_waits_for_the_first() {
    let first = Holds::here().arm("t.second").hold(1);
    let installed = Arc::new(AtomicBool::new(false));
    let flag = installed.clone();
    let (made, second) = mpsc::channel();
    std::thread::spawn(move || {
        let arm = Holds::here().arm("t.second").hold(0);
        flag.store(true, Ordering::SeqCst);
        made.send(arm).ok();
    });
    let queued = std::time::Instant::now();
    while waiting_arms() == 0 {
        assert!(queued.elapsed() < SEEN, "the second arm never queued behind the first");
        std::thread::yield_now();
    }
    let held = arrives("t.second", None);
    first.reached(0);
    assert!(!installed.load(Ordering::SeqCst), "the second arm was installed while the first lived");
    drop(first);
    goes_on(&held, "released by the first arm's drop");
    let _second = second.recv_timeout(SEEN).expect("the second arm after the first's drop");
}

#[tokio::test(flavor = "current_thread")]
async fn an_async_point_never_blocks_its_runtime_thread() {
    let arm = Holds::here().arm("t.async.thread").hold(1);
    let task = tokio::spawn(async {
        crate::hold_point_async!("t.async.thread");
    });
    arm.reached_async(0).await;
    assert!(!task.is_finished());
    arm.release(0);
    task.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_async_point_holds_under_paused_time() {
    let arm = Holds::here().arm("t.async.paused").give_up_after(Duration::from_millis(300)).hold(1);
    let task = tokio::spawn(async {
        crate::hold_point_async!("t.async.paused", "key");
    });
    // On paused time this timer fires the moment the runtime goes idle; it must not while the arrival is held.
    let ended = tokio::time::timeout(Duration::from_secs(3600), task).await;
    assert!(ended.is_ok(), "paused time advanced past a held async arrival");
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| drop(arm))));
    assert!(text.contains("t.async.paused"), "the arrival was held until its own bound: {text}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_sync_point_in_a_task_on_a_current_thread_runtime_fails_the_arm_by_name() {
    let arm = Holds::here().arm("t.sync.task").give_up_after(Duration::from_millis(200)).hold(1);
    let task = tokio::spawn(async {
        crate::hold_point!("t.sync.task");
    });
    // The task holds the runtime's only thread until its give-up, so this resumes after it.
    task.await.expect("the product side never panics");
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| drop(arm))));
    assert!(text.contains("t.sync.task") && text.contains("hold_point_async!"), "{text}");
}

/// The same case as the test's own `reached_async` meets it: it resumes after the give-up and fails by name.
#[tokio::test(flavor = "current_thread")]
#[should_panic(expected = "hold_point_async!")]
async fn reached_async_resumed_after_a_give_up_fails_by_name() {
    // Never dropped: the panic must come from `reached_async` itself, not from the arm's drop.
    let arm = std::mem::ManuallyDrop::new(Holds::here().arm("t.sync.resume").give_up_after(Duration::from_millis(200)).hold(1));
    tokio::spawn(async {
        crate::hold_point!("t.sync.resume");
    });
    arm.reached_async(0).await;
}

/// A cancelled `reached_async` leaves its wait on the blocking pool after the arm is gone: it must end with an error,
/// never a panic on that thread.
#[test]
fn a_wait_that_outlives_its_arm_ends_with_an_error() {
    let arm = Holds::here().arm("t.cancelled").hold(1);
    let id = arm.id;
    drop(arm);
    let failure = hold::wait_arrival(id, 0, SEEN).expect_err("the arm is gone");
    assert!(failure.contains("ended"), "{failure}");
}

/// A plain executor: a thread parked until woken.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    struct Wake(std::thread::Thread);
    impl std::task::Wake for Wake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(Arc::new(Wake(std::thread::current())));
    let mut context = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let std::task::Poll::Ready(out) = future.as_mut().poll(&mut context) {
            return out;
        }
        std::thread::park();
    }
}

#[test]
fn an_async_point_polled_outside_a_runtime_waits_like_a_sync_one_and_fails_the_arm() {
    let arm = Holds::here().arm("t.async.plain").give_up_after(Duration::from_millis(200)).hold(1);
    let (done, gone) = mpsc::channel();
    std::thread::spawn(move || {
        block_on(async { crate::hold_point_async!("t.async.plain") });
        done.send(()).ok();
    });
    goes_on(&gone, "after its give-up");
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| arm.release(0))));
    assert!(text.contains("t.async.plain") && text.contains("went on"), "its ticket is not left held: {text}");
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| drop(arm))));
    assert!(text.contains("t.async.plain"), "{text}");
}

#[test]
fn every_call_of_an_arm_that_gave_up_fails_by_name() {
    let arm = Holds::here().arm("t.gaveup.calls").give_up_after(Duration::from_millis(100)).hold(1);
    goes_on(&arrives("t.gaveup.calls", None), "after its give-up");
    for (call, text) in [
        ("reached", panic_text(catch_unwind(AssertUnwindSafe(|| arm.reached(0))))),
        ("release", panic_text(catch_unwind(AssertUnwindSafe(|| arm.release(0))))),
        ("arrivals", panic_text(catch_unwind(AssertUnwindSafe(|| { arm.arrivals(); })))),
    ] {
        assert!(text.contains("t.gaveup.calls") && text.contains("went on"), "{call}: {text}");
    }
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| drop(arm))));
    assert!(text.contains("t.gaveup.calls"), "{text}");
}

#[test]
fn a_bound_longer_than_the_tools_own_is_refused() {
    let spec = Holds::here().arm("t.bounds");
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| drop(spec.give_up_after(Duration::MAX)))));
    assert!(text.contains("give_up_after") && text.contains("t.bounds"), "{text}");
    let arm = Holds::here().arm("t.bounds").hold(0);
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| arm.reached_within(0, WAIT + Duration::from_secs(1)))));
    assert!(text.contains("reached_within") && text.contains("t.bounds"), "{text}");
}

/// Each arm has its own wake, and a missing notify is only a wait to the bound (the wait re-reads the table when it
/// times out), so the test reads the wait's length: `reached` parks first (its arrival waits on a signal sent just
/// before it) and must be woken well inside its bound.
#[test]
fn an_arrival_wakes_the_reached_that_waits_for_it() {
    const BOUND: Duration = Duration::from_secs(4);
    let arm = Holds::here().arm("t.wake.claim").hold(0);
    for ticket in 0..3 {
        let (go, wait) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            wait.recv().ok();
            arrive("t.wake.claim", None);
        });
        let started = std::time::Instant::now();
        go.send(()).unwrap();
        arm.reached_within(ticket, BOUND);
        assert!(started.elapsed() < BOUND / 2, "reached woke only at its bound: {:?}", started.elapsed());
    }
}

/// Through the handle of a runtime already dropped, the wait's blocking task is cancelled unrun: the ticket must not
/// stay held with the product gone on; the arm fails by name.
#[test]
fn an_async_point_through_a_dropped_runtimes_handle_fails_the_arm_by_name() {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let handle = runtime.handle().clone();
    drop(runtime);
    let arm = Holds::here().arm("t.dead.handle").hold(1);
    let (done, gone) = mpsc::channel();
    std::thread::spawn(move || {
        handle.block_on(async { crate::hold_point_async!("t.dead.handle") });
        done.send(()).ok();
    });
    goes_on(&gone, "through a dead handle");
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| arm.release(0))));
    assert!(text.contains("t.dead.handle") && text.contains("waiter was lost") && !text.contains("hold_point_async!"), "{text}");
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| drop(arm))));
    assert!(text.contains("t.dead.handle"), "{text}");
}

// The folder half: child processes started with the folder's variable. A child is this test binary rerun on
// `a_child_that_arrives` (a no-op in an ordinary run), which arrives at a point `SOT_HOLD_CHILD_ARRIVALS` times, each
// after the first when `next` appears, and records every arrival that went on as `pass.<pid>.<n>`: `go` if a release
// (or the arm's drop) had been written by then, `no-go` if not.

const CHILD_BODY: &str = "test_hold::tests::a_child_that_arrives";

#[test]
fn a_child_that_arrives() {
    use crate::test_isolated::enter;
    let Some(out) = std::env::var_os("SOT_HOLD_CHILD_OUT").map(std::path::PathBuf::from) else { return };
    enter(CHILD_BODY);
    let point: &'static str = Box::leak(std::env::var("SOT_HOLD_CHILD_POINT").expect("point").into_boxed_str());
    let arrivals: usize = std::env::var("SOT_HOLD_CHILD_ARRIVALS").expect("arrivals").parse().expect("a count");
    for round in 1..=arrivals {
        if round > 1 {
            appears(&out.join("next"), "the parent's go-ahead");
        }
        arrive(point, None);
        let folder = std::path::PathBuf::from(std::env::var_os("SOT_TEST_HOLDS").expect("the folder"));
        let released = std::fs::read_dir(&folder).unwrap().flatten().any(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.ends_with(".go") || name.ends_with(".all")
        });
        std::fs::write(out.join(format!("pass.{}.{round}", std::process::id())), if released { "go" } else { "no-go" }).unwrap();
    }
}

fn appears(path: &std::path::Path, what: &str) {
    let started = std::time::Instant::now();
    while !path.exists() {
        assert!(started.elapsed() < SEEN, "{what}: {} did not appear within {SEEN:?}", path.display());
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct ChildRun {
    child: std::process::Child,
    entry: Option<crate::test_isolated::Entry>,
}

impl ChildRun {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The child's own end, within the isolation bound, having entered its body once.
    fn ends(mut self) {
        let status = crate::test_isolated::wait_within(&mut self.child, crate::test_isolated::ISOLATION_TIMEOUT);
        self.entry.take().unwrap().assert_once(self.pid());
        assert!(status.success(), "the child ended badly: {status}");
    }
}

impl Drop for ChildRun {
    fn drop(&mut self) {
        // A child left holding by a failed test ends here, not after its give-up.
        self.child.kill().ok();
        self.child.wait().ok();
        if let Some(entry) = self.entry.take() {
            entry.assert_once(self.child.id());
        }
    }
}

fn start_child(holds: &Holds, out: &std::path::Path, point: &str, arrivals: usize) -> ChildRun {
    let (mut command, entry) = crate::test_isolated::test_command(CHILD_BODY);
    let (name, folder) = holds.env();
    command
        .env(name, folder)
        .env("SOT_HOLD_CHILD_OUT", out)
        .env("SOT_HOLD_CHILD_POINT", point)
        .env("SOT_HOLD_CHILD_ARRIVALS", arrivals.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    let child = command.spawn().expect("spawn the child");
    ChildRun { child, entry: Some(entry) }
}

fn pass_of(out: &std::path::Path, child: &ChildRun, round: usize) -> String {
    let path = out.join(format!("pass.{}.{round}", child.pid()));
    appears(&path, "the child's arrival to go on");
    std::fs::read_to_string(path).unwrap()
}

fn folder_for(name: &str) -> (tempfile::TempDir, Holds, std::path::PathBuf) {
    let tmp = tempfile::Builder::new().prefix(name).tempdir().unwrap();
    let out = tmp.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let holds = Holds::folder(&tmp.path().join("holds"));
    (tmp, holds, out)
}

#[test]
fn a_child_started_with_the_folder_is_held_at_its_line_and_released() {
    let (_tmp, holds, out) = folder_for("hold-child-");
    let arm = holds.arm("t.child.held").hold(1);
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| arm.release(0))));
    assert!(text.contains("has not arrived"), "{text}");
    let child = start_child(&holds, &out, "t.child.held", 1);
    arm.reached(0);
    assert_eq!(arm.arrivals(), 1);
    assert!(!out.join(format!("pass.{}.1", child.pid())).exists(), "the child went on before its release");
    arm.release(0);
    assert_eq!(pass_of(&out, &child, 1), "go", "the child went on without a release");
    child.ends();
}

#[test]
fn two_children_sharing_a_folder_take_distinct_tickets() {
    let (_tmp, holds, out) = folder_for("hold-two-");
    let arm = holds.arm("t.child.two").hold(2);
    let (first, second) = (start_child(&holds, &out, "t.child.two", 1), start_child(&holds, &out, "t.child.two", 1));
    arm.reached(1);
    let folder = holds.env().1.to_path_buf();
    // The ticket's file is made before its pid is written into it.
    let pid_at = |ticket: usize| loop {
        let text = std::fs::read_to_string(folder.join(format!("t.child.two.{ticket}.at"))).unwrap_or_default();
        if let Ok(pid) = text.parse::<u32>() {
            return pid;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut pids = [pid_at(0), pid_at(1)];
    pids.sort();
    let mut started = [first.pid(), second.pid()];
    started.sort();
    assert_eq!(pids, started, "each child holds one ticket, neither shares one");
    arm.release(0);
    arm.release(1);
    assert_eq!(pass_of(&out, &first, 1), "go");
    assert_eq!(pass_of(&out, &second, 1), "go");
    first.ends();
    second.ends();
}

#[test]
fn a_child_arrival_nobody_releases_gives_up_and_the_arm_fails() {
    let (_tmp, holds, out) = folder_for("hold-giveup-");
    let arm = holds.arm("t.child.giveup").give_up_after(Duration::from_millis(200)).hold(1);
    let child = start_child(&holds, &out, "t.child.giveup", 1);
    assert_eq!(pass_of(&out, &child, 1), "no-go", "it went on by its give-up, not a release");
    for (call, text) in [
        ("reached", panic_text(catch_unwind(AssertUnwindSafe(|| arm.reached(0))))),
        ("release", panic_text(catch_unwind(AssertUnwindSafe(|| arm.release(0))))),
        ("arrivals", panic_text(catch_unwind(AssertUnwindSafe(|| { arm.arrivals(); })))),
    ] {
        assert!(text.contains("t.child.giveup") && text.contains("went on"), "{call}: {text}");
    }
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| drop(arm))));
    assert!(text.contains("t.child.giveup") && text.contains("arrival 0"), "{text}");
    child.ends();
}

#[test]
fn an_arm_written_after_the_child_started_holds_its_next_arrival() {
    let (_tmp, holds, out) = folder_for("hold-late-");
    let child = start_child(&holds, &out, "t.child.late", 2);
    assert_eq!(pass_of(&out, &child, 1), "no-go", "no arm yet: the arrival passes");
    let arm = holds.arm("t.child.late").hold(1);
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| arm.reached_within(0, Duration::from_millis(200)))));
    assert!(text.contains("t.child.late") && text.contains("0 arrivals seen"), "an arrival before the arm is not seen: {text}");
    std::fs::write(out.join("next"), "").unwrap();
    arm.reached(0);
    assert_eq!(arm.arrivals(), 1);
    assert!(!out.join(format!("pass.{}.2", child.pid())).exists());
    arm.release(0);
    assert_eq!(pass_of(&out, &child, 2), "go");
    child.ends();
}

#[test]
fn the_arm_drop_releases_a_held_child() {
    let (_tmp, holds, out) = folder_for("hold-drop-");
    let arm = holds.arm("t.child.drop").hold(1);
    let child = start_child(&holds, &out, "t.child.drop", 1);
    arm.reached(0);
    drop(arm);
    assert_eq!(pass_of(&out, &child, 1), "go", "the drop's file lets it go on");
    child.ends();
}

#[test]
fn a_child_killed_while_held_leaves_no_give_up_and_the_drop_does_not_fail() {
    let (_tmp, holds, out) = folder_for("hold-killed-");
    let arm = holds.arm("t.child.killed").hold(1);
    let mut child = start_child(&holds, &out, "t.child.killed", 1);
    arm.reached(0);
    child.child.kill().expect("kill the held child");
    child.child.wait().expect("reap it");
    drop(arm);
    let folder = holds.env().1;
    assert!(folder.join("t.child.killed.all").exists(), "the drop writes the all-go file");
    assert!(!folder.join("t.child.killed.0.gaveup").exists(), "a killed child records no give-up");
}

#[test]
fn a_second_arm_of_a_point_in_one_folder_is_refused_for_the_folders_life() {
    let (_tmp, holds, _out) = folder_for("hold-second-");
    let first = holds.arm("t.child.second").hold(1);
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| drop(holds.arm("t.child.second").hold(0)))));
    assert!(text.contains("t.child.second") && text.contains("refused"), "{text}");
    drop(first);
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| drop(holds.arm("t.child.second").hold(0)))));
    assert!(text.contains("refused"), "no file is deleted, so the point stays armed: {text}");
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| { Holds::here().env(); })));
    assert!(text.contains("Holds::folder"), "{text}");
}

#[test]
fn an_arm_file_is_never_seen_half_written() {
    let (_tmp, holds, _out) = folder_for("hold-whole-");
    let path = holds.env().1.join("t.child.whole.arm");
    let done = Arc::new(AtomicBool::new(false));
    let stop = done.clone();
    let reader = std::thread::spawn(move || {
        let started = std::time::Instant::now();
        while !stop.load(Ordering::SeqCst) {
            assert!(started.elapsed() < SEEN, "the arm file never appeared");
            if let Ok(text) = std::fs::read_to_string(&path) {
                assert!(text.ends_with("\n") && text.contains("give_up_ms"), "a partial arm file was visible ({} bytes)", text.len());
                return;
            }
        }
    });
    // A key of several megabytes: a write in place would take long enough for the reader to see part of it.
    let arm = holds.arm("t.child.whole").key("k".repeat(8 << 20)).hold(1);
    done.store(true, Ordering::SeqCst);
    reader.join().expect("the reader saw only whole files");
    drop(arm);
}
