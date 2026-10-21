use super::*;

/// The chain a walk to the top gives for `recs`, caller first, each record split at `sep`.
fn chain(recs: &[&str], sep: char) -> Vec<Rec> {
    recs.iter()
        .map(|r| Rec::Proc(r.split(sep).map(str::to_string).collect()))
        .chain(std::iter::once(Rec::End))
        .collect()
}

fn count(recs: &[&str], sep: char) -> usize {
    layers(&chain(recs, sep), "").0.len()
}

/// The 0.6.6 table (`comm/tests/agent_layers/table.sh`), case for case.
#[test]
fn every_agent_layer_is_counted_once() {
    let tbl: &[(usize, &[&str])] = &[
        (1, &["bash", "/h/.local/bin/claude --x", "sot-capsule run"]),
        (2, &["bash", "bash -lc p", "/v/codex/codex exec", "node /n/bin/codex exec", "/bin/bash -c s", "/h/.local/bin/claude", "sot-capsule run"]),
        (1, &["bash", "/v/codex/codex", "node /n/bin/codex", "sot-capsule run"]),
        (1, &["bash", "julia s.jl", "bash -c s", "claude", "sot-capsule run"]),
        (1, &["bash", "python3 x.py", "make", "bash", "claude"]),
        (0, &["bash", "-bash", "sot-capsule run"]),
        (2, &["bash", "claude -p", "bash", "claude", "sot-capsule run"]),
        (2, &["bash", "codex", "codex"]),
        (1, &["bash", "claude", "bash", "tmux", "systemd"]),
        (1, &["bash", "claude", "sot-capsule run", "bash", "claude"]),
        (2, &["bash.exe", "codex.exe", "node.exe", "bash.exe", "claude.exe", "sot-capsule.exe"]),
        (1, &["bash", "node /x/claude-code/cli.js", "claude"]),
        (2, &["bash", "node /x y/codex.js", "claude", "sot-capsule run"]),
        (2, &["bash", "node /tmp/agent dir/node_modules/@anthropic-ai/claude-code/cli.js", "codex", "sot-capsule run"]),
        (2, &["bash", "node /tmp/a b/codex.mjs", "claude", "sot-capsule run"]),
    ];
    for (want, recs) in tbl {
        assert_eq!(count(recs, ' '), *want, "{recs:?}");
    }
    let tblu: &[(usize, &[&str])] = &[
        (2, &["bash", "/tmp/agent dir/codex|exec", "/tmp/agent dir/claude", "sot-capsule|run"]),
        (2, &["bash", "node|--no-warnings|/n/bin/codex.js|exec", "claude", "sot-capsule|run"]),
        (2, &["bash", "node|/tmp/x y/codex.js", "claude", "sot-capsule|run"]),
        (2, &["bash", "node|/n/node_modules/@openai/codex/bin/run.js", "claude", "sot-capsule|run"]),
        (2, &["bash", "node|/n/node_modules/@anthropic-ai/claude-code/cli.js", "codex", "sot-capsule|run"]),
        (2, &["bash.exe", r"node.exe|C:\n\node_modules\@openai\codex\bin\run.js", "claude.exe", "sot-capsule.exe"]),
        (1, &["bash", "node|--inspect", "claude", "sot-capsule|run"]),
        (1, &["bash", "/v/codex/codex", "node|/n/node_modules/@openai/codex/bin/run.js", "sot-capsule|run"]),
        (2, &["bash", "/v/claude", "node|/n/bin/codex.js", "sot-capsule|run"]),
        (1, &["bash", "codex", "node|/n/bin/codex.js", "sot-capsule|run"]),
        (2, &["bash.exe", "CLAUDE.EXE", "codex.exe", "sot-capsule.exe"]),
        (2, &["bash", "node|--require|/tmp/x.cjs|/n/node_modules/@anthropic-ai/claude-code/cli.js", "codex", "sot-capsule|run"]),
        (2, &["bash", "node|--require|/tmp/x.cjs|/n/bin/codex.js|exec", "claude", "sot-capsule|run"]),
        (1, &["bash", "node|--require|/tmp/x.cjs|/n/build.js", "claude", "sot-capsule|run"]),
    ];
    for (want, recs) in tblu {
        assert_eq!(count(recs, '|'), *want, "{recs:?}");
    }
}

fn req(recs: &[&str], end: Option<Rec>, row: &str) -> Result<(), (i32, String)> {
    let mut c: Vec<Rec> = recs.iter().map(|r| Rec::Proc(r.split('|').map(str::to_string).collect())).collect();
    c.push(end.unwrap_or(Rec::End));
    judge(Ok(c), row)
}

#[test]
fn a_second_agent_has_no_identity_and_an_unread_ancestry_is_refused() {
    assert_eq!(req(&["bash", "claude", "sot-capsule|run"], None, ""), Ok(()));
    let (rc, why) = req(&["bash", "codex", "bash", "claude", "sot-capsule|run"], None, "").unwrap_err();
    assert_eq!(rc, 1);
    assert!(why.contains("runs under codex, started inside claude's session") && why.contains("has no comm identity"), "{why}");
    let (rc, why) = req(&["bash", "codex"], Some(Rec::Truncated), "").unwrap_err();
    assert_eq!(rc, 2);
    assert!(why.contains("cannot read this process's ancestry"), "{why}");
    assert_eq!(req(&["bash", "claude", "bash"], Some(Rec::Truncated), "").unwrap_err().0, 2, "a cut chain hides the outer agent");
    assert_eq!(req(&["bash", "claude", "sot-capsule|run"], Some(Rec::Truncated), ""), Ok(()), "the capsule came first");
    assert_eq!(judge(Err("no /proc".into()), "").unwrap_err().0, 2);
}

/// A process that names a row acts as its handle only from inside that row's capsule.
#[test]
fn a_process_that_names_a_row_must_run_inside_it() {
    let away = |recs: &[&str]| req(recs, None, "ws-a").unwrap_err();
    for recs in [
        &["bash", "claude"][..],
        &["bash", "claude", "sot-capsule|run|/s/workspaces/ws-b/voyages/v1|v1|--cols|80"],
        &["bash", "claude", "sot-capsule"],
        &["bash", "claude", "sot-capsule|supervise|/s/workspaces/ws-b|--start|--|/s/workspaces/ws-a/voyages/v1"],
        &["bash", "claude", "sot-capsule|run|/s/workspaces/ws-ab/voyages/v1|v1"],
    ] {
        let (rc, why) = away(recs);
        assert!(rc == 2 && why.contains("is not shown to run inside that row's capsule"), "{recs:?}: {why}");
    }
    for recs in [
        &["bash", "claude", "sot-capsule|run|/s/workspaces/ws-a/voyages/v1|v1|--cols|80"][..],
        &["bash", "sot-capsule|supervise|/s/workspaces/ws-a|--start|--|claude"],
        &["bash", "claude", r"sot-capsule|run|C:\s\workspaces\ws-a\voyages\v1"],
    ] {
        assert_eq!(req(recs, None, "ws-a"), Ok(()), "{recs:?}");
    }
    assert_eq!(req(&["bash", "codex", "bash", "claude"], None, "ws-a").unwrap_err().0, 1, "a child is 1 whatever the row");
}

#[test]
fn linux_reads_proc_to_the_top() {
    let proc = tempfile::tempdir().unwrap();
    let put = |pid: u32, ppid: u32, argv: &[&str]| {
        let d = proc.path().join(pid.to_string());
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("stat"), format!("{pid} (a b) S {ppid} 0 0\n")).unwrap();
        std::fs::write(d.join("cmdline"), argv.iter().map(|a| format!("{a}\0")).collect::<String>()).unwrap();
    };
    put(30, 20, &["sotd", "comm", "send"]);
    put(20, 10, &["/h/.local/bin/claude", "--x"]);
    put(10, 1, &["sot-capsule", "run", "a\nb"]);
    let c = linux::chain(proc.path(), 30);
    assert_eq!(c.len(), 4);
    assert_eq!(c[2], Rec::Proc(vec!["sot-capsule".into(), "run".into(), "a\u{1c}b".into()]));
    assert_eq!(c[3], Rec::End);
    assert_eq!(linux::chain(proc.path(), 99), vec![Rec::Truncated], "a process with no record is no chain");
    assert_eq!(linux::ppid_of("5 (we) ird) S 7 1"), Some(7), "the parent follows the name's last parenthesis");
}

#[test]
fn macos_reads_ps_as_the_scripts_did() {
    let table = "  1     0 /sbin/launchd\n 10     1 sot-capsule run /x/workspaces/ws-a/voyages/v\n 20    10 (claude)\n 30    20 bash -c x\n";
    let c = macos::records(table, 30);
    assert_eq!(c[1], Rec::Proc(vec!["claude".into(), UNKNOWN_ARGS.into()]));
    assert_eq!(c.last(), Some(&Rec::End));
    assert_eq!(layers(&c, "ws-a"), (vec!["claude".to_string()], End::Ok));
    let node = macos::records(" 30 20 bash\n 20 1 (node)\n", 30);
    assert_eq!(node.last(), Some(&Rec::Truncated));
    assert!(macos::records("", 30).is_empty());
}

/// The Microsoft C runtime's rules, as the 0.6.6 scripts' `_sot_win_records` split them.
#[test]
fn a_windows_command_line_splits_as_the_c_runtime_splits_it() {
    let s = |cl: &str| split_windows_command_line(cl);
    assert_eq!(s(r#"a "b c" d"#), ["a", "b c", "d"]);
    assert_eq!(s(r#""a\\""#), [r"a\"]);
    assert_eq!(s(r#"a\\\""#), [r#"a\""#]);
    assert_eq!(s(r#""a""b c""#), [r#"a"b c"#]);
    assert_eq!(s("a\tb"), ["a", "b"]);
    assert_eq!(s("\"a\tb\""), ["a\tb"]);
    assert_eq!(s(r"C:\x\y.exe"), [r"C:\x\y.exe"]);
    assert_eq!(windows_record("node.exe", None), Rec::Truncated);
    assert_eq!(windows_record("claude.exe", None), Rec::Proc(vec!["claude.exe".into(), UNKNOWN_ARGS.into()]));
    assert_eq!(windows_record("x.exe", Some(r#""C:\a b\x.exe" -p"#)), Rec::Proc(vec!["x.exe".into(), "-p".into()]));
}

/// A native agent whose command line could not be read never counts once with its node host.
#[test]
fn an_unreadable_native_agent_is_its_own_layer() {
    let c = vec![
        Rec::Proc(vec!["bash.exe".into()]),
        windows_record("claude.exe", None),
        windows_record("node.exe", Some(r"node.exe C:\n\node_modules\@anthropic-ai\claude-code\cli.js")),
        Rec::Proc(vec!["sot-capsule.exe".into(), "run".into()]),
    ];
    assert_eq!(layers(&c, "").0.len(), 2);
}

/// Windows: an MSYS program that execs another leaves no live Windows parent; the walk goes on through the Git Bash
/// table, then natively above the first MSYS process whose parent is native.
#[test]
fn the_windows_walk_crosses_msys_execs_through_the_wrapper_table() {
    // sotd (500) <- wrapper bash (400, cyg 7) <- [exec chain] cyg 6 is 400's cygwin parent, winpid 300 (bash -c)
    // <- cyg 6's parent is native (ppid 1): claude.exe (200) <- sot-capsule.exe (100).
    let table = MsysTable::parse("7;7:400:6,6:300:1").unwrap();
    let native = |pid: u32| match pid {
        500 => Some(400),
        300 => Some(200),
        200 => Some(100),
        _ => None,
    };
    let name = |pid: u32| match pid {
        500 => "sotd.exe",
        400 | 300 => "bash.exe",
        200 => "claude.exe",
        100 => "sot-capsule.exe",
        _ => "x.exe",
    };
    let rec = |pid: u32| Rec::Proc(vec![name(pid).to_string(), "x".to_string()]);
    let c = windows_chain(500, &table, &native, &rec);
    assert_eq!(c.len(), 6, "{c:?}");
    assert_eq!(layers(&c, ""), (vec!["claude".to_string()], End::Ok));
    // With no table the walk stops at the wrapper's shell: a process outside any row passes, one naming a row is refused.
    let none = MsysTable::default();
    let native_only = |pid: u32| (pid == 500).then_some(400);
    let c = windows_chain(500, &none, &native_only, &rec);
    assert_eq!(layers(&c, "").1, End::Ok);
    assert_eq!(layers(&c, "ws-a").1, End::NoRow);
    assert!(MsysTable::parse("garbage").is_none());
}
