//! The Windows install's questions, each cut in turn on a fake computer and walked through the step that reads it to
//! its verdict (scripts/CLAUDE.md, Installers: "Every question the install asks"). The table runs in a child process of its own, since the
//! verdict's step and tail are the process's.

use super::*;
use crate::install::observe::tests::{says, says_err, Canned, Fake};

const CUT: &str = "did not answer within";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// A computer with a prefix whose base clone is there, a pack of `version`, and a pipe no daemon serves.
fn computer(root: &Path) -> Fake {
    std::fs::create_dir_all(root.join("prefix\\repo\\base\\.git")).unwrap();
    std::fs::create_dir_all(root.join("logs")).unwrap();
    let fake = Fake::new(Env::empty().with("USERPROFILE", root.join("home")).with("LOCALAPPDATA", root.join("local")));
    fake.answer("Win32_Process", says(0, r#"[{"ProcessId":1,"ParentProcessId":0,"Name":"x.exe","ExecutablePath":"C:\\x.exe","CommandLine":"x"}]"#))
        .answer("same-version", says_err(2, "sotd same-version: no hello"))
        .answer("update repls", says(0, ""))
        .answer("update stage", says(0, "staged"))
        .answer("GetCurrentProcess().SessionId", says(0, "2\n"))
        .answer("cat-file -e", says(0, ""))
        .answer("tag -l", says(0, "v0.7.0\n"));
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

/// A listing in which this prefix's daemon runs, with no child.
fn daemon_listed(fake: &Fake, root: &Path) {
    let exe = root.join("prefix\\bin\\sotd.exe").to_string_lossy().replace('\\', "\\\\");
    fake.answer("Win32_Process", says(0, &format!(r#"[{{"ProcessId":10,"ParentProcessId":1,"Name":"sotd.exe","ExecutablePath":"{exe}","CommandLine":"sotd"}}]"#)));
}

/// Each question the Windows steps ask, cut in turn: the checkout's Git reads stop the run, a build of the line never
/// asks for a tag, the shortcut that cannot be read is a step, and an update whose logon session or daemon cannot be
/// told stays armed with nothing stopped, its reason in the step; a listing that gives no answer before an update
/// leaves its daemon unknown, which the run asks about and refuses with no console.
#[test]
fn a_cut_question_on_windows_says_why() {
    if !sot_platform::test_isolated::run_isolated_until("install::windows::observe_tests::a_cut_question_on_windows_says_why", std::time::Instant::now() + Duration::from_secs(240)).unwrap() {
        return;
    }
    let stop_line = |r: Result<(), Stop>| r.map(|()| String::new()).unwrap_or_else(|stop| verdict::stop_line(&stop, "").0);
    for (name, pattern, version) in [("cat-file", "cat-file -e", "0.7.0"), ("tag -l", "tag -l", "0.7.0")] {
        let dir = tempfile::tempdir().unwrap();
        let fake = computer(dir.path());
        fake.answer(pattern, Canned::Stall);
        let line = stop_line(init_base(&ctx(&fake, dir.path(), version)));
        assert!(line.starts_with("FAIL: ") && line.contains("could not ask git") && line.contains(CUT), "{name}: {line}");
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
        std::fs::create_dir_all(&c.version_dir).unwrap();
        fake.answer("rev-parse HEAD", Canned::Stall);
        let line = stop_line(version_folder(&c));
        assert!(line.contains("could not read HEAD") && line.contains(CUT), "{line}");
    }
    {
        let dir = tempfile::tempdir().unwrap();
        let fake = computer(dir.path());
        fake.answer("CreateShortcut", Canned::Stall);
        let mut found = Found::default();
        shortcut(&ctx(&fake, dir.path(), "0.7.0"), &mut found).unwrap();
        let line = verdict::line(&found, "").0;
        assert!(line.starts_with("ONE STEP LEFT: ") && line.contains(CUT), "{line}");
    }
    // An update: the logon session, then the daemon before an apply here, then the listing before the question.
    for (name, setup, pattern, armed) in [
        ("logon session", (|_: &Fake, _: &Path| {}) as fn(&Fake, &Path), "GetCurrentProcess().SessionId", true),
        ("daemon before an apply here", daemon_listed, "same-version", true),
        ("listing before the update", |_: &Fake, _: &Path| {}, "Win32_Process", false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let fake = computer(dir.path());
        setup(&fake, dir.path());
        fake.answer(pattern, Canned::Stall);
        let mut c = ctx(&fake, dir.path(), "0.7.0");
        let sotd = c.pack.exe("sotd");
        let got = update(&mut c, &sotd, "0.6.9");
        if armed {
            let step = got.unwrap_or_else(|stop| panic!("{name}: {stop:?}")).unwrap_or_else(|| panic!("{name}: the update applied on an unknown"));
            assert!(step.contains(CUT) && (step.contains("stays armed") || step.contains("the start applies")), "{name}: {step}");
            assert!(!fake.was_asked("sot-apply.ps1") && !fake.was_asked("sot-local-daemon.ps1"), "{name}: nothing was stopped or applied: {:?}", fake.asked());
        } else {
            let stop = got.expect_err("an unknown daemon with no console is refused");
            assert_eq!(stop.code, 2, "{name}: {stop:?}");
            assert!(!fake.was_asked("update stage"), "{name}: the refusal staged nothing");
        }
        println!("{name}: ok");
    }
}
