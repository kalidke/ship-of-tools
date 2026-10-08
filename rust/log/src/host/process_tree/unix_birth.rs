//! unix_birth.rs — the Rust side of the two-phase launcher (`native_birth.c`).
//!
//! [`Launch::begin`] resolves everything in the unforked parent (argv, the complete environment, the stdio and
//! working-directory descriptors) and calls the native `sot_birth_begin`, which forks and returns at once. The
//! caller owns the result ([`Birth`]): the child's pid, whose parent is the caller and so whose wait only the
//! caller can do, the gate's write end and the two status pipes. The child sits at its gate until
//! [`Birth::release`] says GO; [`Birth::cancel`], a closed gate or a drop ends it before the target runs. The
//! native branch runs no Rust: see `native_birth.c` for exactly what it runs.

use std::ffi::{CString, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The byte the gate takes for GO; any other byte, or EOF, is a cancel.
pub const GATE_GO_BYTE: u8 = b'G';

/// The stage a native status record names, in the order the child passes them (`enum sot_birth_stage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Close,
    Signals,
    Session,
    Group,
    Stdio,
    Cwd,
    Inherit,
    Ready,
    Gate,
    Exec,
    Unknown(u32),
}

impl Stage {
    fn from_raw(raw: u32) -> Stage {
        match raw {
            1 => Stage::Close,
            2 => Stage::Signals,
            3 => Stage::Session,
            4 => Stage::Group,
            5 => Stage::Stdio,
            6 => Stage::Cwd,
            7 => Stage::Inherit,
            8 => Stage::Ready,
            9 => Stage::Gate,
            10 => Stage::Exec,
            other => Stage::Unknown(other),
        }
    }
}

/// A launch step that failed in the child, with the errno the child saw.
#[derive(Debug)]
pub struct BirthError {
    pub stage: Stage,
    pub errno: i32,
}

impl std::fmt::Display for BirthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the child failed at {:?}: {}",
            self.stage,
            io::Error::from_raw_os_error(self.errno)
        )
    }
}

impl std::error::Error for BirthError {}

/// The child's report at its gate: it is set up and has not run the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ready {
    pub pid: i32,
    pub pgid: i32,
    pub sid: i32,
}

/// Which process group the child is in when it reaches the gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// The caller's group.
    Leave,
    /// A group of its own, numbered by the child's pid.
    Own,
    /// An existing group.
    Join(i32),
}

/// A test barrier in the native branch (`enum sot_birth_pause`): the child writes one byte on `out` when it reaches
/// `stage`, then waits for one byte on `go`. Built only with the `native-barrier` feature.
#[cfg(feature = "native-barrier")]
pub struct Pause {
    pub stage: u32,
    pub out: OwnedFd,
    pub go: OwnedFd,
}

#[cfg(feature = "native-barrier")]
pub const PAUSE_BEFORE_CLOSE: u32 = 1;
#[cfg(feature = "native-barrier")]
pub const PAUSE_BEFORE_SESSION: u32 = 2;
#[cfg(feature = "native-barrier")]
pub const PAUSE_BEFORE_READY: u32 = 3;

#[repr(C)]
struct SotLaunch {
    abi: u32,
    path: *const libc::c_char,
    argv: *const *const libc::c_char,
    envp: *const *const libc::c_char,
    cwd_fd: libc::c_int,
    stdio_fds: [libc::c_int; 3],
    close_fds: *const libc::c_int,
    n_close: libc::size_t,
    inherit_fds: *const libc::c_int,
    n_inherit: libc::size_t,
    new_session: libc::c_int,
    join_pgid: libc::pid_t,
    pause_stage: libc::c_int,
    pause_out_fd: libc::c_int,
    pause_in_fd: libc::c_int,
}

#[repr(C)]
struct SotRecord {
    stage: u32,
    err: i32,
    pid: i32,
    pgid: i32,
    sid: i32,
}

#[repr(C)]
struct SotBirth {
    pid: libc::pid_t,
    gate_fd: libc::c_int,
    ready_fd: libc::c_int,
    error_fd: libc::c_int,
}

extern "C" {
    fn sot_birth_begin(launch: *const SotLaunch, out: *mut SotBirth) -> libc::c_int;
    fn sot_birth_release(birth: *mut SotBirth) -> libc::c_int;
    fn sot_birth_cancel(birth: *mut SotBirth) -> libc::c_int;
}

const ABI: u32 = 1;

/// Everything the child needs, fixed in the parent before the fork.
pub struct Launch {
    path: PathBuf,
    argv: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    cwd: Option<OwnedFd>,
    stdio: [Option<OwnedFd>; 3],
    close: Vec<RawFd>,
    inherit: Vec<RawFd>,
    new_session: bool,
    group: Group,
    #[cfg(feature = "native-barrier")]
    pause: Option<Pause>,
}

impl Launch {
    /// A launch of the absolute `path` with `argv[0]` its file name and the complete environment of the caller.
    pub fn new(path: impl Into<PathBuf>) -> Launch {
        let path = path.into();
        let name = path
            .file_name()
            .map(OsString::from)
            .unwrap_or_else(|| path.clone().into_os_string());
        Launch {
            path,
            argv: vec![name],
            env: std::env::vars_os().collect(),
            cwd: None,
            stdio: [None, None, None],
            close: Vec::new(),
            inherit: Vec::new(),
            new_session: false,
            group: Group::Leave,
            #[cfg(feature = "native-barrier")]
            pause: None,
        }
    }

    pub fn arg(&mut self, arg: impl Into<OsString>) -> &mut Self {
        self.argv.push(arg.into());
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.argv.extend(args.into_iter().map(Into::into));
        self
    }

    /// Replace the environment with exactly `env`.
    pub fn env_clear(&mut self) -> &mut Self {
        self.env.clear();
        self
    }

    pub fn env(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> &mut Self {
        let key = key.into();
        self.env.retain(|(k, _)| *k != key);
        self.env.push((key, value.into()));
        self
    }

    /// The directory the child enters, opened by the caller.
    pub fn cwd(&mut self, dir: OwnedFd) -> &mut Self {
        self.cwd = Some(dir);
        self
    }

    /// The descriptor that becomes the child's standard input (0), output (1) or error (2); unset is `/dev/null`.
    pub fn stdio(&mut self, index: usize, fd: OwnedFd) -> &mut Self {
        self.stdio[index] = Some(fd);
        self
    }

    /// An endpoint that belongs to the caller only: the child closes it before anything else runs.
    pub fn close_in_child(&mut self, fd: RawFd) -> &mut Self {
        self.close.push(fd);
        self
    }

    /// A descriptor that stays open across the target's exec (its close-on-exec flag is cleared in the child).
    pub fn inherit_across_exec(&mut self, fd: RawFd) -> &mut Self {
        self.inherit.push(fd);
        self
    }

    pub fn new_session(&mut self, yes: bool) -> &mut Self {
        self.new_session = yes;
        self
    }

    pub fn group(&mut self, group: Group) -> &mut Self {
        self.group = group;
        self
    }

    #[cfg(feature = "native-barrier")]
    pub fn pause(&mut self, pause: Pause) -> &mut Self {
        self.pause = Some(pause);
        self
    }

    /// Fork the child and return its owner at once; the target has not run.
    pub fn begin(self) -> io::Result<Birth> {
        let path = cstring(self.path.as_os_str().as_bytes())?;
        let argv = self
            .argv
            .iter()
            .map(|a| cstring(a.as_bytes()))
            .collect::<io::Result<Vec<_>>>()?;
        let env = self
            .env
            .iter()
            .map(|(k, v)| {
                let mut kv = k.as_bytes().to_vec();
                kv.push(b'=');
                kv.extend_from_slice(v.as_bytes());
                cstring(&kv)
            })
            .collect::<io::Result<Vec<_>>>()?;
        let argv_ptrs: Vec<*const libc::c_char> = argv
            .iter()
            .map(|a| a.as_ptr())
            .chain([std::ptr::null()])
            .collect();
        let env_ptrs: Vec<*const libc::c_char> = env
            .iter()
            .map(|e| e.as_ptr())
            .chain([std::ptr::null()])
            .collect();

        // Every descriptor the child dup2s or reads is moved above 2 first, so none overlaps a standard one.
        let cwd = self.cwd.map(high_fd).transpose()?;
        let mut stdio_fds = [-1; 3];
        let mut stdio_keep = Vec::new();
        for (slot, fd) in self.stdio.into_iter().enumerate() {
            if let Some(fd) = fd {
                let fd = high_fd(fd)?;
                stdio_fds[slot] = fd.as_raw_fd();
                stdio_keep.push(fd);
            }
        }
        #[cfg(feature = "native-barrier")]
        let (pause_stage, pause_out, pause_in) = match self.pause {
            Some(p) => (
                p.stage as libc::c_int,
                Some(high_fd(p.out)?),
                Some(high_fd(p.go)?),
            ),
            None => (0, None, None),
        };
        #[cfg(not(feature = "native-barrier"))]
        let (pause_stage, pause_out, pause_in): (
            libc::c_int,
            Option<OwnedFd>,
            Option<OwnedFd>,
        ) = (0, None, None);

        let launch = SotLaunch {
            abi: ABI,
            path: path.as_ptr(),
            argv: argv_ptrs.as_ptr(),
            envp: env_ptrs.as_ptr(),
            cwd_fd: cwd.as_ref().map_or(-1, |f| f.as_raw_fd()),
            stdio_fds,
            close_fds: self.close.as_ptr(),
            n_close: self.close.len(),
            inherit_fds: self.inherit.as_ptr(),
            n_inherit: self.inherit.len(),
            new_session: self.new_session as libc::c_int,
            join_pgid: match self.group {
                Group::Leave => 0,
                Group::Own => -1,
                Group::Join(pgid) => pgid,
            },
            pause_stage,
            pause_out_fd: pause_out.as_ref().map_or(-1, |f| f.as_raw_fd()),
            pause_in_fd: pause_in.as_ref().map_or(-1, |f| f.as_raw_fd()),
        };
        let mut out = SotBirth {
            pid: 0,
            gate_fd: -1,
            ready_fd: -1,
            error_fd: -1,
        };
        // SAFETY: `launch` and everything it points to (the C strings, the pointer arrays, the descriptor lists)
        // outlive the call; `out` is a valid out-parameter. The native function forks and returns in the parent.
        let rc = unsafe { sot_birth_begin(&launch, &mut out) };
        drop((cwd, stdio_keep, pause_out, pause_in));
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc));
        }
        // SAFETY: on success the three descriptors are open, valid and owned by nothing else.
        let (gate, ready, error) = unsafe {
            (
                OwnedFd::from_raw_fd(out.gate_fd),
                OwnedFd::from_raw_fd(out.ready_fd),
                OwnedFd::from_raw_fd(out.error_fd),
            )
        };
        Ok(Birth {
            pid: out.pid,
            gate: Some(gate),
            ready,
            error,
            reaped: false,
        })
    }
}

fn cstring(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "an interior NUL in a launch string",
        )
    })
}

/// `fd` as a descriptor numbered above the standard three.
fn high_fd(fd: OwnedFd) -> io::Result<OwnedFd> {
    if fd.as_raw_fd() > 2 {
        return Ok(fd);
    }
    // SAFETY: a plain duplicate to a number of 3 or more, close-on-exec.
    let dup = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if dup < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `dup` is a fresh descriptor nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(dup) })
}

/// The owned, provisional child of a [`Launch`]: alive, set up or setting up, and not running its target. Its parent
/// is the process that began it, so no one else can reap it and its pid cannot be reused before [`wait`](Self::wait)
/// or the drop. Dropping a birth that was not released, or whose target still runs, ends the child and reaps it.
pub struct Birth {
    pid: i32,
    gate: Option<OwnedFd>,
    ready: OwnedFd,
    error: OwnedFd,
    reaped: bool,
}

impl Birth {
    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// Wait for the child's report that it is set up (or for the step that failed), at most `bound`.
    pub fn ready(&mut self, bound: Duration) -> io::Result<Ready> {
        match read_record(&self.ready, bound)? {
            Some(rec) if Stage::from_raw(rec.stage) == Stage::Ready => Ok(Ready {
                pid: rec.pid,
                pgid: rec.pgid,
                sid: rec.sid,
            }),
            Some(rec) => Err(io::Error::other(BirthError {
                stage: Stage::from_raw(rec.stage),
                errno: rec.err,
            })),
            None => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the child ended before it was ready",
            )),
        }
    }

    /// Say GO: the child execs the target. The gate is spent.
    pub fn release(&mut self) -> io::Result<()> {
        self.spend(sot_birth_release_fn)
    }

    /// Say CANCEL: the child exits before the target runs. The gate is spent.
    pub fn cancel(&mut self) -> io::Result<()> {
        self.spend(sot_birth_cancel_fn)
    }

    fn spend(&mut self, f: fn(*mut SotBirth) -> libc::c_int) -> io::Result<()> {
        let gate = self.gate.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "the gate is already spent")
        })?;
        let mut native = SotBirth {
            pid: self.pid,
            gate_fd: gate.into_raw_fd(),
            ready_fd: -1,
            error_fd: -1,
        };
        // `native` holds the gate's descriptor, which the native call writes to and closes.
        let rc = f(&mut native);
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc));
        }
        Ok(())
    }

    /// After [`release`](Self::release): `Ok(())` once the exec has happened (the error pipe closed with nothing on
    /// it), or the step that failed. At most `bound`.
    pub fn exec_result(&mut self, bound: Duration) -> io::Result<()> {
        match read_record(&self.error, bound)? {
            None => Ok(()),
            Some(rec) => Err(io::Error::other(BirthError {
                stage: Stage::from_raw(rec.stage),
                errno: rec.err,
            })),
        }
    }

    /// Whether the child has exited, seen without reaping it, so its pid stays its own. With `block`, waits.
    pub fn exited(&self, block: bool) -> io::Result<bool> {
        if self.reaped {
            return Ok(true);
        }
        let options = libc::WEXITED | libc::WNOWAIT | if block { 0 } else { libc::WNOHANG };
        loop {
            // SAFETY: a zeroed siginfo_t is a valid out-parameter; waitid only writes it.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: a wait on one child of this process with WNOWAIT, which reaps nothing.
            let rc =
                unsafe { libc::waitid(libc::P_PID, self.pid as libc::id_t, &mut info, options) };
            if rc == 0 {
                return Ok(info.si_signo == libc::SIGCHLD);
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }

    /// Reap the child: blocks until it has exited.
    pub fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        use std::os::unix::process::ExitStatusExt;
        let mut status = 0;
        loop {
            // SAFETY: a plain wait on one child of this process.
            let rc = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if rc == self.pid {
                self.reaped = true;
                return Ok(std::process::ExitStatus::from_raw(status));
            }
            let err = io::Error::last_os_error();
            if rc < 0 && err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }

    /// A signal to the child, while it is unreaped (so its pid is still its own).
    pub fn signal(&self, signal: i32) -> io::Result<()> {
        if self.reaped {
            return Ok(());
        }
        // SAFETY: the pid is this process's own unreaped child.
        if unsafe { libc::kill(self.pid, signal) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl Drop for Birth {
    fn drop(&mut self) {
        if !self.reaped {
            self.gate = None; // a closed gate is a cancel
            let _ = self.signal(libc::SIGKILL);
            let _ = self.wait();
        }
    }
}

fn sot_birth_release_fn(b: *mut SotBirth) -> libc::c_int {
    // SAFETY: forwards to the native call with the caller's valid pointer.
    unsafe { sot_birth_release(b) }
}

fn sot_birth_cancel_fn(b: *mut SotBirth) -> libc::c_int {
    // SAFETY: forwards to the native call with the caller's valid pointer.
    unsafe { sot_birth_cancel(b) }
}

/// One fixed-size record from a status pipe, or `None` at EOF with nothing read. At most `bound`.
fn read_record(fd: &OwnedFd, bound: Duration) -> io::Result<Option<SotRecord>> {
    let want = std::mem::size_of::<SotRecord>();
    let mut buf = vec![0u8; want];
    let mut have = 0;
    let deadline = Instant::now() + bound;
    while have < want {
        let left = deadline.saturating_duration_since(Instant::now());
        let mut pfd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd; the timeout is clamped to what poll accepts.
        let rc = unsafe {
            libc::poll(
                &mut pfd,
                1,
                left.as_millis().min(i32::MAX as u128) as libc::c_int,
            )
        };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if rc == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the child's status pipe stayed silent past its bound",
            ));
        }
        // SAFETY: a read into the unfilled part of `buf` from a descriptor this function borrows.
        let n = unsafe { libc::read(fd.as_raw_fd(), buf[have..].as_mut_ptr().cast(), want - have) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            if have == 0 {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "a torn status record",
            ));
        }
        have += n as usize;
    }
    // SAFETY: `buf` holds exactly one record's bytes; the struct is plain integers.
    Ok(Some(unsafe {
        std::ptr::read_unaligned(buf.as_ptr().cast::<SotRecord>())
    }))
}
