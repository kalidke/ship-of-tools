//! Unit tests for the daemon's log (daemon_log/mod.rs).
use super::*;

/// ADR 0049, User isolation: the daemon's log file never holds a page secret an event carries.
#[test]
fn the_daemon_log_masks_page_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sotd.log");
    let file = std::fs::File::create(&path).unwrap();
    let file = Some(Arc::new(Mutex::new(LogFile { file, path: path.clone(), cap: LOG_ROLL_BYTES, retry_above: 0 })));
    let subscriber = log_subscriber(EnvFilter::new("trace"), move || TeeWriter { file: file.clone(), stdout: false });
    let token = "0123456789abcdef0123456789abcdef";
    let _log = sot_platform::test_log::install(subscriber);
    tracing::error!(%token, "open http://127.0.0.1:1/x?secret=Ab12Cd34");
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("<redacted>"), "the event did not reach the file: {written}");
    assert!(!written.contains(token), "the token reached the file: {written}");
    assert!(!written.contains("Ab12Cd34"), "the secret reached the file: {written}");
    // No colour codes: they would split a field name from its `=`, so `secret=` would not read as one marker.
    assert!(!written.contains('\u{1b}'), "the file carries ANSI escapes: {written:?}");
}

use std::io::Write;

/// A file stdout loses the copy only while the private file holds the log: with no file, stdout is the only record.
#[test]
fn a_daemon_without_a_log_file_keeps_the_stdout_copy() {
    assert!(!copies_to_stdout(true, true));
    assert!(copies_to_stdout(true, false));
    assert!(copies_to_stdout(false, true));
    assert!(copies_to_stdout(false, false));
}

/// A regular file is a file; a pipe (what a launcher that captures the daemon's output through a parent hands it),
/// /dev/null and a socket are not.
#[test]
fn a_regular_file_is_a_file_and_a_pipe_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let file = std::fs::File::create(dir.path().join("f")).unwrap();
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `pipe` fills the two-int array it is given.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        assert!(fd_is_file(file.as_raw_fd()));
        assert!(!fd_is_file(fds[0]));
        assert!(!fd_is_file(fds[1]));
        assert!(!fd_is_file(-1));
        let null = std::fs::File::open("/dev/null").unwrap();
        assert!(!fd_is_file(null.as_raw_fd()));
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(!fd_is_file(socket.as_raw_fd()));
        // SAFETY: the two descriptors were opened above and are closed once.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "exit"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        assert!(handle_is_file(file.as_raw_handle() as _));
        assert!(!handle_is_file(child.stdout.as_ref().unwrap().as_raw_handle() as _));
        let nul = std::fs::OpenOptions::new().write(true).open("NUL").unwrap();
        assert!(!handle_is_file(nul.as_raw_handle() as _));
        let _ = child.wait();
    }
}

fn tee(path: &std::path::Path, cap: u64) -> TeeWriter {
    let file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    TeeWriter { file: Some(Arc::new(Mutex::new(LogFile { file, path: path.to_path_buf(), cap, retry_above: 0 }))), stdout: false }
}

/// A stdout that fails (a closed pipe) never stops the write to the log file.
#[test]
fn a_failing_stdout_never_stops_the_log_file() {
    struct Closed;
    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sotd.log");
    let mut daemon = tee(&path, LOG_ROLL_BYTES);
    daemon.stdout = true;
    assert_eq!(daemon.write_to(&mut Closed, b"one\n"), 4);
    assert_eq!(daemon.write_to(&mut Closed, b"two\n"), 4);
    assert_eq!(std::fs::read(&path).unwrap(), b"one\ntwo\n");
}

fn supervisor(path: &std::path::Path) -> std::fs::File {
    std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap()
}

/// Sizes the file through a handle that may set its length, as a daemon that ran for weeks has grown it; the
/// append handles that write to it are opened after (Windows gives an append-only handle no right to set the end).
fn sized(path: &std::path::Path, len: u64) {
    std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(path).unwrap().set_len(len).unwrap();
}

/// A log already over the cap when the daemon starts rolls at its first write: `.1` (replacing an older one)
/// holds only the last `cap` bytes, the base starts empty, and a writer that held the file open since before
/// keeps landing in the base.
#[test]
fn a_log_over_the_cap_rolls_after_the_first_write_and_a_held_writer_keeps_the_base() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sotd.log");
    let rolled = dir.path().join("sotd.log.1");
    std::fs::write(&rolled, b"older").unwrap();
    sized(&path, 10 * 1024);
    let mut held = supervisor(&path);
    let mut daemon = tee(&path, 4096);
    daemon.write_all(b"x").unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0, "the base is still the 10 KiB file");
    assert_eq!(std::fs::metadata(&rolled).unwrap().len(), 4096, ".1 holds the last cap bytes, not the whole log");
    held.write_all(b"note").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"note", "the held writer lands in the base");

    daemon.write_all(b"y").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"notey", "under the cap nothing rolls");
}

/// The daemon's own writer rolls the file after a write leaves it over the cap, and a supervisor holding the file
/// across two rolls still lands in the readable log.
#[test]
fn a_log_over_the_cap_rolls_within_a_run_and_every_writer_keeps_the_base() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sotd.log");
    let rolled = dir.path().join("sotd.log.1");
    let mut daemon = tee(&path, 4096);
    let mut sup = supervisor(&path);
    daemon.write_all(&vec![b'a'; 5 * 1024]).unwrap();
    assert_eq!(std::fs::read(&rolled).unwrap(), vec![b'a'; 4096], ".1 holds the last 4 KiB");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0, "the base holds only what follows the roll");
    sup.write_all(b"first").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"first", "the supervisor's bytes are in the base, not the .1");

    sup.write_all(&vec![b'b'; 300]).unwrap();
    daemon.write_all(&vec![b'c'; 3900]).unwrap();
    let second = std::fs::read(&rolled).unwrap();
    assert!(second.windows(2).any(|w| w == b"bb"), "the supervisor's lines are readable in .1");
    sup.write_all(b"second").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"second", "after two rolls the supervisor still lands in the base");
}

/// A roll that fails prints one message and is not copied again until the log has grown by another cap; a
/// success clears that.
#[test]
fn a_failing_roll_prints_once_and_is_not_retried_before_the_log_grows_by_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sotd.log");
    let rolled = dir.path().join("sotd.log.1");
    std::fs::create_dir(&rolled).unwrap(); // the rename over a folder always fails
    let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).unwrap();
    let mut log = LogFile { file, path: path.clone(), cap: 4096, retry_above: 0 };
    let write = |log: &mut LogFile, n: usize| {
        log.file.write_all(&vec![b'a'; n]).unwrap();
        log.roll_if_over()
    };
    let first = write(&mut log, 5000).expect("the first failure is reported");
    assert!(first.contains("could not roll"), "{first}");
    assert_eq!(log.retry_above, 5000 + 4096);
    for _ in 0..50 {
        assert!(write(&mut log, 10).is_none(), "no second message before the growth");
    }
    assert_eq!(log.retry_above, 5000 + 4096, "no second roll was tried");
    std::fs::remove_dir(&rolled).unwrap();
    assert!(write(&mut log, 4200).is_none(), "past the growth the roll is tried, and now succeeds");
    assert_eq!(std::fs::metadata(&rolled).unwrap().len(), 4096);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    assert_eq!(log.retry_above, 0, "a success clears the latch");
}

/// A writer that holds what the real subscriber wrote, shared so a test reads it after.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// The real subscriber, writing through `TeeWriter::write_to` so the file and stdout copies are both captured.
fn captured(filter: &str, dir: &std::path::Path) -> (impl tracing::Subscriber + Send + Sync, std::path::PathBuf, Capture) {
    let path = dir.join("sotd.log");
    let file = tee(&path, LOG_ROLL_BYTES).file;
    let out = Capture::default();
    let sink = out.clone();
    let subscriber = log_subscriber(EnvFilter::new(filter), move || {
        struct Both(TeeWriter, Capture);
        impl Write for Both {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                Ok(self.0.write_to(&mut self.1, buf))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        Both(TeeWriter { file: file.clone(), stdout: true }, sink.clone())
    });
    (subscriber, path, out)
}

/// A Slack token and a page secret an event carries reach neither the file nor stdout, whatever else the event holds.
#[test]
fn daemon_log_masks_slack_and_page_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let (subscriber, path, out) = captured("trace", dir.path());
    let _log = sot_platform::test_log::install(subscriber);
    let padding = "pad-zz ".repeat(700);
    tracing::error!(
        bot = "xoxb-not-a-real-token",
        app = %"xapp-not-a-real-token",
        other = 7,
        "open http://127.0.0.1:1/x?secret=Ab12Cd34&id=1 {padding}"
    );
    for (sink, text) in [("file", std::fs::read_to_string(&path).unwrap()), ("stdout", out.text())] {
        assert!(text.contains("<redacted>"), "{sink}: the event did not arrive: {text}");
        assert!(text.contains("pad-zz"), "{sink}: the padding is gone");
        for sentinel in ["xoxb-not-a-real-token", "xapp-not-a-real-token", "Ab12Cd34"] {
            assert!(!text.contains(sentinel), "{sink} holds {sentinel}: {text}");
        }
    }
}

/// No `RUST_LOG` setting, a directive naming the library included, lets a network library's own events in; this
/// crate's still arrive at the level the filter admits.
#[test]
fn no_log_level_lets_a_network_library_into_the_log() {
    for filter in ["trace", "hyper=trace", "hyper::client=trace"] {
        let dir = tempfile::tempdir().unwrap();
        let (subscriber, path, out) = captured(filter, dir.path());
        let _log = sot_platform::test_log::install(subscriber);
        tracing::error!(target: "hyper::client", "from-hyper");
        tracing::error!(target: "tungstenite::handshake", "from-tungstenite");
        tracing::error!(target: "reqwest", "from-reqwest");
        tracing::error!(target: "sotd::probe", "from-us");
        let file = std::fs::read_to_string(&path).unwrap();
        for text in [file, out.text()] {
            for library in ["from-hyper", "from-tungstenite", "from-reqwest"] {
                assert!(!text.contains(library), "{filter}: {library} reached the log: {text}");
            }
            // A directive naming only a network library admits no event of ours; the others do.
            assert_eq!(text.contains("from-us"), !filter.starts_with("hyper"), "{filter}: {text}");
        }
    }
}
