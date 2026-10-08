//! The permanent child signal and synchronized creation/registration of contained trees.
//! Unix requests precede reap; Windows wait can obtain child status first while retaining the job. Cleanup errors propagate; Drop logs them.
//! Fire checks requests without waiting for tree death.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use tokio::sync::watch;

/// One shutdown signal and the trees it requests termination for. The process has one ([`fire`],
/// [`fired`], [`process`]).
pub(crate) struct Signal {
    fired: watch::Sender<bool>,
    /// Every contained tree, protected by the same mutex held during creation and registration. Failed
    /// requests retain ownership for a later checked cleanup; the permanent flag still refuses new starts.
    trees: Mutex<Option<HashMap<u64, crate::lifecycle::contain::Tree>>>,
    next: AtomicU64,
    /// Set by the first controlled exit that reaches the terminal ([`claim_exit`](Self::claim_exit)).
    exit_claimed: std::sync::atomic::AtomicBool,
    /// Test-only: called right after a child is created, to put the shutdown's fire in that window.
    #[cfg(test)]
    pub(super) after_create: Mutex<Option<Box<dyn FnMut(u32) + Send>>>,
    #[cfg(test)]
    pub(crate) after_adopt: Mutex<Option<Box<dyn FnMut(u32) + Send>>>,
}

impl Signal {
    pub(crate) fn new() -> Self {
        Signal {
            fired: watch::channel(false).0,
            trees: Mutex::new(Some(HashMap::new())),
            next: AtomicU64::new(0),
            exit_claimed: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            after_create: Mutex::new(None),
            #[cfg(test)]
            after_adopt: Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn after_create(&self, pid: u32) {
        if let Some(hook) = self
            .after_create
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            hook(pid);
        }
    }

    /// Publish fire, then attempt every registered tree under the start/registry mutex. Successful requests
    /// do not establish death. A stalled OS creation/adoption can delay acquiring this mutex.
    pub(crate) fn fire(&self) -> std::io::Result<()> {
        self.fired.send_replace(true);
        let mut trees = self.trees.lock().unwrap_or_else(|e| e.into_inner());
        let mut failures = Vec::new();
        if let Some(map) = trees.as_mut() {
            map.retain(|id, tree| match tree.terminate() {
                Ok(()) => false,
                Err(error) => {
                    tracing::error!(tree = id, %error, "child fire: termination request failed");
                    failures.push(format!("tree {id}: {error}"));
                    true
                }
            });
            if map.is_empty() {
                *trees = None;
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(std::io::Error::other(failures.join("; ")))
        }
    }

    /// Whether the caller is the first controlled exit (true once, then false): the one whose code the process exits with.
    pub(crate) fn claim_exit(&self) -> bool {
        !self.exit_claimed.swap(true, Ordering::SeqCst)
    }

    pub(crate) fn is_fired(&self) -> bool {
        *self.fired.borrow()
    }

    pub(crate) async fn fired(&self) {
        let mut rx = self.fired.subscribe();
        let _ = rx.wait_for(|fired| *fired).await;
    }

    /// The process-group number of every tree still held.
    #[cfg(all(test, unix))]
    pub(crate) fn held_groups(&self) -> Vec<i32> {
        let trees = self.trees.lock().unwrap_or_else(|e| e.into_inner());
        trees
            .as_ref()
            .map(|map| map.values().map(|t| t.pgid()).collect())
            .unwrap_or_default()
    }

    /// Hold the registry mutex from before creation through adoption and registration. Once fire is published,
    /// even a reservation waiting on this mutex is refused before creating anything.
    fn reserve(
        &'static self,
    ) -> std::io::Result<MutexGuard<'static, Option<HashMap<u64, crate::lifecycle::contain::Tree>>>>
    {
        let trees = self.trees.lock().unwrap_or_else(|e| e.into_inner());
        if self.is_fired() || trees.is_none() {
            return Err(std::io::Error::other("the daemon is shutting down"));
        }
        Ok(trees)
    }

    /// Creation through registration shares fire's mutex. The provisional owner cleans up an error or unwind
    /// without reacquiring it; the guard is released before returning or awaiting.
    pub(crate) fn spawn(
        &'static self,
        cmd: &mut tokio::process::Command,
    ) -> std::io::Result<Contained> {
        let mut trees = self.reserve()?;
        #[cfg(unix)]
        let sigchld = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
        crate::lifecycle::contain::prepare(cmd.as_std_mut());
        cmd.kill_on_drop(true);
        #[allow(
            clippy::disallowed_methods,
            reason = "the containment's own start: reserve and contain::prepare ran before it, and adopt and fill follow (ADR 0050, Shutdown)"
        )]
        let child = cmd.spawn()?;
        let mut provisional = Provisional::new(child);
        #[cfg(test)]
        self.after_create(
            provisional
                .child
                .as_ref()
                .and_then(PartialChild::pid)
                .expect("created identity"),
        );
        provisional.adopt()?;
        #[cfg(test)]
        if let Some(hook) = self
            .after_adopt
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            hook(
                provisional
                    .child
                    .as_ref()
                    .and_then(PartialChild::pid)
                    .expect("created identity"),
            );
        }
        let held = Held::fill(
            self,
            &mut trees,
            provisional.tree.take().expect("adopted tree"),
        );
        let mut child = provisional.child.take().expect("created child");
        drop(trees);
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

    /// Blocking creation uses the same mutex and provisional ownership, with no guard crossing an await.
    pub(crate) fn spawn_std(
        &'static self,
        cmd: &mut std::process::Command,
    ) -> std::io::Result<ContainedStd> {
        let mut trees = self.reserve()?;
        crate::lifecycle::contain::prepare(cmd);
        #[allow(
            clippy::disallowed_methods,
            reason = "the containment's own start: reserve and contain::prepare ran before it, and adopt and fill follow (ADR 0050, Shutdown)"
        )]
        let child = cmd.spawn()?;
        let mut provisional = Provisional::new(child);
        #[cfg(test)]
        self.after_create(
            provisional
                .child
                .as_ref()
                .and_then(PartialChild::pid)
                .expect("created identity"),
        );
        provisional.adopt()?;
        #[cfg(test)]
        if let Some(hook) = self
            .after_adopt
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            hook(
                provisional
                    .child
                    .as_ref()
                    .and_then(PartialChild::pid)
                    .expect("created identity"),
            );
        }
        let held = Held::fill(
            self,
            &mut trees,
            provisional.tree.take().expect("adopted tree"),
        );
        let mut child = provisional.child.take().expect("created child");
        drop(trees);
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
    /// and stderr are each read to their end on a thread of their own.
    /// `ContainedStd::wait` observes exit and requests tree termination before
    /// joining both readers. Unix observes the leader unreaped; Windows observation
    /// uses child.wait() before the job request, retaining the job handle.
    /// No bound is added, as `output` has none.
    pub(crate) fn output(
        &'static self,
        cmd: &mut std::process::Command,
    ) -> std::io::Result<std::process::Output> {
        use std::process::Stdio;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = self.spawn_std(cmd)?;
        let stdout = read_to_end_on_a_thread(child.stdout.take());
        let stderr = read_to_end_on_a_thread(child.stderr.take());
        let status = child.wait();
        let (stdout, stderr) = (stdout.join(), stderr.join());
        Ok(std::process::Output {
            status: status?,
            stdout: joined(stdout)?,
            stderr: joined(stderr)?,
        })
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

/// Immediately owns a created child before adoption, including unwind of the post-create hook.
struct Provisional<C: PartialChild> {
    tree: Option<crate::lifecycle::contain::Tree>,
    child: Option<C>,
}

impl<C: PartialChild> Provisional<C> {
    fn new(child: C) -> Self {
        Self {
            child: Some(child),
            tree: None,
        }
    }

    fn adopt(&mut self) -> std::io::Result<()> {
        let child = self.child.as_ref().expect("created child");
        match crate::lifecycle::contain::adopt(
            child.pid(),
            #[cfg(windows)]
            child.handle(),
        ) {
            Ok(tree) => {
                self.tree = Some(tree);
                Ok(())
            }
            Err(error) => Err(crate::lifecycle::contain::combine(error, self.cleanup())),
        }
    }

    fn cleanup(&mut self) -> std::io::Result<()> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        #[cfg(unix)]
        if self.tree.is_none() {
            self.tree = child.pid().map(crate::lifecycle::contain::partial);
        }
        let request = match self.tree.as_mut() {
            Some(tree) => tree.terminate(),
            None => child.terminate(),
        };
        request?;
        child.reap()
    }
}

impl<C: PartialChild> Drop for Provisional<C> {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            tracing::error!(%error, "partial child start cleanup failed");
        }
    }
}

trait PartialChild {
    fn pid(&self) -> Option<u32>;
    #[cfg(windows)]
    fn handle(&self) -> Option<std::os::windows::io::RawHandle>;
    fn terminate(&mut self) -> std::io::Result<()>;
    fn reap(&mut self) -> std::io::Result<()>;
}

impl PartialChild for std::process::Child {
    fn pid(&self) -> Option<u32> {
        Some(self.id())
    }
    #[cfg(windows)]
    fn handle(&self) -> Option<std::os::windows::io::RawHandle> {
        Some(std::os::windows::io::AsRawHandle::as_raw_handle(self))
    }
    fn terminate(&mut self) -> std::io::Result<()> {
        self.kill()
    }
    fn reap(&mut self) -> std::io::Result<()> {
        crate::lifecycle::contain::reap(self).map(|_| ())
    }
}

impl PartialChild for tokio::process::Child {
    fn pid(&self) -> Option<u32> {
        self.id()
    }
    #[cfg(windows)]
    fn handle(&self) -> Option<std::os::windows::io::RawHandle> {
        self.raw_handle()
    }
    fn terminate(&mut self) -> std::io::Result<()> {
        self.start_kill()
    }
    // Drop transfers the unreaped child to Tokio's orphan queue; no synchronous async reap is promised.
    fn reap(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One registered tree. Release requests termination under the registry mutex; Unix requires an unreaped leader, Windows a retained job.
struct Held {
    sig: &'static Signal,
    id: u64,
}

impl Held {
    /// Fill the map the reservation already holds; no recursive mutex acquisition on start failure or unwind.
    fn fill(
        sig: &'static Signal,
        trees: &mut Option<HashMap<u64, crate::lifecycle::contain::Tree>>,
        tree: crate::lifecycle::contain::Tree,
    ) -> Self {
        let id = sig.next.fetch_add(1, Ordering::SeqCst);
        trees
            .as_mut()
            .expect("reservation holds an open registry")
            .insert(id, tree);
        Self { sig, id }
    }

    fn release(&self) -> std::io::Result<()> {
        let mut trees = self.sig.trees.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(map) = trees.as_mut() {
            if let Some(tree) = map.get_mut(&self.id) {
                tree.terminate()?;
            }
            map.remove(&self.id);
        }
        Ok(())
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Err(error) = self.release() {
            tracing::error!(%error, "contained owner Drop: termination request failed");
            // Final retries happen while this owner still holds the unreaped child. Never retain a failed tree
            // after the async child can enter Tokio's reaper and free its Unix group identity.
            let mut trees = self.sig.trees.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(map) = trees.as_mut() {
                map.remove(&self.id);
            }
        }
    }
}

/// One contained child with its pipes. It owns the `Child`. Unix wait requests termination before reap;
/// Windows wait obtains the child status first, retaining the job for its subsequent termination request.
/// Kill requests termination before waiting. Drop logs failures and transfers the child to Tokio's orphan queue.
/// `held` drops before `child`; Drop does not synchronously reap.
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
    /// Unix observes exit unreaped, requests tree termination, then reaps; Windows waits for child status first, then requests job termination.
    /// Request failures remain errors; successful requests do not establish descendant death.
    pub(crate) async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        #[cfg(unix)]
        {
            if let Some(pid) = self.child.id() {
                // Seen unreaped, so the group number stays the leader's.
                while !crate::lifecycle::contain::exited_pid(pid, false)? {
                    self.sigchld.recv().await;
                }
            }
            self.held.release()?;
            self.child.wait().await
        }
        #[cfg(windows)]
        {
            let status = self.child.wait().await?;
            self.held.release()?;
            Ok(status)
        }
    }

    /// Request tree termination before reaping. A failed request returns before a blocking reap.
    pub(crate) async fn kill(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.held.release()?;
        self.child.wait().await
    }
}

/// One contained child for a blocking caller, owning its `Child` and pipes.
/// Unix wait observes exit unreaped before requesting termination; Windows exit observation uses wait/try_wait first.
/// Windows retains the job handle for the subsequent request. Kill and Drop request termination before waiting.
/// No caller is handed the child to reap. `held` comes before `child`.
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
    /// Whether the leader has exited; with `block` this waits. Unix observes without reaping;
    /// Windows uses wait/try_wait while the containment's job handle remains retained.
    pub(crate) fn exited(&mut self, block: bool) -> std::io::Result<bool> {
        if self.reaped.is_some() {
            return Ok(true);
        }
        crate::lifecycle::contain::exited(&mut self.child, block)
    }

    /// Observe exit, request tree termination and return child status. Unix observation leaves the leader unreaped;
    /// Windows observation uses child.wait() first, retaining the job for the request. Successful requests do not prove descendant death.
    /// Probe and cleanup failures remain errors; a failed request prevents the subsequent reap call.
    pub(crate) fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if let Some(status) = self.reaped {
            return Ok(status);
        }
        let seen = self.exited(true);
        if let Err(error) = self.held.release() {
            return Err(match seen {
                Err(probe) => crate::lifecycle::contain::combine(probe, Err(error)),
                Ok(_) => error,
            });
        }
        let status = crate::lifecycle::contain::reap(&mut self.child);
        self.reaped = status.as_ref().ok().copied();
        if let Err(error) = seen {
            return Err(crate::lifecycle::contain::combine(
                error,
                status.map(|_| ()),
            ));
        }
        status
    }

    /// Request tree termination before reaping. A failed request returns before a blocking reap. Idempotent: once the child
    /// is reaped it touches neither the tree nor the child and returns the status.
    pub(crate) fn kill(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if let Some(status) = self.reaped {
            return Ok(status);
        }
        self.held.release()?;
        let status = crate::lifecycle::contain::reap(&mut self.child);
        self.reaped = status.as_ref().ok().copied();
        status
    }

    /// Bound waiting for a live leader, polling every 10 ms. Timeout returns `Ok(None)` only after successful
    /// tree requests and a confirmed direct-child reap. Probe/request/reap errors remain errors, preserving both
    /// probe and cleanup reasons. OS termination/reap has no wall-clock ceiling; descendant death is not promised.
    pub(crate) fn wait_within(
        &mut self,
        bound: std::time::Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        let deadline = std::time::Instant::now() + bound;
        loop {
            match self.exited(false) {
                Ok(true) => return self.wait().map(Some),
                Ok(false) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                Ok(false) => {
                    self.kill()?;
                    return Ok(None);
                }
                Err(e) => {
                    return Err(crate::lifecycle::contain::combine(
                        e,
                        self.kill().map(|_| ()),
                    ));
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn confirmed_reaped(&self) -> bool {
        self.reaped.is_some()
    }

    #[cfg(test)]
    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for ContainedStd {
    fn drop(&mut self) {
        if let Err(error) = self.kill() {
            tracing::error!(%error, "blocking contained child Drop cleanup failed");
        }
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

/// Resolves once the shutdown has fired; at once if it already has.
pub(crate) async fn fired() {
    process().fired().await
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
    /// leftover dropped with neither fails the test, so a test that forgot to check the process it started cannot pass.
    /// A leftover only watches: the pid came from a file a process wrote or from a descendant, not from this test's own
    /// spawn, so the test never signals it. What a failing run leaves alive ends with the fixture's own bound (each stub
    /// ends itself) or with the container the suite runs in.
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
                *self.identity.borrow_mut() =
                    Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) });
            }
        }

        #[cfg(not(target_os = "linux"))]
        fn open_identity(&self, _pid: i32) {}

        fn pid(&self) -> Option<i32> {
            if let (None, Some(file)) = (self.pid.get(), &self.file) {
                let pid = std::fs::read_to_string(file)
                    .ok()
                    .and_then(|text| text.trim().parse().ok());
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
                        let mut pfd = libc::pollfd {
                            fd: fd.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        };
                        // SAFETY: one valid pollfd; a pidfd is readable once its process has exited.
                        let rc =
                            unsafe { libc::poll(&mut pfd, 1, left.as_millis() as libc::c_int) };
                        // A child's SIGCHLD interrupts the wait: it goes on for what is left of the bound.
                        if rc < 0
                            && std::io::Error::last_os_error().kind()
                                == std::io::ErrorKind::Interrupted
                        {
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

        /// This process is alive at the test's end by design: its stub ends itself.
        pub(crate) fn kept_alive(&self) {
            self.state.set(Seen::KeptAlive);
        }
    }

    impl Drop for Leftover {
        fn drop(&mut self) {
            if self.state.get() == Seen::Gone {
                return;
            }
            let pid = self.pid();
            if self.state.get() == Seen::Unchecked && pid.is_some() && !std::thread::panicking() {
                panic!("a Leftover (pid {}) was dropped without its end observed: the test never checked that the process it started is gone", pid.unwrap_or_default());
            }
        }
    }

    /// A leftover records the process the test started, from the file the process wrote its own pid to, and holds
    /// the test to observing its end: one that is gone is seen gone, one dropped unobserved ends the test with a
    /// failure, and one declared alive at the end is left alone. Nothing here signals the process.
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
            while std::fs::read_to_string(&file)
                .map(|t| t.trim().is_empty())
                .unwrap_or(true)
            {
                assert!(
                    began.elapsed() < Duration::from_secs(5),
                    "the child never wrote its pid"
                );
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

        // Unobserved: the drop fails the test and signals nothing; the test ends its own child.
        let (mut child, file) = start("unseen");
        let leftover = Leftover::of_file(file);
        assert!(leftover.pid().is_some());
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(leftover)));
        assert!(
            failed.is_err(),
            "a leftover dropped without its end observed did not fail the test"
        );
        assert!(
            child.try_wait().unwrap().is_none(),
            "the unobserved leftover's process was signalled"
        );
        child.kill().unwrap();
        child.wait().unwrap();

        // Alive at the end by design: dropped quietly, and not signalled.
        let (mut child, file) = start("kept");
        let leftover = Leftover::of_file(file);
        assert!(leftover.pid().is_some());
        leftover.kept_alive();
        drop(leftover);
        assert!(
            child.try_wait().unwrap().is_none(),
            "the kept-alive leftover's process was signalled"
        );
        child.kill().unwrap();
        child.wait().unwrap();
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
        assert_eq!(signal.held_groups().len(), 1);
        let mut line = String::new();
        tokio::io::AsyncBufReadExt::read_line(
            &mut tokio::io::BufReader::new(contained.stdout.take().unwrap()),
            &mut line,
        )
        .await
        .unwrap();
        let grandchild = Leftover::of_pid(line.trim().parse().expect("grandchild pid"));
        let mut stdin = contained.stdin.take().unwrap();
        let owner = tokio::spawn(async move {
            let _ = stdin.write_all(&vec![0u8; 1 << 20]).await;
            let _ = contained.wait().await;
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        signal.fire().expect("fire");
        tokio::time::timeout(Duration::from_secs(3), owner)
            .await
            .expect("the blocked owner did not return")
            .expect("owner task");
        assert!(grandchild.gone(), "the grandchild survived the shutdown");
        assert!(signal.held_groups().is_empty());
    }

    /// A start after the fire creates nothing.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_start_after_the_fire_creates_nothing() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        signal.fire().expect("fire");
        let created = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&created);
        *signal.after_create.lock().unwrap() =
            Some(Box::new(move |_| flag.store(true, Ordering::SeqCst)));
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("3105");
        assert!(
            signal.spawn(&mut cmd).is_err(),
            "a start after the fire was accepted"
        );
        assert!(
            !created.load(Ordering::SeqCst),
            "a start after the fire created a process"
        );
        assert!(signal.held_groups().is_empty());
    }

    /// The leader's exit takes everything it started with it, before the
    /// leader is reaped, even though nothing fired the signal.
    #[cfg(unix)]
    #[tokio::test]
    async fn wait_kills_the_tree_at_the_leaders_exit() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "sleep 3107 >/dev/null 2>&1 & echo $!"])
            .stdout(std::process::Stdio::piped());
        let mut contained = signal.spawn(&mut cmd).expect("spawn");
        let mut line = String::new();
        tokio::io::AsyncBufReadExt::read_line(
            &mut tokio::io::BufReader::new(contained.stdout.take().unwrap()),
            &mut line,
        )
        .await
        .unwrap();
        let descendant = Leftover::of_pid(line.trim().parse().expect("descendant pid"));
        contained.wait().await.expect("wait");
        assert!(
            descendant.gone(),
            "the leader's descendant survived its exit"
        );
        assert!(
            signal.held_groups().is_empty(),
            "a reaped leader's tree is still held"
        );
        drop(contained);
        assert!(signal.held_groups().is_empty());
    }

    /// A blocking caller sees the exit unreaped: the leader's number is still
    /// its own and the tree is still held, until the caller waits; only then
    /// is the tree killed and the leader reaped.
    #[cfg(unix)]
    fn one_shot_takes_its_tree_after_it_exits() {
        use std::io::BufRead;
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "sleep 3107 >/dev/null 2>&1 & echo $!"])
            .stdout(std::process::Stdio::piped());
        let mut c = signal.spawn_std(&mut cmd).expect("spawn_std");
        let pgid = c.id() as i32;
        let mut line = String::new();
        std::io::BufReader::new(c.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let descendant = Leftover::of_pid(line.trim().parse().expect("descendant pid"));
        assert!(c.exited(true).expect("exited"), "the child had not exited");
        assert!(
            signal.held_groups().contains(&pgid),
            "the tree was released before its owner let go"
        );
        // SAFETY: signal 0 only probes the pid.
        assert_eq!(
            unsafe { libc::kill(pgid, 0) },
            0,
            "the exit was seen by reaping it"
        );
        c.wait().expect("wait");
        assert!(
            signal.held_groups().is_empty(),
            "a reaped leader's tree is still held"
        );
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
        let descendant = Leftover::of_pid(
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse()
                .expect("descendant pid"),
        );
        assert!(descendant.gone(), "the one-shot's descendant survived");
        assert!(
            signal.held_groups().is_empty(),
            "a reaped leader's tree is still held"
        );
        assert!(signal.held_groups().is_empty());
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
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "the child run failed:\n{text}");
        assert!(
            text.contains(&format!("test {name} ... ok")),
            "the scenario did not run:\n{text}"
        );
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
        cmd.args(["--exact", name, "--test-threads=1", "--nocapture"])
            .env("SOT_TEST_SIGCHLD_BLOCKED", "1");
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
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "the child run failed:\n{text}");
        assert!(
            text.contains(&format!("test {name} ... ok")),
            "the scenario did not run:\n{text}"
        );
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
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
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
        assert!(
            c.exited(false).expect("exited after the reap"),
            "a reaped child is not seen as exited"
        );
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
        cmd.arg("3114")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // SAFETY: only setpgid, which is async-signal-safe.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setpgid(0, theirs) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        let mut c = signal.spawn_std(&mut cmd).expect("spawn_std");
        signal.fire().expect("fire");
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
        let status = c
            .wait_within(Duration::from_secs(10))
            .expect("wait_within")
            .expect("the child exited in time");
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
        assert!(
            c.wait_within(Duration::from_millis(200))
                .expect("wait_within")
                .is_none(),
            "a child past its bound was waited for"
        );
        assert!(signal.held_groups().is_empty(), "the tree is still held");
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&c.wait().expect("the reaped status")),
            Some(libc::SIGKILL),
            "the child past its bound was not killed"
        );
        drop(c);
        assert!(signal.held_groups().is_empty());
    }
}
