//! child_signal.rs — the one process-wide `fired` flag every child owner
//! selects on, because no drop runs at `process::exit`.
//! [`Signal::spawn`] and [`Signal::spawn_std`] start a child in its own
//! containment, and [`Contained`] and [`ContainedStd`] own it: firing the
//! signal, or killing, waiting for or dropping either, ends the child and
//! everything it started, and the child is reaped only after.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use tokio::sync::watch;

/// One shutdown signal, the count of children still alive under it, and the
/// trees it will kill when it fires. The process has one ([`fire`],
/// [`fired`], [`process`]).
pub(crate) struct Signal {
    fired: watch::Sender<bool>,
    live: AtomicUsize,
    /// Every contained child's tree by id; `None` once fired, so a start after the fire is refused before it
    /// creates anything ([`reserve`](Self::reserve)), and a child created before the fire is killed when it
    /// registers ([`Held::fill`]).
    trees: Mutex<Option<HashMap<u64, crate::lifecycle::contain::Tree>>>,
    next: AtomicU64,
    /// Test-only: called right after a child is created, to put the shutdown's fire in that window.
    #[cfg(test)]
    after_create: Mutex<Option<Box<dyn FnMut() + Send>>>,
}

impl Signal {
    pub(crate) fn new() -> Self {
        Signal {
            fired: watch::channel(false).0,
            live: AtomicUsize::new(0),
            trees: Mutex::new(Some(HashMap::new())),
            next: AtomicU64::new(0),
            #[cfg(test)]
            after_create: Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn after_create(&self) {
        if let Some(hook) = self.after_create.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            hook();
        }
    }

    /// The flag goes first, so an owner that sees its child die already
    /// reads [`is_fired`](Self::is_fired); then every tree is killed, by
    /// dropping the registry under its lock, whatever each owner is
    /// awaiting.
    pub(crate) fn fire(&self) {
        self.fired.send_replace(true);
        let mut trees = self.trees.lock().unwrap_or_else(|e| e.into_inner());
        let taken = trees.take();
        drop(taken);
        drop(trees);
    }

    pub(crate) fn is_fired(&self) -> bool {
        *self.fired.borrow()
    }

    pub(crate) async fn fired(&self) {
        let mut rx = self.fired.subscribe();
        let _ = rx.wait_for(|fired| *fired).await;
    }

    /// Children counted from their reservation, before anything is created, until their [`Held`] drops.
    pub(crate) fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// The process-group number of every tree still held.
    #[cfg(all(test, unix))]
    pub(crate) fn held_groups(&self) -> Vec<i32> {
        let trees = self.trees.lock().unwrap_or_else(|e| e.into_inner());
        trees.as_ref().map(|map| map.values().map(|t| t.pgid()).collect()).unwrap_or_default()
    }

    /// Count a child before anything is created. Refused once the signal has fired, so a start after the fire
    /// creates nothing; counted under the registry lock, so a fire after this sees it in `live` and the shutdown
    /// waits for it.
    fn reserve(&'static self) -> std::io::Result<Held> {
        let trees = self.trees.lock().unwrap_or_else(|e| e.into_inner());
        if trees.is_none() {
            return Err(std::io::Error::other("the daemon is shutting down"));
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.live.fetch_add(1, Ordering::SeqCst);
        drop(trees);
        Ok(Held { sig: self, id })
    }

    /// Reserve a counted slot, then create `cmd`'s child in its own containment, adopt it and register its
    /// tree ([`reserve`](Self::reserve), create, adopt, [`Held::fill`]). The owner keeps the returned
    /// [`Contained`]: waiting for it, killing it, dropping it, or [`fire`](Self::fire) ends the child and
    /// everything it started. Once the signal has fired the start is refused before anything is created; a
    /// child created while it fires is killed when it registers.
    pub(crate) fn spawn(&'static self, cmd: &mut tokio::process::Command) -> std::io::Result<Contained> {
        let held = self.reserve()?;
        // Made before the spawn, so no exit goes unseen by `Contained::wait`.
        #[cfg(unix)]
        let sigchld = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
        crate::lifecycle::contain::prepare(cmd.as_std_mut());
        #[allow(clippy::disallowed_methods, reason = "the containment's own start: reserve and contain::prepare ran before it, and adopt and fill follow (ADR 0050, Shutdown)")]
        let mut child = cmd.spawn()?;
        #[cfg(test)]
        self.after_create();
        let tree = match crate::lifecycle::contain::adopt(
            child.id(),
            #[cfg(windows)]
            child.raw_handle(),
        ) {
            Ok(tree) => tree,
            Err(e) => {
                let _ = child.start_kill();
                return Err(e);
            }
        };
        // The tree's drop killed the child if the signal fired since the reservation; tokio's orphan queue reaps it.
        held.fill(tree)?;
        Ok(Contained {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            held,
            child,
            #[cfg(unix)]
            sigchld,
        })
    }

    /// [`spawn`](Self::spawn) for a blocking caller. The [`ContainedStd`] owns
    /// the child: only its `wait`, `kill` and drop reap it, each after the
    /// tree's kill.
    pub(crate) fn spawn_std(&'static self, cmd: &mut std::process::Command) -> std::io::Result<ContainedStd> {
        let held = self.reserve()?;
        crate::lifecycle::contain::prepare(cmd);
        #[allow(clippy::disallowed_methods, reason = "the containment's own start: reserve and contain::prepare ran before it, and adopt and fill follow (ADR 0050, Shutdown)")]
        let mut child = cmd.spawn()?;
        #[cfg(test)]
        self.after_create();
        let tree = match crate::lifecycle::contain::adopt(
            Some(child.id()),
            #[cfg(windows)]
            Some(std::os::windows::io::AsRawHandle::as_raw_handle(&child)),
        ) {
            Ok(tree) => tree,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        // The tree's drop killed the leader if the signal fired since the reservation; it is only reaped here.
        if let Err(e) = held.fill(tree) {
            let _ = child.wait();
            return Err(e);
        }
        Ok(ContainedStd {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            held,
            child,
            reaped: None,
        })
    }

    /// `Command::output` for a contained one-shot: stdin is null, and stdout
    /// and stderr are each read to their end on a thread of their own. The
    /// leader's exit is seen unreaped; then the tree is killed, which closes
    /// any pipe a descendant held; then the child is reaped (all of that is
    /// [`ContainedStd::wait`]), and then both readers are joined. No bound is
    /// added, as `output` has none.
    pub(crate) fn output(&'static self, cmd: &mut std::process::Command) -> std::io::Result<std::process::Output> {
        use std::process::Stdio;
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = self.spawn_std(cmd)?;
        let stdout = read_to_end_on_a_thread(child.stdout.take());
        let stderr = read_to_end_on_a_thread(child.stderr.take());
        let status = child.wait();
        let (stdout, stderr) = (stdout.join(), stderr.join());
        Ok(std::process::Output { status: status?, stdout: joined(stdout)?, stderr: joined(stderr)? })
    }
}

/// `pipe` read to its end on a thread of its own.
fn read_to_end_on_a_thread<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            pipe.read_to_end(&mut bytes)?;
        }
        Ok(bytes)
    })
}

fn joined(read: std::thread::Result<std::io::Result<Vec<u8>>>) -> std::io::Result<Vec<u8>> {
    read.map_err(|_| std::io::Error::other("a pipe reader panicked"))?
}

/// A counted slot, reserved before its child is created ([`Signal::reserve`]), then filled with the child's tree
/// ([`fill`](Self::fill)). One tree in a [`Signal`]'s registry, and a count of one live child.
/// [`release`](Self::release), or dropping it, kills the tree under the
/// registry lock if the shutdown has not already; the child's leader is
/// reaped only after that, because its pid is the group's number.
struct Held {
    sig: &'static Signal,
    id: u64,
}

impl Held {
    /// Register the reserved child's tree. If the signal fired since the reservation, the tree is dropped, which
    /// kills it, and the start fails.
    fn fill(&self, tree: crate::lifecycle::contain::Tree) -> std::io::Result<()> {
        let mut trees = self.sig.trees.lock().unwrap_or_else(|e| e.into_inner());
        match trees.as_mut() {
            Some(map) => {
                map.insert(self.id, tree);
                Ok(())
            }
            None => {
                drop(tree);
                Err(std::io::Error::other("the daemon is shutting down"))
            }
        }
    }

    fn release(&self) {
        let mut trees = self.sig.trees.lock().unwrap_or_else(|e| e.into_inner());
        let tree = trees.as_mut().and_then(|map| map.remove(&self.id));
        drop(tree);
        drop(trees);
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.release();
        self.sig.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One contained child with its pipes. It owns the `Child`: only
/// [`wait`](Self::wait), [`kill`](Self::kill) and dropping reap it, each
/// after the tree's kill, so a kill never reaches a group number the leader's
/// reap freed. `held` drops before `child`, so a drop kills the tree first.
pub(crate) struct Contained {
    pub(crate) stdin: Option<tokio::process::ChildStdin>,
    pub(crate) stdout: Option<tokio::process::ChildStdout>,
    pub(crate) stderr: Option<tokio::process::ChildStderr>,
    held: Held,
    child: tokio::process::Child,
    #[cfg(unix)]
    sigchld: tokio::signal::unix::Signal,
}

impl Contained {
    /// Wait for the leader to exit, kill what it started, then reap it. A
    /// descendant that holds the child's pipes open is killed too.
    pub(crate) async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        #[cfg(unix)]
        {
            if let Some(pid) = self.child.id() {
                // Seen unreaped, so the group number stays the leader's.
                while !crate::lifecycle::contain::exited_pid(pid, false)? {
                    self.sigchld.recv().await;
                }
            }
            self.held.release();
            self.child.wait().await
        }
        #[cfg(windows)]
        {
            let status = self.child.wait().await?;
            self.held.release();
            Ok(status)
        }
    }

    /// Kill the tree, then the child, and reap it.
    pub(crate) async fn kill(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.held.release();
        self.child.wait().await
    }
}

/// One contained child for a blocking caller, with its pipes. Like
/// [`Contained`] it owns the `Child`: only [`wait`](Self::wait),
/// [`kill`](Self::kill) and dropping reap it, each after the tree's kill, and
/// no caller is handed the child to reap first. `held` comes before `child`.
pub(crate) struct ContainedStd {
    pub(crate) stdin: Option<std::process::ChildStdin>,
    pub(crate) stdout: Option<std::process::ChildStdout>,
    pub(crate) stderr: Option<std::process::ChildStderr>,
    held: Held,
    child: std::process::Child,
    /// The status of the reap, once `wait` or `kill` has done it: the pid is free from then on, so nothing asks the
    /// kernel about it again.
    reaped: Option<std::process::ExitStatus>,
}

impl ContainedStd {
    /// Whether the leader has exited, seen unreaped (without freeing its pid
    /// on Unix); with `block` this waits for the exit.
    pub(crate) fn exited(&mut self, block: bool) -> std::io::Result<bool> {
        if self.reaped.is_some() {
            return Ok(true);
        }
        crate::lifecycle::contain::exited(&mut self.child, block)
    }

    /// Wait for the leader to exit, kill what it started, then reap it. A
    /// descendant that holds the child's pipes open is killed too. The tree
    /// is killed and the child reaped even if seeing the exit failed.
    pub(crate) fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if let Some(status) = self.reaped {
            return Ok(status);
        }
        let seen = self.exited(true);
        self.held.release();
        let status = self.child.wait();
        self.reaped = status.as_ref().ok().copied();
        seen?;
        status
    }

    /// Kill the tree, then the child, and reap it. Idempotent: once the child
    /// is reaped it touches neither the tree nor the child and returns the status.
    pub(crate) fn kill(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if let Some(status) = self.reaped {
            return Ok(status);
        }
        self.held.release();
        let status = self.child.wait();
        self.reaped = status.as_ref().ok().copied();
        status
    }

    /// [`wait`](Self::wait), bounded: polls for the leader's exit every 10 ms. At the exit it returns the status; when
    /// `bound` passes it kills the tree, reaps the child and returns `None`; on a probe error it does the same and
    /// returns the error.
    pub(crate) fn wait_within(&mut self, bound: std::time::Duration) -> std::io::Result<Option<std::process::ExitStatus>> {
        let deadline = std::time::Instant::now() + bound;
        loop {
            match self.exited(false) {
                Ok(true) => return self.wait().map(Some),
                Ok(false) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(10)),
                Ok(false) => {
                    let _ = self.kill();
                    return Ok(None);
                }
                Err(e) => {
                    let _ = self.kill();
                    return Err(e);
                }
            }
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for ContainedStd {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

/// Put `SIGCHLD` back to its default disposition. An inherited `SIG_IGN`
/// makes the kernel reap every child at its exit, so a contained leader
/// would not stay a zombie holding its pid, and the pid is its group's
/// number: the invariant is that a contained leader stays a zombie until its
/// tree is killed. It also unblocks `SIGCHLD` in the calling thread:
/// `Contained::wait` learns of an exit from the signal, and a mask the parent
/// blocked would hide it. `main` calls it on the main thread, which lives as
/// long as the daemon. Called first thing in the daemon's `main`.
#[cfg(unix)]
pub(crate) fn reset_child_signal() {
    // SAFETY: setting a disposition to SIG_DFL runs no handler code, and the mask call only edits this thread's mask
    // with a set built in place.
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGCHLD);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
    }
}

/// The daemon's one signal; every production owner is given this.
pub(crate) fn process() -> &'static Signal {
    static SIGNAL: OnceLock<Signal> = OnceLock::new();
    SIGNAL.get_or_init(Signal::new)
}

/// Fire the shutdown: every contained child's tree is killed. It stays fired for the life of the process.
pub(crate) fn fire() {
    process().fire()
}

/// Resolves once the shutdown has fired; at once if it already has.
pub(crate) async fn fired() {
    process().fired().await
}

/// Children whose [`Held`] is alive.
pub(crate) fn live_children() -> usize {
    process().live()
}

#[cfg(test)]
#[cfg(unix)]
pub(crate) mod tests {
    use super::*;
    use std::time::Duration;

    /// A pid is gone once a probe fails, polled for up to 3 s (where there is no pidfd to read).
    #[cfg(not(target_os = "linux"))]
    fn gone(pid: i32) -> bool {
        (0..150).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            // SAFETY: signal 0 only probes the pid.
            unsafe { libc::kill(pid, 0) != 0 }
        })
    }

    /// A process a test started that is a descendant of a child, so the test holds no handle to it. A leftover is
    /// built from its pid, or from a file the stub wrote the pid to, read when the pid is first needed; on Linux
    /// the process is opened then as a pidfd, and from then on its end is read from that identity and never from the
    /// number, which the OS may give to a stranger once init has reaped the process. The test must see it gone
    /// ([`gone`](Self::gone)), or say it is alive at the test's end by design ([`kept_alive`](Self::kept_alive)); a
    /// leftover dropped with neither fails the test, after ending the process through its identity, so a test that
    /// forgot to check the process it started cannot pass.
    pub(crate) struct Leftover {
        pid: std::cell::Cell<Option<i32>>,
        file: Option<std::path::PathBuf>,
        state: std::cell::Cell<Seen>,
        #[cfg(target_os = "linux")]
        identity: std::cell::RefCell<Option<std::os::fd::OwnedFd>>,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Seen {
        Unchecked,
        Gone,
        KeptAlive,
    }

    impl Leftover {
        pub(crate) fn of_pid(pid: i32) -> Self {
            let leftover = Self::empty(Some(pid), None);
            leftover.open_identity(pid);
            leftover
        }

        pub(crate) fn of_file(file: impl Into<std::path::PathBuf>) -> Self {
            Self::empty(None, Some(file.into()))
        }

        fn empty(pid: Option<i32>, file: Option<std::path::PathBuf>) -> Self {
            Leftover {
                pid: std::cell::Cell::new(pid),
                file,
                state: std::cell::Cell::new(Seen::Unchecked),
                #[cfg(target_os = "linux")]
                identity: std::cell::RefCell::new(None),
            }
        }

        /// Open the process as an identity while it is alive. A process already gone has none to open: it is seen gone.
        #[cfg(target_os = "linux")]
        fn open_identity(&self, pid: i32) {
            use std::os::fd::FromRawFd;
            // SAFETY: pidfd_open takes a pid and flags and returns a descriptor or -1.
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            if fd >= 0 {
                // SAFETY: a fresh descriptor nothing else owns.
                *self.identity.borrow_mut() = Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) });
            }
        }

        #[cfg(not(target_os = "linux"))]
        fn open_identity(&self, _pid: i32) {}

        fn pid(&self) -> Option<i32> {
            if let (None, Some(file)) = (self.pid.get(), &self.file) {
                let pid = std::fs::read_to_string(file).ok().and_then(|text| text.trim().parse().ok());
                self.pid.set(pid);
                if let Some(pid) = pid {
                    self.open_identity(pid);
                }
            }
            self.pid.get()
        }

        /// Whether the process is gone, polled for up to 3 s. On Linux this reads the identity (a pidfd readable at
        /// the exit, reaped or not); a process whose pid was never readable, or that was gone before it could be
        /// opened, counts as gone only in the second case.
        pub(crate) fn gone(&self) -> bool {
            let gone = match self.pid() {
                None => false,
                #[cfg(target_os = "linux")]
                Some(_) => self.identity.borrow().as_ref().map_or(true, |fd| {
                    use std::os::fd::AsRawFd;
                    let deadline = std::time::Instant::now() + Duration::from_secs(3);
                    loop {
                        let left = deadline.saturating_duration_since(std::time::Instant::now());
                        let mut pfd = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
                        // SAFETY: one valid pollfd; a pidfd is readable once its process has exited.
                        let rc = unsafe { libc::poll(&mut pfd, 1, left.as_millis() as libc::c_int) };
                        // A child's SIGCHLD interrupts the wait: it goes on for what is left of the bound.
                        if rc < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        break rc > 0;
                    }
                }),
                #[cfg(not(target_os = "linux"))]
                Some(pid) => gone(pid),
            };
            if gone {
                self.state.set(Seen::Gone);
            }
            gone
        }

        /// This process is alive at the test's end by design; the drop ends it.
        pub(crate) fn kept_alive(&self) {
            self.state.set(Seen::KeptAlive);
        }

        fn end(&self) {
            #[cfg(target_os = "linux")]
            if let Some(fd) = self.identity.borrow().as_ref() {
                use std::os::fd::AsRawFd;
                // SAFETY: pidfd_send_signal on a descriptor this value owns, with no siginfo.
                unsafe { libc::syscall(libc::SYS_pidfd_send_signal, fd.as_raw_fd(), libc::SIGKILL, 0, 0) };
                return;
            }
            #[cfg(not(target_os = "linux"))]
            if let Some(pid) = self.pid.get() {
                // SAFETY: a plain signal to a process this test started and has not seen gone.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }

    impl Drop for Leftover {
        fn drop(&mut self) {
            if self.state.get() == Seen::Gone {
                return;
            }
            let pid = self.pid();
            self.end();
            if self.state.get() == Seen::Unchecked && pid.is_some() && !std::thread::panicking() {
                panic!("a Leftover (pid {}) was dropped without its end observed: the test never checked that the process it started is gone", pid.unwrap_or_default());
            }
        }
    }

    /// A leftover records the process the test started, from the file the process wrote its own pid to, and holds
    /// the test to observing its end: one that is gone is seen gone, one dropped unobserved ends the test with a
    /// failure after the process is ended, and one declared alive at the end is ended quietly.
    #[test]
    fn leftover_records_its_child_and_requires_the_end_observation() {
        let dir = tempfile::tempdir().unwrap();
        let start = |name: &str| {
            let mut child = std::process::Command::new("sh")
                .args(["-c", "echo $$ > \"$1\"; exec sleep 3120", "sh"])
                .arg(dir.path().join(name))
                .spawn()
                .unwrap();
            let file = dir.path().join(name);
            let began = std::time::Instant::now();
            while std::fs::read_to_string(&file).map(|t| t.trim().is_empty()).unwrap_or(true) {
                assert!(began.elapsed() < Duration::from_secs(5), "the child never wrote its pid");
                std::thread::sleep(Duration::from_millis(10));
            }
            // The child's pid is its own until it is waited for: reading it from its file is what the leftovers do.
            assert!(child.try_wait().unwrap().is_none());
            (child, file)
        };

        // Observed gone: the child is killed and reaped, and the leftover reads the exit from its identity.
        let (mut child, file) = start("seen");
        let leftover = Leftover::of_file(file);
        assert!(leftover.pid().is_some());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(leftover.gone(), "a reaped child was not seen gone");
        drop(leftover);

        // Unobserved: the drop ends the process through its identity, then fails the test.
        let (mut child, file) = start("unseen");
        let leftover = Leftover::of_file(file);
        assert!(leftover.pid().is_some());
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(leftover)));
        assert!(failed.is_err(), "a leftover dropped without its end observed did not fail the test");
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&child.wait().unwrap()),
            Some(libc::SIGKILL),
            "the unobserved leftover's process was not ended"
        );

        // Alive at the end by design: ended quietly.
        let (mut child, file) = start("kept");
        let leftover = Leftover::of_file(file);
        assert!(leftover.pid().is_some());
        leftover.kept_alive();
        drop(leftover);
        assert_eq!(std::os::unix::process::ExitStatusExt::signal(&child.wait().unwrap()), Some(libc::SIGKILL));
    }

    /// An owner stuck writing to a child that never reads cannot poll the
    /// signal; the fire still takes the child and its grandchild.
    #[cfg(unix)]
    #[tokio::test]
    async fn fire_kills_a_tree_whose_owner_is_blocked() {
        use tokio::io::AsyncWriteExt;
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "sleep 3104 & echo $!; exec sleep 3104"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        let mut contained = signal.spawn(&mut cmd).expect("spawn");
        assert_eq!(signal.live(), 1);
        let mut line = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut tokio::io::BufReader::new(contained.stdout.take().unwrap()), &mut line)
            .await
            .unwrap();
        let grandchild = Leftover::of_pid(line.trim().parse().expect("grandchild pid"));
        let mut stdin = contained.stdin.take().unwrap();
        let owner = tokio::spawn(async move {
            let _ = stdin.write_all(&vec![0u8; 1 << 20]).await;
            let _ = contained.wait().await;
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        signal.fire();
        tokio::time::timeout(Duration::from_secs(3), owner)
            .await
            .expect("the blocked owner did not return")
            .expect("owner task");
        assert!(grandchild.gone(), "the grandchild survived the shutdown");
        assert_eq!(signal.live(), 0);
    }

    /// A child created while the signal fires is already counted, so the shutdown's wait for its children covers it,
    /// and it is killed when it registers. That the start returns at all shows the child died: its error path reaps
    /// it, and `sleep 3115` exits only if killed.
    #[cfg(unix)]
    #[test]
    fn a_child_created_while_the_signal_fires_is_counted_and_killed() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let counted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
        let seen = std::sync::Arc::clone(&counted);
        *signal.after_create.lock().unwrap() = Some(Box::new(move || {
            signal.fire();
            seen.store(signal.live(), Ordering::SeqCst);
        }));
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("3115").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        assert!(signal.spawn_std(&mut cmd).is_err(), "a child created while the signal fired was accepted");
        assert_eq!(counted.load(Ordering::SeqCst), 1, "a child in flight was not counted when the signal fired");
        assert_eq!(signal.live(), 0);
    }

    /// A start after the fire creates nothing.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_start_after_the_fire_creates_nothing() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        signal.fire();
        let created = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&created);
        *signal.after_create.lock().unwrap() = Some(Box::new(move || flag.store(true, Ordering::SeqCst)));
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("3105");
        assert!(signal.spawn(&mut cmd).is_err(), "a start after the fire was accepted");
        assert!(!created.load(Ordering::SeqCst), "a start after the fire created a process");
        assert_eq!(signal.live(), 0);
    }

    /// The leader's exit takes everything it started with it, before the
    /// leader is reaped, even though nothing fired the signal.
    #[cfg(unix)]
    #[tokio::test]
    async fn wait_kills_the_tree_at_the_leaders_exit() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "sleep 3107 >/dev/null 2>&1 & echo $!"]).stdout(std::process::Stdio::piped());
        let mut contained = signal.spawn(&mut cmd).expect("spawn");
        let mut line = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut tokio::io::BufReader::new(contained.stdout.take().unwrap()), &mut line)
            .await
            .unwrap();
        let descendant = Leftover::of_pid(line.trim().parse().expect("descendant pid"));
        contained.wait().await.expect("wait");
        assert!(descendant.gone(), "the leader's descendant survived its exit");
        assert!(signal.held_groups().is_empty(), "a reaped leader's tree is still held");
        drop(contained);
        assert_eq!(signal.live(), 0);
    }

    /// A blocking caller sees the exit unreaped: the leader's number is still
    /// its own and the tree is still held, until the caller waits; only then
    /// is the tree killed and the leader reaped.
    #[cfg(unix)]
    fn one_shot_takes_its_tree_after_it_exits() {
        use std::io::BufRead;
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "sleep 3107 >/dev/null 2>&1 & echo $!"]).stdout(std::process::Stdio::piped());
        let mut c = signal.spawn_std(&mut cmd).expect("spawn_std");
        let pgid = c.id() as i32;
        let mut line = String::new();
        std::io::BufReader::new(c.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let descendant = Leftover::of_pid(line.trim().parse().expect("descendant pid"));
        assert!(c.exited(true).expect("exited"), "the child had not exited");
        assert!(signal.held_groups().contains(&pgid), "the tree was released before its owner let go");
        // SAFETY: signal 0 only probes the pid.
        assert_eq!(unsafe { libc::kill(pgid, 0) }, 0, "the exit was seen by reaping it");
        c.wait().expect("wait");
        assert!(signal.held_groups().is_empty(), "a reaped leader's tree is still held");
        assert!(descendant.gone(), "the one-shot's descendant survived");
    }

    /// A one-shot's output is read to its end, and the tree dies after the
    /// leader's exit: a descendant the exit leaves behind does not survive,
    /// and nothing stays registered or counted.
    #[cfg(unix)]
    #[test]
    fn output_takes_the_tree_after_the_exit() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "sleep 3111 >/dev/null 2>&1 & echo $!"]);
        let out = signal.output(&mut cmd).expect("output");
        assert!(out.status.success());
        let descendant = Leftover::of_pid(String::from_utf8_lossy(&out.stdout).trim().parse().expect("descendant pid"));
        assert!(descendant.gone(), "the one-shot's descendant survived");
        assert!(signal.held_groups().is_empty(), "a reaped leader's tree is still held");
        assert_eq!(signal.live(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_one_shot_takes_its_tree_after_it_exits() {
        one_shot_takes_its_tree_after_it_exits();
    }

    /// A parent that started the daemon with `SIGCHLD` ignored must not make
    /// the kernel reap contained leaders; the startup reset undoes it. The
    /// disposition is process-wide, so the scenario runs in a re-executed copy
    /// of this test binary and no test in this process touches `SIGCHLD`.
    #[cfg(unix)]
    #[test]
    fn an_ignored_sigchld_is_reset_at_startup() {
        let name = "lifecycle::child_signal::tests::ignored_sigchld_scenario";
        let out = std::process::Command::new(std::env::current_exe().expect("current_exe"))
            .args(["--exact", name, "--test-threads=1", "--nocapture"])
            .env("SOT_TEST_SIGCHLD_CHILD", "1")
            .output()
            .expect("re-execute the test binary");
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "the child run failed:\n{text}");
        assert!(text.contains(&format!("test {name} ... ok")), "the scenario did not run:\n{text}");
    }

    /// The re-executed half of `an_ignored_sigchld_is_reset_at_startup`; it
    /// does nothing in an ordinary run.
    #[cfg(unix)]
    #[test]
    fn ignored_sigchld_scenario() {
        if std::env::var_os("SOT_TEST_SIGCHLD_CHILD").is_none() {
            return;
        }
        // SAFETY: a plain disposition change in a process of its own.
        unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
        reset_child_signal();
        one_shot_takes_its_tree_after_it_exits();
    }

    /// A parent that started the daemon with `SIGCHLD` blocked would hide every exit from `Contained::wait`, which
    /// learns of one from the signal; the startup reset unblocks it. The mask is the starting thread's and survives
    /// exec, so the scenario runs in a re-executed copy of this test binary whose parent blocked it.
    #[cfg(unix)]
    #[test]
    fn a_blocked_sigchld_is_unblocked_at_startup() {
        use std::os::unix::process::CommandExt;
        let name = "lifecycle::child_signal::tests::blocked_sigchld_scenario";
        let mut cmd = std::process::Command::new(std::env::current_exe().expect("current_exe"));
        cmd.args(["--exact", name, "--test-threads=1", "--nocapture"]).env("SOT_TEST_SIGCHLD_BLOCKED", "1");
        // SAFETY: only sigprocmask on a set built in place, which is async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, libc::SIGCHLD);
                if libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let out = cmd.output().expect("re-execute the test binary");
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "the child run failed:\n{text}");
        assert!(text.contains(&format!("test {name} ... ok")), "the scenario did not run:\n{text}");
    }

    /// The re-executed half of `a_blocked_sigchld_is_unblocked_at_startup`; it does nothing in an ordinary run.
    #[cfg(unix)]
    #[test]
    fn blocked_sigchld_scenario() {
        if std::env::var_os("SOT_TEST_SIGCHLD_BLOCKED").is_none() {
            return;
        }
        reset_child_signal();
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        rt.block_on(async {
            let mut cmd = tokio::process::Command::new("sleep");
            cmd.arg("1");
            let mut contained = signal.spawn(&mut cmd).expect("spawn");
            let status = tokio::time::timeout(Duration::from_secs(10), contained.wait())
                .await
                .expect("the wait never saw the leader's exit")
                .expect("wait");
            assert!(status.success());
        });
    }

    /// Once a `ContainedStd` has reaped its child it answers from the reap: the freed pid is never probed again.
    #[cfg(unix)]
    #[test]
    fn a_reaped_child_answers_from_its_reap() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("3112");
        let mut c = signal.spawn_std(&mut cmd).expect("spawn_std");
        let status = c.kill().expect("kill");
        assert!(c.exited(false).expect("exited after the reap"), "a reaped child is not seen as exited");
        assert_eq!(c.wait().expect("wait after the reap"), status);
        assert_eq!(c.kill().expect("kill after the reap"), status);
    }

    /// A leader that moved to another process group is outside its tree's group kill, so the fire must take it by
    /// pid as well. A killed leader stays a zombie until its owner reaps it, so "gone" is seen as an exit, unreaped.
    #[cfg(unix)]
    #[test]
    fn a_leader_that_left_its_group_dies_at_the_fire() {
        use std::os::unix::process::CommandExt;
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        // SAFETY: a plain read of this process's own group.
        let theirs = unsafe { libc::getpgrp() };
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("3114").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        // SAFETY: only setpgid, which is async-signal-safe.
        unsafe {
            cmd.pre_exec(move || if libc::setpgid(0, theirs) == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) });
        }
        let mut c = signal.spawn_std(&mut cmd).expect("spawn_std");
        signal.fire();
        let began = std::time::Instant::now();
        let mut exited = false;
        while !exited && began.elapsed() < Duration::from_secs(3) {
            exited = c.exited(false).expect("exited");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(exited, "a leader that left its group survived the fire");
        drop(c);
    }

    /// A bounded wait returns the status of a child that exits in time.
    #[cfg(unix)]
    #[test]
    fn wait_within_returns_a_prompt_exit() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "exit 3"]);
        let mut c = signal.spawn_std(&mut cmd).expect("spawn_std");
        let status = c.wait_within(Duration::from_secs(10)).expect("wait_within").expect("the child exited in time");
        assert_eq!(status.code(), Some(3));
    }

    /// A child past its bound is ended with its tree, reaped, and counted out.
    #[cfg(unix)]
    #[test]
    fn wait_within_ends_a_child_past_its_bound() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("3113");
        let mut c = signal.spawn_std(&mut cmd).expect("spawn_std");
        assert!(c.wait_within(Duration::from_millis(200)).expect("wait_within").is_none(), "a child past its bound was waited for");
        assert!(signal.held_groups().is_empty(), "the tree is still held");
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&c.wait().expect("the reaped status")),
            Some(libc::SIGKILL),
            "the child past its bound was not killed"
        );
        drop(c);
        assert_eq!(signal.live(), 0);
    }
}
