use super::*;

#[test]
fn ps_lines_give_pid_parent_name_and_program() {
    let all = parse_ps("  10     1 sotd     /p/bin/sotd --project-root /h --label sot\n  11    10 julia    /j/julia -e x\nnot a line\n");
    assert_eq!(all.len(), 2);
    assert_eq!((all[0].pid, all[0].ppid, all[0].name.as_str(), all[0].exe.as_str()), (10, 1, "sotd", "/p/bin/sotd"));
    assert_eq!(all[1].args, "/j/julia -e x");
}

#[test]
fn cim_json_is_an_array_or_one_object() {
    let one = parse_cim(r#"{"ProcessId":5,"ParentProcessId":4,"Name":"sotd.exe","ExecutablePath":"C:\\p\\bin\\sotd.exe","CommandLine":"x"}"#);
    assert_eq!((one.len(), one[0].pid, one[0].exe.as_str()), (1, 5, "C:\\p\\bin\\sotd.exe"));
    assert_eq!(parse_cim(r#"[{"ProcessId":1},{"ProcessId":2,"ExecutablePath":null}]"#).len(), 2);
    assert!(parse_cim("not json").is_empty());
}

/// The install's daemon is the process the kernel names sotd whose program is the install's binary, never one that only
/// mentions it.
#[cfg(unix)]
#[test]
fn the_daemon_is_known_by_its_program_path() {
    let all = parse_ps("10 1 sotd /p/bin/sotd --label sot\n11 1 sotd /q/bin/sotd\n12 1 bash bash -c /p/bin/sotd\n13 1 sot /p/bin/sot --socket s\n");
    assert_eq!(daemon_pids(&all, Path::new("/p")), [10]);
    assert!(window_runs(&all, Path::new("/p")) && !window_runs(&all, Path::new("/q")));
}
