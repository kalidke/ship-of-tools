//! The acceptor and the connection threads.
//!
//! One thread accepts; each connection has a reader thread (this module's `serve`) and a writer thread that drains the
//! connection's one queue, so replies and events leave in the order they were queued. Nothing is served before the
//! connection's hello is admitted.

use super::proto::{
    AttachReq, HelloReq, HelloRes, InputRes, ResizeReq, RowError, OP_ATTACH, OP_HELLO, OP_INPUT, OP_RESIZE, OP_TAKE,
};
use super::run::{ConnId, Out, RowRun};
use super::ROW_PROTOCOL;
use crate::lane::accept_policy::{accept_policy, AcceptAction, ACCEPT_SLEEP};
use serde::de::DeserializeOwned;
use serde_json::json;
use sot_protocol::codec::{read_frame_blocking, read_frame_blocking_blob, write_frame_blocking_blob};
use sot_protocol::{Frame, Kind};
use std::io::{BufRead, BufReader, Write};
#[cfg(windows)]
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// A connection that has not said hello within this is dropped.
const HELLO_BOUND: Duration = Duration::from_secs(5);

pub struct RowServer {
    path: PathBuf,
    run: Arc<RowRun>,
    stop: Arc<AtomicBool>,
    accept: Option<std::thread::JoinHandle<()>>,
}

/// The row's endpoint: `<runtime_dir>/row-<h>.sock` on Unix, the pipe `\\.\pipe\sot-row-<h>` on Windows.
#[cfg(unix)]
pub fn row_socket_path(h: &str) -> Result<PathBuf, RowError> {
    let dir = sot_platform::host::state_dir::runtime_dir()?;
    socket_path_in(&dir, h)
}

#[cfg(windows)]
pub fn row_socket_path(h: &str) -> Result<PathBuf, RowError> {
    Ok(PathBuf::from(format!(r"\\.\pipe\sot-row-{h}")))
}

/// `<dir>/row-<h>.sock`, refused by name when it is longer than a socket path holds.
#[cfg(unix)]
pub fn socket_path_in(dir: &Path, h: &str) -> Result<PathBuf, RowError> {
    let path = dir.join(format!("row-{h}.sock"));
    check_path_len(&path)?;
    Ok(path)
}

#[cfg(unix)]
fn check_path_len(path: &Path) -> Result<(), RowError> {
    use std::os::unix::ffi::OsStrExt;
    // sun_path holds its terminating NUL.
    let max = std::mem::size_of_val(&unsafe { std::mem::zeroed::<libc::sockaddr_un>() }.sun_path) - 1;
    if path.as_os_str().as_bytes().len() > max {
        return Err(RowError::PathTooLong(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(unix)]
fn peer_euid(fd: std::os::unix::io::RawFd) -> std::io::Result<u32> {
    #[cfg(target_os = "linux")]
    return sot_platform::identity::challenge_unix::peer_credentials(fd).map(|c| c.uid);
    #[cfg(target_os = "macos")]
    return sot_platform::identity::challenge_macos::peer_euid(fd);
}

impl RowServer {
    /// Binds `path` and serves it. The caller holds the row's fence, so a socket file left by a dead supervisor is
    /// removed first; the folder it lives in must be private.
    #[cfg(unix)]
    pub fn bind(path: &Path, run: Arc<RowRun>) -> Result<Self, RowError> {
        Self::bind_as_uid(path, run, sot_platform::host::state_dir::current_uid())
    }

    /// A server that takes this uid for its own, so a test's peer reads as another account.
    #[doc(hidden)]
    #[cfg(unix)]
    pub fn bind_as_uid_for_test(path: &Path, run: Arc<RowRun>, uid: u32) -> Result<Self, RowError> {
        Self::bind_as_uid(path, run, uid)
    }

    #[cfg(unix)]
    fn bind_as_uid(path: &Path, run: Arc<RowRun>, own_uid: u32) -> Result<Self, RowError> {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        check_path_len(path)?;
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if meta.file_type().is_socket() {
                std::fs::remove_file(path)?;
            }
        }
        let listener = std::os::unix::net::UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let accept = {
            let (run, stop) = (Arc::clone(&run), Arc::clone(&stop));
            std::thread::Builder::new()
                .name("row-accept".into())
                .spawn(move || accept_loop(listener, run, stop, own_uid))?
        };
        Ok(Self { path: path.to_path_buf(), run, stop, accept: Some(accept) })
    }

    /// Binds the pipe `path` with the owner-only descriptor on every instance it creates, and serves it.
    #[cfg(windows)]
    pub fn bind(path: &Path, run: Arc<RowRun>) -> Result<Self, RowError> {
        use interprocess::os::windows::named_pipe::{pipe_mode, PipeListenerOptions};
        use interprocess::os::windows::security_descriptor::{AsSecurityDescriptorExt as _, BorrowedSecurityDescriptor};
        let text = path
            .to_str()
            .ok_or_else(|| RowError::Protocol("the pipe name is not valid Unicode".into()))?;
        let descriptor = sot_platform::host::owner_protected_pipe_descriptor().map_err(|e| RowError::Protocol(e.to_string()))?;
        // SAFETY: `descriptor` was built by `ConvertStringSecurityDescriptorToSecurityDescriptorW` (self-relative, valid,
        // unaliased) and outlives this call; `to_owned_sd` copies it before it is freed.
        let owned = unsafe { BorrowedSecurityDescriptor::from_ptr(descriptor.as_ptr()) }
            .to_owned_sd()?;
        #[allow(clippy::disallowed_methods, reason = "listener: an owner-only DACL on every instance")]
        let listener = PipeListenerOptions::new()
            .path(text)
            .security_descriptor(Some(owned))
            .create_duplex::<pipe_mode::Bytes>()?;
        let stop = Arc::new(AtomicBool::new(false));
        let accept = {
            let (run, stop) = (Arc::clone(&run), Arc::clone(&stop));
            std::thread::Builder::new()
                .name("row-accept".into())
                .spawn(move || accept_loop(listener, run, stop))?
        };
        Ok(Self { path: path.to_path_buf(), run, stop, accept: Some(accept) })
    }

    /// Stops accepting, ends every connection and removes the socket.
    pub fn shutdown(mut self) {
        self.end();
    }

    fn end(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.run.close_all();
        // A blocked pipe accept wakes on a connect; a Unix accept polls the flag.
        #[cfg(windows)]
        let _ = std::fs::OpenOptions::new().read(true).write(true).open(&self.path);
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for RowServer {
    fn drop(&mut self) {
        self.end();
    }
}

#[cfg(unix)]
fn accept_loop(listener: std::os::unix::net::UnixListener, run: Arc<RowRun>, stop: Arc<AtomicBool>, own_uid: u32) {
    use std::os::unix::io::AsRawFd;
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                // Another account's peer is dropped before a byte is read.
                match peer_euid(stream.as_raw_fd()) {
                    Ok(uid) if uid == own_uid => {}
                    _ => continue,
                }
                let _ = stream.set_nonblocking(false);
                let Some((id, rx)) = run.open() else { continue };
                let run = Arc::clone(&run);
                let spawned = std::thread::Builder::new().name("row-conn".into()).spawn(move || serve(stream, id, rx, &run));
                if spawned.is_err() {
                    // The closure's captures are gone with it; the connection id is released by the next close_all.
                    continue;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => match accept_policy(&e) {
                AcceptAction::Retry => {}
                AcceptAction::Sleep => std::thread::sleep(ACCEPT_SLEEP),
                AcceptAction::Dead => return,
            },
        }
    }
}

#[cfg(windows)]
fn accept_loop(
    listener: interprocess::os::windows::named_pipe::PipeListener<
        interprocess::os::windows::named_pipe::pipe_mode::Bytes,
        interprocess::os::windows::named_pipe::pipe_mode::Bytes,
    >,
    run: Arc<RowRun>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok(stream) => {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                let Some((id, rx)) = run.open() else { continue };
                let run = Arc::clone(&run);
                let _ = std::thread::Builder::new().name("row-conn".into()).spawn(move || serve(stream, id, rx, &run));
            }
            Err(e) => match accept_policy(&e) {
                AcceptAction::Retry => {}
                AcceptAction::Sleep => std::thread::sleep(ACCEPT_SLEEP),
                AcceptAction::Dead => return,
            },
        }
    }
}

#[cfg(unix)]
fn serve(stream: std::os::unix::net::UnixStream, id: ConnId, rx: std::sync::mpsc::Receiver<Out>, run: &Arc<RowRun>) {
    let (Ok(write_half), Ok(closer), Ok(reader_half)) = (stream.try_clone(), stream.try_clone(), stream.try_clone()) else {
        run.close(id);
        return;
    };
    let _ = write_half.set_write_timeout(Some(HELLO_BOUND));
    let _ = stream.set_read_timeout(Some(HELLO_BOUND));
    serve_io(
        BufReader::new(reader_half),
        write_half,
        move || {
            let _ = closer.shutdown(std::net::Shutdown::Both);
        },
        move || {
            let _ = stream.set_read_timeout(None);
        },
        id,
        rx,
        run,
    );
}

#[cfg(windows)]
type PipeStream = interprocess::os::windows::named_pipe::DuplexPipeStream<interprocess::os::windows::named_pipe::pipe_mode::Bytes>;

/// One pipe, read by this thread and written by the writer's.
#[cfg(windows)]
struct PipeHalf(Arc<PipeStream>);

#[cfg(windows)]
impl Read for PipeHalf {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        (&*self.0).read(buf)
    }
}

#[cfg(windows)]
impl Write for PipeHalf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        (&*self.0).write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        (&*self.0).flush()
    }
}

/// Limits on Windows: a pipe has no read timeout, so a peer that never says hello holds its thread and one of the eight
/// slots until it closes, and a close from this end does not wake the reader until the peer ends too. The pipe's
/// owner-only descriptor keeps both to this account's own processes.
#[cfg(windows)]
fn serve(stream: PipeStream, id: ConnId, rx: std::sync::mpsc::Receiver<Out>, run: &Arc<RowRun>) {
    let shared = Arc::new(stream);
    serve_io(BufReader::new(PipeHalf(Arc::clone(&shared))), PipeHalf(shared), || {}, || {}, id, rx, run);
}

/// The connection's two threads: this one reads requests, the writer drains the connection's queue in order. `closer`
/// ends the stream when the writer is done; `after_admit` runs once the hello has been admitted.
fn serve_io<R: BufRead, W: Write + Send + 'static>(
    mut reader: R,
    mut write_half: W,
    closer: impl FnOnce() + Send + 'static,
    after_admit: impl FnOnce(),
    id: ConnId,
    rx: std::sync::mpsc::Receiver<Out>,
    run: &Arc<RowRun>,
) {
    let writer = std::thread::spawn(move || {
        while let Ok(out) = rx.recv() {
            match out {
                Out::Frame(frame, blob) => {
                    if write_frame_blocking_blob(&mut write_half, &frame, blob.as_deref()).is_err() {
                        break;
                    }
                }
                Out::Close => break,
            }
        }
        closer();
    });
    if admit(&mut reader, id, run) {
        after_admit();
        requests(&mut reader, id, run);
    }
    // Closing the connection drops its queue's sender: the writer sends what is queued (a refused hello's reply
    // included), then ends the stream.
    run.close(id);
    let _ = writer.join();
}

/// The hello: the first frame, before anything is served. The reply names this row, so a client with another row number
/// can say so; a peer that wants another row number gets the reply and is then dropped.
fn admit<R: std::io::BufRead>(reader: &mut R, id: ConnId, run: &Arc<RowRun>) -> bool {
    let Ok(frame) = read_frame_blocking(reader) else { return false };
    if frame.kind != Kind::Req || frame.op != OP_HELLO {
        return false;
    }
    let Ok(req) = serde_json::from_value::<HelloReq>(frame.payload.clone()) else { return false };
    let Ok((pid, created)) = super::run::self_identity() else { return false };
    let res = HelloRes { pid, created, phase: run.phase(), run: run.run_id(), row: ROW_PROTOCOL };
    run.send(id, Frame::res(frame.id, OP_HELLO, serde_json::to_value(res).unwrap_or_default()), None);
    req.row == ROW_PROTOCOL
}

fn refuse(run: &RowRun, id: ConnId, req: &Frame, code: &str, msg: &str) {
    run.send(id, Frame::res(req.id, &req.op, json!({"error": msg, "code": code})), None);
}

fn payload<T: DeserializeOwned>(frame: &Frame) -> Result<T, String> {
    serde_json::from_value(frame.payload.clone()).map_err(|e| format!("bad payload for {}: {e}", frame.op))
}

fn requests<R: std::io::BufRead>(reader: &mut R, id: ConnId, run: &Arc<RowRun>) {
    loop {
        let Ok((frame, blob)) = read_frame_blocking_blob(reader) else { return };
        if frame.kind != Kind::Req {
            continue;
        }
        match frame.op.as_str() {
            OP_ATTACH => match payload::<AttachReq>(&frame).and_then(|r| run.attach(id, frame.id, r.cols, r.rows, r.driver)) {
                Ok(()) => {}
                Err(msg) => refuse(run, id, &frame, "bad_request", &msg),
            },
            OP_TAKE => {
                run.take(id);
                run.send(id, Frame::res(frame.id, OP_TAKE, json!({})), None);
            }
            OP_INPUT => match run.input(id, blob.unwrap_or_default()) {
                Ok(written) => {
                    let res = InputRes { written: written as u64 };
                    run.send(id, Frame::res(frame.id, OP_INPUT, serde_json::to_value(res).unwrap_or_default()), None);
                }
                Err(msg) => refuse(run, id, &frame, "refused", &msg),
            },
            OP_RESIZE => match payload::<ResizeReq>(&frame).and_then(|r| run.resize(id, r.cols, r.rows)) {
                Ok(()) => run.send(id, Frame::res(frame.id, OP_RESIZE, json!({})), None),
                Err(msg) => refuse(run, id, &frame, "refused", &msg),
            },
            other => refuse(run, id, &frame, "unknown_op", &format!("no row op {other}")),
        }
    }
}
