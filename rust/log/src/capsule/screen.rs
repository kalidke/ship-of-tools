//! The screen file: the leg's latest screen, kept so a restarted leg can show it again.
use super::*;

/// How often, at most, the screen is written while output arrives.
pub(super) const SCREEN_PACE: Duration = Duration::from_secs(5);

/// Writes `<state_dir>/screen` from the leg's own parser. A failed write is noted and tried again at the next
/// pace; it never reaches the output path.
pub(super) struct ScreenFile {
    path: PathBuf,
    write: Box<dyn FnMut(&std::path::Path, &[u8]) -> std::io::Result<()> + Send>,
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

    pub(super) fn with_writer(
        path: PathBuf,
        now: Instant,
        write: Box<dyn FnMut(&std::path::Path, &[u8]) -> std::io::Result<()> + Send>,
    ) -> Self {
        Self { path, write, last_write: now, dirty: false, failing: false, notes: 0 }
    }

    /// Output reached the parser since the last write.
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
        let _ = (parser, now);
        unimplemented!("the screen file is written in the next commit")
    }
}

#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[path = "screen_tests.rs"]
mod tests;
