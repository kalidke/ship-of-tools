//! `sotd topology apply` and `refresh`: the hub's systemd --user relay units and drop-ins,
//! converged with hosts.toml; unit text comes from `sot_protocol::topology`.

use super::cli::self_host;
#[cfg(target_os = "linux")]
use crate::topology_cli;
use sot_protocol::topology::{self, Topology};
use std::path::{Path, PathBuf};

/// `sotd topology apply` (plan §C, §F step 5): on the hub, converge both
/// systemd --user unit families (plus each instance's ConditionHost
/// drop-in, `topology::apply_dropin`) with the declared list —
/// `sot-relay-tunnel@<host>` for the comm relay's reverse tunnels, and
/// `sot-host-relay-<host>.socket` (plus the per-connection
/// `sot-host-relay-<host>@.service`) for the hub's own socket per dialable
/// host — files apply WRITES from `topology::relay_files`, since their text is per host.
/// Refuses off the hub (`topology::require_hub`). `dry_run` prints every
/// action without touching systemd or the filesystem — the default (ADR
/// 0028 units forward a live relay port; this box silently flipping which
/// remotes it tunnels to is not something `apply` does on its own say-so).
/// Pass `--yes` to actually run it.
pub(super) fn apply(topo: &Topology, dry_run: bool) -> Result<(), String> {
    let me = self_host()?;
    topology::require_hub(topo, &me, "apply")?;
    let plan = topology::apply_plan(topo, &enabled_hosts(TUNNEL_TEMPLATE)?, &enabled_hosts(RELAY_TEMPLATE)?);
    if plan.is_empty() {
        println!(
            "up to date: {} tunnel instance(s), {} relay instance(s)",
            topology::tunnel_hosts(topo).len(),
            topology::relay_hosts(topo).len()
        );
        return Ok(());
    }
    let verb = if dry_run { "would " } else { "" };
    // `generated` is the difference between the two families: the reverse
    // tunnel rides ONE hand-installed `sot-relay-tunnel@.service` template,
    // while the relay's unit text is per host (`topology::relay_unit` says
    // why systemd leaves no choice), so apply writes the pair before
    // enabling it and takes it away again on disable.
    // BEST EFFORT, not all-or-nothing: one host that is powered off or
    // refusing ssh must not stop every other host being converged. A `?`
    // here aborted the whole fleet on the first failing unit -- including
    // work for hosts that were fine -- and left the operator to guess how
    // far it got. Each unit's failure is reported as it happens, the rest
    // still run, and the aggregate is returned at the end so the exit
    // status is still honest.
    let mut failures: Vec<String> = Vec::new();
    for (diff, unit, generated) in [
        (&plan.tunnels, topology::tunnel_unit as fn(&str) -> String, false),
        (&plan.relays, topology::relay_unit as fn(&str) -> String, true),
    ] {
        for h in &diff.enable {
            println!("{verb}enable {}", unit(h));
            if !dry_run {
                let step = || -> Result<(), String> {
                    if generated {
                        write_relay_units(h)?;
                    }
                    write_dropin(&unit(h), &topo.hub)?;
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
                    remove_dropin(&unit(h))?;
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
/// (`topology::relay_unit`).
const TUNNEL_TEMPLATE: (&str, &str) = ("sot-relay-tunnel@", ".service");
const RELAY_TEMPLATE: (&str, &str) = ("sot-host-relay-", ".socket");

/// Host names of every unit in one family systemd --user currently
/// reports `enabled`. The only I/O `apply` does to read state;
/// `topology::apply_plan` is the pure decision made from its result.
fn enabled_hosts((prefix, suffix): (&str, &str)) -> Result<Vec<String>, String> {
    let pattern = format!("{prefix}*{suffix}");
    let out = std::process::Command::new("systemctl")
        .args(["--user", "list-unit-files", &pattern, "--no-legend", "--no-pager"])
        .output()
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

/// Writes every file `topology::relay_files` names for one host, overwriting
/// whatever was there: they are apply's, and the header in each says so.
fn write_relay_units(host: &str) -> Result<(), String> {
    let dir = systemd_user_dir()?;
    for (name, text) in topology::relay_files(host) {
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
/// `topology::relay_files` whose text differs (the command drop-in comes last,
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
    topology::require_hub(topo, me, "refresh")?;
    let hosts: Vec<&str> = topology::relay_hosts(topo).into_iter().filter(|h| enabled.iter().any(|e| e == h)).collect();
    if hosts.is_empty() {
        return Ok(());
    }
    let mut failures: Vec<String> = Vec::new();
    for &h in &hosts {
        for (name, text) in topology::relay_files(h) {
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
        let dd = dir.join(format!("{}.d", topology::relay_service_unit_file(h)));
        let Ok(entries) = std::fs::read_dir(&dd) else { continue };
        let mut late: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter(|e| e.file_name().to_str().is_some_and(|n| n.ends_with(".conf") && n > topology::RELAY_COMMAND_DROPIN))
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
        let (socket, instances) = (topology::relay_unit(h), format!("sot-host-relay-{h}@*.service"));
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
        if let Err(e) = systemctl(&["--user", "restart", &topology::relay_unit(h)]) {
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
    let want = format!("argv[]={} ;", topology::relay_command_line());
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
    if me != topo.hub {
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
/// its MainPID is the daemon's own pid.
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
    let out = std::process::Command::new("systemctl")
        .args(["--user", "show", "-p", "MainPID", "--value", DAEMON_UNIT])
        .output()
        .map_err(|e| format!("{what}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{what}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(is_main_pid(&String::from_utf8_lossy(&out.stdout), std::process::id()))
}

/// Removes every generated file for a host, then the service's `.d/` if that
/// left it empty (a hand-made drop-in keeps it). A missing file is not an error
/// — the disable that precedes this is the operation that mattered.
fn remove_relay_units(host: &str) -> Result<(), String> {
    let dir = systemd_user_dir()?;
    for (name, _) in topology::relay_files(host) {
        let _ = std::fs::remove_file(dir.join(name));
    }
    let _ = std::fs::remove_dir(dir.join(format!("{}.d", topology::relay_service_unit_file(host))));
    Ok(())
}

/// `~/.config/systemd/user` — same `$XDG_CONFIG_HOME`-or-`$HOME/.config`
/// rule `scripts/install.sh` uses for its own unit writes.
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
    let out = std::process::Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!("systemctl {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn dropin_path(unit: &str) -> Result<PathBuf, String> {
    Ok(systemd_user_dir()?.join(format!("{unit}.d")).join(topology::APPLY_DROPIN_FILE))
}

/// Writes only apply's own fixed-named file (`topology::APPLY_DROPIN_FILE`)
/// inside the instance's `.d/` directory — any other, hand-made drop-in
/// beside it is never touched (mirrors `scripts/install.sh`'s own rule for
/// `sotd.service.d`: it heals only the one drop-in name it knows).
fn write_dropin(unit: &str, hub: &str) -> Result<(), String> {
    let path = dropin_path(unit)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    // `ConditionHost=` is compared against the hostname systemd sees, which
    // on a domain-joined box is the FQDN -- so the topology's SHORT hub name
    // never matched and every generated unit was enabled, correct and
    // permanently skipped. `apply` only ever runs ON the hub (`require_hub`),
    // so the hostname this process reads IS the value to write: exact, no
    // glob. A glob (`<hub>*`) also matches, but it would match a different
    // box whose name merely starts the same -- and the shared home these
    // units live on is precisely where such a collision would bite, which is
    // the condition's whole reason for existing.
    let condition_host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_string())
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| hub.to_string());
    std::fs::write(&path, topology::apply_dropin(&condition_host))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Removes apply's own drop-in file, then the `.d/` directory only if that
/// left it empty — a hand-made drop-in under a different name keeps the
/// directory (and itself) alive.
fn remove_dropin(unit: &str) -> Result<(), String> {
    let path = dropin_path(unit)?;
    let _ = std::fs::remove_file(&path);
    if let Some(dir) = path.parent() {
        let _ = std::fs::remove_dir(dir);
    }
    Ok(())
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
        if a[1] == "show" { format!("ExecStart={{ argv[]={} ; }}\nLoadState=loaded\n", topology::relay_command_line()) } else { String::new() }
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
        let ours = format!("ExecStart={{ path=/usr/bin/ssh ; argv[]={} ; ignore_errors=no ; pid=0 }}", topology::relay_command_line());
        let old = "ExecStart={ path=/usr/bin/ssh ; argv[]=/usr/bin/ssh -T x sotd stdio-bridge --label local ; ignore_errors=no ; pid=0 }";
        let longer = format!("ExecStart={{ path=/usr/bin/ssh ; argv[]={} --label local ; ignore_errors=no ; pid=0 }}", topology::relay_command_line());
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
        std::fs::write(dir.join("sot-host-relay-remote-a.socket"), topology::relay_socket_unit("remote-a")).unwrap();
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
        assert_eq!(
            said,
            [
                format!("rewrote {}", dir.join("sot-host-relay-remote-a@.service").display()),
                format!("rewrote {}", dd.join("zz-sot-relay-command.conf").display())
            ]
        );
        assert_eq!(std::fs::read_to_string(dir.join("sot-host-relay-remote-a@.service")).unwrap(), topology::relay_service_unit("remote-a"));
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
}

/// The hub keeps its generated relay units equal to this binary's text.
/// refresh_at_start acts only in sotd.service's main process, never in a
/// hand-started daemon, and the daemon never waits on it, so a wedged
/// user manager cannot keep it down.
#[cfg(target_os = "linux")]
pub(crate) fn spawn_refresh_at_start() {
    let _ = std::thread::Builder::new().name("relay-refresh".into()).spawn(topology_cli::refresh_at_start);
}
