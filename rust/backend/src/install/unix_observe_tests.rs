//! The Unix install's questions, each cut in turn on a fake computer and walked through the phase that reads it to its
//! verdict line (scripts/CLAUDE.md, Installers: "Every question the install asks"). One table per phase; each runs
//! in a child process of its own, since the verdict's step is the process's.

use super::*;
use crate::install::observe::tests::{says, says_err, stall_after, tree, Canned, Fake};
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
            .answer("update direction", says(0, "newer\n"))
            .answer("--user cat sotd.service", says(0, &format!("[Service]\nExecStart={}/bin/sotd --label sot\n", root.join("prefix").display())));
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
        self
    }

    /// An install's record already at the prefix, whose bytes a layout that stops keeps.
    fn recorded(&self) -> Vec<u8> {
        std::fs::create_dir_all(self.prefix()).unwrap();
        let text = b"{\n  \"version\": \"0.6.9\",\n  \"commit\": \"fedcba9876543210fedcba9876543210fedcba98\"\n}\n".to_vec();
        std::fs::write(self.prefix().join("install.json"), &text).unwrap();
        text
    }

    /// The command line a row cuts: a fixed part, or one under the prefix.
    fn cut(&self, cut: &Cut) -> String {
        match cut {
            Cut::Line(p) => (*p).to_string(),
            Cut::Under(p) => format!("{}/{p}", self.prefix().display()),
        }
    }
}

/// Which command a row cuts.
enum Cut {
    Line(&'static str),
    /// A path under the prefix and what follows it on the command line.
    Under(&'static str),
}

/// What a row expects of the verdict.
enum Then {
    /// The run stops before it writes: FAIL with this check and exit code, nothing written under the prefix or home.
    Stop(&'static str, i32),
    /// The run had written: FAIL with this check and exit 1.
    Fail(&'static str),
    /// The run goes on, and its verdict is ONE STEP LEFT.
    Step,
    /// The run goes on, and the reason is a clause of its verdict.
    Clause,
}

/// One row: its name, its computer, the command it cuts and how, the run's flags, the verdict it expects with the
/// reason's words, and whether its code runs only on Linux.
struct Row {
    name: &'static str,
    setup: fn(&Computer),
    cut: Cut,
    how: Canned,
    flags: &'static [&'static str],
    then: Then,
    reason: &'static str,
    linux: bool,
}

fn row(name: &'static str, setup: fn(&Computer), cut: Cut, flags: &'static [&'static str], then: Then, linux: bool) -> Row {
    Row { name, setup, cut, how: Canned::Stall, flags, then, reason: CUT, linux }
}

fn none(_: &Computer) {}

/// One cut question, through the phase to the verdict: the line's word, its check, its exit code, the reason and, for
/// a stop, nothing written under the prefix or the home.
fn walk(c: &Computer, r: &Row, run: impl FnOnce(&Computer, &mut Found) -> Result<(), Stop>) -> String {
    let (name, pattern) = (r.name, c.cut(&r.cut));
    c.fake.answer(pattern.clone(), r.how.clone());
    let before = c.written();
    let mut found = Found::default();
    let (line, code) = match run(c, &mut found) {
        Ok(()) => verdict::line(&found, ""),
        Err(stop) => verdict::stop_line(&stop, ""),
    };
    assert!(c.fake.asked().iter().any(|l| l.contains(&pattern)), "{name}: `{pattern}` was never asked: {:?}", c.fake.asked());
    assert!(line.contains(r.reason), "{name}: the reason ({}) is in the verdict: {line}", r.reason);
    match &r.then {
        Then::Stop(check, want) => {
            assert!(line.starts_with(&format!("FAIL: {check}: ")) && code == *want, "{name}: exit {code}: {line}");
            assert_eq!(c.written(), before, "{name}: a stop before the first write wrote under the prefix or the home");
        }
        Then::Fail(check) => assert!(line.starts_with(&format!("FAIL: {check}: ")) && code == 1, "{name}: exit {code}: {line}"),
        Then::Step => assert!(code == 0 && line.starts_with("ONE STEP LEFT: "), "{name}: {line}"),
        Then::Clause => assert!(code == 0 && line.starts_with("PASS: "), "{name}: {line}"),
    }
    println!("{name}: {line}");
    line
}

/// Every question decide asks, cut in turn, and each answer a reader cannot read: each stops the run with its reason
/// and nothing written, but those the run goes past (a filesystem it cannot type is a step; a named run over an
/// install it cannot read, and a restart the person asked for over a daemon that cannot be told, say so in the PASS).
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
        assert!(!c.fake.was_asked("is-active") && !c.fake.was_asked("--user cat"), "no unit file: the user manager is never asked: {:?}", c.fake.asked());
    }
    for flags in [&["--local", "--no-service"][..], &["--be-only", "--no-service"][..]] {
        let c = Computer::new();
        c.fake.answer("is-active sotd.service", Canned::Stall);
        c.decide(flags, &mut Found::default()).unwrap_or_else(|stop| panic!("{flags:?}: {stop:?}"));
        assert!(!c.fake.was_asked("is-active"), "{flags:?}: no unit file, never asked: {:?}", c.fake.asked());
    }
    if linux {
        // An inactive unit of another install: a run that writes no unit leaves it alone (both questions asked), and
        // one that would write over it is refused by the unit file's own owner.
        let other = |c: &Computer| {
            let units = c.root().join("home/.config/systemd/user");
            std::fs::create_dir_all(&units).unwrap();
            std::fs::write(units.join("sotd.service"), "[Service]\nExecStart=/other/bin/sotd --label sot\n").unwrap();
            c.fake.answer("--user cat sotd.service", says(0, "[Service]\nExecStart=/other/bin/sotd --label sot\n"));
        };
        let c = Computer::new();
        other(&c);
        c.decide(&["--local", "--no-service"], &mut Found::default()).expect("an inactive unit of another install, left alone");
        assert!(c.fake.was_asked("is-active") && c.fake.was_asked("--user cat"), "{:?}", c.fake.asked());
        let c = Computer::new();
        other(&c);
        let Err(stop) = c.decide(&["--local"], &mut Found::default()) else { panic!("a run that would write over another install's unit went on") };
        assert!(stop.code == 2 && stop.detail.contains("/other/bin/sotd"), "{stop:?}");
    }
    let rows = vec![
        row("stat", none, Cut::Line("stat -f"), &["--local"], Then::Step, true),
        row("pack sotd", none, Cut::Line("pack/sot-0.7.0"), &["--local"], Then::Stop("unpack", 1), false),
        row("pack sot", none, Cut::Line("/sot --version"), &["--local"], Then::Stop("role", 1), false),
        row("topology status", none, Cut::Line("topology status"), &["--local"], Then::Stop("role", 1), false),
        Row { how: says(0, "garbage\n"), reason: "printed no table", ..row("topology status, unreadable", none, Cut::Line("topology status"), &["--local"], Then::Stop("role", 1), false) },
        row("topology plan, declared", |c| { c.fake.answer("topology status", says(0, "HOST DECLARED\nme frontend\n")); }, Cut::Line("topology plan"), &["--local"], Then::Stop("role", 1), false),
        Row { how: says(0, "nonsense\n"), reason: "printed no plan", ..row("topology plan, unreadable", |c| { c.fake.answer("topology status", says(0, "HOST DECLARED\nme frontend\n")); }, Cut::Line("topology plan"), &["--local"], Then::Stop("role", 1), false) },
        row("ldd", none, Cut::Line("ldd --version"), &["--local"], Then::Stop("role", 1), true),
        Row { how: says(0, "musl libc\n"), reason: "printed no glibc version", ..row("ldd, unreadable", none, Cut::Line("ldd --version"), &["--local"], Then::Stop("role", 1), true) },
        row("is-active", |c| { c.unit_file(); }, Cut::Line("is-active sotd.service"), &["--local"], Then::Stop("gate", 1), true),
        row("is-active, --no-service", |c| { c.unit_file(); }, Cut::Line("is-active sotd.service"), &["--local", "--no-service"], Then::Stop("gate", 1), true),
        Row { how: Canned::Missing, reason: "could not start", ..row("systemctl missing", |c| { c.unit_file(); }, Cut::Line("is-active sotd.service"), &["--local", "--no-service"], Then::Stop("gate", 1), true) },
        row("systemctl cat, active", |c| { c.unit_runs(); }, Cut::Line("--user cat sotd.service"), &["--local"], Then::Stop("gate", 2), true),
        row("systemctl cat, inactive", |c| { c.unit_file(); }, Cut::Line("--user cat sotd.service"), &["--be-only", "--no-service"], Then::Stop("gate", 2), true),
        Row { how: says(0, "[Unit]\n"), reason: "names no program", ..row("systemctl cat, unreadable", |c| { c.unit_file(); }, Cut::Line("--user cat sotd.service"), &["--local"], Then::Stop("gate", 2), true) },
        row("socket path", none, Cut::Line("session-socket-path"), &["--local"], Then::Stop("daemon", 1), false),
        row("ps", none, Cut::Line("ps -u"), &["--local"], Then::Stop("daemon", 1), false),
        row("same-version, unit running", |c| { c.unit_runs(); }, Cut::Line("same-version"), &["--local"], Then::Stop("daemon", 2), true),
        Row { how: stall_after(1, says(0, "1 0 init /sbin/init\n")), reason: "its children could not be listed", ..row("children, a daemon that cannot be told", |c| {
            c.unit_runs();
            c.fake.answer("same-version", Canned::Stall);
        }, Cut::Line("ps -u"), &["--local"], Then::Stop("daemon", 2), true) },
        row("update repls, unit on another version", |c| {
            c.unit_runs();
            c.fake.answer("same-version", says(3, "0.6.9\n"));
        }, Cut::Line("update repls"), &["--local"], Then::Stop("daemon", 2), true),
        Row { how: says(3, ""), reason: "printed no reading of its live work", ..row("update repls, unreadable", |c| {
            c.unit_runs();
            c.fake.answer("same-version", says(3, "0.6.9\n"));
        }, Cut::Line("update repls"), &["--local"], Then::Stop("daemon", 2), true) },
        row("restart asked for over a daemon that cannot be told", |c| { c.unit_runs(); }, Cut::Line("same-version"), &["--local", "--restart-daemon"], Then::Clause, true),
        row("installed sotd, implicit", |c| {
            std::fs::create_dir_all(c.prefix().join("bin")).unwrap();
            std::fs::write(c.prefix().join("bin/sotd"), "").unwrap();
            c.fake.set_env("SOT_INSTALL_IMPLICIT", "1");
        }, Cut::Under("bin/sotd --version"), &["--local"], Then::Stop("direction", 2), false),
        row("installed sotd, named", |c| {
            std::fs::create_dir_all(c.prefix().join("bin")).unwrap();
            std::fs::write(c.prefix().join("bin/sotd"), "").unwrap();
        }, Cut::Under("bin/sotd --version"), &["--local", "--version", "v0.7.0"], Then::Clause, false),
        row("direction, implicit", |c| {
            c.recorded();
            c.fake.set_env("SOT_INSTALL_IMPLICIT", "1");
        }, Cut::Line("update direction"), &["--local"], Then::Stop("direction", 2), false),
        row("direction, named", |c| { c.recorded(); }, Cut::Line("update direction"), &["--local", "--version", "v0.7.0"], Then::Clause, false),
        row("a build of the line, rev-list", |c| {
            std::fs::create_dir_all(c.prefix().join("repo/base")).unwrap();
            std::fs::write(c.prefix().join("repo/base/HEAD"), "ref: refs/heads/main\n").unwrap();
            std::fs::write(c.prefix().join("install.json"), "{\n  \"version\": \"0.7.0-dev+abcdef1\"\n}\n").unwrap();
            c.fake.set_env("SOT_INSTALL_IMPLICIT", "1");
        }, Cut::Line("rev-list --all"), &["--local"], Then::Stop("target", 2), false),
        row("a build of the line, merge-base", |c| {
            std::fs::create_dir_all(c.prefix().join("repo/base")).unwrap();
            std::fs::write(c.prefix().join("repo/base/HEAD"), "ref: refs/heads/main\n").unwrap();
            std::fs::write(c.prefix().join("install.json"), "{\n  \"version\": \"0.7.0-dev+abcdef1\"\n}\n").unwrap();
            c.fake.answer("rev-list --all", says(0, &format!("{}\nabcdef1{}\n", c.commit, "0".repeat(33))));
            c.fake.set_env("SOT_INSTALL_IMPLICIT", "1");
        }, Cut::Line("merge-base --is-ancestor"), &["--local"], Then::Stop("target", 2), false),
    ];
    for r in rows {
        if r.linux && !linux {
            continue;
        }
        let c = Computer::new();
        (r.setup)(&c);
        walk(&c, &r, |c, found| c.decide(r.flags, found).map(|_| ()));
    }
    // The console's ssh check, after a person chose the remote role (the console itself is a person's, not a fake's).
    let c = Computer::new();
    walk(&c, &row("console ssh check", none, Cut::Line("ssh"), &[], Then::Stop("role", 1), false), |c, _| {
        verdict::step("role");
        remote_role(&c.fake, "box".into()).map(|_| ())
    });
}

/// The state a healthy run left: the checkout, `repo/current`, the record, comm and the manifests; and the plan.
fn laid(c: &Computer, flags: &[&str]) -> Plan {
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
}

/// The checks after the run, each question cut in turn and each answer that does not read: FAIL with its reason,
/// never PASS; the boot-start line that cannot be read is a step.
#[test]
fn a_cut_question_in_the_check_is_never_pass() {
    if !isolated("install::unix::observe_tests::a_cut_question_in_the_check_is_never_pass") {
        return;
    }
    let linux = cfg!(target_os = "linux");
    {
        let c = Computer::new();
        let plan = laid(&c, &["--local"]);
        let mut found = Found::default();
        check(&c.fake, &plan, false, &mut found);
        let (line, code) = verdict::line(&found, "");
        assert!(code == 0 && line.starts_with(&format!("PASS: Ship of Tools {VERSION} at ")), "a healthy check passes: {line}");
    }
    let rows = vec![
        row("checkout HEAD", none, Cut::Under("repo/versions/v0.7.0 rev-parse HEAD"), &["--local"], Then::Fail("checkout"), false),
        Row { how: says(0, "garbage\n"), reason: "printed no commit id", ..row("checkout HEAD, unreadable", none, Cut::Under("repo/versions/v0.7.0 rev-parse HEAD"), &["--local"], Then::Fail("checkout"), false) },
        row("repo/current HEAD", none, Cut::Under("repo/current rev-parse HEAD"), &["--local"], Then::Fail("checkout"), false),
        row("bin/sotd", none, Cut::Under("bin/sotd --version"), &["--local"], Then::Fail("binaries"), false),
        row("bin/sot", none, Cut::Under("bin/sot --version"), &["--local"], Then::Fail("binaries"), false),
        row("unit state", none, Cut::Line("is-active sotd.service"), &["--local"], Then::Fail("daemon"), true),
        row("daemon's answer", none, Cut::Line("same-version"), &["--local"], Then::Fail("daemon"), true),
        Row { how: says(3, ""), reason: "printed another version with no version named", ..row("daemon's answer, unreadable", none, Cut::Line("same-version"), &["--local"], Then::Fail("daemon"), true) },
        row("boot wiring", none, Cut::Line("sot_boot_wired"), &["--local"], Then::Step, true),
        row("boot-off line", |c| { c.fake.answer("sot_boot_wired", says(1, "")); }, Cut::Line("sot_boot_off_line"), &["--local"], Then::Step, true),
        row("listing, no unit, the daemon current", none, Cut::Line("ps -u"), &["--be-only", "--no-service"], Then::Fail("daemon"), false),
        row("plan after a failed hub sync", |c| {
            c.fake.answer("topology sync", says_err(1, "ssh: connect to host box: Connection refused"));
        }, Cut::Line("topology plan"), &["--backend", "box"], Then::Fail("enroll"), false),
    ];
    for r in rows {
        if r.linux && !linux {
            continue;
        }
        let c = Computer::new();
        let plan = laid(&c, r.flags);
        (r.setup)(&c);
        walk(&c, &r, |c, found| {
            check(&c.fake, &plan, false, found);
            Ok(())
        });
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
    for r in [
        row("version gate", none, Cut::Line("exit(VERSION"), &[], Then::Fail("julia"), false),
        row("channel probe", none, Cut::Line("+1.12 -e exit(0)"), &[], Then::Fail("julia"), false),
        row("Julia version", none, Cut::Line("print(VERSION)"), &[], Then::Step, false),
        row("manifest tracking", none, Cut::Line("ls-files"), &[], Then::Clause, false),
    ] {
        let c = Computer::new();
        let plan = ready(&c);
        let line = walk(&c, &r, |c, found| julia_and_comm(&c.fake, &plan, found));
        if matches!(r.then, Then::Clause) {
            assert!(line.contains("kept julia/kernel/Manifest.toml") && plan.checkout.join("julia/kernel/Manifest.toml").is_file(), "a kept manifest stays: {line}");
        }
    }
}

/// A retire with no hosts.toml whose boot wiring cannot be read is a step, the unit folder untouched; a record whose
/// commit cannot be read is not written, and the one there stays byte for byte.
#[cfg(target_os = "linux")]
#[test]
fn a_cut_question_in_the_retire_or_the_record_says_why() {
    if !isolated("install::unix::observe_tests::a_cut_question_in_the_retire_or_the_record_says_why") {
        return;
    }
    let c = Computer::new();
    c.unit_file();
    let units = c.root().join("home/.config/systemd/user");
    let plan = c.decide(&["--backend", "box"], &mut Found::default()).unwrap();
    c.fake.answer("sot_boot_keep", says(0, "")).answer("stop sotd.service", says(0, "")).answer("trust declare", says(0, "Declared\n"));
    let units_before = tree(&units);
    walk(&c, &row("retire boot wiring", none, Cut::Line("sot_boot_wired"), &[], Then::Step, true), |c, found| configure(&c.fake, &plan, found));
    assert_eq!(tree(&units), units_before, "the cut wrote nothing in the unit folder");

    let c = Computer::new();
    let record = c.recorded();
    let plan = c.decide(&["--local"], &mut Found::default()).unwrap();
    walk(&c, &row("record commit", none, Cut::Line("rev-parse HEAD"), &[], Then::Stop("record", 1), true), |c, _| write_record(&c.fake, &plan));
    assert_eq!(std::fs::read(c.prefix().join("install.json")).unwrap(), record, "the record there stays byte for byte");
}

/// The layout's Git questions, cut in turn on a real clone of a scratch origin: the target's commit, the checkout's
/// state, a pack of the line's commit after a failed fetch and an old layout's checkout stop the run (FAIL, the record
/// there kept byte for byte), and an old version folder git cannot read is kept and named.
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

    let computer = || {
        let c = Computer::at(&commit);
        c.fake.set_env("SOT_INSTALL_ORIGIN", &origin);
        c.fake.answer("http.lowSpeedLimit=1000", Canned::Live);
        c
    };
    {
        let c = computer();
        let plan = c.decide(&["--local"], &mut Found::default()).unwrap();
        let mut found = Found::default();
        lay_out(&c.fake, &plan, &mut found).unwrap();
        assert!(plan.checkout.join("scripts/lib/sot-daemon.sh").is_file() && found.checked.is_empty(), "a healthy layout: {found:?}");
    }
    type Before = fn(&Computer, &Plan, &dyn Fn(&[&str]) -> String);
    let fresh: Before = |_, _, _| {};
    let laid_once: Before = |c, plan, _| lay_out(&c.fake, plan, &mut Found::default()).unwrap();
    let old_layout: Before = |c, _, git| {
        let current = c.prefix().join("repo/current");
        let cur = current.to_string_lossy().into_owned();
        git(&["init", "-q", &cur]);
        git(&["-C", &cur, "commit", "-q", "--allow-empty", "-m", "old"]);
    };
    let archive_fetch_fails: Before = |c, plan, _| {
        lay_out(&c.fake, plan, &mut Found::default()).unwrap();
        c.fake.answer("fetch --tags --force origin", says_err(1, "fatal: unable to access origin"));
    };
    let rows: Vec<(Row, Before, &[&str])> = vec![
        (row("the target's tag", none, Cut::Line("rev-parse refs/tags/v0.7.0"), &[], Then::Fail("layout"), false), fresh, &["--local"]),
        (row("the checkout's HEAD", none, Cut::Under("repo/versions/v0.7.0 rev-parse HEAD"), &[], Then::Fail("layout"), false), fresh, &["--local"]),
        (row("the checkout's state", none, Cut::Under("repo/versions/v0.7.0 status --porcelain"), &[], Then::Fail("layout"), false), laid_once, &["--local"]),
        (row("a pack of the line after a failed fetch", none, Cut::Line("rev-parse --verify"), &[], Then::Fail("layout"), false), archive_fetch_fails, &["--local", "--archive", "/a"]),
        (row("an old layout's checkout", none, Cut::Under("repo/current rev-parse --git-dir"), &[], Then::Fail("layout"), false), old_layout, &["--local"]),
        (row("an old layout's state", none, Cut::Under("repo/current status --porcelain"), &[], Then::Fail("layout"), false), old_layout, &["--local"]),
    ];
    for (r, before, flags) in rows {
        let c = computer();
        let record = c.recorded();
        let plan = c.decide(flags, &mut Found::default()).unwrap();
        before(&c, &plan, &git);
        walk(&c, &r, |c, found| lay_out(&c.fake, &plan, found));
        assert_eq!(std::fs::read(c.prefix().join("install.json")).unwrap(), record, "{}: the record there stays byte for byte", r.name);
    }
    let c = computer();
    for v in ["v0.0.0", "v0.0.1"] {
        std::fs::create_dir_all(c.prefix().join("repo/versions").join(v)).unwrap();
    }
    std::os::unix::fs::symlink(c.prefix().join("repo/versions/v0.0.1"), c.prefix().join("repo/current")).unwrap();
    let plan = c.decide(&["--local"], &mut Found::default()).unwrap();
    let line = walk(&c, &row("an old version folder", none, Cut::Under("repo/versions/v0.0.0 status --porcelain"), &[], Then::Clause, false), |c, found| lay_out(&c.fake, &plan, found));
    assert!(c.prefix().join("repo/versions/v0.0.0").is_dir() && line.contains("kept old version folder v0.0.0"), "{line}");
}
