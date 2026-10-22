//! The screen file: the leg's latest screen, kept so a restarted leg can show it again.
use super::*;

/// How often, at most, the screen is written while output arrives.
pub(super) const SCREEN_PACE: Duration = Duration::from_secs(5);

type WriteFn = Box<dyn FnMut(&std::path::Path, &[u8]) -> std::io::Result<()> + Send>;

/// Writes `<state_dir>/screen` from the leg's own parser. A failed write is noted and tried again at the next
/// pace; it never reaches the output path.
pub(super) struct ScreenFile {
    path: PathBuf,
    write: WriteFn,
    last_write: Instant,
    dirty: bool,
    /// The last write failed; the failure was noted once for the streak.
    failing: bool,
    /// Failures noted so far (a test reads it).
    pub(super) notes: u32,
}

impl ScreenFile {
    pub(super) fn new(path: PathBuf, now: Instant) -> Self {
        Self::with_writer(path, now, Box::new(|p, b| sot_platform::host::write_durable(p, b)))
    }

    pub(super) fn with_writer(path: PathBuf, now: Instant, write: WriteFn) -> Self {
        Self { path, write, last_write: now, dirty: false, failing: false, notes: 0 }
    }

    /// The screen changed since the last write.
    pub(super) fn output_arrived(&mut self) {
        self.dirty = true;
    }

    /// Writes the screen when it changed and the pace has passed.
    pub(super) fn tick(&mut self, parser: &vt100_ctt::Parser, now: Instant) {
        if self.dirty && now.duration_since(self.last_write) >= SCREEN_PACE {
            self.save(parser, now);
        }
    }

    /// Writes the screen now if it changed: the agent's end and stop.
    pub(super) fn flush(&mut self, parser: &vt100_ctt::Parser, now: Instant) {
        if self.dirty {
            self.save(parser, now);
        }
    }

    fn save(&mut self, parser: &vt100_ctt::Parser, now: Instant) {
        self.last_write = now;
        let result = match parser.screen().checkpoint() {
            Ok(bytes) => (self.write)(&self.path, &bytes),
            Err(e) => Err(std::io::Error::other(e.to_string())),
        };
        match result {
            Ok(()) => {
                self.dirty = false;
                self.failing = false;
            }
            Err(e) => {
                if !self.failing {
                    self.failing = true;
                    self.notes += 1;
                    leg_note(format_args!(
                        "sot-capsule: the screen file {:?} could not be written ({e}); trying again at the next pace",
                        self.path
                    ));
                }
            }
        }
    }
}

/// Restores the screen the file at `path` holds into `parser`, then sets its geometry, and writes the restart line
/// below it. A missing file leaves the parser as it is; one that does not restore (torn, another format version) is
/// removed and the screen starts empty. Returns whether a screen was restored.
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
    parser.process(format!("\x1b[0m\r\n[restarted {}]\r\n", utc_stamp(at)).as_bytes());
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
