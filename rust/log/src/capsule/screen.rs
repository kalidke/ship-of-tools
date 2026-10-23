//! The screen file: the leg's latest screen, kept so a restarted leg can show it again.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// How often, at most, the screen is written while output arrives.
pub(super) const SCREEN_PACE: Duration = Duration::from_secs(5);

type WriteFn = Box<dyn FnMut(&std::path::Path, &[u8]) -> std::io::Result<()> + Send>;

/// A test's replacement for the screen file's write, and its pace: the next `ScreenFile::new` takes it.
#[cfg(any(test, feature = "test-support"))]
static HOOK: Mutex<Option<(WriteFn, Duration)>> = Mutex::new(None);

/// Test seam: the next leg writes its screen through `write` at `pace` instead of `write_durable` every 5 s.
#[cfg(any(test, feature = "test-support"))]
pub fn set_screen_hook_for_test(write: WriteFn, pace: Duration) {
    *HOOK.lock().unwrap() = Some((write, pace));
}

/// Test seam (feature `test-support`): with `SOT_TEST_LEG_CRASH_AFTER_SAVE_WHEN` naming a file, the leg exits abruptly
/// (as a crash does) right after the next screen save that finds the file, removing it first: a test crashes the leg
/// it spawned without aiming at its pid. `SOT_TEST_SCREEN_PACE_MS` shortens the pace.
#[cfg(feature = "test-support")]
fn crash_if_asked() {
    if let Some(go) = std::env::var_os("SOT_TEST_LEG_CRASH_AFTER_SAVE_WHEN") {
        if std::fs::remove_file(&go).is_ok() {
            std::process::abort();
        }
    }
}

/// The newest snapshot waiting for the writer thread, and whether the thread should end.
struct Slot {
    latest: Option<Vec<u8>>,
    closing: bool,
}

struct Shared {
    slot: Mutex<Slot>,
    wake: Condvar,
    /// The last write failed: the next pace takes a fresh snapshot and tries again.
    failed: AtomicBool,
}

/// Keeps `<state_dir>/screen` from the leg's own parser. The leg hands the newest snapshot to one writer thread
/// through a slot that keeps only the newest, so a stalled or failing disk never reaches the output loop; a failed
/// write is noted once per failing streak and tried again at the next pace.
pub(super) struct ScreenFile {
    pace: Duration,
    last_write: Instant,
    dirty: bool,
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ScreenFile {
    pub(super) fn new(path: PathBuf, now: Instant) -> Self {
        #[cfg(any(test, feature = "test-support"))]
        if let Some((write, pace)) = HOOK.lock().unwrap().take() {
            let mut file = Self::with_writer(path, now, write);
            file.pace = pace;
            return file;
        }
        Self::with_writer(path, now, Box::new(|p, b| sot_platform::host::write_durable(p, b)))
    }

    pub(super) fn with_writer(path: PathBuf, now: Instant, mut write: WriteFn) -> Self {
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot { latest: None, closing: false }),
            wake: Condvar::new(),
            failed: AtomicBool::new(false),
        });
        let thread = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let mut failing = false;
                loop {
                    let bytes = {
                        let mut slot = shared.slot.lock().unwrap();
                        loop {
                            if let Some(bytes) = slot.latest.take() {
                                break bytes;
                            }
                            if slot.closing {
                                return;
                            }
                            slot = shared.wake.wait(slot).unwrap();
                        }
                    };
                    match write(&path, &bytes) {
                        Ok(()) => {
                            #[cfg(feature = "test-support")]
                            crash_if_asked();
                            failing = false;
                            shared.failed.store(false, Ordering::SeqCst);
                        }
                        Err(e) => {
                            shared.failed.store(true, Ordering::SeqCst);
                            if !failing {
                                failing = true;
                                leg_note(format_args!(
                                    "sot-capsule: the screen file {path:?} could not be written ({e}); trying again at the next pace"
                                ));
                            }
                        }
                    }
                }
            })
        };
        #[allow(unused_mut)]
        let mut pace = SCREEN_PACE;
        #[cfg(feature = "test-support")]
        if let Some(ms) = std::env::var("SOT_TEST_SCREEN_PACE_MS").ok().and_then(|v| v.parse().ok()) {
            pace = Duration::from_millis(ms);
        }
        Self { pace, last_write: now, dirty: false, shared, thread: Some(thread) }
    }

    /// The screen changed since the last snapshot was handed over.
    pub(super) fn output_arrived(&mut self) {
        self.dirty = true;
    }

    /// Hands the screen to the writer when it changed and the pace has passed; a failed write marks it changed again.
    pub(super) fn tick(&mut self, parser: &vt100_ctt::Parser, now: Instant) {
        if self.shared.failed.swap(false, Ordering::SeqCst) {
            self.dirty = true;
        }
        if self.dirty && now.duration_since(self.last_write) >= self.pace {
            self.hand_over(parser, now);
        }
    }

    /// The agent's end: hands over the screen if it changed, then waits for the writer, at most `bound`.
    pub(super) fn finish(&mut self, parser: &vt100_ctt::Parser, now: Instant, bound: Duration) {
        if self.dirty {
            self.hand_over(parser, now);
        }
        self.shared.slot.lock().unwrap().closing = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            // A writer still stalled at the bound is left behind: the leg's end never waits on a disk.
            crate::lane::transport::join_within(thread, Instant::now() + bound);
        }
    }

    fn hand_over(&mut self, parser: &vt100_ctt::Parser, now: Instant) {
        self.last_write = now;
        match parser.screen().checkpoint() {
            Ok(bytes) => {
                self.shared.slot.lock().unwrap().latest = Some(bytes);
                self.shared.wake.notify_all();
                self.dirty = false;
            }
            Err(e) => leg_note(format_args!("sot-capsule: the screen cannot be checkpointed ({e})")),
        }
    }
}

impl Drop for ScreenFile {
    fn drop(&mut self) {
        self.shared.slot.lock().unwrap().closing = true;
        self.shared.wake.notify_all();
    }
}

/// Restores the screen the file at `path` holds into `parser`, then sets its geometry and writes the restart line
/// below the retained content: out of the alternate screen, scrolling margins reset, the cursor on the last row. A
/// missing file leaves the parser as it is; one that does not restore (torn, another format version) is removed and
/// the screen starts empty. Returns whether a screen was restored.
///
/// On Windows the ConPTY host repaints (clears) the visible screen when the agent starts, so the retained screen and
/// the restart line are scrolled into the history first, where the repaint leaves them.
pub(super) fn restore(path: &std::path::Path, parser: &mut vt100_ctt::Parser, rows: u16, cols: u16, at: SystemTime) -> bool {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(e) => {
            leg_note(format_args!("sot-capsule: the screen file {path:?} could not be read ({e}); starting empty"));
            return false;
        }
    };
    if let Err(e) = parser.restore_screen(&bytes) {
        leg_note(format_args!("sot-capsule: the screen file {path:?} does not restore ({e}); removing it"));
        let _ = std::fs::remove_file(path);
        return false;
    }
    parser.screen_mut().set_size(rows, cols);
    // Below the last row that holds anything; on a full screen the line scrolls the top row into the history.
    let screen = parser.screen();
    let used = (0..rows).rev().find(|&r| (0..cols).any(|c| screen.cell(r, c).is_some_and(|cell| !cell.contents().trim().is_empty())));
    let stamp = utc_stamp(at);
    match used {
        Some(r) if r + 1 >= rows => {
            parser.process(format!("\x1b[?1049l\x1b[0m\x1b[r\x1b[{rows};1H\r\n[restarted {stamp}]\r\n").as_bytes());
        }
        _ => {
            let row = used.map_or(1, |r| r + 2);
            parser.process(format!("\x1b[?1049l\x1b[0m\x1b[r\x1b[{row};1H[restarted {stamp}]\r\n").as_bytes());
        }
    }
    if cfg!(windows) {
        parser.process("\r\n".repeat(usize::from(rows)).as_bytes());
    }
    true
}

/// `YYYY-MM-DD HH:MM:SS UTC` for `t`.
fn utc_stamp(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days since 1970-01-01 to a civil date (Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC", rem / 3_600, rem % 3_600 / 60, rem % 60)
}

#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[path = "screen_tests.rs"]
mod tests;
