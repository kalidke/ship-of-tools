//! guard.rs — the Linux lifetime guard. Every serving daemon is the child of a guard that is a subreaper: a process
//! the daemon starts, at any depth, stays a descendant of the guard through `setsid`, `detach` or a double fork, and
//! every orphan in that subtree comes to the guard. When the daemon ends, the guard kills its own children until it has
//! none, then ends the way the daemon did.
//!
//! Two kernel rules carry it: an orphan goes to its nearest living subreaper ancestor, and an unreaped child's pid is
//! never reused. The guard is a process, not a thread: it is forked before the runtime and any thread exist, and it
//! shares nothing with the daemon but the standard descriptors.

use std::io;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// How long the drain may take before it logs what is left and goes on to exit.
pub const DRAIN_BOUND: Duration = Duration::from_secs(10);

/// The guard's pid in the daemon, 0 in no guard.
static GUARD_PID: AtomicU32 = AtomicU32::new(0);

/// The guard that supervises this daemon, once [`install`] has run in it.
pub fn guard_pid() -> Option<u32> {
    match GUARD_PID.load(Ordering::SeqCst) {
        0 => None,
        pid => Some(pid),
    }
}

/// The number of threads in this process.
fn thread_count() -> io::Result<usize> {
    Ok(std::fs::read_dir("/proc/self/task")?.count())
}

fn errno() -> io::Error {
    io::Error::last_os_error()
}

/// Fork the guard. Returns in the daemon (the child); the guard (the parent) never returns. An error means no guard
/// exists and the boot must stop.
pub fn install() -> io::Result<()> {
    let threads = thread_count()?;
    if threads != 1 {
        return Err(io::Error::other(format!("the lifetime guard needs a single-threaded process, and this one has {threads} threads")));
    }
    // An inherited SIG_IGN on SIGCHLD would let the kernel reap the guard's children and void the drain's pid safety.
    // SAFETY: plain signal-disposition and mask calls over locally owned values, in a single-threaded process.
    let (saved, guard) = unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
        let mut saved: libc::sigset_t = std::mem::zeroed();
        let mut all: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&mut all);
        if libc::sigprocmask(libc::SIG_SETMASK, &all, &mut saved) != 0 {
            return Err(errno());
        }
        if libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0 {
            return Err(errno());
        }
        (saved, libc::getpid())
    };
    // SAFETY: this process has one thread, so the child may run ordinary Rust code after the fork.
    #[allow(
        clippy::disallowed_methods,
        reason = "the lifetime guard forks before any thread exists"
    )]
    let pid = unsafe { libc::fork() };
    match pid {
        -1 => Err(errno()),
        0 => {
            // The daemon. Its end is the guard's cue; the guard's end is its own: it dies at once.
            // SAFETY: restoring the saved mask, asking for the death signal, and a self-kill when the guard is already gone.
            unsafe {
                libc::sigprocmask(libc::SIG_SETMASK, &saved, std::ptr::null_mut());
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
                if libc::getppid() != guard {
                    libc::raise(libc::SIGKILL);
                }
            }
            GUARD_PID.store(guard as u32, Ordering::SeqCst);
            Ok(())
        }
        daemon => keep_guard(daemon),
    }
}

/// The guard's life. Never returns.
fn keep_guard(daemon: libc::pid_t) -> ! {
    // The guard holds no channel, socket, lock or log of the daemon's.
    close_descriptors_above_stderr();
    // SAFETY: the name is a NUL-terminated literal.
    unsafe { libc::prctl(libc::PR_SET_NAME, c"sotd-guard".as_ptr(), 0, 0, 0) };
    let status = wait_for_daemon(daemon);
    drain();
    exit_as(status)
}

fn close_descriptors_above_stderr() {
    // SAFETY: close_range closes descriptors this process owns; a kernel without it (before 5.9) answers ENOSYS and the
    // fallback below closes what /proc lists.
    let rc = unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) };
    if rc == 0 {
        return;
    }
    let fds: Vec<i32> = std::fs::read_dir("/proc/self/fd")
        .map(|dir| {
            dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    for fd in fds.into_iter().filter(|fd| *fd > 2) {
        // SAFETY: closing a descriptor of this process; the directory's own is already gone or closes harmlessly.
        unsafe { libc::close(fd) };
    }
}

/// Read the blocked signals until the daemon is reaped; forward all but SIGCHLD to the daemon. Returns its wait status.
fn wait_for_daemon(daemon: libc::pid_t) -> libc::c_int {
    // SAFETY: a signalfd over every signal the guard blocks (all but the two that cannot be blocked).
    let sfd = unsafe {
        let mut all: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&mut all);
        libc::signalfd(-1, &all, libc::SFD_CLOEXEC)
    };
    loop {
        if let Some(status) = reap_daemon(daemon) {
            return status;
        }
        let mut info = std::mem::MaybeUninit::<libc::signalfd_siginfo>::zeroed();
        // SAFETY: one read of one signalfd_siginfo into a buffer of that size, from the signalfd just made.
        let n = unsafe {
            libc::read(
                sfd,
                info.as_mut_ptr().cast(),
                std::mem::size_of::<libc::signalfd_siginfo>(),
            )
        };
        if n != std::mem::size_of::<libc::signalfd_siginfo>() as isize {
            continue;
        }
        // SAFETY: the read filled the whole structure.
        let signo = unsafe { info.assume_init() }.ssi_signo as libc::c_int;
        if signo != libc::SIGCHLD {
            // SAFETY: the daemon is this guard's unreaped child, so its pid is still its own.
            unsafe { libc::kill(daemon, signo) };
        }
    }
}

/// Reap every child that has ended; the daemon's status when it is among them.
fn reap_daemon(daemon: libc::pid_t) -> Option<libc::c_int> {
    let mut found = None;
    loop {
        let mut status = 0;
        // SAFETY: a non-blocking wait on any child of this process.
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid <= 0 {
            return found;
        }
        if pid == daemon {
            found = Some(status);
        }
    }
}

/// The pids whose parent is this process, from `/proc`.
fn own_children() -> Vec<i32> {
    // SAFETY: a plain read of this process's own pid.
    let me = unsafe { libc::getpid() };
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|entry| {
        let pid: i32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The fields after the command name: state, then the parent.
        let ppid: i32 = stat
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        (ppid == me).then_some(pid)
    })
    .collect()
}

/// Kill and reap this process's children until it has none, within [`DRAIN_BOUND`]. Only the guard reaps its children and
/// it reaps none between listing and killing, so a listed pid is still the child it was.
fn drain() {
    let deadline = Instant::now() + DRAIN_BOUND;
    loop {
        reap_all();
        let children = own_children();
        if children.is_empty() && no_children_left() {
            return;
        }
        for pid in &children {
            // SAFETY: the pid is an unreaped child of this process (see above).
            unsafe { libc::kill(*pid, libc::SIGKILL) };
        }
        if Instant::now() >= deadline {
            for pid in children {
                let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
                eprintln!("sotd-guard: pid {pid} ({}) did not end within {DRAIN_BOUND:?}; it has SIGKILL pending", name.trim());
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn reap_all() {
    loop {
        // SAFETY: a non-blocking wait on any child of this process.
        let pid = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
        if pid <= 0 {
            return;
        }
    }
}

/// Whether `waitpid` says there is no child at all (ECHILD).
fn no_children_left() -> bool {
    // SAFETY: a non-blocking wait on any child of this process.
    let rc = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
    rc < 0 && errno().raw_os_error() == Some(libc::ECHILD)
}

/// Leave the way the daemon did: its exit code, or its signal with no core and the default disposition.
fn exit_as(status: libc::c_int) -> ! {
    if libc::WIFEXITED(status) {
        // SAFETY: an immediate exit of the guard; it holds nothing to flush.
        unsafe { libc::_exit(libc::WEXITSTATUS(status)) };
    }
    let signo = libc::WTERMSIG(status);
    // SAFETY: a core limit of zero, the default disposition for the signal, the signal unblocked and raised; if it
    // returns (an ignored default), the guard exits with the conventional status.
    unsafe {
        let none = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        libc::setrlimit(libc::RLIMIT_CORE, &none);
        libc::signal(signo, libc::SIG_DFL);
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, signo);
        libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        libc::raise(signo);
        libc::_exit(128 + signo)
    }
}
