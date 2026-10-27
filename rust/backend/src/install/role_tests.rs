use super::*;
use crate::install::options::RoleFlag;

/// The role is declared: the role rule and the unit's host pin agree on who runs sotd (for every host of one hosts.toml
/// the install puts a daemon exactly where `sotd_hosts` lets the unit start; a frontend runs its own); with no flag and
/// no entry the role is what install.json recorded (a plan that dials another daemon host gets the dialing launcher:
/// `install::unix::observe_tests`).
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
        let table = sot_topology::status_rows(&topo);
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
        // Every ExecStart a release shipped, rendered as its installer rendered it (`sed` of `@SOT_BIN@` to
        // `<prefix>/bin/sotd`, `@SOT_PORT@` and `@SOT_PROJECT_ROOT@`), read through `systemctl show`'s record.
        let current = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/sotd.service")).unwrap();
        let current = current.lines().find_map(|l| l.strip_prefix("ExecStart=")).unwrap().to_string();
        for (tag, template) in [
            ("v0.3.0", "@SOT_BIN@ --tcp 127.0.0.1:@SOT_PORT@ --project-root @SOT_PROJECT_ROOT@ --label sot"),
            ("v0.3.2", "@SOT_BIN@ --project-root @SOT_PROJECT_ROOT@ --label sot"),
            ("v0.6.0 on (deploy/sotd.service)", current.as_str()),
        ] {
            let rendered = template.replace("@SOT_BIN@", "/p/bin/sotd").replace("@SOT_PORT@", "7777").replace("@SOT_PROJECT_ROOT@", "/h");
            assert_eq!(unit_owner_shown(&shown(&rendered)).as_deref(), Ok("/p/bin/sotd"), "{tag}: {rendered}");
        }
        let record = |path: &str, argv: &str| format!("{{ path={path} ; argv[]={argv} ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }}\n");
        for (why, bad) in [
            ("printf naming sotd in its argument", record("/usr/bin/printf", "/usr/bin/printf exec \"/p/bin/sotd\"")),
            ("bash running another script", record("/bin/bash", "/bin/bash -c exec \"/p/bin/sotd\" --label sot")),
            ("bash form with a quote in the root", record("/bin/bash", "/bin/bash -c [ -r \"$HOME/.bashrc\" ] && . \"$HOME/.bashrc\"; exec \"/p/bin/sotd\" --project-root \"/h\"x\" --label sot")),
            ("a program not named sotd", record("/p/bin/other", "/p/bin/other --label sot")),
            ("argv[0] not the program", record("/p/bin/sotd", "/q/bin/sotd --label sot")),
            ("a relative program", record("sotd", "sotd --label sot")),
            ("two commands (a drop-in adds one)", format!("{}{}", record("/p/bin/sotd", "/p/bin/sotd"), record("/q/bin/sotd", "/q/bin/sotd"))),
            ("no command", String::new()),
            ("no record", "/p/bin/sotd --label sot\n".to_string()),
        ] {
            assert!(unit_owner_shown(&bad).is_err(), "{why}: {bad:?}");
        }
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
        assert!(matches!(ownership(None, None, home.path(), "/p", false, true, false, false), Gate::Refuse(_)));
        assert_eq!(ownership(None, None, home.path(), "/p", false, true, false, true), Gate::Allow);
        assert_eq!(ownership(None, None, home.path(), "/p", false, false, false, false), Gate::Allow);
        std::fs::write(bin.join("sot-launch"), "something else\n").unwrap();
        assert!(matches!(ownership(None, None, home.path(), "/p", false, true, false, true), Gate::Unresolvable(_)));
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
        // The unit's owner is systemd's reading of it (a unit whose owner cannot be read stops the run before the gate).
        let home = tempfile::tempdir().unwrap();
        let other = Some("/q/bin/sotd");
        assert!(matches!(ownership(None, other, home.path(), "/p", false, false, true, false), Gate::Refuse(_)));
        assert_eq!(ownership(None, other, home.path(), "/p", false, false, true, true), Gate::Allow, "--force-role-change consents");
        assert_eq!(ownership(None, other, home.path(), "/p", false, false, false, false), Gate::Allow, "a run that wires no unit leaves it");
        assert_eq!(ownership(None, Some("/p/bin/sotd"), home.path(), "/p", false, false, true, false), Gate::Allow, "this install's own unit");
    }
}


/// `systemctl show -p ExecStart --value`'s record of a rendered ExecStart line, as systemd prints it: the program, then
/// the arguments joined by spaces with the single quotes of a `-c '...'` script removed.
fn shown(exec_start: &str) -> String {
    let path = exec_start.split(' ').next().unwrap();
    let argv = match exec_start.split_once(" -c '") {
        Some((head, script)) => format!("{head} -c {}", script.strip_suffix('\'').unwrap()),
        None => exec_start.to_string(),
    };
    format!("{{ path={path} ; argv[]={argv} ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }}\n")
}
