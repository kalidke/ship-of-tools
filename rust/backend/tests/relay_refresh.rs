#![cfg(target_os = "linux")]
//! `sotd topology refresh` on a scratch hub: the real binary, a stand-in `systemctl` first on PATH. Wiring-level.
use {std::path::PathBuf, tempfile::TempDir};

const STALE_SERVICE: &str = include_str!("../../protocol/src/testdata/relay-service-rc9.8.unit");

const NO_MUX: &str = include_str!("../../protocol/src/testdata/relay-dropin-no-mux.conf");

const OVERRIDE: &str = include_str!("../../protocol/src/testdata/relay-dropin-override.conf");

/// list-unit-files answers in systemd 249's line format; daemon-reload fails while `reload-fails` exists.
const FAKE_SYSTEMCTL: &str = "#!/bin/sh\nd=$(dirname \"$0\")\necho \"$*\" >> \"$d/calls\"\ncase \"$*\" in\n  *list-unit-files*) echo 'sot-host-relay-remote-a.socket enabled enabled' ;;\n  *daemon-reload*) if [ -e \"$d/reload-fails\" ]; then echo 'Failed to reload daemon: Connection timed out' >&2; exit 1; fi ;;\nesac\nexit 0\n";

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
    for (k, _) in std::env::vars_os().filter(|(k, _)| k.to_string_lossy().starts_with("SOT_")) {
        cmd.env_remove(k);
    }
    let path = format!("{}:{}", t.path().join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let out = cmd
        .args(["topology", "refresh"])
        .env("HOME", t.path())
        .env("XDG_CONFIG_HOME", t.path().join("config"))
        .env("SOT_HOSTS", t.path().join("hosts.toml"))
        .env("SOT_SELF_HOST", "hub-box")
        .env("PATH", path)
        .output()
        .unwrap();
    (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn refresh_retires_an_exec_start_dropin_spelled_with_spaces() {
    let (t, spaced) = (hub(), NO_MUX.replace("ExecStart=", "ExecStart = ")); // the real drop-in, respelled; systemd applies it the same
    std::fs::write(dropins(&t).join("no-mux.conf"), &spaced).unwrap();
    std::fs::write(dropins(&t).join("override.conf"), OVERRIDE).unwrap();
    let (code, so, se) = refresh(&t);
    assert_eq!(code, Some(0), "stdout={so}\nstderr={se}");
    assert!(!dropins(&t).join("no-mux.conf").exists(), "a spaced ExecStart drop-in still overrides the generated command\n{so}");
    assert_eq!(std::fs::read_to_string(dropins(&t).join("no-mux.conf.retired")).unwrap(), spaced);
    assert_eq!(std::fs::read_to_string(dropins(&t).join("override.conf")).unwrap(), OVERRIDE);
    assert!(so.lines().any(|l| l.starts_with("retired ") && l.contains("no-mux.conf")), "{so}");
}

#[test]
fn refresh_prints_each_rewrite_even_when_the_reload_then_fails() {
    let t = hub();
    std::fs::write(units(&t).join("sot-host-relay-remote-a.socket"), "stale\n").unwrap();
    std::fs::write(units(&t).join("sot-host-relay-remote-a@.service"), STALE_SERVICE).unwrap();
    std::fs::write(t.path().join("bin/reload-fails"), "").unwrap();
    let (code, so, se) = refresh(&t);
    assert!(code == Some(2) && se.contains("daemon-reload failed"), "code={code:?}\nstderr={se}");
    for f in ["sot-host-relay-remote-a.socket", "sot-host-relay-remote-a@.service"] {
        let want = format!("rewrote {}", units(&t).join(f).display());
        assert!(so.lines().any(|l| l == want), "the rewrite of {f} went unreported because the reload after it failed\nstdout={so}");
    }
}
