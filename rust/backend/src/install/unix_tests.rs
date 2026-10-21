use super::*;
use std::cell::RefCell;

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
        sot_log::test_exec::write_executable(
            &d.join("prefix/bin/sotd"),
            format!(
                "#!/bin/sh\nprintf 'sotd %s\\n' \"$*\" >> '{}'\ncase \"$*\" in *--claim*) mkdir -p '{}'; printf '[Unit]\\nConditionHost=|{claim_host}\\nConditionHost=|{claim_host}.*\\n' > '{}/topology.conf' ;; esac\n",
                log.display(),
                pin.display(),
                pin.display()
            ),
        );
        for name in ["systemctl", "loginctl"] {
            sot_log::test_exec::write_executable(&d.join("stubs").join(name), format!("#!/bin/sh\nprintf '{name} %s\\n' \"$*\" >> '{}'\n", log.display()));
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
    run(&Units { home: &home, prefix: &prefix, checkout: &checkout, systemctl: &systemctl, loginctl: &loginctl })
}

fn template() -> PathBuf {
    tree().join("deploy/sotd.service")
}

/// The unit is pinned to its hosts before any manager reloads it, then started; its boot start is the drop-in, and no
/// `enable` runs (on a shared home an enable would link it for every host).
#[test]
fn install_pins_the_unit_before_it_is_loaded() {
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
}

/// Boot start by one rule for both update paths: a unit this run finds new is wired; over an existing one only where an
/// old link or the drop-in is, so a person's turn-off survives.
#[test]
fn install_wires_boot_start_by_one_rule() {
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

/// A retire under a declared topology pins the unit, stops this host's run and reloads, and never disables the shared
/// unit or removes its file.
#[test]
fn a_retire_under_a_topology_pins_and_stops_and_never_disables() {
    let f = Fixture::new("me");
    std::fs::write(f.units().join("sotd.service"), "[Service]\nExecStart=/x/bin/sotd\n").unwrap();
    with_units(&f, |u| retire_local_service(u, true, "me"));
    let d = f.units().display().to_string();
    let pin = f.at(&format!("sotd topology pin --dir {d}")).expect("a pin");
    let stop = f.at("systemctl --user stop sotd.service").expect("a stop");
    let reload = f.at("systemctl --user daemon-reload").expect("a reload");
    assert!(pin < stop && stop < reload, "{:?}", f.log());
    assert!(!f.log().iter().any(|l| l.contains("disable")), "{:?}", f.log());
    assert!(f.units().join("sotd.service").is_file());
    let f = Fixture::new("me");
    with_units(&f, |u| retire_local_service(u, true, "me"));
    assert!(f.log().is_empty(), "with no unit file nothing runs: {:?}", f.log());
}

/// A home two hosts share, with no hosts.toml: a retire on a host the pin does not name alone stops its own run and
/// keeps the boot drop-in; on the host the pin names alone it removes it; a claim names the host it moved the daemon from.
#[test]
fn a_home_two_hosts_share_with_no_hosts_toml() {
    let f = Fixture::new("unithost-a");
    with_units(&f, |u| enable_local_service(u, &template(), "/s.sock")).unwrap();
    assert_eq!(f.wiring(), "dropin none");
    with_units(&f, |u| retire_local_service(u, false, "unithost-b"));
    assert!(!f.log().iter().any(|l| l.contains("disable")), "{:?}", f.log());
    assert_eq!(f.wiring(), "dropin none", "host B's retire keeps host A's boot start");
    with_units(&f, |u| retire_local_service(u, false, "unithost-a"));
    assert_eq!(f.wiring(), "none none", "host A's own retire removes the drop-in its pin serves alone");
    let g = Fixture::new("unithost-b");
    let pin = g.units().join("sotd.service.d");
    std::fs::create_dir_all(&pin).unwrap();
    std::fs::write(pin.join("topology.conf"), "[Unit]\nConditionHost=|unithost-a\nConditionHost=|unithost-a.*\n").unwrap();
    assert_eq!(role::pin_hosts(&g.units()), ["unithost-a"]);
    with_units(&g, |u| enable_local_service(u, &template(), "/s.sock")).unwrap();
    assert_eq!(role::pin_hosts(&g.units()), ["unithost-b"], "the claim moved the daemon to host B");
}

fn work(rows: &[&str], pluto: bool, unsure: Option<&str>) -> daemon::Work {
    daemon::Work { rows: rows.iter().map(|r| (*r).to_string()).collect(), pluto, unsure: unsure.map(String::from) }
}

/// The daemon question, run with stand-ins for the daemon, its children and the console: what it decided, and whether
/// it asked.
fn ask(q: &Question, live: daemon::Work, kids: Option<Vec<&'static str>>, answer: bool) -> (Result<(daemon::Work, bool), Stop>, bool) {
    let asked = RefCell::new(false);
    let asks = Asks {
        live: &|| live.clone(),
        kids: &|| kids.clone(),
        ask: &|_| {
            *asked.borrow_mut() = true;
            answer
        },
        note: &|_| {},
    };
    let got = question(q, &asks);
    (got, asked.into_inner())
}

fn q(before: State, unit_runs: bool, restart_flag: bool) -> Question<'static> {
    Question { before, unit_runs, retire_stops: false, restart_flag, socket: "/s.sock", why: "no answer" }
}

/// A run whose role has no daemon here stops an active local unit (the retire), so its live work is asked as a
/// restart's is, whatever version it runs; with nothing live, or --restart-daemon, it goes on.
#[test]
fn a_retire_asks_before_it_stops_a_live_daemon() {
    let retire = |before| Question { before, unit_runs: false, retire_stops: true, restart_flag: false, socket: "/s.sock", why: "no answer" };
    for before in [State::Current, State::Other, State::Held] {
        let (got, asked) = ask(&retire(before), work(&["alpha"], false, None), None, false);
        let stop = got.unwrap_err();
        assert!(asked && stop.code == 2 && stop.detail.contains("stopping it would end the REPLs of: alpha"), "{before:?}: {stop:?}");
    }
    let (got, asked) = ask(&retire(State::Current), work(&[], false, None), None, false);
    assert!(!asked && got.is_ok(), "nothing live: the retire goes on");
    let (got, asked) = ask(&Question { restart_flag: true, ..retire(State::Current) }, work(&["alpha"], false, None), None, false);
    assert!(!asked && got.is_ok(), "--restart-daemon answers it");
    let (got, _) = ask(&retire(State::Unknown), work(&[], false, None), Some(vec!["repl"]), false);
    assert_eq!(got.unwrap_err().code, 2, "a daemon that cannot be told, with a REPL child, stops the run");
}

/// A live daemon is asked before it ends: a unit this run would restart, on another version with a live REPL, stops the
/// run unless the console says yes or the person gave --restart-daemon; one a window holds, or one no unit runs, is
/// not restarted here and is not asked.
#[test]
fn a_live_daemon_is_asked_before_it_ends() {
    let repl = work(&["alpha"], false, None);
    let (got, asked) = ask(&q(State::Other, true, false), repl.clone(), None, false);
    let stop = got.unwrap_err();
    assert!(asked && stop.code == 2 && stop.detail.contains("restarting it would end the REPLs of: alpha"), "{stop:?}");
    let (got, asked) = ask(&q(State::Other, true, false), repl.clone(), None, true);
    assert!(asked && got.is_ok());
    let (got, asked) = ask(&q(State::Other, true, true), repl.clone(), None, false);
    assert!(!asked && got.is_ok(), "--restart-daemon answers it");
    for (before, unit) in [(State::Held, true), (State::Other, false)] {
        let (got, asked) = ask(&q(before, unit, false), repl.clone(), None, false);
        assert!(!asked && got.unwrap().0.rows == ["alpha"], "{before:?} {unit}: the step names the rows");
    }
    let (got, asked) = ask(&q(State::Other, true, false), work(&[], false, None), None, false);
    assert!(!asked && got.is_ok(), "nothing live is nothing to ask");
}

/// A daemon that runs and cannot be told its version is never taken for none: the run stops unless the unit can be
/// restarted and nothing live is lost (its own children say so) or the answer is yes.
#[test]
fn a_daemon_that_cannot_be_told_stops_the_run() {
    let (got, _) = ask(&q(State::Unknown, false, true), work(&[], false, None), Some(vec![]), true);
    assert_eq!(got.unwrap_err().code, 2, "no unit to restart: nothing answers it");
    let (got, asked) = ask(&q(State::Unknown, true, false), work(&[], false, None), Some(vec![]), false);
    assert!(!asked && got.is_ok(), "no REPL or Pluto child: the restart ends nothing live");
    let (got, asked) = ask(&q(State::Unknown, true, false), work(&[], false, None), Some(vec!["repl"]), false);
    assert!(asked && got.unwrap_err().code == 2);
    let (got, asked) = ask(&q(State::Unknown, true, false), work(&[], false, None), None, true);
    assert!(asked && got.is_ok());
}

/// A daemon that cannot say all of its live work is judged by its children; what it could not say is unknown.
#[test]
fn a_daemon_that_cannot_say_is_judged_by_its_children() {
    let unsure = work(&[], false, Some("no pluto field"));
    let (got, asked) = ask(&q(State::Other, true, false), unsure.clone(), Some(vec![]), false);
    assert!(!asked && !got.unwrap().1, "no REPL or Pluto child: nothing is unknown");
    let (got, asked) = ask(&q(State::Other, true, false), unsure.clone(), Some(vec!["pluto"]), false);
    let stop = got.unwrap_err();
    assert!(asked && stop.detail.starts_with("could not ask the daemon on /s.sock in full: it runs another version; no pluto field"), "{stop:?}");
    let (got, _) = ask(&q(State::Held, true, false), unsure, Some(vec!["repl"]), false);
    assert!(got.unwrap().1, "a daemon left running says its work is unknown in the step");
}

/// The front gate refuses a prefix or home a unit, launcher or shell line would read as syntax, naming the character.
#[test]
fn a_path_with_syntax_in_it_is_refused() {
    assert_eq!(path_refused("prefix", "/home/me/.local/share/sot"), None);
    for (path, ch) in [("/a b", "space"), ("/a%b", "%"), ("/a'b", "'"), ("/a\nb", "newline"), ("/a$b", "$"), ("/a;b", ";")] {
        let line = path_refused("prefix", path).unwrap();
        assert!(line.contains(&format!(" holds {ch}, ")), "{path}: {line}");
    }
}

/// Folder trust is the agents' command's: what it did is said, and a failure (an older sotd, none at all, an outcome it
/// does not know) is never described as a declaration.
#[test]
fn folder_trust_is_said_as_the_command_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    let said = |body: &str| {
        let sotd = dir.path().join(format!("sotd-{}", body.len()));
        sot_log::test_exec::write_executable(&sotd, format!("#!/bin/sh\n{body}\n"));
        trust_report(&sotd, dir.path())
    };
    assert_eq!(said("echo Declared"), ["folder trust declared"]);
    assert_eq!(said("echo Kept"), ["folder trust kept"]);
    assert_eq!(said("echo Maybe"), ["WARN: folder trust not declared - unexpected command outcome"]);
    assert_eq!(said("echo 'unknown subcommand trust' >&2; exit 64"), ["WARN: folder trust not declared (exit 64)", "unknown subcommand trust"]);
    assert_eq!(trust_report(&dir.path().join("missing"), dir.path())[0], "WARN: folder trust not declared (exit none)");
}

/// A unit that cannot be pinned to its hosts is never loaded or started: on a shared home every host would start it.
#[test]
fn a_unit_that_cannot_be_pinned_is_never_loaded() {
    let f = Fixture::new("me");
    sot_log::test_exec::write_executable(&f.path("prefix/bin/sotd"), format!("#!/bin/sh\nprintf 'sotd %s\\n' \"$*\" >> '{}'\necho 'sotd topology: hosts.toml line 2: expected key = value' >&2\nexit 2\n", f.path("log").display()));
    let stop = with_units(&f, |u| enable_local_service(u, &template(), "/s.sock")).unwrap_err();
    assert!(stop.detail.contains("could not pin sotd.service") && stop.detail.contains("hosts.toml line 2"), "{stop:?}");
    assert!(!f.log().iter().any(|l| l.starts_with("systemctl --user daemon-reload") || l.starts_with("systemctl --user start")), "{:?}", f.log());
}
