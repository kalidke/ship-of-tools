//! The install on Windows: the steps of `scripts/install.ps1`'s body, in its order. A fresh install lays down `bin\`,
//! `repo\base`, `repo\versions\v<v>` and the `repo\current` junction; an update stages the pack with its own `sotd update
//! stage` and applies it one way, by what runs here: a supervisor that logs its launcher code id gets the converge
//! sentinel, an older supervisor or a lone window leaves it armed for a start, nothing running at the desk applies it
//! here, nothing running over ssh leaves it armed. `repo\current` is read by its target and never traversed, except by
//! the shortcut step, because an ssh session on a box with redirection trust cannot traverse a junction.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::daemon::{self, State};
use super::options::Options;
use super::pack::Pack;
use super::supervisor::Converge;
use super::verdict::{self, step, Found, Stop};
use super::{cmd, julia, procs, record};

const TARGET: &str = "windows-x86_64";

/// What the run works with.
struct Ctx {
    opts: Options,
    prefix: PathBuf,
    pack: Pack,
    named: bool,
    pipe: String,
    log_dir: PathBuf,
    base: PathBuf,
    version_dir: PathBuf,
    /// A converge restarted the daemon: it must answer as the target.
    expect_daemon: bool,
}

impl Ctx {
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

fn local_app_data() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn powershell(args: &[&str]) -> Command {
    let mut c = Command::new("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass"]).args(args);
    c
}

/// Run a script of a checkout and show its lines; its exit code and what it said.
fn script(path: &Path, args: &[&str]) -> cmd::Ran {
    let mut c = powershell(&["-File"]);
    c.arg(path).args(args);
    let ran = cmd::output(&mut c);
    for l in ran.out.lines().chain(ran.err.lines()).filter(|l| !l.trim().is_empty()) {
        println!("  {l}");
    }
    ran
}

pub(super) fn install(opts: Options) -> Result<(Found, String), Stop> {
    step("options");
    let prefix = opts.prefix.as_deref().map_or_else(|| local_app_data().join("sot"), super::absolute);
    step("preflight");
    let arch = std::env::var("PROCESSOR_ARCHITEW6432").or_else(|_| std::env::var("PROCESSOR_ARCHITECTURE")).unwrap_or_default();
    if arch != "AMD64" {
        return Err(Stop::new(2, format!("Ship of Tools needs Windows x86_64 (this is {arch})")));
    }
    if !cmd::on_path("git") {
        return Err(Stop::new(2, "git is not on PATH: winget install --id Git.Git -e"));
    }
    if !cmd::on_path("jq") {
        return Err(Stop::new(1, "sot-comm needs jq, which is not on PATH: install it with `winget install jqlang.jq`, then run this installer again"));
    }
    step("fetch");
    let implicit = super::implicit_run() && !opts.named();
    let named = opts.named() || (super::bootstrap_tag().is_some() && !implicit);
    let want = opts.version.clone().or_else(|| if opts.archive.is_none() { super::bootstrap_tag() } else { None });
    let pack = super::pack::read(&opts.pack, TARGET, want.as_deref()).map_err(|why| Stop::new(1, why))?;
    pack.check_binary("sot").map_err(|why| Stop::new(1, why))?;
    println!("installing Ship of Tools {} into {}", pack.tag(), prefix.display());
    let new_sotd = pack.exe("sotd");
    let pipe = cmd::output(Command::new(&new_sotd).args(["session-socket-path", "local"])).first_line();
    let pipe = if pipe.starts_with(r"\\.\pipe\") { pipe } else { String::new() };
    let version_dir = prefix.join("repo\\versions").join(pack.tag());
    let mut ctx = Ctx {
        base: prefix.join("repo\\base"),
        log_dir: local_app_data().join("sot\\logs"),
        version_dir,
        opts,
        prefix,
        pack,
        named,
        pipe,
        expect_daemon: false,
    };
    let pin = if named { verdict::pin_tail(&ctx.pack.version) } else { String::new() };
    let installed = super::installed_version(&ctx.prefix);
    let mut dir = String::new();
    let way = super::meet(&installed, ctx.sotd().is_file(), || {
        step("direction");
        if implicit && installed.contains('+') {
            let way = super::build_direction(&ctx.prefix, &installed, &ctx.pack.commit);
            return Err(Stop::at("target", 2, super::build_stop(&installed, &ctx.pack.tag(), &way, "-Archive", "-Version")));
        }
        dir = super::direction(&new_sotd, &ctx.pack.version, &ctx.prefix, &installed);
        super::guard_direction(implicit, &installed, &dir, &ctx.pack.version, &ctx.prefix, "-Version")
    })?;
    let pending = match way {
        super::Meet::Update => update_or_repair(&mut ctx, &new_sotd, &installed, &dir)?,
        super::Meet::Fresh => {
            fresh(&ctx)?;
            None
        }
    };
    let mut found = Found::default();
    found.pass = format!("Ship of Tools {} at {} on this computer", ctx.pack.version, ctx.pack.short());
    if let Some(step) = pending {
        // The update stays armed for a start: the run ends with that step.
        found.step(step);
        return Ok((found, pin));
    }
    if julia::which("julia").is_some() {
        step("julia");
        let facts = julia::facts(&ctx.version_dir, None).map_err(|why| Stop::new(1, why))?;
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
    if let Some(hub) = refusal {
        let me = sot_log::host::state_dir::host_name().unwrap_or_else(|_| std::env::var("COMPUTERNAME").unwrap_or_default().to_lowercase());
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
    let origin = super::origin();
    if !ctx.base.join(".git").exists() {
        let _ = std::fs::create_dir_all(ctx.base.parent().unwrap_or(&ctx.base));
        let ran = cmd::output(cmd::git().args(["clone", "--quiet", "--filter=blob:none", "--no-checkout", &origin]).arg(&ctx.base));
        if !ran.ok() {
            return Err(Stop::new(1, format!("could not clone {origin} into {}: {}", ctx.base.display(), ran.last_said())));
        }
    }
    let commit = &ctx.pack.commit;
    if !cmd::output(cmd::git_in(&ctx.base).args(["cat-file", "-e", &format!("{commit}^{{commit}}")])).ok()
        && !cmd::output(cmd::git_in(&ctx.base).args(["fetch", "--quiet", "origin", commit])).ok()
    {
        return Err(Stop::new(1, format!("{commit} is not in {} and could not be fetched from {origin}", ctx.base.display())));
    }
    let tag = ctx.pack.tag();
    if !ctx.pack.version.contains('+') && cmd::output(cmd::git_in(&ctx.base).args(["tag", "-l", &tag])).out.trim().is_empty() {
        let _ = cmd::output(cmd::git_in(&ctx.base).args(["fetch", "--quiet", "origin", &format!("refs/tags/{tag}:refs/tags/{tag}")]));
    }
    Ok(())
}

fn version_folder(ctx: &Ctx) -> Result<(), Stop> {
    let head = |dir: &Path| cmd::output(cmd::git_in(dir).args(["rev-parse", "HEAD"])).first_line();
    if ctx.version_dir.exists() {
        if head(&ctx.version_dir) == ctx.pack.commit {
            return Ok(());
        }
        return Err(Stop::new(1, format!("{} exists but is not at {}; remove that folder and run this again", ctx.version_dir.display(), ctx.pack.short())));
    }
    let _ = cmd::output(cmd::git_in(&ctx.base).args(["worktree", "prune"]));
    let ran = cmd::output(cmd::git_in(&ctx.base).args(["worktree", "add", "--detach"]).arg(&ctx.version_dir).arg(&ctx.pack.commit));
    if !ran.ok() {
        return Err(Stop::new(1, format!("could not add the worktree {} at {}: {}", ctx.version_dir.display(), ctx.pack.short(), ran.last_said())));
    }
    if head(&ctx.version_dir) != ctx.pack.commit {
        return Err(Stop::new(1, format!("{} is not at {}", ctx.version_dir.display(), ctx.pack.commit)));
    }
    Ok(())
}

/// An install already here: never backwards unasked, a repair run when nothing moves, else the update. The step of an
/// update left armed for a start, when it is.
fn update_or_repair(ctx: &mut Ctx, new_sotd: &Path, installed: &str, dir: &str) -> Result<Option<String>, Stop> {
    let same_commit = record::string(&record::read(&ctx.record_path()), "commit").as_deref() == Some(ctx.pack.commit.as_str());
    let (state, _) = daemon_state(ctx, new_sotd);
    if dir == "same" && same_commit && matches!(state, State::None | State::Current) {
        println!("this computer already runs {installed} at {}: a repair run", ctx.pack.short());
        // The pin first, then the pointer armed over the install: no arm lands between.
        write_pin(ctx, ctx.named)?;
        let _ = std::fs::remove_file(ctx.pointer());
        return Ok(None);
    }
    let from = if installed.is_empty() { "a version that cannot be read" } else { installed };
    let how = if dir == "same" { ": same version, another build".to_string() } else { format!(" ({dir})") };
    println!("this run moves the install from {from} to {}{how}", ctx.pack.version);
    update(ctx, new_sotd, installed)
}

/// The daemon's state: its pipe asked; a daemon whose pipe exists or whose program runs and cannot be told is
/// `Unknown`. A window that holds it counts as another version: the converge or the start replaces it.
fn daemon_state(ctx: &Ctx, sotd: &Path) -> (State, String) {
    let runs = pipe_listed(&ctx.pipe) || !procs::daemon_pids(&procs::snapshot(), &ctx.prefix).is_empty();
    if ctx.pipe.is_empty() {
        return (if runs { State::Unknown } else { State::None }, "no pipe name".into());
    }
    let (st, why) = daemon::state(sotd, &ctx.endpoint(), runs);
    (if st == State::Held { State::Other } else { st }, why)
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

/// What a restart of the daemon would end, as the reason of the question; empty when nothing is live or unsure.
fn live_text(state: State, pipe: &str, work: &daemon::Work, why: &str) -> String {
    let is = if state == State::Current { "is running" } else { "runs another version" };
    let rows = work.rows.join(", ");
    if state == State::Unknown {
        let rest = if rows.is_empty() { "; restarting it could end live REPLs and Pluto notebooks".to_string() } else { format!("; restarting it would end the REPLs of: {rows}") };
        return format!("a daemon runs on {pipe} and could not be told its version ({why}){rest}");
    }
    if let Some(reason) = &work.unsure {
        let rest = if rows.is_empty() { String::new() } else { format!(", and restarting it would end the REPLs of: {rows}") };
        return format!("could not ask the daemon on {pipe} in full: it {is}{rest}; sotd update repls: {reason}");
    }
    let t = daemon::work_words(&work.rows, work.pluto, "restarting it would end");
    if t.is_empty() { t } else { format!("the daemon on {pipe} {is}, and {t}") }
}

/// Stage with the pack's own sotd, then apply one way, by what runs here; the step when the update stays armed.
#[allow(clippy::too_many_lines, reason = "install.ps1's update in its order: the question, the stage, then the one way it applies")]
fn update(ctx: &mut Ctx, new_sotd: &Path, installed: &str) -> Result<Option<String>, Stop> {
    step("daemon");
    // Every path below that stops or restarts a daemon asks first, whatever its version.
    let (state, why) = daemon_state(ctx, new_sotd);
    let mut live = String::new();
    if state != State::None {
        let work = daemon::work(new_sotd, &ctx.endpoint());
        let unsure = work.unsure.is_some() || state == State::Unknown;
        let kids = if unsure { daemon::children(&ctx.prefix) } else { None };
        if unsure && kids.as_ref().is_some_and(Vec::is_empty) && work.rows.is_empty() && !work.pluto {
            println!("note: could not ask the daemon on {} in full ({}); it has no REPL or Pluto child, so restarting it ends nothing live", ctx.pipe, work.unsure.as_deref().unwrap_or(&why));
        } else {
            live = live_text(state, &ctx.pipe, &work, &why);
        }
        if !live.is_empty() && !ctx.opts.restart_daemon {
            let (repl, pluto) = kids.as_ref().map_or((false, false), |k| (k.contains(&"repl"), k.contains(&"pluto")));
            if daemon::ask_restart(&daemon::ask_words(&work.rows, repl, work.pluto || pluto, unsure)) {
                println!("note: restarting the daemon as answered");
            } else {
                println!("       Restarting the daemon ends every live REPL and Pluto notebook (their variables are lost); the rows and their agents keep running. Nothing under the install was changed. Ask whoever uses this computer, then run this again with -RestartDaemon.");
                return Err(Stop::new(2, live));
            }
        }
    }
    step("stage");
    if !ctx.base.join(".git").exists() {
        init_base(ctx)?;
        step("stage");
    }
    // The pin goes in before the stage, so no automatic arm lands between the stage and the pin.
    let was_pinned = record::boolean(&record::read(&ctx.record_path()), "pinned") == Some(true);
    write_pin(ctx, ctx.named)?;
    println!("staging and preparing {} (this can take many minutes)", ctx.pack.tag());
    let ran = cmd::output(Command::new(new_sotd).args(["update", "stage"]).arg(&ctx.pack.dir).arg("--prefix").arg(&ctx.prefix));
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
    let all = procs::snapshot();
    let sup = supervisor(&ctx.log_dir, &all);
    let window_only = sup.is_none() && procs::window_runs(&all, &ctx.prefix);
    let work = if live.is_empty() { String::new() } else { format!("; {live}") };
    if sup.as_ref().is_some_and(|(_, code)| !code.is_empty()) {
        let wait = super::secs_env("SOT_INSTALL_CONVERGE_WAIT_S", 1800);
        let relaunch = relaunch_script(ctx, installed)?;
        converge(ctx, wait, &relaunch)?;
        ctx.expect_daemon = state != State::None;
        verdict::set_tail(if ctx.named { verdict::pin_tail(&ctx.pack.version) } else { String::new() });
        return Ok(None);
    }
    if sup.is_some() || window_only {
        return Ok(Some(format!("quit Ship of Tools, choose Keep, and start it from the shortcut; the start applies {}, then run this again to confirm (a converge here could end the box's sessions){work}", ctx.pack.tag())));
    }
    if session_id() == Some(0) {
        return Ok(Some(format!("start Ship of Tools at the desk; the start applies {} (nothing was stopped from this session, which is not at the desk){work}", ctx.pack.tag())));
    }
    // Nothing runs and this is a desktop session: apply here, the daemon stopped first so it pins no stale image.
    step("apply");
    if daemon_state(ctx, new_sotd).0 != State::None {
        let old = if installed.is_empty() { String::new() } else { format!("v{installed}") };
        let stopper = ctx.prefix.join("repo\\versions").join(old).join("scripts\\sot-local-daemon.ps1");
        if stopper.is_file() {
            let _ = script(&stopper, &["-Stop"]);
        } else {
            println!("  note: no sot-local-daemon.ps1 of the installed checkout to stop the daemon with; the apply below goes on");
        }
    }
    let apply = ctx.version_dir.join("scripts\\sot-apply.ps1");
    let ran = script(&apply, &["-Prefix", &ctx.prefix.to_string_lossy()]);
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
    let sentinel = local_app_data().join("sot\\relaunch.request");
    let q = cmd::output(powershell(&["-File"]).arg(relaunch).arg("-Converge"));
    if !q.ok() {
        return Err(Stop::new(1, format!("relaunch-sot.ps1 -Converge ended with exit {}: {}", q.code.map_or("none".into(), |c| c.to_string()), q.last_said())));
    }
    println!("converge requested: waiting for the window's supervisor to apply {} (up to {} s)", ctx.pack.tag(), bound.as_secs());
    let t0 = Instant::now();
    let mut last_note = t0;
    let outcome = loop {
        if t0.elapsed() >= bound {
            break Err(Stop::new(1, format!("the supervisor did not report {} applied within {} s: see {}", ctx.pack.tag(), bound.as_secs(), log.display())));
        }
        std::thread::sleep(Duration::from_secs(1));
        let text = std::fs::read(&log).map(|b| String::from_utf8_lossy(&b[usize::try_from(start).unwrap_or(0).min(b.len())..]).into_owned()).unwrap_or_default();
        match super::supervisor::converge_state(&text, &ctx.pack.tag()) {
            Converge::Done => break Ok(()),
            Converge::Restored => break Err(Stop::new(1, format!("the supervisor's apply did not complete: see {}", log.display()))),
            Converge::Waiting => {}
        }
        if last_note.elapsed() >= Duration::from_secs(30) {
            last_note = Instant::now();
            println!("  still waiting ({} s)", t0.elapsed().as_secs());
        }
    };
    // A sentinel nobody consumed would make the next window converge the moment it starts.
    let _ = std::fs::remove_file(&sentinel);
    outcome
}

/// This process's logon session: 0 for an ssh logon, a later one at the desk.
fn session_id() -> Option<u32> {
    cmd::output(&mut powershell(&["-Command", "[System.Diagnostics.Process]::GetCurrentProcess().SessionId"])).first_line().parse().ok()
}

/// Folder trust, the junction, the Julia environments and comm: what the launcher's first start does, done here so the
/// install is whole before a window starts.
fn layout_and_comm(ctx: &Ctx, found: &mut Found) -> Result<(), Stop> {
    step("layout");
    let ran = cmd::output(Command::new(ctx.sotd()).args(["trust", "declare"]).arg(super::home_dir().unwrap_or_default()));
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
        let made = cmd::output(&mut c);
        if !made.ok() {
            return Err(Stop::new(1, format!("could not create the junction {} -> {}: {}", current.display(), ctx.version_dir.display(), made.last_said())));
        }
        println!("  created repo\\current -> {}", ctx.version_dir.display());
    }
    if julia::which("julia").is_none() {
        return Ok(());
    }
    if !ctx.version_dir.join("julia\\kernel\\Manifest.toml").is_file() {
        let scratch = ctx.pack.dir.join("tmp");
        let _ = std::fs::create_dir_all(&scratch);
        for failed in julia::instantiate(&ctx.version_dir, None, &scratch, &|t| println!("  {t}")) {
            found.fail("julia", failed);
        }
    }
    step("comm");
    let scratch = ctx.pack.dir.join("tmp");
    let _ = std::fs::create_dir_all(&scratch);
    if !julia::update_comm(&ctx.version_dir, None, &scratch) {
        return Err(Stop::new(1, "ShipTools.update_comm() failed"));
    }
    Ok(())
}

/// The Desktop shortcut and the taskbar pin, by the checkout's `install-shortcut.ps1` through `repo\current` where this
/// session can traverse it (the shortcut's working folder is then the junction, which a later apply keeps); over ssh on
/// a box that enforces redirection trust, a shortcut that already targets the junction is kept, else it is a step.
fn shortcut(ctx: &Ctx, found: &mut Found) -> Result<(), Stop> {
    step("shortcut");
    let via = ctx.prefix.join("repo\\current\\scripts\\install-shortcut.ps1");
    if via.is_file() {
        let ran = script(&via, &["-Prefix", &ctx.prefix.to_string_lossy(), "-NoManifest"]);
        if !ran.ok() {
            return Err(Stop::new(1, format!("install-shortcut.ps1 ended with exit {}", ran.code.map_or("none".into(), |c| c.to_string()))));
        }
        return Ok(());
    }
    let lnk = super::home_dir().unwrap_or_default().join("Desktop\\Ship of Tools.lnk");
    let mut c = powershell(&["-Command", "try { (New-Object -ComObject WScript.Shell).CreateShortcut($env:SOT_SHORTCUT).Arguments } catch { '' }"]);
    c.env("SOT_SHORTCUT", &lnk);
    if cmd::output(&mut c).out.to_ascii_lowercase().contains(r"repo\current\scripts\launch-sot.ps1") {
        println!("  shortcut: kept (repo\\current cannot be traversed in this session, and the shortcut already targets it)");
    } else {
        found.step("run this again at the desk, not over ssh: this session cannot traverse repo\\current, so the Desktop shortcut was not written");
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
    let ran = cmd::output(Command::new(ctx.sotd()).args(["topology", "sync", "--hub", &hub]));
    let said = format!("{}{}", ran.out, ran.err).split_whitespace().collect::<Vec<_>>().join(" ");
    if ran.ok() {
        println!("  topology sync --hub {hub} ok: {said}");
        return None;
    }
    println!("  topology sync --hub {hub} failed (exit {}): {said}", ran.code.map_or("none".into(), |c| c.to_string()));
    said.contains("does not list this box").then_some(hub)
}

fn write_record(ctx: &Ctx) -> Result<(), Stop> {
    step("record");
    let path = ctx.record_path();
    let before = record::read(&path);
    let hub = ctx.opts.hub.clone().or_else(|| record::string(&before, "hub")).unwrap_or_default();
    let config = std::env::var_os("APPDATA").map_or_else(PathBuf::new, PathBuf::from).join("sot");
    let r = record::Record {
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
    };
    // A new install is a new transaction: a pointer armed over the one it replaces would apply onto it.
    if record::string(&before, "version").as_deref() != Some(&ctx.pack.version) || record::string(&before, "commit").as_deref() != Some(&ctx.pack.commit) {
        let _ = std::fs::remove_file(ctx.pointer());
    }
    record::write_whole(&path, &record::text(&r)).map_err(|e| Stop::new(1, format!("writing {}: {e}", path.display())))?;
    verdict::set_tail(if ctx.named { verdict::pin_tail(&ctx.pack.version) } else { String::new() });
    Ok(())
}

/// The launcher code id of a scripts folder, as `Get-SotLauncherCodeId` computes it: the SHA-256 of `launch-sot.ps1` and
/// of each file it sources, upper-case hex joined with `-`; empty when one cannot be read.
fn launcher_code_id(scripts: &Path) -> String {
    let mut ids = Vec::new();
    for f in ["launch-sot.ps1", "sot-hosts.ps1", "sot-install-layout.ps1", "sot-freshness.ps1", "sot-lease.ps1"] {
        let Ok(bytes) = std::fs::read(scripts.join(f)) else { return String::new() };
        ids.push(sot_updater::fetch::hash::sha256_bytes(&bytes).to_ascii_uppercase());
    }
    ids.join("-")
}

/// What the run left: the binaries, the record, the junction, no armed pointer, comm and the kernel environment, the
/// window's launcher code, the daemon.
fn end_state(ctx: &Ctx, found: &mut Found) {
    step("check");
    let (v, short) = (&ctx.pack.version, ctx.pack.short());
    for b in ["sotd", "sot"] {
        let exe = ctx.prefix.join("bin").join(format!("{b}.exe"));
        let line = cmd::output(Command::new(&exe).arg("--version")).first_line();
        if !super::pack::binary_says(&line, b, v, &ctx.pack.commit) {
            found.fail("binaries", format!("bin\\{b}.exe reports '{line}', not {v} at {short}"));
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
    if julia::which("julia").is_some() {
        let comm = std::env::var_os("SOT_COMM_HOME").map_or_else(|| super::home_dir().unwrap_or_default().join(".sot-comm"), PathBuf::from);
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
    if let Some((_, code)) = supervisor(&ctx.log_dir, &procs::snapshot()) {
        let want = launcher_code_id(&ctx.version_dir.join("scripts"));
        if !want.is_empty() && code != want {
            found.step("quit Ship of Tools, choose Keep, and start it from the shortcut; the window runs an older launcher");
        }
    }
    let sotd = ctx.sotd();
    let (mut st, why) = daemon_state(ctx, &sotd);
    if ctx.expect_daemon {
        let bound = super::secs_env("SOT_INSTALL_DAEMON_WAIT_S", 60);
        let runs = st != State::None;
        st = daemon::wait_current(&sotd, &ctx.endpoint(), bound, false, runs);
        if st != State::Current {
            found.fail("daemon", format!("the daemon on {} did not answer as {v} within {} s after the converge (it is '{}')", ctx.pipe, bound.as_secs(), st.word()));
        }
    }
    match st {
        State::Unknown => found.fail("daemon", format!("a daemon runs on {} and could not be told its version ({why})", ctx.pipe)),
        State::Other | State::Held => found.fail("daemon", format!("the daemon on {} still runs another version than {v}", ctx.pipe)),
        _ => {}
    }
}
