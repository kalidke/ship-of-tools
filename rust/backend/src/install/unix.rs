//! The install on Linux and macOS: the steps of `scripts/install.sh`'s body, in its order. Every gate runs before the
//! first write under the prefix, `~/.local/bin`, the unit folder or the comm home; the unit, the wrappers and the boot
//! drop-in are rendered by the checkout's own `scripts/lib/sot-daemon.sh`, which the apply and the wrapper also run.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::daemon::{self, State};
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
    pre_version: String,
    pre_commit: String,
    socket: String,
    unit_runs: bool,
    retire_stops: bool,
    before: State,
    windows_before: bool,
    live: daemon::Work,
    rows_unknown: bool,
}

impl Plan {
    fn sotd(&self) -> PathBuf {
        self.prefix.join("bin/sotd")
    }
    fn units(&self) -> PathBuf {
        self.home.join(".config/systemd/user")
    }
    fn unit_managed(&self) -> bool {
        !self.macos && self.role.daemon && !self.no_service
    }
    fn units_ctx(&self) -> Units<'_> {
        Units { home: &self.home, prefix: &self.prefix, checkout: &self.checkout, systemctl: "systemctl", loginctl: "loginctl" }
    }
}

pub(super) fn install(opts: Options) -> Result<(Found, String), Stop> {
    step("install");
    let home = super::home_dir().ok_or_else(|| Stop::new(1, "HOME is not set"))?;
    let prefix = super::absolute(&opts.prefix.clone().or_else(|| std::env::var_os("SOT_PREFIX").filter(|v| !v.is_empty()).map(PathBuf::from)).unwrap_or_else(|| home.join(".local/share/sot")));
    for (what, p) in [("prefix", &prefix), ("home", &home)] {
        if let Some(why) = path_refused(what, &p.to_string_lossy()) {
            println!("ERROR: {why}");
            return Err(Stop::new(1, format!("refusing the {what} (named above)")));
        }
    }
    let plan = decide(opts, prefix, home)?;
    lay_out(&plan)?;
    let mut found = Found::default();
    julia_and_comm(&plan, &mut found)?;
    configure(&plan)?;
    let restarted = wire(&plan)?;
    write_record(&plan)?;
    check(&plan, restarted, &mut found);
    let pin = if plan.named { verdict::pin_tail(&plan.pack.version) } else { String::new() };
    if found.fail.is_none() && plan.unit_managed() {
        let wired = lib(&plan.checkout, "sot_boot_wired", &[&plan.units().to_string_lossy()]).ok();
        if wired {
            println!("sotd.service: stop it on this computer with systemctl --user stop sotd.service; its boot start follows hosts.toml and the unit's host pin, and enable or disable change no file");
        } else {
            print!("{}", lib(&plan.checkout, "sot_boot_off_line", &[]).out);
        }
    }
    Ok((found, pin))
}

/// Run one function of the checkout's `scripts/lib/sot-daemon.sh` with its arguments.
fn lib(checkout: &Path, func: &str, args: &[&str]) -> cmd::Ran {
    let library = checkout.join("scripts/lib/sot-daemon.sh");
    cmd::output(Command::new("bash").arg("-c").arg(". \"$1\" || exit 1; shift; \"$@\"").arg("bash").arg(&library).arg(func).args(args))
}

fn systemctl(args: &[&str]) -> cmd::Ran {
    cmd::output(Command::new("systemctl").arg("--user").args(args))
}

fn sotd_at(sotd: &Path, args: &[&str]) -> cmd::Ran {
    cmd::output(Command::new(sotd).args(args))
}

/// Everything decided before the first write: the system, the pack, the direction, the role and its gates, the daemon.
#[allow(clippy::too_many_lines, reason = "the decisions in install.sh's order, each a few lines that read as one sequence")]
fn decide(opts: Options, prefix: PathBuf, home: PathBuf) -> Result<Plan, Stop> {
    step("preflight");
    let target = sot_updater::platform::this_platform().filter(|t| *t != "windows-x86_64").ok_or_else(|| {
        Stop::new(1, "this system has no prebuilt release (Linux x86_64 and macOS on Apple silicon do); build from source")
    })?;
    let macos = target == "macos-aarch64";
    if macos {
        say("macOS support is EXPERIMENTAL — both roles work; please report anything broken");
    } else {
        for (label, dir) in [("$HOME".to_string(), home.clone()), ("the state dir".to_string(), std::env::var_os("XDG_STATE_HOME").map_or_else(|| home.join(".local/state"), PathBuf::from))] {
            warn_remote_fs(&label, &dir);
        }
    }
    if !cmd::on_path("jq") {
        let how = if macos { "brew install jq" } else { "sudo apt-get install jq" };
        return Err(Stop::new(1, format!("sot-comm needs jq, which is not on PATH: install it with `{how}`, then run this installer again")));
    }
    step("unpack");
    let want = opts.version.clone().or_else(|| if opts.archive.is_none() { super::bootstrap_tag() } else { None });
    let pack = super::pack::read(&opts.pack, target, want.as_deref()).map_err(|why| Stop::new(1, why))?;
    if let Some(libc) = &pack.libc {
        say(&format!("archive sotd libc: {libc}"));
    }
    let implicit = super::implicit_run() && !opts.named();
    let named = opts.named() || (super::bootstrap_tag().is_some() && !implicit);
    say(&format!("installing Ship of Tools {} into {}", pack.tag(), prefix.display()));
    let scratch = pack.dir.join("tmp");
    let _ = std::fs::create_dir_all(&scratch);

    step("direction");
    let record_path = prefix.join("install.json");
    let pre_text = record::read(&record_path);
    let pre_version = super::installed_version(&prefix);
    let pre_commit = record::string(&pre_text, "commit").unwrap_or_default();
    let new_sotd = pack.exe("sotd");
    if implicit && pre_version.contains('+') {
        let way = super::build_direction(&prefix, &pre_version, &pack.commit);
        return Err(Stop::at("target", 2, super::build_stop(&pre_version, &pack.tag(), &way, "--archive", "--version")));
    }
    if record_path.is_file() || !pre_version.is_empty() {
        let dir = super::direction(&new_sotd, &pack.version, &prefix, &pre_version);
        match dir.as_str() {
            "unknown" => say(&format!("note: could not tell which way {} moves the install at {} (it records {pre_version})", pack.tag(), prefix.display())),
            "same" if pre_commit != pack.commit && super::is_commit(&pre_commit) => {
                say(&format!("this computer already runs {pre_version}: same version, another build ({} to {})", &pre_commit[..9], pack.short()));
            }
            "same" => say(&format!("this computer already runs {pre_version}: a repair run")),
            _ => say(&format!("this run moves the install from {pre_version} to {} ({dir})", pack.version)),
        }
        super::guard_direction(implicit, &pre_version, &dir, &pack.version, &prefix, "--version")?;
    }

    let config = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map_or_else(|| home.join(".config"), PathBuf::from).join("sot");
    heal_old_hosts(&config);

    step("role");
    let mut hub = opts.hub.clone().unwrap_or_default();
    if !hub.is_empty() {
        let ran = sotd_at(&new_sotd, &["topology", "sync", "--hub", &hub]);
        print!("{}{}", ran.out, ran.err);
        if !ran.ok() {
            return Err(Stop::new(1, format!("topology sync --hub {hub} failed")));
        }
    } else if let Some(recorded) = record::string(&pre_text, "hub").filter(|h| !h.is_empty()) {
        hub = recorded;
        if !sotd_at(&new_sotd, &["topology", "sync", "--hub", &hub]).ok() {
            say(&format!("note: could not refresh hosts.toml from the recorded hub '{hub}'; the copy here stands"));
        }
    }
    let me = sot_log::host::state_dir::host_name().map_err(|e| Stop::new(1, e))?;
    let status = sotd_at(&new_sotd, &["topology", "status"]);
    let (mut topo_declared, topo_readable, topo_role) = if status.ok() {
        (true, true, role::from_status(&status.out, &me))
    } else {
        (false, false, None)
    };
    if !status.ok() && !status.err.contains("no hosts.toml at") {
        topo_declared = true;
        say(&format!("WARNING: hosts.toml could not be read ({}); the role comes from the flags, and the shared sotd.service is never disabled from here", status.last_said()));
    }
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
                None => ask_role(&me)?,
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
    if resolved.daemon && !["claude", "ccx"].iter().any(|p| cmd::on_path(p)) && ![".local/bin/claude", ".claude/local/claude", ".local/bin/ccx"].iter().any(|p| home.join(p).is_file()) {
        say("WARNING: neither claude nor ccx (the Codex launcher) found on PATH — install and log in a coding agent on this machine before running sessions");
    }
    if !macos && resolved.frontend {
        let glibc = glibc_version();
        if glibc.is_none_or(|g| g < GLIBC_FLOOR_FE) {
            let shown = glibc.map_or("0".to_string(), |(a, b)| format!("{a}.{b}"));
            return Err(Stop::new(1, format!("the frontend binary needs glibc >= {}.{} (this box: {shown}); --be-only installs the backend without it.", GLIBC_FLOOR_FE.0, GLIBC_FLOOR_FE.1)));
        }
    }
    if resolved.frontend {
        pack.check_binary("sot").map_err(|why| Stop::new(1, why))?;
    }

    step("gate");
    let prefix_s = prefix.to_string_lossy().into_owned();
    let running = running_daemon_bin();
    match role::ownership(running.as_deref(), &home, &prefix_s, macos, resolved.frontend, opts.force_role_change) {
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
    let socket = sotd_at(&new_sotd, &["session-socket-path", "sot"]).first_line();
    let endpoint = format!("unix:{socket}");
    let all = procs::snapshot();
    let unit_active = !macos && cmd::on_path("systemctl") && systemctl(&["is-active", "sotd.service"]).ok();
    let unit_runs = resolved.daemon && !no_service && unit_active;
    // A role with no daemon here stops this host's run of the unit (the retire), so its daemon is asked too.
    let retire_stops = !resolved.daemon && unit_active && home.join(".config/systemd/user/sotd.service").is_file();
    let (before, why) = if resolved.daemon || retire_stops {
        daemon::state(&new_sotd, &endpoint, unit_runs || !procs::daemon_pids(&all, &prefix).is_empty())
    } else {
        (State::None, String::new())
    };
    let windows_before = procs::window_runs(&all, &prefix);
    let mut plan = Plan {
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
    ask_daemon(&mut plan, &new_sotd, &why)?;
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
/// GNU `stat -f -c %T`'s for `REMOTE_FS_TYPES` in `rust/log/src/host/volume.rs`).
fn warn_remote_fs(label: &str, dir: &Path) {
    let ran = cmd::output(Command::new("stat").args(["-f", "-c", "%T"]).arg(dir));
    let fstype = ran.first_line();
    if ran.ok() && ["nfs", "nfs4", "cifs", "smb2", "fuse.sshfs", "9p"].contains(&fstype.as_str()) {
        say(&format!("WARNING: {label} ({}) is on a remote filesystem ({fstype}).", dir.display()));
        say("  Capsule session records cannot live on a remote filesystem (ADR 0043 decision 23). Set XDG_STATE_HOME to a");
        say("  local-disk directory in the environment sotd starts from -- for a systemd install: systemctl --user");
        say("  set-environment XDG_STATE_HOME=/path/on/local/disk, or export it in ~/.bashrc (which the unit sources) --");
        say("  then restart sotd. sotd requires that directory to be owned by you with mode 700 and does not back it up --");
        say("  pick a path that persists across reboots, not /tmp.");
    }
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

/// The program of the `sotd.service` running for this user here, or `None` when none runs: `is-active` is this host's
/// own state, where the unit file may be another host's on a shared home.
fn running_daemon_bin() -> Option<String> {
    if !cmd::on_path("systemctl") || !systemctl(&["is-active", "--quiet", "sotd.service"]).ok() {
        return None;
    }
    role::unit_owner_path(&systemctl(&["cat", "sotd.service"]).out)
}

/// The glibc this box runs, from `ldd --version`'s first line.
fn glibc_version() -> Option<(u32, u32)> {
    let ran = cmd::output(Command::new("ldd").arg("--version"));
    let line = ran.out.lines().next()?.trim_end().to_string();
    let last = line.rsplit(|c: char| !(c.is_ascii_digit() || c == '.')).next()?;
    let (a, b) = last.split_once('.')?;
    Some((a.parse().ok()?, b.split('.').next()?.parse().ok()?))
}

/// The role asked at the console, when nothing else names one (it works under `curl | bash`: the questions go to
/// `/dev/tty`). No console is a refusal.
fn ask_role(me: &str) -> Result<RoleFlag, Stop> {
    let question = format!(
        "No declared topology names this machine ({me}) — where should Ship of Tools run?\n  1) all on this machine        (frontend + backend here)\n  2) frontend here, backend on another machine over SSH\n  3) backend only on this machine (headless server)\nChoose [1-3]: "
    );
    let Some(choice) = daemon::read_answer(&question) else {
        return Err(Stop::new(2, "pick a role: --local | --backend <alias> | --be-only (no TTY for interactive setup, and no install here records one)"));
    };
    match choice.trim() {
        "1" => Ok(RoleFlag::Local),
        "3" => Ok(RoleFlag::BeOnly),
        "2" => {
            let alias = daemon::read_answer("SSH alias/hostname of the backend machine (key-based auth required): ").unwrap_or_default().trim().to_string();
            if alias.is_empty() {
                return Err(Stop::new(1, "backend host is required for the remote layout"));
            }
            let mut ssh = Command::new("ssh");
            ssh.args(sot_protocol::topology::ssh_bridge::SSH_OPTS).args(["-n", &alias, "true"]);
            let ran = cmd::output_within(&mut ssh, std::time::Duration::from_secs(30));
            match ran.code {
                Some(0) => Ok(RoleFlag::Remote(alias)),
                None if ran.err.contains("did not finish") => Err(Stop::new(1, format!("ssh to '{alias}' did not finish within 30s (a network, sshd or remote login that stopped answering)"))),
                _ => Err(Stop::new(1, format!("key-based ssh to '{alias}' doesn't work (ssh-copy-id first, or fix ~/.ssh/config)"))),
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

/// How the question asks: the daemon's live work, its child processes, the person at the console, and a note.
pub(super) struct Asks<'a> {
    pub live: &'a dyn Fn() -> daemon::Work,
    pub kids: &'a dyn Fn() -> Option<Vec<&'static str>>,
    pub ask: &'a dyn Fn(&str) -> bool,
    pub note: &'a dyn Fn(&str),
}

/// The daemon question (scripts/CLAUDE.md, Installers: "A live daemon is asked before it ends"): an active unit on
/// another version is restarted by this run, so its live work is asked first, yes at a console or `--restart-daemon`;
/// a daemon the run cannot tell stops the run unless the unit can be restarted and the answer is yes; a daemon that
/// cannot say is judged by its own children. Changes nothing. The live work, and whether part of it is unknown.
pub(super) fn question(q: &Question, a: &Asks) -> Result<(daemon::Work, bool), Stop> {
    let (socket, why) = (q.socket, q.why);
    let refuse_hint = "Restarting the daemon ends every live REPL and Pluto notebook (their variables are lost); the rows and their agents keep running. Nothing under the install was changed. Ask whoever uses this computer, then re-run with --restart-daemon.";
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
            println!("       Restarting the daemon ends those REPLs and notebooks (their variables are lost); the rows and their agents keep running. Nothing under the install was changed. Ask whoever uses this computer, then re-run with --restart-daemon.");
            let what = if q.retire_stops { format!("this run stops the daemon on {socket}, since this computer runs none in this role") } else { format!("the daemon on {socket} runs another version") };
            return Err(Stop::new(2, format!("{what}, and {}", ended(act))));
        }
        Some(reason) => {
            // The daemon could not say; its own children can: with none that is a REPL or the Pluto server, nothing live
            // is left to end.
            let kids = (a.kids)();
            let (repl, pluto) = kids.as_ref().map_or((false, false), |k| (k.contains(&"repl"), k.contains(&"pluto")));
            if kids.as_ref().is_some_and(Vec::is_empty) && live.rows.is_empty() && !live.pluto {
                if will_restart {
                    (a.note)(&format!("note: could not ask the daemon on {socket} in full ({reason}); it has no REPL or Pluto child, so restarting it ends nothing live"));
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

fn ask_daemon(plan: &mut Plan, new_sotd: &Path, why: &str) -> Result<(), Stop> {
    let endpoint = format!("unix:{}", plan.socket);
    let prefix = plan.prefix.clone();
    let q = Question { before: plan.before, unit_runs: plan.unit_runs, retire_stops: plan.retire_stops, restart_flag: plan.opts.restart_daemon, socket: &plan.socket, why };
    let asks = Asks {
        live: &|| daemon::work(new_sotd, &endpoint),
        kids: &|| daemon::children(&prefix),
        ask: &daemon::ask_restart,
        note: &|t| say(t),
    };
    let (live, unknown) = question(&q, &asks)?;
    plan.live = live;
    plan.rows_unknown = unknown;
    Ok(())
}

/// The commit, the binaries and the checkout: the first writes under the prefix.
#[allow(clippy::too_many_lines, reason = "the layout in install.sh's order: the commit, the binaries, the checkout, the flip and the prune")]
fn lay_out(plan: &Plan) -> Result<(), Stop> {
    step("layout");
    let repo = plan.prefix.join("repo");
    let base = repo.join("base");
    let checkout = plan.checkout.clone();
    let current = repo.join("current");
    let tag = plan.pack.tag();
    let archive = plan.opts.archive.is_some();
    if !julia::which("git").is_some_and(|p| p.is_file()) {
        return Err(Stop::new(1, "git is required (the install includes a repo checkout)"));
    }
    if !base.is_dir() {
        say("creating base clone (blobless, no checkout)");
        let ok = cmd::shown(cmd::git().args(["clone", "--filter=blob:none", "--no-checkout", &super::origin()]).arg(&base)) == Some(0);
        if !ok {
            return Err(Stop::new(1, "base clone failed"));
        }
    }
    // --force on the tag fetch: a release tag moved upstream would otherwise abort every later fetch.
    let commit_ref = format!("{}^{{commit}}", plan.pack.commit);
    if cmd::shown(cmd::git_in(&base).args(["fetch", "--tags", "--force", "origin"])) != Some(0) {
        if archive && cmd::output(cmd::git_in(&base).args(["rev-parse", "--verify", &commit_ref])).ok() {
            say(&format!("note: could not fetch origin; {} already holds {}", base.display(), plan.pack.short()));
        } else {
            return Err(Stop::new(1, "fetch failed"));
        }
    }
    let want = if archive {
        let ran = cmd::output(cmd::git_in(&base).args(["rev-parse", "--verify", &commit_ref]));
        if !ran.ok() {
            return Err(Stop::new(1, format!("commit {} is not in origin after fetch — push the line first", plan.pack.commit)));
        }
        ran.first_line()
    } else {
        let ran = cmd::output(cmd::git_in(&base).args(["rev-parse", &format!("refs/tags/{tag}^{{commit}}")]));
        if !ran.ok() {
            return Err(Stop::new(1, format!("tag {tag} not present after fetch")));
        }
        ran.first_line()
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
            let _ = cmd::output(Command::new("xattr").args(["-d", "com.apple.quarantine"]).arg(&dst));
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
    say(&format!("binaries: {}", sotd_at(&plan.sotd(), &["--version"]).first_line()));

    step("layout");
    if checkout.is_dir() {
        if !cmd::output(cmd::git_in(&checkout).args(["status", "--porcelain"])).out.trim().is_empty() {
            return Err(Stop::new(1, format!("the checkout at {} has local changes — commit/stash/revert, then re-run (updates refuse to move a dirty tree)", checkout.display())));
        }
    } else {
        say(&format!("adding version worktree {tag}"));
        let _ = cmd::output(cmd::git_in(&base).args(["worktree", "prune"]));
        if cmd::shown(cmd::git_in(&base).args(["worktree", "add", "--detach"]).arg(&checkout).arg(&want)) != Some(0) {
            return Err(Stop::new(1, "worktree add failed"));
        }
    }
    let have = cmd::output(cmd::git_in(&checkout).args(["rev-parse", "HEAD"])).first_line();
    if have != want {
        return Err(Stop::new(1, format!("checkout HEAD ({have}) != {tag} commit ({want}) — refusing")));
    }
    if !checkout.join("scripts/lib/sot-daemon.sh").is_file() {
        return Err(Stop::new(1, format!("cannot read {}/scripts/lib/sot-daemon.sh", checkout.display())));
    }
    let prev = flip_current(&repo, &current, &checkout)?;
    let compat = plan.prefix.join("julia");
    let _ = std::fs::create_dir_all(&compat);
    let _ = replace_link(&checkout, &compat.join("current"));
    if !bin.join("sot-apply").is_file() {
        let _ = copy_whole(&checkout.join("scripts/sot-apply.sh"), &bin.join("sot-apply"), Some(0o755));
    }
    prune_versions(&repo, &base, &tag, prev.as_deref());
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
fn flip_current(repo: &Path, current: &Path, checkout: &Path) -> Result<Option<String>, Stop> {
    let mut prev = None;
    if let Ok(target) = std::fs::read_link(current) {
        prev = target.file_name().map(|n| n.to_string_lossy().into_owned());
    } else if current.is_dir() {
        if !cmd::output(cmd::git_in(current).args(["rev-parse", "--git-dir"])).ok() {
            return Err(Stop::new(1, format!("repo/current exists but is not a readable git checkout — refusing to migrate (inspect {})", current.display())));
        }
        if !cmd::output(cmd::git_in(current).args(["status", "--porcelain"])).out.trim().is_empty() {
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
/// which is named and kept. A run that found `repo/current` at the target already prunes nothing.
fn prune_versions(repo: &Path, base: &Path, tag: &str, prev: Option<&str>) {
    let Ok(entries) = std::fs::read_dir(repo.join("versions")) else { return };
    for v in entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let name = v.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name == tag || Some(name.as_str()) == prev || prev == Some(tag) {
            continue;
        }
        let status = cmd::output(cmd::git_in(&v).args(["status", "--porcelain"]));
        let dirty = if status.ok() { status.out.trim().to_string() } else { format!("git status failed: {}", status.last_said()) };
        if dirty.is_empty() {
            say(&format!("pruning old version dir {name}"));
            if !cmd::output(cmd::git_in(base).args(["worktree", "remove"]).arg(&v)).ok() {
                let _ = std::fs::remove_dir_all(&v);
            }
        } else {
            say(&format!("keeping old version dir {name}: it has local changes ({}); it stays until it is clean or you remove it", dirty.lines().next().unwrap_or("")));
        }
    }
    let _ = cmd::output(cmd::git_in(base).args(["worktree", "prune"]));
}

/// Julia (installed through juliaup when none at the channel is on PATH), the floor check, comm, and a daemon role's
/// environments. The channel the steps take, when one is selected.
fn julia_and_comm(plan: &Plan, found: &mut Found) -> Result<(), Stop> {
    step("julia");
    let juliaup_bin = plan.home.join(".juliaup/bin");
    let prepend = |dir: &Path| {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![dir.to_path_buf()];
        dirs.extend(std::env::split_paths(&path));
        if let Ok(joined) = std::env::join_paths(dirs) {
            std::env::set_var("PATH", joined);
        }
    };
    if juliaup_bin.join("juliaup").is_file() {
        prepend(&juliaup_bin);
    }
    let fresh_enough = julia::which("julia").is_some()
        && cmd::output(Command::new("julia").args(["-e", "exit(VERSION >= v\"1.12\" ? 0 : 1)"])).ok();
    if !fresh_enough {
        if cmd::on_path("juliaup") {
            say(&format!("installing Julia channel {} with juliaup", julia::CHANNEL));
            let _ = cmd::output(Command::new("juliaup").args(["add", julia::CHANNEL]));
        } else {
            say(&format!("installing Julia (juliaup, channel {})", julia::CHANNEL));
            let script = format!("curl {} -fsSL https://install.julialang.org | sh -s -- --yes --default-channel \"$1\"", cmd::CURL_BOUND.join(" "));
            let _ = cmd::shown(Command::new("sh").args(["-c", &script, "sh", julia::CHANNEL]));
        }
        prepend(&juliaup_bin);
    }
    if julia::which("julia").is_none() {
        return Err(Stop::new(1, "Julia install failed; julia is still not on PATH"));
    }
    // The channel is chosen once, by a probe, never by running a step and rerunning it on failure.
    let selected = cmd::output(Command::new("julia").args([&format!("+{}", julia::CHANNEL), "-e", "exit(0)"])).ok().then(|| julia::CHANNEL.to_string());
    let checkout = plan.checkout.clone();
    let facts = julia::facts(&checkout, selected.as_deref()).map_err(|why| Stop::new(1, why))?;
    match julia::judge(&facts) {
        julia::Judged::Ok => {}
        julia::Judged::Step(line) => found.step(line),
        julia::Judged::Stop(line) => return Err(Stop::new(1, line)),
    }

    step("comm");
    say("installing agent comm resources (sot-comm, Claude/Codex skills)");
    if !julia::update_comm(&checkout, selected.as_deref(), &plan.scratch) {
        return Err(Stop::new(1, "ShipTools.update_comm() failed"));
    }
    if plan.role.daemon {
        for failed in julia::instantiate(&checkout, selected.as_deref(), &plan.scratch, &|t| say(t)) {
            found.fail("julia", failed);
        }
        let mathjax = checkout.join("rust/backend/sidecars/mathjax");
        if cmd::on_path("npm") {
            say("installing MathJax sidecar deps (npm ci)");
            if cmd::shown(Command::new("npm").args(["ci", "--silent"]).current_dir(&mathjax)) != Some(0) {
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
fn configure(plan: &Plan) -> Result<(), Stop> {
    step("config");
    // An install from before 2026-09-02 wrote a drop-in carrying the installing shell's Julia depot into the unit; no
    // depot is ever set, so it goes on every run.
    let depot = plan.units().join("sotd.service.d/depot.conf");
    if depot.is_file() && std::fs::remove_file(&depot).is_ok() && cmd::on_path("systemctl") {
        let _ = systemctl(&["daemon-reload"]);
    }
    if !plan.role.daemon {
        let kernel_host = sot_log::host::state_dir::kernel_host_name().unwrap_or_default();
        retire_local_service(&plan.units_ctx(), plan.topo_declared, &kernel_host);
    }
    for line in trust_report(&plan.sotd(), &plan.home) {
        say(&line);
    }
    Ok(())
}

/// Folder trust, declared by the agents' own offline command (`sotd trust declare <home>`): the lines that say what came
/// of it. A failure is said, with what the command said, and never described as a declaration.
pub(super) fn trust_report(sotd: &Path, home: &Path) -> Vec<String> {
    let ran = cmd::output(Command::new(sotd).args(["trust", "declare"]).arg(home));
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

/// The programs and folders the unit's wiring touches: the systemd user folder under `home`, the prefix whose `sotd`
/// pins it, the checkout whose library renders it, and `systemctl` and `loginctl` (stand-ins in a test).
pub(super) struct Units<'a> {
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
        cmd::output(Command::new(self.systemctl).arg("--user").args(args).env("HOME", self.home))
    }
    fn has_systemctl(&self) -> bool {
        Path::new(self.systemctl).is_file() || cmd::on_path(self.systemctl)
    }
    /// One function of the checkout's library, with this home.
    fn lib(&self, func: &str, args: &[&str]) -> cmd::Ran {
        let library = self.checkout.join("scripts/lib/sot-daemon.sh");
        cmd::output(Command::new("bash").arg("-c").arg(". \"$1\" || exit 1; shift; \"$@\"").arg("bash").arg(&library).arg(func).args(args).env("HOME", self.home))
    }
    fn pin(&self, claim: bool) -> bool {
        let dir = self.dir().to_string_lossy().into_owned();
        let mut args = vec!["topology", "pin", "--dir", dir.as_str()];
        if claim {
            args.push("--claim");
        }
        cmd::output(Command::new(self.prefix.join("bin/sotd")).args(&args).env("HOME", self.home)).ok()
    }
}

/// A role with no daemon here must not leave an earlier local daemon running. The unit and its boot drop-in may serve
/// another host of a shared home, so only this host's run is stopped and the shared unit is never disabled; under a
/// declared topology the unit's host pin keeps it from starting here, and with no hosts.toml a pin that names this host
/// alone lets the drop-in go. `kernel_host` is the kernel's name for this host, which the pin names.
pub(super) fn retire_local_service(u: &Units, declared: bool, kernel_host: &str) {
    let ud = u.dir().to_string_lossy().into_owned();
    if !u.has_systemctl() || !u.dir().join("sotd.service").is_file() {
        return;
    }
    if declared {
        if !u.pin(false) {
            say("WARNING: sotd topology pin failed: sotd.service is not pinned to the hosts that run sotd");
        }
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
        if u.lib("sot_boot_wired", &[&ud]).ok() {
            say("Stopped this computer's sotd. Boot start follows hosts.toml and the unit's host pin, not enable or disable, which change no file for sotd.service, so a hand disable no longer turns it off; to move it, change hosts.toml, or with none run sotd topology pin --dir ~/.config/systemd/user --claim on the host that should run it. If no other computer shares this home folder, turn boot start off with: rm -f ~/.config/systemd/user/default.target.d/sotd.conf (write that file again, with the two lines [Unit] and Wants=sotd.service, to turn it back on)");
        } else {
            say("Stopped this computer's sotd. Its boot start is already off: sotd.service has no boot drop-in, and enable and disable change no file for it; to turn it on, write ~/.config/systemd/user/default.target.d/sotd.conf with the two lines [Unit] and Wants=sotd.service");
        }
    }
}

/// The unit, the launcher and the restart; whether the run restarted the daemon.
fn wire(plan: &Plan) -> Result<bool, Stop> {
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
        enable_local_service(&plan.units_ctx(), &plan.pack.bin.join("sotd.service"), &plan.socket)?;
    }
    step("launcher");
    if plan.role.frontend {
        wire_launcher(plan)?;
    }
    step("restart");
    let restart = plan.unit_runs && matches!(plan.before, State::Other | State::Unknown);
    if restart {
        say("restarting sotd to pick up the new version");
        let _ = systemctl(&["try-restart", "sotd.service"]);
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
    if !u.lib("render_sotd_unit", &[&u.prefix.to_string_lossy(), &template.to_string_lossy(), &unit.to_string_lossy()]).ok() {
        return Err(Stop::new(1, "could not write the sotd unit"));
    }
    let before = role::pin_hosts(&units);
    if !u.pin(true) {
        say("WARNING: sotd topology pin failed: sotd.service is not pinned to the hosts that run sotd");
    }
    let after = role::pin_hosts(&units);
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
    let user = std::env::var("USER").unwrap_or_else(|_| cmd::output(Command::new("id").arg("-un")).first_line());
    let _ = cmd::output(Command::new(u.loginctl).args(["enable-linger", &user]));
    say(&format!("sotd running: {} (socket {socket})", u.systemctl(&["is-active", "sotd.service"]).first_line()));
    Ok(())
}

/// The window's launcher: the all-in-one wrapper (this computer's daemon), or the one that dials the topology plan when
/// the run names a backend or the plan lists another daemon host; then the desktop entry or the app bundle.
fn wire_launcher(plan: &Plan) -> Result<(), Stop> {
    let local_bin = plan.home.join(".local/bin");
    std::fs::create_dir_all(&local_bin).map_err(|e| Stop::new(1, format!("creating {}: {e}", local_bin.display())))?;
    let others = plan.be_alias.is_none() && {
        let ran = sotd_at(&plan.sotd(), &["topology", "plan", "--self", &plan.me]);
        ran.ok() && role::plan_dials_others(&ran.out)
    };
    if others {
        say("the topology plan lists other daemon hosts: the launcher dials every host of the plan, and this computer's own daemon beside them");
    }
    let wrapper = local_bin.join("sot-launch");
    let (prefix, dest) = (plan.prefix.to_string_lossy().into_owned(), wrapper.to_string_lossy().into_owned());
    let rendered = if plan.be_alias.is_some() || others {
        lib(&plan.checkout, "render_remote_launch", &[&prefix, plan.be_alias.as_deref().unwrap_or(""), &dest])
    } else {
        lib(&plan.checkout, "render_sot_launch", &[&prefix, plan.target, &dest])
    };
    if !rendered.ok() {
        print!("{}", rendered.err);
        return Err(Stop::new(1, format!("writing {dest} failed")));
    }
    if plan.macos { app_bundle(plan, &dest) } else { desktop_entry(plan, &dest) }
}

/// A minimal app bundle, so the window starts from Launchpad, Spotlight and the Dock; its icon made from the checkout's
/// logo with sips and iconutil (both ship with macOS). A bundle made here carries no quarantine attribute.
fn app_bundle(plan: &Plan, wrapper: &str) -> Result<(), Stop> {
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
    if logo.is_file() && cmd::on_path("sips") && cmd::on_path("iconutil") {
        let iconset = plan.scratch.join("sot.iconset");
        let _ = std::fs::create_dir_all(&iconset);
        for sz in [16, 32, 128, 256, 512] {
            for (scale, suffix) in [(1, ""), (2, "@2x")] {
                let px = (sz * scale).to_string();
                let _ = cmd::output(Command::new("sips").args(["-z", &px, &px]).arg(&logo).arg("--out").arg(iconset.join(format!("icon_{sz}x{sz}{suffix}.png"))));
            }
        }
        let made = cmd::output(Command::new("iconutil").args(["-c", "icns"]).arg(&iconset).arg("-o").arg(app.join("Resources/sot.icns"))).ok();
        say(if made { "app icon generated from the checkout logo" } else { "icon generation failed (cosmetic) — bundle works without it" });
    }
    say("FE launcher: sot-launch + 'Ship of Tools' in ~/Applications (Launchpad/Spotlight)");
    Ok(())
}

/// The desktop entry, with the logo installed into the hicolor theme and kept under the prefix as the fallback icon
/// path (the prefix does not move; the checkout does).
fn desktop_entry(plan: &Plan, wrapper: &str) -> Result<(), Stop> {
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
            if cmd::on_path("gtk-update-icon-cache") {
                let _ = cmd::output(Command::new("gtk-update-icon-cache").args(["-q", "-t", "-f"]).arg(&theme));
            }
        }
    }
    let entry = format!("[Desktop Entry]\nType=Application\nName=Ship of Tools\nComment=Agentic Julia development environment\nExec={wrapper}\nIcon={icon}\nTerminal=false\nCategories=Development;IDE;\nStartupWMClass=sot\n");
    record::write_whole(&apps.join("ship-of-tools.desktop"), &entry).map_err(|e| Stop::new(1, format!("writing the desktop entry failed: {e}")))?;
    if cmd::on_path("update-desktop-database") {
        let _ = cmd::output(Command::new("update-desktop-database").arg(&apps));
    }
    say(&format!("FE launcher: sot-launch (+ desktop entry, icon {icon})"));
    Ok(())
}

/// install.json, last, whole.
fn write_record(plan: &Plan) -> Result<(), Stop> {
    step("record");
    let commit = cmd::output(cmd::git_in(&plan.checkout).args(["rev-parse", "HEAD"])).first_line();
    let r = record::Record {
        prefix: plan.prefix.to_string_lossy().into_owned(),
        config: plan.config.to_string_lossy().into_owned(),
        service: role::service_record(!plan.macos, plan.role.daemon, plan.no_service).into(),
        version: plan.pack.version.clone(),
        commit: if commit.is_empty() { "unknown".into() } else { commit },
        installed_at: record::now_utc(),
        hub: plan.hub.clone(),
        daemon: plan.role.daemon,
        frontend: plan.role.frontend,
        pinned: plan.named,
    };
    let path = plan.prefix.join("install.json");
    record::write_whole(&path, &record::text(&r)).map_err(|e| Stop::new(1, format!("writing {}: {e}", path.display())))?;
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
/// STEP LEFT.
#[allow(clippy::too_many_lines, reason = "the checks in install.sh's order, one block each")]
fn check(plan: &Plan, restarted: bool, found: &mut Found) {
    step("check");
    let (version, prefix) = (&plan.pack.version, &plan.prefix);
    let checkout = plan.checkout.clone();
    let commit = cmd::output(cmd::git_in(&checkout).args(["rev-parse", "HEAD"])).first_line();
    let short = commit.get(..9).unwrap_or("").to_string();
    found.pass = format!("Ship of Tools {version} at {short} on this computer");
    if commit.is_empty() {
        found.fail("checkout", format!("cannot read the commit of {}", checkout.display()));
    }
    for b in ["sotd", "sot"] {
        if b == "sot" && !plan.role.frontend {
            continue;
        }
        let ran = sotd_at(&prefix.join("bin").join(b), &["--version"]);
        let line = if ran.ok() { ran.first_line() } else { format!("does not run: {}", ran.last_said()) };
        if !super::pack::binary_says(&line, b, version, &commit) {
            found.fail("binaries", format!("{}/bin/{b} says '{line}', not {version} at {short}", prefix.display()));
        }
    }
    let text = record::read(&prefix.join("install.json"));
    if record::string(&text, "version").as_deref() != Some(version) || record::string(&text, "commit").as_deref() != Some(&commit) {
        found.fail("record", format!("{}/install.json does not name {version} at {short}", prefix.display()));
    }
    let current = prefix.join("repo/current");
    let resolves = std::fs::read_link(&current).ok() == Some(checkout.clone())
        && cmd::output(cmd::git_in(&current).args(["rev-parse", "HEAD"])).first_line() == commit;
    if !resolves {
        found.fail("checkout", format!("{} does not resolve to {} at {short}", current.display(), checkout.display()));
    }
    let pointer = prefix.join(format!("updates/pending-{}.json", plan.target));
    if pointer.exists() {
        found.fail("pointer", format!("an update pointer is armed again ({}): another daemon armed it after this run dropped it; remove it and run this again", pointer.display()));
    }
    let comm = std::env::var_os("SOT_COMM_HOME").map_or_else(|| plan.home.join(".sot-comm"), PathBuf::from);
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
    let after = daemon_after(plan, restarted, found);
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
        let sync = sotd_at(&plan.sotd(), &["topology", "sync", "--hub", &hub]);
        let said = format!("{}{}", sync.out, sync.err);
        if sync.ok() {
        } else if said.contains("does not list this box") {
            found.fail("enroll", format!("the hosts.toml on '{hub}' does not list this computer ('{}'), so the window would open with no hosts: on '{hub}' run  sotd topology set add {} --frontend  (the sotd of its install), then run this installer again", plan.me, plan.me));
        } else if !sotd_at(&plan.sotd(), &["topology", "plan", "--self", &plan.me]).ok() {
            found.fail("enroll", format!("could not read the hosts.toml on '{hub}': {}", sync.last_said()));
        }
    }
}

/// The daemon after the run. A unit this run restarted or started must answer as the target within the bound; one it
/// left alone may be held by a window on its old version; any other state of an active unit is a failure. A daemon with
/// no unit is read once: a person starts it again.
fn daemon_after(plan: &Plan, restarted: bool, found: &mut Found) -> State {
    if !plan.role.daemon {
        return State::None;
    }
    let endpoint = format!("unix:{}", plan.socket);
    let socket = &plan.socket;
    let journal = "see journalctl --user -u sotd.service";
    if !plan.unit_managed() {
        let runs = !procs::daemon_pids(&procs::snapshot(), &plan.prefix).is_empty();
        let (st, why) = daemon::state(&plan.sotd(), &endpoint, runs);
        if st == State::Unknown {
            found.fail("daemon", format!("a daemon runs on {socket} and could not be told its version ({why})"));
        }
        return st;
    }
    let held_ok = plan.unit_runs && !restarted;
    if systemctl(&["is-active", "sotd.service"]).ok() {
        let st = daemon::wait_current(&plan.sotd(), &endpoint, super::secs_env("SOT_INSTALL_RESTART_WAIT_S", 30), held_ok, true);
        match st {
            State::Current => {}
            State::Held if held_ok => {}
            State::Held => found.fail("daemon", format!("sotd.service is active, but the daemon on {socket} still runs another version, held by a window; {journal}")),
            State::Other => found.fail("daemon", format!("sotd.service is active, but the daemon on {socket} still runs another version, not {}; {journal}", plan.pack.version)),
            State::Unknown => {
                let (_, why) = daemon::state(&plan.sotd(), &endpoint, true);
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
