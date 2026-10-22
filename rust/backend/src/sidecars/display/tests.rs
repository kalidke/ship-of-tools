use super::{start_with, Display};
use sot_containment::Signal;
use crate::sidecars::contract_tests::{executable, holding_display, within, x11_answer, x11_cookie, x11_server};
use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Duration;

fn private_signal() -> &'static Signal {
    Box::leak(Box::new(Signal::new()))
}

/// A process that holds `cookie` as its `XAUTHORITY`, contained under `sig` in a process group of its own, outside
/// the display's, as Pluto's notebook worker and a detached process run outside their child's.
fn holder(sig: &'static Signal, cookie: &OsStr) -> sot_containment::Contained {
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
    sot_platform::test_exec::write_executable(&stub, "#!/bin/sh\necho 'Fatal server error: no screens found' >&2\nexit 1\n");
    let ended = start_with(stub.as_os_str(), &own("ended"), sig, Duration::from_secs(10)).await;
    let why = ended.err().expect("a server that exits names no display");
    assert!(why.ends_with("ended before naming a display: Fatal server error: no screens found"), "{why}");

    let deaf = folder.path().join("deaf");
    sot_platform::test_exec::write_executable(&deaf, "#!/bin/sh\nexec sleep 30\n");
    let silent = start_with(deaf.as_os_str(), &own("silent"), sig, Duration::from_millis(500)).await;
    let why = silent.err().expect("a server that never answers names no display");
    assert!(why.ends_with("named no display within 500ms"), "{why}");

    // A server that is still starting at the bound: the reason carries what it last wrote.
    let slow = folder.path().join("slow");
    let script = "#!/bin/sh\necho 'xkbcomp: still compiling the keymap' >&2\nexec sleep 30\n";
    sot_platform::test_exec::write_executable(&slow, script);
    let stalled = start_with(slow.as_os_str(), &own("stalled"), sig, Duration::from_millis(500)).await;
    let why = stalled.err().expect("a server that never answers names no display");
    assert!(why.ends_with("named no display within 500ms: xkbcomp: still compiling the keymap"), "{why}");
    assert_eq!(sig.live(), 0, "a failed start leaves no server");
}
