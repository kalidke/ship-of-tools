//! `sotd topology <plan|status|relay-endpoint|relay-sockets|sync|apply>` — what a box
//! derives from the declared topology (`sot_protocol::topology`, the one
//! parser). A pure query arm of `main` (no startup side effects). Output is
//! line-oriented so a shell or PowerShell launcher reads it with `split`;
//! the `plan` line set is documented on `topology::plan` and nowhere else.

use super::relay_units::{apply, refresh_cmd};
use sot_protocol::topology::{self, Topology};

pub(crate) const USAGE: &str = "\
Usage: sotd topology <subcommand>

  plan [--self <host>]  this box's derived facts, one per line: self, hub,
                        relay-endpoint, dial <host> <endpoint> (every daemon
                        host — unix:/pipe: for itself and, on the hub, for
                        any other host; ssh:<hub> or ssh:<hub>/<host>
                        everywhere else)
  status                the declared table (HOST DECLARED), plus a
                        \"cache diverged\" line when this box's file hash
                        disagrees with the hub's (skipped ON the hub, and
                        best-effort — silent if the hub can't be reached)
  relay-endpoint        SOT_RELAY_ENDPOINT for this box
  relay-sockets         hub only: `<host> <path>` for each host the hub
                        serves a socket for (every daemon host but the hub
                        itself). A peer asks the hub for these over ssh —
                        they are the hub's own paths and cannot be derived
                        anywhere else — and forwards its own dial port to
                        one, exactly as it already does with the hub's own
                        session socket.
  sync [--hub <alias>]  fetch the hub's ~/.config/sot/hosts.toml into this
                        box's config dir. The local copy's own hub always
                        wins over --hub (a bootstrap-only fallback, used
                        when there is no local copy yet); re-point a box by
                        deleting its copy first. A no-op on the hub itself.
                        Refused if the fetched file does not parse or does
                        not list this box; nothing written when it equals
                        the local copy.
  apply [--yes]         hub only: converge two systemd --user unit
                        families (enable/disable) plus each instance's
                        ConditionHost drop-in with the declared list —
                        sot-relay-tunnel@<host> (the comm relay's reverse
                        tunnels) and sot-host-relay-<host>.socket with its
                        per-connection sot-host-relay-<host>@.service (the
                        hub's own socket per dialable host, unit text and
                        all: apply WRITES those files, they are not shipped).
                        Default is a DRY RUN (prints what it would do);
                        --yes runs systemctl for real. --dry-run is
                        accepted too, as the explicit spelling of the
                        default. Refuses on a non-hub box, naming the hub.
  refresh               hub only: rewrite each enabled host's relay unit files
                        and the drop-in that sets their command, when their
                        text differs from this sotd's; clear failed relay instances,
                        reload, and restart every enabled relay socket, every
                        run, so a step that failed is redone at the next. The
                        hub's daemon does this at each start as sotd.service's
                        main process; the verb is for a --no-service hub and
                        for hand recovery.
  set <edit>            send one edit to the hub daemon over this box's own
                        control dial (the dial itself is the authorisation —
                        no second credential). <edit> is one of:
                          add <host> [--daemon] [--frontend]
                          remove <host>
                          flag <host> daemon|frontend true|false
                          monitor-add <label> <target>
                          monitor-remove <label>

The file: $SOT_HOSTS, else <config dir>/hosts.toml (~/.config/sot).";

/// Exit status for `main`.
pub fn run(args: &[String]) -> i32 {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    match sub {
        "plan" => with_topology(|t| {
            let me = match flag("--self") { Some(h) => Ok(h), None => self_host() };
            me.and_then(|me| topology::plan(t, &me)).map(|s| print!("{s}"))
        }),
        "status" => with_topology(|t| {
            print!("{}", topology::status_table(t));
            report_cache_divergence(t);
            Ok(())
        }),
        // NOT `with_topology`: main's ruling (isolation-plan.md §3 C10's
        // Rust half) is that this one subcommand answers even with no
        // `hosts.toml` at all — a box that has never declared a topology
        // has no hub and no declared peers, so its own daemon is the only
        // daemon it could mean. The other five keep the wrapper and its
        // `no hosts.toml` error verbatim.
        "relay-endpoint" => report(relay_endpoint_cmd()),
        "relay-sockets" => with_topology(|t| {
            topology::relay_units::require_hub(t, &self_host()?, "relay-sockets")?;
            for h in topology::relay_units::relay_hosts(t) {
                println!("{h} {}", topology::relay_socket_path(h).display());
            }
            Ok(())
        }),
        "sync" => report(sync(flag("--hub"))),
        "apply" => with_topology(|t| apply(t, !args.iter().any(|a| a == "--yes"))),
        "refresh" => with_topology(|t| refresh_cmd(t)),
        "set" => report(set(&args[1..])),
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

pub(super) fn self_host() -> Result<String, String> {
    sot_log::host::state_dir::host_name()
}

fn with_topology(f: impl FnOnce(&Topology) -> Result<(), String>) -> i32 {
    let loaded = match topology::load() {
        Ok(Some(x)) => x,
        Ok(None) => {
            let looked = topology::locate().map(|p| p.display().to_string()).unwrap_or_default();
            return report(Err(format!("no hosts.toml at {looked} (run `sotd topology sync --hub <alias>`)")));
        }
        Err(e) => return report(Err(e)),
    };
    report(f(&loaded.1))
}

/// `relay-endpoint`'s own load, deliberately not [`with_topology`]: a file
/// that lists other hosts but not this one is still `relay_endpoint`'s own
/// `` `<host>` is not a listed host `` error (a different case — other
/// boxes provably exist, so answering "myself" there would be the silent
/// wrong-box failure this whole design deletes); no file at all is this
/// box's own endpoint, because nothing has ever told it of another box.
fn relay_endpoint_cmd() -> Result<(), String> {
    let me = self_host()?;
    match topology::load()? {
        Some((_, t)) => topology::relay_endpoint(&t, &me).map(|e| println!("{e}")),
        None => {
            println!("{}", topology::endpoint::local_endpoint());
            Ok(())
        }
    }
}

fn report(r: Result<(), String>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("sotd topology: {e}");
            2
        }
    }
}

/// `ssh <hub> cat ~/.config/sot/hosts.toml` into this box's own copy,
/// tmp + rename, only after the fetched text parses, and only when it
/// differs from the copy here: on a shared-home box the "copy" IS the
/// canonical file, and a rename over it could drop an owner edit made
/// between fetch and rename. The hub alias is [`resolve_hub`]'s call — the
/// local copy wins whenever one exists; `--hub` is a bootstrap value only.
fn sync(hub: Option<String>) -> Result<(), String> {
    let dest = topology::locate().ok_or("no config dir: set $HOME (or %LOCALAPPDATA%) or $SOT_HOSTS")?;
    let local = topology::load()?;
    let hub = resolve_hub(local.as_ref().map(|(_, t)| t), hub.as_deref(), &dest)?;
    let me = self_host()?;
    if self_is_hub(&me, &hub) {
        println!("this box is the hub; nothing to sync");
        return Ok(());
    }
    let out = std::process::Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", &hub, "cat ~/.config/sot/hosts.toml"])
        .output()
        .map_err(|e| format!("ssh {hub}: {e}"))?;
    if !out.status.success() {
        return Err(format!("ssh {hub}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8(out.stdout).map_err(|_| format!("hub `{hub}`: hosts.toml is not UTF-8"))?;
    let fetched = topology::parse(&text).map_err(|e| format!("hub `{hub}`: fetched hosts.toml rejected, keeping the local copy: {e}"))?;
    refuse_if_not_listed(&fetched, &me)?;
    if std::fs::read_to_string(&dest).ok().as_deref() == Some(text.as_str()) {
        println!("{} is current (same as {hub})", dest.display());
        return Ok(());
    }
    crate::topology::store::write_atomic(&dest, &text)?;
    println!("synced {} from {hub} ({} hosts, {} monitor targets)", dest.display(), fetched.hosts.len(), fetched.monitor.len());
    Ok(())
}

/// Which hub `sync` fetches from (orchestrator ruling): the LOCAL copy's
/// `hub` always wins when a copy already exists — re-pointing a box to a
/// different hub means deleting its copy and running `sync --hub <new>`
/// again, not overriding a live copy in place. `--hub` is read only when
/// there is no local copy at all (bootstrap: the first sync on a box that
/// has never had one). Both present and disagreeing is not an error — the
/// copy still wins — but it is said out loud rather than silently ignored.
fn resolve_hub(local: Option<&Topology>, cli_hub: Option<&str>, dest: &std::path::Path) -> Result<String, String> {
    match local {
        Some(t) => {
            if let Some(cli) = cli_hub {
                if cli != t.hub {
                    println!("local copy already names hub `{}`; ignoring --hub `{cli}` (delete the local copy to re-point this box)", t.hub);
                }
            }
            Ok(t.hub.clone())
        }
        None => cli_hub.map(str::to_string).ok_or_else(|| format!("no hosts.toml at {} yet: pass --hub <alias>", dest.display())),
    }
}

/// True when this box IS the hub named by `hub` — syncing would fetch its
/// own file over ssh to itself, which is nonsense (the hub's copy is
/// already canonical). Checked BEFORE any ssh call, so this is a cheap
/// no-op on the hub rather than a wasted (and often permission-denied)
/// self-ssh round trip on every launch.
fn self_is_hub(me: &str, hub: &str) -> bool {
    me == hub
}

/// A stale file that doesn't list this box is the root cause of the
/// old-vs-new install confusion (a v1 file never listed the frontend box):
/// caught here, at fetch time, rather than left for `plan` to fail on
/// silently later. Factored out so it's testable without ssh or a real
/// `self_host()`.
fn refuse_if_not_listed(fetched: &Topology, me: &str) -> Result<(), String> {
    if fetched.host(me).is_none() {
        return Err(format!("hub's hosts.toml does not list this box; add `[host.{me}]` on the hub"));
    }
    Ok(())
}

/// `sotd topology set <edit>`. Sends one edit to the hub over this box's
/// own control dial — the SAME endpoint `plan`'s `dial <hub>` line already
/// gives out (`hub_endpoint`, below: this box's own socket when it IS the
/// hub, else `ssh:<hub>`, the ssh child every non-hub box already spawns
/// for everything else). No separate "find the hub" step: authorisation is
/// the dial itself (`op::TOPOLOGY_SET`'s own doc).
fn set(words: &[String]) -> Result<(), String> {
    let edit = parse_edit(words)?;
    let (_, topo) = topology::load()?.ok_or_else(|| "no hosts.toml yet (run `sotd topology sync --hub <alias>`)".to_string())?;
    let me = self_host()?;

    // "removing or clearing `daemon` on a host with running rows" — the
    // HUB only ever checks its OWN rows (plan §B: a star, it cannot see
    // another daemon's). When THIS edit touches THIS box's own daemon
    // flag, this box's own CLI does the equivalent check against its own
    // local daemon before ever dialing the hub, so the gap the hub can't
    // cover is covered here instead.
    let touches_own_daemon = match &edit {
        topology::TopologyEdit::RemoveHost { name } => *name == me,
        topology::TopologyEdit::SetFlag { name, key, value } => *name == me && key == "daemon" && !*value,
        _ => false,
    };
    if touches_own_daemon && local_has_running_rows(&me) {
        return Err(format!("`{me}` has running capsule rows on this box's own daemon; stop them before removing or un-daemoning this host"));
    }

    let endpoint = hub_endpoint(&topo, &me);
    let payload = serde_json::json!({ "edit": edit });
    let res = crate::topology::dial::dial_and_call(&endpoint, &me, sot_protocol::op::TOPOLOGY_SET, payload)?;
    if let Some(err) = res.get("error").and_then(|v| v.as_str()) {
        let code = res.get("code").and_then(|v| v.as_str()).unwrap_or("refused");
        return Err(format!("{err} ({code})"));
    }
    let hash = res.get("hash").and_then(|v| v.as_str()).unwrap_or("?");
    println!("topology.set ok, hash {hash}");
    Ok(())
}

/// This box's own dial to the hub: its own socket when it IS the hub, else
/// the SAME `ssh:<hub>` endpoint `plan`'s `dial <hub>` line already uses —
/// an ssh child into the hub, running `sotd stdio-bridge` there with no
/// argument (its own daemon, which IS the hub's).
fn hub_endpoint(topo: &Topology, me: &str) -> String {
    if me == topo.hub {
        topology::endpoint::local_endpoint()
    } else {
        format!("ssh:{}", topo.hub)
    }
}

/// Best-effort: dial THIS box's own local daemon and ask `workspace.list`
/// for any row whose phase is `starting`/`ready`. A daemon that can't be
/// reached here means nothing is running here either (`false`), same as
/// no daemon at all — this is a local safety pre-check, not a source of
/// truth the hub relies on.
fn local_has_running_rows(me: &str) -> bool {
    let endpoint = topology::endpoint::local_endpoint();
    let res = match crate::topology::dial::dial_and_call(&endpoint, me, sot_protocol::op::WORKSPACE_LIST, serde_json::json!({})) {
        Ok(v) => v,
        Err(_) => return false,
    };
    res.get("workspaces")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter().any(|r| {
                r.get("phase")
                    .and_then(|p| p.as_str())
                    .is_some_and(|p| p.eq_ignore_ascii_case("starting") || p.eq_ignore_ascii_case("ready"))
            })
        })
        .unwrap_or(false)
}

/// `sotd topology status`'s cache line: skipped on the hub itself (its
/// file IS the canonical copy, trivially current); best-effort elsewhere —
/// a hub that can't be reached prints nothing rather than an error, since
/// `status` is meant to work offline too.
fn report_cache_divergence(topo: &Topology) {
    let Ok(me) = self_host() else { return };
    if me == topo.hub {
        return;
    }
    let Some(path) = topology::locate() else { return };
    let Ok(local_text) = std::fs::read_to_string(&path) else { return };
    let local_hash = topology::hash_text(&local_text);
    let endpoint = hub_endpoint(topo, &me);
    let Ok(res) = crate::topology::dial::dial_and_call(&endpoint, &me, sot_protocol::op::VERSION_QUERY, serde_json::json!({})) else {
        return;
    };
    let Some(hub_hash) = res.get("daemon").and_then(|d| d.get("hosts_toml_hash")).and_then(|v| v.as_str()) else {
        return;
    };
    if !hub_hash.is_empty() && hub_hash != local_hash {
        println!("cache diverged: this box's hosts.toml ({local_hash}) != the hub's ({hub_hash}); run `sotd topology sync`");
    }
}

/// Parse `sotd topology set <words...>` into one [`topology::TopologyEdit`]
/// — the shapes documented in [`USAGE`].
fn parse_edit(words: &[String]) -> Result<topology::TopologyEdit, String> {
    const USAGE: &str = "usage: sotd topology set <add|remove|flag|monitor-add|monitor-remove> ...";
    match words.first().map(String::as_str) {
        Some("add") => {
            let name = words.get(1).cloned().ok_or(USAGE)?;
            let daemon = words.iter().any(|w| w == "--daemon");
            let frontend = words.iter().any(|w| w == "--frontend");
            Ok(topology::TopologyEdit::AddHost { name, daemon, frontend })
        }
        Some("remove") => Ok(topology::TopologyEdit::RemoveHost { name: words.get(1).cloned().ok_or(USAGE)? }),
        Some("flag") => {
            let name = words.get(1).cloned().ok_or(USAGE)?;
            let key = words.get(2).cloned().ok_or(USAGE)?;
            let value = match words.get(3).map(String::as_str) {
                Some("true") => true,
                Some("false") => false,
                _ => return Err(USAGE.to_string()),
            };
            Ok(topology::TopologyEdit::SetFlag { name, key, value })
        }
        Some("monitor-add") => Ok(topology::TopologyEdit::MonitorAdd {
            label: words.get(1).cloned().ok_or(USAGE)?,
            target: words.get(2).cloned().ok_or(USAGE)?,
        }),
        Some("monitor-remove") => Ok(topology::TopologyEdit::MonitorRemove { label: words.get(1).cloned().ok_or(USAGE)? }),
        _ => Err(USAGE.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_is_default_yes_flips_it() {
        let dry = |args: &[&str]| !args.iter().any(|a| *a == "--yes");
        assert!(dry(&[]));
        assert!(dry(&["--dry-run"]));
        assert!(!dry(&["--yes"]));
    }

    fn w(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_edit_covers_every_shape() {
        assert_eq!(
            parse_edit(&w(&["add", "gamma", "--daemon"])).unwrap(),
            topology::TopologyEdit::AddHost { name: "gamma".into(), daemon: true, frontend: false }
        );
        assert_eq!(parse_edit(&w(&["remove", "gamma"])).unwrap(), topology::TopologyEdit::RemoveHost { name: "gamma".into() });
        assert_eq!(
            parse_edit(&w(&["flag", "gamma", "daemon", "true"])).unwrap(),
            topology::TopologyEdit::SetFlag { name: "gamma".into(), key: "daemon".into(), value: true }
        );
        assert_eq!(
            parse_edit(&w(&["monitor-add", "gpu", "box"])).unwrap(),
            topology::TopologyEdit::MonitorAdd { label: "gpu".into(), target: "box".into() }
        );
        assert_eq!(parse_edit(&w(&["monitor-remove", "gpu"])).unwrap(), topology::TopologyEdit::MonitorRemove { label: "gpu".into() });
        assert!(parse_edit(&w(&["flag", "gamma", "daemon", "maybe"])).is_err());
        assert!(parse_edit(&w(&[])).is_err());
    }

    #[test]
    fn hub_endpoint_is_local_on_the_hub_else_an_ssh_child_into_it() {
        let t = topology::parse("hub = \"alpha\"\n[host.alpha]\ndaemon = true\n").unwrap();
        assert_eq!(hub_endpoint(&t, "alpha"), topology::endpoint::local_endpoint());
        assert_eq!(hub_endpoint(&t, "beta"), "ssh:alpha");
    }

    #[test]
    fn sync_refuses_a_fetched_file_that_does_not_list_self() {
        let fetched = topology::parse("hub = \"hub\"\n[host.hub]\ndaemon = true\n").unwrap();
        let e = refuse_if_not_listed(&fetched, "frontend-box").unwrap_err();
        assert!(e.contains("does not list this box") && e.contains("[host.frontend-box]"), "{e}");
        assert!(refuse_if_not_listed(&fetched, "hub").is_ok());
    }

    #[test]
    fn self_is_hub_predicate() {
        assert!(self_is_hub("hub-box", "hub-box"));
        assert!(!self_is_hub("frontend-box", "hub-box"));
    }

    #[test]
    fn self_is_hub_is_exact_byte_equality() {
        assert!(!self_is_hub("Hub-Box", "hub-box"));
        assert!(!self_is_hub("hub-box ", "hub-box"));
        assert!(self_is_hub("", ""));
    }

    #[test]
    fn resolve_hub_prefers_the_local_copy_over_a_disagreeing_flag() {
        let t = topology::parse("hub = \"hub-a\"\n[host.hub-a]\ndaemon = true\n").unwrap();
        let dest = std::path::Path::new("/nowhere/hosts.toml");
        assert_eq!(resolve_hub(Some(&t), None, dest).unwrap(), "hub-a", "no flag: the copy's hub");
        assert_eq!(resolve_hub(Some(&t), Some("hub-a"), dest).unwrap(), "hub-a", "agreeing flag: no complaint, same answer");
        assert_eq!(resolve_hub(Some(&t), Some("hub-b"), dest).unwrap(), "hub-a", "disagreeing flag: the copy still wins");
    }

    #[test]
    fn resolve_hub_falls_back_to_the_flag_only_with_no_local_copy() {
        let dest = std::path::Path::new("/nowhere/hosts.toml");
        assert_eq!(resolve_hub(None, Some("hub-a"), dest).unwrap(), "hub-a");
        let e = resolve_hub(None, None, dest).unwrap_err();
        assert!(e.contains("/nowhere/hosts.toml") && e.contains("pass --hub <alias>"), "{e}");
    }
}
