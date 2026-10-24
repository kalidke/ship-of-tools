//! The acceptor and the connection threads.
//!
//! One thread accepts; each connection has a reader thread (this module's `serve`) and a writer thread that drains the
//! connection's one queue, so replies and events leave in the order they were queued. Nothing is served before the
//! connection's hello is admitted.

use super::proto::{
    AttachReq, EndRunReq, HelloReq, HelloRes, InputRes, ResizeReq, RowError, OP_ATTACH, OP_END_RUN, OP_HELLO, OP_INPUT,
    OP_RESET, OP_RESIZE, OP_STATUS, OP_STOP, OP_TAKE, OP_WATCH,
};
use super::run::{code_of, ConnId, Out, OutQueue, RowRun, Slot};
use super::ROW_PROTOCOL;
use sot_platform::transport::accept_policy::{accept_policy, AcceptAction, ACCEPT_SLEEP};
use serde::de::DeserializeOwned;
use serde_json::json;
use sot_protocol::codec::{read_frame_blocking, read_frame_blocking_blob, write_frame_blocking_blob};
use sot_protocol::{Frame, Kind};
use std::io::{BufRead, BufReader, Write};
#[cfg(windows)]
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(unix)]
use std::time::{Duration, Instant};

/// A write that makes no progress for this long fails, so a connection whose peer stopped reading ends.
#[cfg(unix)]
const WRITE_BOUND: Duration = Duration::from_secs(5);

/// What the acceptor calls when it ends without a shutdown.
type Death = Arc<dyn Fn() + Send + Sync>;

/// The connection threads the acceptor started, which shutdown joins.
type Threads = Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>;

pub struct RowServer {
    path: PathBuf,
    run: Arc<RowRun>,
    stop: Arc<AtomicBool>,
    accept: Option<std::thread::JoinHandle<()>>,
    threads: Threads,
}

/// Test seam: `SOT_TEST_ROW_LISTENER_DIES=first` ends this process's first acceptor, and `always` every one, at once, as
/// a permanent listener failure would. Inert unless named.
fn listener_dies_for_test() -> bool {
    static STARTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = STARTED.fetch_add(1, Ordering::AcqRel);
    match std::env::var("SOT_TEST_ROW_LISTENER_DIES").as_deref() {
        Ok("always") => true,
        Ok("first") => n == 0,
        _ => false,
    }
}

/// Test seam: `SOT_TEST_ROW_LISTENER_DIES_ON=<path>` ends every acceptor, this process's first and each one bound after,
/// once that file exists, as a permanent listener failure that comes while the row is up. Inert unless named.
#[cfg(unix)]
fn listener_dies_on_trigger_for_test() -> bool {
    std::env::var_os("SOT_TEST_ROW_LISTENER_DIES_ON").is_some_and(|path| Path::new(&path).exists())
}

/// Keeps a connection's thread to be joined at shutdown, and forgets the ones that have ended.
fn keep(threads: &Threads, handle: std::thread::JoinHandle<()>) {
    let mut all = threads.lock().unwrap_or_else(|p| p.into_inner());
    all.retain(|h| !h.is_finished());
    all.push(handle);
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
        Self::bind_as_uid(path, run, sot_platform::host::state_dir::current_uid(), Arc::new(|| {}))
    }

    /// As [`Self::bind`]; `on_death` is called, once, from the acceptor's own thread if it ends without a shutdown: a
    /// permanent listener failure, after which no viewer can connect.
    #[cfg(unix)]
    pub fn bind_watched(path: &Path, run: Arc<RowRun>, on_death: impl Fn() + Send + Sync + 'static) -> Result<Self, RowError> {
        Self::bind_as_uid(path, run, sot_platform::host::state_dir::current_uid(), Arc::new(on_death))
    }

    /// A server that takes this uid for its own, so a test's peer reads as another account.
    #[doc(hidden)]
    #[cfg(unix)]
    pub fn bind_as_uid_for_test(path: &Path, run: Arc<RowRun>, uid: u32) -> Result<Self, RowError> {
        Self::bind_as_uid(path, run, uid, Arc::new(|| {}))
    }

    #[cfg(unix)]
    fn bind_as_uid(path: &Path, run: Arc<RowRun>, own_uid: u32, on_death: Death) -> Result<Self, RowError> {
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
        let threads: Threads = Arc::default();
        let accept = {
            let (run, stop, threads) = (Arc::clone(&run), Arc::clone(&stop), Arc::clone(&threads));
            std::thread::Builder::new()
                .name("row-accept".into())
                .spawn(move || accept_loop(listener, run, stop, threads, own_uid, on_death))?
        };
        Ok(Self { path: path.to_path_buf(), run, stop, accept: Some(accept), threads })
    }

    /// Binds the pipe `path` with the owner-only descriptor on every instance it creates, and serves it.
    #[cfg(windows)]
    pub fn bind(path: &Path, run: Arc<RowRun>) -> Result<Self, RowError> {
        Self::bind_watched(path, run, || {})
    }

    /// As [`Self::bind`]; `on_death` is called, once, from the acceptor's own thread if it ends without a shutdown: a
    /// permanent listener failure, after which no viewer can connect.
    #[cfg(windows)]
    pub fn bind_watched(path: &Path, run: Arc<RowRun>, on_death: impl Fn() + Send + Sync + 'static) -> Result<Self, RowError> {
        use interprocess::os::windows::named_pipe::{pipe_mode, PipeListenerOptions};
        use interprocess::os::windows::security_descriptor::{AsSecurityDescriptorExt as _, BorrowedSecurityDescriptor};
        let on_death: Death = Arc::new(on_death);
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
        let threads: Threads = Arc::default();
        let accept = {
            let (run, stop, threads) = (Arc::clone(&run), Arc::clone(&stop), Arc::clone(&threads));
            std::thread::Builder::new()
                .name("row-accept".into())
                .spawn(move || accept_loop(listener, run, stop, threads, on_death))?
        };
        Ok(Self { path: path.to_path_buf(), run, stop, accept: Some(accept), threads })
    }

    /// Stops accepting, ends every connection and removes the socket.
    pub fn shutdown(mut self) {
        self.end();
    }

    /// Stops the acceptor first (it rechecks the stop after every accept, so no connection is registered behind
    /// this), then ends every connection's stream and joins every connection thread.
    fn end(&mut self) {
        self.stop.store(true, Ordering::Release);
        // A blocked pipe accept wakes on a connect; a Unix accept polls the flag.
        #[cfg(windows)]
        let _ = std::fs::OpenOptions::new().read(true).write(true).open(&self.path);
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
        self.run.close_all();
        let threads = std::mem::take(&mut *self.threads.lock().unwrap_or_else(|p| p.into_inner()));
        for t in threads {
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
fn accept_loop(
    listener: std::os::unix::net::UnixListener,
    run: Arc<RowRun>,
    stop: Arc<AtomicBool>,
    threads: Threads,
    own_uid: u32,
    on_death: Death,
) {
    use std::os::unix::io::AsRawFd;
    if listener_dies_for_test() {
        on_death();
        return;
    }
    while !stop.load(Ordering::Acquire) {
        #[allow(
            clippy::disallowed_methods,
            reason = "listener: row socket: the peer's uid is checked at accept, before a byte is read"
        )]
        let accepted = listener.accept();
        match accepted {
            Ok((stream, _)) => {
                // A shutdown that began during the accept registers nothing.
                if stop.load(Ordering::Acquire) {
                    return;
                }
                // Another account's peer is dropped before a byte is read.
                match peer_euid(stream.as_raw_fd()) {
                    Ok(uid) if uid == own_uid => {}
                    _ => continue,
                }
                let _ = stream.set_nonblocking(false);
                let Ok(closer_stream) = stream.try_clone() else { continue };
                let closer: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                    let _ = closer_stream.shutdown(std::net::Shutdown::Both);
                });
                let Some((id, rx, slot)) = run.open(closer) else { continue };
                let hello_bound = run.hello_bound();
                let conn_run = Arc::clone(&run);
                let spawned = std::thread::Builder::new()
                    .name("row-conn".into())
                    .spawn(move || serve(stream, id, rx, slot, hello_bound, &conn_run));
                match spawned {
                    Ok(handle) => keep(&threads, handle),
                    // The closure, and with it the slot, is gone; the registration is not.
                    Err(_) => run.close(id),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if listener_dies_on_trigger_for_test() {
                    on_death();
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => match accept_policy(&e) {
                AcceptAction::Retry => {}
                AcceptAction::Sleep => std::thread::sleep(ACCEPT_SLEEP),
                AcceptAction::Dead => {
                    // A shutdown that began meanwhile is not a death.
                    if !stop.load(Ordering::Acquire) {
                        on_death();
                    }
                    return;
                }
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
    threads: Threads,
    on_death: Death,
) {
    if listener_dies_for_test() {
        on_death();
        return;
    }
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok(stream) => {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                let stream = Arc::new(stream);
                let closed = Arc::new(AtomicBool::new(false));
                let closer: Arc<dyn Fn() + Send + Sync> = {
                    let (stream, closed) = (Arc::clone(&stream), Arc::clone(&closed));
                    Arc::new(move || cancel_pipe_io(&stream, &closed))
                };
                let Some((id, rx, slot)) = run.open(closer) else { continue };
                let conn_run = Arc::clone(&run);
                let spawned = std::thread::Builder::new()
                    .name("row-conn".into())
                    .spawn(move || serve(stream, closed, id, rx, slot, &conn_run));
                match spawned {
                    Ok(handle) => keep(&threads, handle),
                    Err(_) => run.close(id),
                }
            }
            Err(e) => match accept_policy(&e) {
                AcceptAction::Retry => {}
                AcceptAction::Sleep => std::thread::sleep(ACCEPT_SLEEP),
                AcceptAction::Dead => {
                    // A shutdown that began meanwhile is not a death.
                    if !stop.load(Ordering::Acquire) {
                        on_death();
                    }
                    return;
                }
            },
        }
    }
}

/// A `Read` over the connection's stream that gives the hello one absolute deadline: before each read the stream's
/// timeout is set to what is left of it, and it is cleared once the hello is admitted.
#[cfg(unix)]
struct HelloDeadline {
    stream: std::os::unix::net::UnixStream,
    deadline: Instant,
    admitted: Arc<AtomicBool>,
    cleared: bool,
}

#[cfg(unix)]
impl std::io::Read for HelloDeadline {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.admitted.load(Ordering::Acquire) {
            if !self.cleared {
                self.stream.set_read_timeout(None)?;
                self.cleared = true;
            }
        } else {
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "no hello by its deadline"));
            }
            self.stream.set_read_timeout(Some(left))?;
        }
        self.stream.read(buf)
    }
}

#[cfg(unix)]
fn serve(stream: std::os::unix::net::UnixStream, id: ConnId, rx: Arc<OutQueue>, slot: Slot, hello_bound: Duration, run: &Arc<RowRun>) {
    let Ok(write_half) = stream.try_clone() else {
        run.close(id);
        return;
    };
    let _ = write_half.set_write_timeout(Some(WRITE_BOUND));
    let admitted = Arc::new(AtomicBool::new(false));
    let closer = stream.try_clone();
    let reader = HelloDeadline { stream, deadline: Instant::now() + hello_bound, admitted: Arc::clone(&admitted), cleared: false };
    serve_io(
        BufReader::new(reader),
        write_half,
        move || {
            if let Ok(s) = closer {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        },
        move || admitted.store(true, Ordering::Release),
        id,
        rx,
        slot,
        run,
    );
}

#[cfg(windows)]
type PipeStream = interprocess::os::windows::named_pipe::DuplexPipeStream<interprocess::os::windows::named_pipe::pipe_mode::Bytes>;

/// Cancels the I/O a thread has blocked on this pipe, from any thread, and makes every later call fail. A thread that
/// began its call between the flag's check and the cancel is caught by the second call the writer makes as it ends.
#[cfg(windows)]
fn cancel_pipe_io(stream: &PipeStream, closed: &AtomicBool) {
    use std::os::windows::io::AsRawHandle;
    closed.store(true, Ordering::Release);
    // SAFETY: the handle is the live pipe's own; a null overlapped cancels every pending call on it.
    unsafe {
        windows_sys::Win32::System::IO::CancelIoEx(stream.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE, std::ptr::null());
    }
}

/// One pipe, read by this thread and written by the writer's.
#[cfg(windows)]
struct PipeHalf {
    stream: Arc<PipeStream>,
    closed: Arc<AtomicBool>,
}

#[cfg(windows)]
impl PipeHalf {
    fn check(&self) -> std::io::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "the connection was ended"));
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Read for PipeHalf {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.check()?;
        (&*self.stream).read(buf)
    }
}

#[cfg(windows)]
impl Write for PipeHalf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.check()?;
        (&*self.stream).write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.check()?;
        (&*self.stream).flush()
    }
}

/// A pipe has no read timeout, so on Windows the hello has no deadline: a peer of this account that never says hello
/// holds its two threads and one of the eight slots until it closes or the row ends. Every other end goes through
/// `cancel_pipe_io`, which wakes a blocked read or write.
#[cfg(windows)]
fn serve(stream: Arc<PipeStream>, closed: Arc<AtomicBool>, id: ConnId, rx: Arc<OutQueue>, slot: Slot, run: &Arc<RowRun>) {
    let closer_stream = Arc::clone(&stream);
    let closer_flag = Arc::clone(&closed);
    serve_io(
        BufReader::new(PipeHalf { stream: Arc::clone(&stream), closed: Arc::clone(&closed) }),
        PipeHalf { stream, closed },
        move || cancel_pipe_io(&closer_stream, &closer_flag),
        || {},
        id,
        rx,
        slot,
        run,
    );
}

/// The connection's two threads: this one reads requests, the writer drains the connection's queue in order. `closer`
/// ends the stream when the writer is done; `after_admit` runs once the hello has been admitted. The slot goes back
/// only after the writer has been joined.
fn serve_io<R: BufRead, W: Write + Send + 'static>(
    mut reader: R,
    mut write_half: W,
    closer: impl FnOnce() + Send + 'static,
    after_admit: impl FnOnce(),
    id: ConnId,
    rx: Arc<OutQueue>,
    slot: Slot,
    run: &Arc<RowRun>,
) {
    let spawned = std::thread::Builder::new().name("row-write".into()).spawn(move || {
        while let Some(Out::Frame(frame, blob)) = rx.pop() {
            if write_frame_blocking_blob(&mut write_half, &frame, blob.as_deref()).is_err() {
                break;
            }
        }
        closer();
    });
    let Ok(writer) = spawned else {
        run.close(id);
        run.released(id);
        drop(slot);
        return;
    };
    if admit(&mut reader, id, run) {
        after_admit();
        requests(&mut reader, id, run);
    }
    // Closing the connection finishes its queue: the writer sends what is queued (a refused hello's reply included),
    // then ends the stream.
    run.close(id);
    let _ = writer.join();
    run.released(id);
    drop(slot);
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

/// A refusal from the run, named by its own code.
fn refuse_with(run: &RowRun, id: ConnId, req: &Frame, msg: &str) {
    refuse(run, id, req, code_of(msg), msg);
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
                Err(msg) => refuse_with(run, id, &frame, &msg),
            },
            OP_TAKE => {
                run.take(id);
                run.send(id, Frame::res(frame.id, OP_TAKE, json!({})), None);
            }
            OP_INPUT => match run.input(id, blob.unwrap_or_default(), frame.payload.get("run").and_then(|r| r.as_str())) {
                Ok(written) => {
                    let res = InputRes { written: written as u64 };
                    run.send(id, Frame::res(frame.id, OP_INPUT, serde_json::to_value(res).unwrap_or_default()), None);
                }
                Err(msg) => refuse_with(run, id, &frame, &msg),
            },
            OP_RESIZE => match payload::<ResizeReq>(&frame).and_then(|r| run.resize(id, r.cols, r.rows)) {
                Ok(()) => run.send(id, Frame::res(frame.id, OP_RESIZE, json!({})), None),
                Err(msg) => refuse_with(run, id, &frame, &msg),
            },
            OP_STATUS | OP_WATCH | OP_END_RUN | OP_RESET | OP_STOP => {
                let Some(management) = run.management() else {
                    refuse(run, id, &frame, "unknown_op", &format!("no row op {}", frame.op));
                    continue;
                };
                match frame.op.as_str() {
                    OP_STATUS => answer(run, id, &frame, &management.status()),
                    OP_WATCH => run.watch(id, frame.id, &*management),
                    OP_END_RUN => match payload::<EndRunReq>(&frame) {
                        Ok(req) => answer(run, id, &frame, &management.end_run(&req.run, &req.reason)),
                        Err(msg) => refuse(run, id, &frame, "bad_payload", &msg),
                    },
                    OP_RESET => answer(run, id, &frame, &management.reset()),
                    _ => {
                        // A stop's reply is the connection's last word: the supervisor leaves once it is delivered.
                        answer(run, id, &frame, &management.stop(id));
                        return;
                    }
                }
            }
            other => refuse(run, id, &frame, "unknown_op", &format!("no row op {other}")),
        }
    }
}

fn answer(run: &RowRun, id: ConnId, req: &Frame, res: &impl serde::Serialize) {
    run.send(id, Frame::res(req.id, &req.op, serde_json::to_value(res).unwrap_or_default()), None);
}
