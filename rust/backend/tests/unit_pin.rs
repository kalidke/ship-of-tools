#![cfg(target_os = "linux")]
//! Host pins as systemd reads them: the real `sotd topology pin` and `sotd topology refresh` write into a scratch
//! systemd folder, real systemd loads that folder in test mode (`systemd --test --user`: no manager runs, nothing is
//! started) and real `systemd-analyze --user condition` evaluates on this box the conditions systemd attached to the
//! unit. A home several hosts share shows every unit in it to every host's manager; these prove each unit starts only
//! where hosts.toml says.
use {std::path::Path, std::process::Command, tempfile::TempDir};

#[path = "support/sotd.rs"]
mod sotd;

const UNIT: &str = include_str!("../../../deploy/sotd.service");

/// This box as `host_name()` names it from the kernel hostname, which is all systemd sees.
fn me() -> String {
    let raw = std::fs::read_to_string("/proc/sys/kernel/hostname").expect("kernel hostname");
    raw.split('.').next().unwrap().trim().to_lowercase()
}

/// A scratch home with a systemd folder holding the rendered daemon unit and the reverse-tunnel template.
fn home() -> TempDir {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(units(&t)).unwrap();
    let unit = UNIT.replace("@SOT_BIN@", "/nonexistent/sotd").replace("@SOT_APPLY@", "/nonexistent/sot-apply").replace("@SOT_PROJECT_ROOT@", "/nonexistent");
    std::fs::write(units(&t).join("sotd.service"), unit).unwrap();
    std::fs::write(units(&t).join("sot-relay-tunnel@.service"), "[Unit]\nDescription=tunnel to %i\n[Service]\nExecStart=/bin/true\n").unwrap();
    t
}

fn units(t: &TempDir) -> std::path::PathBuf {
    t.path().join("config/systemd/user")
}

/// `sotd topology <args>` against `hosts` (written to the scratch home; `None` means no hosts.toml), with every path
/// inside the scratch home and a PATH holding only the scratch bin, so no route reaches the real `systemctl`.
fn topology(t: &TempDir, hosts: Option<&str>, self_host: &str, args: &[&str]) -> (Option<i32>, String) {
    let file = t.path().join("hosts.toml");
    match hosts {
        Some(text) => std::fs::write(&file, text).unwrap(),
        None => {
            let _ = std::fs::remove_file(&file);
        }
    }
    let bin = t.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    sot_log::test_exec::write_executable(&bin.join("systemctl"), "#!/bin/sh\necho \"$*\" >> \"${0%/*}/calls\"\nexit 0\n");
    let out = sotd::sotd_command()
        .env_clear()
        .arg("topology")
        .args(args)
        .env("HOME", t.path())
        .env("XDG_CONFIG_HOME", t.path().join("config"))
        .env("SOT_HOSTS", &file)
        .env("SOT_SELF_HOST", self_host)
        .env("PATH", &bin)
        .output()
        .unwrap();
    (out.status.code(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

fn pin(t: &TempDir, hosts: Option<&str>) -> (Option<i32>, String) {
    let dir = units(t).display().to_string();
    topology(t, hosts, &me(), &["pin", "--dir", &dir])
}

fn systemd_env(cmd: &mut Command, t: &TempDir) {
    use std::os::unix::fs::PermissionsExt;
    let run = t.path().join("run");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
    cmd.env_clear().env("PATH", "/usr/bin:/bin").env("HOME", t.path()).env("XDG_RUNTIME_DIR", &run);
}

/// The `ConditionHost=` assignments systemd attached to `unit` when it loaded the scratch folder, and the drop-ins it
/// read for it: `systemd --test --user` builds the start transaction for `unit` from that folder alone (plus systemd's
/// own units) and dumps it; nothing is started.
fn loaded(t: &TempDir, unit: &str) -> (Vec<String>, Vec<String>) {
    let systemd = ["/usr/lib/systemd/systemd", "/lib/systemd/systemd"].into_iter().find(|p| Path::new(p).exists()).expect("the systemd binary");
    let mut cmd = Command::new(systemd);
    systemd_env(&mut cmd, t);
    let out = cmd
        .env("SYSTEMD_UNIT_PATH", format!("{}:", units(t).display())) // the trailing ':' keeps systemd's own folders
        .args(["--test", "--user", "--no-pager", "--log-target=console", &format!("--unit={unit}")])
        .output()
        .unwrap();
    let dump = String::from_utf8_lossy(&out.stdout).into_owned();
    let head = format!("-> Unit {unit}:");
    let section: Vec<&str> = dump.lines().skip_while(|l| l.trim() != head).skip(1).take_while(|l| !l.trim_start().starts_with("-> Unit ")).collect();
    assert!(!section.is_empty(), "systemd dumped no section for {unit}:\n{}{}", dump, String::from_utf8_lossy(&out.stderr));
    let conditions = section.iter().filter_map(|l| l.trim().strip_prefix("ConditionHost: ")).map(|v| format!("ConditionHost={}", v.trim_end_matches(" untested"))).collect();
    // Only the scratch folder's drop-ins: a distribution may ship drop-ins for every user service.
    let ours = units(t).display().to_string();
    let dropins = section.iter().filter_map(|l| l.trim().strip_prefix("DropIn Path: ")).filter(|p| p.starts_with(&ours)).map(str::to_string).collect();
    (conditions, dropins)
}

/// Whether systemd, on this box, would start a unit carrying `conditions`.
fn starts_here(t: &TempDir, conditions: &[String]) -> bool {
    let mut cmd = Command::new("systemd-analyze");
    systemd_env(&mut cmd, t);
    let out = cmd.args(["--user", "condition"]).args(conditions).output().expect("systemd-analyze");
    match out.status.code() {
        Some(0) => true,
        Some(1) => false,
        other => panic!("systemd-analyze condition {conditions:?}: {other:?}\n{}", String::from_utf8_lossy(&out.stderr)),
    }
}

/// The done test: the daemon unit, which every host sharing the home sees, starts only on the hosts hosts.toml runs
/// sotd on. Each case is this box under another declaration.
#[test]
fn sotd_unit_is_skipped_on_a_host_the_topology_does_not_run_it_on() {
    let me = me();
    let cases = [
        (format!("hub = \"unithost-hub\"\n[host.unithost-hub]\ndaemon = true\n[host.{me}]\n"), false, "listed, neither daemon nor frontend"),
        ("hub = \"unithost-hub\"\n[host.unithost-hub]\ndaemon = true\n".to_string(), false, "not listed"),
        (format!("hub = \"{me}\"\n[host.{me}]\ndaemon = true\n[host.unithost-server]\n"), true, "the hub"),
        (format!("hub = \"unithost-hub\"\n[host.unithost-hub]\ndaemon = true\n[host.{me}]\nfrontend = true\n"), true, "a frontend, which runs its own daemon"),
    ];
    for (hosts, runs, what) in cases {
        let t = home();
        let (code, out) = pin(&t, Some(&hosts));
        assert_eq!(code, Some(0), "{what}: {out}");
        let (conditions, dropins) = loaded(&t, "sotd.service");
        assert_eq!(dropins, [units(&t).join("sotd.service.d/topology.conf").display().to_string()], "{what}");
        assert!(!conditions.is_empty(), "{what}: systemd attached no host condition to sotd.service");
        assert_eq!(starts_here(&t, &conditions), runs, "{what}: {conditions:?}");
    }
}

/// With no hosts.toml nothing is declared, so a lone box runs its own daemon: a pin left from before goes.
#[test]
fn with_no_hosts_toml_the_daemon_unit_is_unpinned() {
    let t = home();
    let (code, out) = pin(&t, Some("hub = \"unithost-hub\"\n[host.unithost-hub]\ndaemon = true\n"));
    assert_eq!(code, Some(0), "{out}");
    let (code, out) = pin(&t, None);
    assert_eq!(code, Some(0), "{out}");
    assert!(!units(&t).join("sotd.service.d").exists(), "{out}");
    let (conditions, dropins) = loaded(&t, "sotd.service");
    assert!(conditions.is_empty() && dropins.is_empty(), "{conditions:?} {dropins:?}");
}

/// An invalid hosts.toml declares nothing new: the pin stays as it was and the verb fails.
#[test]
fn an_invalid_hosts_toml_leaves_the_pin_alone() {
    let t = home();
    let (code, out) = pin(&t, Some("hub = \"unithost-hub\"\n[host.unithost-hub]\ndaemon = true\n"));
    assert_eq!(code, Some(0), "{out}");
    let before = std::fs::read(units(&t).join("sotd.service.d/topology.conf")).unwrap();
    let (code, out) = pin(&t, Some("hub = \"unithost-hub\"\nnot toml\n"));
    assert_eq!(code, Some(2), "{out}");
    assert_eq!(std::fs::read(units(&t).join("sotd.service.d/topology.conf")).unwrap(), before);
}

/// The installer tells a lone box (no hosts.toml) from an unreadable one by this error text
/// (`installer_topology_unreadable` in `scripts/install.sh` matches it), so the text is pinned here.
#[test]
fn status_without_a_hosts_toml_says_so_in_the_words_the_installer_matches() {
    let t = home();
    let (code, out) = topology(&t, None, &me(), &["status"]);
    assert_eq!(code, Some(2), "{out}");
    assert!(out.contains("no hosts.toml at"), "{out}");
}

/// A pin that cannot be written stops `apply` before any enable: a unit enabled without its pin would start on every
/// host that shares the home.
#[test]
fn apply_enables_nothing_when_a_pin_cannot_be_written() {
    let t = home();
    std::fs::write(units(&t).join("sot-relay-tunnel@.service.d"), "a file where the pin's folder goes").unwrap();
    let hosts = "hub = \"unithost-hub\"\n[host.unithost-hub]\ndaemon = true\n[host.unithost-server]\n";
    let (code, out) = topology(&t, Some(hosts), "unithost-hub", &["apply", "--yes"]);
    assert_eq!(code, Some(2), "{out}");
    assert!(out.contains("nothing was enabled"), "{out}");
    let calls = std::fs::read_to_string(t.path().join("bin/calls")).unwrap_or_default();
    assert!(!calls.contains("enable"), "{calls}");
}

/// A dry `apply` whose only change is a pin says it is a dry run, and writes nothing.
#[test]
fn a_dry_apply_that_would_only_write_pins_says_so_and_writes_nothing() {
    let t = home();
    let hosts = "hub = \"unithost-hub\"\n[host.unithost-hub]\ndaemon = true\n[host.unithost-fe]\nfrontend = true\n";
    let (code, out) = topology(&t, Some(hosts), "unithost-hub", &["apply"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("would write") && out.contains("(dry run"), "{out}");
    assert!(!units(&t).join("sotd.service.d/topology.conf").exists(), "{out}");
}

/// The hub's refresh pins every reverse-tunnel instance to the hub on the template, where systemd reads it for each
/// instance.
#[test]
fn the_hub_pins_every_reverse_tunnel_where_systemd_reads_it() {
    let t = home();
    let hosts = "hub = \"unithost-hub\"\n[host.unithost-hub]\ndaemon = true\n[host.unithost-server]\n";
    let (code, out) = topology(&t, Some(hosts), "unithost-hub", &["refresh"]);
    assert_eq!(code, Some(0), "{out}");
    let (conditions, dropins) = loaded(&t, "sot-relay-tunnel@unithost-server.service");
    assert_eq!(dropins, [units(&t).join("sot-relay-tunnel@.service.d/topology.conf").display().to_string()]);
    assert!(!conditions.is_empty() && !starts_here(&t, &conditions), "{conditions:?}");
    let (conditions, _) = loaded(&t, "sotd.service");
    assert!(!conditions.is_empty() && !starts_here(&t, &conditions), "{conditions:?}");
}

/// The installer's role rule (`installer_topology_role`, shell) and `sotd_hosts` are twins: for every host of one
/// hosts.toml, the installer installs a daemon exactly where the pin lets the unit start.
#[test]
fn the_installer_and_the_pin_agree_on_who_runs_sotd() {
    let t = home();
    let hosts = "hub = \"h\"\n[host.h]\ndaemon = true\n[host.d]\ndaemon = true\n[host.f]\nfrontend = true\n[host.df]\ndaemon = true\nfrontend = true\n[host.s]\n";
    let (code, table) = topology(&t, Some(hosts), "h", &["status"]);
    assert_eq!(code, Some(0), "{table}");
    let install = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install.sh");
    let topo = sot_protocol::topology::parse(hosts).unwrap();
    let pinned = sot_protocol::topology::sotd_hosts(&topo);
    for h in ["h", "d", "f", "df", "s", "absent"] {
        let out = Command::new("bash")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", t.path())
            .env("SOT_INSTALL_SOURCE_ONLY", "1")
            .args(["-c", ". \"$1\"; installer_topology_role \"$2\" \"$3\"", "bash"])
            .arg(&install)
            .arg(&table)
            .arg(h)
            .output()
            .unwrap();
        let role = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success() && !role.is_empty(), "{h}: {role:?} {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(role.contains("daemon:1"), pinned.contains(&h), "{h}: installer says {role:?}, pin names {pinned:?}");
    }
}
