use super::*;

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
        sot_log::test_exec::write_executable(&f.path("prefix/bin/sotd"), format!("#!/bin/sh\nprintf 'sotd %s\\n' \"$*\" >> '{}'\necho 'sotd topology: hosts.toml line 2: expected key = value' >&2\nexit 2\n", f.path("log").display()));
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

    {
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

    {
        use sot_protocol::topology::relay_units::{host_pin, install_pin};
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
        let ran = sot_log::test_wait::run_within(
            std::process::Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"]).args(args),
            std::time::Duration::from_secs(30),
        );
        assert_eq!(ran.status.code(), Some(0), "git {args:?}: {ran}");
        ran.stdout.trim().to_string()
    };
    let origin = dir.path().join("origin");
    let o = origin.to_string_lossy().into_owned();
    git(&["init", "-q", &o]);
    git(&["-C", &o, "commit", "-q", "--allow-empty", "-m", "built"]);
    let built = git(&["-C", &o, "rev-parse", "HEAD"]);
    let prefix = dir.path().join("prefix");
    let base = prefix.join("repo/base");
    git(&["clone", "-q", &o, &base.to_string_lossy()]);
    git(&["-C", &o, "commit", "-q", "--allow-empty", "-m", "released"]);
    let release = git(&["-C", &o, "rev-parse", "HEAD"]);
    let objects = |p: &std::path::Path| git(&["-C", &p.to_string_lossy(), "count-objects", "-v"]);
    let before = objects(&base);
    let way = super::super::build_direction(&prefix, &format!("0.6.7-dev+{}", &built[..9]), &release);
    assert!(way.starts_with("unknown: "), "{way}");
    assert_eq!(objects(&base), before, "the advice fetched into the install's base clone");
    git(&["-C", &base.to_string_lossy(), "fetch", "-q", "origin"]);
    assert_eq!(super::super::build_direction(&prefix, &format!("0.6.7-dev+{}", &built[..9]), &release), "forward");
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

