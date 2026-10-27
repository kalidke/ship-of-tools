//! The Unix install's questions, each cut in turn on a fake computer and walked through the phase that reads it to its
//! verdict line (scripts/CLAUDE.md, Installers: "Every question the install asks"). One table per phase; each runs in a child process of
//! its own, since the verdict's step is the process's.

use super::*;
use crate::install::observe::tests::{says, says_err, tree, Canned, Fake};
use sot_platform::host::env::Env;

const VERSION: &str = "0.7.0";
/// How a cut question's reason reads (a wait's last ask may be cut at less than the stall bound).
const CUT: &str = "did not answer within";

fn isolated(name: &str) -> bool {
    sot_platform::test_isolated::run_isolated_until(name, std::time::Instant::now() + Duration::from_secs(240)).unwrap()
}

use std::time::Duration;

/// A computer for one run: a home, a prefix, a pack of `VERSION` at `commit` for this system, a PATH of marker files
/// (found, never run: the fake answers them), and the fake's answers for a healthy fresh `--local` install.
struct Computer {
    dir: tempfile::TempDir,
    fake: Fake,
    commit: String,
}

impl Computer {
    fn new() -> Self {
        Self::at("0123456789abcdef0123456789abcdef01234567")
    }

    fn at(commit: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let target = sot_updater::platform::this_platform().unwrap();
        for d in ["home", "bin", "comm"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        for marker in ["jq", "systemctl", "git", "julia", "claude"] {
            std::fs::write(root.join("bin").join(marker), "").unwrap();
        }
        let pack = root.join("pack");
        let bin = pack.join(format!("sot-{VERSION}-{target}"));
        std::fs::create_dir_all(&bin).unwrap();
        for b in ["sot", "sotd", "sot-capsule"] {
            std::fs::write(bin.join(b), b).unwrap();
        }
        let archive = format!("sot-{VERSION}-{target}.tar.gz");
        std::fs::write(pack.join(&archive), "archive").unwrap();
        std::fs::write(pack.join("COMMIT"), format!("{commit}\n")).unwrap();
        let sum = |p: &str| sot_updater::fetch::hash::sha256_bytes(&std::fs::read(pack.join(p)).unwrap());
        std::fs::write(pack.join("SHA256SUMS"), format!("{}  {archive}\n{}  COMMIT\n", sum(&archive), sum("COMMIT"))).unwrap();

        let env = Env::empty()
            .with("HOME", root.join("home"))
            .with("PATH", root.join("bin"))
            .with("SOT_SELF_HOST", "me")
            .with("SOT_INSTALL_TTY", root.join("no-console"))
            .with("SOT_INSTALL_RESTART_WAIT_S", "2")
            .with("SOT_COMM_HOME", root.join("comm"))
            .with("JULIAUP_DEPOT_PATH", root.join("no-juliaup"));
        let fake = Fake::new(env);
        let short = &commit[..10];
        fake.answer("stat -f", says(0, "ext2/ext3\n"))
            .answer("/sotd --version", says(0, &format!("sotd {VERSION} ({short} 2026-10-26, protocol 9)\n")))
            .answer("/sot --version", says(0, &format!("sot {VERSION} ({short} 2026-10-26, protocol 9)\n")))
            .answer("topology status", says_err(1, "sotd topology: no hosts.toml at /h/.config/sot/hosts.toml (run `sotd topology sync --hub <alias>`)"))
            .answer("topology plan", says(0, "self me\ndial me unix:/run/sot.sock\n"))
            .answer("ldd --version", says(0, "ldd (GNU libc) 2.35\n"))
            .answer("is-active sotd.service", says(3, "inactive\n"))
            .answer("session-socket-path", says(0, "/run/sot.sock\n"))
            .answer("ps -u", says(0, "1 0 init /sbin/init\n"))
            .answer("same-version", says_err(2, "sotd same-version: no hello on unix:/run/sot.sock"))
            .answer("update repls", says(0, ""))
            .answer("update direction", says(0, "newer\n"));
        Self { dir, fake, commit: commit.to_string() }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn prefix(&self) -> PathBuf {
        self.root().join("prefix")
    }

    fn options(&self, flags: &[&str]) -> Options {
        let mut args = vec!["--pack".to_string(), self.root().join("pack").to_string_lossy().into_owned()];
        args.extend(flags.iter().map(|f| (*f).to_string()));
        super::super::options::parse(&args).unwrap()
    }

    fn decide(&self, flags: &[&str], found: &mut Found) -> Result<Plan, Stop> {
        decide(&self.fake, self.options(flags), self.prefix(), self.root().join("home"), found)
    }

    /// What the run could have written: the prefix and the home.
    fn written(&self) -> (Vec<(String, Vec<u8>)>, Vec<(String, Vec<u8>)>) {
        (tree(&self.prefix()), tree(&self.root().join("home")))
    }

    /// A `sotd.service` unit file of this install in the user's unit folder.
    fn unit_file(&self) -> &Self {
        let units = self.root().join("home/.config/systemd/user");
        std::fs::create_dir_all(&units).unwrap();
        std::fs::write(units.join("sotd.service"), format!("[Service]\nExecStart={}/bin/sotd --label sot\n", self.prefix().display())).unwrap();
        self
    }

    /// An active `sotd.service` of this install.
    fn unit_runs(&self) -> &Self {
        self.unit_file();
        self.fake.answer("is-active sotd.service", says(0, "active\n"));
        self.fake.answer("--user cat sotd.service", says(0, &format!("[Service]\nExecStart={}/bin/sotd --label sot\n", self.prefix().display())));
        self
    }
}

/// What a row expects of the verdict.
enum Then {
    /// The run stops before it writes: FAIL with this check and exit code.
    Stop(&'static str, i32),
    /// The run goes on, and its verdict is ONE STEP LEFT.
    Step,
    /// The run goes on, and the reason is a clause of its verdict.
    Clause,
}

/// One cut question, through the phase to the verdict: the line's word, its check, the reason and, for a stop, nothing
/// written under the prefix or the home.
fn walk(name: &str, c: &Computer, pattern: &str, then: &Then, run: impl FnOnce(&Computer, &mut Found) -> Result<(), Stop>) {
    c.fake.answer(pattern, Canned::Stall);
    let before = c.written();
    let mut found = Found::default();
    let (line, code) = match run(c, &mut found) {
        Ok(()) => verdict::line(&found, ""),
        Err(stop) => verdict::stop_line(&stop, ""),
    };
    assert!(c.fake.asked().iter().any(|l| l.contains(pattern)), "{name}: `{pattern}` was never asked: {:?}", c.fake.asked());
    assert!(line.contains(CUT), "{name}: the reason is in the verdict: {line}");
    match then {
        Then::Stop(check, want) => {
            assert!(line.starts_with(&format!("FAIL: {check}: ")) && code == *want, "{name}: exit {code}: {line}");
            assert_eq!(c.written(), before, "{name}: a stop before the first write wrote under the prefix or the home");
        }
        Then::Step => assert!(code == 0 && line.starts_with("ONE STEP LEFT: "), "{name}: {line}"),
        Then::Clause => assert!(code == 0 && line.starts_with("PASS: "), "{name}: {line}"),
    }
    println!("{name}: {line}");
}

/// Every question decide asks, cut in turn: each stops the run with its reason and nothing written, but the two the
/// run goes past (a filesystem it cannot type is a step; a named run over an install it cannot read says so).
#[test]
fn a_cut_question_in_decide_stops_before_any_write() {
    if !isolated("install::unix::observe_tests::a_cut_question_in_decide_stops_before_any_write") {
        return;
    }
    let linux = cfg!(target_os = "linux");
    {
        let c = Computer::new();
        let mut found = Found::default();
        let plan = c.decide(&["--local"], &mut found).expect("a healthy computer decides");
        assert!(found.steps.is_empty() && found.checked.is_empty() && !plan.others, "{found:?}");
        assert!(!c.fake.asked().iter().any(|l| l.contains("is-active")), "no unit file: the user manager is never asked: {:?}", c.fake.asked());
    }
    for flags in [&["--local", "--no-service"][..], &["--be-only", "--no-service"][..]] {
        let c = Computer::new();
        c.fake.answer("is-active sotd.service", Canned::Stall);
        c.decide(flags, &mut Found::default()).unwrap_or_else(|stop| panic!("{flags:?}: {stop:?}"));
        assert!(!c.fake.asked().iter().any(|l| l.contains("is-active")), "{flags:?}: no unit file, never asked: {:?}", c.fake.asked());
    }
    type Setup = fn(&Computer);
    let none: Setup = |_| {};
    let rows: Vec<(&str, Setup, &str, &[&str], Then, bool)> = vec![
        ("stat", none, "stat -f", &["--local"], Then::Step, true),
        ("pack sotd", none, "pack/sot-0.7.0", &["--local"], Then::Stop("unpack", 1), false),
        ("pack sot", none, "/sot --version", &["--local"], Then::Stop("role", 1), false),
        ("topology status", none, "topology status", &["--local"], Then::Stop("role", 1), false),
        ("ldd", none, "ldd --version", &["--local"], Then::Stop("role", 1), true),
        ("is-active", |c| { c.unit_file(); }, "is-active sotd.service", &["--local"], Then::Stop("gate", 1), true),
        ("is-active, --no-service", |c| { c.unit_file(); }, "is-active sotd.service", &["--local", "--no-service"], Then::Stop("gate", 1), true),
        ("systemctl cat", |c| { c.unit_runs(); }, "--user cat sotd.service", &["--local"], Then::Stop("gate", 2), true),
        ("socket path", none, "session-socket-path", &["--local"], Then::Stop("daemon", 1), false),
        ("ps", none, "ps -u", &["--local"], Then::Stop("daemon", 1), false),
        ("same-version, unit running", |c| { c.unit_runs(); }, "same-version", &["--local"], Then::Stop("daemon", 2), true),
        ("update repls, unit on another version", |c| {
            c.unit_runs();
            c.fake.answer("same-version", says(3, "0.6.9\n"));
        }, "update repls", &["--local"], Then::Stop("daemon", 2), true),
        ("topology plan, declared", |c| {
            c.fake.answer("topology status", says(0, "HOST DECLARED\nme frontend\n"));
        }, "topology plan", &["--local"], Then::Stop("role", 1), false),
        ("installed sotd, implicit", |c| {
            std::fs::create_dir_all(c.prefix().join("bin")).unwrap();
            std::fs::write(c.prefix().join("bin/sotd"), "").unwrap();
            c.fake.set_env("SOT_INSTALL_IMPLICIT", "1");
        }, "prefix/bin/sotd --version", &["--local"], Then::Stop("direction", 2), false),
        ("installed sotd, named", |c| {
            std::fs::create_dir_all(c.prefix().join("bin")).unwrap();
            std::fs::write(c.prefix().join("bin/sotd"), "").unwrap();
        }, "prefix/bin/sotd --version", &["--local", "--version", "v0.7.0"], Then::Clause, false),
        ("direction, implicit", |c| {
            std::fs::create_dir_all(c.prefix()).unwrap();
            std::fs::write(c.prefix().join("install.json"), "{\n  \"version\": \"0.6.9\"\n}\n").unwrap();
            c.fake.set_env("SOT_INSTALL_IMPLICIT", "1");
        }, "update direction", &["--local"], Then::Stop("direction", 2), false),
        ("direction, named", |c| {
            std::fs::create_dir_all(c.prefix()).unwrap();
            std::fs::write(c.prefix().join("install.json"), "{\n  \"version\": \"0.6.9\"\n}\n").unwrap();
        }, "update direction", &["--local", "--version", "v0.7.0"], Then::Clause, false),
        ("a build of the line, implicit", |c| {
            std::fs::create_dir_all(c.prefix().join("repo/base")).unwrap();
            std::fs::write(c.prefix().join("repo/base/HEAD"), "ref: refs/heads/main\n").unwrap();
            std::fs::write(c.prefix().join("install.json"), "{\n  \"version\": \"0.7.0-dev+abcdef1\"\n}\n").unwrap();
            c.fake.set_env("SOT_INSTALL_IMPLICIT", "1");
        }, "rev-list --all", &["--local"], Then::Stop("target", 2), false),
    ];
    for (name, setup, pattern, flags, then, linux_only) in rows {
        if linux_only && !linux {
            continue;
        }
        let c = Computer::new();
        setup(&c);
        walk(name, &c, pattern, &then, |c, found| c.decide(flags, found).map(|_| ()));
    }
}

/// The checks after the run, each question cut in turn: FAIL with its reason, never PASS; the boot-start line that
/// cannot be read is a step.
#[test]
fn a_cut_question_in_the_check_is_never_pass() {
    if !isolated("install::unix::observe_tests::a_cut_question_in_the_check_is_never_pass") {
        return;
    }
    let linux = cfg!(target_os = "linux");
    // The state a healthy run left: the checkout, `repo/current`, the record, comm and the manifests.
    let laid = |c: &Computer, flags: &[&str]| -> Plan {
        let mut found = Found::default();
        let plan = c.decide(flags, &mut found).unwrap();
        std::fs::create_dir_all(c.prefix().join("repo")).unwrap();
        for (env, _) in julia::ENVS {
            std::fs::create_dir_all(plan.checkout.join("julia").join(env)).unwrap();
            std::fs::write(plan.checkout.join("julia").join(env).join("Manifest.toml"), "").unwrap();
        }
        std::os::unix::fs::symlink(&plan.checkout, c.prefix().join("repo/current")).unwrap();
        std::fs::write(c.prefix().join("install.json"), format!("{{\n  \"version\": \"{VERSION}\",\n  \"commit\": \"{}\"\n}}\n", c.commit)).unwrap();
        std::fs::write(c.root().join("comm/VERSION"), format!("{}\n", &c.commit[..9])).unwrap();
        c.fake.answer("rev-parse HEAD", says(0, &format!("{}\n", c.commit)))
            .answer("is-active sotd.service", says(0, "active\n"))
            .answer("same-version", says(0, ""))
            .answer("sot_boot_wired", says(0, ""))
            .answer("sot_boot_off_line", says(0, "sotd.service: boot start is off\n"));
        plan
    };
    {
        let c = Computer::new();
        let plan = laid(&c, &["--local"]);
        let mut found = Found::default();
        check(&c.fake, &plan, false, &mut found);
        let (line, code) = verdict::line(&found, "");
        assert!(code == 0 && line.starts_with(&format!("PASS: Ship of Tools {VERSION} at ")), "a healthy check passes: {line}");
    }
    type Setup = fn(&Computer);
    let none: Setup = |_| {};
    let rows: Vec<(&str, Setup, &str, &[&str], Then, bool)> = vec![
        ("checkout HEAD", none, "rev-parse HEAD", &["--local"], Then::Stop("checkout", 1), false),
        ("bin/sotd", none, "prefix/bin/sotd --version", &["--local"], Then::Stop("binaries", 1), false),
        ("bin/sot", none, "prefix/bin/sot --version", &["--local"], Then::Stop("binaries", 1), false),
        ("unit state", none, "is-active sotd.service", &["--local"], Then::Stop("daemon", 1), true),
        ("daemon's answer", none, "same-version", &["--local"], Then::Stop("daemon", 1), true),
        ("boot wiring", none, "sot_boot_wired", &["--local"], Then::Step, true),
        ("boot-off line", |c| {
            c.fake.answer("sot_boot_wired", says(1, ""));
        }, "sot_boot_off_line", &["--local"], Then::Step, true),
        ("listing, no unit", |c| {
            c.fake.answer("same-version", says_err(2, "sotd same-version: no hello"));
        }, "ps -u", &["--be-only", "--no-service"], Then::Stop("daemon", 1), false),
        ("plan after a failed hub sync", |c| {
            c.fake.answer("topology sync", says_err(1, "ssh: connect to host box: Connection refused"));
        }, "topology plan", &["--backend", "box"], Then::Stop("enroll", 1), false),
    ];
    for (name, setup, pattern, flags, then, linux_only) in rows {
        if linux_only && !linux {
            continue;
        }
        let c = Computer::new();
        let plan = laid(&c, flags);
        setup(&c);
        // The check runs after every write, so its stops are FAILs of a run that wrote: only the verdict is compared.
        let then = match then {
            Then::Stop(check, _) => Then::Stop(check, 1),
            other => other,
        };
        c.fake.answer(pattern, Canned::Stall);
        let mut found = Found::default();
        check(&c.fake, &plan, false, &mut found);
        let (line, code) = verdict::line(&found, "");
        assert!(line.contains(CUT), "{name}: the reason is in the verdict: {line}");
        match then {
            Then::Stop(check, _) => assert!(code == 1 && line.starts_with(&format!("FAIL: {check}: ")), "{name}: {line}"),
            Then::Step => assert!(code == 0 && line.starts_with("ONE STEP LEFT: "), "{name}: {line}"),
            Then::Clause => unreachable!(),
        }
        println!("{name}: {line}");
    }
}

/// The Julia step's questions, cut in turn: the version and channel probes stop it (FAIL), a Julia whose version
/// cannot be asked is a step, a manifest git cannot place is kept and named.
#[test]
fn a_cut_question_in_the_julia_step_says_why() {
    if !isolated("install::unix::observe_tests::a_cut_question_in_the_julia_step_says_why") {
        return;
    }
    let ready = |c: &Computer| -> Plan {
        let plan = c.decide(&["--local"], &mut Found::default()).unwrap();
        std::fs::create_dir_all(plan.checkout.join("julia/kernel")).unwrap();
        std::fs::write(plan.checkout.join("julia/kernel/Project.toml"), "[compat]\njulia = \"1.12\"\n").unwrap();
        std::fs::write(plan.checkout.join("julia/kernel/Manifest.toml"), "").unwrap();
        c.fake.answer("exit(VERSION", says(0, ""))
            .answer("+1.12", says(0, ""))
            .answer("print(VERSION)", says(0, "1.12.1"))
            .answer("ls-files", says(1, ""))
            .answer("update_comm", says(0, ""))
            .answer("Pkg.instantiate", says(0, ""));
        plan
    };
    {
        let c = Computer::new();
        let plan = ready(&c);
        let mut found = Found::default();
        julia_and_comm(&c.fake, &plan, &mut found).unwrap();
        assert!(found.steps.is_empty() && found.checked.is_empty() && found.fail.is_none(), "{found:?}");
    }
    for (name, pattern, then) in [
        ("version gate", "exit(VERSION", Then::Stop("julia", 1)),
        ("channel probe", "+1.12", Then::Stop("julia", 1)),
        ("Julia version", "print(VERSION)", Then::Step),
        ("manifest tracking", "ls-files", Then::Clause),
    ] {
        let c = Computer::new();
        let plan = ready(&c);
        c.fake.answer(pattern, Canned::Stall);
        let mut found = Found::default();
        let line = match julia_and_comm(&c.fake, &plan, &mut found) {
            Ok(()) => verdict::line(&found, "").0,
            Err(stop) => verdict::stop_line(&stop, "").0,
        };
        assert!(line.contains(CUT), "{name}: {line}");
        match then {
            Then::Stop(check, _) => assert!(line.starts_with(&format!("FAIL: {check}: ")), "{name}: {line}"),
            Then::Step => assert!(line.starts_with("ONE STEP LEFT: "), "{name}: {line}"),
            Then::Clause => assert!(line.starts_with("PASS: ") && line.contains("kept julia/kernel/Manifest.toml"), "{name}: {line}"),
        }
        assert!(plan.checkout.join("julia/kernel/Manifest.toml").is_file() || !matches!(then, Then::Clause), "{name}: a kept manifest stays");
        println!("{name}: {line}");
    }
}

/// A retire with no hosts.toml whose boot wiring cannot be read is a step; a record whose commit cannot be read is not
/// written.
#[cfg(target_os = "linux")]
#[test]
fn a_cut_question_in_the_retire_or_the_record_says_why() {
    if !isolated("install::unix::observe_tests::a_cut_question_in_the_retire_or_the_record_says_why") {
        return;
    }
    let c = Computer::new();
    let units = c.root().join("home/.config/systemd/user");
    std::fs::create_dir_all(&units).unwrap();
    std::fs::write(units.join("sotd.service"), format!("[Service]\nExecStart={}/bin/sotd --x\n", c.prefix().display())).unwrap();
    let plan = c.decide(&["--backend", "box"], &mut Found::default()).unwrap();
    c.fake.answer("sot_boot_keep", says(0, "")).answer("stop sotd.service", says(0, "")).answer("trust declare", says(0, "Declared\n"));
    let units_before = tree(&units);
    walk("retire boot wiring", &c, "sot_boot_wired", &Then::Step, |c, found| configure(&c.fake, &plan, found));
    assert_eq!(tree(&units), units_before, "the cut wrote nothing in the unit folder");

    let c = Computer::new();
    let plan = c.decide(&["--local"], &mut Found::default()).unwrap();
    walk("record commit", &c, "rev-parse HEAD", &Then::Stop("record", 1), |c, _| write_record(&c.fake, &plan));
    assert!(!c.prefix().join("install.json").exists(), "no record is written");
}

/// The layout's Git questions, cut in turn on a real clone of a scratch origin: the target's tag and the checkout's
/// state stop the run (FAIL, the record not written), and an old version folder git cannot read is kept and named.
#[test]
fn a_cut_question_in_the_layout_keeps_what_is_there() {
    if !isolated("install::unix::observe_tests::a_cut_question_in_the_layout_keeps_what_is_there") {
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let origin = scratch.path().join("origin");
    let git = |args: &[&str]| {
        let ran = sot_platform::test_wait::run_within(
            std::process::Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"]).args(args),
            Duration::from_secs(30),
        );
        assert_eq!(ran.status.code(), Some(0), "git {args:?}: {ran}");
        ran.stdout.trim().to_string()
    };
    let o = origin.to_string_lossy().into_owned();
    git(&["init", "-q", &o]);
    std::fs::create_dir_all(origin.join("scripts/lib")).unwrap();
    std::fs::write(origin.join("scripts/lib/sot-daemon.sh"), "# library\n").unwrap();
    std::fs::write(origin.join("scripts/sot-apply.sh"), "#!/bin/sh\n").unwrap();
    git(&["-C", &o, "add", "."]);
    git(&["-C", &o, "commit", "-q", "-m", "release"]);
    git(&["-C", &o, "tag", &format!("v{VERSION}")]);
    let commit = git(&["-C", &o, "rev-parse", "HEAD"]);

    let computer = |old_versions: bool| {
        let c = Computer::at(&commit);
        c.fake.set_env("SOT_INSTALL_ORIGIN", &origin);
        c.fake.answer("http.lowSpeedLimit=1000", Canned::Live);
        if old_versions {
            for v in ["v0.0.0", "v0.0.1"] {
                std::fs::create_dir_all(c.prefix().join("repo/versions").join(v)).unwrap();
            }
            std::os::unix::fs::symlink(c.prefix().join("repo/versions/v0.0.1"), c.prefix().join("repo/current")).unwrap();
        }
        c
    };
    {
        let c = computer(false);
        let plan = c.decide(&["--local"], &mut Found::default()).unwrap();
        let mut found = Found::default();
        lay_out(&c.fake, &plan, &mut found).unwrap();
        assert!(plan.checkout.join("scripts/lib/sot-daemon.sh").is_file() && found.checked.is_empty(), "a healthy layout: {found:?}");
    }
    for (name, pattern, again, then) in [
        ("the target's tag", "rev-parse refs/tags/v0.7.0", false, Then::Stop("layout", 1)),
        ("the checkout's HEAD", "rev-parse HEAD", false, Then::Stop("layout", 1)),
        ("the checkout's state", "status --porcelain", true, Then::Stop("layout", 1)),
    ] {
        let c = computer(false);
        let plan = c.decide(&["--local"], &mut Found::default()).unwrap();
        if again {
            lay_out(&c.fake, &plan, &mut Found::default()).unwrap();
        }
        c.fake.answer(pattern, Canned::Stall);
        let line = match lay_out(&c.fake, &plan, &mut Found::default()) {
            Ok(()) => panic!("{name}: a cut layout question went on"),
            Err(stop) => verdict::stop_line(&stop, "").0,
        };
        let Then::Stop(check, _) = then else { unreachable!() };
        assert!(line.starts_with(&format!("FAIL: {check}: ")) && line.contains(CUT), "{name}: {line}");
        assert!(!c.prefix().join("install.json").exists(), "{name}: the record is written last, so not at all");
        println!("{name}: {line}");
    }
    let c = computer(true);
    let plan = c.decide(&["--local"], &mut Found::default()).unwrap();
    let old = c.prefix().join("repo/versions/v0.0.0");
    c.fake.answer(format!("{} status --porcelain", old.display()), Canned::Stall);
    let mut found = Found::default();
    lay_out(&c.fake, &plan, &mut found).unwrap();
    let line = verdict::line(&found, "").0;
    assert!(old.is_dir() && line.contains("kept old version folder v0.0.0") && line.contains(CUT), "an old folder git cannot read is kept and named: {line}");
}
