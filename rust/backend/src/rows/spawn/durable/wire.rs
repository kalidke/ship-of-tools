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
    use std::sync::{mpsc, Arc, Weak};
    use std::time::{Duration, Instant};

    /// How long every worker has to report its first phase; the exchange's bound starts only after the last one has.
    const SETUP: Duration = Duration::from_secs(5);
    /// How long cleanup waits for every worker's end once it has shut down the exchange's sockets.
    const CLEANUP: Duration = Duration::from_secs(2);

    /// What a worker reports on its exchange's one channel: each phase it enters, then its end with its result.
    enum Event {
        Phase(&'static str),
        End(Result<(), String>),
    }

    type Report = (usize, Instant, Event);

    /// A worker's handle on the channel it reports on.
    struct Worker {
        index: usize,
        reports: mpsc::Sender<Report>,
    }

    impl Worker {
        fn phase(&self, phase: &'static str) {
            let _ = self
                .reports
                .send((self.index, Instant::now(), Event::Phase(phase)));
        }
    }

    /// What the owner knows of one worker: only what the worker reported.
    struct Seen {
        label: &'static str,
        phase: &'static str,
        started: Option<Instant>,
        ended: bool,
    }

    /// One bounded exchange. Its workers report on one channel. The bound starts once every worker has reported a
    /// phase; the verdict is the first worker failure as it arrives, or each unfinished worker's own phase at the
    /// deadline. Cleanup shuts down every socket the exchange made (each shutdown ends a call blocked on that socket)
    /// and is confirmed only by every worker's reported end; a later worker failure is kept beside the verdict.
    struct Exchange {
        bound: Duration,
        sender: mpsc::Sender<Report>,
        reports: mpsc::Receiver<Report>,
        workers: Vec<Seen>,
        sockets: Vec<Weak<Channel>>,
        others: Vec<String>,
    }

    impl Exchange {
        fn new(bound: Duration) -> Self {
            let (sender, reports) = mpsc::channel();
            Self {
                bound,
                sender,
                reports,
                workers: Vec::new(),
                sockets: Vec::new(),
                others: Vec::new(),
            }
        }

        /// A socketpair cleanup can shut down. The exchange keeps only weak handles: a worker closes its end, and so
        /// gives its peer EOF, by dropping it.
        fn pair(&mut self) -> (Arc<Channel>, Arc<Channel>) {
            let (a, b) = constrained_pair();
            let (a, b) = (Arc::new(a), Arc::new(b));
            self.sockets
                .extend([Arc::downgrade(&a), Arc::downgrade(&b)]);
            (a, b)
        }

        fn spawn(&mut self, label: &'static str, body: impl FnOnce(&Worker) + Send + 'static) {
            let worker = Worker {
                index: self.workers.len(),
                reports: self.sender.clone(),
            };
            self.workers.push(Seen {
                label,
                phase: "start",
                started: None,
                ended: false,
            });
            std::thread::spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&worker)))
                        .map_err(panic_text);
                let _ = worker
                    .reports
                    .send((worker.index, Instant::now(), Event::End(result)));
            });
        }

        fn finish(mut self) -> Result<(), String> {
            match self.verdict() {
                None => Ok(()),
                Some(verdict) => Err(format!("{verdict}; {}", self.cleanup())),
            }
        }

        fn verdict(&mut self) -> Option<String> {
            let setup = Instant::now() + SETUP;
            let mut deadline: Option<Instant> = None;
            while self.workers.iter().any(|worker| !worker.ended) {
                let until = deadline.unwrap_or(setup);
                let wait = until.saturating_duration_since(Instant::now());
                let Ok((index, at, event)) = self.reports.recv_timeout(wait) else {
                    return Some(self.expired(deadline));
                };
                // A report stamped after the deadline cannot pass, even if this owner was descheduled.
                if deadline.is_some_and(|deadline| at > deadline) {
                    let expired = self.expired(deadline);
                    if let Some(failure) = self.apply(index, at, event) {
                        self.others.push(failure);
                    }
                    return Some(expired);
                }
                if let Some(failure) = self.apply(index, at, event) {
                    return Some(failure);
                }
                if deadline.is_none() && self.workers.iter().all(|worker| worker.started.is_some())
                {
                    let last = self
                        .workers
                        .iter()
                        .filter_map(|worker| worker.started)
                        .max();
                    deadline = last.map(|last| last + self.bound);
                }
            }
            None
        }

        /// Records one report; a failed end is returned as its failure.
        fn apply(&mut self, index: usize, at: Instant, event: Event) -> Option<String> {
            let worker = &mut self.workers[index];
            worker.started.get_or_insert(at);
            match event {
                Event::Phase(phase) => worker.phase = phase,
                Event::End(result) => {
                    worker.ended = true;
                    if let Err(text) = result {
                        return Some(format!(
                            "wire worker {} failed phase={}: {text}",
                            worker.label, worker.phase
                        ));
                    }
                }
            }
            None
        }

        fn expired(&self, deadline: Option<Instant>) -> String {
            let stage = if deadline.is_some() {
                "deadline"
            } else {
                "setup"
            };
            format!("wire exchange {stage}: {}", self.unfinished())
        }

        fn unfinished(&self) -> String {
            let workers = self.workers.iter().filter(|worker| !worker.ended);
            let named: Vec<_> = workers
                .map(|worker| format!("{} phase={}", worker.label, worker.phase))
                .collect();
            named.join(", ")
        }

        fn cleanup(&mut self) -> String {
            for socket in self.sockets.iter().filter_map(Weak::upgrade) {
                // SAFETY: a live socket this exchange made. Shutdown may fail where the peer already closed
                // (ENOTCONN on macOS); the workers' reported ends, not this result, confirm cleanup.
                let _ = unsafe { libc::shutdown(socket.as_raw_fd(), libc::SHUT_RDWR) };
            }
            let until = Instant::now() + CLEANUP;
            while self.workers.iter().any(|worker| !worker.ended) {
                let wait = until.saturating_duration_since(Instant::now());
                let Ok((index, at, event)) = self.reports.recv_timeout(wait) else {
                    break;
                };
                if let Some(failure) = self.apply(index, at, event) {
                    self.others.push(failure);
                }
            }
            let mut text = match self.unfinished() {
                unfinished if unfinished.is_empty() => "cleanup=confirmed".to_string(),
                unfinished => format!("cleanup=unconfirmed {unfinished}"),
            };
            for other in &self.others {
                text.push_str("; also ");
                text.push_str(other);
            }
            text
        }
    }

    fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|text| text.to_string()))
            .unwrap_or_else(|| "panic without text".into())
    }

    /// Every byte of a received large payload is compared; a failure names only the length or the first mismatch
    /// (an `assert_eq!` of two 100,000-byte vectors prints both, some 600 KB).
    fn assert_payload(payload: &[u8]) {
        assert!(
            payload.len() == 100_000,
            "payload length expected=100000 observed={}",
            payload.len()
        );
        if let Some(index) = payload.iter().position(|&byte| byte != 7) {
            panic!(
                "payload mismatch index={index} expected=7 observed={}",
                payload[index]
            );
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
    fn a_message_and_its_descriptor_cross_the_channel_and_eof_ends_it() {
        let mut exchange = Exchange::new(Duration::from_secs(5));
        let (a, b) = exchange.pair();
        exchange.spawn("sender", move |worker| {
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(b"carried").unwrap();
            worker.phase("small send");
            a.send(b"hello", &[file.as_raw_fd()]).unwrap();
            worker.phase("large send");
            a.send(&vec![7u8; 100_000], &[]).unwrap();
            // Dropping this only strong handle closes the sender: the receiver's EOF asserts that.
        });
        exchange.spawn("receiver", move |worker| {
            worker.phase("first receive");
            let first = b.recv().unwrap().unwrap();
            assert_eq!(first.payload, b"hello");
            assert_eq!(first.fds.len(), 1);
            worker.phase("descriptor read");
            let mut received = std::fs::File::from(first.fds.into_iter().next().unwrap());
            received.rewind().unwrap();
            let mut text = String::new();
            received.read_to_string(&mut text).unwrap();
            assert_eq!(text, "carried", "the descriptor is the same open file");
            worker.phase("large receive");
            let second = b.recv().unwrap().unwrap();
            assert_payload(&second.payload);
            assert!(second.fds.is_empty());
            worker.phase("EOF receive");
            assert!(
                b.recv().unwrap().is_none(),
                "EOF between messages is the end"
            );
        });
        exchange.finish().unwrap();
    }

    #[test]
    fn a_message_past_the_bound_is_refused_before_it_is_written() {
        let mut exchange = Exchange::new(Duration::from_secs(5));
        let (a, b) = exchange.pair();
        exchange.spawn("sender", move |worker| {
            worker.phase("oversize send");
            let error = a.send(&vec![0u8; MAX_MESSAGE + 1], &[]).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        });
        exchange.spawn("receiver", move |worker| {
            worker.phase("oversize EOF receive");
            assert!(
                b.recv().unwrap().is_none(),
                "a refused send must write no bytes before EOF"
            );
        });
        exchange.finish().unwrap();
    }

    /// A worker stuck on a silent peer, or on a peer that never reads, is named with its phase at the deadline and
    /// ended by cleanup. Each starts after the 100 ms an exchange's bound would have spent had it begun at creation.
    #[test]
    fn a_stalled_exchange_reports_its_phase_within_the_deadline() {
        for (label, phase) in [("receiver", "first receive"), ("sender", "large send")] {
            let origin = Instant::now();
            let mut exchange = Exchange::new(Duration::from_millis(100));
            let (a, b) = exchange.pair();
            let (stuck, held) = if label == "receiver" { (b, a) } else { (a, b) };
            exchange.spawn(label, move |worker| {
                std::thread::sleep(Duration::from_millis(300));
                worker.phase(phase);
                let _ = match label {
                    "receiver" => stuck.recv().map(|_| ()),
                    _ => stuck.send(&vec![0u8; MAX_MESSAGE], &[]),
                };
            });
            let error = exchange.finish().expect_err("a stalled worker must expire");
            drop(held);
            assert!(
                error.contains(&format!("wire exchange deadline: {label} phase={phase}")),
                "{error}"
            );
            assert!(error.contains("cleanup=confirmed"), "{error}");
            assert!(
                origin.elapsed() < Duration::from_secs(3),
                "the exchange exceeded its setup, deadline and cleanup: {error}"
            );
            println!("wire stalled-proof worker={label} phase={phase} cleanup=confirmed");
        }
    }

    /// A worker's failure is the verdict when it arrives, though a worker spawned before it is still blocked. Workers
    /// block only on a socket of their exchange, which cleanup's shutdown ends, never on a clock: no start order can
    /// change the verdict.
    #[test]
    fn a_later_worker_failure_is_reported_while_an_earlier_one_runs() {
        let mut exchange = Exchange::new(Duration::from_secs(1));
        let (silent, waiting) = exchange.pair();
        exchange.spawn("waiting", move |worker| {
            worker.phase("waiting");
            let _ = waiting.recv();
        });
        exchange.spawn("failing", |worker| {
            worker.phase("failing");
            panic!("worker two failed on purpose");
        });
        let error = exchange
            .finish()
            .expect_err("a failed worker must fail the exchange");
        drop(silent);
        assert!(
            error.contains("worker two failed on purpose"),
            "a later worker's failure was hidden behind an earlier worker: {error}"
        );
        assert!(
            error.starts_with("wire worker failing failed phase=failing"),
            "{error}"
        );
        assert!(error.contains("cleanup=confirmed"), "{error}");
        println!("wire completion-proof verdict={error}");
    }

    /// A deadline names each unfinished worker with its own phase. Both block on silent peers until cleanup's shutdown.
    #[test]
    fn a_deadline_names_each_unfinished_workers_phase() {
        let mut exchange = Exchange::new(Duration::from_millis(300));
        let mut silent = Vec::new();
        for (label, phase) in [("first", "first receive"), ("second", "second receive")] {
            let (peer, end) = exchange.pair();
            silent.push(peer);
            exchange.spawn(label, move |worker| {
                worker.phase(phase);
                let _ = end.recv();
            });
        }
        let error = exchange
            .finish()
            .expect_err("two stuck workers must expire");
        drop(silent);
        assert!(
            error.contains("phase=first receive") && error.contains("phase=second receive"),
            "the deadline did not name each unfinished worker's phase: {error}"
        );
        assert!(
            error.starts_with(
                "wire exchange deadline: first phase=first receive, second phase=second receive"
            ),
            "{error}"
        );
        assert!(error.contains("cleanup=confirmed"), "{error}");
        println!("wire deadline-proof verdict={error}");
    }

    #[test]
    fn a_payload_mismatch_reports_only_its_first_index() {
        let valid = vec![7u8; 100_000];
        assert_payload(&valid);
        let mut corrupt = valid.clone();
        corrupt[617] = 8;
        let mismatch = panic_text(
            std::panic::catch_unwind(|| assert_payload(&corrupt))
                .expect_err("corrupt payload was accepted"),
        );
        let length = panic_text(
            std::panic::catch_unwind(|| assert_payload(&valid[..99_999]))
                .expect_err("short payload was accepted"),
        );
        println!("wire payload-probe valid=accepted mismatch=caught length=caught");
        assert!(
            mismatch.len() < 1024 && mismatch.contains("index=617 expected=7 observed=8"),
            "payload mismatch diagnostic exceeded its bound or omitted first index"
        );
        assert!(
            length.len() < 1024 && length.contains("payload length expected=100000 observed=99999"),
            "wrong payload length diagnostic: {length}"
        );
        println!("wire payload-proof comparison=whole first-mismatch=617 diagnostic=bounded");
    }
}
