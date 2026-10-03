#![cfg(target_os = "linux")]
//! `sotd topology refresh` on a scratch hub: the real binary, a stand-in `systemctl` first on PATH. Wiring-level.
use {std::path::PathBuf, tempfile::TempDir};

const STALE_SERVICE: &str = include_str!("../../protocol/src/testdata/relay-service-rc9.8.unit");

const OVERRIDE: &str = include_str!("../../protocol/src/testdata/relay-dropin-override.conf");

/// list-unit-files answers in systemd 249's line format; daemon-reload fails while `reload-fails` exists.
const FAKE_SYSTEMCTL: &str = "#!/bin/sh\nd=${0%/*}\necho \"$*\" >> \"$d/calls\"\ncase \"$*\" in\n  *list-unit-files*) echo 'sot-host-relay-remote-a.socket enabled enabled' ;;\n  *daemon-reload*) if [ -e \"$d/reload-fails\" ]; then echo 'Failed to reload daemon: Connection timed out' >&2; exit 1; fi ;;\nesac\nexit 0\n";

fn units(t: &TempDir) -> PathBuf {
    t.path().join("config/systemd/user")
}

fn dropins(t: &TempDir) -> PathBuf {
    units(t).join("sot-host-relay-remote-a@.service.d")
}

fn hub() -> TempDir {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let ctl = t.path().join("bin/systemctl");
    for d in [t.path().join("bin"), dropins(&t)] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(&ctl, FAKE_SYSTEMCTL).unwrap();
    std::fs::set_permissions(&ctl, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(t.path().join("hosts.toml"), "hub = \"hub-box\"\n[host.hub-box]\ndaemon = true\n[host.remote-a]\ndaemon = true\n").unwrap();
    t
}

/// Exit code, stdout, stderr of `sotd topology refresh` with every path inside `t`.
fn refresh(t: &TempDir) -> (Option<i32>, String, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_sotd"));
    // A cleared environment and a PATH of the scratch bin alone: no route reaches the real systemctl or the
    // user's service manager, even where the scratch stub cannot run (a noexec temp mount fails the test instead).
    let out = cmd
        .env_clear()
        .args(["topology", "refresh"])
        .env("HOME", t.path())
        .env("XDG_CONFIG_HOME", t.path().join("config"))
        .env("SOT_HOSTS", t.path().join("hosts.toml"))
        .env("SOT_SELF_HOST", "hub-box")
        .env("PATH", t.path().join("bin"))
        .output()
        .unwrap();
    (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Round 4's two drop-ins, as systemd 249 reads them (systemd-analyze verify): an override behind lines ending in two
/// backslashes, which systemd applies; an alias override whose `ExecStart\ ` lines systemd ignores, keeping the Environment.
const EVEN_BACKSLASH: &str = "[Service]\nEnvironment=X=C:\\\\\nExecStart=\nEnvironment=Y=C:\\\\\nExecStart=/nonexistent/old-ssh -T remote-a sotd stdio-bridge --label local\n";

const ALIAS: &str = "[Service]\nExecStart\\ \n=/bin/true\nEnvironment=SOT_RELAY_TARGET=my-alias\n";

const COMMAND: &str = "zz-sot-relay-command.conf";

/// The hub holding those two and the real Windows Environment drop-in, after one refresh.
fn refreshed() -> TempDir {
    let t = hub();
    for (name, text) in [("no-mux.conf", EVEN_BACKSLASH), ("alias.conf", ALIAS), ("override.conf", OVERRIDE)] {
        std::fs::write(dropins(&t).join(name), text).unwrap();
    }
    let (code, so, se) = refresh(&t);
    assert_eq!(code, Some(0), "stdout={so}\nstderr={se}");
    t
}

/// Real systemd's reading of the scratch units (`systemd-analyze --user verify`: a private manager, cleared env, scratch runtime dir).
fn verify(t: &TempDir) -> String {
    use std::os::unix::fs::PermissionsExt;
    let run = t.path().join("run");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
    let out = std::process::Command::new("systemd-analyze")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", t.path())
        .env("XDG_RUNTIME_DIR", &run)
        .env("SYSTEMD_UNIT_PATH", format!("{}:", units(t).display())) // the trailing ':' keeps systemd's own dirs (basic.target)
        .args(["--user", "verify"])
        .arg(units(t).join("sot-host-relay-remote-a@probe.service"))
        .output()
        .expect("systemd-analyze on /usr/bin:/bin");
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

#[test]
fn refresh_writes_the_command_dropin_and_leaves_every_other_dropin_alone() {
    let t = refreshed();
    let text = std::fs::read_to_string(dropins(&t).join(COMMAND)).expect("refresh wrote no command drop-in");
    let set: Vec<&str> = text.lines().filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    assert!(set.len() == 3 && set[..2] == ["[Service]", "ExecStart="] && set[2].starts_with("ExecStart=/usr/bin/ssh "), "{set:?}");
    for (name, text) in [("no-mux.conf", EVEN_BACKSLASH), ("alias.conf", ALIAS), ("override.conf", OVERRIDE)] {
        assert_eq!(std::fs::read_to_string(dropins(&t).join(name)).ok().as_deref(), Some(text), "refresh changed or moved {name}");
    }
    assert_eq!(std::fs::read_dir(dropins(&t)).unwrap().count(), 4, "refresh left another file beside the drop-ins");
}

#[test]
fn systemd_runs_the_generated_command_whatever_an_older_dropin_says() {
    let t = refreshed();
    // The command's own failure lines only; another systemd may warn about something else.
    let wrong = |o: &str| o.lines().any(|l| l.starts_with("sot-host-relay-remote-a@probe.service:") && (l.contains("ExecStart") || l.contains("Command ")));
    let with = verify(&t);
    assert!(!wrong(&with), "systemd does not run the generated command:\n{with}");
    std::fs::remove_file(dropins(&t).join(COMMAND)).unwrap();
    let without = verify(&t);
    assert!(without.contains("Command /nonexistent/old-ssh is not executable"), "control: without the command drop-in systemd must run the override, or the check above proved nothing\n{without}");
}

#[test]
fn refresh_prints_each_rewrite_even_when_the_reload_then_fails() {
    let t = hub();
    std::fs::write(units(&t).join("sot-host-relay-remote-a.socket"), "stale\n").unwrap();
    std::fs::write(units(&t).join("sot-host-relay-remote-a@.service"), STALE_SERVICE).unwrap();
    std::fs::write(t.path().join("bin/reload-fails"), "").unwrap();
    let (code, so, se) = refresh(&t);
    assert!(code == Some(2) && se.contains("daemon-reload failed"), "code={code:?}\nstderr={se}");
    for f in ["sot-host-relay-remote-a.socket", "sot-host-relay-remote-a@.service", "sot-host-relay-remote-a@.service.d/zz-sot-relay-command.conf"] {
        let want = format!("rewrote {}", units(&t).join(f).display());
        assert!(so.lines().any(|l| l == want), "the rewrite of {f} went unreported because the reload after it failed\nstdout={so}");
    }
}
