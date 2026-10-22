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
use std::io::BufReader;
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

/// `<runtime_dir>/row-<h>.sock`.
pub fn row_socket_path(h: &str) -> Result<PathBuf, RowError> {
    let dir = sot_platform::host::state_dir::runtime_dir()?;
    socket_path_in(&dir, h)
}

/// `<dir>/row-<h>.sock`, refused by name when it is longer than a socket path holds.
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

#[cfg(not(unix))]
fn check_path_len(_path: &Path) -> Result<(), RowError> {
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

    /// Stops accepting, ends every connection and removes the socket.
    pub fn shutdown(mut self) {
        self.end();
    }

    fn end(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.run.close_all();
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
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

#[cfg(unix)]
fn serve(stream: std::os::unix::net::UnixStream, id: ConnId, rx: std::sync::mpsc::Receiver<Out>, run: &Arc<RowRun>) {
    let Ok(write_half) = stream.try_clone() else {
        run.close(id);
        return;
    };
    let _ = write_half.set_write_timeout(Some(HELLO_BOUND));
    let writer = std::thread::spawn(move || {
        let mut w = write_half;
        while let Ok(out) = rx.recv() {
            match out {
                Out::Frame(frame, blob) => {
                    if write_frame_blocking_blob(&mut w, &frame, blob.as_deref()).is_err() {
                        break;
                    }
                }
                Out::Close => break,
            }
        }
        let _ = w.shutdown(std::net::Shutdown::Both);
    });
    let _ = stream.set_read_timeout(Some(HELLO_BOUND));
    let mut reader = BufReader::new(stream);
    if admit(&mut reader, id, run) {
        let _ = reader.get_ref().set_read_timeout(None);
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
