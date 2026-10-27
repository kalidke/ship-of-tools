//! The install on Linux and macOS: the steps of `scripts/install.sh`'s body, in its order. Every gate runs before the
//! first write under the prefix, `~/.local/bin`, the unit folder or the comm home; the unit, the wrappers and the boot
//! drop-in are rendered by the checkout's own `scripts/lib/sot-daemon.sh`, which the apply and the wrapper also run.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::daemon::{self, Asks, State};
use super::observe::{self, Fact, Live, Machine, Unknown, Why, ASK_BOUND};
use super::options::{Options, RoleFlag};
use super::pack::Pack;
use super::role::{self, Gate, Role};
use super::verdict::{self, step, Found, Stop};
use super::{cmd, julia, procs, record, say};

/// The glibc the frontend binary needs (the build image's).
const GLIBC_FLOOR_FE: (u32, u32) = (2, 35);

/// The characters a prefix or home may not hold: a unit, launcher or shell line would read them as syntax (systemd `%`
/// and `\`, the unit's `bash -c` and the launchers' quotes, `$`, backquote, newline; sed, JSON and shell lines `&`, `|`,
/// `;`, space, tab). The one list; `scripts/lib/sot-daemon.sh` and `scripts/sot-apply.sh` carry it for what runs after.
pub(super) fn path_refused(what: &str, path: &str) -> Option<String> {
    let named = [('%', "%"), ('\\', "\\"), ('\'', "'"), ('"', "\""), ('$', "$"), ('`', "`"), ('&', "&"), ('|', "|"), (';', ";"), (' ', "space"), ('\t', "tab"), ('\n', "newline")];
    let (_, ch) = named.iter().find(|(c, _)| path.contains(*c))?;
    Some(format!(
        "the {what} {path} holds {ch}, which a unit, launcher or shell line would read as syntax; use a {what} with none of: % \\ ' \" $ ` & | ; space tab newline"
    ))
}

/// Everything the run knows once it has decided: what it installs, where, and over what.
struct Plan {
    opts: Options,
    prefix: PathBuf,
    home: PathBuf,
    config: PathBuf,
    target: &'static str,
    macos: bool,
    pack: Pack,
    /// `repo/versions/<tag>`, the checkout of the target.
    checkout: PathBuf,
    scratch: PathBuf,
    named: bool,
    role: Role,
    be_alias: Option<String>,
    hub: String,
    me: String,
    topo_named: bool,
    topo_declared: bool,
    pin_excludes: bool,
    no_service: bool,
    /// The topology plan dials a daemon host other than this one: the launcher dials the plan.
    others: bool,
    pre_version: String,
    pre_commit: String,
    socket: String,
    unit_runs: bool,
    retire_stops: bool,
    /// A named run of the installed version and commit with no daemon on another version: it moves nothing.
    repair: bool,
    before: State,
    windows_before: bool,
    live: daemon::Work,
    rows_unknown: bool,
}

impl Plan {
    /// The record this run writes, at `commit`.
    fn record(&self, commit: &str) -> record::Record {
        record::Record {
            prefix: self.prefix.to_string_lossy().into_owned(),
            config: self.config.to_string_lossy().into_owned(),
            service: role::service_record(!self.macos, self.role.daemon, self.no_service).into(),
            version: self.pack.version.clone(),
            commit: commit.to_string(),
            installed_at: record::now_utc(),
            hub: self.hub.clone(),
            daemon: self.role.daemon,
            frontend: self.role.frontend,
            pinned: self.named,
        }
    }
    fn sotd(&self) -> PathBuf {
        self.prefix.join("bin/sotd")
    }
    fn units(&self) -> PathBuf {
        self.home.join(".config/systemd/user")
    }
    fn unit_managed(&self) -> bool {
        !self.macos && self.role.daemon && !self.no_service
    }
    fn units_ctx<'a>(&'a self, m: &'a dyn Machine) -> Units<'a> {
        Units { m, home: &self.home, prefix: &self.prefix, checkout: &self.checkout, systemctl: "systemctl", loginctl: "loginctl" }
    }
}

pub(super) fn install(opts: Options) -> Result<(Found, String), Stop> {
    let m = &Live;
    step("install");
    let env = m.env();
    let home = super::home_dir(&env).ok_or_else(|| Stop::new(1, "HOME is not set"))?;
    let prefix = super::absolute(&opts.prefix.clone().or_else(|| env.get_nonempty("SOT_PREFIX").map(PathBuf::from)).unwrap_or_else(|| home.join(".local/share/sot")));
    for (what, p) in [("prefix", &prefix), ("home", &home)] {
        if let Some(why) = path_refused(what, &p.to_string_lossy()) {
            println!("ERROR: {why}");
            return Err(Stop::new(1, format!("refusing the {what} (named above)")));
        }
    }
    let publish = super::publish::Plan::capture()?;
    let mut found = Found::default();
    let plan = decide(m, opts, prefix, home, &mut found)?;
    lay_out(m, &plan, &mut found)?;
    let enrolled = publish.enroll(&plan.checkout, &plan.pack.commit, &plan.prefix.join("repo/base"), &mut found)?;
    julia_and_comm(m, &plan, &mut found)?;
    configure(m, &plan, &mut found)?;
    let restarted = wire(m, &plan)?;
    write_record(m, &plan)?;
    check(m, &plan, restarted, &mut found);
    enrolled.check(&mut found);
    let pin = if plan.named { verdict::pin_tail(&plan.pack.version) } else { String::new() };
    Ok((found, pin))
}

/// Run one function of the checkout's `scripts/lib/sot-daemon.sh` with its arguments.
fn lib(m: &dyn Machine, checkout: &Path, func: &str, args: &[&str]) -> cmd::Ran {
    m.act(&mut lib_command(checkout, func, args))
}

/// `bash` running one function of the checkout's library; a library that cannot be sourced exits 127.
fn lib_command(checkout: &Path, func: &str, args: &[&str]) -> Command {
    let library = checkout.join("scripts/lib/sot-daemon.sh");
    let mut c = Command::new("bash");
    c.arg("-c").arg(". \"$1\" || exit 127; shift; \"$@\"").arg("bash").arg(&library).arg(func).args(args);
    c
}

fn systemctl(m: &dyn Machine, args: &[&str]) -> cmd::Ran {
    m.act(Command::new("systemctl").arg("--user").args(args))
}

fn sotd_at(m: &dyn Machine, sotd: &Path, args: &[&str]) -> cmd::Ran {
    m.act(Command::new(sotd).args(args))
}

/// A git read in `dir`: its first line when it exits 0, else the `Stop` that names `missing` (git answered with a
/// failure), or why git gave no answer.
fn git_value(m: &dyn Machine, dir: &Path, args: &[&str], missing: impl FnOnce() -> String) -> Result<String, Stop> {
    observe::ask(m, cmd::git_in(dir).args(args), ASK_BOUND, |a| Ok((a.exit == 0).then(|| a.first_line())))
        .or(|why| Stop::new(1, format!("could not ask git in {}: {why}", dir.display())))?
        .ok_or_else(|| Stop::new(1, missing()))
}

/// Whether a work tree has local changes, by `git status --porcelain`.
fn tree_dirty(m: &dyn Machine, dir: &Path) -> Fact<bool> {
    observe::ask(m, cmd::git_in(dir).args(["status", "--porcelain"]), ASK_BOUND, |a| Ok(!a.zero()?.out.trim().is_empty()))
}

/// Whether the unit folder `dir` wires boot start: `sot_boot_wired` exits 0 or 1, and any other exit (127: the library
/// cannot be sourced) is no answer.
fn boot_wired(m: &dyn Machine, checkout: &Path, dir: &str) -> Fact<bool> {
    observe::ask(m, &mut lib_command(checkout, "sot_boot_wired", &[dir]), ASK_BOUND, |a| match a.exit {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(a.failed()),
    })
}

/// The line about the daemon's boot start after the run, or why it could not be read.
fn boot_start_note(m: &dyn Machine, checkout: &Path, dir: &str) -> Fact<String> {
    boot_wired(m, checkout, dir).read(
        |wired| {
            if wired {
                return Fact::known("sotd.service: stop it on this computer with systemctl --user stop sotd.service; its boot start follows hosts.toml and the unit's host pin, and enable or disable change no file".to_string());
            }
            observe::ask(m, &mut lib_command(checkout, "sot_boot_off_line", &[]), ASK_BOUND, |a| Ok(a.zero()?.out.trim_end().to_string()))
        },
        Fact::unknown,
    )
}

/// The step for a boot start that could not be read.
fn boot_unknown(why: &Unknown) -> String {
    format!("check whether sotd starts at boot on this computer (~/.config/systemd/user/default.target.d/sotd.conf): {why}")
}

/// What `systemctl --user is-active sotd.service` said. It prints the unit's state and exits 0 only for an active one; a
/// state it documents as inactive is an answer, and anything else (a bus or permission failure prints no state) is not:
/// it is never "not active".
fn unit_active_of(a: &observe::Answer) -> Result<bool, Why> {
    match (a.exit, a.first_line().as_str()) {
        (0, "active" | "reloading" | "refreshing") => Ok(true),
        (code, "inactive" | "failed" | "activating" | "deactivating" | "maintenance") if code != 0 => Ok(false),
        (_, word) => Err(Why::Failed(format!("said '{word}' (exit {}): {}", a.exit, a.last_said()))),
    }
}

/// Whether `sotd.service` is active for this user here.
fn unit_active(m: &dyn Machine, systemctl: &str) -> Fact<bool> {
    observe::ask(m, Command::new(systemctl).arg("--user").args(["is-active", "sotd.service"]), ASK_BOUND, unit_active_of)
}

/// A unit question that could not be answered, before the run has written anything: it stops the run.
fn unit_unknown(why: Unknown) -> Stop {
    Stop::new(1, format!("{why}, so whether sotd.service runs is unknown; nothing was changed. Run this from a full login session, where systemctl --user reaches the user manager (XDG_RUNTIME_DIR set)"))
}

/// The daemon and window facts the decisions read from the process listing: whether a daemon process of the install
/// runs and whether a window does. A listing that failed stops the run before it writes: neither is then known.
fn running_here(all: Fact<Vec<procs::Proc>>, prefix: &Path) -> Result<(bool, bool), Stop> {
    let all = all.or(|why| Stop::new(1, format!("could not list this account's processes ({why}), so whether the install's daemon or window runs is unknown; nothing was changed")))?;
    Ok((!procs::daemon_pids(&all, prefix).is_empty(), procs::window_runs(&all, prefix)))
}

/// What `sotd topology status` says: its table, no hosts.toml, or a hosts.toml it cannot read (why). Every exit is an
/// answer; only a status that gave none is unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Topo {
    Table(String),
    Missing,
    Unreadable(String),
}

fn topology_status(m: &dyn Machine, sotd: &Path) -> Fact<Topo> {
    observe::ask(m, Command::new(sotd).args(["topology", "status"]), ASK_BOUND, |a| {
        Ok(match a.exit {
            0 => Topo::Table(a.out.clone()),
            _ if a.err.contains("no hosts.toml at") => Topo::Missing,
            _ => Topo::Unreadable(a.last_said()),
        })
    })
}

/// Whether the topology plan dials a daemon host other than this one (`me`): exit 0 is the plan, and an exit that names
/// no hosts.toml is none; any other exit is no answer.
fn plan_dials(m: &dyn Machine, sotd: &Path, me: &str) -> Fact<bool> {
    observe::ask(m, Command::new(sotd).args(["topology", "plan", "--self", me]), ASK_BOUND, |a| match a.exit {
        0 => Ok(role::plan_dials_others(&a.out)),
        _ if a.err.contains("no hosts.toml at") => Ok(false),
        _ => Err(a.failed()),
    })
}

/// Everything decided before the first write: the system, the pack, the direction, the role and its gates, the daemon.
/// Every question here that gives no answer stops the run, which has then written nothing; `found` takes the steps and
/// the clauses of what the run goes past.
#[allow(clippy::too_many_lines, reason = "the decisions in install.sh's order, each a few lines that read as one sequence")]
fn decide(m: &dyn Machine, opts: Options, prefix: PathBuf, home: PathBuf, found: &mut Found) -> Result<Plan, Stop> {
    step("preflight");
    let env = m.env();
    let target = sot_updater::platform::this_platform().filter(|t| *t != "windows-x86_64").ok_or_else(|| {
        Stop::new(1, "this system has no prebuilt release (Linux x86_64 and macOS on Apple silicon do); build from source")
    })?;
    let macos = target == "macos-aarch64";
    if macos {
        say("macOS support is EXPERIMENTAL — both roles work; please report anything broken");
    } else {
        for (label, dir) in [("$HOME", home.clone()), ("the state dir", env.get_nonempty("XDG_STATE_HOME").map_or_else(|| home.join(".local/state"), PathBuf::from))] {
            warn_remote_fs(m, label, &dir, found);
        }
    }
    if !observe::on_path(m, "jq") {
        let how = if macos { "brew install jq" } else { "sudo apt-get install jq" };
        return Err(Stop::new(1, format!("sot-comm needs jq, which is not on PATH: install it with `{how}`, then run this installer again")));
    }
    step("unpack");
    let want = opts.version.clone().or_else(|| if opts.archive.is_none() { super::bootstrap_tag(&env) } else { None });
    let pack = super::pack::read(m, &opts.pack, target, want.as_deref()).map_err(|why| Stop::new(1, why))?;
    if let Some(libc) = &pack.libc {
        say(&format!("archive sotd libc: {libc}"));
    }
    let implicit = super::implicit_run(&env) && !opts.named();
    let named = opts.named() || (super::bootstrap_tag(&env).is_some() && !implicit);
    say(&format!("installing Ship of Tools {} into {}", pack.tag(), prefix.display()));
    let scratch = pack.dir.join("tmp");
    let _ = std::fs::create_dir_all(&scratch);

    step("direction");
    let record_path = prefix.join("install.json");
    let pre_text = record::read(&record_path);
    let pre_version = super::installed_for(super::installed_version(m, &prefix), &prefix, implicit, "--version", found)?;
    let pre_commit = record::string(&pre_text, "commit").unwrap_or_default();
    let new_sotd = pack.exe("sotd");
    if implicit && pre_version.contains('+') {
        let way = super::build_direction(m, &prefix, &pre_version, &pack.commit);
        return Err(Stop::at("target", 2, super::build_stop(&pre_version, &pack.tag(), way, "--archive", "--version")));
    }
    let mut dir = None;
    if record_path.is_file() || !pre_version.is_empty() {
        let asked = super::direction(m, &new_sotd, &pack.version, &prefix, &pre_version);
        dir = super::guard_direction(implicit, &pre_version, asked, &pack.version, &prefix, "--version", found)?;
        match dir {
            None => say(&format!("note: could not tell which way {} moves the install at {} (it records {pre_version})", pack.tag(), prefix.display())),
            Some(super::Direction::Same) if pre_commit != pack.commit && super::is_commit(&pre_commit) => {
                say(&format!("this computer already runs {pre_version}: same version, another build ({} to {})", &pre_commit[..9], pack.short()));
            }
            Some(super::Direction::Same) => say(&format!("this computer already runs {pre_version} at {}", pack.short())),
            Some(way) => say(&format!("this run moves the install from {pre_version} to {} ({})", pack.version, if way == super::Direction::Newer { "newer" } else { "older" })),
        }
    }

    let config = sot_platform::host::state_dir::sot_config_dir_in(&env.clone().with("HOME", &home)).unwrap_or_else(|| home.join(".config").join("sot"));
    heal_old_hosts(&config);

    step("role");
    let mut hub = opts.hub.clone().unwrap_or_default();
    if !hub.is_empty() {
        let ran = sotd_at(m, &new_sotd, &["topology", "sync", "--hub", &hub]);
        print!("{}{}", ran.out, ran.err);
        if !ran.ok() {
            return Err(Stop::new(1, format!("topology sync --hub {hub} failed")));
        }
    } else if let Some(recorded) = record::string(&pre_text, "hub").filter(|h| !h.is_empty()) {
        hub = recorded;
        if !sotd_at(m, &new_sotd, &["topology", "sync", "--hub", &hub]).ok() {
            say(&format!("note: could not refresh hosts.toml from the recorded hub '{hub}'; the copy here stands"));
        }
    }
    let me = sot_platform::host::state_dir::host_name_in(&env).map_err(|e| Stop::new(1, e))?;
    let status = topology_status(m, &new_sotd).or(|why| Stop::new(1, format!("could not read the topology ({why}); nothing was changed")))?;
    let (topo_declared, topo_readable, topo_role) = match &status {
        Topo::Table(table) => (true, true, role::from_status(table, &me)),
        Topo::Missing => (false, false, None),
        Topo::Unreadable(said) => {
            say(&format!("WARNING: hosts.toml could not be read ({said}); the role comes from the flags, and the shared sotd.service is never disabled from here"));
            (true, false, None)
        }
    };
    let mut no_service = opts.no_service;
    let mut be_alias = None;
    let resolved = if let Some(r) = topo_role {
        if let Some(flag) = &opts.role {
            say(&format!("note: --{} given, but the declared topology names this host — the topology wins", flag_word(flag)));
        }
        say(&format!("role: the declared topology names '{me}' ({}) — installing/enabling accordingly", role_words(r)));
        r
    } else {
        let flag = match opts.role.clone() {
            Some(flag) => flag,
            None => match role::recorded(&pre_text, &std::fs::read_to_string(home.join(".local/bin/sot-launch")).unwrap_or_default()) {
                Some(flag) => {
                    if !macos && !matches!(flag, RoleFlag::Remote(_)) && record::string(&pre_text, "service").as_deref() == Some("none") {
                        no_service = true;
                    }
                    say(&format!("role: the install at {} recorded --{}{}; keeping it", prefix.display(), flag_word(&flag), if no_service { " --no-service" } else { "" }));
                    flag
                }
                None => ask_role(m, &me)?,
            },
        };
        if let RoleFlag::Remote(alias) = &flag {
            be_alias = Some(alias.clone());
        }
        let r = role::from_flag(&flag);
        say(&format!("role: no topology entry for '{me}' — using {} ({})", flag_word(&flag), role_words(r)));
        r
    };
    let pin_excludes = topo_readable && topo_role.is_none() && resolved.daemon;
    if pin_excludes {
        say(&format!("WARNING: hosts.toml declares no daemon on '{me}', so sotd.service's host pin keeps it from starting here; declare the host (daemon or frontend) in hosts.toml"));
    }
    if resolved.daemon && !["claude", "ccx"].iter().any(|p| observe::on_path(m, p)) && ![".local/bin/claude", ".claude/local/claude", ".local/bin/ccx"].iter().any(|p| home.join(p).is_file()) {
        say("WARNING: neither claude nor ccx (the Codex launcher) found on PATH — install and log in a coding agent on this machine before running sessions");
    }
    if !macos && resolved.frontend {
        let glibc = glibc_version(m).or(|why| Stop::new(1, format!("could not read this box's glibc ({why}); nothing was changed")))?;
        if glibc < GLIBC_FLOOR_FE {
            return Err(Stop::new(1, format!("the frontend binary needs glibc >= {}.{} (this box: {}.{}); --be-only installs the backend without it.", GLIBC_FLOOR_FE.0, GLIBC_FLOOR_FE.1, glibc.0, glibc.1)));
        }
    }
    if resolved.frontend {
        pack.check_binary(m, "sot").map_err(|why| Stop::new(1, why))?;
    }
    // The launcher: the one that dials the plan when the run names a backend or the plan lists another daemon host. A
    // hosts.toml that is not there, or that status could not read (the WARNING above), has no plan to dial.
    let others = resolved.frontend && be_alias.is_none() && topo_readable && plan_dials(m, &new_sotd, &me).or(|why| Stop::new(1, format!("could not read the topology plan ({why}); nothing was changed")))?;

    step("gate");
    let prefix_s = prefix.to_string_lossy().into_owned();
    // `sotd.service` is asked about wherever its unit file is in the user's unit folder, whatever the options: a run
    // never replaces another install's unit or works under one it does not own, and the retire stops it. A run that
    // finds no unit file (a fresh box, a container, macOS) never reads the user manager.
    let wires_unit = !macos && resolved.daemon && !no_service;
    let unit_file = home.join(".config/systemd/user/sotd.service").is_file();
    let unit_active = !macos && unit_file && observe::on_path(m, "systemctl") && unit_active(m, "systemctl").or(unit_unknown)?;
    let running = if unit_active { Some(running_unit_program(m)?) } else { None };
    match role::ownership(running.as_deref(), &home, &prefix_s, macos, resolved.frontend, wires_unit, opts.force_role_change) {
        Gate::Allow => {}
        Gate::Refuse(why) => {
            println!("       Installing here would replace or disable another install's files. Re-run with --force-role-change if that is what you want.");
            return Err(Stop::new(2, why));
        }
        Gate::Unresolvable(why) => return Err(Stop::new(2, why)),
    }
    if let Gate::Refuse(why) = role::host(topo_readable, topo_role.is_some_and(|r| r.daemon), &prefix, opts.force_role_change) {
        println!("       Declare this host in hosts.toml (frontend for a machine that runs a window, daemon for one other machines dial; a machine with a home of its own is declared in the hub's hosts.toml and re-runs with --hub <hub alias> to fetch it) and re-run, or run the installer on a host hosts.toml already declares. --force-role-change overrides this check.");
        return Err(Stop::new(2, why));
    }
    let service = role::service_record(!macos, resolved.daemon, no_service);
    if let Gate::Refuse(why) = role::record_gate(&record_path, topo_declared, resolved.daemon, service, opts.force_role_change) {
        println!("       If this host runs the daemon, drop --no-service. If hosts.toml could not be read, fix it (sotd topology status names the line). If this machine's own role changed, re-run with --force-role-change.");
        return Err(Stop::new(2, why));
    }

    step("daemon");
    let socket = super::endpoint_name(super::ask_endpoint(m, &new_sotd, "sot", |name| !name.is_empty()))?;
    let endpoint = format!("unix:{socket}");
    let (own_daemon, windows_before) = running_here(procs::snapshot(m), &prefix)?;
    let unit_runs = wires_unit && unit_active;
    // A role with no daemon here stops this host's run of the unit (the retire), so its daemon is asked too.
    let retire_stops = !resolved.daemon && unit_active && unit_file;
    let (before, why) = discover(m, &new_sotd, &endpoint, resolved.daemon, retire_stops, unit_runs, own_daemon);
    // A repair ends no daemon: the predicate the restart step and the retire act on says so.
    let repair = dir == Some(super::Direction::Same) && pre_commit == pack.commit && matches!(before, State::None | State::Current) && !ends_daemon(unit_runs, retire_stops, before);
    let mut plan = Plan {
        repair,
        checkout: prefix.join("repo/versions").join(pack.tag()),
        opts,
        prefix,
        home,
        config,
        target,
        macos,
        pack,
        scratch,
        named,
        role: resolved,
        be_alias,
        hub,
        me,
        topo_named: topo_role.is_some(),
        topo_declared,
        pin_excludes,
        no_service,
        others,
        pre_version,
        pre_commit,
        socket,
        unit_runs,
        retire_stops,
        before,
        windows_before,
        live: daemon::Work::default(),
        rows_unknown: false,
    };
    // A repair changes nothing in the record but its pin: a run of the same build that changes the role or the hub
    // is an install over it.
    plan.repair = plan.repair && record::keeps(&pre_text, &plan.record(&plan.pack.commit));
    if plan.repair {
        say("a repair run: nothing moves, and the record gets its pin");
    }
    ask_daemon(m, &mut plan, &new_sotd, &why)?;
    Ok(plan)
}

fn flag_word(flag: &RoleFlag) -> String {
    match flag {
        RoleFlag::Local => "local".into(),
        RoleFlag::BeOnly => "be-only".into(),
        RoleFlag::Remote(alias) => format!("backend {alias}"),
    }
}

fn role_words(r: Role) -> String {
    format!("daemon:{} frontend:{}", u8::from(r.daemon), u8::from(r.frontend))
}

/// The warning for a folder on a remote filesystem, where the daemon will not keep capsule records (Linux; the names are
/// GNU `stat -f -c %T`'s for `REMOTE_FS_TYPES` in `rust/platform/src/host/volume.rs`). A folder not made yet is judged by
/// the nearest one that is, the filesystem it will be made on; a `stat` that gives no answer is a step.
fn warn_remote_fs(m: &dyn Machine, label: &str, dir: &Path, found: &mut Found) {
    let on = dir.ancestors().find(|d| d.exists()).unwrap_or(dir);
    let fstype = observe::ask(m, Command::new("stat").args(["-f", "-c", "%T"]).arg(on), ASK_BOUND, |a| {
        Some(a.zero()?.first_line()).filter(|t| !t.is_empty()).ok_or_else(|| Why::Malformed("no filesystem type".into()))
    });
    fstype.read(
        |fstype| {
            if ["nfs", "nfs4", "cifs", "smb2", "fuse.sshfs", "9p"].contains(&fstype.as_str()) {
                say(&format!("WARNING: {label} ({}) is on a remote filesystem ({fstype}).", dir.display()));
                say("  Capsule session records cannot live on a remote filesystem (ADR 0043 decision 23). Set XDG_STATE_HOME to a");
                say("  local-disk directory in the environment sotd starts from -- for a systemd install: systemctl --user");
                say("  set-environment XDG_STATE_HOME=/path/on/local/disk, or export it in ~/.bashrc (which the unit sources) --");
                say("  then restart sotd. sotd requires that directory to be owned by you with mode 700 and does not back it up --");
                say("  pick a path that persists across reboots, not /tmp.");
            }
        },
        |why| found.step(format!("check that {label} ({}) is on a local disk, not a remote filesystem, which the daemon's records cannot use: {why}", dir.display())),
    );
}

/// A pre-0.6 hosts.toml (`default_host` at its top) is a parse error to the current grammar: it is moved aside, never
/// deleted, so a sync writes the current grammar.
fn heal_old_hosts(config: &Path) {
    let hosts = config.join("hosts.toml");
    let old = std::fs::read_to_string(&hosts).is_ok_and(|t| t.lines().any(|l| l.starts_with("default_host")));
    if !old {
        return;
    }
    let aside = config.join(format!("hosts.toml.v1-{}", record::now_utc().replace(['-', ':', 'T', 'Z'], "")));
    if std::fs::rename(&hosts, &aside).is_ok() {
        say(&format!("pre-0.6 hosts.toml moved aside to {}; the hub sync writes the current grammar on next launch (or run: sotd topology sync --hub <alias>)", aside.display()));
    }
}

/// The program of the active `sotd.service` for this user here (`is-active` is this host's own state, where the unit
/// file may be another host's on a shared home). An active unit whose program cannot be read is not "no running owner":
/// the ownership gate cannot be answered, and the run stops.
fn running_unit_program(m: &dyn Machine) -> Result<String, Stop> {
    let unit = observe::ask(m, Command::new("systemctl").args(["--user", "cat", "sotd.service"]), ASK_BOUND, |a| {
        role::unit_owner_path(&a.zero()?.out).ok_or_else(|| Why::Malformed("a unit that names no program".into()))
    });
    unit.or(|why| Stop::new(2, format!("sotd.service is active but its unit could not be read ({why}); nothing was changed")))
}

/// The glibc this box runs, from an exit-0 `ldd --version`'s first line.
fn glibc_version(m: &dyn Machine) -> Fact<(u32, u32)> {
    observe::ask(m, Command::new("ldd").arg("--version"), ASK_BOUND, |a| glibc_of(&a.zero()?.out).ok_or_else(|| Why::Malformed(format!("no glibc version ('{}')", a.first_line()))))
}

fn glibc_of(text: &str) -> Option<(u32, u32)> {
    let line = text.lines().next()?.trim_end().to_string();
    let last = line.rsplit(|c: char| !(c.is_ascii_digit() || c == '.')).next()?;
    let (a, b) = last.split_once('.')?;
    Some((a.parse().ok()?, b.split('.').next()?.parse().ok()?))
}

/// The role asked at the console, when nothing else names one (it works under `curl | bash`: the questions go to
/// `/dev/tty`). No console is a refusal.
fn ask_role(m: &dyn Machine, me: &str) -> Result<RoleFlag, Stop> {
    let env = m.env();
    let question = format!(
        "No declared topology names this machine ({me}) — where should Ship of Tools run?\n  1) all on this machine        (frontend + backend here)\n  2) frontend here, backend on another machine over SSH\n  3) backend only on this machine (headless server)\nChoose [1-3]: "
    );
    let Some(choice) = daemon::read_answer(&env, &question) else {
        return Err(Stop::new(2, "pick a role: --local | --backend <alias> | --be-only (no TTY for interactive setup, and no install here records one)"));
    };
    match choice.trim() {
        "1" => Ok(RoleFlag::Local),
        "3" => Ok(RoleFlag::BeOnly),
        "2" => {
            let alias = daemon::read_answer(&env, "SSH alias/hostname of the backend machine (key-based auth required): ").unwrap_or_default().trim().to_string();
            if alias.is_empty() {
                return Err(Stop::new(1, "backend host is required for the remote layout"));
            }
            let mut ssh = Command::new("ssh");
            ssh.args(sot_topology::ssh_bridge::SSH_OPTS).args(["-n", &alias, "true"]);
            let works = observe::ask(m, &mut ssh, sot_topology::ssh_bridge::SSH_LOGIN_BOUND, |a| Ok(a.exit == 0))
                .or(|why| Stop::new(1, format!("ssh to '{alias}' gave no answer ({why}: a network, sshd or remote login that stopped answering)")))?;
            if works {
                Ok(RoleFlag::Remote(alias))
            } else {
                Err(Stop::new(1, format!("key-based ssh to '{alias}' doesn't work (ssh-copy-id first, or fix ~/.ssh/config)")))
            }
        }
        other => Err(Stop::new(1, format!("no such choice: '{other}'"))),
    }
}

/// What the daemon question reads: the daemon's state before the run, whether its unit runs (this run restarts only
/// that), the person's `--restart-daemon`, the socket, and sotd's line when it could not tell the version.
pub(super) struct Question<'a> {
    pub before: State,
    pub unit_runs: bool,
    /// The role runs no daemon here and `sotd.service` is active: the retire stops it, whatever its version.
    pub retire_stops: bool,
    pub restart_flag: bool,
    pub socket: &'a str,
    pub why: &'a str,
}

/// The daemon question (scripts/CLAUDE.md, Installers: "A live daemon is asked before it ends"): an active unit on
/// another version is restarted by this run, so its live work is asked first, yes at a console or `--restart-daemon`;
/// a daemon the run cannot tell stops the run unless the unit can be restarted and the answer is yes; a daemon that
/// cannot say is judged by its own children. Changes nothing. The live work, and whether part of it is unknown.
pub(super) fn question(q: &Question, a: &Asks) -> Result<(daemon::Work, bool), Stop> {
    let (socket, why) = (q.socket, q.why);
    let agents = daemon::agents_note(q.before, why);
    let refuse_hint = format!("Restarting the daemon ends every live REPL and Pluto notebook (their variables are lost); {agents} Nothing under the install was changed. Ask whoever uses this computer, then re-run with --restart-daemon.");
    // The run ends the unit it restarts, or the one its role's retire stops.
    let ends_unit = q.unit_runs || q.retire_stops;
    let (acting, act) = if q.retire_stops { ("stopping", "stopping it would end") } else { ("restarting", "restarting it would end") };
    if q.before == State::Unknown {
        let kids = (a.kids)();
        let (repl, pluto) = kids.as_ref().map_or((false, false), |k| (k.contains(&"repl"), k.contains(&"pluto")));
        if ends_unit && q.restart_flag {
            (a.note)(&format!("note: the daemon on {socket} could not be told its version ({why}); restarting sotd.service ends any live REPL or Pluto notebook (--restart-daemon)"));
        } else if ends_unit && kids.as_ref().is_some_and(Vec::is_empty) {
            (a.note)(&format!("note: the daemon on {socket} could not be told its version ({why}); it has no REPL or Pluto child, so restarting sotd.service ends nothing live"));
        } else if ends_unit && (a.ask)(&daemon::ask_words(&[], repl, pluto, true)) {
            (a.note)(&format!("note: the daemon on {socket} could not be told its version ({why}); restarting sotd.service as answered"));
        } else {
            let hint = if ends_unit {
                "Restarting the daemon ends every live REPL and Pluto notebook (their variables are lost); ask whoever uses this computer, then re-run with --restart-daemon."
            } else {
                "Find why it does not answer, or stop it, then run this again."
            };
            println!("       Nothing under the install was changed. {hint}");
            return Err(Stop::new(2, format!("a daemon runs on {socket} and could not be told its version ({why})")));
        }
    }
    if !(matches!(q.before, State::Other | State::Held) || (q.retire_stops && q.before == State::Current)) {
        return Ok((daemon::Work::default(), false));
    }
    let will_restart = (q.unit_runs && q.before == State::Other) || q.retire_stops;
    let retires = will_restart && daemon::retires_agents(q.before, why);
    if retires {
        (a.note)(&format!("note: {agents}"));
    }
    let live = (a.live)();
    let ended = |lead: &str| daemon::work_words(&live.rows, live.pluto, lead);
    let mut unknown = false;
    match &live.unsure {
        None if live.rows.is_empty() && !live.pluto => {}
        None if !will_restart => {}
        None if q.restart_flag => (a.note)(&format!("{} (--restart-daemon)", ended(&format!("{acting} the daemon ends")))),
        None if (a.ask)(&daemon::ask_words(&live.rows, false, live.pluto, false)) => {
            (a.note)(&format!("{} (answered yes)", ended(&format!("{acting} the daemon ends"))));
        }
        None => {
            println!("       Restarting the daemon ends those REPLs and notebooks (their variables are lost); {agents} Nothing under the install was changed. Ask whoever uses this computer, then re-run with --restart-daemon.");
            let what = if q.retire_stops { format!("this run stops the daemon on {socket}, since this computer runs none in this role") } else { format!("the daemon on {socket} runs another version") };
            return Err(Stop::new(2, format!("{what}, and {}", ended(act))));
        }
        Some(reason) => {
            // The daemon could not say; its own children can: with none that is a REPL or the Pluto server, none of
            // that work is left to end.
            let kids = (a.kids)();
            let (repl, pluto) = kids.as_ref().map_or((false, false), |k| (k.contains(&"repl"), k.contains(&"pluto")));
            if kids.as_ref().is_some_and(Vec::is_empty) && live.rows.is_empty() && !live.pluto {
                if will_restart {
                    let ends = if retires { "no live REPL or Pluto notebook" } else { "nothing live" };
                    (a.note)(&format!("note: could not ask the daemon on {socket} in full ({reason}); it has no REPL or Pluto child, so restarting it ends {ends}"));
                }
            } else if !will_restart {
                unknown = true;
            } else if q.restart_flag {
                unknown = true;
                (a.note)(&format!("note: could not ask the daemon on {socket} in full ({reason}); restarting it ends any REPL or Pluto notebook that is live (--restart-daemon)"));
            } else if (a.ask)(&daemon::ask_words(&live.rows, repl, live.pluto || pluto, true)) {
                unknown = true;
                (a.note)(&format!("note: could not ask the daemon on {socket} in full ({reason}); restarting it as answered"));
            } else {
                println!("       {refuse_hint}");
                let rows = if live.rows.is_empty() { String::new() } else { format!(", and restarting it would end the REPLs of: {}", live.rows.join(", ")) };
                return Err(Stop::new(2, format!("could not ask the daemon on {socket} in full: it runs another version{rows}; {reason}")));
            }
        }
    }
    Ok((live, unknown))
}

/// Whether the run restarts this host's unit: it runs (`unit_runs`) another version, or one that cannot be told.
fn restarts(unit_runs: bool, before: State) -> bool {
    unit_runs && matches!(before, State::Other | State::Unknown)
}

/// Whether the run ends a daemon here: the restart, or the retire of an active unit. A repair is a run for which this
/// is false, so it stops and restarts nothing by construction.
fn ends_daemon(unit_runs: bool, retire_stops: bool, before: State) -> bool {
    restarts(unit_runs, before) || retire_stops
}

/// The daemon a run finds, asked of the target's sotd when the run may end one: its role runs a daemon here, or the
/// retire stops an active unit. One runs when the unit runs (the one this run restarts, or the active one the retire
/// stops, whichever install's it is) or the install's own daemon process does (`own_pids`), so one that cannot be told
/// is never read as none.
pub(super) fn discover(m: &dyn Machine, sotd: &Path, endpoint: &str, daemon_here: bool, retire_stops: bool, unit_runs: bool, own_pids: bool) -> (State, String) {
    if daemon_here || retire_stops {
        daemon::state(m, sotd, endpoint, unit_runs || retire_stops || own_pids)
    } else {
        (State::None, String::new())
    }
}

fn ask_daemon(m: &dyn Machine, plan: &mut Plan, new_sotd: &Path, why: &str) -> Result<(), Stop> {
    let endpoint = format!("unix:{}", plan.socket);
    let prefix = plan.prefix.clone();
    let env = m.env();
    let q = Question { before: plan.before, unit_runs: plan.unit_runs, retire_stops: plan.retire_stops, restart_flag: plan.opts.restart_daemon, socket: &plan.socket, why };
    let asks = Asks {
        live: &|| daemon::work(m, new_sotd, &endpoint),
        kids: &|| daemon::children(m, &prefix),
        ask: &|work| daemon::ask_restart(&env, work),
        note: &|t| say(t),
    };
    let (live, unknown) = question(&q, &asks)?;
    plan.live = live;
    plan.rows_unknown = unknown;
    Ok(())
}

/// The commit, the binaries and the checkout: the first writes under the prefix.
#[allow(clippy::too_many_lines, reason = "the layout in install.sh's order: the commit, the binaries, the checkout, the flip and the prune")]
fn lay_out(m: &dyn Machine, plan: &Plan, found: &mut Found) -> Result<(), Stop> {
    step("layout");
    let repo = plan.prefix.join("repo");
    let base = repo.join("base");
    let checkout = plan.checkout.clone();
    let current = repo.join("current");
    let tag = plan.pack.tag();
    let archive = plan.opts.archive.is_some();
    if observe::which(m, "git").is_none() {
        return Err(Stop::new(1, "git is required (the install includes a repo checkout)"));
    }
    if !base.is_dir() {
        say("creating base clone (blobless, no checkout)");
        let ok = m.show(cmd::git().args(["clone", "--filter=blob:none", "--no-checkout", &super::origin(&m.env())]).arg(&base)) == Some(0);
        if !ok {
            return Err(Stop::new(1, "base clone failed"));
        }
    }
    // --force on the tag fetch: a release tag moved upstream would otherwise abort every later fetch.
    let commit_ref = format!("{}^{{commit}}", plan.pack.commit);
    if m.show(cmd::git_in(&base).args(["fetch", "--tags", "--force", "origin"])) != Some(0) {
        if archive && git_value(m, &base, &["rev-parse", "--verify", &commit_ref], String::new).is_ok() {
            say(&format!("note: could not fetch origin; {} already holds {}", base.display(), plan.pack.short()));
        } else {
            return Err(Stop::new(1, "fetch failed"));
        }
    }
    let want = if archive {
        git_value(m, &base, &["rev-parse", "--verify", &commit_ref], || format!("commit {} is not in origin after fetch — push the line first", plan.pack.commit))?
    } else {
        git_value(m, &base, &["rev-parse", &format!("refs/tags/{tag}^{{commit}}")], || format!("tag {tag} not present after fetch"))?
    };
    if want != plan.pack.commit {
        return Err(Stop::new(1, format!("{tag} is at {want} in origin, not the pack's {}: refusing", plan.pack.commit)));
    }

    for d in ["bin", "updates", "repo"] {
        std::fs::create_dir_all(plan.prefix.join(d)).map_err(|e| Stop::new(1, format!("creating {}: {e}", plan.prefix.join(d).display())))?;
    }
    let bin = plan.prefix.join("bin");
    for b in ["sot", "sotd", "sot-capsule"] {
        let src = plan.pack.exe(b);
        if b == "sot-capsule" && !src.is_file() {
            continue;
        }
        let dst = bin.join(b);
        // The .prev copy is the rollback: a binary already equal to the target's (a repeat) is not the previous one.
        if dst.is_file() && std::fs::read(&dst).ok() != std::fs::read(&src).ok() {
            copy_whole(&dst, &bin.join(format!("{b}.prev")), None).map_err(|e| Stop::new(1, format!("backing up {} failed: {e}", dst.display())))?;
        }
        copy_whole(&src, &dst, Some(0o755)).map_err(|e| Stop::new(1, format!("installing {} failed: {e}", dst.display())))?;
        if plan.macos {
            let _ = m.act(Command::new("xattr").args(["-d", "com.apple.quarantine"]).arg(&dst));
        }
    }
    let packed_apply = plan.pack.bin.join("sot-apply");
    if packed_apply.is_file() {
        copy_whole(&packed_apply, &bin.join("sot-apply"), Some(0o755)).map_err(|e| Stop::new(1, format!("installing {}/sot-apply failed: {e}", bin.display())))?;
    }
    // A run is a new transaction: rollback state of an earlier apply must not pair with these .prev binaries, and a
    // pointer armed over the install this one replaces would apply onto it. Only this target's: a shared home serves others.
    let updates = plan.prefix.join("updates");
    for name in [format!("last-good-{}.json", plan.target), format!("just-applied-{}", plan.target), format!("pending-{}.json", plan.target)] {
        let _ = std::fs::remove_file(updates.join(name));
    }
    step("layout");
    if checkout.is_dir() {
        if tree_dirty(m, &checkout).or(|why| Stop::new(1, format!("could not read the checkout at {}: {why}", checkout.display())))? {
            return Err(Stop::new(1, format!("the checkout at {} has local changes — commit/stash/revert, then re-run (updates refuse to move a dirty tree)", checkout.display())));
        }
    } else {
        say(&format!("adding version worktree {tag}"));
        if m.show(cmd::git_in(&base).args(["worktree", "add", "--detach"]).arg(&checkout).arg(&want)) != Some(0) {
            return Err(Stop::new(1, "worktree add failed; inspect this version path and its existing registration with git worktree list; registrations for unavailable paths are kept"));
        }
    }
    let have = git_value(m, &checkout, &["rev-parse", "HEAD"], || format!("cannot read HEAD of {}", checkout.display()))?;
    if have != want {
        return Err(Stop::new(1, format!("checkout HEAD ({have}) != {tag} commit ({want}) — refusing")));
    }
    if !checkout.join("scripts/lib/sot-daemon.sh").is_file() {
        return Err(Stop::new(1, format!("cannot read {}/scripts/lib/sot-daemon.sh", checkout.display())));
    }
    let prev = flip_current(m, &repo, &current, &checkout)?;
    let compat = plan.prefix.join("julia");
    let _ = std::fs::create_dir_all(&compat);
    let _ = replace_link(&checkout, &compat.join("current"));
    if !bin.join("sot-apply").is_file() {
        let _ = copy_whole(&checkout.join("scripts/sot-apply.sh"), &bin.join("sot-apply"), Some(0o755));
    }
    prune_versions(m, &repo, &base, &tag, prev.as_deref(), found);
    Ok(())
}

/// Copy `src` to `dst` whole: to a name of this process's own beside it, its mode set, then renamed over it.
#[allow(clippy::disallowed_methods, reason = "sotd install copies the pack's files in the foreground and starts no program while a copy is open; no test copies a program through it")]
fn copy_whole(src: &Path, dst: &Path, mode: Option<u32>) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let tmp = dst.with_file_name(format!("{}.new.{}", dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), std::process::id()));
    let done = std::fs::copy(src, &tmp)
        .and_then(|_| match mode {
            Some(m) => std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(m)),
            None => Ok(()),
        })
        .and_then(|()| std::fs::rename(&tmp, dst));
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

/// Point the symlink `link` at `target`, replacing what is there (`ln -sfn`).
fn replace_link(target: &Path, link: &Path) -> std::io::Result<()> {
    let tmp = link.with_file_name(format!(".{}.new.{}", link.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    std::fs::rename(&tmp, link).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Flip `repo/current` to the checkout; the version folder it named before, if any. A pre-versioned clone at
/// `repo/current` (a plain folder) is moved aside whole, never deleted, and refused when it has local changes.
fn flip_current(m: &dyn Machine, repo: &Path, current: &Path, checkout: &Path) -> Result<Option<String>, Stop> {
    let mut prev = None;
    if let Ok(target) = std::fs::read_link(current) {
        prev = target.file_name().map(|n| n.to_string_lossy().into_owned());
    } else if current.is_dir() {
        git_value(m, current, &["rev-parse", "--git-dir"], || format!("repo/current exists but is not a readable git checkout — refusing to migrate (inspect {})", current.display()))?;
        if tree_dirty(m, current).or(|why| Stop::new(1, format!("could not read the old-layout checkout at {}: {why}", current.display())))? {
            return Err(Stop::new(1, format!("the old-layout checkout at {} has local changes — commit/stash/revert, then re-run", current.display())));
        }
        say("migrating pre-versioned layout (old clone preserved at repo/current.pre-versioned)");
        let aside = repo.join("current.pre-versioned");
        let _ = std::fs::remove_dir_all(&aside);
        std::fs::rename(current, &aside).map_err(|e| Stop::new(1, format!("moving {} aside: {e}", current.display())))?;
    }
    replace_link(checkout, current).map_err(|e| Stop::new(1, format!("flipping {}: {e}", current.display())))?;
    Ok(prev)
}

/// Keep the target and the previously active version folder; prune the rest, except a folder with local changes,
/// which is named and kept, and one git cannot read, which is kept and named in the verdict. A run that found
/// `repo/current` at the target already prunes nothing.
fn prune_versions(m: &dyn Machine, repo: &Path, base: &Path, tag: &str, prev: Option<&str>, found: &mut Found) {
    let Ok(entries) = std::fs::read_dir(repo.join("versions")) else { return };
    for v in entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let name = v.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name == tag || Some(name.as_str()) == prev || prev == Some(tag) {
            continue;
        }
        let status = observe::ask(m, cmd::git_in(&v).args(["status", "--porcelain"]), ASK_BOUND, |a| Ok(a.zero()?.out.trim().to_string()));
        let Some(dirty) = status.read(Some, |why| {
            say(&format!("keeping old version dir {name}: git could not read it ({why})"));
            found.checked(format!("kept old version folder {name}: {why}"));
            None
        }) else {
            continue;
        };
        if dirty.is_empty() {
            say(&format!("pruning old version dir {name}"));
            if !m.act(cmd::git_in(base).args(["worktree", "remove"]).arg(&v)).ok() {
                say(&format!("keeping old version dir {name}: git could not remove its worktree; inspect that exact path before removing it"));
            }
        } else {
            say(&format!("keeping old version dir {name}: it has local changes ({}); it stays until it is clean or you remove it", dirty.lines().next().unwrap_or("")));
        }
    }
}

/// Julia (installed through juliaup when none at the channel is on PATH), the floor check, comm, and a daemon role's
/// environments. The channel the steps take, when one is selected.
fn julia_and_comm(m: &dyn Machine, plan: &Plan, found: &mut Found) -> Result<(), Stop> {
    step("julia");
    let juliaup_bin = plan.home.join(".juliaup/bin");
    if juliaup_bin.join("juliaup").is_file() {
        m.prepend_path(&juliaup_bin);
    }
    // Any exit is an answer (0: at 1.12 or later); a julia that gave none stops the run.
    let fresh_enough = observe::which(m, "julia").is_some()
        && observe::ask(m, Command::new("julia").args(["-e", "exit(VERSION >= v\"1.12\" ? 0 : 1)"]), ASK_BOUND, |a| Ok(a.exit == 0))
            .or(|why| Stop::new(1, format!("julia gave no answer to its version check ({why})")))?;
    if !fresh_enough {
        if observe::on_path(m, "juliaup") {
            say(&format!("installing Julia channel {} with juliaup", julia::CHANNEL));
            let _ = m.act(Command::new("juliaup").args(["add", julia::CHANNEL]));
        } else {
            say(&format!("installing Julia (juliaup, channel {})", julia::CHANNEL));
            let script = format!("curl {} -fsSL https://install.julialang.org | sh -s -- --yes --default-channel \"$1\"", cmd::CURL_BOUND.join(" "));
            let _ = m.show(Command::new("sh").args(["-c", &script, "sh", julia::CHANNEL]));
        }
        m.prepend_path(&juliaup_bin);
    }
    if observe::which(m, "julia").is_none() {
        return Err(Stop::new(1, "Julia install failed; julia is still not on PATH"));
    }
    // The channel is chosen once, by a probe, never by running a step and rerunning it on failure.
    let selected = observe::ask(m, Command::new("julia").args([&format!("+{}", julia::CHANNEL), "-e", "exit(0)"]), ASK_BOUND, |a| Ok(a.exit == 0))
        .or(|why| Stop::new(1, format!("julia +{} gave no answer to its channel probe ({why})", julia::CHANNEL)))?
        .then(|| julia::CHANNEL.to_string());
    let checkout = plan.checkout.clone();
    let facts = julia::facts(m, &checkout, selected.as_deref()).map_err(|why| Stop::new(1, why))?;
    match julia::judge(&facts) {
        julia::Judged::Ok => {}
        julia::Judged::Step(line) => found.step(line),
        julia::Judged::Stop(line) => return Err(Stop::new(1, line)),
    }

    step("comm");
    say("installing agent comm resources (sot-comm, Claude/Codex skills)");
    if !julia::update_comm(m, &checkout, selected.as_deref(), &plan.scratch) {
        return Err(Stop::new(1, "ShipTools.update_comm() failed"));
    }
    if plan.role.daemon {
        julia::instantiate(m, &checkout, selected.as_deref(), &plan.scratch, plan.repair, &|t| say(t), found);
        let mathjax = checkout.join("rust/backend/sidecars/mathjax");
        if observe::on_path(m, "npm") {
            say("installing MathJax sidecar deps (npm ci)");
            if m.show(Command::new("npm").args(["ci", "--silent"]).current_dir(&mathjax)) != Some(0) {
                say("WARN: npm ci failed in sidecars/mathjax — math rendering unavailable until you run it manually");
            }
        } else {
            say("WARN: node/npm not found — math rendering in markdown previews needs it.");
            say(&format!("      Install node, then run: (cd {} && npm ci)", mathjax.display()));
        }
    }
    Ok(())
}

/// The depot heal, the retire of a daemon this host no longer runs, and the folder-trust declaration.
fn configure(m: &dyn Machine, plan: &Plan, found: &mut Found) -> Result<(), Stop> {
    step("config");
    // An install from before 2026-09-02 wrote a drop-in carrying the installing shell's Julia depot into the unit; no
    // depot is ever set, so it goes on every run.
    let depot = plan.units().join("sotd.service.d/depot.conf");
    if depot.is_file() && std::fs::remove_file(&depot).is_ok() && observe::on_path(m, "systemctl") {
        let _ = systemctl(m, &["daemon-reload"]);
    }
    if !plan.role.daemon {
        let kernel_host = sot_platform::host::state_dir::kernel_host_name().unwrap_or_default();
        retire_local_service(&plan.units_ctx(m), plan.topo_declared, &kernel_host, found)?;
    }
    for line in trust_report(m, &plan.sotd(), &plan.home) {
        say(&line);
    }
    Ok(())
}

/// Folder trust, declared by the agents' own offline command (`sotd trust declare <home>`): the lines that say what came
/// of it. A failure is said, with what the command said, and never described as a declaration.
pub(super) fn trust_report(m: &dyn Machine, sotd: &Path, home: &Path) -> Vec<String> {
    let ran = m.act(Command::new(sotd).args(["trust", "declare"]).arg(home));
    match (ran.code, ran.out.trim()) {
        (Some(0), "Declared") => vec!["folder trust declared".into()],
        (Some(0), "Kept") => vec!["folder trust kept".into()],
        (Some(0), _) => vec!["WARN: folder trust not declared - unexpected command outcome".into()],
        (code, _) => {
            let mut lines = vec![format!("WARN: folder trust not declared (exit {})", code.map_or("none".into(), |c| c.to_string()))];
            let said = format!("{}{}", ran.out, ran.err);
            lines.extend(said.lines().filter(|l| !l.trim().is_empty()).map(String::from));
            lines
        }
    }
}

/// The programs and folders the unit's wiring touches: the machine that runs them, the systemd user folder under `home`,
/// the prefix whose `sotd` pins it, the checkout whose library renders it, and `systemctl` and `loginctl` (stand-ins in
/// a test).
pub(super) struct Units<'a> {
    pub m: &'a dyn Machine,
    pub home: &'a Path,
    pub prefix: &'a Path,
    pub checkout: &'a Path,
    pub systemctl: &'a str,
    pub loginctl: &'a str,
}

impl Units<'_> {
    fn dir(&self) -> PathBuf {
        self.home.join(".config/systemd/user")
    }
    fn systemctl(&self, args: &[&str]) -> cmd::Ran {
        self.m.act(Command::new(self.systemctl).arg("--user").args(args).env("HOME", self.home))
    }
    fn has_systemctl(&self) -> bool {
        Path::new(self.systemctl).is_file() || observe::on_path(self.m, self.systemctl)
    }
    /// One function of the checkout's library, with this home.
    fn lib(&self, func: &str, args: &[&str]) -> cmd::Ran {
        self.m.act(lib_command(self.checkout, func, args).env("HOME", self.home))
    }
    /// Pin the unit to the hosts that run sotd (`sotd topology pin`, `--claim` for this host with no hosts.toml); why
    /// not, when it could not.
    fn pin(&self, claim: bool) -> Result<(), String> {
        let dir = self.dir().to_string_lossy().into_owned();
        let ran = self.m.act(Command::new(self.prefix.join("bin/sotd")).args(pin_args(&dir, claim)).env("HOME", self.home));
        if ran.ok() { Ok(()) } else { Err(Some(ran.last_said()).filter(|s| !s.is_empty()).unwrap_or_else(|| "sotd topology pin failed".into())) }
    }
}

/// `sotd`'s arguments that pin the unit in `dir` (`--claim`: to this host, with no hosts.toml): the install's pin and the
/// command the retire tells a person to run are this one list.
fn pin_args(dir: &str, claim: bool) -> Vec<&str> {
    let mut args = vec!["topology", "pin", "--dir", dir];
    if claim {
        args.push("--claim");
    }
    args
}

/// A role with no daemon here must not leave an earlier local daemon running. The unit and its boot drop-in may serve
/// another host of a shared home, so only this host's run is stopped and the shared unit is never disabled; under a
/// declared topology the unit's host pin keeps it from starting here, and with no hosts.toml a pin that names this host
/// alone lets the drop-in go. `kernel_host` is the kernel's name for this host, which the pin names.
pub(super) fn retire_local_service(u: &Units, declared: bool, kernel_host: &str, found: &mut Found) -> Result<(), Stop> {
    let ud = u.dir().to_string_lossy().into_owned();
    if !u.has_systemctl() || !u.dir().join("sotd.service").is_file() {
        return Ok(());
    }
    if declared {
        // The pin before any reload: a unit left unpinned would start on every host of a shared home.
        u.pin(false).map_err(|why| Stop::new(1, format!("could not pin sotd.service to the hosts that run sotd ({why}); it was not stopped or reloaded: fix hosts.toml (sotd topology status names the line) and run this again")))?;
        if !u.lib("sot_boot_keep", &[&ud]).ok() {
            say("WARNING: could not move sotd.service's boot start onto its drop-in");
        }
        let _ = u.systemctl(&["stop", "sotd.service"]);
        let _ = u.systemctl(&["daemon-reload"]);
        say("stopped this host's sotd.service; the declared topology runs no daemon here");
    } else if role::pin_names_only(&u.dir(), kernel_host) {
        let _ = u.systemctl(&["stop", "sotd.service"]);
        let _ = u.lib("sot_boot_unwants", &[&ud]);
        let _ = u.systemctl(&["daemon-reload"]);
        say("stopped sotd.service and removed its boot start, whose host pin names this host alone");
    } else {
        if !u.lib("sot_boot_keep", &[&ud]).ok() {
            say("WARNING: could not move sotd.service's boot start onto its drop-in");
        }
        let _ = u.systemctl(&["stop", "sotd.service"]);
        boot_wired(u.m, u.checkout, &ud).read(
            |wired| {
                if wired {
                    let claim = pin_args("~/.config/systemd/user", true).join(" ");
                    say(&format!("Stopped this computer's sotd. Boot start follows hosts.toml and the unit's host pin, not enable or disable, which change no file for sotd.service, so a hand disable no longer turns it off; to move it, change hosts.toml, or with none run sotd {claim} on the host that should run it. If no other computer shares this home folder, turn boot start off with: rm -f ~/.config/systemd/user/default.target.d/sotd.conf (write that file again, with the two lines [Unit] and Wants=sotd.service, to turn it back on)"));
                } else {
                    say("Stopped this computer's sotd. Its boot start is already off: sotd.service has no boot drop-in, and enable and disable change no file for it; to turn it on, write ~/.config/systemd/user/default.target.d/sotd.conf with the two lines [Unit] and Wants=sotd.service");
                }
            },
            |why| {
                say("Stopped this computer's sotd.");
                found.step(boot_unknown(&why));
            },
        );
    }
    Ok(())
}

/// The unit, the launcher and the restart; whether the run restarted the daemon.
fn wire(m: &dyn Machine, plan: &Plan) -> Result<bool, Stop> {
    step("service");
    if plan.role.daemon && plan.no_service && !plan.macos {
        say("skipping systemd unit (--no-service) — supervise sotd yourself, e.g.:");
        say(&format!("  systemd-run --user --unit=sotd-canary -p Restart=always {} --project-root $HOME --label sot", plan.sotd().display()));
    }
    if plan.macos && plan.role.daemon {
        say("macOS: no service manager wiring yet — the sot-launch wrapper starts sotd on demand");
        if !plan.role.frontend {
            say(&format!("  be-only: start it with  {} --project-root ~ --label sot", plan.sotd().display()));
        }
    }
    if plan.unit_managed() {
        enable_local_service(&plan.units_ctx(m), &plan.pack.bin.join("sotd.service"), &plan.socket)?;
    }
    step("launcher");
    if plan.role.frontend {
        wire_launcher(m, plan)?;
    }
    step("restart");
    let restart = restarts(plan.unit_runs, plan.before);
    if restart {
        say("restarting sotd to pick up the new version");
        let _ = systemctl(m, &["try-restart", "sotd.service"]);
    }
    Ok(restart)
}

/// Render, pin, wire and start the unit: pinned to the hosts that run sotd (with no hosts.toml, to this one) before any
/// manager reloads it; boot start wired for a unit this run finds new, and over an existing one only where it was; no
/// `enable`, which on a shared home would link the unit for every host.
pub(super) fn enable_local_service(u: &Units, template: &Path, socket: &str) -> Result<(), Stop> {
    let units = u.dir();
    let ud = units.to_string_lossy().into_owned();
    std::fs::create_dir_all(&units).map_err(|e| Stop::new(1, format!("creating {ud}: {e}")))?;
    // v0.6.0 deleted the tmux runtime: the keeper unit earlier installs enabled goes.
    let tmux = units.join("sot-tmux.service");
    if tmux.is_file() {
        let _ = u.systemctl(&["disable", "--now", "sot-tmux.service"]);
        let _ = std::fs::remove_file(&tmux);
    }
    let unit = units.join("sotd.service");
    let fresh = !unit.is_file();
    // The pin first: unpinned, every host of a shared home would start the unit, so a pin that fails writes no unit,
    // and nothing is reloaded or started.
    let before = role::pin_hosts(&units);
    u.pin(true).map_err(|why| Stop::new(1, format!("could not pin sotd.service to the hosts that run sotd ({why}); the unit was not written, reloaded or started: fix hosts.toml (sotd topology status names the line) and run this again")))?;
    let after = role::pin_hosts(&units);
    if !u.lib("render_sotd_unit", &[&u.prefix.to_string_lossy(), &template.to_string_lossy(), &unit.to_string_lossy()]).ok() {
        return Err(Stop::new(1, "could not write the sotd unit"));
    }
    for h in before.iter().filter(|h| !after.contains(h)) {
        say(&format!("The daemon moved here from {h}: {h}'s sotd will not start at its next login."));
    }
    if !u.lib(if fresh { "sot_boot_wants" } else { "sot_boot_keep" }, &[&ud]).ok() {
        return Err(Stop::new(1, "could not write the sotd boot drop-in"));
    }
    for args in [&["daemon-reload"][..], &["start", "sotd.service"][..]] {
        let ran = u.systemctl(args);
        if !ran.ok() {
            return Err(Stop::new(1, format!("systemctl --user {} failed: {}", args.join(" "), ran.last_said())));
        }
    }
    // Linger by the numeric uid, which loginctl takes: its no-argument form needs a logind session an ssh or CI run may
    // lack, and no user name is asked of the machine.
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let _ = u.m.act(Command::new(u.loginctl).args(["enable-linger", &uid.to_string()]));
    say(&format!("started sotd.service (socket {socket}); the check below reads what it runs"));
    Ok(())
}

/// The window's launcher: the all-in-one wrapper (this computer's daemon), or the one that dials the topology plan when
/// the run names a backend or the plan lists another daemon host; then the desktop entry or the app bundle.
fn wire_launcher(m: &dyn Machine, plan: &Plan) -> Result<(), Stop> {
    let local_bin = plan.home.join(".local/bin");
    std::fs::create_dir_all(&local_bin).map_err(|e| Stop::new(1, format!("creating {}: {e}", local_bin.display())))?;
    let others = plan.others;
    if others {
        say("the topology plan lists other daemon hosts: the launcher dials every host of the plan, and this computer's own daemon beside them");
    }
    let wrapper = local_bin.join("sot-launch");
    let (prefix, dest) = (plan.prefix.to_string_lossy().into_owned(), wrapper.to_string_lossy().into_owned());
    let rendered = if plan.be_alias.is_some() || others {
        lib(m, &plan.checkout, "render_remote_launch", &[&prefix, plan.be_alias.as_deref().unwrap_or(""), &dest])
    } else {
        lib(m, &plan.checkout, "render_sot_launch", &[&prefix, plan.target, &dest])
    };
    if !rendered.ok() {
        print!("{}", rendered.err);
        return Err(Stop::new(1, format!("writing {dest} failed")));
    }
    if plan.macos { app_bundle(m, plan, &dest) } else { desktop_entry(m, plan, &dest) }
}

/// A minimal app bundle, so the window starts from Launchpad, Spotlight and the Dock; its icon made from the checkout's
/// logo with sips and iconutil (both ship with macOS). A bundle made here carries no quarantine attribute.
fn app_bundle(m: &dyn Machine, plan: &Plan, wrapper: &str) -> Result<(), Stop> {
    let app = plan.home.join("Applications/Ship of Tools.app/Contents");
    for d in ["MacOS", "Resources"] {
        let _ = std::fs::create_dir_all(app.join(d));
    }
    let plist = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n    <key>CFBundlePackageType</key><string>APPL</string>\n    <key>CFBundleName</key><string>Ship of Tools</string>\n    <key>CFBundleIdentifier</key><string>dev.ship-of-tools.sot</string>\n    <key>CFBundleExecutable</key><string>sot-launch</string>\n    <key>CFBundleIconFile</key><string>sot</string>\n    <key>NSHighResolutionCapable</key><true/>\n</dict></plist>\n";
    let write = |name: &str, text: &str, mode: u32| -> Result<(), Stop> {
        let src = plan.scratch.join(name.replace('/', "-"));
        std::fs::write(&src, text).and_then(|()| copy_whole(&src, &app.join(name), Some(mode))).map_err(|e| Stop::new(1, format!("writing {} failed: {e}", app.join(name).display())))
    };
    write("Info.plist", plist, 0o644)?;
    write("MacOS/sot-launch", &format!("#!/usr/bin/env bash\nexec \"{wrapper}\"\n"), 0o755)?;
    let logo = plan.checkout.clone().join("logo.png");
    if logo.is_file() && observe::on_path(m, "sips") && observe::on_path(m, "iconutil") {
        let iconset = plan.scratch.join("sot.iconset");
        let _ = std::fs::create_dir_all(&iconset);
        for sz in [16, 32, 128, 256, 512] {
            for (scale, suffix) in [(1, ""), (2, "@2x")] {
                let px = (sz * scale).to_string();
                let _ = m.act(Command::new("sips").args(["-z", &px, &px]).arg(&logo).arg("--out").arg(iconset.join(format!("icon_{sz}x{sz}{suffix}.png"))));
            }
        }
        let made = m.act(Command::new("iconutil").args(["-c", "icns"]).arg(&iconset).arg("-o").arg(app.join("Resources/sot.icns"))).ok();
        say(if made { "app icon generated from the checkout logo" } else { "icon generation failed (cosmetic) — bundle works without it" });
    }
    say("FE launcher: sot-launch + 'Ship of Tools' in ~/Applications (Launchpad/Spotlight)");
    Ok(())
}

/// The desktop entry, with the logo installed into the hicolor theme and kept under the prefix as the fallback icon
/// path (the prefix does not move; the checkout does).
fn desktop_entry(m: &dyn Machine, plan: &Plan, wrapper: &str) -> Result<(), Stop> {
    let apps = plan.home.join(".local/share/applications");
    let _ = std::fs::create_dir_all(&apps);
    let logo = plan.checkout.clone().join("logo.png");
    let fallback = plan.prefix.join("logo.png").to_string_lossy().into_owned();
    let mut icon = fallback.clone();
    if logo.is_file() {
        let _ = copy_whole(&logo, &plan.prefix.join("logo.png"), None);
        let theme = plan.home.join(".local/share/icons/hicolor");
        let dir = theme.join("256x256/apps");
        if std::fs::create_dir_all(&dir).is_ok() && copy_whole(&logo, &dir.join("ship-of-tools.png"), None).is_ok() {
            icon = "ship-of-tools".into();
            if observe::on_path(m, "gtk-update-icon-cache") {
                let _ = m.act(Command::new("gtk-update-icon-cache").args(["-q", "-t", "-f"]).arg(&theme));
            }
        }
    }
    let entry = format!("[Desktop Entry]\nType=Application\nName=Ship of Tools\nComment=Agentic Julia development environment\nExec={wrapper}\nIcon={icon}\nTerminal=false\nCategories=Development;IDE;\nStartupWMClass=sot\n");
    record::write_whole(&apps.join("ship-of-tools.desktop"), &entry).map_err(|e| Stop::new(1, format!("writing the desktop entry failed: {e}")))?;
    if observe::on_path(m, "update-desktop-database") {
        let _ = m.act(Command::new("update-desktop-database").arg(&apps));
    }
    say(&format!("FE launcher: sot-launch (+ desktop entry, icon {icon})"));
    Ok(())
}

/// install.json, last, whole.
fn write_record(m: &dyn Machine, plan: &Plan) -> Result<(), Stop> {
    step("record");
    let commit = git_value(m, &plan.checkout, &["rev-parse", "HEAD"], || format!("cannot read HEAD of {}", plan.checkout.display()))?;
    let r = plan.record(if commit.is_empty() { "unknown" } else { &commit });
    let path = plan.prefix.join("install.json");
    record::write_whole(&path, &record::text_for(&record::read(&path), &r, plan.repair)).map_err(|e| Stop::new(1, format!("writing {}: {e}", path.display())))?;
    if plan.named {
        verdict::set_tail(verdict::pin_tail(&plan.pack.version));
    }
    say(&format!("wrote {} (schema 1, daemon={} frontend={}, service={})", path.display(), u8::from(r.daemon), u8::from(r.frontend), r.service));
    say(&format!("Ship of Tools {} installed (daemon={} frontend={}).", plan.pack.tag(), u8::from(r.daemon), u8::from(r.frontend)));
    if let Some(alias) = &plan.be_alias {
        say(&format!("reminder: key-based ssh to '{alias}' is required (ssh {alias} true)"));
    }
    Ok(())
}

/// What the run left, read from the disk and the processes: a failed check is FAIL, a step only a person takes is ONE
/// STEP LEFT. A check whose question gives no answer is FAIL with the reason, never PASS.
#[allow(clippy::too_many_lines, reason = "the checks in install.sh's order, one block each")]
fn check(m: &dyn Machine, plan: &Plan, restarted: bool, found: &mut Found) {
    step("check");
    let (version, prefix) = (&plan.pack.version, &plan.prefix);
    let checkout = plan.checkout.clone();
    let head = |dir: &Path| observe::ask(m, cmd::git_in(dir).args(["rev-parse", "HEAD"]), ASK_BOUND, |a| Ok(a.zero()?.first_line()));
    let commit = head(&checkout).read(
        |commit| commit,
        |why| {
            found.fail("checkout", format!("cannot read the commit of {}: {why}", checkout.display()));
            String::new()
        },
    );
    let short = commit.get(..9).unwrap_or("").to_string();
    found.pass = format!("Ship of Tools {version} at {short} on this computer");
    if commit.is_empty() {
        found.fail("checkout", format!("cannot read the commit of {}", checkout.display()));
    }
    for b in ["sotd", "sot"] {
        if b == "sot" && !plan.role.frontend {
            continue;
        }
        let exe = prefix.join("bin").join(b);
        if let Err(said) = super::pack::says(super::pack::version_of(m, &exe), b, version, &commit) {
            found.fail("binaries", format!("{} {said}, not {version} at {short}", exe.display()));
        }
    }
    let text = record::read(&prefix.join("install.json"));
    if record::string(&text, "version").as_deref() != Some(version) || record::string(&text, "commit").as_deref() != Some(&commit) {
        found.fail("record", format!("{}/install.json does not name {version} at {short}", prefix.display()));
    }
    let current = prefix.join("repo/current");
    if std::fs::read_link(&current).ok() == Some(checkout.clone()) {
        let at = head(&current).read(Some, |why| {
            found.fail("checkout", format!("cannot read the commit of {}: {why}", current.display()));
            None
        });
        if at.is_some_and(|at| at != commit) {
            found.fail("checkout", format!("{} does not resolve to {} at {short}", current.display(), checkout.display()));
        }
    } else {
        found.fail("checkout", format!("{} does not resolve to {} at {short}", current.display(), checkout.display()));
    }
    let pointer = prefix.join(format!("updates/pending-{}.json", plan.target));
    if pointer.exists() {
        found.fail("pointer", format!("an update pointer is armed again ({}): another daemon armed it after this run dropped it; remove it and run this again", pointer.display()));
    }
    let comm = m.env().get_nonempty("SOT_COMM_HOME").map_or_else(|| plan.home.join(".sot-comm"), PathBuf::from);
    let stamp = std::fs::read_to_string(comm.join("VERSION")).ok().and_then(|t| t.lines().next().map(|l| l.trim().trim_end_matches("-dirty").to_string())).unwrap_or_default();
    if stamp.is_empty() {
        found.fail("comm", format!("{} records no version, not {short}", comm.display()));
    } else if !commit.starts_with(&stamp) {
        found.fail("comm", format!("{} is at '{stamp}', not {short}", comm.display()));
    }
    if plan.role.daemon {
        for (env, _) in julia::ENVS {
            if !checkout.join("julia").join(env).join("Manifest.toml").is_file() {
                found.fail("julia", format!("julia/{env} has no Manifest.toml: its environment is not instantiated"));
            }
        }
    }
    let after = daemon_after(m, plan, restarted, found);
    let changed = plan.pre_version != *version || plan.pre_commit != commit;
    let left = matches!(after, State::Held | State::Other);
    let live = &plan.live;
    let mut window_step = false;
    if plan.windows_before && changed {
        let note = if left { daemon::rows_note(&live.rows, live.pluto, plan.rows_unknown) } else { String::new() };
        let old = if plan.pre_version.is_empty() { "the old version" } else { &plan.pre_version };
        found.step(format!("quit Ship of Tools and start it again: its window still runs {old}{}", if note.is_empty() { String::new() } else { format!(" ({note})") }));
        window_step = true;
    }
    if left && !window_step {
        let skew = "still runs another version, beside the new repo/current and bin/sot-capsule";
        let socket = &plan.socket;
        let mut text = if plan.role.frontend && (after != State::Held || plan.windows_before) {
            format!("start Ship of Tools again (the daemon on {socket} {skew})")
        } else if plan.unit_managed() {
            if after == State::Held {
                format!("run systemctl --user restart sotd.service once the window that holds the daemon on {socket} is closed (it {skew})")
            } else {
                format!("run systemctl --user restart sotd.service (the daemon on {socket} {skew})")
            }
        } else {
            format!("restart the sotd on {socket} by hand (it {skew})")
        };
        let note = daemon::rows_note(&live.rows, live.pluto, plan.rows_unknown);
        if !note.is_empty() {
            text = format!("{text}; {note}");
        }
        found.step(text);
    }
    if let Some(alias) = plan.be_alias.as_ref().filter(|_| !plan.topo_named) {
        let hub = if plan.hub.is_empty() { alias.clone() } else { plan.hub.clone() };
        let sync = sotd_at(m, &plan.sotd(), &["topology", "sync", "--hub", &hub]);
        let said = format!("{}{}", sync.out, sync.err);
        if sync.ok() {
        } else if said.contains("does not list this box") {
            found.fail("enroll", format!("the hosts.toml on '{hub}' does not list this computer ('{}'), so the window would open with no hosts: on '{hub}' run  sotd topology set add {} --frontend  (the sotd of its install), then run this installer again", plan.me, plan.me));
        } else {
            let readable = observe::ask(m, Command::new(plan.sotd()).args(["topology", "plan", "--self", &plan.me]), ASK_BOUND, |a| Ok(a.exit == 0));
            let readable = readable.read(Some, |why| {
                found.fail("enroll", format!("could not read the topology plan ({why})"));
                None
            });
            if readable == Some(false) {
                found.fail("enroll", format!("could not read the hosts.toml on '{hub}': {}", sync.last_said()));
            }
        }
    }
    if found.fail.is_none() && plan.unit_managed() {
        boot_start_note(m, &plan.checkout, &plan.units().to_string_lossy()).read(|line| println!("{line}"), |why| found.step(boot_unknown(&why)));
    }
}

/// The daemon after the run. A unit this run restarted or started must answer as the target within the bound; one it
/// left alone may be held by a window on its old version; any other state of an active unit is a failure. A daemon with
/// no unit is read once: a person starts it again.
fn daemon_after(m: &dyn Machine, plan: &Plan, restarted: bool, found: &mut Found) -> State {
    if !plan.role.daemon {
        return State::None;
    }
    let endpoint = format!("unix:{}", plan.socket);
    let socket = &plan.socket;
    let journal = "see journalctl --user -u sotd.service";
    if !plan.unit_managed() {
        let all = procs::snapshot(m);
        let runs = procs::daemon_may_run(&all, &plan.prefix);
        let (st, why) = daemon::state(m, &plan.sotd(), &endpoint, runs);
        let why = all.read(|_| why.clone(), |listing| format!("{listing}; {why}"));
        if st == State::Unknown {
            found.fail("daemon", format!("a daemon runs on {socket} and could not be told its version ({why})"));
        }
        return st;
    }
    let held_ok = plan.unit_runs && !restarted;
    let Some(active) = unit_active(m, "systemctl").read(Some, |why| {
        found.fail("daemon", format!("{why}, so whether sotd.service runs is unknown; {journal}"));
        None
    }) else {
        return State::Unknown;
    };
    if active {
        let (st, why) = daemon::wait_current(m, &plan.sotd(), &endpoint, super::secs_env(&m.env(), "SOT_INSTALL_RESTART_WAIT_S", 30), held_ok, true);
        match st {
            State::Current => {}
            State::Held if held_ok => {}
            State::Held => found.fail("daemon", format!("sotd.service is active, but the daemon on {socket} still runs another version, held by a window; {journal}")),
            State::Other => found.fail("daemon", format!("sotd.service is active, but the daemon on {socket} still runs another version, not {}; {journal}", plan.pack.version)),
            State::Unknown => {
                found.fail("daemon", format!("sotd.service is active, but the daemon on {socket} could not be told its version ({why}); {journal}"));
            }
            State::None => found.fail("daemon", format!("sotd.service is active, but no daemon answers on {socket}; {journal}")),
        }
        st
    } else {
        let detail = if plan.pin_excludes {
            format!("sotd.service is not running because its host pin excludes this computer ('{}'): hosts.toml declares no daemon here; declare the host (daemon or frontend) in hosts.toml, then run this installer again", plan.me)
        } else if restarted {
            format!("sotd.service is not running after the restart; {journal}")
        } else {
            format!("sotd.service is not running after this run started it; {journal}")
        };
        found.fail("daemon", detail);
        State::None
    }
}

#[cfg(test)]
#[path = "unix_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "unix_observe_tests.rs"]
mod observe_tests;
