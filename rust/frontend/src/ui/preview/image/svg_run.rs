//! The window's half of the drawing child's bounds (`svg_child`, `sot --draw-svg`), its start: `start` puts the child
//! in its cleared environment (`child_env`), with stderr null, behind the birth lock, and on Windows in a kill-on-close
//! job before it can be given any input. The bounds' constants live here. A leaf on std and platform: the test target
//! `svg_child` includes this file.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

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

/// A started child; its pipes are still in `child`.
pub(crate) struct Started {
    pub child: Child,
    /// Windows: the kill-on-close job the child was assigned to before it was given any input.
    #[cfg(windows)]
    pub job: sot_platform::host::winjob::AnonymousJob,
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
    #[allow(clippy::disallowed_methods, reason = "the window's SVG child: starts nothing, CPU-limited on Unix, in a kill-on-close job on Windows")]
    let child = {
        #[cfg(unix)]
        let _fork = sot_platform::host::fd::fork_guard();
        command.spawn()?
    };
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut started = Started {
        child,
        #[cfg(windows)]
        job,
    };
    #[cfg(windows)]
    if let Err(error) = started.job.assign(&started.child) {
        let _ = started.child.kill();
        let _ = started.child.wait();
        return Err(io::Error::other(error));
    }
    Ok(started)
}
