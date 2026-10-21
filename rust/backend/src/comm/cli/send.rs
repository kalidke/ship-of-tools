//! `sotd comm send` (comm-send.sh) and `sotd comm relay send` (comm-relay.sh): one message, one verdict. A handle this
//! box's registry lists is filed here: by this process when it takes the folder's lock itself, else by the daemon
//! that owns the folder. One it does not list goes to the hub, which files it or offers it to its linked daemons.

use crate::comm::Env;
use std::io::Read;

use super::{context, gate, routable, wire, Out};
use crate::comm::mail::{filer, inbox};
use crate::comm::registry::registry::{entry, read_doc, touch, unix_now_secs, Entry};

const USAGE: &str = "usage: comm-send.sh @name 'msg' | @name --file <path | -> | @name - | --broadcast 'msg'";

/// What a send was asked to do.
#[derive(Debug, Default, PartialEq, Eq)]
struct Ask {
    broadcast: bool,
    target: String,
    /// The text given as arguments, joined by spaces.
    text: String,
    /// `--file <path>`; `-` is standard input.
    file: Option<String>,
}

/// comm-send.sh's arguments: the first positional `@arg` before any text is the recipient, so a message may itself
/// begin with `@`; a message of exactly `-` is standard input, as `--file -`.
fn parse_send(args: &[String]) -> Result<Ask, String> {
    let mut a = Ask::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--broadcast" => a.broadcast = true,
            "--file" => {
                if a.file.is_some() {
                    return Err("usage: comm-send.sh @name --file <path | -> (one file)".into());
                }
                a.file = Some(it.next().ok_or("usage: comm-send.sh @name --file <path | -> (one file)")?.clone());
            }
            s if a.target.is_empty() && a.text.is_empty() && !a.broadcast && s.starts_with('@') => a.target = s[1..].to_string(),
            s if a.text.is_empty() => a.text = s.to_string(),
            s => {
                a.text.push(' ');
                a.text.push_str(s);
            }
        }
    }
    if a.file.is_none() && a.text == "-" {
        a.text.clear();
        a.file = Some("-".into());
    }
    if a.file.is_some() && !a.text.is_empty() {
        return Err("usage: comm-send.sh @name --file <path | -> takes the whole message; give text or --file, not both".into());
    }
    if a.file.is_none() && a.text.is_empty() {
        return Err(USAGE.into());
    }
    Ok(a)
}

/// comm-relay.sh's arguments: `send`, then the first `@arg` anywhere is the recipient and every other argument is text.
fn parse_relay(args: &[String]) -> Result<Ask, String> {
    let usage = |why: &str| format!("usage: comm-relay.sh send @to \"msg\"  ({why})");
    let Some(("send", rest)) = args.split_first().map(|(v, r)| (v.as_str(), r)) else {
        return Err(usage("the one verb is send"));
    };
    let mut a = Ask::default();
    let mut words: Vec<&str> = Vec::new();
    for arg in rest {
        match arg.strip_prefix('@') {
            Some(to) if a.target.is_empty() => a.target = to.to_string(),
            _ => words.push(arg),
        }
    }
    a.text = words.join(" ");
    if a.target.is_empty() {
        return Err(usage("no recipient"));
    }
    if a.text.is_empty() {
        return Err(usage("empty message"));
    }
    Ok(a)
}

pub(crate) fn send(env: &Env, args: &[String]) -> Out {
    match parse_send(args) {
        Ok(ask) => deliver_all(env, ask, read_input),
        Err(usage) => {
            let mut o = Out::default();
            o.err(usage);
            o.exit(1)
        }
    }
}

pub(crate) fn relay(env: &Env, args: &[String]) -> Out {
    match parse_relay(args) {
        Ok(ask) => deliver_all(env, ask, read_input),
        Err(usage) => {
            let mut o = Out::default();
            o.err(usage);
            o.exit(1)
        }
    }
}

/// A `--file` body: the file's bytes, or standard input's for `-`, read whole.
fn read_input(file: &str) -> std::io::Result<String> {
    if file == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        Ok(s)
    } else {
        std::fs::read_to_string(file)
    }
}

/// The send: the gate, the identity, the body, then each target's verdict, and the sender's own `last_seen` touch.
fn deliver_all(env: &Env, ask: Ask, read: impl FnOnce(&str) -> std::io::Result<String>) -> Out {
    let mut o = Out::default();
    let label = if ask.broadcast { "--broadcast".to_string() } else { format!("@{}", ask.target) };
    let refuse = |mut o: Out, why: &str| {
        o.err(format!("FAILED -> {label}: {why}"));
        o.exit(1)
    };
    if let Err((_, why)) = gate(env) {
        return refuse(o, &why);
    }
    let mut notes = Vec::new();
    let ctx = match context(env, false, &mut notes) {
        Ok(c) => c,
        Err(e) => {
            o.errs(&notes);
            o.err(e);
            return o.exit(1);
        }
    };
    o.errs(&notes);
    if let Err(why) = routable(env, &ctx.name, &ctx.project_root, &ctx.registry()) {
        return refuse(o, &why);
    }
    let text = match &ask.file {
        None => ask.text.clone(),
        Some(f) => match read(f) {
            Ok(t) if t.is_empty() => return refuse(o, &format!("the message file '{f}' is empty; nothing was sent")),
            Ok(t) => t,
            Err(_) => return refuse(o, &format!("cannot read the message file '{f}'")),
        },
    };
    let code = if ask.broadcast {
        let doc = match read_doc(&ctx.registry()) {
            Ok(d) => d,
            Err(_) => return refuse(o, "the registry could not be read; nothing was sent"),
        };
        let targets: Vec<String> = doc["agents"].as_object().map(|a| a.keys().filter(|k| **k != ctx.name).cloned().collect()).unwrap_or_default();
        let filed = targets.iter().filter(|t| deliver(env, &mut o, &ctx, t, true, &text)).count();
        if filed == targets.len() {
            o.out(format!("Broadcast to {filed} agent(s)."));
            0
        } else {
            o.out(format!("Broadcast to {filed} of {} agent(s).", targets.len()));
            1
        }
    } else if ask.target.is_empty() {
        o.err("no target; use @name or --broadcast");
        1
    } else if deliver(env, &mut o, &ctx, &ask.target, false, &text) {
        0
    } else {
        1
    };
    let _ = touch(env, &ctx.registry(), &ctx.name);
    o.exit(code)
}

/// One target's verdict, printed: `  filed -> @t` for a line this box's folder took (by this process or its daemon),
/// `filed -> @t` for one the hub took, else `FAILED -> @t: <why>`. True when filed.
fn deliver(env: &Env, o: &mut Out, ctx: &crate::comm::identity::context::Context, t: &str, broadcast: bool, text: &str) -> bool {
    let fail = |o: &mut Out, why: &str| {
        o.err(format!("FAILED -> @{t}: {why}"));
        false
    };
    let row = match entry(&ctx.registry(), t) {
        Entry::Unreadable => return fail(o, &format!("the registry could not be read, so @{t} could not be routed; nothing was sent")),
        Entry::Absent => None,
        Entry::Present(e) => Some(e),
    };
    let host = row.as_ref().and_then(|e| e.get("host")).and_then(serde_json::Value::as_str).unwrap_or("");
    if host.is_empty() {
        if broadcast {
            o.err(format!("no such handle: {t}"));
            return false;
        }
        // Not listed here: the hub's to file, or to offer to the box whose folder lists it.
        let endpoint = match wire::relay_endpoint(env) {
            Ok(e) => e,
            Err(e) => return fail(o, &format!("no sotd daemon found; `sotd topology relay-endpoint` gave no endpoint for this box ({e})")),
        };
        return match wire::comm_file(env, &endpoint, &ctx.name, t, text, false) {
            Ok(()) => {
                o.out(format!("filed -> @{t}"));
                true
            }
            Err(why) => fail(o, &why),
        };
    }
    let last_seen = row.as_ref().and_then(|e| e.get("last_seen")).and_then(serde_json::Value::as_str);
    if !filer::heartbeat_fresh(last_seen, unix_now_secs()) {
        return fail(o, &format!("no live session holds @{t}"));
    }
    let own = inbox::lock_identity(&ctx.inbox_dir());
    let filed = if inbox::appends_here(&ctx.comm_home, &own) {
        filer::file_here(&ctx.comm_home, &ctx.name, &ctx.repo, t, broadcast, text, unix_now_secs(), inbox::inbox_lock_wait(env), &own)
            .map_err(|(_, why)| why)
    } else {
        // The daemon that owns this folder: this box's own, else the hub (a box that shares the home and runs none).
        match wire::own_daemon(env).map_or_else(|| wire::relay_endpoint(env), Ok) {
            Ok(endpoint) => wire::comm_file(env, &endpoint, &ctx.name, t, text, broadcast),
            Err(_) => Err("this box cannot take the inbox lock itself, and no daemon is reachable to file it".into()),
        }
    };
    match filed {
        Ok(()) => {
            o.out(format!("  filed -> @{t}"));
            true
        }
        Err(why) => fail(o, &why),
    }
}

#[cfg(test)]
#[path = "send_tests.rs"]
mod tests;
