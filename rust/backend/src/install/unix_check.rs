//! The Unix installer's final checks and ordered checklist.

use super::super::{pack, secs_env};
use super::*;

/// What the run left, read from the disk and the processes: a failed check is FAIL, a step only a person takes is ONE
/// STEP LEFT, and each check that passed records its checklist line. `state` is the state folder the service step chose
/// (none when this computer runs no daemon).
#[allow(clippy::too_many_lines, reason = "the checks in install.sh's order, one block each")]
pub(super) fn check(plan: &Plan, restarted: bool, state: Option<state::Choice>, found: &mut Found) {
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
        if pack::binary_says(&line, b, version, &commit) {
            found.item(Mark::Ok, format!("{b} {version} at {short} ({}/bin/{b})", prefix.display()));
        } else {
            found.fail("binaries", format!("{}/bin/{b} says '{line}', not {version} at {short}", prefix.display()));
        }
    }
    let text = record::read(&prefix.join("install.json"));
    if record::string(&text, "version").as_deref() != Some(version) || record::string(&text, "commit").as_deref() != Some(&commit) {
        found.fail("record", format!("{}/install.json does not name {version} at {short}", prefix.display()));
    } else {
        found.item(Mark::Ok, format!("install record {}/install.json", prefix.display()));
    }
    let current = prefix.join("repo/current");
    let resolves = std::fs::read_link(&current).ok() == Some(checkout.clone())
        && cmd::output(cmd::git_in(&current).args(["rev-parse", "HEAD"])).first_line() == commit;
    if !resolves {
        found.fail("checkout", format!("{} does not resolve to {} at {short}", current.display(), checkout.display()));
    } else if !commit.is_empty() {
        found.item(Mark::Ok, format!("checkout {} at {short}", current.display()));
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
    } else {
        found.item(Mark::Ok, format!("comm and skills at {short} ({})", comm.display()));
    }
    if plan.role.daemon {
        let mut whole = true;
        for (env, _) in julia::ENVS {
            if !checkout.join("julia").join(env).join("Manifest.toml").is_file() {
                found.fail("julia", format!("julia/{env} has no Manifest.toml: its environment is not instantiated"));
                whole = false;
            }
        }
        if whole {
            found.item(Mark::Ok, "Julia environments instantiated");
        }
    }
    if let Some(state) = state {
        state.record(found);
    }
    if plan.role.daemon {
        check_claude(std::env::var_os("PATH").as_deref(), &plan.home, found);
    }
    let after = daemon_after(plan, restarted, found);
    if plan.role.daemon && after == State::Current {
        found.item(Mark::Ok, format!("daemon running at {version}"));
    }
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

/// Claude Code, found by the rule a Claude row's launch uses (agents' `resolve_claude`): a new Claude session cannot
/// start without it.
pub(super) fn check_claude(path: Option<&std::ffi::OsStr>, home: &Path, found: &mut Found) {
    match crate::agents::argv::resolve_claude(path, Some(home)) {
        Ok(path) => found.item(Mark::Ok, format!("Claude Code {path}")),
        Err(why) => found.step(format!("install Claude Code on this computer, then run this installer again ({why})")),
    }
}

/// The publish guard's checklist lines: one per clone its check found ready, from what it recorded for the verdict.
pub(super) fn guard_items(found: &mut Found) {
    let ready = found.checked.iter().filter(|c| c.starts_with("publish guard checked ") && c.ends_with(": ready")).cloned().collect::<Vec<_>>();
    for line in ready {
        found.item(Mark::Ok, line);
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
        let runs = match procs::checked(
            procs::snapshot(),
            found,
            "daemon",
            "what runs under the install",
        ) {
            Some(all) => !procs::daemon_pids(&all, &plan.prefix).is_empty(),
            None => return State::Unknown,
        };
        let (st, why) = daemon::state(&plan.sotd(), &endpoint, runs);
        if st == State::Unknown {
            found.fail(
                "daemon",
                format!("a daemon runs on {socket} and could not be told its version ({why})"),
            );
        }
        return st;
    }
    let held_ok = plan.unit_runs && !restarted;
    if systemctl(&["is-active", "sotd.service"]).ok() {
        let (st, why) = daemon::wait_current(
            &plan.sotd(),
            &endpoint,
            secs_env("SOT_INSTALL_RESTART_WAIT_S", 30),
            held_ok,
            true,
        );
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
