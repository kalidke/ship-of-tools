use super::ChildTmp;
use sot_containment::Signal;
use crate::sidecars::contract_tests::{executable, within};
use std::time::Duration;
use tokio::process::Command;

/// A child that outlives the test unless it is ended.
fn long_child() -> Command {
    #[cfg(unix)]
    {
        let mut cmd = Command::new(executable("sleep"));
        cmd.arg("30");
        cmd
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new(executable("powershell"));
        cmd.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"]);
        cmd
    }
}

/// A folder with a file in it is removed once its child is ended. The order (after the reap) is `retire`'s own; this
/// test sees only that the folder goes.
#[tokio::test]
async fn retire_removes_the_folder_after_the_reap() {
    let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
    let tmp = ChildTmp::new().expect("temporary folder");
    let folder = tmp.dir.path().to_path_buf();
    std::fs::write(folder.join("made"), b"x").expect("setup: a file in the folder");
    let mut cmd = long_child();
    let mut child = sig.spawn(&mut cmd).expect("spawn the child");
    let status = tmp.retire(&mut child).await.expect("retire the child");
    assert!(!status.success(), "the child was ended, not left to exit: {status}");
    within(Duration::from_secs(30), "the folder is removed after the reap", || !folder.exists()).await;
}

/// Every child folder is made in this process's held folder of them, `sotd-julia-<random>` in its temporary folder,
/// which is made with the first and goes with the last. Run in a process of its own with a temporary folder of its
/// own, so no other test's child folder counts.
#[test]
fn the_held_folder_comes_with_the_first_child_folder_and_goes_with_the_last() {
    if !crate::sidecars::contract_tests::isolated(
        "sidecars::child::tmp::tests::the_held_folder_comes_with_the_first_child_folder_and_goes_with_the_last",
        Duration::from_secs(30),
    ) {
        return;
    }
    let owned = tempfile::tempdir().expect("owned temporary folder");
    for key in super::TMP_VARS {
        std::env::set_var(key, owned.path());
    }
    let first = ChildTmp::new().expect("a child folder");
    let second = ChildTmp::new().expect("a second child folder");
    let held = first.dir.path().parent().expect("its folder").to_path_buf();
    assert_eq!(held.parent(), Some(owned.path()), "{held:?}");
    let name = held.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    assert!(name.starts_with(super::PREFIX), "{held:?}");
    assert_eq!(second.dir.path().parent(), Some(held.as_path()), "one held folder for all of this process's child folders");
    drop(first);
    assert!(held.is_dir(), "the held folder stays while a child folder is in it");
    drop(second);
    let left: Vec<_> = std::fs::read_dir(owned.path()).expect("owned folder").flatten().map(|e| e.file_name()).collect();
    assert!(left.is_empty(), "the held folder goes with the last child folder: {left:?}");
}

/// Each end of a user-code child ends every process that holds its display's cookie, wherever its process group:
/// the owner's `retire` (a restart, the child's death, its row's end) and the drop of a `ChildTmp` that was never
/// retired. The case that would break it: a `retire` that ended the child and its folder but left the display to the
/// display's own end, so a process the child started detached ran on holding a cookie whose server was gone.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn every_end_of_a_user_code_child_ends_the_processes_that_hold_its_display() {
    use crate::sidecars::child::display::Display;
    use crate::sidecars::contract_tests::holding_display;
    executable("Xvfb");
    let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
    let theirs: &'static Signal = Box::leak(Box::new(Signal::new()));
    for route in ["retire", "drop"] {
        let mut tmp = ChildTmp::for_user_code(sig).await.expect("a user-code child's folder");
        let Some(Display::Up { ref cookie, .. }) = tmp.display else { panic!("the display must start where Xvfb is installed") };
        let cookie = cookie.to_path_buf();
        let mut cmd = long_child();
        tmp.apply(&mut cmd);
        let mut child = sig.spawn(&mut cmd).expect("start the child");
        // The child's own descendant, outside its process group, holding the cookie it was given.
        let mut detached = Command::new(executable("sleep"));
        detached.arg("300").env("XAUTHORITY", &cookie);
        let _held = theirs.spawn(&mut detached).expect("start a process that holds the cookie");
        within(Duration::from_secs(10), "setup: the child and the process hold the display", || {
            holding_display(&cookie).len() == 2
        })
        .await;
        if route == "retire" {
            tmp.retire(&mut child).await.expect("retire");
        } else {
            drop(child);
            drop(tmp);
        }
        let what = format!("the child's {route} ends what holds its display");
        within(Duration::from_secs(10), &what, || holding_display(&cookie).is_empty()).await;
    }
}
