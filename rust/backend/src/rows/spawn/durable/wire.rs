//! The private channel between the daemon and its durable parent: one socketpair, length-prefixed JSON messages, and
//! descriptors passed with `SCM_RIGHTS`. Nothing here is a daemon wire op: only the daemon and the parent it started
//! hold the two ends.

use serde::{Deserialize, Serialize};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;

/// The largest message either side accepts (an environment is the bulk of a launch).
pub const MAX_MESSAGE: usize = 4 << 20;
/// The most descriptors one message carries (a launch carries one: the supervisor's stderr).
const MAX_FDS: usize = 4;

/// What the daemon asks of the parent.
#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// Claim the row's fence and fork the supervisor, held at its gate. Carries one descriptor: its stderr.
    Launch(LaunchSpec),
    /// The daemon has published the birth: let the target run.
    Release { id: u64 },
    /// The daemon will not publish the birth: end it before the target runs.
    Cancel { id: u64 },
}

/// One capsule launch, complete: the parent adds only the two descriptor flags the supervisor takes over its claim with.
#[derive(Debug, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub id: u64,
    /// The row's capsule state folder; the parent creates it, claims its `supervisor.lock` and tells the supervisor.
    pub state_dir: PathBuf,
    /// The absolute program: `systemd-run` on Linux when a scope is granted, else `sot-capsule`.
    pub program: Vec<u8>,
    pub args: Vec<Vec<u8>>,
    /// Where in `args` the parent inserts `--claim-fd <n> --takeover-fd <n>`: just after the supervisor's mode flag.
    pub inject_at: usize,
    /// The complete environment; the parent starts from nothing.
    pub env: Vec<(Vec<u8>, Vec<u8>)>,
    pub cwd: PathBuf,
}

/// What the parent tells the daemon.
#[derive(Debug, Serialize, Deserialize)]
pub enum Reply {
    /// The parent is up (first message), with the control group it runs in (empty where there is none).
    Hello { pid: u32, cgroup: String },
    /// The row's fence is held by another claim or authority: nothing was accepted and nothing was forked.
    Contended { id: u64 },
    /// The launch was refused before any child was left alive.
    Failed { id: u64, text: String },
    /// The launch is accepted: the claim is held, the child is forked and set up, and its target waits at the gate.
    Born {
        id: u64,
        pid: i32,
        pgid: i32,
        sid: i32,
    },
    /// The gate is open and the target has exec'd.
    Released { id: u64 },
    /// The target could not be exec'd (the child is gone).
    ExecFailed { id: u64, text: String },
    /// The supervisor ended: its exit code, or the signal that ended it.
    Exited {
        id: u64,
        code: Option<i32>,
        signal: Option<i32>,
    },
}

/// One end of the socketpair.
pub struct Channel(OwnedFd);

/// A received message: its payload and the descriptors that came with it.
pub struct Message {
    pub payload: Vec<u8>,
    pub fds: Vec<OwnedFd>,
}

impl Channel {
    pub fn pair() -> io::Result<(Channel, Channel)> {
        let mut fds = [0 as libc::c_int; 2];
        #[cfg(target_os = "linux")]
        let kind = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
        #[cfg(not(target_os = "linux"))]
        let kind = libc::SOCK_STREAM;
        // SAFETY: socketpair fills the two-element array on success.
        if unsafe { libc::socketpair(libc::AF_UNIX, kind, 0, fds.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: two fresh descriptors nothing else owns.
        let (a, b) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        #[cfg(not(target_os = "linux"))]
        for fd in [&a, &b] {
            set_cloexec(fd.as_raw_fd())?;
        }
        Ok((Channel(a), Channel(b)))
    }

    pub fn from_owned(fd: OwnedFd) -> Channel {
        Channel(fd)
    }

    pub fn into_owned(self) -> OwnedFd {
        self.0
    }

    pub fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }

    pub fn try_clone(&self) -> io::Result<Channel> {
        self.0.try_clone().map(Channel)
    }

    /// Send one message, with `fds` attached to its first bytes. Blocks until all of it is written.
    pub fn send(&self, payload: &[u8], fds: &[RawFd]) -> io::Result<()> {
        if payload.len() > MAX_MESSAGE || fds.len() > MAX_FDS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a message past the channel's bounds",
            ));
        }
        let mut buf = Vec::with_capacity(4 + payload.len());
        buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        buf.extend_from_slice(payload);
        let mut sent = 0;
        while sent < buf.len() {
            let attach = if sent == 0 { fds } else { &[] };
            sent += self.sendmsg(&buf[sent..], attach)?;
        }
        Ok(())
    }

    fn sendmsg(&self, bytes: &[u8], fds: &[RawFd]) -> io::Result<usize> {
        let mut iov = libc::iovec {
            iov_base: bytes.as_ptr() as *mut libc::c_void,
            iov_len: bytes.len(),
        };
        // SAFETY: a zeroed msghdr is valid; the fields set below are the ones sendmsg reads.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        let space = if fds.is_empty() {
            0
        } else {
            cmsg_space(fds.len())
        };
        let mut control = vec![0u64; space.div_ceil(8)];
        if !fds.is_empty() {
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = space as _;
            // SAFETY: the control buffer is `space` bytes, aligned for a cmsghdr, and holds exactly one header.
            unsafe {
                let header = libc::CMSG_FIRSTHDR(&msg);
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as _;
                std::ptr::copy_nonoverlapping(
                    fds.as_ptr(),
                    libc::CMSG_DATA(header).cast::<RawFd>(),
                    fds.len(),
                );
            }
        }
        #[cfg(target_os = "linux")]
        let flags = libc::MSG_NOSIGNAL;
        #[cfg(not(target_os = "linux"))]
        let flags = 0;
        loop {
            // SAFETY: `msg` and everything it points to outlive the call.
            let n = unsafe { libc::sendmsg(self.0.as_raw_fd(), &msg, flags) };
            if n >= 0 {
                return Ok(n as usize);
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }

    /// Receive one message, blocking. `Ok(None)` at EOF between messages.
    pub fn recv(&self) -> io::Result<Option<Message>> {
        let mut header = [0u8; 4];
        let mut fds = Vec::new();
        let mut have = 0;
        while have < header.len() {
            match self.recvmsg(&mut header[have..], &mut fds)? {
                0 if have == 0 => return Ok(None),
                0 => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "a torn message header",
                    ))
                }
                n => have += n,
            }
        }
        let len = u32::from_be_bytes(header) as usize;
        if len > MAX_MESSAGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a message past the channel's bounds",
            ));
        }
        let mut payload = vec![0u8; len];
        let mut have = 0;
        while have < len {
            match self.recvmsg(&mut payload[have..], &mut fds)? {
                0 => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "a torn message",
                    ))
                }
                n => have += n,
            }
        }
        Ok(Some(Message { payload, fds }))
    }

    fn recvmsg(&self, into: &mut [u8], fds: &mut Vec<OwnedFd>) -> io::Result<usize> {
        let mut iov = libc::iovec {
            iov_base: into.as_mut_ptr().cast(),
            iov_len: into.len(),
        };
        let mut control = vec![0u64; cmsg_space(MAX_FDS).div_ceil(8)];
        // SAFETY: a zeroed msghdr is valid; the fields set below are the ones recvmsg reads.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = cmsg_space(MAX_FDS) as _;
        #[cfg(target_os = "linux")]
        let flags = libc::MSG_CMSG_CLOEXEC;
        #[cfg(not(target_os = "linux"))]
        let flags = 0;
        let n = loop {
            // SAFETY: `msg` and everything it points to outlive the call.
            let n = unsafe { libc::recvmsg(self.0.as_raw_fd(), &mut msg, flags) };
            if n >= 0 {
                break n as usize;
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        };
        // SAFETY: the kernel filled `msg`'s control buffer; each header it holds is walked with the CMSG macros, and a
        // descriptor in an SCM_RIGHTS payload is a fresh one this process owns.
        unsafe {
            let mut header = libc::CMSG_FIRSTHDR(&msg);
            while !header.is_null() {
                if (*header).cmsg_level == libc::SOL_SOCKET
                    && (*header).cmsg_type == libc::SCM_RIGHTS
                {
                    let count = ((*header).cmsg_len as usize - libc::CMSG_LEN(0) as usize)
                        / std::mem::size_of::<RawFd>();
                    let data = libc::CMSG_DATA(header).cast::<RawFd>();
                    for i in 0..count {
                        let fd = std::ptr::read_unaligned(data.add(i));
                        #[cfg(not(target_os = "linux"))]
                        let _ = set_cloexec(fd);
                        fds.push(OwnedFd::from_raw_fd(fd));
                    }
                }
                header = libc::CMSG_NXTHDR(&msg, header);
            }
        }
        if msg.msg_flags & libc::MSG_CTRUNC != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "descriptors were cut off a message",
            ));
        }
        Ok(n)
    }

    /// Whether a message (or EOF) is waiting, within `bound`.
    pub fn readable(&self, bound: std::time::Duration) -> io::Result<bool> {
        poll_readable(self.0.as_raw_fd(), bound)
    }
}

fn cmsg_space(count: usize) -> usize {
    // SAFETY: a pure size computation.
    unsafe { libc::CMSG_SPACE((count * std::mem::size_of::<RawFd>()) as u32) as usize }
}

/// Whether `fd` is readable (data or EOF), within `bound`.
pub fn poll_readable(fd: RawFd, bound: std::time::Duration) -> io::Result<bool> {
    let deadline = std::time::Instant::now() + bound;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let mut pfd = libc::pollfd {
            fd,
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
        if rc > 0 {
            return Ok(true);
        }
        if rc == 0 {
            return Ok(false);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn set_cloexec(fd: RawFd) -> io::Result<()> {
    // SAFETY: a plain flag change on a descriptor the caller owns.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Encode a message.
pub fn encode<T: Serialize>(message: &T) -> io::Result<Vec<u8>> {
    serde_json::to_vec(message).map_err(io::Error::other)
}

/// Decode a message.
pub fn decode<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> io::Result<T> {
    serde_json::from_slice(payload).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, Write};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Clone)]
    struct Phase(Arc<Mutex<&'static str>>);

    impl Phase {
        fn set(&self, phase: &'static str) {
            *self.0.lock().unwrap_or_else(|e| e.into_inner()) = phase;
        }

        fn name(&self) -> &'static str {
            match self.0.try_lock() {
                Ok(phase) => *phase,
                Err(std::sync::TryLockError::Poisoned(error)) => *error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => "phase snapshot busy",
            }
        }
    }

    struct Worker {
        ended: mpsc::Receiver<(Instant, Result<(), String>)>,
        complete: bool,
    }

    struct Exchange {
        deadline: Instant,
        // A receiving-end duplicate can unblock both ends without keeping the sender alive at EOF.
        shutdown: Channel,
        phase: Phase,
        workers: Vec<Worker>,
    }

    impl Exchange {
        fn new(bound: Duration) -> (Self, Channel, Channel) {
            let deadline = Instant::now() + bound;
            let (a, b) = constrained_pair();
            let shutdown = b
                .try_clone()
                .expect("duplicate only the owned receiving endpoint");
            (
                Self {
                    deadline,
                    shutdown,
                    phase: Phase(Arc::new(Mutex::new("setup"))),
                    workers: Vec::new(),
                },
                a,
                b,
            )
        }

        fn spawn(&mut self, body: impl FnOnce() + Send + 'static) {
            let (sent, ended) = mpsc::channel();
            std::thread::spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).map_err(
                    |payload| {
                        payload
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| payload.downcast_ref::<&str>().map(|text| text.to_string()))
                            .unwrap_or_else(|| "worker panic without text".into())
                    },
                );
                let _ = sent.send((Instant::now(), result));
            });
            self.workers.push(Worker {
                ended,
                complete: false,
            });
        }

        fn finish(mut self) -> Result<(), String> {
            let mut failure = None;
            for worker in &mut self.workers {
                let left = self.deadline.saturating_duration_since(Instant::now());
                match worker.ended.recv_timeout(left) {
                    Ok((ended, result)) => {
                        worker.complete = true;
                        // A completion queued after expiry cannot become green if this owner was descheduled.
                        if ended > self.deadline {
                            failure = Some(format!(
                                "wire exchange deadline phase={}",
                                self.phase.name()
                            ));
                            break;
                        }
                        if let Err(text) = result {
                            failure = Some(format!(
                                "wire exchange failed phase={}: {text}",
                                self.phase.name()
                            ));
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        failure = Some(format!(
                            "wire exchange deadline phase={}",
                            self.phase.name()
                        ));
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        worker.complete = true;
                        failure = Some(format!("wire worker lost phase={}", self.phase.name()));
                        break;
                    }
                }
            }
            match failure {
                None => Ok(()),
                Some(text) => Err(format!("{text}; {}", self.cleanup())),
            }
        }

        fn cleanup(&mut self) -> String {
            // SAFETY: this duplicate belongs to this exchange and refers only to its receiving socket.
            let result = unsafe { libc::shutdown(self.shutdown.as_raw_fd(), libc::SHUT_RDWR) };
            let error = (result < 0).then(io::Error::last_os_error);
            let deadline = Instant::now() + Duration::from_secs(2);
            for worker in &mut self.workers {
                if !worker.complete {
                    worker.complete = match worker
                        .ended
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    {
                        Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => true,
                        Err(mpsc::RecvTimeoutError::Timeout) => false,
                    };
                }
            }
            if error.is_none() && self.workers.iter().all(|worker| worker.complete) {
                "cleanup=confirmed".into()
            } else {
                format!("cleanup=unconfirmed shutdown={error:?}")
            }
        }
    }

    fn constrained_pair() -> (Channel, Channel) {
        let (a, b) = Channel::pair().expect("wire fixture socketpair");
        #[cfg(target_os = "linux")]
        for channel in [&a, &b] {
            for option in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
                let bytes: libc::c_int = 8192;
                // SAFETY: a live owned socket and a correctly sized integer option value.
                let result = unsafe {
                    libc::setsockopt(
                        channel.as_raw_fd(),
                        libc::SOL_SOCKET,
                        option,
                        (&bytes as *const libc::c_int).cast(),
                        std::mem::size_of_val(&bytes) as _,
                    )
                };
                assert_eq!(
                    result,
                    0,
                    "wire fixture setsockopt option={option}: {}",
                    io::Error::last_os_error()
                );
            }
            let mut bytes: libc::c_int = 0;
            let mut length = std::mem::size_of_val(&bytes) as libc::socklen_t;
            // SAFETY: a live socket and writable integer/length buffers for its option.
            let result = unsafe {
                libc::getsockopt(
                    channel.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&mut bytes as *mut libc::c_int).cast(),
                    &mut length,
                )
            };
            assert_eq!(
                result,
                0,
                "wire fixture getsockopt: {}",
                io::Error::last_os_error()
            );
            assert_eq!(length as usize, std::mem::size_of_val(&bytes));
            assert!(
                bytes > 0 && bytes < 100_004,
                "wire fixture capacity is not below the framed payload: {bytes}"
            );
            println!("wire fixture send-capacity={bytes} framed-bytes=100004");
        }
        (a, b)
    }

    #[test]
    fn a_busy_phase_snapshot_returns_without_waiting() {
        let phase = Phase(Arc::new(Mutex::new("large send")));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (holder_tx, holder_rx) = mpsc::channel();
        let held = phase.clone();
        std::thread::spawn(move || {
            let guard = held.0.lock().unwrap_or_else(|error| error.into_inner());
            let _ = ready_tx.send(());
            let released = release_rx.recv_timeout(Duration::from_secs(5));
            drop(guard);
            let _ = holder_tx.send(released.is_ok());
        });
        let ready = ready_rx.recv_timeout(Duration::from_secs(1));
        let (entry_tx, entry_rx) = mpsc::channel();
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let (complete_tx, complete_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = entry_tx.send(());
            let _ = snapshot_tx.send(phase.name());
            let _ = complete_tx.send(());
        });
        let entered = entry_rx.recv_timeout(Duration::from_secs(1));
        let snapshot = snapshot_rx.recv_timeout(Duration::from_secs(1));
        let released = release_tx.send(());
        let holder = holder_rx.recv_timeout(Duration::from_secs(2));
        let complete = complete_rx.recv_timeout(Duration::from_secs(2));
        let late = if snapshot.is_err() {
            Some(snapshot_rx.recv_timeout(Duration::from_secs(1)))
        } else {
            None
        };
        assert!(ready.is_ok(), "phase holder readiness missing: {ready:?}");
        assert!(entered.is_ok(), "snapshot entry missing: {entered:?}");
        assert!(
            released.is_ok(),
            "phase holder release missing: {released:?}"
        );
        assert_eq!(holder, Ok(true), "phase holder completion missing");
        assert!(
            complete.is_ok(),
            "snapshot completion missing: {complete:?}"
        );
        if let Some(late) = late {
            assert_eq!(late, Ok("large send"), "late snapshot fixture failed");
        }
        println!("wire phase-proof holder=ready snapshot=entered holders=completed");
        assert!(
            snapshot.is_ok(),
            "phase snapshot waited for a busy diagnostic mutex"
        );
        assert_eq!(snapshot.unwrap(), "phase snapshot busy");
        println!("wire phase-proof busy=snapshot-returned holders=completed");
    }

    #[test]
    fn a_message_and_its_descriptor_cross_the_channel_and_eof_ends_it() {
        let (mut exchange, a, b) = Exchange::new(Duration::from_secs(5));
        let phase = exchange.phase.clone();
        exchange.spawn(move || {
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(b"carried").unwrap();
            phase.set("small send");
            a.send(b"hello", &[file.as_raw_fd()]).unwrap();
            phase.set("large send");
            a.send(&vec![7u8; 100_000], &[]).unwrap();
            // Closing this original sender is what the receiver's EOF asserts.
        });
        let phase = exchange.phase.clone();
        exchange.spawn(move || {
            phase.set("first receive");
            let first = b.recv().unwrap().unwrap();
            assert_eq!(first.payload, b"hello");
            assert_eq!(first.fds.len(), 1);
            phase.set("descriptor read");
            let mut received = std::fs::File::from(first.fds.into_iter().next().unwrap());
            received.rewind().unwrap();
            let mut text = String::new();
            received.read_to_string(&mut text).unwrap();
            assert_eq!(text, "carried", "the descriptor is the same open file");
            phase.set("large receive");
            let second = b.recv().unwrap().unwrap();
            assert_eq!(second.payload, vec![7u8; 100_000]);
            assert!(second.fds.is_empty());
            phase.set("EOF receive");
            assert!(
                b.recv().unwrap().is_none(),
                "EOF between messages is the end"
            );
        });
        exchange.finish().unwrap();
    }

    #[test]
    fn a_message_past_the_bound_is_refused_before_it_is_written() {
        let (mut exchange, a, b) = Exchange::new(Duration::from_secs(5));
        let phase = exchange.phase.clone();
        exchange.spawn(move || {
            phase.set("oversize send");
            let error = a.send(&vec![0u8; MAX_MESSAGE + 1], &[]).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        });
        let phase = exchange.phase.clone();
        exchange.spawn(move || {
            phase.set("oversize EOF receive");
            assert!(
                b.recv().unwrap().is_none(),
                "a refused send must write no bytes before EOF"
            );
        });
        exchange.finish().unwrap();
    }

    #[test]
    fn a_stalled_exchange_reports_its_phase_within_the_deadline() {
        let origin = Instant::now();
        let (mut exchange, _silent, b) = Exchange::new(Duration::from_millis(100));
        let phase = exchange.phase.clone();
        exchange.spawn(move || {
            phase.set("first receive");
            let _ = b.recv();
        });
        let error = exchange.finish().expect_err("a silent peer must expire");
        assert!(
            error.contains("wire exchange deadline phase=first receive"),
            "{error}"
        );
        assert!(error.contains("cleanup=confirmed"), "{error}");
        assert!(
            origin.elapsed() < Duration::from_secs(3),
            "the exchange exceeded its deadline and cleanup reserve"
        );
    }
}
