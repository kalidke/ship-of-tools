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
        let topo = sot_protocol::topology::parse(hosts).unwrap();
        let table = sot_protocol::topology::status_table(&topo);
        let pinned = sot_protocol::topology::sotd_hosts(&topo);
        for h in ["h", "d", "f", "df", "s"] {
            let role = from_status(&table, h).unwrap();
            assert_eq!(role.daemon, pinned.contains(&h), "{h}: {role:?}, the pin names {pinned:?}");
            assert_eq!(role.daemon, sot_protocol::topology::runs_backend(Some(&topo), h, None), "{h}");
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
