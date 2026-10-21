//! The permanent child signal and synchronized creation/registration of contained trees.
//! Unix requests precede reap; Windows wait can obtain child status first while retaining the job. Cleanup errors propagate; Drop logs them.
//! Fire checks requests without waiting for tree death.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use tokio::sync::watch;

/// One shutdown signal and the trees it requests termination for. The process has one ([`process`]).
pub(crate) struct Signal {
    fired: watch::Sender<bool>,
    /// Every contained tree, protected by the same mutex held during creation and registration. Failed
    /// requests retain ownership for a later checked cleanup; the permanent flag still refuses new starts.
    trees: Mutex<Option<HashMap<u64, crate::lifecycle::contain::Tree>>>,
    next: AtomicU64,
    /// Set by the first controlled exit that reaches the terminal ([`claim_exit`](Self::claim_exit)).
    exit_claimed: std::sync::atomic::AtomicBool,
    /// Test-only: contained children whose owner still holds them (a [`Held`] not yet dropped).
    #[cfg(test)]
    live: std::sync::atomic::AtomicUsize,
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
            live: std::sync::atomic::AtomicUsize::new(0),
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

    /// Test-only: contained children whose owner still holds them.
    #[cfg(test)]
    pub(crate) fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
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
        let child = {
            #[cfg(unix)]
            let _fork = sot_platform::host::fd::fork_guard();
            cmd.spawn()?
        };
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
        let child = {
            #[cfg(unix)]
            let _fork = sot_platform::host::fd::fork_guard();
            cmd.spawn()?
        };
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
    /// `ContainedStd::wait_within` observes exit and requests tree termination before
    /// joining both readers. Unix observes the leader unreaped; Windows observation
    /// uses child.wait() before the job request, retaining the job handle.
    /// A child that has not exited within `within` has its tree killed, and the call is a `TimedOut` error saying so.
    pub(crate) fn output(
        &'static self,
        cmd: &mut std::process::Command,
        within: std::time::Duration,
    ) -> std::io::Result<std::process::Output> {
        use std::process::Stdio;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = self.spawn_std(cmd)?;
        let stdout = read_to_end_on_a_thread(child.stdout.take());
        let stderr = read_to_end_on_a_thread(child.stderr.take());
        let status = child.wait_within(within);
        let (stdout, stderr) = (stdout.join(), stderr.join());
        let status = status?.ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, format!("did not exit within {within:?}"))
        })?;
        Ok(std::process::Output {
            status,
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
        #[cfg(test)]
        sig.live.fetch_add(1, Ordering::SeqCst);
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
        #[cfg(test)]
        self.sig.live.fetch_sub(1, Ordering::SeqCst);
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
    /// Wait for the leader to exit without releasing its tree or reaping it: the child's identity and containment
    /// stay owned, so a caller can close requests and revoke grants before the checked [`wait`](Self::wait) or
    /// [`kill`](Self::kill) takes the tree. Cancelling it loses nothing; a later call observes the same exit.
    pub(crate) async fn wait_until_exited(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            if let Some(pid) = self.child.id() {
                // Seen unreaped, so the group number stays the leader's.
                while !crate::lifecycle::contain::exited_pid(pid, false)? {
                    self.sigchld.recv().await;
                }
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            self.child.wait().await.map(|_| ())
        }
    }

    /// Unix observes exit unreaped, requests tree termination, then reaps; Windows waits for child status first, then
    /// requests job termination. Request failures remain errors; successful requests do not establish descendant death.
    pub(crate) async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.wait_until_exited().await?;
        self.held.release()?;
        self.child.wait().await
    }

    /// Request tree termination before reaping. A failed request returns before a blocking reap.
    pub(crate) async fn kill(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.held.release()?;
        self.child.wait().await
    }

    /// Ask the child to interrupt what it runs (`contain::interrupt`); a child already reaped is not asked.
    pub(crate) fn interrupt(&self) -> std::io::Result<()> {
        match self.child.id() {
            Some(pid) => crate::lifecycle::contain::interrupt(pid),
            None => Ok(()),
        }
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

#[cfg(test)]
#[cfg(unix)]
#[path = "child_signal_tests.rs"]
pub(crate) mod tests;

/// A contained one-shot that has not exited at its bound is an error naming the bound, and its tree is killed.
#[cfg(all(test, unix))]
#[path = "child_signal_output_bound_tests.rs"]
mod output_bound_tests;
