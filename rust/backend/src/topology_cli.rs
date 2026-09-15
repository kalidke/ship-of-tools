//! `sotd topology <plan|status|relay-endpoint|sync|apply>` — what a box
//! derives from the declared topology (`sot_protocol::topology`, the one
//! parser). A pure query arm of `main` (no startup side effects). Output is
//! line-oriented so a shell or PowerShell launcher reads it with `split`;
//! the `plan` line set is documented on `topology::plan` and nowhere else.
//! Warnings from the parser (accepted v1 keys) go to stderr, one line each.

use sot_protocol::topology::{self, Topology};
use std::path::PathBuf;

const USAGE: &str = "\
Usage: sotd topology <subcommand>

  plan [--self <host>]  this box's derived facts, one per line: self, hub,
                        relay-endpoint, dial <host> <endpoint> (every daemon
                        host), tunnel <host> <port> (every daemon host but
                        self; the hub is 18743, others ordinal in file order)
  status                the declared table (HOST DECLARED); live state is
                        version.query on each daemon
  relay-endpoint        SOT_RELAY_ENDPOINT for this box
  sync [--hub <alias>]  fetch the hub's ~/.config/sot/hosts.toml into this
                        box's config dir (refused if it does not parse)
  apply                 print the relay tunnel units the hub would enable

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
        "status" => with_topology(|t| Ok(print!("{}", topology::status_table(t)))),
        "relay-endpoint" => with_topology(|t| {
            self_host().and_then(|me| topology::relay_endpoint(t, &me)).map(|e| println!("{e}"))
        }),
        "sync" => report(sync(flag("--hub"))),
        "apply" => with_topology(|t| {
            for h in t.hosts.iter().filter(|h| h.name != t.hub && !h.frontend) {
                println!("enable sot-relay-tunnel@{}", h.name);
            }
            println!("# lane E implements this: today `apply` only prints what it would enable");
            Ok(())
        }),
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

fn self_host() -> Result<String, String> {
    sot_log::state_dir::host_name()
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
    for w in &loaded.1.warnings {
        eprintln!("sotd topology: {}: {w}", loaded.0.display());
    }
    report(f(&loaded.1))
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
/// tmp + rename, only after the fetched text parses. The hub alias comes
/// from `--hub`, else from the copy already here.
fn sync(hub: Option<String>) -> Result<(), String> {
    let dest = topology::locate().ok_or("no config dir: set $HOME (or %LOCALAPPDATA%) or $SOT_HOSTS")?;
    let hub = match hub {
        Some(h) => h,
        None => topology::load()?
            .map(|(_, t)| t.hub)
            .ok_or_else(|| format!("no hosts.toml at {} yet: pass --hub <alias>", dest.display()))?,
    };
    let out = std::process::Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", &hub, "cat ~/.config/sot/hosts.toml"])
        .output()
        .map_err(|e| format!("ssh {hub}: {e}"))?;
    if !out.status.success() {
        return Err(format!("ssh {hub}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8(out.stdout).map_err(|_| format!("hub `{hub}`: hosts.toml is not UTF-8"))?;
    let fetched = topology::parse(&text).map_err(|e| format!("hub `{hub}`: fetched hosts.toml rejected, keeping the local copy: {e}"))?;
    if self_host().as_deref() == Ok(fetched.hub.as_str()) {
        return Err(format!("this box is the hub `{}`; its file is the canonical copy", fetched.hub));
    }
    write_atomic(&dest, &text)?;
    println!("synced {} from {hub} ({} hosts, {} monitor targets)", dest.display(), fetched.hosts.len(), fetched.monitor.len());
    for w in &fetched.warnings {
        eprintln!("sotd topology: {}: {w}", dest.display());
    }
    Ok(())
}

fn write_atomic(dest: &PathBuf, text: &str) -> Result<(), String> {
    let io = |e: std::io::Error| format!("{}: {e}", dest.display());
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let tmp = dest.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(io)?;
    std::fs::rename(&tmp, dest).map_err(io)
}
