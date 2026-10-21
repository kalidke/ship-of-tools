use super::*;

const STALE_SERVICE: &str = include_str!("../../tests/fixtures/relay/relay-service-rc9.8.unit");
const NO_MUX: &str = include_str!("../../tests/fixtures/relay/relay-dropin-no-mux.conf");
const OVERRIDE: &str = include_str!("../../tests/fixtures/relay/relay-dropin-override.conf");

const SHOW: &str = "show -p ExecStart -p LoadState sot-host-relay-remote-a@refresh-check.service";

/// The stand-in answer to `show`: the generated command, as systemd prints it.
fn shown(a: &[&str]) -> String {
    if a[1] == "show" { format!("ExecStart={{ argv[]={} ; }}\nLoadState=loaded\n", topology::relay_units::relay_command_line()) } else { String::new() }
}

fn hub_topo() -> Topology {
    topology::parse("hub = \"hub-box\"\n[host.hub-box]\ndaemon = true\n[host.remote-a]\ndaemon = true\n").unwrap()
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sot-f3-refresh-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("sot-host-relay-remote-a@.service.d")).unwrap();
    d
}

#[test]
fn runs_generated_command_needs_one_record_that_is_ours_and_a_loaded_unit() {
    let ours = format!("ExecStart={{ path=/usr/bin/ssh ; argv[]={} ; ignore_errors=no ; pid=0 }}", topology::relay_units::relay_command_line());
    let old = "ExecStart={ path=/usr/bin/ssh ; argv[]=/usr/bin/ssh -T x sotd stdio-bridge --label local ; ignore_errors=no ; pid=0 }";
    let longer = format!("ExecStart={{ path=/usr/bin/ssh ; argv[]={} --label local ; ignore_errors=no ; pid=0 }}", topology::relay_units::relay_command_line());
    assert!(runs_generated_command(&format!("{ours}\nLoadState=loaded\n")));
    for bad in [
        format!("{old}\nLoadState=loaded\n"),
        format!("{ours}\n{old}\nLoadState=bad-setting\n"),
        format!("{old}\n{ours}\nLoadState=bad-setting\n"),
        format!("{ours}\nLoadState=bad-setting\n"),
        format!("{ours}\nLoadState=masked\n"),
        format!("{longer}\nLoadState=loaded\n"),
        format!("{ours}\n"),
        "LoadState=loaded\n".to_string(),
    ] {
        assert!(!runs_generated_command(&bad), "{bad}");
    }
}

#[test]
fn refresh_writes_and_reloads_in_order() {
    let topo = hub_topo();
    let dir = scratch("order");
    let dd = dir.join("sot-host-relay-remote-a@.service.d");
    std::fs::write(dir.join("sot-host-relay-remote-a.socket"), topology::relay_units::relay_socket_unit("remote-a")).unwrap();
    std::fs::write(dir.join("sot-host-relay-remote-a@.service"), STALE_SERVICE).unwrap();
    std::fs::write(dd.join("no-mux.conf"), NO_MUX).unwrap();
    std::fs::write(dd.join("override.conf"), OVERRIDE).unwrap();
    let enabled = vec!["remote-a".to_string()];

    let mut calls: Vec<String> = Vec::new();
    let mut said: Vec<String> = Vec::new();
    refresh(&topo, "hub-box", &dir, &enabled, &mut |a| {
        calls.push(a[1..].join(" "));
        Ok(shown(a))
    }, &mut |l| said.push(l))
    .unwrap();
    // Each path as refresh joins it: dir plus relay_files' name, whose '/' a Windows join keeps.
    assert_eq!(
        said,
        [
            format!("wrote {}", dir.join("sotd.service.d/topology.conf").display()),
            format!("wrote {}", dir.join("sot-relay-tunnel@.service.d/topology.conf").display()),
            format!("wrote {}", dir.join("sot-host-relay-remote-a.socket.d/topology.conf").display()),
            format!("rewrote {}", dir.join("sot-host-relay-remote-a@.service").display()),
            format!("rewrote {}", dir.join("sot-host-relay-remote-a@.service.d/zz-sot-relay-command.conf").display())
        ]
    );
    assert_eq!(std::fs::read_to_string(dir.join("sot-host-relay-remote-a@.service")).unwrap(), topology::relay_units::relay_service_unit("remote-a"));
    assert_eq!(std::fs::read_to_string(dd.join("no-mux.conf")).unwrap(), NO_MUX);
    assert_eq!(std::fs::read_dir(&dd).unwrap().count(), 3);
    assert_eq!(std::fs::read_to_string(dd.join("override.conf")).unwrap(), OVERRIDE);
    assert_eq!(
        calls,
        ["reset-failed sot-host-relay-remote-a.socket sot-host-relay-remote-a@*.service", "daemon-reload", SHOW, "restart sot-host-relay-remote-a.socket"]
    );

    calls.clear();
    said.clear();
    refresh(&topo, "hub-box", &dir, &enabled, &mut |a| {
        calls.push(a[1..].join(" "));
        Ok(shown(a))
    }, &mut |l| said.push(l))
    .unwrap();
    assert!(said.is_empty(), "{said:?}");
    assert_eq!(
        calls,
        ["reset-failed sot-host-relay-remote-a.socket sot-host-relay-remote-a@*.service", "daemon-reload", SHOW, "restart sot-host-relay-remote-a.socket"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn refresh_after_a_failed_reload_reloads_and_restarts_again() {
    let topo = hub_topo();
    let dir = scratch("retry");
    std::fs::write(dir.join("sot-host-relay-remote-a.socket"), "stale\n").unwrap();
    std::fs::write(dir.join("sot-host-relay-remote-a@.service"), STALE_SERVICE).unwrap();
    let enabled = vec!["remote-a".to_string()];
    let e = refresh(&topo, "hub-box", &dir, &enabled, &mut |a| {
        if a[1] == "daemon-reload" { Err("Failed to reload daemon: Connection timed out".to_string()) } else { Ok(shown(a)) }
    }, &mut |_| {})
    .unwrap_err();
    assert!(e.contains("daemon-reload"), "{e}");
    let mut calls: Vec<String> = Vec::new();
    refresh(&topo, "hub-box", &dir, &enabled, &mut |a| {
        calls.push(a[1..].join(" "));
        Ok(shown(a))
    }, &mut |_| {})
    .unwrap();
    assert_eq!(
        calls,
        ["reset-failed sot-host-relay-remote-a.socket sot-host-relay-remote-a@*.service", "daemon-reload", SHOW, "restart sot-host-relay-remote-a.socket"],
        "the files already match after the failed run; the reload and the restart must still be redone"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(target_os = "linux")]
#[test]
fn supervised_means_the_units_main_pid_not_an_inherited_variable() {
    assert!(is_main_pid("4242\n", 4242));
    assert!(!is_main_pid("0\n", 4242), "a stopped or missing unit");
    assert!(!is_main_pid("4243\n", 4242), "a session child or a hand-started daemon");
    assert!(!is_main_pid("", 4242));
}

/// The hub pins sotd.service to every host that runs sotd and its reverse tunnels to itself, on their template so
/// systemd reads the pin for every instance. A second run changes nothing and, with no relay socket enabled,
/// reloads nothing.
#[test]
fn refresh_pins_the_daemon_and_the_tunnels() {
    let topo = topology::parse("hub = \"hub-box\"\n[host.hub-box]\ndaemon = true\n[host.laptop]\nfrontend = true\n[host.server]\n").unwrap();
    let dir = scratch("pins");
    let mut calls: Vec<String> = Vec::new();
    refresh(&topo, "hub-box", &dir, &[], &mut |a| {
        calls.push(a[1..].join(" "));
        Ok(String::new())
    }, &mut |_| {})
    .unwrap();
    let read = |p: &str| std::fs::read_to_string(dir.join(p)).ok();
    assert_eq!(read("sotd.service.d/topology.conf"), Some(topology::relay_units::host_pin(&["hub-box", "laptop"])));
    assert_eq!(read("sot-relay-tunnel@.service.d/topology.conf"), Some(topology::relay_units::host_pin(&["hub-box"])));
    assert_eq!(calls, ["daemon-reload"]);
    calls.clear();
    refresh(&topo, "hub-box", &dir, &[], &mut |a| {
        calls.push(a[1..].join(" "));
        Ok(String::new())
    }, &mut |_| {})
    .unwrap();
    assert!(calls.is_empty(), "{calls:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn refresh_off_the_hub_touches_nothing() {
    let dir = scratch("offhub");
    let mut called = false;
    let e = refresh(&hub_topo(), "remote-a", &dir, &["remote-a".to_string()], &mut |_| {
        called = true;
        Ok(String::new())
    }, &mut |_| {})
    .unwrap_err();
    assert!(e.contains("not the hub"), "{e}");
    assert!(!called && !dir.join("sot-host-relay-remote-a@.service").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The program `systemctl()` starts in place of the real one while a test holds it; `None` outside such a test.
pub(super) static STUB_SYSTEMCTL: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Clears the stub program on every path out of the test.
#[cfg(target_os = "linux")]
struct ClearStub;

#[cfg(target_os = "linux")]
impl Drop for ClearStub {
    fn drop(&mut self) {
        *STUB_SYSTEMCTL.lock().unwrap() = None;
    }
}

/// The refresh's one-shots run in their own containment: what a
/// `systemctl` leaves running dies with it, though it exits at once.
#[cfg(target_os = "linux")]
#[test]
fn the_relay_probe_takes_its_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bg = dir.path().join("bg");
    let descendant = crate::lifecycle::child_signal::tests::Leftover::of_file(bg.clone());
    let _clear = ClearStub;
    let stub = dir.path().join("systemctl");
    sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\nsleep 3110 >/dev/null 2>&1 &\necho $! > '{}'\necho 0\n", bg.display()));
    *STUB_SYSTEMCTL.lock().unwrap() = Some(stub);
    assert_eq!(supervised_by_systemd(), Ok(false));
    assert!(descendant.gone(), "the relay probe's descendant survived");
}
