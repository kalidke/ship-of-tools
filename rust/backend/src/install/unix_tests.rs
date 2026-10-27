use super::*;
use super::super::observe::Live;
use sot_platform::host::env::Env;

/// A scratch home and prefix whose `sotd`, `systemctl` and `loginctl` stand-ins append their argv to one log, in call
/// order; the checkout is this tree, whose library renders the unit. The pin a `--claim` writes names `claim_host`.
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new(claim_host: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let log = d.join("log");
        std::fs::create_dir_all(d.join("home/.config/systemd/user")).unwrap();
        std::fs::create_dir_all(d.join("prefix/bin")).unwrap();
        std::fs::create_dir_all(d.join("stubs")).unwrap();
        let pin = d.join("home/.config/systemd/user/sotd.service.d");
        sot_platform::test_exec::write_executable(
            &d.join("prefix/bin/sotd"),
            format!(
                "#!/bin/sh\nprintf 'sotd %s\\n' \"$*\" >> '{}'\ncase \"$*\" in *--claim*) mkdir -p '{}'; printf '[Unit]\\nConditionHost=|{claim_host}\\nConditionHost=|{claim_host}.*\\n' > '{}/topology.conf' ;; esac\n",
                log.display(),
                pin.display(),
                pin.display()
            ),
        );
        for name in ["systemctl", "loginctl"] {
            sot_platform::test_exec::write_executable(&d.join("stubs").join(name), format!("#!/bin/sh\nprintf '{name} %s\\n' \"$*\" >> '{}'\n", log.display()));
        }
        Self { dir }
    }
    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }
    fn stub(&self, name: &str) -> String {
        self.path("stubs").join(name).to_string_lossy().into_owned()
    }
    fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("log")).unwrap_or_default().lines().map(String::from).collect()
    }
    fn at(&self, line: &str) -> Option<usize> {
        self.log().iter().position(|l| l == line)
    }
    fn units(&self) -> PathBuf {
        self.path("home/.config/systemd/user")
    }
    fn wiring(&self) -> String {
        let u = self.units();
        let dropin = if u.join("default.target.d/sotd.conf").is_file() { "dropin" } else { "none" };
        let link = if std::fs::symlink_metadata(u.join("default.target.wants/sotd.service")).is_ok_and(|m| m.file_type().is_symlink()) { "link" } else { "none" };
        format!("{dropin} {link}")
    }
}

fn tree() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn with_units<T>(f: &Fixture, run: impl FnOnce(&Units) -> T) -> T {
    let (home, prefix, checkout) = (f.path("home"), f.path("prefix"), tree());
    let (systemctl, loginctl) = (f.stub("systemctl"), f.stub("loginctl"));
    run(&Units { m: &Live, home: &home, prefix: &prefix, checkout: &checkout, systemctl: &systemctl, loginctl: &loginctl })
}

fn template() -> PathBuf {
    tree().join("deploy/sotd.service")
}

/// The unit is pinned to its hosts before any reload: the pin (claimed for this host with no hosts.toml) comes before a
/// manager reloads the unit, then it starts; a unit that cannot be pinned is never written, loaded or started, since on
/// a shared home every host would start it. Boot start is the drop-in, never an `enable` (on a shared home an enable
/// links the unit for every host): wired for a unit this run finds new, and over an existing one only where an old link
/// or the drop-in is, so a person's turn-off survives. A retire never disables the shared unit or removes its file:
/// under a declared topology it pins the unit, stops this host's run and reloads; with no hosts.toml, a retire on a
/// host the pin does not name alone keeps the boot drop-in, and on the host the pin names alone removes it; the claimed
/// pin names this host alone, and a claim names the host it moved the daemon from.
#[test]
fn the_unit_is_pinned_before_any_reload() {
    {
        let f = Fixture::new("me");
        with_units(&f, |u| enable_local_service(u, &template(), "/s.sock")).unwrap();
        let d = f.units().display().to_string();
        let pin = f.at(&format!("sotd topology pin --dir {d} --claim")).expect("a claimed pin");
        let reload = f.at("systemctl --user daemon-reload").expect("a reload");
        let start = f.at("systemctl --user start sotd.service").expect("a start");
        assert!(pin < reload && reload < start, "{:?}", f.log());
        assert!(f.units().join("sotd.service").is_file());
        assert_eq!(f.wiring(), "dropin none");
        assert!(!f.log().iter().any(|l| l.contains(" enable") && l.starts_with("systemctl")), "{:?}", f.log());

        let f = Fixture::new("me");
        sot_platform::test_exec::write_executable(&f.path("prefix/bin/sotd"), format!("#!/bin/sh\nprintf 'sotd %s\\n' \"$*\" >> '{}'\necho 'sotd topology: hosts.toml line 2: expected key = value' >&2\nexit 2\n", f.path("log").display()));
        let stop = with_units(&f, |u| enable_local_service(u, &template(), "/s.sock")).unwrap_err();
        assert!(stop.detail.contains("could not pin sotd.service") && stop.detail.contains("hosts.toml line 2"), "{stop:?}");
        assert!(!f.log().iter().any(|l| l.starts_with("systemctl --user daemon-reload") || l.starts_with("systemctl --user start")), "{:?}", f.log());
        assert!(!f.units().join("sotd.service").exists(), "no unpinned unit is written");
    }

    {
        for (state, want) in [("link", "dropin none"), ("dropin", "dropin none"), ("none", "none none")] {
            let f = Fixture::new("me");
            let u = f.units();
            std::fs::write(u.join("sotd.service"), format!("[Service]\nExecStart={}/bin/sotd --x\n", f.path("prefix").display())).unwrap();
            match state {
                "link" => {
                    std::fs::create_dir_all(u.join("default.target.wants")).unwrap();
                    std::os::unix::fs::symlink("../sotd.service", u.join("default.target.wants/sotd.service")).unwrap();
                }
                "dropin" => {
                    std::fs::create_dir_all(u.join("default.target.d")).unwrap();
                    std::fs::write(u.join("default.target.d/sotd.conf"), "[Unit]\nWants=sotd.service\n").unwrap();
                }
                _ => {}
            }
            with_units(&f, |u| enable_local_service(u, &template(), "/s.sock")).unwrap();
            assert_eq!(f.wiring(), want, "{state}");
        }
    }

    {
        let f = Fixture::new("me");
        std::fs::write(f.units().join("sotd.service"), "[Service]\nExecStart=/x/bin/sotd\n").unwrap();
        with_units(&f, |u| retire_local_service(u, true, "me", &mut Found::default())).unwrap();
        let d = f.units().display().to_string();
        let pin = f.at(&format!("sotd topology pin --dir {d}")).expect("a pin");
        let stop = f.at("systemctl --user stop sotd.service").expect("a stop");
        let reload = f.at("systemctl --user daemon-reload").expect("a reload");
        assert!(pin < stop && stop < reload, "{:?}", f.log());
        assert!(!f.log().iter().any(|l| l.contains("disable")), "{:?}", f.log());
        assert!(f.units().join("sotd.service").is_file());
        let f = Fixture::new("me");
        with_units(&f, |u| retire_local_service(u, true, "me", &mut Found::default())).unwrap();
        assert!(f.log().is_empty(), "with no unit file nothing runs: {:?}", f.log());
        let f = Fixture::new("me");
        std::fs::write(f.units().join("sotd.service"), "[Service]\nExecStart=/x/bin/sotd\n").unwrap();
        sot_platform::test_exec::write_executable(&f.path("prefix/bin/sotd"), format!("#!/bin/sh\nprintf 'sotd %s\\n' \"$*\" >> '{}'\necho 'sotd topology: hosts.toml line 2: expected key = value' >&2\nexit 2\n", f.path("log").display()));
        let stop = with_units(&f, |u| retire_local_service(u, true, "me", &mut Found::default())).unwrap_err();
        assert!(stop.detail.contains("hosts.toml line 2"), "{stop:?}");
        assert!(!f.log().iter().any(|l| l.starts_with("systemctl --user daemon-reload")), "a retire whose pin failed reloaded: {:?}", f.log());
    }

    {
        let f = Fixture::new("unithost-a");
        with_units(&f, |u| enable_local_service(u, &template(), "/s.sock")).unwrap();
        assert_eq!(f.wiring(), "dropin none");
        with_units(&f, |u| retire_local_service(u, false, "unithost-b", &mut Found::default())).unwrap();
        assert!(!f.log().iter().any(|l| l.contains("disable")), "{:?}", f.log());
        assert_eq!(f.wiring(), "dropin none", "host B's retire keeps host A's boot start");
        with_units(&f, |u| retire_local_service(u, false, "unithost-a", &mut Found::default())).unwrap();
        assert_eq!(f.wiring(), "none none", "host A's own retire removes the drop-in its pin serves alone");
        let g = Fixture::new("unithost-b");
        let pin = g.units().join("sotd.service.d");
        std::fs::create_dir_all(&pin).unwrap();
        std::fs::write(pin.join("topology.conf"), "[Unit]\nConditionHost=|unithost-a\nConditionHost=|unithost-a.*\n").unwrap();
        assert_eq!(role::pin_hosts(&g.units()), ["unithost-a"]);
        with_units(&g, |u| enable_local_service(u, &template(), "/s.sock")).unwrap();
        assert_eq!(role::pin_hosts(&g.units()), ["unithost-b"], "the claim moved the daemon to host B");
    }

    {
        use sot_topology::relay_units::{host_pin, install_pin};
        let dir = tempfile::tempdir().unwrap();
        let (rel, text) = install_pin("me");
        let pin = dir.path().join(rel);
        std::fs::create_dir_all(pin.parent().unwrap()).unwrap();
        std::fs::write(&pin, text).unwrap();
        assert!(role::pin_names_only(dir.path(), "me") && !role::pin_names_only(dir.path(), "other") && !role::pin_names_only(dir.path(), ""));
        assert_eq!(role::pin_hosts(dir.path()), ["me"]);
        std::fs::write(&pin, host_pin(&["me", "you"])).unwrap();
        assert!(!role::pin_names_only(dir.path(), "me"));
        assert_eq!(role::pin_hosts(dir.path()), ["me", "you"]);
    }
}

/// Nothing is written before every gate: the front gate refuses a prefix or home a unit, launcher or shell line would
/// read as syntax, naming the character; the direction gate's advice to a build of the line fetches nothing into the
/// install's base clone, and says it could not tell.
#[test]
fn nothing_is_written_before_every_gate() {
    assert_eq!(path_refused("prefix", "/home/me/.local/share/sot"), None);
    for (path, ch) in [("/a b", "space"), ("/a%b", "%"), ("/a'b", "'"), ("/a\nb", "newline"), ("/a$b", "$"), ("/a;b", ";")] {
        let line = path_refused("prefix", path).unwrap();
        assert!(line.contains(&format!(" holds {ch}, ")), "{path}: {line}");
    }

    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let ran = sot_platform::test_wait::run_within(
            std::process::Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"]).args(args),
            std::time::Duration::from_secs(30),
        );
        assert_eq!(ran.status.code(), Some(0), "git {args:?}: {ran}");
        ran.stdout.trim().to_string()
    };
    let origin = dir.path().join("origin");
    let o = origin.to_string_lossy().into_owned();
    git(&["init", "-q", &o]);
    git(&["-C", &o, "config", "uploadpack.allowFilter", "true"]);
    std::fs::write(origin.join("f"), "built\n").unwrap();
    git(&["-C", &o, "add", "f"]);
    git(&["-C", &o, "commit", "-q", "-m", "built"]);
    let built = git(&["-C", &o, "rev-parse", "HEAD"]);
    let prefix = dir.path().join("prefix");
    let base = prefix.join("repo/base");
    // An install's base clone is blobless: a commit it lacks is one its promisor remote could be asked for.
    git(&["clone", "-q", "--filter=blob:none", "--no-checkout", &format!("file://{o}"), &base.to_string_lossy()]);
    std::fs::write(origin.join("f"), "released\n").unwrap();
    git(&["-C", &o, "commit", "-q", "-am", "released"]);
    let release = git(&["-C", &o, "rev-parse", "HEAD"]);
    let objects = |p: &std::path::Path| git(&["-C", &p.to_string_lossy(), "count-objects", "-v"]);
    let before = objects(&base);
    let way = super::super::build_direction(&Live, &prefix, &format!("0.6.7-dev+{}", &built[..9]), &release);
    assert!(way.as_ref().read(|w| matches!(w, super::super::Way::Untold(_)), |_| false), "{way:?}");
    assert_eq!(objects(&base), before, "the advice fetched into the install's base clone");
    git(&["-C", &base.to_string_lossy(), "fetch", "-q", "origin"]);
    assert_eq!(super::super::build_direction(&Live, &prefix, &format!("0.6.7-dev+{}", &built[..9]), &release), Fact::known(super::super::Way::Forward));
}

/// Folder trust is the agents' command's: what it did is said, and a failure (an older sotd, none at all, an outcome it
/// does not know) is never described as a declaration.
#[test]
fn folder_trust_is_said_as_the_command_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    let said = |body: &str| {
        let sotd = dir.path().join(format!("sotd-{}", body.len()));
        sot_platform::test_exec::write_executable(&sotd, format!("#!/bin/sh\n{body}\n"));
        trust_report(&Live, &sotd, dir.path())
    };
    assert_eq!(said("echo Declared"), ["folder trust declared"]);
    assert_eq!(said("echo Kept"), ["folder trust kept"]);
    assert_eq!(said("echo Maybe"), ["WARN: folder trust not declared - unexpected command outcome"]);
    assert_eq!(said("echo 'unknown subcommand trust' >&2; exit 64"), ["WARN: folder trust not declared (exit 64)", "unknown subcommand trust"]);
    assert_eq!(trust_report(&Live, &dir.path().join("missing"), dir.path())[0], "WARN: folder trust not declared (exit none)");
}

/// Cleanup may remove only a named, available version worktree. An unavailable registration can belong to another
/// computer sharing this repository, and a locked/dirty checkout must remain intact when Git refuses its removal.
#[test]
fn version_cleanup_keeps_unavailable_registrations_and_refused_worktrees() {
    // The actual cleanup runs in a child test with a scratch HOME; no process-wide environment is changed.
    if let Some(root) = std::env::var_os("SOT_TEST_VERSION_CLEANUP_ROOT") {
        let root = PathBuf::from(root);
        prune_versions(&Live, &root.join("repo"), &root.join("base"), "current", Some("previous"), &mut Found::default());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let base = root.join("base");
    let repo = root.join("repo");
    let versions = repo.join("versions");
    std::fs::create_dir_all(&versions).unwrap();
    let git = |args: &[&str]| {
        let mut command = std::process::Command::new("git");
        for (key, _) in std::env::vars_os().filter(|(k, _)| k.to_string_lossy().starts_with("GIT_")) {
            command.env_remove(key);
        }
        let ran = sot_platform::test_wait::run_within(
            command.env("HOME", &root)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false"])
                .args(args),
            std::time::Duration::from_secs(30),
        );
        assert_eq!(ran.status.code(), Some(0), "git {args:?}: {ran}");
        ran.stdout
    };
    let b = base.to_string_lossy().into_owned();
    git(&["init", "-q", &b]);
    git(&["-C", &b, "config", "gc.worktreePruneExpire", "now"]);
    std::fs::write(base.join("data"), "clean\n").unwrap();
    git(&["-C", &b, "add", "data"]);
    git(&["-C", &b, "commit", "-qm", "fixture"]);
    for name in ["old", "locked", "dirty"] {
        git(&["-C", &b, "worktree", "add", "--detach", &versions.join(name).to_string_lossy(), "HEAD"]);
    }
    let unavailable = root.join("unavailable");
    git(&["-C", &b, "worktree", "add", "--detach", &unavailable.to_string_lossy(), "HEAD"]);
    std::fs::remove_dir_all(&unavailable).unwrap();
    git(&["-C", &b, "worktree", "lock", &versions.join("locked").to_string_lossy()]);
    std::fs::write(versions.join("dirty/data"), "local change\n").unwrap();
    let mut cleanup = std::process::Command::new(std::env::current_exe().unwrap());
    for (key, _) in std::env::vars_os().filter(|(k, _)| k.to_string_lossy().starts_with("GIT_")) {
        cleanup.env_remove(key);
    }
    let ran = sot_platform::test_wait::run_within(
        cleanup.args(["--exact", "install::unix::tests::version_cleanup_keeps_unavailable_registrations_and_refused_worktrees", "--nocapture"])
            .env("SOT_TEST_VERSION_CLEANUP_ROOT", &root)
            .env("HOME", &root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1"),
        std::time::Duration::from_secs(30),
    );
    assert_eq!(ran.status.code(), Some(0), "{ran}");
    assert!(ran.stdout.contains("1 passed"), "cleanup body did not execute: {ran}");
    assert!(!versions.join("old").exists(), "an available clean version is removed");
    assert!(versions.join("locked/data").is_file(), "a refused worktree is kept intact");
    assert_eq!(std::fs::read_to_string(versions.join("dirty/data")).unwrap(), "local change\n");
    let registrations = git(&["-C", &b, "worktree", "list", "--porcelain"]);
    assert!(registrations.contains(&format!("worktree {}\n", unavailable.display())), "{registrations}");
}

/// A listing that failed stops the run before it writes; one that shows nothing says so.
#[test]
fn a_failed_listing_stops_the_run_before_it_writes() {
    let dir = tempfile::tempdir().unwrap();
    let prefix = dir.path();
    let cut = Fact::unknown(Unknown::new("ps -u 1", Why::Cut(ASK_BOUND)));
    let stop = running_here(cut, prefix).unwrap_err();
    assert!(stop.code == 1 && stop.detail.contains("did not answer within 60 s") && stop.detail.contains("nothing was changed"), "{stop:?}");
    let seen = procs::parse_ps(&format!("10 1 sotd {0}/bin/sotd --label sot\n13 1 sot {0}/bin/sot --socket s\n", prefix.display())).unwrap();
    assert_eq!(running_here(Fact::known(seen), prefix).unwrap(), (true, true));
    assert_eq!(running_here(Fact::known(procs::parse_ps("12 1 bash bash -c x\n").unwrap()), prefix).unwrap(), (false, false), "a listing that shows none says so");
}

/// `systemctl --user is-active` is read by what it prints: its documented inactive states are answers, a failure that
/// prints no state is not, and a stalled one ends at its bound; none is ever "not active" by silence.
#[test]
fn a_unit_question_is_read_by_its_state_and_a_failure_is_no_answer() {
    let said = |exit: i32, out: &str, err: &str| unit_active_of(&observe::Answer { exit, out: out.into(), err: err.into() });
    assert_eq!(said(0, "active\n", ""), Ok(true));
    assert_eq!(said(3, "inactive\n", ""), Ok(false));
    assert_eq!(said(3, "failed\n", ""), Ok(false));
    for (exit, out, err) in [(1, "", "Failed to connect to bus: No medium found"), (3, "mystery", ""), (0, "inactive", "")] {
        let why = said(exit, out, err).unwrap_err();
        assert!(why.to_string().starts_with("said '"), "{why}");
    }
    let fake = super::super::observe::tests::Fake::new(Env::empty());
    fake.answer("is-active", super::super::observe::tests::Canned::Stall);
    let why = unit_active(&fake, "systemctl").read(|a| panic!("a stalled unit question answered {a}"), |u| u);
    assert_eq!(why.why(), &Why::Cut(super::super::observe::tests::STALL_BOUND));
    let stop = unit_unknown(why);
    assert!(stop.code == 1 && stop.detail.contains("nothing was changed") && stop.detail.contains("XDG_RUNTIME_DIR"), "{stop:?}");
    let missing = unit_active(&Live, &tempfile::tempdir().unwrap().path().join("none").to_string_lossy()).read(|_| panic!(), |u| u);
    assert!(matches!(missing.why(), Why::NotStarted(_)), "{missing}");
}

/// A boot-wiring question answers only by exit 0 or 1; a library that cannot be sourced is no answer.
#[test]
fn boot_wiring_is_read_only_from_its_two_answers() {
    let dir = tempfile::tempdir().unwrap();
    let units = dir.path().join("units");
    std::fs::create_dir_all(&units).unwrap();
    let checkout = tree();
    let unit_dir = units.to_string_lossy().into_owned();
    assert_eq!(boot_wired(&Live, &checkout, &unit_dir), Fact::known(false));
    std::fs::create_dir_all(units.join("default.target.d")).unwrap();
    std::fs::write(units.join("default.target.d/sotd.conf"), "[Unit]\nWants=sotd.service\n").unwrap();
    assert_eq!(boot_wired(&Live, &checkout, &unit_dir), Fact::known(true));
    let why = boot_wired(&Live, &dir.path().join("no-checkout"), &unit_dir).read(|w| panic!("an unsourced library answered {w}"), |u| u);
    assert!(why.to_string().contains("exit 127"), "a library that cannot be sourced is not 'not wired': {why}");
    assert!(boot_start_note(&Live, &dir.path().join("no-checkout"), &unit_dir).read(|_| false, |_| true));
}

/// Git reads that cannot be answered are errors, never a clean tree or a present commit.
#[test]
fn a_git_read_that_fails_is_not_a_clean_tree() {
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert!(tree_dirty(&Live, &plain).read(|_| false, |_| true), "a folder git cannot read is not clean");
    let stop = git_value(&Live, &plain, &["rev-parse", "HEAD"], || "missing".into()).unwrap_err();
    assert!(stop.code == 1 && stop.detail == "missing", "{stop:?}");
    assert_eq!(glibc_of("ldd (Ubuntu GLIBC 2.35-0ubuntu3.8) 2.35\n"), Some((2, 35)));
    assert_eq!(glibc_of(""), None);
    assert_eq!(glibc_of("ldd (GNU libc) 2.39\nCopyright (C) 2024 Free Software Foundation, Inc.\n"), Some((2, 39)));
    assert_eq!(glibc_of("ldd (Debian GLIBC 2.36-9+deb12u4) 2.36\n"), Some((2, 36)));
    for bad in ["musl libc (x86_64)\nVersion 1.2.4\n", "something else 99.99\n", "ldd (busybox) 1.36\n", "ldd (GNU libc) 2\n", "ldd (GNU libc) two.35\n", "ldd (GNU libc)\n", "ldd (GNU libc) 99.99..\n", "ldd (GNU libc) 2..35\n", "ldd (GNU libc) .2.35\n"] {
        assert_eq!(glibc_of(bad), None, "{bad:?}");
    }
}
