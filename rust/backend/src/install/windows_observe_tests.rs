//! The Windows install's questions, each cut in turn on a fake computer and walked through the step that reads it to
//! its verdict line (scripts/CLAUDE.md, Installers: "Every question the install asks"). The tables run in a child
//! process of their own, since the verdict's step and tail are the process's.

use super::*;
use crate::install::observe::tests::{repls, same_version, says, stall_after, tree, Canned, Fake};

const CUT: &str = "did not answer within";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const LISTING: &str = r#"[{"ProcessId":1,"ParentProcessId":0,"Name":"x.exe","ExecutablePath":"C:\\x.exe","CommandLine":"x"}]"#;

fn isolated(name: &str) -> bool {
    sot_platform::test_isolated::run_isolated_until(name, std::time::Instant::now() + Duration::from_secs(240)).unwrap()
}

/// A computer with a prefix whose base clone is there, a pack of 0.7.0 at `COMMIT`, and a pipe no daemon serves.
fn computer(root: &Path) -> Fake {
    std::fs::create_dir_all(root.join("prefix\\repo\\base\\.git")).unwrap();
    std::fs::create_dir_all(root.join("prefix\\bin")).unwrap();
    std::fs::create_dir_all(root.join("logs")).unwrap();
    let fake = Fake::new(Env::empty().with("USERPROFILE", root.join("home")).with("LOCALAPPDATA", root.join("local")).with("SOT_INSTALL_DAEMON_WAIT_S", "2"));
    fake.answer("Win32_Process", says(0, LISTING))
        .answer("same-version", same_version(crate::topology::same_version::Answer::Untold("no hello".into())))
        .answer("update repls", repls(crate::update::cli::Repls::default()))
        .answer("update stage", says(0, "staged"))
        .answer("GetCurrentProcess().SessionId", says(0, "2\n"))
        .answer("cat-file -e", says(0, ""))
        .answer("tag -l", says(0, "v0.7.0\n"))
        .answer("sotd.exe --version", says(0, "sotd 0.7.0 (0123456789 2026-10-26, protocol 9)\n"))
        .answer("sot.exe --version", says(0, "sot 0.7.0 (0123456789 2026-10-26, protocol 9)\n"))
        .answer("Get-SotLauncherCodeId", says(0, "AAA\n"));
    fake
}

fn ctx<'a>(fake: &'a Fake, root: &Path, version: &str) -> Ctx<'a> {
    let prefix = root.join("prefix");
    Ctx {
        m: fake,
        env: fake.env(),
        opts: Options::default(),
        pack: Pack { dir: root.join("pack"), version: version.into(), commit: COMMIT.into(), bin: root.join("pack\\bin"), libc: None },
        named: false,
        pipe: format!(r"\\.\pipe\sot-observe-test-{}", std::process::id()),
        log_dir: root.join("logs"),
        base: prefix.join("repo\\base"),
        version_dir: prefix.join("repo\\versions").join(format!("v{version}")),
        prefix,
        expect_daemon: false,
        repair: false,
    }
}

/// A listing in which this prefix's daemon runs, with no child, and a window supervisor whose launcher code id is
/// `AAA` when `supervised`.
fn daemon_listed(fake: &Fake, root: &Path, supervised: bool) {
    let exe = root.join("prefix\\bin\\sotd.exe").to_string_lossy().replace('\\', "\\\\");
    let sup = if supervised { r#",{"ProcessId":77,"ParentProcessId":1,"Name":"powershell.exe","ExecutablePath":"C:\\p.exe","CommandLine":"powershell -File launch-sot.ps1"}"# } else { "" };
    fake.answer("Win32_Process", says(0, &format!(r#"[{{"ProcessId":10,"ParentProcessId":1,"Name":"sotd.exe","ExecutablePath":"{exe}","CommandLine":"sotd"}}{sup}]"#)));
    if supervised {
        std::fs::write(root.join("logs\\launcher.pid"), "77\n").unwrap();
        std::fs::write(root.join("logs\\supervisor.log"), "t  pid=77  supervisor start (relaunched=False) depth=0 ws=90MB code=AAA\n").unwrap();
    }
}

/// The verdict a step's outcome gives: a stop's FAIL line and code, carrying what the run had collected, or the line
/// of what it found.
fn verdict_of(outcome: Result<(), Stop>, found: &Found) -> (String, i32) {
    match outcome {
        Ok(()) => verdict::line(found, ""),
        Err(stop) => verdict::stop_line(&found.carry(stop), ""),
    }
}

/// An install's record already at the prefix, whose bytes a layout that stops keeps.
fn recorded(root: &Path) -> Vec<u8> {
    let text = b"{\n  \"version\": \"0.6.9\",\n  \"commit\": \"fedcba9876543210fedcba9876543210fedcba98\"\n}\n".to_vec();
    std::fs::write(root.join("prefix\\install.json"), &text).unwrap();
    text
}

/// An update the stage armed: its pointer, whose bytes an update that stays armed keeps.
fn armed(c: &Ctx) -> Vec<u8> {
    let text = format!("{{\"version\":\"{}\"}}\n", c.pack.version).into_bytes();
    std::fs::create_dir_all(c.pointer().parent().unwrap()).unwrap();
    std::fs::write(c.pointer(), &text).unwrap();
    text
}

/// Each question the Windows layout and update ask, cut in turn: the checkout's Git reads stop the run, the record there
/// kept byte for byte, a build of the line never asks for a tag, the shortcut that cannot be read is a step, and an
/// update whose listing after the stage, logon session or daemon before an apply cannot be told stays armed (its
/// pointer kept byte for byte) with nothing stopped, its reason in the ONE STEP LEFT; a listing that gives no answer
/// before an update leaves its daemon unknown, which the run asks about and refuses with no console, staging nothing.
#[test]
fn a_cut_question_on_windows_says_why() {
    if !isolated("install::windows::observe_tests::a_cut_question_on_windows_says_why") {
        return;
    }
    for (name, pattern, version) in [("cat-file", "cat-file -e", "0.7.0"), ("tag -l", "tag -l", "0.7.0")] {
        let dir = tempfile::tempdir().unwrap();
        let fake = computer(dir.path());
        let record = recorded(dir.path());
        fake.answer(pattern, Canned::Stall);
        let (line, code) = verdict_of(init_base(&ctx(&fake, dir.path(), version)), &Found::default());
        assert!(code == 1 && line.starts_with("FAIL: ") && line.contains("could not ask git") && line.contains(CUT), "{name}: {line}");
        assert_eq!(std::fs::read(dir.path().join("prefix\\install.json")).unwrap(), record, "{name}: the record there stays byte for byte");
    }
    {
        let dir = tempfile::tempdir().unwrap();
        let fake = computer(dir.path());
        fake.answer("tag -l", Canned::Stall);
        init_base(&ctx(&fake, dir.path(), "0.7.0-dev+abc")).expect("a build of the line asks for no tag");
        assert!(!fake.was_asked("tag -l"), "{:?}", fake.asked());
    }
    {
        let dir = tempfile::tempdir().unwrap();
        let fake = computer(dir.path());
        let c = ctx(&fake, dir.path(), "0.7.0");
        let record = recorded(dir.path());
        std::fs::create_dir_all(&c.version_dir).unwrap();
        fake.answer("rev-parse HEAD", Canned::Stall);
        let (line, code) = verdict_of(version_folder(&c), &Found::default());
        assert!(code == 1 && line.contains("could not read HEAD") && line.contains(CUT), "{line}");
        assert_eq!(std::fs::read(dir.path().join("prefix\\install.json")).unwrap(), record, "the record there stays byte for byte");
    }
    {
        let dir = tempfile::tempdir().unwrap();
        let fake = computer(dir.path());
        fake.answer("CreateShortcut", Canned::Stall);
        let mut found = Found::default();
        let outcome = shortcut(&ctx(&fake, dir.path(), "0.7.0"), &mut found);
        let (line, code) = verdict_of(outcome, &found);
        assert!(code == 0 && line.starts_with("ONE STEP LEFT: ") && line.contains(CUT), "{line}");
    }
    type Setup = fn(&Fake, &Path);
    let none: Setup = |_, _| {};
    let listed: Setup = |f, r| daemon_listed(f, r, false);
    for (name, setup, pattern, how, armed) in [
        ("listing after the stage", none, "Win32_Process", stall_after(1, says(0, LISTING)), true),
        ("logon session", none, "GetCurrentProcess().SessionId", Canned::Stall, true),
        ("daemon before an apply here", listed, "same-version", Canned::Stall, true),
        ("listing before the update", none, "Win32_Process", Canned::Stall, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let fake = computer(dir.path());
        setup(&fake, dir.path());
        fake.answer(pattern, how);
        let bin = tree(&dir.path().join("prefix\\bin"));
        let mut c = ctx(&fake, dir.path(), "0.7.0");
        // The stage arms the update; the fake's stage writes nothing, so the pointer it would leave is here first.
        let pointer = armed.then(|| self::armed(&c));
        let sotd = c.pack.exe("sotd");
        let mut found = Found::default();
        let before = daemon_state(&c, &sotd, &mut found);
        let outcome = update(&mut c, &sotd, "0.6.9", before, &mut found).map(|step| {
            if let Some(step) = step {
                found.step(step);
            }
        });
        let (line, code) = verdict_of(outcome, &found);
        assert!(line.contains(CUT), "{name}: the reason is in the verdict: {line}");
        if armed {
            assert!(code == 0 && line.starts_with("ONE STEP LEFT: ") && (line.contains("stays armed") || line.contains("the start applies")), "{name}: {line}");
            assert!(!fake.was_asked("sot-apply.ps1") && !fake.was_asked("sot-local-daemon.ps1"), "{name}: nothing was stopped or applied: {:?}", fake.asked());
            assert_eq!(std::fs::read(c.pointer()).ok(), pointer, "{name}: the update stays armed, its pointer byte for byte");
        } else {
            assert!(code == 2 && line.starts_with("FAIL: daemon: "), "{name}: {line}");
            assert!(!fake.was_asked("update stage") && !c.pointer().exists(), "{name}: the refusal staged nothing");
        }
        assert_eq!(tree(&dir.path().join("prefix\\bin")), bin, "{name}: the binaries are untouched");
        println!("{name}: {line}");
    }
}

/// What a Windows run left, each question cut in turn: the binaries' versions, the listing, the window's launcher code
/// id and the daemon's answer (once, and after a converge) are FAIL with their reason, never PASS.
#[test]
fn a_cut_question_in_the_windows_check_is_never_pass() {
    if !isolated("install::windows::observe_tests::a_cut_question_in_the_windows_check_is_never_pass") {
        return;
    }
    // What a healthy run left: the binaries, the record, the junction to the version folder with its launcher.
    let laid = |root: &Path| -> Fake {
        let fake = computer(root);
        let c = ctx(&fake, root, "0.7.0");
        std::fs::create_dir_all(c.version_dir.join("scripts")).unwrap();
        std::fs::write(c.version_dir.join("scripts\\launch-sot.ps1"), "").unwrap();
        std::fs::write(c.record_path(), format!("{{\n  \"version\": \"0.7.0\",\n  \"commit\": \"{COMMIT}\"\n}}\n")).unwrap();
        let ran = sot_platform::test_wait::run_within(
            std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(c.prefix.join("repo\\current")).arg(&c.version_dir),
            Duration::from_secs(30),
        );
        assert_eq!(ran.status.code(), Some(0), "mklink /J: {ran}");
        fake
    };
    {
        let dir = tempfile::tempdir().unwrap();
        let fake = laid(dir.path());
        let mut found = Found::default();
        end_state(&ctx(&fake, dir.path(), "0.7.0"), &mut found);
        assert!(found.fails.is_empty(), "a healthy end state: {found:?}");
    }
    type Setup = fn(&Fake, &Path);
    let none: Setup = |_, _| {};
    let listed: Setup = |f, r| daemon_listed(f, r, false);
    let supervised: Setup = |f, r| daemon_listed(f, r, true);
    for (name, setup, pattern, check, converge) in [
        ("bin\\sotd.exe", none, "sotd.exe --version", "binaries", false),
        ("bin\\sot.exe", none, "sot.exe --version", "binaries", false),
        ("listing", none, "Win32_Process", "launcher", false),
        ("launcher code id", supervised, "Get-SotLauncherCodeId", "launcher", false),
        ("daemon's answer", listed, "same-version", "daemon", false),
        ("daemon's answer after a converge", listed, "same-version", "daemon", true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let fake = laid(dir.path());
        setup(&fake, dir.path());
        fake.answer(pattern, Canned::Stall);
        let mut c = ctx(&fake, dir.path(), "0.7.0");
        c.expect_daemon = converge;
        let mut found = Found::default();
        end_state(&c, &mut found);
        let (line, code) = verdict::line(&found, "");
        assert!(code == 1 && line.starts_with(&format!("FAIL: {check}: ")) && line.contains(CUT), "{name}: {line}");
        println!("{name}: {line}");
    }
}

/// The Windows run's questions before it writes, each cut in turn through `install_on` to its verdict line: the pack's
/// own binary and the daemon's endpoint stop the run (exit 1), and an implicit run over an install whose version, way
/// or build cannot be told refuses (exit 2); each leaves the binaries and the record there byte for byte.
#[test]
fn a_cut_question_before_a_windows_run_writes_stops_it() {
    if !isolated("install::windows::observe_tests::a_cut_question_before_a_windows_run_writes_stops_it") {
        return;
    }
    // A pack of 0.7.0 at COMMIT, a PATH with git and jq, and the fake's answers for a healthy run up to its first write.
    let machine = |root: &Path| -> (Fake, Options) {
        let fake = computer(root);
        let pack = root.join("pack");
        std::fs::create_dir_all(pack.join("sot-0.7.0-windows-x86_64")).unwrap();
        for b in ["sot.exe", "sotd.exe", "sot-capsule.exe"] {
            std::fs::write(pack.join("sot-0.7.0-windows-x86_64").join(b), b).unwrap();
        }
        std::fs::write(pack.join("sot-0.7.0-windows-x86_64.zip"), "archive").unwrap();
        std::fs::write(pack.join("COMMIT"), format!("{COMMIT}\n")).unwrap();
        let sum = |p: &str| sot_updater::fetch::hash::sha256_bytes(&std::fs::read(pack.join(p)).unwrap());
        std::fs::write(pack.join("SHA256SUMS"), format!("{}  sot-0.7.0-windows-x86_64.zip\n{}  COMMIT\n", sum("sot-0.7.0-windows-x86_64.zip"), sum("COMMIT"))).unwrap();
        std::fs::create_dir_all(root.join("path")).unwrap();
        for tool in ["git.exe", "jq.exe"] {
            std::fs::write(root.join("path").join(tool), "").unwrap();
        }
        fake.set_env("PATH", root.join("path"));
        fake.set_env("PROCESSOR_ARCHITECTURE", "AMD64");
        fake.answer("session-socket-path", says(0, &format!("\\\\.\\pipe\\sot-observe-{}-local\n", std::process::id())))
            .answer("update direction", says(0, "newer\n"));
        let args = ["--pack".to_string(), pack.to_string_lossy().into_owned(), "-Prefix".to_string(), root.join("prefix").to_string_lossy().into_owned()];
        (fake, super::super::options::parse(&args).unwrap())
    };
    type Setup = fn(&Fake, &Path);
    let none: Setup = |_, _| {};
    let installed: Setup = |f, r| {
        std::fs::write(r.join("prefix\\bin\\sotd.exe"), "").unwrap();
        std::fs::write(r.join("prefix\\install.json"), "{\n  \"commit\": \"fedcba9876543210fedcba9876543210fedcba98\"\n}\n").unwrap();
        f.set_env("SOT_INSTALL_IMPLICIT", "1");
    };
    let recorded_implicit: Setup = |f, r| {
        recorded(r);
        f.set_env("SOT_INSTALL_IMPLICIT", "1");
    };
    let build: Setup = |f, r| {
        std::fs::write(r.join("prefix\\install.json"), "{\n  \"version\": \"0.7.0-dev+abcdef1\"\n}\n").unwrap();
        f.set_env("SOT_INSTALL_IMPLICIT", "1");
    };
    let build_listed: Setup = |f, r| {
        std::fs::write(r.join("prefix\\install.json"), "{\n  \"version\": \"0.7.0-dev+abcdef1\"\n}\n").unwrap();
        f.set_env("SOT_INSTALL_IMPLICIT", "1");
        f.answer("rev-list --all", says(0, &format!("{COMMIT}\nabcdef1{}\n", "0".repeat(33))));
    };
    for (name, setup, pattern, check, code) in [
        ("the pack's own sotd", none, "pack\\sot-0.7.0-windows-x86_64\\sotd.exe --version", "fetch", 1),
        ("the pack's own sot", none, "pack\\sot-0.7.0-windows-x86_64\\sot.exe --version", "fetch", 1),
        ("the daemon's endpoint", none, "session-socket-path", "fetch", 1),
        ("installed sotd, implicit", installed, "prefix\\bin\\sotd.exe --version", "direction", 2),
        ("direction, implicit", recorded_implicit, "update direction", "direction", 2),
        ("a build of the line", build, "rev-list --all", "target", 2),
        ("a build of the line, merge-base", build_listed, "merge-base --is-ancestor", "target", 2),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (fake, opts) = machine(dir.path());
        setup(&fake, dir.path());
        fake.answer(pattern, Canned::Stall);
        let (bin, record) = (tree(&dir.path().join("prefix\\bin")), std::fs::read(dir.path().join("prefix\\install.json")).ok());
        let Err(stop) = install_on(&fake, opts) else { panic!("{name}: the run went on past a cut question") };
        let (line, got) = verdict::stop_line(&stop, "");
        assert!(fake.was_asked(pattern), "{name}: `{pattern}` was never asked: {:?}", fake.asked());
        assert!(got == code && line.starts_with(&format!("FAIL: {check}: ")) && line.contains(CUT), "{name}: exit {got}: {line}");
        assert_eq!(tree(&dir.path().join("prefix\\bin")), bin, "{name}: the binaries are untouched");
        assert_eq!(std::fs::read(dir.path().join("prefix\\install.json")).ok(), record, "{name}: the record there stays byte for byte");
        println!("{name}: {line}");
    }
}
