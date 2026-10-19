// The daemon's log: the tee to `<state>/sotd.log` and, when stdout is not a regular file, to stdout.

use std::sync::{Arc, Mutex};

use sot_log::secret::RedactingWriter;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::paths;

/// Writer for `tracing_subscriber::fmt`: mirrors every log line to a private `0600` file under `paths::state_dir()` and,
/// unless stdout is a regular file (a launcher's per-start log, which would grow without a bound beside the file that
/// rolls), to stdout (a terminal, a pipe or a journal socket). The file half is best-effort: `None` or a write failure
/// there never breaks the stdout path.
struct TeeWriter {
    file: Option<Arc<Mutex<LogFile>>>,
    /// Whether stdout gets a copy; decided once at start by [`stdout_is_file`].
    stdout: bool,
}

/// Whether this process's stdout is a regular file. Unix: `fstat` says `S_ISREG`.
#[cfg(unix)]
fn stdout_is_file() -> bool {
    fd_is_file(1)
}

#[cfg(unix)]
fn fd_is_file(fd: std::os::fd::RawFd) -> bool {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fstat` writes a `stat` into the pointer it is given and reads nothing else; a closed or invalid `fd`
    // is an error return, which reads as not a file.
    unsafe { libc::fstat(fd, st.as_mut_ptr()) == 0 && (st.assume_init().st_mode & libc::S_IFMT) == libc::S_IFREG }
}

/// Whether this process's stdout is a regular file. Windows: `GetFileType` says `FILE_TYPE_DISK`.
#[cfg(windows)]
fn stdout_is_file() -> bool {
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    // SAFETY: `GetStdHandle` only reads this process's standard handle table.
    handle_is_file(unsafe { GetStdHandle(STD_OUTPUT_HANDLE) })
}

#[cfg(windows)]
fn handle_is_file(handle: windows_sys::Win32::Foundation::HANDLE) -> bool {
    use windows_sys::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_DISK};
    // SAFETY: `GetFileType` takes any handle value; an invalid one returns FILE_TYPE_UNKNOWN, which is not a file.
    unsafe { GetFileType(handle) == FILE_TYPE_DISK }
}

impl TeeWriter {
    /// The file half first, then `out` when stdout gets a copy: a stdout that fails (a closed pipe) never stops the
    /// log of record.
    fn write_to(&mut self, out: &mut dyn std::io::Write, buf: &[u8]) -> usize {
        if let Some(Ok(mut f)) = self.file.as_ref().map(|f| f.lock()) {
            let _ = std::io::Write::write_all(&mut f.file, buf);
            if let Some(failed) = f.roll_if_over() {
                eprintln!("{failed}");
            }
        }
        if self.stdout {
            let _ = out.write_all(buf);
        }
        buf.len()
    }
}

impl std::io::Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(self.write_to(&mut std::io::stdout(), buf))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(Ok(mut f)) = self.file.as_ref().map(|f| f.lock()) {
            let _ = f.file.flush();
        }
        if self.stdout {
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        Ok(())
    }
}

/// The daemon's log is rolled when it is over this size, after each write the daemon makes.
const LOG_ROLL_BYTES: u64 = 64 * 1024 * 1024;

/// The daemon's handle on `sotd.log`, its path and its cap. Capsule supervisors append to the same file, so the size
/// is read from the file, which counts their appends too.
struct LogFile {
    file: std::fs::File,
    path: std::path::PathBuf,
    cap: u64,
    /// After a failed roll, the size below which the roll is not tried again (the size at the failure plus `cap`),
    /// so a roll that cannot succeed costs one copy per `cap` of growth, not one per write; 0 when none failed.
    retry_above: u64,
}

impl LogFile {
    /// Rolls the log when it is over the cap; returns the message for a roll that failed, to print once.
    fn roll_if_over(&mut self) -> Option<String> {
        let len = self.file.metadata().map(|m| m.len()).ok()?;
        if len <= self.cap || len <= self.retry_above {
            return None;
        }
        match roll_log(&self.path, len, self.cap) {
            Ok(()) => {
                self.retry_above = 0;
                None
            }
            Err(e) => {
                self.retry_above = len + self.cap;
                Some(format!("sotd: could not roll log file {}: {e}", self.path.display()))
            }
        }
    }
}

/// Copies the last `cap` bytes of the log at `path`, which holds `len`, to `<path>.1` (replacing any older one) and
/// truncates it in place. Every writer holds a handle on the one file and appends at its end, so a supervisor that
/// opened the log weeks ago keeps writing into the readable `sotd.log`: a line is readable in `sotd.log` or
/// `sotd.log.1` until the second roll after it. A line appended between the copy's last read and the truncate is lost
/// (its window is the copy's close and rename: microseconds on a local disk, longer on a network one). On error the
/// log is left whole.
fn roll_log(path: &std::path::Path, len: u64, cap: u64) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut rolled = path.as_os_str().to_owned();
    rolled.push(".1");
    let mut tmp = rolled.clone();
    tmp.push(".tmp");
    let copied = std::fs::File::open(path)
        .and_then(|mut from| from.seek(SeekFrom::Start(len.saturating_sub(cap))).map(|_| from))
        .and_then(|mut from| std::fs::File::create(&tmp).and_then(|mut to| std::io::copy(&mut from, &mut to)))
        .and_then(|_| std::fs::rename(&tmp, &rolled));
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::OpenOptions::new().write(true).truncate(true).open(path).map(|_| ())
}

/// Open (create/append) the private log file, enforcing `0600` unconditionally (a file from before this fix, or opened
/// under a looser umask, won't self-correct otherwise). `None` on any failure: the caller logs to stdout only, as a
/// log-file problem shouldn't block the daemon from starting.
fn open_private_log_file() -> Option<Arc<Mutex<LogFile>>> {
    let dir = paths::state_dir();
    if let Err(e) = paths::ensure_private_dir(&dir) {
        eprintln!(
            "sotd: could not create state dir {}: {e} (logging to stdout only)",
            dir.display()
        );
        return None;
    }
    let path = dir.join("sotd.log");
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "sotd: could not open log file {}: {e} (logging to stdout only)",
                path.display()
            );
            return None;
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = file.set_permissions(std::fs::Permissions::from_mode(0o600)) {
            eprintln!("sotd: could not chmod log file {}: {e}", path.display());
        }
    }
    Some(Arc::new(Mutex::new(LogFile { file, path, cap: LOG_ROLL_BYTES, retry_above: 0 })))
}

/// Whether the log is copied to stdout: always when there is no private file to hold it, else unless stdout is a regular file
/// (a launcher's per-start log, which would grow without a bound beside the rolling `sotd.log`).
fn copies_to_stdout(file_open: bool, stdout_is_file: bool) -> bool {
    !file_open || !stdout_is_file
}

/// The daemon's log: events at the level `filter` passes (`main` gives the `RUST_LOG` level, default `info`), each masked of page secrets
/// (`sot_log::secret`) and written, without colour codes, through `TeeWriter` to the private file and, unless that file is
/// open and stdout is a regular file, to stdout.
fn log_subscriber(filter: EnvFilter, file: Option<Arc<Mutex<LogFile>>>) -> impl tracing::Subscriber + Send + Sync {
    let stdout = copies_to_stdout(file.is_some(), stdout_is_file());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(move || RedactingWriter(TeeWriter { file: file.clone(), stdout }))
        .finish()
}

/// Starts the daemon's log. When the private file is open and stdout is a regular file, which then holds only stderr,
/// one stderr line says where the log lines went: whatever launcher captured stderr into a file names the truth.
pub fn init(filter: EnvFilter) {
    let file = open_private_log_file();
    if let (Some(f), true) = (&file, stdout_is_file()) {
        if let Ok(f) = f.lock() {
            eprintln!("sotd: stdout is a file; log lines go to {}", f.path.display());
        }
    }
    log_subscriber(filter, file).init();
}

#[cfg(test)]
mod log_tests;
