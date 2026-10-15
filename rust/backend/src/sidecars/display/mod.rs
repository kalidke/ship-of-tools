//! A user-code child's own X display (Linux): an Xvfb that only the child's cookie opens, started before the child,
//! named in its environment, watched while the child lives, and ended after it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use sot_protocol::child_lines::ChildLines;
use tokio::process::Command;

use crate::lifecycle::child_signal::{Contained, Signal};

/// How long a starting Xvfb may take to name its display, which it does once it accepts connections (`-displayfd`):
/// 50 to 150 ms on the hosts measured.
const START_BOUND: Duration = Duration::from_secs(10);

/// A child's display: a running server, or why there is none.
pub(crate) enum Display {
    /// Xvfb on `:number`, which only the cookie in the file `cookie` opens.
    Up { server: Contained, number: u32, cookie: PathBuf },
    /// No server, and why. The child is told (`SOT_NO_DISPLAY`), and the REPL shim says it at its first eval.
    Missing(String),
}

impl Display {
    /// Writes a new cookie into `folder` (the child's own temporary folder, readable by this account alone) and starts
    /// an Xvfb that only that cookie opens. A failure leaves the child without a display and says why in the daemon's
    /// log; it never fails the child's start.
    pub(crate) async fn start(folder: &Path, sig: &'static Signal) -> Self {
        match start_with(OsStr::new("Xvfb"), folder, sig, START_BOUND).await {
            Ok((server, number, cookie)) => Display::Up { server, number, cookie },
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
                    .env("XAUTHORITY", cookie)
                    .env("XDG_SESSION_TYPE", "x11");
            }
            Display::Missing(why) => {
                cmd.env("SOT_NO_DISPLAY", why);
            }
        }
    }

    /// Returns once the server has exited, observed and left unreaped (`end` reaps it); never, when there is no
    /// server. Its owner then ends the child: once the server is gone, any account can bind the display's abstract
    /// name, and a child that opened its display only then would send its cookie and its pixels to that account's
    /// server. An exit that cannot be observed counts as an end.
    pub(crate) async fn ended(&mut self) {
        match self {
            Display::Up { server, number, .. } => {
                if let Err(e) = server.wait_until_exited().await {
                    tracing::warn!(display = *number, error = %e, "a child's display server's exit could not be observed");
                }
                tracing::warn!(display = *number, "a child's display ended under it; the child is ended with it");
            }
            Display::Missing(_) => std::future::pending().await,
        }
    }

    /// Ends the server: its process group, then its reap. A failed request is logged; the server's `Contained` has
    /// already asked for its group's end either way.
    pub(crate) async fn end(self) {
        if let Display::Up { mut server, number, .. } = self {
            if let Err(e) = server.kill().await {
                tracing::warn!(display = number, error = %e, "a child's display server is not confirmed ended");
            }
        }
    }
}

/// Starts `program` as an Xvfb whose cookie is a new file in `folder`, and waits at most `bound` for the display
/// number it writes on its stdout once it accepts connections. Its arguments: `-displayfd 1` takes the lowest free
/// display number (a number another server holds is never taken) and makes no lock file; `-nolisten unix` leaves only
/// the abstract socket, so a killed server leaves no file in `/tmp/.X11-unix`; `-auth` makes the cookie the only way in,
/// and `-noreset` keeps the cookie it read at the start (a server resets when its last client leaves and reads the file
/// again, so a removed file would lock the child out); no TCP. Its stderr goes to a file beside the cookie, the reason
/// a failed start names.
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
        Err(_) => format!("{name} named no display within {bound:?}"),
    };
    if let Err(e) = server.kill().await {
        tracing::warn!(error = %e, "a display server that failed to start is not confirmed ended");
    }
    Err(why)
}

/// The last three lines of the server's log, joined: the reason Xvfb gives for a failed start.
fn last_lines(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && *l != "(EE)").collect();
    lines[lines.len().saturating_sub(3)..].join(" / ")
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
mod tests {
    use super::{start_with, Display};
    use crate::lifecycle::child_signal::Signal;
    use crate::sidecars::contract_tests::{executable, holding_display, within, x11_answer, x11_cookie, x11_server};
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::time::Duration;

    fn private_signal() -> &'static Signal {
        Box::leak(Box::new(Signal::new()))
    }

    /// A process that holds `cookie` as its `XAUTHORITY`, contained under `sig` in a process group of its own, outside
    /// the display's, as Pluto's notebook worker and a detached process run outside their child's.
    fn holder(sig: &'static Signal, cookie: &OsStr) -> crate::lifecycle::child_signal::Contained {
        let mut cmd = tokio::process::Command::new(executable("sleep"));
        cmd.arg("300").env("XAUTHORITY", cookie);
        sig.spawn(&mut cmd).expect("start a process that holds a cookie")
    }

    /// A started display answers its own cookie, refuses a client with none or another, and once ended answers
    /// nothing with that cookie: the end takes the server (its process group), not just the handle.
    #[tokio::test]
    async fn a_display_opens_only_to_its_cookie_and_ends_with_its_owner() {
        executable("Xvfb");
        let sig = private_signal();
        let folder = tempfile::tempdir().expect("an owned folder");
        let display = Display::start(folder.path(), sig).await;
        let Display::Up { number, ref cookie, .. } = display else {
            panic!("the display must start where Xvfb is installed")
        };
        let secret = x11_cookie(cookie);
        assert_eq!(x11_answer(number, Some(&secret)), Some(1), "its own cookie opens the display");
        assert_eq!(x11_answer(number, None), Some(0), "a client with no cookie is refused");
        assert_eq!(x11_answer(number, Some(&[7u8; 16])), Some(0), "another cookie is refused");
        assert_eq!(sig.live(), 1, "the server is contained under the Signal it was given");
        display.end().await;
        assert_eq!(sig.live(), 0, "the end reaps the server");
        assert_ne!(x11_answer(number, Some(&secret)), Some(1), "an ended display answers its cookie no more");
    }

    /// The Signal's fire (the daemon's stop) ends a display the owner never ended.
    #[tokio::test]
    async fn a_fired_signal_ends_a_display() {
        executable("Xvfb");
        let sig = private_signal();
        let folder = tempfile::tempdir().expect("an owned folder");
        let display = Display::start(folder.path(), sig).await;
        let Display::Up { number, ref cookie, .. } = display else {
            panic!("the display must start where Xvfb is installed")
        };
        let secret = x11_cookie(cookie);
        sig.fire().expect("fire");
        within(Duration::from_secs(30), "a fired Signal's display answers its cookie no more", || {
            x11_answer(number, Some(&secret)) != Some(1)
        })
        .await;
        drop(display);
    }

    /// A server that exits under its owner is seen to end (`ended`), having ended at once what holds its cookie, and
    /// its end still reaps it; a missing display never ends.
    #[tokio::test]
    async fn a_display_whose_server_exits_is_seen_to_end() {
        executable("Xvfb");
        let sig = private_signal();
        let folder = tempfile::tempdir().expect("an owned folder");
        let mut display = Display::start(folder.path(), sig).await;
        let Display::Up { ref cookie, .. } = display else {
            panic!("the display must start where Xvfb is installed")
        };
        let server = x11_server(cookie);
        let path = cookie.to_path_buf();
        let _holds = holder(private_signal(), path.as_os_str());
        within(Duration::from_secs(10), "setup: a process holds the display", || {
            holding_display(&path).len() == 1
        })
        .await;
        let mut missing = Display::Missing("Xvfb is not installed: none on the daemon's PATH".into());
        assert!(
            tokio::time::timeout(Duration::from_millis(300), display.ended()).await.is_err(),
            "a live server has not ended"
        );
        // SAFETY: a signal to the X server this test started.
        assert_eq!(unsafe { libc::kill(server as libc::pid_t, libc::SIGKILL) }, 0, "kill the server");
        tokio::time::timeout(Duration::from_secs(10), display.ended())
            .await
            .expect("a killed server is seen to end");
        within(Duration::from_secs(10), "its server's end ends what holds its cookie", || {
            holding_display(&path).is_empty()
        })
        .await;
        assert_eq!(sig.live(), 1, "seen, not reaped: the owner's end reaps it");
        display.end().await;
        assert_eq!(sig.live(), 0, "the end reaps the server");
        assert!(
            tokio::time::timeout(Duration::from_millis(300), missing.ended()).await.is_err(),
            "a missing display never ends"
        );
    }

    /// A display's end, and its drop, end every process that holds its cookie, in no process group of the display's,
    /// and no other: not one whose cookie path only begins with this one's.
    #[tokio::test]
    async fn a_display_ends_every_process_that_holds_its_cookie() {
        executable("Xvfb");
        let sig = private_signal();
        let theirs = private_signal();
        let folder = tempfile::tempdir().expect("an owned folder");
        for route in ["end", "drop"] {
            // Each start writes a new cookie, so each gets a folder of its own.
            let own = folder.path().join(route);
            std::fs::create_dir(&own).expect("a start's own folder");
            let display = Display::start(&own, sig).await;
            let Display::Up { ref cookie, .. } = display else {
                panic!("the display must start where Xvfb is installed")
            };
            let cookie = cookie.to_path_buf();
            let other = PathBuf::from(format!("{}-other", cookie.display()));
            let (_holds, _other) = (holder(theirs, cookie.as_os_str()), holder(theirs, other.as_os_str()));
            within(Duration::from_secs(10), "setup: a process holds the display", || {
                holding_display(&cookie).len() == 1
            })
            .await;
            if route == "end" {
                display.end().await;
            } else {
                drop(display);
            }
            let what = format!("the display's {route} ends what holds its cookie");
            within(Duration::from_secs(10), &what, || holding_display(&cookie).is_empty()).await;
            assert_eq!(holding_display(&other).len(), 1, "a process that holds another cookie is not ended");
        }
    }

    /// A child with no display is told why (the REPL shim says it at its first eval), and nothing it inherited is
    /// taken away.
    #[test]
    fn a_missing_display_names_its_reason_to_the_child() {
        let mut cmd = tokio::process::Command::new("julia");
        Display::Missing("Xvfb is not installed: none on the daemon's PATH".into()).apply(&mut cmd);
        let set: Vec<(String, Option<String>)> = cmd
            .as_std()
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned())))
            .collect();
        assert_eq!(
            set,
            [("SOT_NO_DISPLAY".to_owned(), Some("Xvfb is not installed: none on the daemon's PATH".to_owned()))]
        );
    }

    /// Each way a start fails is a reason, never a panic or a wait past its bound, and leaves nothing running.
    #[tokio::test]
    async fn a_failed_start_says_why() {
        let sig = private_signal();
        let folder = tempfile::tempdir().expect("an owned folder");
        let missing = start_with(OsStr::new("sot-no-such-xvfb"), folder.path(), sig, Duration::from_secs(10)).await;
        assert_eq!(missing.err().as_deref(), Some("sot-no-such-xvfb is not installed: none on the daemon's PATH"));

        // Each start writes a new cookie, so each gets a folder of its own.
        let own = |name: &str| {
            let dir = folder.path().join(name);
            std::fs::create_dir(&dir).expect("a start's own folder");
            dir
        };
        let stub = folder.path().join("stub");
        sot_log::test_exec::write_executable(&stub, "#!/bin/sh\necho 'Fatal server error: no screens found' >&2\nexit 1\n");
        let ended = start_with(stub.as_os_str(), &own("ended"), sig, Duration::from_secs(10)).await;
        let why = ended.err().expect("a server that exits names no display");
        assert!(why.ends_with("ended before naming a display: Fatal server error: no screens found"), "{why}");

        let deaf = folder.path().join("deaf");
        sot_log::test_exec::write_executable(&deaf, "#!/bin/sh\nexec sleep 30\n");
        let silent = start_with(deaf.as_os_str(), &own("silent"), sig, Duration::from_millis(500)).await;
        let why = silent.err().expect("a server that never answers names no display");
        assert!(why.ends_with("named no display within 500ms"), "{why}");
        assert_eq!(sig.live(), 0, "a failed start leaves no server");
    }
}
