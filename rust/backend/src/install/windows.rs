//! The install on Windows: the steps of `scripts/install.ps1`'s body, in its order. A fresh install lays down `bin\`,
//! `repo\base`, `repo\versions\v<v>` and the `repo\current` junction; an update stages the pack with its own `sotd update
//! stage` and applies it one way, by what runs here: a supervisor that logs its launcher code id gets the converge
//! sentinel, an older supervisor or a lone window leaves it armed for a start, nothing running at the desk applies it
//! here, nothing running over ssh leaves it armed. `repo\current` is read by its target and never traversed, except by
//! the shortcut step, because an ssh session on a box with redirection trust cannot traverse a junction.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sot_platform::host::env::Env;

use super::daemon::{self, State};
use super::observe::{self, Fact, Live, Machine, ASK_BOUND};
use super::options::Options;
use super::pack::Pack;
use super::supervisor::Way;
use super::verdict::{self, step, Found, Stop};
use super::{julia, procs, record};

const TARGET: &str = "windows-x86_64";

/// What the run works with.
pub(super) struct Ctx<'a> {
    /// Every child the run starts, and the environment it reads.
    pub(super) m: &'a dyn Machine,
    pub(super) env: Env,
    pub(super) opts: Options,
    pub(super) prefix: PathBuf,
    pub(super) pack: Pack,
    pub(super) named: bool,
    pub(super) pipe: String,
    pub(super) log_dir: PathBuf,
    pub(super) base: PathBuf,
    pub(super) version_dir: PathBuf,
    /// A converge restarted the daemon: it must answer as the target.
    pub(super) expect_daemon: bool,
    /// A repair run: it moves nothing, and the record gets its pin alone.
    pub(super) repair: bool,
}

impl Ctx<'_> {
    fn sotd(&self) -> PathBuf {
        self.prefix.join("bin\\sotd.exe")
    }
    fn record_path(&self) -> PathBuf {
        self.prefix.join("install.json")
    }
    fn pointer(&self) -> PathBuf {
        self.prefix.join("updates\\pending-windows-x86_64.json")
    }
    fn endpoint(&self) -> String {
        format!("pipe:{}", self.pipe)
    }
}

fn local_app_data(env: &Env) -> PathBuf {
    env.get_nonempty("LOCALAPPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn powershell(args: &[&str]) -> Command {
    let mut c = Command::new("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass"]).args(args);
    c
}

/// Run a script of a checkout and show its lines; its exit code and what it said.
fn script(m: &dyn Machine, path: &Path, args: &[&str]) -> super::cmd::Ran {
    let mut c = powershell(&["-File"]);
    c.arg(path).args(args);
    let ran = m.act(&mut c);
    for l in ran.out.lines().chain(ran.err.lines()).filter(|l| !l.trim().is_empty()) {
        println!("  {l}");
    }
    ran
}

pub(super) fn install(opts: Options) -> Result<(Found, String), Stop> {
    install_on(&Live, opts)
}

fn install_on(m: &dyn Machine, opts: Options) -> Result<(Found, String), Stop> {
    step("options");
    let env = m.env();
    let prefix = opts.prefix.as_deref().map_or_else(|| local_app_data(&env).join("sot"), super::absolute);
    step("preflight");
    let arch = Some(env.text("PROCESSOR_ARCHITEW6432")).filter(|a| !a.is_empty()).unwrap_or_else(|| env.text("PROCESSOR_ARCHITECTURE"));
    if arch != "AMD64" {
        return Err(Stop::new(2, format!("Ship of Tools needs Windows x86_64 (this is {arch})")));
    }
    if !observe::on_path(m, "git") {
        return Err(Stop::new(2, "git is not on PATH: winget install --id Git.Git -e"));
    }
    if !observe::on_path(m, "jq") {
        return Err(Stop::new(1, "sot-comm needs jq, which is not on PATH: install it with `winget install jqlang.jq`, then run this installer again"));
    }
    step("fetch");
    let implicit = super::implicit_run(&env) && !opts.named();
    let named = opts.named() || (super::bootstrap_tag(&env).is_some() && !implicit);
    let want = opts.version.clone().or_else(|| if opts.archive.is_none() { super::bootstrap_tag(&env) } else { None });
    let pack = super::pack::read(m, &opts.pack, TARGET, want.as_deref()).map_err(|why| Stop::new(1, why))?;
    pack.check_binary(m, "sot").map_err(|why| Stop::new(1, why))?;
    println!("installing Ship of Tools {} into {}", pack.tag(), prefix.display());
    let new_sotd = pack.exe("sotd");
    let pipe = super::endpoint_name(super::ask_endpoint(m, &new_sotd, "local", |name| name.starts_with(r"\\.\pipe\")))?;
    let version_dir = prefix.join("repo\\versions").join(pack.tag());
    let mut found = Found::default();
    let mut ctx = Ctx {
        m,
        base: prefix.join("repo\\base"),
        log_dir: local_app_data(&env).join("sot\\logs"),
        env,
        version_dir,
        opts,
        prefix,
        pack,
        named,
        pipe,
        expect_daemon: false,
        repair: false,
    };
    let pin = if named { verdict::pin_tail(&ctx.pack.version) } else { String::new() };
    let installed = super::installed_for(super::installed_version(m, &ctx.prefix), &ctx.prefix, implicit, "-Version", &mut found)?;
    let mut dir = None;
    let way = super::meet(&installed, ctx.sotd().is_file(), || {
        step("direction");
        if implicit && installed.contains('+') {
            let way = super::build_direction(m, &ctx.prefix, &installed, &ctx.pack.commit);
            return Err(Stop::at("target", 2, super::build_stop(&installed, &ctx.pack.tag(), way, "-Archive", "-Version")));
        }
        let asked = super::direction(m, &new_sotd, &ctx.pack.version, &ctx.prefix, &installed);
        dir = super::guard_direction(implicit, &installed, asked, &ctx.pack.version, &ctx.prefix, "-Version", &mut found)?;
        Ok(())
    })?;
    let publish = super::publish::Plan::capture()?;
    let pending = match way {
        super::Meet::Update => update_or_repair(&mut ctx, &new_sotd, &installed, dir, &mut found)?,
        super::Meet::Fresh => {
            fresh(&ctx)?;
            None
        }
    };
    found.pass = format!("Ship of Tools {} at {} on this computer", ctx.pack.version, ctx.pack.short());
    if let Some(step) = pending {
        // The update stays armed for a start: the run ends with that step.
        found.step(step);
        publish.pending(&ctx.version_dir, &ctx.pack.commit, &ctx.base, &mut found)?;
        return Ok((found, pin));
    }
    let enrolled = publish.enroll(&ctx.version_dir, &ctx.pack.commit, &ctx.base, &mut found)?;
    if observe::which(m, "julia").is_some() {
        step("julia");
        let facts = julia::facts(m, &ctx.version_dir, None).map_err(|why| Stop::new(1, why))?;
        match julia::judge(&facts) {
            julia::Judged::Ok => {}
            julia::Judged::Step(line) => found.step(line),
            julia::Judged::Stop(line) => return Err(Stop::new(1, line)),
        }
    }
    layout_and_comm(&ctx, &mut found)?;
    shortcut(&ctx, &mut found)?;
    let refusal = sync_hub(&ctx);
    write_record(&ctx)?;
    end_state(&ctx, &mut found);
    enrolled.check(&mut found);
    if let Some(hub) = refusal {
        let me = sot_platform::host::state_dir::host_name_in(&ctx.env).unwrap_or_else(|_| ctx.env.text("COMPUTERNAME").to_lowercase());
        found.fail("enroll", format!("the hosts.toml on '{hub}' does not list this computer ('{me}'), so the window would open with no hosts: on '{hub}' run  sotd topology set add {me} --frontend  (the sotd of its install), then run this installer again"));
    }
    Ok((found, pin))
}

fn armed_tail(ctx: &Ctx) -> String {
    let pin = if ctx.named { verdict::pin_tail(&ctx.pack.version) } else { String::new() };
    format!("; the update to {} stays armed: the next start applies it{pin}", ctx.pack.tag())
}

/// Copy the binaries, clone the base and add the version folder.
#[allow(clippy::disallowed_methods, reason = "sotd install copies the pack's binaries in the foreground and starts no program while a copy is open")]
fn fresh(ctx: &Ctx) -> Result<(), Stop> {
    step("binaries");
    let bin = ctx.prefix.join("bin");
    std::fs::create_dir_all(&bin).map_err(|e| Stop::new(1, format!("creating {}: {e}", bin.display())))?;
    for e in ["sot.exe", "sotd.exe", "sot-capsule.exe"] {
        std::fs::copy(ctx.pack.bin.join(e), bin.join(e)).map_err(|err| Stop::new(1, format!("could not write {}\\{e}: {err}", bin.display())))?;
    }
    init_base(ctx)?;
    version_folder(ctx)
}

/// `repo\base`: a blobless clone that holds the pack's commit, with its tag for a release.
fn init_base(ctx: &Ctx) -> Result<(), Stop> {
    step("checkout");
    let origin = super::origin(&ctx.env);
    if !ctx.base.join(".git").exists() {
        std::fs::create_dir_all(ctx.base.parent().unwrap_or(&ctx.base)).ok();
        let ran = ctx.m.act(super::cmd::git().args(["clone", "--quiet", "--filter=blob:none", "--no-checkout", &origin]).arg(&ctx.base));
        if !ran.ok() {
            return Err(Stop::new(1, format!("could not clone {origin} into {}: {}", ctx.base.display(), ran.last_said())));
        }
    }
    let commit = &ctx.pack.commit;
    let no_answer = |why| Stop::new(1, format!("could not ask git in {}: {why}", ctx.base.display()));
    // `cat-file -e` exits 0 when the commit is there and nonzero when it is not; a git that gives no answer is not
    // "absent".
    let held = observe::ask(ctx.m, super::cmd::git_in(&ctx.base).args(["cat-file", "-e", &format!("{commit}^{{commit}}")]), ASK_BOUND, |a| Ok(a.exit == 0)).or(no_answer)?;
    if !held && !ctx.m.act(super::cmd::git_in(&ctx.base).args(["fetch", "--quiet", "origin", commit])).ok() {
        return Err(Stop::new(1, format!("{commit} is not in {} and could not be fetched from {origin}", ctx.base.display())));
    }
    // A release's tag, fetched when the clone lacks it; a build of the line has no tag and asks nothing.
    if !ctx.pack.version.contains('+') {
        let tag = ctx.pack.tag();
        let tagged = observe::ask(ctx.m, super::cmd::git_in(&ctx.base).args(["tag", "-l", &tag]), ASK_BOUND, |a| Ok(!a.zero()?.out.trim().is_empty())).or(no_answer)?;
        if !tagged {
            let _ = ctx.m.act(super::cmd::git_in(&ctx.base).args(["fetch", "--quiet", "origin", &format!("refs/tags/{tag}:refs/tags/{tag}")]));
        }
    }
    Ok(())
}

fn version_folder(ctx: &Ctx) -> Result<(), Stop> {
    let head = |dir: &Path| observe::ask(ctx.m, super::cmd::git_in(dir).args(["rev-parse", "HEAD"]), ASK_BOUND, |a| Ok(a.zero()?.first_line())).or(|why| Stop::new(1, format!("could not read HEAD of {}: {why}", dir.display())));
    if ctx.version_dir.exists() {
        if head(&ctx.version_dir)? == ctx.pack.commit {
            return Ok(());
        }
        return Err(Stop::new(1, format!("{} exists but is not at {}; remove that folder and run this again", ctx.version_dir.display(), ctx.pack.short())));
    }
    let ran = ctx.m.act(super::cmd::git_in(&ctx.base).args(["worktree", "add", "--detach"]).arg(&ctx.version_dir).arg(&ctx.pack.commit));
    if !ran.ok() {
        return Err(Stop::new(1, format!("could not add the worktree {} at {}: {}", ctx.version_dir.display(), ctx.pack.short(), ran.last_said())));
    }
    if head(&ctx.version_dir)? != ctx.pack.commit {
        return Err(Stop::new(1, format!("{} is not at {}", ctx.version_dir.display(), ctx.pack.commit)));
    }
    Ok(())
}

/// An install already here: never backwards unasked, a repair run when nothing moves, else the update. The step of an
/// update left armed for a start, when it is.
fn update_or_repair(ctx: &mut Ctx, new_sotd: &Path, installed: &str, dir: Option<super::Direction>, found: &mut Found) -> Result<Option<String>, Stop> {
    // A repair changes nothing in the record but its pin: a new -Hub is an install over the same build. A daemon that
    // cannot be told is no repair: the update asks about it, with its reason.
    let keeps = record::keeps(&record::read(&ctx.record_path()), &record_of(ctx));
    let before = daemon_state(ctx, new_sotd);
    if dir == Some(super::Direction::Same) && keeps && matches!(before.0, State::None | State::Current) {
        println!("this computer already runs {installed} at {}: a repair run", ctx.pack.short());
        ctx.repair = true;
        // The pin first, then the pointer armed over the install: no arm lands between.
        write_pin(ctx, ctx.named)?;
        std::fs::remove_file(ctx.pointer()).ok();
        return Ok(None);
    }
    let from = if installed.is_empty() { "a version that cannot be read" } else { installed };
    let how = match dir {
        Some(super::Direction::Same) => ": same version, another build",
        Some(super::Direction::Newer) => " (newer)",
        Some(super::Direction::Older) => " (older)",
        None => " (which way could not be told)",
    };
    println!("this run moves the install from {from} to {}{how}", ctx.pack.version);
    update(ctx, new_sotd, installed, before, found)
}

/// The daemon's state: its pipe asked; a daemon whose pipe exists or whose program runs and cannot be told is
/// `Unknown`. A window that holds it counts as another version: the converge or the start replaces it. Whether one
/// runs is asked only when it could not be told; a process listing that failed then presumes one does, so it is
/// `Unknown`, never `None`, and the listing's reason leads `why`.
fn daemon_state(ctx: &Ctx, sotd: &Path) -> (State, String) {
    let (st, why) = daemon::state(ctx.m, sotd, &ctx.endpoint(), &|| daemon_runs(ctx));
    (if st == State::Held { State::Other } else { st }, why)
}

/// Whether a daemon may run: its pipe is listed, or its program runs.
fn daemon_runs(ctx: &Ctx) -> Fact<bool> {
    if pipe_listed(&ctx.pipe) {
        return Fact::known(true);
    }
    procs::snapshot(ctx.m).map(|all| !procs::daemon_pids(&all, &ctx.prefix).is_empty())
}

/// Whether the pipe is listed (never opened: a probe must not take an instance).
fn pipe_listed(pipe: &str) -> bool {
    let Some(name) = pipe.strip_prefix(r"\\.\pipe\") else { return false };
    std::fs::read_dir(r"\\.\pipe\").is_ok_and(|d| d.flatten().any(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name)))
}

fn write_pin(ctx: &Ctx, pinned: bool) -> Result<(), Stop> {
    record::set_pin(&ctx.record_path(), &ctx.prefix, pinned).map_err(|e| Stop::new(1, format!("writing the pin to {}: {e}", ctx.record_path().display())))?;
    verdict::set_tail(if pinned { verdict::pin_tail(&ctx.pack.version) } else { String::new() });
    Ok(())
}

/// Stage with the pack's own sotd, then apply one way, by what runs here; the step when the update stays armed.
#[allow(clippy::too_many_lines, reason = "install.ps1's update in its order: the question, the stage, then the one way it applies")]
fn update(ctx: &mut Ctx, new_sotd: &Path, installed: &str, before: (State, String), found: &mut Found) -> Result<Option<String>, Stop> {
    step("daemon");
    // Every path below that stops or restarts a daemon asks first, whatever its version.
    let (state, why) = before;
    let clauses = std::cell::RefCell::new(Vec::new());
    let asks = daemon::Asks {
        live: &|| daemon::work(ctx.m, new_sotd, &ctx.endpoint()),
        kids: &|| daemon::children(ctx.m, &ctx.prefix),
        ask: &|work| daemon::ask_restart(&ctx.env, work),
        note: &|t| println!("{t}"),
        clause: &|c| clauses.borrow_mut().push(c),
    };
    let asked = daemon::ask_before_update(state, &ctx.pipe, &why, ctx.opts.restart_daemon, &asks);
    for c in clauses.into_inner() {
        found.checked(c);
    }
    let live = asked?;
    step("stage");
    if !ctx.base.join(".git").exists() {
        init_base(ctx)?;
        step("stage");
    }
    // The pin goes in before the stage, so no automatic arm lands between the stage and the pin.
    let was_pinned = record::boolean(&record::read(&ctx.record_path()), "pinned") == Some(true);
    write_pin(ctx, ctx.named)?;
    println!("staging and preparing {} (this can take many minutes)", ctx.pack.tag());
    let ran = ctx.m.act(Command::new(new_sotd).args(["update", "stage"]).arg(&ctx.pack.dir).arg("--prefix").arg(&ctx.prefix));
    for l in ran.out.lines() {
        println!("  {l}");
    }
    if !ran.ok() {
        let mut detail = ran.last_said();
        if detail.is_empty() {
            detail = format!("sotd update stage ended with exit {}", ran.code.map_or("none".into(), |c| c.to_string()));
        }
        if format!("{}{}", ran.out, ran.err).contains("not armed") {
            detail = format!("{detail} (remove {}\\updates\\bad-{}-windows-x86_64 if the version was marked bad, then run this again)", ctx.prefix.display(), ctx.pack.tag());
        }
        write_pin(ctx, was_pinned)?;
        verdict::set_tail(String::new());
        return Err(Stop::new(1, detail));
    }
    verdict::set_tail(armed_tail(ctx));
    // A listing that gave no answer is no supervisor, window or desk known: the way is unknown, and nothing is stopped.
    let (seen, listing) = procs::snapshot(ctx.m).read(
        |all| {
            let sup = supervisor(&ctx.log_dir, &all);
            let window_only = sup.is_none() && procs::window_runs(&all, &ctx.prefix);
            (Some((sup, window_only)), None)
        },
        |why| (None, Some(why.to_string())),
    );
    let work = if live.is_empty() { String::new() } else { format!("; {live}") };
    let session = std::cell::RefCell::new(None);
    let session_of = || {
        let fact = session_id(ctx.m);
        fact.as_ref().read(|id| Some(*id), |why| {
            *session.borrow_mut() = Some(why.to_string());
            None
        })
    };
    match super::supervisor::way(seen.as_ref().map(|(sup, window_only)| (sup.as_ref().map(|(_, code)| code.as_str()), *window_only)), &session_of) {
        Way::Unknown => {
            let why = listing.or_else(|| session.borrow().clone()).unwrap_or_else(|| "its logon session could not be read".into());
            return Ok(Some(format!("start Ship of Tools at the desk or quit it and start it again; the start applies {} (this run could not tell whether a window or supervisor runs, or whether it is at the desk ({why}), and stopped nothing){work}", ctx.pack.tag())));
        }
        Way::Converge => {
            let wait = super::secs_env(&ctx.env, "SOT_INSTALL_CONVERGE_WAIT_S", 1800);
            let relaunch = relaunch_script(ctx, installed)?;
            converge(ctx, wait, &relaunch)?;
            ctx.expect_daemon = state != State::None;
            verdict::set_tail(if ctx.named { verdict::pin_tail(&ctx.pack.version) } else { String::new() });
            return Ok(None);
        }
        Way::Quit => {
            return Ok(Some(format!("quit Ship of Tools, choose Keep, and start it from the shortcut; the start applies {}, then run this again to confirm (a converge here could end the box's sessions){work}", ctx.pack.tag())));
        }
        Way::Desk => {
            return Ok(Some(format!("start Ship of Tools at the desk; the start applies {} (nothing was stopped from this session, which is not at the desk){work}", ctx.pack.tag())));
        }
        Way::Here => {}
    }
    // Nothing runs and this is a desktop session: apply here, the daemon stopped first so it pins no stale image.
    step("apply");
    let state = daemon_state(ctx, new_sotd);
    if daemon::apply_here(state.0) == daemon::Apply::Hold {
        return Ok(Some(format!("a daemon runs on {} and could not be told its version ({}); nothing was stopped, and the update to {} stays armed: start Ship of Tools at the desk, and the start applies it{work}", ctx.pipe, state.1, ctx.pack.tag())));
    }
    if daemon::apply_here(state.0) == daemon::Apply::StopFirst {
        let old = if installed.is_empty() { String::new() } else { format!("v{installed}") };
        let stopper = ctx.prefix.join("repo\\versions").join(old).join("scripts\\sot-local-daemon.ps1");
        if stopper.is_file() {
            let _ = script(ctx.m, &stopper, &["-Stop"]);
        } else {
            println!("  note: no sot-local-daemon.ps1 of the installed checkout to stop the daemon with; the apply below goes on");
        }
    }
    let apply = ctx.version_dir.join("scripts\\sot-apply.ps1");
    let ran = script(ctx.m, &apply, &["-Prefix", &ctx.prefix.to_string_lossy()]);
    if !ran.out.contains("sot-apply: APPLIED") && !ran.err.contains("sot-apply: APPLIED") {
        return Err(Stop::new(1, format!("sot-apply.ps1 did not apply {}: {}", ctx.pack.tag(), ran.last_said())));
    }
    verdict::set_tail(if ctx.named { verdict::pin_tail(&ctx.pack.version) } else { String::new() });
    Ok(None)
}

/// The resident supervisor: its pid (`launcher.pid` names a live `launch-sot.ps1`) and the launcher code id its last
/// start line logged (`code=`; rc9.13 and later, empty for an older one, which never re-invokes after a converge).
fn supervisor(log_dir: &Path, all: &[procs::Proc]) -> Option<(u32, String)> {
    let pid: u32 = std::fs::read_to_string(log_dir.join("launcher.pid")).ok()?.lines().next()?.trim().parse().ok()?;
    all.iter().find(|p| p.pid == pid && p.args.to_ascii_lowercase().contains("launch-sot.ps1"))?;
    let log = std::fs::read_to_string(log_dir.join("supervisor.log")).unwrap_or_default();
    Some((pid, super::supervisor::start_code(&log, pid)))
}

/// The installed checkout's `relaunch-sot.ps1`, the converge sentinel's one writer: the installed version's, else the
/// target's.
fn relaunch_script(ctx: &Ctx, installed: &str) -> Result<PathBuf, Stop> {
    [format!("v{installed}"), ctx.pack.tag()]
        .iter()
        .map(|tag| ctx.prefix.join("repo\\versions").join(tag).join("scripts\\relaunch-sot.ps1"))
        .find(|p| p.is_file())
        .ok_or_else(|| Stop::at("apply", 1, format!("no relaunch-sot.ps1 under {}\\repo\\versions to ask the window's supervisor with", ctx.prefix.display())))
}

/// Ask the supervisor to converge and wait for its apply: `sot-apply: APPLIED <tag>`, then `status: DONE`, in its log.
fn converge(ctx: &Ctx, bound: Duration, relaunch: &Path) -> Result<(), Stop> {
    step("apply");
    let log = ctx.log_dir.join("supervisor.log");
    let start = std::fs::metadata(&log).map_or(0, |m| m.len());
    let sentinel = local_app_data(&ctx.env).join("sot\\relaunch.request");
    let q = ctx.m.act(powershell(&["-File"]).arg(relaunch).arg("-Converge"));
    if !q.ok() {
        return Err(Stop::new(1, format!("relaunch-sot.ps1 -Converge ended with exit {}: {}", q.code.map_or("none".into(), |c| c.to_string()), q.last_said())));
    }
    println!("converge requested: waiting for the window's supervisor to apply {} (up to {} s)", ctx.pack.tag(), bound.as_secs());
    super::supervisor::wait_converge(&log, start, &sentinel, &ctx.pack.tag(), bound, &|s| println!("  still waiting ({s} s)")).map_err(|why| Stop::new(1, why))
}

/// This process's logon session: 0 for an ssh logon, a later one at the desk.
fn session_id(m: &dyn Machine) -> Fact<u32> {
    observe::ask(m, &mut powershell(&["-Command", "[System.Diagnostics.Process]::GetCurrentProcess().SessionId"]), ASK_BOUND, |a| {
        a.zero()?.first_line().parse().map_err(|_| observe::Why::Malformed(format!("no session id ('{}')", a.first_line())))
    })
}

/// Folder trust, the junction, the Julia environments and comm: what the launcher's first start does, done here so the
/// install is whole before a window starts.
fn layout_and_comm(ctx: &Ctx, found: &mut Found) -> Result<(), Stop> {
    step("layout");
    let ran = ctx.m.act(Command::new(ctx.sotd()).args(["trust", "declare"]).arg(super::home_dir(&ctx.env).unwrap_or_default()));
    match (ran.code, ran.out.trim()) {
        (Some(0), "Declared") => println!("  folder trust declared"),
        (Some(0), "Kept") => println!("  folder trust kept"),
        (Some(0), _) => println!("  folder trust not declared - unexpected command outcome"),
        (code, _) => println!("  folder trust not declared (exit {}): {}", code.map_or("none".into(), |c| c.to_string()), ran.last_said()),
    }
    let current = ctx.prefix.join("repo\\current");
    if std::fs::symlink_metadata(&current).is_err() {
        let mut c = powershell(&["-Command", "New-Item -ItemType Junction -Path $env:SOT_JUNCTION_LINK -Target $env:SOT_JUNCTION_TARGET -ErrorAction Stop | Out-Null"]);
        c.env("SOT_JUNCTION_LINK", &current).env("SOT_JUNCTION_TARGET", &ctx.version_dir);
        let made = ctx.m.act(&mut c);
        if !made.ok() {
            return Err(Stop::new(1, format!("could not create the junction {} -> {}: {}", current.display(), ctx.version_dir.display(), made.last_said())));
        }
        println!("  created repo\\current -> {}", ctx.version_dir.display());
    }
    if observe::which(ctx.m, "julia").is_none() {
        return Ok(());
    }
    environments(ctx, found, &|checkout, scratch, repair, found| julia::instantiate(ctx.m, checkout, None, scratch, repair, &|t| println!("  {t}"), found));
    step("comm");
    let scratch = ctx.pack.dir.join("tmp");
    std::fs::create_dir_all(&scratch).ok();
    if !julia::update_comm(ctx.m, &ctx.version_dir, None, &scratch) {
        return Err(Stop::new(1, "ShipTools.update_comm() failed"));
    }
    Ok(())
}

/// The Julia environments of a checkout whose kernel was never instantiated, by `instantiate(checkout, scratch, repair)`;
/// a repair keeps the environments' own manifests.
pub(super) fn environments(ctx: &Ctx, found: &mut Found, instantiate: &dyn Fn(&Path, &Path, bool, &mut Found)) {
    if ctx.version_dir.join("julia\\kernel\\Manifest.toml").is_file() {
        return;
    }
    let scratch = ctx.pack.dir.join("tmp");
    std::fs::create_dir_all(&scratch).ok();
    instantiate(&ctx.version_dir, &scratch, ctx.repair, found);
}

/// The Desktop shortcut and the taskbar pin, by the checkout's `install-shortcut.ps1` through `repo\current` where this
/// session can traverse it (the shortcut's working folder is then the junction, which a later apply keeps); over ssh on
/// a box that enforces redirection trust, a shortcut that already targets the junction is kept, else it is a step.
fn shortcut(ctx: &Ctx, found: &mut Found) -> Result<(), Stop> {
    step("shortcut");
    let via = ctx.prefix.join("repo\\current\\scripts\\install-shortcut.ps1");
    if via.is_file() {
        let ran = script(ctx.m, &via, &["-Prefix", &ctx.prefix.to_string_lossy(), "-NoManifest"]);
        if !ran.ok() {
            return Err(Stop::new(1, format!("install-shortcut.ps1 ended with exit {}", ran.code.map_or("none".into(), |c| c.to_string()))));
        }
        return Ok(());
    }
    let lnk = super::home_dir(&ctx.env).unwrap_or_default().join("Desktop\\Ship of Tools.lnk");
    let mut c = powershell(&["-Command", "try { (New-Object -ComObject WScript.Shell).CreateShortcut($env:SOT_SHORTCUT).Arguments } catch { '' }"]);
    c.env("SOT_SHORTCUT", &lnk);
    let read = observe::ask(ctx.m, &mut c, ASK_BOUND, |a| Ok(a.zero()?.out.to_ascii_lowercase().contains(r"repo\current\scripts\launch-sot.ps1")));
    match read.read(Ok, Err) {
        Ok(true) => println!("  shortcut: kept (repo\\current cannot be traversed in this session, and the shortcut already targets it)"),
        Ok(false) => found.step("run this again at the desk, not over ssh: this session cannot traverse repo\\current, so the Desktop shortcut was not written"),
        Err(why) => found.step(format!("run this again at the desk, not over ssh: this session cannot traverse repo\\current, and the Desktop shortcut could not be read ({why})")),
    }
    Ok(())
}

/// The first topology sync from the hub this run names, else the one the record names: the hub that refused this box,
/// when it did.
fn sync_hub(ctx: &Ctx) -> Option<String> {
    step("record");
    let hub = ctx.opts.hub.clone().or_else(|| record::string(&record::read(&ctx.record_path()), "hub")).filter(|h| !h.is_empty());
    let Some(hub) = hub else {
        println!("  note: no -Hub given and none recorded: this box cannot fetch hosts.toml until install.ps1 runs with -Hub <alias>");
        return None;
    };
    let ran = ctx.m.act(Command::new(ctx.sotd()).args(["topology", "sync", "--hub", &hub]));
    let said = format!("{}{}", ran.out, ran.err).split_whitespace().collect::<Vec<_>>().join(" ");
    if ran.ok() {
        println!("  topology sync --hub {hub} ok: {said}");
        return None;
    }
    println!("  topology sync --hub {hub} failed (exit {}): {said}", ran.code.map_or("none".into(), |c| c.to_string()));
    said.contains("does not list this box").then_some(hub)
}

/// The record this run writes: the hub this run names, else the one recorded.
fn record_of(ctx: &Ctx) -> record::Record {
    let hub = ctx.opts.hub.clone().or_else(|| record::string(&record::read(&ctx.record_path()), "hub")).unwrap_or_default();
    let config = ctx.env.get_nonempty("APPDATA").map_or_else(PathBuf::new, PathBuf::from).join("sot");
    record::Record {
        prefix: ctx.prefix.to_string_lossy().into_owned(),
        config: config.to_string_lossy().into_owned(),
        service: "none".into(),
        version: ctx.pack.version.clone(),
        commit: ctx.pack.commit.clone(),
        installed_at: record::now_utc(),
        hub,
        daemon: true,
        frontend: true,
        pinned: ctx.named,
    }
}

fn write_record(ctx: &Ctx) -> Result<(), Stop> {
    step("record");
    let path = ctx.record_path();
    let before = record::read(&path);
    let r = record_of(ctx);
    // A new install is a new transaction: a pointer armed over the one it replaces would apply onto it.
    if record::string(&before, "version").as_deref() != Some(&ctx.pack.version) || record::string(&before, "commit").as_deref() != Some(&ctx.pack.commit) {
        std::fs::remove_file(ctx.pointer()).ok();
    }
    record::write_whole(&path, &record::text_for(&before, &r, ctx.repair)).map_err(|e| Stop::new(1, format!("writing {}: {e}", path.display())))?;
    verdict::set_tail(if ctx.named { verdict::pin_tail(&ctx.pack.version) } else { String::new() });
    Ok(())
}

/// The launcher code id of a scripts folder, asked of that folder's own `Get-SotLauncherCodeId` (`sot-install-layout.ps1`),
/// the one definition the launcher logs at its start; why not, when the ask gave no answer or did not exit 0 (it prints
/// nothing when its hashing fails, which [`supervisor::launcher`] reads as unknown).
fn launcher_code_id(m: &dyn Machine, scripts: &Path) -> Fact<String> {
    let mut c = powershell(&["-Command", ". (Join-Path $env:SOT_SCRIPTS 'sot-install-layout.ps1'); Get-SotLauncherCodeId -ScriptsDir $env:SOT_SCRIPTS"]);
    c.env("SOT_SCRIPTS", scripts);
    observe::ask(m, &mut c, ASK_BOUND, |a| Ok(a.zero()?.first_line()))
}

/// What the run left: the binaries, the record, the junction, no armed pointer, comm and the kernel environment, the
/// window's launcher code, the daemon.
fn end_state(ctx: &Ctx, found: &mut Found) {
    step("check");
    let (v, short) = (&ctx.pack.version, ctx.pack.short());
    for b in ["sotd", "sot"] {
        let exe = ctx.prefix.join("bin").join(format!("{b}.exe"));
        if let Err(said) = super::pack::says(super::pack::version_of(ctx.m, &exe), b, v, &ctx.pack.commit) {
            found.fail("binaries", format!("bin\\{b}.exe {said}, not {v} at {short}"));
        }
    }
    let text = record::read(&ctx.record_path());
    if record::string(&text, "version").as_deref() != Some(v) || record::string(&text, "commit").as_deref() != Some(&ctx.pack.commit) {
        found.fail("record", format!("{} does not name {v} at {short}", ctx.record_path().display()));
    }
    let current = ctx.prefix.join("repo\\current");
    let same = std::fs::read_link(&current).ok().and_then(|t| std::fs::canonicalize(t).ok()).zip(std::fs::canonicalize(&ctx.version_dir).ok()).is_some_and(|(a, b)| a == b);
    if !same {
        found.fail("checkout", format!("{} does not point at {}", current.display(), ctx.version_dir.display()));
    } else if !ctx.version_dir.join("scripts\\launch-sot.ps1").is_file() {
        found.fail("checkout", format!("{} holds no launcher", ctx.version_dir.display()));
    }
    if ctx.pointer().exists() {
        found.fail("pointer", "an update pointer is still armed after this run");
    }
    if observe::which(ctx.m, "julia").is_some() {
        let comm = ctx.env.get_nonempty("SOT_COMM_HOME").map_or_else(|| super::home_dir(&ctx.env).unwrap_or_default().join(".sot-comm"), PathBuf::from);
        let stamp = std::fs::read_to_string(comm.join("VERSION")).unwrap_or_default().trim().trim_end_matches("-dirty").to_string();
        if stamp != short {
            found.fail("comm", format!("agent comm is at '{stamp}', not {short}"));
        }
        let manifest = ctx.version_dir.join("julia\\kernel\\Manifest.toml");
        if !manifest.is_file() {
            found.fail("julia", format!("the kernel environment was never instantiated ({} is missing)", manifest.display()));
        }
    } else {
        found.step("install Julia (winget install --id Julialang.Juliaup -e); agent comm and local REPLs need it, then run this again");
    }
    match procs::snapshot(ctx.m).read(Ok, Err) {
        Ok(all) => {
            if let Some((_, code)) = supervisor(&ctx.log_dir, &all) {
                match super::supervisor::launcher(&code, launcher_code_id(ctx.m, &ctx.version_dir.join("scripts"))) {
                    super::supervisor::Launcher::Current => {}
                    super::supervisor::Launcher::Older => {
                        found.step("quit Ship of Tools, choose Keep, and start it from the shortcut; the window runs an older launcher");
                    }
                    super::supervisor::Launcher::Unknown(why) => found.fail("launcher", format!("the launcher code id of the installed scripts could not be read ({why}), so whether the window runs an older launcher is unknown")),
                }
            }
        }
        Err(why) => found.fail("launcher", format!("could not list this account's processes ({why}), so whether the window's supervisor runs an older launcher is unknown")),
    }
    let sotd = ctx.sotd();
    // After a converge the waiter is the only ask, so the check keeps its bound; else one ask says what runs.
    let (st, why) = if ctx.expect_daemon {
        let bound = super::secs_env(&ctx.env, "SOT_INSTALL_DAEMON_WAIT_S", 60);
        let (st, why) = daemon::wait_current(ctx.m, &sotd, &ctx.endpoint(), bound, false, &|| daemon_runs(ctx));
        if st != State::Current {
            found.fail("daemon", format!("the daemon on {} did not answer as {v} within {} s after the converge (it is '{}': {why})", ctx.pipe, bound.as_secs(), st.word()));
        }
        (st, why)
    } else {
        daemon_state(ctx, &sotd)
    };
    match st {
        State::Unknown => found.fail("daemon", format!("a daemon runs on {} and could not be told its version ({why})", ctx.pipe)),
        State::Other | State::Held => found.fail("daemon", format!("the daemon on {} still runs another version than {v}", ctx.pipe)),
        _ => {}
    }
}

#[cfg(test)]
#[path = "windows_observe_tests.rs"]
mod observe_tests;
