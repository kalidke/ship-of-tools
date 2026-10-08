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
    /// The parent is up (first message).
    Hello { pid: u32 },
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

    #[test]
    fn a_message_and_its_descriptor_cross_the_channel_and_eof_ends_it() {
        let (a, b) = Channel::pair().unwrap();
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"carried").unwrap();
        a.send(b"hello", &[file.as_raw_fd()]).unwrap();
        a.send(&vec![7u8; 100_000], &[]).unwrap();
        let first = b.recv().unwrap().unwrap();
        assert_eq!(first.payload, b"hello");
        assert_eq!(first.fds.len(), 1);
        let mut received = std::fs::File::from(first.fds.into_iter().next().unwrap());
        received.rewind().unwrap();
        let mut text = String::new();
        received.read_to_string(&mut text).unwrap();
        assert_eq!(text, "carried", "the descriptor is the same open file");
        let second = b.recv().unwrap().unwrap();
        assert_eq!((second.payload.len(), second.fds.len()), (100_000, 0));
        drop(a);
        assert!(
            b.recv().unwrap().is_none(),
            "EOF between messages is the end"
        );
    }

    #[test]
    fn a_message_past_the_bound_is_refused_before_it_is_written() {
        let (a, _b) = Channel::pair().unwrap();
        assert!(a.send(&vec![0u8; MAX_MESSAGE + 1], &[]).is_err());
    }
}
