//! `sotd topology apply`, `refresh` and `pin`: the systemd --user files sotd writes into the user's systemd folder,
//! converged with hosts.toml (the hub's relay units and drop-ins, and the host pins); their text comes from
//! `sot_protocol::topology`.

use super::cli::self_host;
use sot_protocol::topology::{self, Topology};
use std::path::{Path, PathBuf};

/// `sotd topology apply` (plan §C, §F step 5): on the hub, converge the
/// host pins (`converge_hub_pins`) and both systemd --user unit families
/// with the declared list —
/// `sot-relay-tunnel@<host>` for the comm relay's reverse tunnels, and
/// `sot-host-relay-<host>.socket` (plus the per-connection
/// `sot-host-relay-<host>@.service`) for the hub's own socket per dialable
/// host — files apply WRITES from `topology::relay_units::relay_files`, since their text is per host.
/// Refuses off the hub (`topology::relay_units::require_hub`). `dry_run` prints every
/// action without touching systemd or the filesystem — the default (ADR
/// 0028 units forward a live relay port; this box silently flipping which
/// remotes it tunnels to is not something `apply` does on its own say-so).
/// Pass `--yes` to actually run it.
pub(super) fn apply(topo: &Topology, dry_run: bool) -> Result<(), String> {
    let me = self_host()?;
    topology::relay_units::require_hub(topo, &me, "apply")?;
    let plan = topology::relay_units::apply_plan(topo, &enabled_hosts(TUNNEL_TEMPLATE)?, &enabled_hosts(RELAY_TEMPLATE)?);
    let mut failures: Vec<String> = Vec::new();
    // The pins first, on every run: a unit enabled below never starts without its pin, so a pin that cannot be
    // written stops the run before any enable; a run whose plan is empty still heals a pin that differs.
    let pins_changed = converge_hub_pins(topo, &systemd_user_dir()?, &topology::relay_units::relay_hosts(topo), dry_run, &mut |l| println!("{l}"))
        .map_err(|e| format!("the host pins could not be written, so nothing was enabled: {e}"))?;
    if pins_changed && !dry_run {
        if let Err(e) = run_systemctl(&["--user", "daemon-reload"]) {
            failures.push(e);
        }
    }
    if plan.is_empty() && failures.is_empty() {
        if pins_changed && dry_run {
            println!("(dry run — pass --yes to apply)");
            return Ok(());
        }
        println!(
            "up to date: {} tunnel instance(s), {} relay instance(s)",
            topology::relay_units::tunnel_hosts(topo).len(),
            topology::relay_units::relay_hosts(topo).len()
        );
        return Ok(());
    }
    let verb = if dry_run { "would " } else { "" };
    // `generated` is the difference between the two families: the reverse
    // tunnel rides ONE hand-installed `sot-relay-tunnel@.service` template,
    // while the relay's unit text is per host (`topology::relay_units::relay_unit` says
    // why systemd leaves no choice), so apply writes the pair before
    // enabling it and takes it away again on disable.
    // BEST EFFORT, not all-or-nothing: one host that is powered off or
    // refusing ssh must not stop every other host being converged. A `?`
    // here aborted the whole fleet on the first failing unit -- including
    // work for hosts that were fine -- and left the operator to guess how
    // far it got. Each unit's failure is reported as it happens, the rest
    // still run, and the aggregate is returned at the end so the exit
    // status is still honest.
    for (diff, unit, generated) in [
        (&plan.tunnels, topology::relay_units::tunnel_unit as fn(&str) -> String, false),
        (&plan.relays, topology::relay_units::relay_unit as fn(&str) -> String, true),
    ] {
        for h in &diff.enable {
            println!("{verb}enable {}", unit(h));
            if !dry_run {
                let step = || -> Result<(), String> {
                    if generated {
                        write_relay_units(h)?;
                    }
                    run_systemctl(&["--user", "daemon-reload"])?;
                    run_systemctl(&["--user", "enable", "--now", &unit(h)])
                };
                if let Err(e) = step() {
                    eprintln!("  FAILED {}: {e}", unit(h));
                    failures.push(format!("{}: {e}", unit(h)));
                }
            }
        }
        for h in &diff.disable {
            println!("{verb}disable {}", unit(h));
            if !dry_run {
                let step = || -> Result<(), String> {
                    run_systemctl(&["--user", "disable", "--now", &unit(h)])?;
                    if generated {
                        remove_relay_units(h)?;
                    }
                    Ok(())
                };
                if let Err(e) = step() {
                    eprintln!("  FAILED {}: {e}", unit(h));
                    failures.push(format!("{}: {e}", unit(h)));
                }
            }
        }
    }
    if !failures.is_empty() {
        return Err(format!(
            "{} unit(s) failed; every other unit in the plan was applied:\n  {}",
            failures.len(),
            failures.join("\n  ")
        ));
    }
    if dry_run {
        println!("(dry run — pass --yes to apply)");
    }
    Ok(())
}

/// `(prefix, suffix)` around the host name in each family's unit names —
/// `sot-relay-tunnel@<host>.service` and `sot-host-relay-<host>.socket`.
/// Both halves are needed because only one family is an `@` template
/// (`topology::relay_units::relay_unit`).
const TUNNEL_TEMPLATE: (&str, &str) = ("sot-relay-tunnel@", ".service");
const RELAY_TEMPLATE: (&str, &str) = ("sot-host-relay-", ".socket");

/// A `systemctl` command. A test names its own program in `tests::STUB_SYSTEMCTL`, so none changes the process `PATH`.
fn systemctl() -> std::process::Command {
    #[cfg(test)]
    if let Some(program) = tests::STUB_SYSTEMCTL.lock().unwrap().clone() {
        return std::process::Command::new(program);
    }
    std::process::Command::new("systemctl")
}

/// Host names of every unit in one family systemd --user currently
/// reports `enabled`. The only I/O `apply` does to read state;
/// `topology::relay_units::apply_plan` is the pure decision made from its result.
fn enabled_hosts((prefix, suffix): (&str, &str)) -> Result<Vec<String>, String> {
    let pattern = format!("{prefix}*{suffix}");
    let mut cmd = systemctl();
    cmd.args(["--user", "list-unit-files", &pattern, "--no-legend", "--no-pager"]);
    let out = crate::lifecycle::child_signal::process()
        .output(&mut cmd)
        .map_err(|e| format!("systemctl --user list-unit-files: {e}"))?;
    if !out.status.success() {
        // systemd exits 1 when a PATTERN matched no unit files, with
        // nothing on stderr -- which is the ordinary state of a family
        // before its first host is enrolled, not a failure. Treating it
        // as one made `apply` refuse on exactly the bootstrap it exists
        // to perform, and report it as an error naming nothing.
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        if err.is_empty() {
            return Ok(Vec::new());
        }
        return Err(format!("systemctl --user list-unit-files: {err}"));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let unit = w.next()?;
            if w.next()? != "enabled" {
                return None;
            }
            unit.strip_prefix(prefix)?.strip_suffix(suffix).filter(|h| !h.is_empty()).map(str::to_string)
        })
        .collect())
}

/// Writes every file `topology::relay_units::relay_files` names for one host, overwriting
/// whatever was there: they are apply's, and the header in each says so.
fn write_relay_units(host: &str) -> Result<(), String> {
    let dir = systemd_user_dir()?;
    for (name, text) in topology::relay_units::relay_files(host) {
        write_atomic(&dir.join(name), &text)?;
    }
    Ok(())
}

/// Temp file in the same directory, then rename: a reader (systemd) never
/// sees half a unit file. Creates the directory first (a drop-in's `.d/`).
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| io(d, e))?;
    }
    let tmp = PathBuf::from(format!("{}.tmp.{}", path.display(), std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io(path, e)
    })
}

/// The hub keeps its generated relay units equal to this binary's text. For
/// every host that is both a relay host and enabled: rewrite each of
/// `topology::relay_units::relay_files` whose text differs (the command drop-in comes last,
/// so no other drop-in is read),
/// `reset-failed` every host before any reload (accumulated failed
/// instances are what wedges `daemon-reload`), then always reload and
/// restart every enabled relay socket: the files on disk are no record of
/// what systemd applied, so a run after a failed reload or restart redoes it,
/// and a socket restart leaves running bridges alone and revives one stopped
/// by `trigger-limit-hit`. `say` hears each
/// rewrite the moment it is done, so a later failure cannot
/// hide a change already on disk. Only the reload is fatal; other failures are collected and returned at the end, like
/// `apply`.
fn refresh(
    topo: &Topology,
    me: &str,
    dir: &Path,
    enabled: &[String],
    systemctl: &mut dyn FnMut(&[&str]) -> Result<String, String>,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    topology::relay_units::require_hub(topo, me, "refresh")?;
    let hosts: Vec<&str> = topology::relay_units::relay_hosts(topo).into_iter().filter(|h| enabled.iter().any(|e| e == h)).collect();
    let mut failures: Vec<String> = Vec::new();
    let pinned = converge_hub_pins(topo, dir, &hosts, false, say).unwrap_or_else(|e| {
        failures.push(e);
        false
    });
    if hosts.is_empty() {
        if pinned {
            systemctl(&["--user", "daemon-reload"]).map_err(|e| format!("daemon-reload failed: {e}"))?;
        }
        return match failures.is_empty() {
            true => Ok(()),
            false => Err(format!("{} relay refresh step(s) failed:\n  {}", failures.len(), failures.join("\n  "))),
        };
    }
    for &h in &hosts {
        for (name, text) in topology::relay_units::relay_files(h) {
            let path = dir.join(&name);
            if std::fs::read_to_string(&path).is_ok_and(|now| now == text) {
                continue;
            }
            match write_atomic(&path, &text) {
                Ok(()) => say(format!("rewrote {}", path.display())),
                Err(e) => failures.push(e),
            }
        }
    }
    for &h in &hosts {
        let dd = dir.join(format!("{}.d", topology::relay_units::relay_service_unit_file(h)));
        let Ok(entries) = std::fs::read_dir(&dd) else { continue };
        let mut late: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter(|e| e.file_name().to_str().is_some_and(|n| n.ends_with(".conf") && n > topology::relay_units::RELAY_COMMAND_DROPIN))
            .map(|e| e.path())
            .collect();
        late.sort();
        for path in late {
            let mut retired = path.clone().into_os_string();
            retired.push(".retired");
            match std::fs::rename(&path, &retired) {
                Ok(()) => say(format!("retired {}: it sorts after the hub's command drop-in, so it would override the relay command", path.display())),
                Err(e) => failures.push(format!("retire {}: {e}", path.display())),
            }
        }
    }
    for &h in &hosts {
        let (socket, instances) = (topology::relay_units::relay_unit(h), format!("sot-host-relay-{h}@*.service"));
        if let Err(e) = systemctl(&["--user", "reset-failed", &socket, &instances]) {
            failures.push(e);
        }
    }
    systemctl(&["--user", "daemon-reload"]).map_err(|e| format!("daemon-reload failed: {e}"))?;
    for &h in &hosts {
        let unit = format!("sot-host-relay-{h}@refresh-check.service");
        match systemctl(&["--user", "show", "-p", "ExecStart", "-p", "LoadState", &unit]) {
            Ok(out) if runs_generated_command(&out) => {}
            Ok(out) => failures.push(format!(
                "relay command for {h} is overridden outside the hub's directory, or the unit does not load; systemd reports: {}; check `systemctl --user show -p DropInPaths {unit}`",
                out.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(300).collect::<String>()
            )),
            Err(e) => failures.push(e),
        }
    }
    for &h in &hosts {
        if let Err(e) = systemctl(&["--user", "restart", &topology::relay_units::relay_unit(h)]) {
            failures.push(e);
        }
    }
    if !failures.is_empty() {
        return Err(format!("{} relay refresh step(s) failed:\n  {}", failures.len(), failures.join("\n  ")));
    }
    Ok(())
}

/// Whether `systemctl show -p ExecStart -p LoadState` says systemd will run exactly the hub's command: one
/// `ExecStart=` record, its argv field the generated line, and the unit loaded. systemd 249 (measured on a scratch
/// unit) prints each command as `ExecStart={ path=… ; argv[]=<as written, variables unexpanded> ; … }` on a line of
/// its own, and a second command that a drop-in appends leaves a second record and `LoadState=bad-setting`.
fn runs_generated_command(show: &str) -> bool {
    let want = format!("argv[]={} ;", topology::relay_units::relay_command_line());
    let execs: Vec<&str> = show.lines().filter(|l| l.starts_with("ExecStart=")).collect();
    execs.len() == 1 && execs[0].contains(&want) && show.lines().any(|l| l.trim_end() == "LoadState=loaded")
}

/// `sotd topology refresh`: [`refresh`] on the real directory, each action
/// printed as it is done.
pub(super) fn refresh_cmd(topo: &Topology) -> Result<(), String> {
    let me = self_host()?;
    refresh(topo, &me, &systemd_user_dir()?, &enabled_hosts(RELAY_TEMPLATE)?, &mut |a| systemctl_stdout(a), &mut |l| println!("{l}"))
}

/// The daemon's own call at start (main.rs). Gates, cheapest first: hosts.toml
/// loads, this box is the hub, and this process is `sotd.service`'s MainPID.
/// The first two return silently, the third logs one line. The daemon never
/// waits on this.
#[cfg(target_os = "linux")]
pub fn refresh_at_start() {
    let Ok(Some((_, topo))) = topology::load() else { return };
    let Ok(me) = self_host() else { return };
    if !super::cli::self_is_hub(&me, &topo.hub) {
        return;
    }
    match supervised_by_systemd() {
        Ok(true) => {}
        Ok(false) => return tracing::info!("relay refresh skipped: this sotd is not {DAEMON_UNIT}'s main process; `sotd topology refresh` does it by hand"),
        Err(e) => return tracing::warn!(error = %e, "relay refresh skipped"),
    }
    let dir = match systemd_user_dir() {
        Ok(d) => d,
        Err(e) => return tracing::warn!(error = %e, "relay refresh skipped"),
    };
    let enabled = match enabled_hosts(RELAY_TEMPLATE) {
        Ok(v) => v,
        Err(e) => return tracing::warn!(error = %e, "relay refresh skipped"),
    };
    if let Err(e) = refresh(&topo, &me, &dir, &enabled, &mut |a| systemctl_stdout(a), &mut |l| tracing::warn!("{l}")) {
        tracing::warn!(error = %e, "relay refresh failed");
    }
}

/// The unit deploy/sotd.service installs. Its ExecStart `exec`s sotd, so
/// its MainPID is the launched process: the daemon's lifetime guard.
#[cfg(target_os = "linux")]
const DAEMON_UNIT: &str = "sotd.service";

/// True when `systemctl --user show -p MainPID --value` printed `me`: the
/// one test that this process is the daemon systemd supervises. An
/// inherited INVOCATION_ID is no such test: every child of the unit has
/// it, each session's shell included. A stopped or missing unit prints 0.
#[cfg(target_os = "linux")]
fn is_main_pid(show: &str, me: u32) -> bool {
    show.trim().parse::<u32>() == Ok(me)
}

#[cfg(target_os = "linux")]
fn supervised_by_systemd() -> Result<bool, String> {
    let what = format!("systemctl --user show -p MainPID --value {DAEMON_UNIT}");
    let mut cmd = systemctl();
    cmd.args(["--user", "show", "-p", "MainPID", "--value", DAEMON_UNIT]);
    let out = crate::lifecycle::child_signal::process().output(&mut cmd).map_err(|e| format!("{what}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{what}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    // The unit's main process is the lifetime guard that supervises this daemon, else the daemon itself.
    let me = crate::lifecycle::daemon_children::guard::guard_pid().unwrap_or_else(std::process::id);
    Ok(is_main_pid(&String::from_utf8_lossy(&out.stdout), me))
}

/// Removes every generated file for a host, then the service's `.d/` if that
/// left it empty (a hand-made drop-in keeps it). A missing file is not an error
/// — the disable that precedes this is the operation that mattered.
fn remove_relay_units(host: &str) -> Result<(), String> {
    let dir = systemd_user_dir()?;
    for (name, _) in topology::relay_units::relay_files(host) {
        let _ = std::fs::remove_file(dir.join(name));
    }
    let _ = std::fs::remove_dir(dir.join(format!("{}.d", topology::relay_units::relay_service_unit_file(host))));
    remove_pin(&dir, &format!("{}.d/{}", topology::relay_units::relay_unit(host), topology::relay_units::PIN_FILE), false, &mut |_| {})?;
    Ok(())
}

/// `$XDG_CONFIG_HOME/systemd/user`, else `~/.config/systemd/user`: the folder a user manager reads with that
/// environment. `scripts/install.sh` writes `sotd.service` under `$HOME/.config/systemd/user` whatever
/// `$XDG_CONFIG_HOME` says, so it names that folder to `sotd topology pin --dir` rather than leave this rule to
/// choose.
fn systemd_user_dir() -> Result<PathBuf, String> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("systemd").join("user"))
        .ok_or_else(|| "no $XDG_CONFIG_HOME or $HOME: can't find ~/.config/systemd/user".to_string())
}

fn run_systemctl(args: &[&str]) -> Result<(), String> {
    systemctl_stdout(args).map(|_| ())
}

/// `systemctl args`: its stdout on success.
fn systemctl_stdout(args: &[&str]) -> Result<String, String> {
    let mut cmd = systemctl();
    cmd.args(args);
    let out = crate::lifecycle::child_signal::process()
        .output(&mut cmd)
        .map_err(|e| format!("systemctl {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!("systemctl {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Writes each pin in `pins` (path under `dir`, text) whose text differs, through a temp file and a rename, and says
/// each; `dry_run` only says what it would write. True when a pin changed, so the caller reloads once.
fn converge_pins(dir: &Path, pins: &[(String, String)], dry_run: bool, say: &mut dyn FnMut(String)) -> Result<bool, String> {
    let mut changed = false;
    for (name, text) in pins {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).is_ok_and(|now| now == *text) {
            continue;
        }
        if dry_run {
            say(format!("would write {}", path.display()));
            continue;
        }
        write_atomic(&path, text)?;
        say(format!("wrote {}", path.display()));
        changed = true;
    }
    Ok(changed)
}

/// Removes the pin at `name` under `dir`, then its folder when that left it empty (a hand-made drop-in beside it
/// keeps the folder). True when a pin was removed.
fn remove_pin(dir: &Path, name: &str, dry_run: bool, say: &mut dyn FnMut(String)) -> Result<bool, String> {
    let path = dir.join(name);
    if std::fs::symlink_metadata(&path).is_err() {
        return Ok(false);
    }
    if dry_run {
        say(format!("would remove {}", path.display()));
        return Ok(false);
    }
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(folder) = path.parent() {
        let _ = std::fs::remove_dir(folder);
    }
    say(format!("removed {}", path.display()));
    Ok(true)
}

/// The hub's pins under `dir`: sotd.service's, the reverse tunnels' template pin (always: a tunnel instance still
/// enabled after the topology stopped wanting it must not start on another host) and each relay socket's in `relays`,
/// written when their text differs. True when anything changed.
fn converge_hub_pins(topo: &Topology, dir: &Path, relays: &[&str], dry_run: bool, say: &mut dyn FnMut(String)) -> Result<bool, String> {
    let mut pins = vec![topology::relay_units::daemon_pin(topo)];
    pins.extend(topology::relay_units::hub_pins(topo, relays));
    converge_pins(dir, &pins, dry_run, say)
}

/// `sotd topology pin --dir <dir>`: sotd.service's pin under `dir`, from this box's hosts.toml, written when its text
/// differs; removed when there is no hosts.toml, since a lone box runs its own daemon. An invalid hosts.toml is an
/// error and changes nothing. Runs no systemctl: the installer and the update apply reload after it.
pub(super) fn pin_daemon(dir: &Path, say: &mut dyn FnMut(String)) -> Result<(), String> {
    match topology::load()? {
        Some((_, topo)) => converge_pins(dir, &[topology::relay_units::daemon_pin(&topo)], false, say).map(|_| ()),
        None => remove_pin(dir, &format!("sotd.service.d/{}", topology::relay_units::PIN_FILE), false, say).map(|_| ()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STALE_SERVICE: &str = include_str!("../../tests/fixtures/relay/relay-service-rc9.8.unit");
    const NO_MUX: &str = include_str!("../../tests/fixtures/relay/relay-dropin-no-mux.conf");
    const OVERRIDE: &str = include_str!("../../tests/fixtures/relay/relay-dropin-override.conf");

    const SHOW: &str = "show -p ExecStart -p LoadState sot-host-relay-remote-a@refresh-check.service";

    /// The stand-in answer to `show`: the generated command, as systemd prints it.
    fn shown(a: &[&str]) -> String {
        if a[1] == "show" { format!("ExecStart={{ argv[]={} ; }}\nLoadState=loaded\n", topology::relay_units::relay_command_line()) } else { String::new() }
    }

    fn hub_topo() -> Topology {
        topology::parse("hub = \"hub-box\"\n[host.hub-box]\ndaemon = true\n[host.remote-a]\ndaemon = true\n").unwrap()
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sot-f3-refresh-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sot-host-relay-remote-a@.service.d")).unwrap();
        d
    }

    #[test]
    fn runs_generated_command_needs_one_record_that_is_ours_and_a_loaded_unit() {
        let ours = format!("ExecStart={{ path=/usr/bin/ssh ; argv[]={} ; ignore_errors=no ; pid=0 }}", topology::relay_units::relay_command_line());
        let old = "ExecStart={ path=/usr/bin/ssh ; argv[]=/usr/bin/ssh -T x sotd stdio-bridge --label local ; ignore_errors=no ; pid=0 }";
        let longer = format!("ExecStart={{ path=/usr/bin/ssh ; argv[]={} --label local ; ignore_errors=no ; pid=0 }}", topology::relay_units::relay_command_line());
        assert!(runs_generated_command(&format!("{ours}\nLoadState=loaded\n")));
        for bad in [
            format!("{old}\nLoadState=loaded\n"),
            format!("{ours}\n{old}\nLoadState=bad-setting\n"),
            format!("{old}\n{ours}\nLoadState=bad-setting\n"),
            format!("{ours}\nLoadState=bad-setting\n"),
            format!("{ours}\nLoadState=masked\n"),
            format!("{longer}\nLoadState=loaded\n"),
            format!("{ours}\n"),
            "LoadState=loaded\n".to_string(),
        ] {
            assert!(!runs_generated_command(&bad), "{bad}");
        }
    }

    #[test]
    fn refresh_writes_and_reloads_in_order() {
        let topo = hub_topo();
        let dir = scratch("order");
        let dd = dir.join("sot-host-relay-remote-a@.service.d");
        std::fs::write(dir.join("sot-host-relay-remote-a.socket"), topology::relay_units::relay_socket_unit("remote-a")).unwrap();
        std::fs::write(dir.join("sot-host-relay-remote-a@.service"), STALE_SERVICE).unwrap();
        std::fs::write(dd.join("no-mux.conf"), NO_MUX).unwrap();
        std::fs::write(dd.join("override.conf"), OVERRIDE).unwrap();
        let enabled = vec!["remote-a".to_string()];

        let mut calls: Vec<String> = Vec::new();
        let mut said: Vec<String> = Vec::new();
        refresh(&topo, "hub-box", &dir, &enabled, &mut |a| {
            calls.push(a[1..].join(" "));
            Ok(shown(a))
        }, &mut |l| said.push(l))
        .unwrap();
        // Each path as refresh joins it: dir plus relay_files' name, whose '/' a Windows join keeps.
        assert_eq!(
            said,
            [
                format!("wrote {}", dir.join("sotd.service.d/topology.conf").display()),
                format!("wrote {}", dir.join("sot-relay-tunnel@.service.d/topology.conf").display()),
                format!("wrote {}", dir.join("sot-host-relay-remote-a.socket.d/topology.conf").display()),
                format!("rewrote {}", dir.join("sot-host-relay-remote-a@.service").display()),
                format!("rewrote {}", dir.join("sot-host-relay-remote-a@.service.d/zz-sot-relay-command.conf").display())
            ]
        );
        assert_eq!(std::fs::read_to_string(dir.join("sot-host-relay-remote-a@.service")).unwrap(), topology::relay_units::relay_service_unit("remote-a"));
        assert_eq!(std::fs::read_to_string(dd.join("no-mux.conf")).unwrap(), NO_MUX);
        assert_eq!(std::fs::read_dir(&dd).unwrap().count(), 3);
        assert_eq!(std::fs::read_to_string(dd.join("override.conf")).unwrap(), OVERRIDE);
        assert_eq!(
            calls,
            ["reset-failed sot-host-relay-remote-a.socket sot-host-relay-remote-a@*.service", "daemon-reload", SHOW, "restart sot-host-relay-remote-a.socket"]
        );

        calls.clear();
        said.clear();
        refresh(&topo, "hub-box", &dir, &enabled, &mut |a| {
            calls.push(a[1..].join(" "));
            Ok(shown(a))
        }, &mut |l| said.push(l))
        .unwrap();
        assert!(said.is_empty(), "{said:?}");
        assert_eq!(
            calls,
            ["reset-failed sot-host-relay-remote-a.socket sot-host-relay-remote-a@*.service", "daemon-reload", SHOW, "restart sot-host-relay-remote-a.socket"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_after_a_failed_reload_reloads_and_restarts_again() {
        let topo = hub_topo();
        let dir = scratch("retry");
        std::fs::write(dir.join("sot-host-relay-remote-a.socket"), "stale\n").unwrap();
        std::fs::write(dir.join("sot-host-relay-remote-a@.service"), STALE_SERVICE).unwrap();
        let enabled = vec!["remote-a".to_string()];
        let e = refresh(&topo, "hub-box", &dir, &enabled, &mut |a| {
            if a[1] == "daemon-reload" { Err("Failed to reload daemon: Connection timed out".to_string()) } else { Ok(shown(a)) }
        }, &mut |_| {})
        .unwrap_err();
        assert!(e.contains("daemon-reload"), "{e}");
        let mut calls: Vec<String> = Vec::new();
        refresh(&topo, "hub-box", &dir, &enabled, &mut |a| {
            calls.push(a[1..].join(" "));
            Ok(shown(a))
        }, &mut |_| {})
        .unwrap();
        assert_eq!(
            calls,
            ["reset-failed sot-host-relay-remote-a.socket sot-host-relay-remote-a@*.service", "daemon-reload", SHOW, "restart sot-host-relay-remote-a.socket"],
            "the files already match after the failed run; the reload and the restart must still be redone"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn supervised_means_the_units_main_pid_not_an_inherited_variable() {
        assert!(is_main_pid("4242\n", 4242));
        assert!(!is_main_pid("0\n", 4242), "a stopped or missing unit");
        assert!(!is_main_pid("4243\n", 4242), "a session child or a hand-started daemon");
        assert!(!is_main_pid("", 4242));
    }

    /// The hub pins sotd.service to every host that runs sotd and its reverse tunnels to itself, on their template so
    /// systemd reads the pin for every instance. A second run changes nothing and, with no relay socket enabled,
    /// reloads nothing.
    #[test]
    fn refresh_pins_the_daemon_and_the_tunnels() {
        let topo = topology::parse("hub = \"hub-box\"\n[host.hub-box]\ndaemon = true\n[host.laptop]\nfrontend = true\n[host.server]\n").unwrap();
        let dir = scratch("pins");
        let mut calls: Vec<String> = Vec::new();
        refresh(&topo, "hub-box", &dir, &[], &mut |a| {
            calls.push(a[1..].join(" "));
            Ok(String::new())
        }, &mut |_| {})
        .unwrap();
        let read = |p: &str| std::fs::read_to_string(dir.join(p)).ok();
        assert_eq!(read("sotd.service.d/topology.conf"), Some(topology::relay_units::host_pin(&["hub-box", "laptop"])));
        assert_eq!(read("sot-relay-tunnel@.service.d/topology.conf"), Some(topology::relay_units::host_pin(&["hub-box"])));
        assert_eq!(calls, ["daemon-reload"]);
        calls.clear();
        refresh(&topo, "hub-box", &dir, &[], &mut |a| {
            calls.push(a[1..].join(" "));
            Ok(String::new())
        }, &mut |_| {})
        .unwrap();
        assert!(calls.is_empty(), "{calls:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_off_the_hub_touches_nothing() {
        let dir = scratch("offhub");
        let mut called = false;
        let e = refresh(&hub_topo(), "remote-a", &dir, &["remote-a".to_string()], &mut |_| {
            called = true;
            Ok(String::new())
        }, &mut |_| {})
        .unwrap_err();
        assert!(e.contains("not the hub"), "{e}");
        assert!(!called && !dir.join("sot-host-relay-remote-a@.service").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The program `systemctl()` starts in place of the real one while a test holds it; `None` outside such a test.
    pub(super) static STUB_SYSTEMCTL: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

    /// Clears the stub program on every path out of the test.
    #[cfg(target_os = "linux")]
    struct ClearStub;

    #[cfg(target_os = "linux")]
    impl Drop for ClearStub {
        fn drop(&mut self) {
            *STUB_SYSTEMCTL.lock().unwrap() = None;
        }
    }

    /// The refresh's one-shots run in their own containment: what a
    /// `systemctl` leaves running dies with it, though it exits at once.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_relay_probe_takes_its_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bg = dir.path().join("bg");
        let descendant = crate::lifecycle::child_signal::tests::Leftover::of_file(bg.clone());
        let _clear = ClearStub;
        let stub = dir.path().join("systemctl");
        sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\nsleep 3110 >/dev/null 2>&1 &\necho $! > '{}'\necho 0\n", bg.display()));
        *STUB_SYSTEMCTL.lock().unwrap() = Some(stub);
        assert_eq!(supervised_by_systemd(), Ok(false));
        assert!(descendant.gone(), "the relay probe's descendant survived");
    }
}

/// The hub keeps its generated relay units equal to this binary's text.
/// refresh_at_start acts only in sotd.service's main process, never in a
/// hand-started daemon, and the daemon never waits on it, so a wedged
/// user manager cannot keep it down.
#[cfg(target_os = "linux")]
pub(crate) fn spawn_refresh_at_start() {
    let _ = std::thread::Builder::new().name("relay-refresh".into()).spawn(refresh_at_start);
}
