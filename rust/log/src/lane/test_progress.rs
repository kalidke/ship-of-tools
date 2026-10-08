//! Transport-local checkpoints and scoped regression controls, one recorder per server (and per socket client).
//! Normal builds carry only zero-sized no-ops.

#[cfg(any(test, feature = "test-support"))]
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex, MutexGuard, TryLockError,
    },
    time::{Duration, Instant},
};

#[cfg(any(test, feature = "test-support"))]
const CAPACITY: usize = 256;

/// How many times a checkpoint tries a momentarily busy ring before it is skipped: bounded, never a wait.
#[cfg(any(test, feature = "test-support"))]
const ADMIT_ATTEMPTS: u32 = 64;

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct Checkpoint {
    pub transport: &'static str,
    pub conn: Option<u64>,
    pub step: &'static str,
    pub elapsed_ms: u128,
    pub caller: String,
    pub result: String,
    pub errno: Option<i32>,
}

#[cfg(any(test, feature = "test-support"))]
impl fmt::Display for Checkpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let conn = self
            .conn
            .map_or_else(|| "pending".into(), |id| id.to_string());
        let errno = self
            .errno
            .map_or_else(|| "none".into(), |code| code.to_string());
        write!(
            f,
            "transport-progress transport={} conn={conn} step={} elapsed_ms={} caller={} result={} errno={errno}",
            self.transport, self.step, self.elapsed_ms, self.caller, self.result
        )
    }
}

#[cfg(any(test, feature = "test-support"))]
struct Ring {
    records: VecDeque<Arc<Checkpoint>>,
    overwritten: usize,
}

#[cfg(any(test, feature = "test-support"))]
pub struct Progress {
    transport: &'static str,
    started: Instant,
    ring: Mutex<Ring>,
    skipped: AtomicUsize,
}

#[cfg(not(any(test, feature = "test-support")))]
#[derive(Default)]
pub struct Progress;

#[cfg(any(test, feature = "test-support"))]
impl Default for Progress {
    fn default() -> Self {
        Self::new("socket")
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Progress {
    /// A recorder for `transport` (`socket` or `pipe`).
    pub(crate) fn new(transport: &'static str) -> Self {
        Self {
            transport,
            started: Instant::now(),
            skipped: AtomicUsize::new(0),
            ring: Mutex::new(Ring {
                records: VecDeque::with_capacity(CAPACITY),
                overwritten: 0,
            }),
        }
    }
}

#[cfg(all(windows, not(any(test, feature = "test-support"))))]
impl Progress {
    #[inline]
    pub(crate) fn new(_transport: &'static str) -> Self {
        Progress
    }
}

/// The repository-relative spelling of a source path as rustc recorded it.
#[cfg(any(test, feature = "test-support"))]
fn repo_relative(file: &str) -> String {
    let file = file.replace('\\', "/");
    if let Some(at) = file.find("rust/") {
        file[at..].to_string()
    } else if file.starts_with("log/") {
        format!("rust/{file}")
    } else {
        format!("rust/log/{file}")
    }
}

impl Progress {
    /// A checkpoint whose caller is the site that noted it and whose errno is none.
    #[cfg(any(test, feature = "test-support"))]
    #[track_caller]
    pub(crate) fn note(&self, conn: Option<u64>, step: &'static str, result: impl fmt::Display) {
        let at = std::panic::Location::caller();
        let caller = format!("{}:{}", repo_relative(at.file()), at.line());
        self.note_with(conn, step, result, &caller, None);
    }

    /// A checkpoint with an explicit caller (a repository-relative function) and the numeric errno captured at the call.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn note_with(
        &self,
        conn: Option<u64>,
        step: &'static str,
        result: impl fmt::Display,
        caller: &str,
        errno: Option<i32>,
    ) {
        let record = Arc::new(Checkpoint {
            transport: self.transport,
            conn,
            step,
            elapsed_ms: self.started.elapsed().as_millis(),
            caller: caller.to_string(),
            result: result.to_string(),
            errno,
        });
        let mut ring = match self.admit() {
            Some(ring) => ring,
            None => {
                self.skipped.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        if ring.records.len() == CAPACITY {
            ring.records.pop_front();
            ring.overwritten += 1;
        }
        ring.records.push_back(record);
    }

    #[cfg(not(any(test, feature = "test-support")))]
    #[inline]
    pub(crate) fn note(
        &self,
        _conn: Option<u64>,
        _step: &'static str,
        _result: impl std::fmt::Display,
    ) {
    }

    #[cfg(all(unix, not(any(test, feature = "test-support"))))]
    #[inline]
    pub(crate) fn note_with(
        &self,
        _conn: Option<u64>,
        _step: &'static str,
        _result: impl std::fmt::Display,
        _caller: &str,
        _errno: Option<i32>,
    ) {
    }

    /// The ring if it is free within a bounded few attempts. Another thread's checkpoint or a polling snapshot holds
    /// it for microseconds; a deliberate fixture hold or a poisoned ring outlasts the attempts, and the checkpoint is
    /// skipped (and counted) rather than waited for.
    #[cfg(any(test, feature = "test-support"))]
    fn admit(&self) -> Option<MutexGuard<'_, Ring>> {
        for attempt in 0..ADMIT_ATTEMPTS {
            match self.ring.try_lock() {
                Ok(ring) => return Some(ring),
                Err(TryLockError::Poisoned(_)) => return None,
                Err(TryLockError::WouldBlock) if attempt < ADMIT_ATTEMPTS / 2 => {
                    std::hint::spin_loop()
                }
                Err(TryLockError::WouldBlock) => std::thread::yield_now(),
            }
        }
        None
    }

    /// Deliberate fixture hold; passive operations never use this blocking acquisition.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg_attr(windows, allow(dead_code))]
    pub(crate) fn hold(&self) -> Hold<'_> {
        Hold {
            _held: self.ring.lock().expect("hold the recorder fixture"),
        }
    }

    /// Copies the ring without waiting for its lock, independently of connection state.
    #[cfg(any(test, feature = "test-support"))]
    pub fn snapshot(&self) -> Snapshot {
        match self.ring.try_lock() {
            Ok(ring) => {
                // Only reference counts are copied under the lock, so a polling reader keeps it for microseconds.
                let (shared, overwritten) = (ring.records.clone(), ring.overwritten);
                drop(ring);
                Snapshot {
                    records: shared.iter().map(|record| (**record).clone()).collect(),
                    overwritten: Some(overwritten),
                    skipped: self.skipped.load(Ordering::Relaxed),
                    unavailable: false,
                    reason: None,
                }
            }
            Err(error) => Snapshot {
                records: Vec::new(),
                overwritten: None,
                skipped: self.skipped.load(Ordering::Relaxed),
                unavailable: true,
                reason: Some(match error {
                    TryLockError::WouldBlock => "busy",
                    TryLockError::Poisoned(_) => "poisoned",
                }),
            },
        }
    }
}

/// Which of a connection's two workers a regression control addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Reader,
    Writer,
}

#[cfg(any(test, feature = "test-support"))]
impl Role {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Role::Reader => "reader",
            Role::Writer => "writer",
        }
    }
}

/// A one-shot rendezvous: the guarded site marks it reached and waits; the test observes that and releases it.
/// Neither side holds a transport lock while it waits.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
pub struct Gate {
    state: Mutex<(bool, bool)>,
    cv: Condvar,
}

#[cfg(any(test, feature = "test-support"))]
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new((false, false)),
            cv: Condvar::new(),
        })
    }

    /// The guarded site's side: mark reached, then wait for the release.
    pub(crate) fn pass(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        self.cv.notify_all();
        while !state.1 {
            state = self.cv.wait(state).unwrap();
        }
    }

    /// True once the guarded site is waiting here, within `timeout`.
    pub fn wait_reached(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        while !state.0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self.cv.wait_timeout(state, left).unwrap().0;
        }
        true
    }

    /// Let the guarded site continue. Idempotent.
    pub fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.cv.notify_all();
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
struct ControlState {
    exit_holds: HashMap<(u64, Role), Arc<Gate>>,
    exit_panics: HashSet<(u64, Role)>,
    barriers: HashMap<&'static str, Arc<Gate>>,
    #[cfg(windows)]
    failures: HashSet<&'static str>,
    teardown: Option<Duration>,
}

/// Scoped regression controls of one server: worker-exit holds and panics, named barriers, injected failures and a
/// short teardown deadline. Every site reaches them after its last I/O and outside every transport lock. Normal builds
/// carry a zero-sized value whose methods do nothing.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
pub struct Controls {
    state: Mutex<ControlState>,
}

#[cfg(not(any(test, feature = "test-support")))]
#[derive(Default)]
pub struct Controls;

#[cfg(any(test, feature = "test-support"))]
impl Controls {
    /// Arm a one-shot hold of `conn`'s `role` worker at its exit point.
    pub(crate) fn arm_exit_hold(&self, conn: u64, role: Role) -> Arc<Gate> {
        let gate = Gate::new();
        self.state
            .lock()
            .unwrap()
            .exit_holds
            .insert((conn, role), Arc::clone(&gate));
        gate
    }

    /// Arm a one-shot panic of `conn`'s `role` worker at its exit point.
    pub(crate) fn arm_exit_panic(&self, conn: u64, role: Role) {
        self.state.lock().unwrap().exit_panics.insert((conn, role));
    }

    /// Arm a one-shot barrier at the named site.
    pub(crate) fn arm_barrier(&self, name: &'static str) -> Arc<Gate> {
        let gate = Gate::new();
        self.state
            .lock()
            .unwrap()
            .barriers
            .insert(name, Arc::clone(&gate));
        gate
    }

    /// Arm a one-shot failure at the named site.
    #[cfg(windows)]
    pub(crate) fn arm_failure(&self, name: &'static str) {
        self.state.lock().unwrap().failures.insert(name);
    }

    pub(crate) fn set_teardown_deadline(&self, deadline: Duration) {
        self.state.lock().unwrap().teardown = Some(deadline);
    }

    /// A normal close's report budget: a test's short one, else the production budget.
    pub(crate) fn close_budget(&self) -> Duration {
        self.state
            .lock()
            .unwrap()
            .teardown
            .unwrap_or(crate::lane::pending::NORMAL_CLOSE_BUDGET)
    }

    /// The shutdown deadline `Drop` uses: a test's short one, else the production aggregate.
    pub(crate) fn teardown_deadline(&self) -> Duration {
        self.state
            .lock()
            .unwrap()
            .teardown
            .unwrap_or(crate::lane::transport::TEARDOWN_AGGREGATE_DEADLINE)
    }

    /// A worker's exit point, reached after its last I/O and its teardown request: the armed hold waits here until
    /// released, then the armed panic fires.
    pub(crate) fn exit_point(&self, progress: &Progress, conn: u64, role: Role) {
        let (hold, panic) = {
            let mut state = self.state.lock().unwrap();
            (
                state.exit_holds.remove(&(conn, role)),
                state.exit_panics.remove(&(conn, role)),
            )
        };
        if let Some(gate) = hold {
            let step = if role == Role::Reader {
                "reader.hold"
            } else {
                "writer.hold"
            };
            progress.note(Some(conn), step, "held");
            gate.pass();
            progress.note(Some(conn), step, "released");
        }
        if panic {
            let step = if role == Role::Reader {
                "reader.panic"
            } else {
                "writer.panic"
            };
            progress.note(Some(conn), step, "injected");
            panic!("injected {} worker panic", role.name());
        }
    }

    /// The named barrier, if armed: wait here until the test releases it.
    pub(crate) fn barrier_point(&self, progress: &Progress, conn: Option<u64>, name: &'static str) {
        let gate = self.state.lock().unwrap().barriers.remove(name);
        if let Some(gate) = gate {
            progress.note(conn, name, "held");
            gate.pass();
            progress.note(conn, name, "released");
        }
    }

    /// True once if a failure was armed at `name`.
    #[cfg(windows)]
    pub(crate) fn take_failure(&self, name: &'static str) -> bool {
        self.state.lock().unwrap().failures.remove(name)
    }
}

#[cfg(not(any(test, feature = "test-support")))]
impl Controls {
    #[inline]
    pub(crate) fn close_budget(&self) -> std::time::Duration {
        crate::lane::pending::NORMAL_CLOSE_BUDGET
    }
    #[inline]
    pub(crate) fn teardown_deadline(&self) -> std::time::Duration {
        crate::lane::transport::TEARDOWN_AGGREGATE_DEADLINE
    }
    #[inline]
    pub(crate) fn exit_point(&self, _progress: &Progress, _conn: u64, _role: Role) {}
    #[inline]
    pub(crate) fn barrier_point(
        &self,
        _progress: &Progress,
        _conn: Option<u64>,
        _name: &'static str,
    ) {
    }
    #[cfg(windows)]
    #[inline]
    pub(crate) fn take_failure(&self, _name: &'static str) -> bool {
        false
    }
}

/// A test's end of a [`Gate`]: wait until the guarded site is stopped there, then release it. Dropping releases, so a
/// failed test cannot leave a worker held behind its server.
#[cfg(any(test, feature = "test-support"))]
pub struct Pause {
    gate: Arc<Gate>,
}

#[cfg(any(test, feature = "test-support"))]
impl Pause {
    pub(crate) fn new(gate: Arc<Gate>) -> Self {
        Self { gate }
    }

    /// True once the guarded site is stopped here, within `timeout`.
    pub fn wait_reached(&self, timeout: Duration) -> bool {
        self.gate.wait_reached(timeout)
    }

    /// Let the guarded site continue. Idempotent.
    pub fn release(&self) {
        self.gate.release();
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for Pause {
    fn drop(&mut self) {
        self.gate.release();
    }
}

/// One descriptor a factory created, as it was when ownership was taken and before any flag-setting call.
#[cfg(all(unix, any(test, feature = "test-support")))]
#[derive(Clone, Debug)]
pub struct Birth {
    pub role: &'static str,
    pub fd: std::os::fd::RawFd,
    /// `F_GETFD` read at birth; `-1` when that read failed.
    pub fd_flags: i32,
}

#[cfg(all(unix, any(test, feature = "test-support")))]
#[derive(Default)]
struct BirthScope {
    births: Vec<Birth>,
    /// Fail the n-th (1-based) flag-setting call made in this scope.
    fail_at: Option<usize>,
    calls: usize,
}

#[cfg(all(unix, any(test, feature = "test-support")))]
thread_local! {
    static BIRTHS: std::cell::RefCell<Option<BirthScope>> = const { std::cell::RefCell::new(None) };
}

/// Run `factory` on this thread with every descriptor birth observed and, when `fail_at` is set, that flag-setting
/// call (`set_cloexec`/`set_nonblocking` and the lease's checked pass) failing for real through the factory's own
/// error path. Returns the factory's result, the births in order, and how many flag-setting calls it made. Reads
/// `F_GETFD` only; it changes no descriptor.
#[cfg(all(unix, any(test, feature = "test-support")))]
pub fn observe_births<R>(
    fail_at: Option<usize>,
    factory: impl FnOnce() -> R,
) -> (R, Vec<Birth>, usize) {
    BIRTHS.with(|scope| {
        *scope.borrow_mut() = Some(BirthScope {
            fail_at,
            ..BirthScope::default()
        })
    });
    let result = factory();
    let scope = BIRTHS
        .with(|scope| scope.borrow_mut().take())
        .expect("the scope opened above");
    (result, scope.births, scope.calls)
}

/// A factory has just taken ownership of `fd` for `role`, before any flag-setting call.
#[cfg(unix)]
#[inline]
pub(crate) fn birth(role: &'static str, fd: std::os::fd::RawFd) {
    #[cfg(any(test, feature = "test-support"))]
    BIRTHS.with(|scope| {
        if let Some(scope) = scope.borrow_mut().as_mut() {
            let fd_flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            scope.births.push(Birth { role, fd, fd_flags });
        }
    });
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = (role, fd);
}

/// A flag-setting call is about to run: the injected error when this is the call a scope chose to fail.
#[cfg(unix)]
#[inline]
pub(crate) fn flag_call() -> Option<std::io::Error> {
    #[cfg(any(test, feature = "test-support"))]
    return BIRTHS.with(|scope| {
        let mut scope = scope.borrow_mut();
        let scope = scope.as_mut()?;
        scope.calls += 1;
        (scope.fail_at == Some(scope.calls)).then(|| std::io::Error::from_raw_os_error(libc::EPERM))
    });
    #[cfg(not(any(test, feature = "test-support")))]
    None
}

/// Opaque test fixture holding only a server's recorder.
#[cfg(any(test, feature = "test-support"))]
pub struct Hold<'a> {
    _held: MutexGuard<'a, Ring>,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
pub struct Snapshot {
    pub records: Vec<Checkpoint>,
    pub overwritten: Option<usize>,
    pub skipped: usize,
    pub reason: Option<&'static str>,
    pub unavailable: bool,
}

#[cfg(any(test, feature = "test-support"))]
impl fmt::Display for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.unavailable {
            return writeln!(
                f,
                "transport-progress snapshot unavailable skipped={} reason={}",
                self.skipped,
                self.reason.unwrap()
            );
        }
        writeln!(
            f,
            "transport-progress snapshot records={} overwritten={} skipped={}",
            self.records.len(),
            self.overwritten.unwrap(),
            self.skipped
        )?;
        for record in &self.records {
            writeln!(f, "{record}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_checkpoint_admission_is_skipped_and_counted() {
        let progress = std::sync::Arc::new(Progress::default());
        progress.note(Some(1), "registered", "ok");
        let before = progress.snapshot().to_string();
        let held = progress.ring.lock().unwrap();
        let (entered, entry) = std::sync::mpsc::channel();
        let (finished, completion) = std::sync::mpsc::channel();
        let producer = std::thread::spawn({
            let progress = std::sync::Arc::clone(&progress);
            move || {
                entered.send(()).unwrap();
                for _ in 0..7 {
                    progress.note(Some(2), "registered", "ok");
                }
                finished.send(()).unwrap();
            }
        });
        let observed_entry = entry
            .recv_timeout(std::time::Duration::from_secs(2))
            .is_ok();
        let completed_while_held = completion
            .recv_timeout(std::time::Duration::from_secs(2))
            .is_ok();
        drop(held);
        producer.join().unwrap();
        assert!(observed_entry, "checkpoint producer did not enter");
        assert!(
            completed_while_held,
            "checkpoint admission waited for the busy recorder"
        );
        let after = progress.snapshot().to_string();
        assert!(
            after.contains("skipped=7"),
            "known checkpoint attempts were not counted: {after}"
        );
        assert_eq!(
            before.lines().skip(1).collect::<Vec<_>>(),
            after.lines().skip(1).collect::<Vec<_>>()
        );
        assert!(
            after.contains("records=1 overwritten=0"),
            "busy admission changed the ring: {after}"
        );
        eprintln!("recorder-proof attempts=7 skipped=7 completed=while-held bodies=1");
    }

    #[test]
    fn busy_snapshot_does_not_wait() {
        let progress = Progress::default();
        let _held = progress.ring.lock().unwrap();
        let snapshot = progress.snapshot();
        assert!(snapshot.unavailable);
        assert_eq!(
            snapshot.to_string(),
            "transport-progress snapshot unavailable skipped=0 reason=busy\n"
        );
    }

    #[test]
    fn poisoned_snapshot_reports_unavailable_without_ring_counts() {
        let progress = Progress::default();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = progress.ring.lock().unwrap();
            panic!("poison the test-owned recorder");
        }));
        progress.note(None, "registered", "skipped");
        let snapshot = progress.snapshot();
        assert!(snapshot.unavailable);
        assert_eq!(snapshot.overwritten, None);
        assert_eq!(snapshot.skipped, 1);
        assert_eq!(
            snapshot.to_string(),
            "transport-progress snapshot unavailable skipped=1 reason=poisoned\n"
        );
    }

    #[test]
    fn overwrite_keeps_the_last_256_even_after_a_connection_ends() {
        let progress = Progress::default();
        for id in 0..300 {
            progress.note(Some(id), "closed.enqueue", "ok");
        }
        let snapshot = progress.snapshot();
        assert_eq!(snapshot.overwritten, Some(44));
        assert_eq!(snapshot.skipped, 0);
        assert_eq!(snapshot.records.len(), 256);
        assert_eq!(snapshot.records[0].conn, Some(44));
        assert_eq!(snapshot.records[255].conn, Some(299));
    }

    fn fields(record: &Checkpoint) -> Vec<String> {
        record
            .to_string()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn checkpoint_line_names_transport_caller_result_and_errno() {
        let progress = Progress::new("pipe");
        progress.note_with(
            None,
            "shutdown.result",
            "rc=-1",
            "rust/log/src/lane/socket_unix/client.rs::cancel",
            Some(107),
        );
        progress.note(Some(4), "registered", "ok");
        let snapshot = progress.snapshot();
        let first = fields(&snapshot.records[0]);
        assert_eq!(
            first[..3],
            ["transport-progress", "transport=pipe", "conn=pending"]
        );
        assert_eq!(first[3], "step=shutdown.result");
        assert!(first[4].starts_with("elapsed_ms="));
        assert_eq!(
            first[5..],
            [
                "caller=rust/log/src/lane/socket_unix/client.rs::cancel",
                "result=rc=-1",
                "errno=107"
            ]
        );
        let second = fields(&snapshot.records[1]);
        assert_eq!(second[2], "conn=4");
        assert!(
            second[5].starts_with("caller=rust/log/src/lane/test_progress.rs:"),
            "an unnamed caller is the site that noted: {}",
            second[5]
        );
        assert_eq!(second[6..], ["result=ok", "errno=none"]);
    }

    #[test]
    fn exit_hold_blocks_once_and_panic_fires_once_at_the_exit_point() {
        let progress = Progress::default();
        let controls = Arc::new(Controls::default());
        let gate = controls.arm_exit_hold(7, Role::Reader);
        controls.arm_exit_panic(7, Role::Reader);
        let worker = std::thread::spawn({
            let controls = Arc::clone(&controls);
            move || {
                let progress = Progress::default();
                controls.exit_point(&progress, 7, Role::Reader);
            }
        });
        assert!(
            gate.wait_reached(Duration::from_secs(5)),
            "the worker never stopped at its hold"
        );
        assert!(!worker.is_finished(), "the hold did not hold");
        gate.release();
        assert!(
            worker.join().is_err(),
            "the armed panic did not fire after the release"
        );
        // Both were consumed: the same point now passes.
        controls.exit_point(&progress, 7, Role::Reader);
        controls.exit_point(&progress, 7, Role::Writer);
    }

    #[test]
    fn barrier_is_one_shot_and_unarmed_points_pass() {
        let progress = Progress::default();
        let controls = Controls::default();
        controls.barrier_point(&progress, None, "registration.barrier");
        let gate = controls.arm_barrier("registration.barrier");
        std::thread::scope(|scope| {
            let waiting =
                scope.spawn(|| controls.barrier_point(&progress, Some(1), "registration.barrier"));
            assert!(gate.wait_reached(Duration::from_secs(5)));
            gate.release();
            waiting.join().unwrap();
        });
        controls.barrier_point(&progress, Some(2), "registration.barrier");
    }
}
