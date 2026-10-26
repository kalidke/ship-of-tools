use super::*;
use super::observe::tests::{says, says_err, tree, Canned, Fake};
use super::observe::Unknown;

fn unknown(why: &str) -> Unknown {
    Unknown::new("q", Why::Failed(why.into()))
}

#[test]
fn a_commit_is_forty_lower_case_hex_digits() {
    assert!(is_commit(&"a".repeat(40)) && !is_commit(&"A".repeat(40)) && !is_commit("abc") && !is_commit(&"g".repeat(40)));
}

/// Never backwards unasked: an implicit run refuses an older target, and one it cannot compare while the installed
/// version is readable, whether or not its binary is there (the prefix is laid down fresh only after the direction says
/// it may be); a named run goes either way, and a direction it could not tell is said in its verdict and is never a
/// repair; a build of the line is told by ancestry which way the release moves it, and stops. The installed version is
/// the record's, else the installed sotd's own.
#[test]
fn a_run_that_names_no_target_never_moves_back() {
    let p = std::path::Path::new("/p");
    let mut found = verdict::Found::default();
    let guard = |implicit, installed: &str, dir: Fact<Direction>, found: &mut verdict::Found| guard_direction(implicit, installed, dir, "0.7.0", p, "--version", found);
    assert_eq!(guard(true, "0.7.1", Fact::known(Direction::Older), &mut found).unwrap_err().code, 2);
    assert_eq!(guard(true, "0.7.1", Fact::unknown(unknown("cut")), &mut found).unwrap_err().code, 2);
    assert!(found.checked.is_empty(), "a refusal names no clause: {found:?}");
    assert_eq!(guard(true, "", Fact::unknown(unknown("cut")), &mut found).unwrap(), None, "a record with no readable version is rewritten");
    assert!(found.checked[0].contains("could not tell which way v0.7.0 moved") && found.checked[0].contains("cut"), "{found:?}");
    assert_eq!(guard(false, "0.7.1", Fact::known(Direction::Older), &mut found).unwrap(), Some(Direction::Older), "a named run moves back");
    assert_eq!(guard(false, "0.7.1", Fact::unknown(unknown("cut")), &mut found).unwrap(), None, "a named run goes on, never as a repair");
    assert_eq!(guard(true, "0.7.0", Fact::known(Direction::Newer), &mut found).unwrap(), Some(Direction::Newer));

    let refuse = || Err(verdict::Stop::at("direction", 2, "older"));
    assert_eq!(meet("0.7.1", false, refuse).unwrap_err().code, 2);
    assert_eq!(meet("0.7.1", true, refuse).unwrap_err().code, 2);
    assert_eq!(meet("", false, refuse).unwrap(), Meet::Fresh, "nothing installed: nothing to guard");
    assert_eq!(meet("0.7.0", false, || Ok(())).unwrap(), Meet::Fresh);
    assert_eq!(meet("0.7.0", true, || Ok(())).unwrap(), Meet::Update);

    let line = build_stop("0.6.7-dev+abc", "v0.6.7", Fact::known(Way::Forward), "--archive", "--version");
    assert!(line.starts_with("this computer runs 0.6.7-dev+abc, a build of the line, not a release: install a pack of the line with --archive <folder>; the newest release, v0.6.7, contains this build: naming it with --version moves this computer forward"), "{line}");
    assert!(build_stop("x", "v1", Fact::known(Way::Untold("no base clone at /b".into())), "-Archive", "-Version").contains("(no base clone at /b)"));
    assert!(build_stop("x", "v1", Fact::unknown(unknown("git rev-list did not answer")), "-Archive", "-Version").contains("could not tell which way the newest release, v1, moves this computer (`q` git rev-list did not answer)"));
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::new(Env::empty());
    assert_eq!(build_direction(&fake, dir.path(), "0.6.7-dev+src", "0".repeat(40).as_str()), Fact::known(Way::Untold("this build names no commit".into())));
    assert!(build_direction(&fake, dir.path(), "0.6.7-dev+abcdef1", "0".repeat(40).as_str()).read(|w| matches!(w, Way::Untold(why) if why.starts_with("no base clone at")), |_| false));
    assert!(fake.asked().is_empty(), "nothing to ask: no child ran");

    assert_eq!(installed_version(&fake, dir.path()), Fact::known(String::new()), "no install is no version, not an unknown");
    std::fs::write(dir.path().join("install.json"), "{\"version\": \"0.6.6\", \"commit\": \"x\"}").unwrap();
    assert_eq!(installed_version(&fake, dir.path()), Fact::known("0.6.6".to_string()));
}

/// An installed `sotd` whose version cannot be read (it fails, prints none, or does not answer in time) is an install
/// that is there, never none: a run that names no target stops (exit 2, nothing written); one that names its target goes
/// on, and its verdict says the version could not be read.
#[test]
fn an_installed_sotd_that_cannot_say_its_version_is_not_no_install() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("bin")).unwrap();
    std::fs::write(dir.path().join("bin").join(pack::exe_name("sotd")), "").unwrap();
    let before = tree(dir.path());
    let fake = Fake::new(Env::empty());
    let sotd = format!("{} --version", dir.path().join("bin").join(pack::exe_name("sotd")).display());
    for (canned, why) in [(says_err(1, "cannot execute"), "ended with exit 1: cannot execute"), (says(0, "garbage\n"), "printed no version ('garbage')"), (Canned::Stall, "did not answer within 1 s")] {
        fake.answer(sotd.clone(), canned);
        let read = installed_version(&fake, dir.path());
        assert!(read.as_ref().read(|v| panic!("an unreadable sotd answered {v}"), |u| u.to_string().contains(why)), "{read:?}");
        let mut found = verdict::Found::default();
        let stop = installed_for(read.clone(), dir.path(), true, "--version", &mut found).unwrap_err();
        assert!(stop.code == 2 && stop.detail.contains("cannot be read") && stop.detail.contains(why) && stop.detail.contains("--version vX.Y.Z"), "{stop:?}");
        assert_eq!(installed_for(read, dir.path(), false, "--version", &mut found).unwrap(), "", "a named target moves either way");
        let (line, code) = verdict::line(&found, "");
        assert!(code == 0 && line.contains("could not be read") && line.contains(why), "the reason is in the verdict: {line}");
    }
    assert_eq!(tree(dir.path()), before, "the decision wrote nothing");
}

/// The daemon's endpoint name is asked before the run writes anything: a stalled `session-socket-path`, a failing one and
/// an invalid name each stop the run (exit 1, naming why), and none is read as "no daemon here".
#[test]
fn the_endpoint_name_is_a_decision_input_not_a_default() {
    let fake = Fake::new(Env::empty());
    let ask = |canned: Canned| {
        fake.answer("session-socket-path", canned);
        endpoint_name(ask_endpoint(&fake, Path::new("/pack/sotd"), "sot", |name| !name.is_empty()))
    };
    let stop = ask(Canned::Stall).unwrap_err();
    assert!(stop.code == 1 && stop.detail.contains("could not be named") && stop.detail.contains("did not answer within") && stop.detail.contains("nothing was changed"), "{stop:?}");
    assert_eq!(ask(says(0, "/run/sot.sock\n")).unwrap(), "/run/sot.sock");
    assert!(ask(says(0, "")).unwrap_err().detail.contains("no usable name"), "an empty name is no name");
    assert!(ask(says(1, "/run/sot.sock\n")).unwrap_err().detail.contains("ended with exit 1"), "a name from a failing exit is no name");
    fake.answer("session-socket-path", says(0, "C:\\other"));
    assert!(endpoint_name(ask_endpoint(&fake, Path::new("/pack/sotd"), "local", |name| name.starts_with("\\\\.\\pipe\\"))).is_err(), "an invalid pipe name is no name");
}

/// `update direction` answers newer, same or older; it prints `unknown` or fails when it cannot compare, and that, like
/// no answer, is unknown.
#[test]
fn a_direction_is_one_of_three_words() {
    let fake = Fake::new(Env::empty());
    let dir = tempfile::tempdir().unwrap();
    for (canned, want) in [(says(0, "newer\n"), Some(Direction::Newer)), (says(0, "same\n"), Some(Direction::Same)), (says(0, "older\n"), Some(Direction::Older)), (says(0, "unknown\n"), None), (says_err(1, "install.json records no version"), None), (Canned::Stall, None)] {
        fake.answer("update direction", canned);
        assert_eq!(direction(&fake, Path::new("/pack/sotd"), "0.7.0", dir.path(), "0.6.6").read(Some, |_| None), want);
    }
}
