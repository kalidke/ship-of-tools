//! The window's half of the drawing child's bounds (`svg_child`, `sot --draw-svg`): `start` puts the child in its
//! cleared environment, with stderr null, behind the birth lock, and on Windows in a kill-on-close job before any input;
//! `finish` writes its request on one thread, reads and checks every reply (each header against its ask before the body, the end of
//! the stream after the last reply), holds it to the wall clock, always kills and reaps it, and turns each job into an
//! `Outcome` in the person's words; a `Stop` from the worker kills it early. The exit status wins over a pipe error. The reap after a kill is the one unbounded
//! wait (an uninterruptible sleep), as in containment's guarantee 4. A leaf on std and platform: the test target
//! `svg_child` includes this file.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::svg_wire::*;

/// The wall clock of one child: twice the dearest lawful drawing measured (a 15 MB scatter of 200000 circles under a
/// blur, 4.7 s at 12.6 Mpx).
pub(crate) const SVG_WALL: Duration = Duration::from_secs(10);
/// The child's heap, in the allocator's unit (requested bytes): the high-water mark of the dearest of three measured
/// drawings at 16 Mpx was 0.58 GB, so twice it, rounded up to a power of two.
pub(crate) const SVG_HEAP: u64 = 2 << 30;

/// What a child is held to. `cpu` (seconds) bounds a child whose window died mid-draw, which on macOS nothing else ends.
pub(crate) struct Bounds {
    pub wall: Duration,
    pub heap: u64,
    pub cpu: u64,
}

pub(crate) const SVG_BOUNDS: Bounds = Bounds { wall: SVG_WALL, heap: SVG_HEAP, cpu: SVG_WALL.as_secs() + 1 };

impl Bounds {
    /// The child's arguments: the mode, its two bounds, and `--version`, which a binary without the mode obeys.
    pub(crate) fn args(&self) -> [String; 4] {
        ["--draw-svg".into(), self.heap.to_string(), self.cpu.to_string(), "--version".into()]
    }
}

/// One picture: `svg` drawn onto exactly `size` pixels.
pub(crate) struct Job {
    pub size: (u32, u32),
    pub svg: Arc<[u8]>,
}

/// A job's end: its pixels, or the words a person reads. `Ended` is a child that hit a bound or crashed (the worker
/// counts these); `Refused` is anything else (the drawing's own first line of error, a bad reply, no start). `Stale`
/// is the worker's: the job was stopped or skipped because the pane moved on, so nothing is said of it.
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    Drawn { w: u32, h: u32, rgba: Vec<u8> },
    Refused(String),
    Ended(String),
    Stale,
}

/// What the thread waiting on a child hears: a reply, a reply that broke its form, or the stop.
enum Reply {
    Outcome(Outcome),
    Violation(String),
    Stopped,
    /// The reader's last word, sent when it ends: a `Stop` holds a sender, so the channel never closes by itself.
    Done,
}

struct EndOnDrop(Sender<Reply>);

impl Drop for EndOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(Reply::Done);
    }
}

/// The off switch of one draw, held by the thread that asked for it. `finish` arms it with its own channel, so a stop
/// wakes the wait at once; a stop before the arming is kept and met when it comes.
#[derive(Clone, Default)]
pub(crate) struct Stop(Arc<Mutex<(bool, Option<Sender<Reply>>)>>);

impl Stop {
    pub(crate) fn stop(&self) {
        let mut held = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        held.0 = true;
        if let Some(tx) = &held.1 {
            let _ = tx.send(Reply::Stopped);
        }
    }

    /// A stand-in draw's way to see its stop (a real one is woken through `finish`).
    #[cfg(test)]
    pub(crate) fn is_stopped(&self) -> bool {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).0
    }

    fn arm(&self, tx: Sender<Reply>) {
        let mut held = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if held.0 {
            let _ = tx.send(Reply::Stopped);
        }
        held.1 = Some(tx);
    }
}

const BAD_REPLY: &str = "the drawing's reply was not what was asked";

/// The window's own binary: on Linux the running image, even after an update replaced the file (a computer with no
/// `/proc` fails to start it, so every draw is refused), elsewhere the executable's path.
pub(crate) fn own_program() -> PathBuf {
    if cfg!(target_os = "linux") { "/proc/self/exe".into() } else { std::env::current_exe().unwrap_or_default() }
}

pub(crate) fn child_command(program: impl AsRef<OsStr>, bounds: &Bounds) -> Command {
    let mut command = Command::new(program);
    command.args(bounds.args());
    command
}

/// What the font code reads (fontdb and fontconfig-parser): the child's whole environment.
#[cfg(unix)]
const FONT_VARS: &[&str] = &["HOME", "FONTCONFIG_FILE", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"];
#[cfg(windows)]
const FONT_VARS: &[&str] = &["SYSTEMROOT", "USERPROFILE"];

/// `vars` cut to the font code's names (Windows spells them in any case).
pub(crate) fn child_env(vars: impl Iterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    let font = |name: &str| FONT_VARS.iter().any(|var| if cfg!(windows) { var.eq_ignore_ascii_case(name) } else { *var == name });
    vars.filter(|(name, _)| name.to_str().is_some_and(font)).collect()
}

/// A started child; its pipes are still in `child`. Dropping it kills and reaps the child, so no return or unwind
/// leaves one running (a kill after the reap is harmless).
pub(crate) struct Started {
    pub child: Child,
    /// Windows: the kill-on-close job the child was assigned to before it was given any input.
    #[cfg(windows)]
    pub job: sot_platform::host::winjob::AnonymousJob,
}

impl Drop for Started {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts `command` as the child (`child_command`'s, or a stand-in under test).
pub(crate) fn start(mut command: Command) -> io::Result<Started> {
    command.env_clear().envs(child_env(std::env::vars_os()));
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    // `sot.exe` is a console program, so a child would open a console of its own.
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000);
    #[cfg(windows)]
    let job = sot_platform::host::winjob::AnonymousJob::create().map_err(io::Error::other)?;
    #[allow(clippy::disallowed_methods, reason = "the window's SVG child: starts nothing, killed at its wall, CPU-limited on Unix, in a kill-on-close job on Windows")]
    let child = {
        #[cfg(unix)]
        let _fork = sot_platform::host::fd::fork_guard();
        command.spawn()?
    };
    let started = Started {
        child,
        #[cfg(windows)]
        job,
    };
    #[cfg(windows)]
    started.job.assign(&started.child).map_err(io::Error::other)?;
    Ok(started)
}

/// `jobs` drawn by the child `command` starts: one outcome per job, in order.
pub(crate) fn draw_in_child(command: Command, jobs: Vec<Job>, bounds: &Bounds, stop: &Stop) -> Vec<Outcome> {
    match start(command) {
        Ok(started) => finish(started, jobs, bounds, stop),
        Err(_) => refusals(jobs.len(), "cannot start the drawing"),
    }
}

fn refusals(count: usize, words: &str) -> Vec<Outcome> {
    (0..count).map(|_| Outcome::Refused(words.to_owned())).collect()
}

/// Writes `jobs` to `started`'s child and reads its replies, to the end of the stream or `bounds.wall`; the child is
/// dead and reaped when this returns. A reply that breaks its form refuses every job; a child that ends early costs the
/// jobs it did not answer, in the words of its end, and one the runner had to kill (it closed its stdout and lived) in
/// the wall's. A `stop` ends the wait at once: the jobs not yet answered are `Stale`. The two threads are not joined:
/// each ends when its pipe reports the child's end.
pub(crate) fn finish(mut started: Started, jobs: Vec<Job>, bounds: &Bounds, stop: &Stop) -> Vec<Outcome> {
    let count = jobs.len();
    let asks: Vec<_> = jobs.iter().map(|job| job.size).collect();
    let (stdin, stdout) = (started.child.stdin.take().unwrap(), started.child.stdout.take().unwrap());
    let (tx, rx) = mpsc::channel();
    let tx_stop = tx.clone();
    // Two threads, so that a child that never reads its input (a binary without the mode) is still read from.
    let writer = std::thread::Builder::new().spawn(move || drop(write_request(stdin, &jobs)));
    let reader = std::thread::Builder::new().spawn(move || {
        let _end = EndOnDrop(tx.clone());
        // Only a reply that broke its form is told; a pipe that ended is the exit status's to explain.
        if let Err(error) = read_replies(stdout, &asks, &tx) {
            if error.kind() == io::ErrorKind::InvalidData {
                let _ = tx.send(Reply::Violation(error.to_string()));
            }
        }
    });
    if writer.is_err() || reader.is_err() {
        return refusals(count, "cannot start the drawing");
    }
    stop.arm(tx_stop);
    let deadline = Instant::now() + bounds.wall;
    let (mut outcomes, mut violation, mut timed_out, mut stopped) = (Vec::new(), None, false, false);
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Reply::Outcome(outcome)) => outcomes.push(outcome),
            Ok(Reply::Violation(text)) => violation = Some(text),
            Ok(Reply::Stopped) => {
                stopped = true;
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                timed_out = true;
                break;
            }
            Ok(Reply::Done) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = started.child.kill();
    let status = started.child.wait();
    if let Some(words) = violation {
        return refusals(count, &words);
    }
    if outcomes.len() < count {
        let words = match status {
            _ if stopped => None,
            _ if timed_out || status.as_ref().is_ok_and(killed_by_us) => Some(format!("took past {} s", bounds.wall.as_secs())),
            Ok(status) if status.code() == Some(EXIT_HEAP) => Some(format!("needed more than {} MiB", bounds.heap >> 20)),
            Ok(status) => Some(format!("the drawing ended ({status})")),
            Err(error) => Some(format!("the drawing ended ({error})")),
        };
        outcomes.extend((outcomes.len()..count).map(|_| words.clone().map_or(Outcome::Stale, Outcome::Ended)));
    }
    outcomes
}

/// Whether `status` is the end the runner's own kill gives: the child had not ended when it was killed.
pub(crate) fn killed_by_us(status: &ExitStatus) -> bool {
    #[cfg(unix)]
    return std::os::unix::process::ExitStatusExt::signal(status) == Some(9);
    // `Child::kill` terminates a process with code 1; the child's own codes are 71 to 74.
    #[cfg(windows)]
    return status.code() == Some(1);
}

fn write_request(mut to: impl Write, jobs: &[Job]) -> io::Result<()> {
    to.write_all(&MAGIC)?;
    to.write_all(&(jobs.len() as u32).to_le_bytes())?;
    for job in jobs {
        let (w, h, len) = (job.size.0.to_le_bytes(), job.size.1.to_le_bytes(), (job.svg.len() as u64).to_le_bytes());
        for part in [&[ASK_STRETCH][..], &w, &h, &len, &job.svg] {
            to.write_all(part)?;
        }
    }
    Ok(())
}

/// One outcome sent per reply read, `asks` being each job's `(w, h)`. A reply that breaks its form is `InvalidData`,
/// whose text is the person's words: the magic, a size other than the ask's (a zero side is drawn as one), an error
/// text past its bound, a tag unknown, or a byte after the last reply. The body is read as it arrives, so a size the
/// child only claims allocates nothing.
fn read_replies(mut from: impl Read, asks: &[(u32, u32)], tx: &Sender<Reply>) -> io::Result<()> {
    if word::<4>(&mut from)? != MAGIC {
        return Err(invalid("not a drawing program"));
    }
    for &(w, h) in asks {
        let outcome = match word::<1>(&mut from)?[0] {
            REPLY_PIXELS => {
                let (rw, rh) = (u32::from_le_bytes(word(&mut from)?), u32::from_le_bytes(word(&mut from)?));
                let len = (rw as u64 * rh as u64).checked_mul(4).filter(|_| (rw, rh) == (w.max(1), h.max(1)));
                let Some(len) = len else { return Err(invalid(BAD_REPLY)) };
                let mut rgba = Vec::new();
                if from.by_ref().take(len).read_to_end(&mut rgba)? as u64 != len {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                Outcome::Drawn { w: rw, h: rh, rgba }
            }
            REPLY_ERROR => {
                let len = u16::from_le_bytes(word(&mut from)?) as usize;
                if len > ERROR_BYTES {
                    return Err(invalid(BAD_REPLY));
                }
                let mut text = vec![0; len];
                from.read_exact(&mut text)?;
                Outcome::Refused(String::from_utf8(text).map_err(|_| invalid(BAD_REPLY))?)
            }
            _ => return Err(invalid(BAD_REPLY)),
        };
        if tx.send(Reply::Outcome(outcome)).is_err() {
            return Ok(());
        }
    }
    if from.read(&mut [0])? != 0 {
        return Err(invalid(BAD_REPLY));
    }
    Ok(())
}
