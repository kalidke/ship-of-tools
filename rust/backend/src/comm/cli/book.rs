//! The address book's commands: `context` (comm-context.sh), `leave` (comm-leave.sh) and `list` (comm-list.sh).

use crate::comm::Env;
use super::{context as ctx_of, gate, Out};
use crate::comm::registry::registry::{binding, command_lock_wait, entry, read_doc, unix_now_secs, update, Binding, Entry};

/// `sotd comm context [--rebind]`: the identity, eval-able. The comm folder is made whole only when the gate passes;
/// the identity is read either way.
pub(crate) fn context(env: &Env, args: &[String]) -> Out {
    let mut o = Out::default();
    let rebind = args.first().is_some_and(|a| a == "--rebind");
    let mut notes = Vec::new();
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            o.err(format!("comm-context: no working folder ({e})"));
            return o.exit(1);
        }
    };
    let resolved = crate::comm::identity::context::resolve(env, rebind, &cwd, || gate(env).is_ok(), &mut notes);
    o.errs(&notes);
    match resolved {
        Ok(c) => {
            o.stdout = c.eval_lines();
            o
        }
        Err(e) => {
            o.err(e);
            o.exit(1)
        }
    }
}

/// `sotd comm leave [--name <h>]`: this session's own entry and self file, or (`--name`) another handle's entry
/// only, refused for a handle a row holds unless the daemon says no row does.
pub(crate) fn leave(env: &Env, args: &[String]) -> Out {
    let mut o = Out::default();
    let mut who = String::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--name" => match it.next() {
                Some(h) if !h.is_empty() => who = h.clone(),
                _ => {
                    o.err("--name needs a handle");
                    return o.exit(1);
                }
            },
            s if s.starts_with("--name=") => {
                who = s["--name=".len()..].to_string();
                if who.is_empty() {
                    o.err("--name= needs a handle");
                    return o.exit(1);
                }
            }
            _ => {
                o.err("usage: comm-leave.sh [--name <handle>]");
                return o.exit(1);
            }
        }
    }
    if let Err((_, why)) = gate(env) {
        o.err(format!("comm-leave.sh: {why}"));
        return o.exit(1);
    }
    let mut notes = Vec::new();
    let ctx = match ctx_of(env, false, &mut notes) {
        Ok(c) => c,
        Err(e) => {
            o.errs(&notes);
            o.err(e);
            return o.exit(1);
        }
    };
    o.errs(&notes);
    let del = |h: &str| {
        update(env, &ctx.registry(), command_lock_wait(env), |agents| {
            let gone = agents.remove(h).is_some();
            Ok(((), gone))
        })
    };
    if !who.is_empty() && who != ctx.name {
        match entry(&ctx.registry(), &who) {
            Entry::Present(_) => {}
            Entry::Absent => {
                o.out(format!("@{who} not in registry \u{2014} nothing to do."));
                return o;
            }
            Entry::Unreadable => {
                o.err("FAILED: the registry could not be read; nothing was removed");
                return o.exit(1);
            }
        }
        if let Binding::Other { .. } = binding(&ctx.registry(), &who, &ctx.row_id) {
            match super::join::outside_clearance(env, &ctx, &who) {
                Ok(_) => {}
                Err((2, why)) => {
                    o.err(format!("comm-leave.sh: FAILED: {why}"));
                    return o.exit(1);
                }
                Err((_, why)) => {
                    o.err(format!("comm-leave.sh: REFUSING to remove @{who}: {}", why.replace(" cannot join as it", " cannot remove it")));
                    return o.exit(1);
                }
            }
        }
        if let Err(e) = del(&who) {
            o.err(format!("FAILED: {e}"));
            return o.exit(1);
        }
        o.out(format!("Removed @{who} from the registry (row only; comm-despawn.sh does full teardown)."));
        return o;
    }
    if ctx.name.is_empty() {
        o.out("Not joined \u{2014} nothing to do.");
        return o;
    }
    if let Err(e) = del(&ctx.name) {
        o.err(format!("FAILED: {e}"));
        return o.exit(1);
    }
    let _ = std::fs::remove_file(&ctx.self_file);
    o.out(format!("Left sot-comm (@{} removed).", ctx.name));
    o
}

/// A compact age: `just now`, `2m ago`, `1h ago`, `3d ago`.
pub(crate) fn fmt_age(secs: u64) -> String {
    match secs {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// A `YYYY-MM-DDTHH:MM:SSZ` stamp in seconds since the epoch, or `None`.
pub(crate) fn epoch_of(stamp: &str) -> Option<u64> {
    let b = stamp.as_bytes();
    if b.len() != 20 || b[19] != b'Z' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| stamp.get(r)?.parse::<i64>().ok();
    let (y, m, d, hh, mm, ss) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from the civil date (Howard Hinnant's algorithm), the inverse of the registry's own stamp.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

/// `sotd comm list`: every registered handle with its liveness and work-state line, then the frontend boxes this
/// daemon talks to (`sot-fe version`, bounded at 5 s).
pub(crate) fn list(env: &Env, _args: &[String]) -> Out {
    let mut o = Out::default();
    let mut notes = Vec::new();
    let cwd = std::env::current_dir().unwrap_or_default();
    let ctx = match crate::comm::identity::context::resolve(env, false, &cwd, || gate(env).is_ok(), &mut notes) {
        Ok(c) => c,
        Err(e) => {
            o.errs(&notes);
            o.err(e);
            return o.exit(1);
        }
    };
    o.errs(&notes);
    let Ok(doc) = read_doc(&ctx.registry()) else {
        o.err("FAILED: the registry could not be read; nothing was listed");
        return o.exit(1);
    };
    let now = unix_now_secs();
    o.out(format!("sot-comm agents  ({}):", crate::comm::identity::paths::native(&ctx.registry().to_string_lossy())));
    let agents = doc["agents"].as_object().cloned().unwrap_or_default();
    for (name, e) in &agents {
        let s = |k: &str| e.get(k).and_then(serde_json::Value::as_str).unwrap_or("").to_string();
        let seen = s("last_seen");
        let status = if crate::comm::mail::filer::heartbeat_fresh(Some(&seen), now) {
            "live".to_string()
        } else {
            format!("stale {}s", now.saturating_sub(epoch_of(&seen).unwrap_or(0)))
        };
        let exp: Vec<String> = e.get("expertise").and_then(serde_json::Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
        let me = if *name == ctx.name { "  <- me" } else { "" };
        o.out(format!("  @{name:<18} {:<10} {:<14} {status:<12} [{}]{me}", s("host"), s("repo"), exp.join("/")));
        let state = Some(s("state")).filter(|v| !v.is_empty()).unwrap_or_else(|| "idle".into());
        let mut line = format!("    [{state}]");
        if !s("summary").is_empty() {
            line.push(' ');
            line.push_str(&s("summary"));
        }
        if let Some(at) = epoch_of(&s("status_at")).filter(|t| *t > 0) {
            line.push_str(&format!(" \u{b7} {}", fmt_age(now.saturating_sub(at))));
        }
        o.out(line);
    }
    if agents.is_empty() {
        o.out("  (none)");
    }
    o.out("");
    let fe = std::path::PathBuf::from(super::bin_dir(env)).join("sot-fe");
    let mut cmd = std::process::Command::new(&fe);
    cmd.arg("version");
    let fe_out = sot_containment::process()
        .output(&mut cmd, std::time::Duration::from_secs(5))
        .map(|r| String::from_utf8_lossy(&r.stdout).into_owned())
        .unwrap_or_default();
    let uptime = fe_out.split(", up ").nth(1).and_then(|r| r.split(')').next()).map(str::to_string);
    match uptime {
        Some(u) => o.out(format!("attached frontend boxes (this daemon, up {u}):")),
        None => o.out("attached frontend boxes (this daemon):"),
    }
    let mut keep = false;
    let mut rows = Vec::new();
    for l in fe_out.lines() {
        if l.starts_with("fe@") {
            keep = true;
            rows.push(l);
        } else if keep && l.starts_with(' ') {
            rows.push(l);
        } else {
            keep = false;
        }
    }
    if rows.is_empty() {
        o.out("  (none attached, or no daemon answered)");
    }
    for r in rows {
        o.out(format!("  {r}"));
    }
    o
}

#[cfg(test)]
#[path = "book_tests.rs"]
mod tests;
