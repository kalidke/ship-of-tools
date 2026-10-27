use super::*;
use crate::install::options::RoleFlag;

/// The role is declared: the role rule and the unit's host pin agree on who runs sotd (for every host of one hosts.toml
/// the install puts a daemon exactly where `sotd_hosts` lets the unit start; a frontend runs its own); with no flag and
/// no entry the role is what install.json recorded; a plan that dials another daemon host gets the dialing launcher.
/// A refused install writes nothing: owners are read from the files' own text, in every shape an install wrote; the
/// gates refuse another install's files, consent waives only those and never a file whose owner cannot be named; a
/// unit file another install wrote is never replaced unasked, whether or not it runs here, since on a shared home it is
/// what the other hosts start; a host the declared topology runs no sotd on never writes over the shared install, and
/// under a declared topology a run never records less than install.json does.
#[test]
fn the_role_is_declared() {
    {
        let hosts = "hub = \"h\"\n[host.h]\ndaemon = true\n[host.d]\ndaemon = true\n[host.f]\nfrontend = true\n[host.df]\ndaemon = true\nfrontend = true\n[host.s]\n";
        let topo = sot_topology::parse(hosts).unwrap();
        let table = sot_topology::status_table(&topo);
        let pinned = sot_topology::sotd_hosts(&topo);
        for h in ["h", "d", "f", "df", "s"] {
            let role = from_status(&table, h).unwrap();
            assert_eq!(role.daemon, pinned.contains(&h), "{h}: {role:?}, the pin names {pinned:?}");
            assert_eq!(role.daemon, sot_topology::runs_backend(Some(&topo), h, None), "{h}");
            assert_eq!(role.frontend, h == "f" || h == "df");
        }
        assert_eq!(from_status(&table, "absent"), None);
    }

    {
        let rec = |d: bool, f: bool| format!("{{\n  \"daemon\": {d},\n  \"frontend\": {f}\n}}");
        assert_eq!(recorded(&rec(true, true), ""), Some(RoleFlag::Local));
        assert_eq!(recorded(&rec(true, false), ""), Some(RoleFlag::BeOnly));
        assert_eq!(recorded(&rec(false, true), "export SOT_HOST=\"box\"\n"), Some(RoleFlag::Remote("box".into())));
        assert_eq!(recorded(&rec(false, true), ""), None);
        assert_eq!(recorded("{\"role\": \"be-only\"}", ""), Some(RoleFlag::BeOnly));
        assert_eq!(recorded(&rec(false, false), ""), None);
    }

    {
        let unit = "[Service]\nExecStartPre=-/bin/bash -c 'x; exec \"/p/bin/sot-apply\"'\nExecStart=/bin/bash -c '[ -r \"$HOME/.bashrc\" ] && . \"$HOME/.bashrc\"; exec \"/p/bin/sotd\" --project-root \"/h\" --label sot'\n";
        assert_eq!(unit_owner_path(unit).as_deref(), Some("/p/bin/sotd"));
        assert_eq!(unit_owner_path("ExecStart=/old/bin/sotd --label sot\n").as_deref(), Some("/old/bin/sotd"));
        assert_eq!(wrapper_owner_prefix("#!/bin/bash\nPENDING=\"/p/updates/pending-linux-x86_64.json\"\n").as_deref(), Some("/p"));
        assert_eq!(wrapper_owner_prefix("export SOT_FRONTEND_BIN=\"/p/bin/sot\"\n").as_deref(), Some("/p"));
        assert_eq!(wrapper_owner_prefix("exec \"/p/bin/sot\" \"$@\"\n").as_deref(), Some("/p"));
        assert_eq!(wrapper_owner_prefix("#!/bin/sh\necho hello\n"), None);
        assert_eq!(exec_target("[Desktop Entry]\nExec=/h/.local/bin/sot-launch\n").as_deref(), Some("/h/.local/bin/sot-launch"));
        assert_eq!(exec_target("#!/usr/bin/env bash\nexec \"/h/.local/bin/sot-launch\"\n").as_deref(), Some("/h/.local/bin/sot-launch"));
    }

    {
        assert_eq!(running_daemon(None, "/p", false), Gate::Allow);
        assert_eq!(running_daemon(Some("/p/bin/sotd"), "/p", false), Gate::Allow);
        assert!(matches!(running_daemon(Some("/q/bin/sotd"), "/p", false), Gate::Refuse(_)));
        assert_eq!(running_daemon(Some("/q/bin/sotd"), "/p", true), Gate::Allow);
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("sot-launch"), "PENDING=\"/q/updates/pending-x.json\"\n").unwrap();
        assert!(matches!(ownership(None, home.path(), "/p", false, true, false, false), Gate::Refuse(_)));
        assert_eq!(ownership(None, home.path(), "/p", false, true, false, true), Gate::Allow);
        assert_eq!(ownership(None, home.path(), "/p", false, false, false, false), Gate::Allow);
        std::fs::write(bin.join("sot-launch"), "something else\n").unwrap();
        assert!(matches!(ownership(None, home.path(), "/p", false, true, false, true), Gate::Unresolvable(_)));
    }

    {
        let prefix = tempfile::tempdir().unwrap();
        assert_eq!(host(true, false, prefix.path(), false), Gate::Allow, "a first install");
        std::fs::write(prefix.path().join("install.json"), "{\n  \"daemon\": true,\n  \"service\": \"systemd\"\n}\n").unwrap();
        assert!(matches!(host(true, false, prefix.path(), false), Gate::Refuse(_)));
        assert_eq!(host(true, true, prefix.path(), false), Gate::Allow);
        assert_eq!(host(false, false, prefix.path(), false), Gate::Allow);
        assert_eq!(host(true, false, prefix.path(), true), Gate::Allow);
        let rec = prefix.path().join("install.json");
        assert!(matches!(record_gate(&rec, true, false, "none", false), Gate::Refuse(_)));
        assert!(matches!(record_gate(&rec, true, true, "none", false), Gate::Refuse(_)));
        assert_eq!(record_gate(&rec, true, true, "systemd", false), Gate::Allow);
        assert_eq!(record_gate(&rec, false, false, "none", false), Gate::Allow);
        assert_eq!(record_gate(&rec, true, false, "none", true), Gate::Allow);
        assert_eq!((service_record(true, true, false), service_record(true, true, true), service_record(false, true, false)), ("systemd", "none", "none"));
    }

    {
        assert!(plan_dials_others("self me\ndial me unix:/s\ndial other ssh:other\n"));
        assert!(!plan_dials_others("dial me unix:/s\nself me\n"));
        assert!(!plan_dials_others(""));
    }

    {
        let home = tempfile::tempdir().unwrap();
        let units = home.path().join(".config/systemd/user");
        std::fs::create_dir_all(&units).unwrap();
        std::fs::write(units.join("sotd.service"), "[Service]\nExecStart=/q/bin/sotd --label sot\n").unwrap();
        assert!(matches!(ownership(None, home.path(), "/p", false, false, true, false), Gate::Refuse(_)));
        assert_eq!(ownership(None, home.path(), "/p", false, false, true, true), Gate::Allow, "--force-role-change consents");
        assert_eq!(ownership(None, home.path(), "/p", false, false, false, false), Gate::Allow, "a run that wires no unit leaves it");
        std::fs::write(units.join("sotd.service"), "[Service]\nExecStart=/p/bin/sotd --label sot\n").unwrap();
        assert_eq!(ownership(None, home.path(), "/p", false, false, true, false), Gate::Allow, "this install's own unit");
        std::fs::write(units.join("sotd.service"), "[Unit]\nDescription=x\n").unwrap();
        assert!(matches!(ownership(None, home.path(), "/p", false, false, true, true), Gate::Unresolvable(_)), "a unit with no program is never taken");
    }
}

/// The table and plan readers read what topology's own producers print: every line of a status table with hosts, a
/// monitor-only row and a warning, and a plan whose endpoints hold a space (a private runtime folder may), read; a
/// table line or a plan line outside the grammar does not. The plan runs in a child process of its own, since its
/// endpoints come from the process's runtime folder.
#[cfg(unix)]
#[test]
fn the_topology_readers_read_what_topology_prints() {
    let hosts = "hub = \"h\"\n[host.h]\ndaemon = true\n[host.d]\ndaemon = true\n[host.f]\nfrontend = true\n[host.s]\n[monitor]\ngpu-box = \"other-user@gpu-box\"\nd = \"\"\n";
    let topo = sot_topology::parse(hosts).unwrap();
    let table = sot_topology::status_table(&topo);
    assert!(table.contains(" monitor-only "), "the table has a monitor-only row: {table}");
    assert_eq!(status_unread(&table), None, "{table}");
    assert_eq!(status_unread(&format!("{table}WARNING something the parser noticed\ncache diverged: this box's hosts.toml (a) != the hub's (b); run `sotd topology sync`\n")), None);
    for bad in ["me nonsense", "me", "Me daemon", "me daemon extra", ""] {
        assert_eq!(status_unread(&format!("HOST DECLARED\n{bad}\n")), Some(bad), "'{bad}' is not a table line");
    }
    assert!(plan_reads("self me\nhub h\nrelay-endpoint ssh:h\nnew-fact a later release adds\n", "me"), "a line whose first word is unknown is ignored");
    for bad in ["hub h\n", "self other\n", "self me\ndial d\n", "self me\ndial\n", "self me\nhub\n"] {
        assert!(!plan_reads(bad, "me"), "{bad:?}");
    }

    if !sot_platform::test_isolated::run_isolated_until("install::role::tests::the_topology_readers_read_what_topology_prints", std::time::Instant::now() + std::time::Duration::from_secs(60)).unwrap() {
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let run = scratch.path().join("run dir");
    std::fs::create_dir(&run).unwrap();
    std::fs::set_permissions(&run, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    std::env::set_var("XDG_RUNTIME_DIR", &run);
    for name in ["SOT_RUNTIME_DIR", "SOT_SOCKET"] {
        std::env::remove_var(name);
    }
    let plan = sot_topology::plan(&topo, "d").unwrap();
    assert!(plan.lines().any(|l| l.starts_with("relay-endpoint ") && l.contains("run dir")), "the plan's endpoints hold the space: {plan}");
    assert!(plan_reads(&plan, "d"), "{plan}");
    assert!(plan_dials_others(&plan), "{plan}");
}
