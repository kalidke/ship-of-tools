//! The Windows install's questions, each cut in turn on a fake computer and walked through the step that reads it to
//! its verdict line (scripts/CLAUDE.md, Installers: "Every question the install asks"). The tables run in a child
//! process of their own, since the verdict's step and tail are the process's.

use super::*;
use crate::install::observe::tests::{says, says_err, stall_after, tree, Canned, Fake};

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
        .answer("same-version", says_err(2, "sotd same-version: no hello"))
        .answer("update repls", says(0, ""))
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
