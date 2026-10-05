//! Every `sotd` subcommand, given `--help` or `-h`, prints its usage and exits 0 without
//! writing, dialing or exec'ing anything. The run is isolated from the live box: a cleared
//! environment, a private home/config/state/runtime tree, and fake `ssh`/`systemctl` that
//! leave a marker if anything dials.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

#[path = "support/sotd.rs"]
mod sotd;

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        out.push(p.clone());
        if p.is_dir() {
            files_under(&p, out);
        }
    }
}

fn listing(tmp: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    files_under(tmp, &mut v);
    v.sort();
    v
}

fn make_env(tmp: &Path) {
    for d in ["home", "cfg", "state", "fakebin"] {
        std::fs::create_dir_all(tmp.join(d)).unwrap();
    }
    std::fs::create_dir_all(tmp.join("run")).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(tmp.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
    for tool in ["ssh", "systemctl"] {
        let p = tmp.join("fakebin").join(tool);
        sot_log::test_exec::write_executable(&p, format!("#!/bin/sh\ntouch {}/DIALED\nexit 1\n", tmp.display()));
    }
}

fn sotd_help<S: AsRef<std::ffi::OsStr>>(tmp: &Path, args: &[S]) -> (i32, String, String) {
    let mut child = sotd::sotd_command()
        .args(args)
        .env_clear()
        .env("HOME", tmp.join("home"))
        .env("XDG_CONFIG_HOME", tmp.join("cfg"))
        .env("XDG_STATE_HOME", tmp.join("state"))
        .env("XDG_RUNTIME_DIR", tmp.join("run"))
        .env("PATH", format!("{}:/usr/bin:/bin", tmp.join("fakebin").display()))
        .current_dir(tmp.join("home"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn help_never_acts() {
    let td = tempfile::tempdir().unwrap();
    let tmp = td.path();
    make_env(tmp);
    let rows: &[&[&str]] = &[
        &["session-socket-path", "--help"],
        &["agent-exec", "--help"],
        &["ancestors", "--help"],
        &["stdio-bridge", "--help"],
        &["status", "--help"],
        &["topology", "--help"],
        &["topology", "plan", "--help"],
        &["topology", "status", "--help"],
        &["topology", "relay-endpoint", "--help"],
        &["topology", "relay-sockets", "--help"],
        &["topology", "sync", "--help"],
        &["topology", "refresh", "--help"],
        &["topology", "apply", "--help"],
        &["topology", "apply", "--yes", "--help"],
        &["topology", "apply", "--help", "--yes"],
        &["topology", "set", "--help"],
        &["topology", "set", "add", "--help"],
        &["topology", "set", "add", "h", "--help"],
        &["topology", "set", "remove", "--help"],
        &["--label", "x", "--help"],
        &["--help"],
    ];
    let before = listing(tmp);
    for row in rows {
        for flag in ["--help", "-h"] {
            let args: Vec<&str> = row.iter().map(|a| if *a == "--help" { flag } else { *a }).collect();
            let (code, stdout, stderr) = sotd_help(tmp, &args);
            assert_eq!(code, 0, "{args:?}: exit; stdout={stdout:?} stderr={stderr:?}");
            let second = stdout.lines().nth(1).unwrap_or("");
            assert!(
                second.starts_with("Usage") || second.starts_with("usage"),
                "{args:?}: line after the version line: {second:?}"
            );
            assert!(!tmp.join("DIALED").exists(), "{args:?}: dialed ssh or systemctl");
            assert_eq!(listing(tmp), before, "{args:?}: wrote files");
        }
    }
}

/// A value that is not UTF-8 never stops a help or version query from answering.
#[cfg(unix)]
#[test]
fn non_utf8_value_does_not_break_a_query() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let td = tempfile::tempdir().unwrap();
    let tmp = td.path();
    make_env(tmp);
    let bad = OsStr::from_bytes(b"\xff");
    for args in [vec![OsStr::new("--help"), OsStr::new("--project-root"), bad], vec![OsStr::new("--version"), bad]] {
        let (code, stdout, stderr) = sotd_help(tmp, &args);
        assert_eq!(code, 0, "{args:?}: exit; stdout={stdout:?} stderr={stderr:?}");
    }
}
