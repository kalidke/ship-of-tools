//! A user-code child's own X display (Linux): an Xvfb that only the child's cookie opens, started before the child,
//! named in its environment, watched while the child lives, and ended after it, together with every process that holds
//! its cookie.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use sot_protocol::child_lines::ChildLines;
use tokio::process::Command;

use sot_containment::{Contained, Signal};

mod holders;

/// How long a starting Xvfb may take to name its display, which it does once it accepts connections (`-displayfd`):
/// 50 to 150 ms on the hosts measured.
const START_BOUND: Duration = Duration::from_secs(10);

/// A child's display: a running server, or why there is none.
pub(crate) enum Display {
    /// Xvfb on `:number`, which only the cookie in the file `cookie` opens. `cookie` comes first, so a display dropped
    /// without its `end` ends what holds its cookie before its server's group.
    Up { cookie: Cookie, server: Contained, number: u32 },
    /// No server, and why. The child is told (`SOT_NO_DISPLAY`), and the REPL shim says it at its first eval.
    Missing(String),
}

impl Display {
    /// Writes a new cookie into `folder` (the child's own temporary folder, readable by this account alone) and starts
    /// an Xvfb that only that cookie opens. A failure leaves the child without a display and says why in the daemon's
    /// log; it never fails the child's start.
    pub(crate) async fn start(folder: &Path, sig: &'static Signal) -> Self {
        match start_with(OsStr::new("Xvfb"), folder, sig, START_BOUND).await {
            Ok((server, number, cookie)) => Display::Up { cookie: Cookie(cookie), server, number },
            Err(why) => {
                tracing::warn!(reason = %why, "a Julia child starts without a display");
                Display::Missing(why)
            }
        }
    }

    /// Names the display in `cmd`'s environment, replacing any display the daemon inherited: `DISPLAY`,
    /// `XAUTHORITY`, and `XDG_SESSION_TYPE=x11`, without which GLFW 3.4 tries a Wayland session's default socket
    /// before X11. With no server it adds only `SOT_NO_DISPLAY`, the reason.
    pub(crate) fn apply(&self, cmd: &mut Command) {
        match self {
            Display::Up { number, cookie, .. } => {
                cmd.env("DISPLAY", format!(":{number}"))
                    .env("XAUTHORITY", cookie.as_os_str())
                    .env("XDG_SESSION_TYPE", "x11");
            }
            Display::Missing(why) => {
                cmd.env("SOT_NO_DISPLAY", why);
            }
        }
    }

    /// Returns once the server has exited, observed and left unreaped (`end` reaps it), having ended every process
    /// that holds its cookie, the child among them; never, when there is no server. Its owner then retires the child:
    /// once the server is gone, any account can bind the display's abstract name, and a child that opened its display
    /// only then would send its cookie and its pixels to that account's server. An exit that cannot be observed counts
    /// as an end.
    pub(crate) async fn ended(&mut self) {
        match self {
            Display::Up { cookie, server, number } => {
                if let Err(e) = server.wait_until_exited().await {
                    tracing::warn!(display = *number, error = %e, "a child's display server's exit could not be observed");
                }
                tracing::warn!(display = *number, "a child's display ended under it; the child is ended with it");
                // On this task, not `spawn_blocking`: the walk ends the child, and its owner's select must see this return
                // before the child's exit, or it names the death of the child and not the end of the display.
                holders::end(cookie);
            }
            Display::Missing(_) => std::future::pending().await,
        }
    }

    /// Ends every process that holds the cookie (the cookie's drop), then the server: its process group, then its
    /// reap. A failed request is logged; the server's `Contained` has already asked for its group's end either way.
    pub(crate) async fn end(self) {
        if let Display::Up { cookie, mut server, number } = self {
            // What holds the cookie ends first, while the server still holds the display's name, which no account
            // can bind until the server ends.
            // The cookie's drop is the walk, so it runs off the runtime's workers; a task that never runs still drops it.
            tokio::task::spawn_blocking(move || drop(cookie)).await.ok();
            if let Err(e) = server.kill().await {
                tracing::warn!(display = number, error = %e, "a child's display server is not confirmed ended");
            }
        }
    }
}

/// A running display's cookie file. Its drop ends every process of this account that still names it as its
/// `XAUTHORITY` ([`holders::end`]), so however the display ends, nothing that could open its name outlives it.
pub(crate) struct Cookie(PathBuf);

impl std::ops::Deref for Cookie {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Cookie {
    fn drop(&mut self) {
        holders::end(&self.0);
    }
}

/// Starts `program` as an Xvfb whose cookie is a new file in `folder`, and waits at most `bound` for the display
/// number it writes on its stdout once it accepts connections. Its arguments: `-displayfd 1` takes the lowest free
/// display number (a number another server holds is never taken) and makes no lock file; `-nolisten unix` leaves only
/// the abstract socket, so a killed server leaves no file in `/tmp/.X11-unix`; `-auth` makes the cookie the only way in,
/// and `-noreset` keeps the cookie it read at the start (a server resets when its last client leaves and reads the file
/// again, so a removed file would lock the child out); no TCP. Its stderr goes to a file beside the cookie, whose last
/// lines a failed start names, a start past its bound included.
async fn start_with(
    program: &OsStr,
    folder: &Path,
    sig: &'static Signal,
    bound: Duration,
) -> Result<(Contained, u32, PathBuf), String> {
    let name = program.to_string_lossy();
    let cookie = folder.join("Xauthority");
    write_cookie(&cookie).map_err(|e| format!("its cookie was not written: {e}"))?;
    let log = folder.join("Xvfb.log");
    let stderr = std::fs::File::create(&log).map_err(|e| format!("its log was not created: {e}"))?;
    let mut cmd = Command::new(program);
    cmd.args(["-displayfd", "1", "-auth"])
        .arg(&cookie)
        .args(["-noreset", "-nolisten", "tcp", "-nolisten", "unix", "-screen", "0", "3840x2160x24"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr);
    let mut server = sig.spawn(&mut cmd).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("{name} is not installed: none on the daemon's PATH"),
        _ => format!("{name} did not start: {e}"),
    })?;
    let mut stdout = ChildLines::new(server.stdout.take().expect("the server's stdout is piped"));
    let why = match tokio::time::timeout(bound, stdout.next_line()).await {
        Ok(Ok(Some(line))) => match line.trim().parse::<u32>() {
            Ok(number) => return Ok((server, number, cookie)),
            Err(_) => format!("{name} named no display number: {:?}", line.trim()),
        },
        Ok(_) => format!("{name} ended before naming a display: {}", last_lines(&log)),
        Err(_) => match last_lines(&log) {
            said if said.is_empty() => format!("{name} named no display within {bound:?}"),
            said => format!("{name} named no display within {bound:?}: {said}"),
        },
    };
    if let Err(e) = server.kill().await {
        tracing::warn!(error = %e, "a display server that failed to start is not confirmed ended");
    }
    Err(why)
}

/// The last three lines of the server's log, joined: the reason Xvfb gives for a failed start.
fn last_lines(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    super::child::last_lines(text.lines().filter(|l| l.trim() != "(EE)"), 3, usize::MAX)
}

/// One Xauthority entry that every address and display number match (family FamilyWild, 0xFFFF, with an empty address
/// and number), so the server (`-auth`) and every client read one file written before the number is known:
/// `MIT-MAGIC-COOKIE-1` with 16 bytes from the OS's secure generator, in a new file only this account can read.
fn write_cookie(path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut secret = [0u8; 16];
    getrandom::fill(&mut secret).map_err(std::io::Error::from)?;
    let mut entry = 0xFFFFu16.to_be_bytes().to_vec();
    for field in [&b""[..], b"", b"MIT-MAGIC-COOKIE-1", &secret] {
        entry.extend_from_slice(&u16::try_from(field.len()).expect("a short field").to_be_bytes());
        entry.extend_from_slice(field);
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(&entry)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
