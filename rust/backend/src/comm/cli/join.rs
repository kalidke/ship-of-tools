//! `sotd comm join` (comm-join.sh): take this session's handle. The handle is the one the session's self file already
//! names (for a row, the one the row was created with), else derived (`identity::derive`), the derivation and its
//! registry write one step under the registry lock; no argument or variable chooses it (decision 0031). Then the self file, the inbox, and for a row the declaration to its daemon
//! (`agent.join`).

use crate::comm::Env;
use serde_json::{json, Value};

use super::{bin_dir, context, gate, wire, Out};
use crate::comm::identity::context::{shell_quote, Context};
use crate::comm::identity::derive::{derive, still_cleared, tier, Derived, Mode};
use crate::comm::identity::self_file;
use crate::comm::registry::registry::{binding, command_lock_wait, iso8601_utc_now, read_doc, unix_now_secs, update, Binding};

pub(crate) const HELP: &str = "comm-join.sh \u{2014} register this session in the sot-comm network.

Usage:
  comm-join.sh                       just run it: joins as this session's
                                     handle, the one its self file names,
                                     else derived: <repo>-<host>
  comm-join.sh --repin               this row really is this project now:
                                     write the identity even though the slot
                                     names a different one (refused otherwise)
  comm-join.sh --expertise \"a, b\"    optional comma-separated expertise tags
  comm-join.sh --expertise=\"a, b\"    (equals form also accepted)
  comm-join.sh -h | --help           this help

Handles are derived, never chosen: <repo>-<host>, disambiguated against the
registry's recorded project root when two different projects share a
basename+host: <repo>-<parentdir>-<host>, or a hash-qualified handle as a last
resort. A row keeps the handle it was created with through its self file.

On success prints \"Joined sot-comm as @<handle>\" \u{2014} that line IS your
identity confirmation.";

/// What `join` was asked.
#[derive(Debug, Default, PartialEq, Eq)]
struct Ask {
    expertise: String,
    repin: bool,
    help: bool,
}

/// The refusal of a chosen handle.
const NO_NAME: &str = "comm-join.sh: handles are derived, never chosen (decision 0031): a row keeps the one it was created with through its self file, and any other session joins as <repo>-<host>; run comm-join.sh with no name";

fn parse(args: &[String]) -> Result<Ask, String> {
    let mut a = Ask::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => a.help = true,
            "--repin" => a.repin = true,
            "--expertise" => a.expertise = it.next().cloned().unwrap_or_default(),
            s if s == "--name" || s.starts_with("--name=") => return Err(NO_NAME.to_string()),
            s if s.starts_with("--expertise=") => a.expertise = s["--expertise=".len()..].to_string(),
            s if s.starts_with('-') => return Err(format!("comm-join.sh: unknown option '{s}' (see --help)")),
            _ => return Err(NO_NAME.to_string()),
        }
    }
    Ok(a)
}

/// May a session outside any row take `h`? The daemon rules from its own rows, whatever the registry file binds
/// (`agent.join` with an empty row id). `Ok` is the row id the entry binds now (`""` for none), which the write under
/// the lock is held to; `Err((1, why))` a refusal; `Err((2, why))` an unreadable registry. An entry bound to a row on
/// another computer of the shared home is held, since no daemon here can rule on it; where no daemon rules, a bound
/// entry is held and an unbound or absent one is taken (the named limit).
pub(crate) fn outside_clearance(env: &Env, ctx: &Context, h: &str) -> Result<String, (i32, String)> {
    let state = binding(&ctx.registry(), h, "");
    if state == Binding::Unreadable {
        return Err((2, format!("the registry could not be read, so nothing can say whether @{h} is held; a session outside any row cannot join as it, and nothing was written")));
    }
    let ws = match &state {
        Binding::Other { ws, .. } => ws.clone(),
        _ => String::new(),
    };
    let mut ruled = false;
    if let Some(endpoint) = wire::own_daemon(env) {
        if let Ok(answer) = wire::agent_join(env, &endpoint, "", h) {
            if answer.get("ok").and_then(Value::as_bool) == Some(true) {
                ruled = true;
            } else if answer.get("code").and_then(Value::as_str) == Some("held_by_row") {
                let row = if ws.is_empty() { String::new() } else { format!(" (row {ws})") };
                return Err((1, format!("@{h} is held by a row{row}; a session outside any row cannot join as it")));
            }
        }
    }
    if let Binding::Other { ws, host } = &state {
        if !host.is_empty() && *host != ctx.host {
            return Err((1, format!("@{h} is bound to row {ws} on another computer ({host}), which no daemon here can say is gone; a session outside any row cannot join as it")));
        }
        if !ruled {
            return Err((1, format!("@{h} is bound to row {ws} and the daemon could not say that row is gone; a session outside any row cannot join as it")));
        }
    }
    Ok(ws)
}

/// The entry a join writes: the whole entry, as 0.6.6's join replaced it.
fn entry_for(ctx: &Context, expertise: &str) -> Value {
    let ts = iso8601_utc_now();
    let exp: Vec<String> = expertise.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
    json!({
        "host": ctx.host, "workspace_id": ctx.row_id, "repo": ctx.repo, "root": ctx.project_root,
        "expertise": exp, "status": "idle", "joined": ts, "last_seen": ts,
    })
}

pub(crate) fn join(env: &Env, args: &[String]) -> Out {
    let mut o = Out::default();
    let ask = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            o.err(e);
            return o.exit(2);
        }
    };
    if ask.help {
        o.out(HELP);
        return o;
    }
    if let Err((_, why)) = gate(env) {
        o.err(format!("comm-join.sh: REFUSING to bind: {why}"));
        return o.exit(1);
    }
    let mut notes = Vec::new();
    let ctx = match context(env, true, &mut notes) {
        Ok(c) => c,
        Err(e) => {
            o.errs(&notes);
            o.err(e);
            return o.exit(1);
        }
    };
    o.errs(&notes);
    o.errs(&crate::comm::home::ensure(&ctx.comm_home, env));
    joined(env, o, &ctx, ask)
}

/// The join once the identity is known: the name, the registry write, the self file, the inbox, the declaration.
fn joined(env: &Env, mut o: Out, ctx: &Context, ask: Ask) -> Out {
    let mut name = ctx.name.clone();
    let expertise = if ask.expertise.is_empty() { env.text("SOT_COMM_EXPERTISE") } else { ask.expertise.clone() };
    if !ask.repin {
        if let Some(_claim) = self_file::conflict(&ctx.self_file, &ctx.repo, &ctx.project_root) {
            o.err(format!(
                "comm-join.sh: REFUSING to put a '{}' identity in that slot \u{2014} it is keyed by this shell's $SOT_WORKSPACE_ID ('{}'), and a row that comes to name another project's session reads that session's mail. Either run this from inside the row it belongs to, or, if this row really is '{}' now, re-run with --repin.",
                ctx.repo,
                if ctx.workspace_id.is_empty() { "unset" } else { &ctx.workspace_id },
                ctx.repo
            ));
            return o.exit(3);
        }
    }
    // A session outside any row never takes a handle a row holds: what the daemon cleared, with the row id each entry
    // bound then, holds the write under the lock.
    let outside = ctx.row_id.is_empty();
    let mut cleared: Vec<(String, String)> = Vec::new();
    if outside {
        if !name.is_empty() {
            match outside_clearance(env, ctx, &name) {
                Ok(ws) => cleared.push((name.clone(), ws)),
                Err((2, why)) => {
                    o.err(format!("comm-join.sh: FAILED: {why}"));
                    return o.exit(1);
                }
                Err(_) => {
                    o.err(format!("comm: @{name} (named by this session's self file) is held by a row; a session outside any row joins as its own handle"));
                    name.clear();
                }
            }
        }
        if name.is_empty() {
            let host = crate::comm::identity::derive::handle_host(&ctx.handle_host);
            for n in 1..=3 {
                let (_, t) = tier(n, &ctx.project_root, &host, ctx.kind);
                if let Ok(ws) = outside_clearance(env, ctx, &t) {
                    cleared.push((t, ws));
                }
            }
        }
    }
    let obj = entry_for(ctx, &expertise);
    let derived = name.is_empty();
    let claim = update(env, &ctx.registry(), command_lock_wait(env), |agents| {
        if derived {
            let mut lines = Vec::new();
            let d = derive(Mode::Reclaim, &ctx.project_root, &ctx.handle_host, ctx.kind, agents, outside.then_some(&cleared[..]), &mut lines)?;
            agents.insert(d.name.clone(), obj.clone());
            Ok(((Some(d), lines), true))
        } else {
            if outside && !still_cleared(agents, &name, &cleared) {
                return Err(format!("comm-join.sh: REFUSING to join: @{name} was taken by a row while this join ran; nothing was written"));
            }
            agents.insert(name.clone(), obj.clone());
            Ok(((None, Vec::new()), true))
        }
    });
    let derivation: Option<Derived> = match claim {
        Ok((d, lines)) => {
            o.errs(&lines);
            d
        }
        Err(e) => {
            o.err(if e.starts_with("comm") { e } else { format!("FAILED: {e}") });
            return o.exit(1);
        }
    };
    if let Some(d) = &derivation {
        name = d.name.clone();
        stranding_warning(env, &mut o, ctx, d, outside, &cleared);
    }
    // The self file, under the registry lock (the lock every join's writes take).
    let wrote = crate::comm::registry::lock::acquire_in(env, &ctx.comm_home.join(".registry.lock"), command_lock_wait(env)).and_then(|_held| {
        if !ask.repin {
            if let Some(why) = self_file::conflict(&ctx.self_file, &ctx.repo, &ctx.project_root) {
                return Err(format!("conflict:{why}"));
            }
        }
        self_file::write(&ctx.self_file, &name, &ctx.repo, &ctx.project_root)
    });
    match wrote {
        Ok(()) => {}
        Err(e) if e.starts_with("conflict:") => {
            o.err(format!(
                "comm-join.sh: REFUSING to put a '{}' identity in the slot '{}' \u{2014} it was claimed for another project while this join ran ({}). The registry row for @{name} is claimed but this shell has no identity record, so re-run the join: from inside the row it belongs to, or with --repin if this row really is '{}' now.",
                ctx.repo,
                ctx.self_file.display(),
                &e["conflict:".len()..],
                ctx.repo
            ));
            return o.exit(3);
        }
        Err(e) => {
            o.err(format!(
                "comm-join.sh: FATAL \u{2014} joined as @{name} in the registry, but could not write the local self-file at '{}' ({e}). This shell has no identity record; every comm-* call from it will say 'not joined'. Fix the self-file directory's permissions and re-run comm-join.sh.",
                ctx.self_file.display()
            ));
            return o.exit(1);
        }
    }
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(ctx.inbox_dir().join(format!("{name}.jsonl")));
    status_join(env);
    declare(env, &mut o, ctx, &name);
    let others = read_doc(&ctx.registry()).ok().map(|doc| {
        let have = doc.get("protocol_version").and_then(Value::as_u64).unwrap_or(0);
        if have != u64::from(crate::comm::home::PROTOCOL_VERSION) {
            o.err(format!(
                "WARNING: registry protocol v{have} != client v{} \u{2014} run ShipTools.update_comm() on all machines",
                crate::comm::home::PROTOCOL_VERSION
            ));
        }
        doc["agents"].as_object().map(|a| a.keys().filter(|k| **k != name).cloned().collect::<Vec<_>>().join(", ")).unwrap_or_default()
    });
    o.out(format!("Joined sot-comm as @{name}  ({} on {}).", ctx.repo, ctx.host));
    o.out(format!("  inbox: {}/{name}.jsonl", crate::comm::identity::paths::native(&ctx.inbox_dir().to_string_lossy())));
    if let Some(others) = others {
        o.out(format!("Others registered: {}", if others.is_empty() { "none" } else { &others }));
    }
    o
}

/// A derived join that stepped away from tier 1 while tier 1 is still live: often the session's own earlier handle,
/// evicted as stale. Said loudly; never reclaimed (a fresh `last_seen` does not prove ownership).
fn stranding_warning(env: &Env, o: &mut Out, ctx: &Context, d: &Derived, outside: bool, cleared: &[(String, String)]) {
    if d.qualifier.is_empty() || d.tier1 == d.name {
        return;
    }
    let reg = read_doc(&ctx.registry()).ok();
    let agents = reg.as_ref().and_then(|r| r["agents"].as_object());
    if outside && !agents.is_some_and(|a| still_cleared(a, &d.tier1, cleared)) {
        return;
    }
    let live = agents
        .and_then(|a| a.get(&d.tier1))
        .and_then(|e| e.get("last_seen"))
        .and_then(Value::as_str)
        .is_some_and(|s| crate::comm::mail::filer::heartbeat_fresh(Some(s), unix_now_secs()));
    if !live {
        return;
    }
    let (n, t1, bin) = (shell_quote(&d.name), shell_quote(&d.tier1), shell_quote(&bin_dir(env)));
    let (name, tier1) = (&d.name, &d.tier1);
    o.err(format!(
        "\n*** WARNING: joined as '@{name}', but '@{tier1}' (this project's bare\n\
         *** handle) is still being heartbeated under this user: a session acted, or\n\
         *** the daemon running its row stamped it, in the last ten minutes. That is\n\
         *** often YOUR OWN earlier identity -- this session's own '@{tier1}'\n\
         *** row, evicted as stale (see comm-context.sh), so this join escalated away\n\
         *** from it instead of reclaiming it -- but a running row of another project\n\
         *** with this basename on this box looks the same. Proceeding with the\n\
         *** qualified join as '@{name}' (a fresh last_seen does not prove ownership,\n\
         *** so this is never auto-reclaimed); if '@{tier1}' IS your own\n\
         *** handle, you can now strand yourself silently: its inbox is the one still\n\
         *** being filled while everyone else now addresses you as '@{name}'.\n\
         ***\n\
         *** If you confirm sole ownership (one live session with this repo as cwd,\n\
         *** and no other row on this box holding '@{tier1}'), free the bare\n\
         *** handle and join again, bare: the derivation takes it.\n\
         ***   {bin}/comm-leave.sh --name {n}\n\
         ***   {bin}/comm-leave.sh --name {t1}\n\
         ***   {bin}/comm-join.sh"
    ));
}

/// Rows' work-state verb a join sends (`comm-status.sh join`): the turn this join runs in starts its floor from the
/// turn mark. Best effort, as 0.6.6's join sent it; only where the wrappers named the scripts' folder.
fn status_join(env: &Env) {
    let Some(bin) = env.get_nonempty("SOT_COMM_BIN") else { return };
    let script = std::path::PathBuf::from(bin).join("comm-status.sh");
    if !script.is_file() {
        return;
    }
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(&script).arg("join");
    let _ = sot_containment::process().output(&mut cmd, std::time::Duration::from_secs(15));
}

/// A row's declaration of its handle to the daemon that pinned its identity: best effort, one warning on failure, as
/// the next session start retries it.
fn declare(env: &Env, o: &mut Out, ctx: &Context, name: &str) {
    if ctx.row_id.is_empty() {
        return;
    }
    let Some(endpoint) = wire::own_daemon(env) else {
        o.err(format!(
            "comm-join.sh: WARNING \u{2014} this shell's identity names row '{}' but no daemon endpoint could be resolved; the daemon won't learn '@{name}' until the next comm-session-start.",
            ctx.row_id
        ));
        return;
    };
    let ok = wire::agent_join(env, &endpoint, &ctx.row_id, name).is_ok_and(|a| a.get("ok").and_then(Value::as_bool) == Some(true));
    if !ok {
        o.err(format!("comm-join.sh: WARNING \u{2014} could not declare '@{name}' to the daemon (agent.join); it will retry at the next comm-session-start."));
    }
}

#[cfg(test)]
#[path = "join_tests.rs"]
mod tests;
