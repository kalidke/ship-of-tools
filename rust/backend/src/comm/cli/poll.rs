//! `sotd comm poll` (comm-poll.sh), the one reader of message bodies and the one cursor writer, and `sotd comm unread`,
//! the end-of-turn check's count.

use crate::comm::Env;
use super::{context, gate, Out};
use crate::comm::mail::cursor::{complete, counts, offset, write};
use crate::comm::mail::inbox::{appends_here, lock_identity, read_lock, ReadLock};
use crate::comm::registry::registry::{binding, touch, Binding};

/// A JSON value as `jq -r` prints it: a string raw, `null` and `false` as missing, anything else as JSON.
fn raw(v: Option<&serde_json::Value>) -> Option<String> {
    match v? {
        serde_json::Value::Null | serde_json::Value::Bool(false) => None,
        serde_json::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// One inbox line as poll shows it, `[ts] [from:repo] msg`, or `None` for a line that is not a JSON object (still
/// read: one bad line cannot pin the cursor).
pub(crate) fn shown(line: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(line).ok()?;
    let o = v.as_object()?;
    let f = |k: &str| raw(o.get(k)).unwrap_or_default();
    let msg = raw(o.get("msg")).or_else(|| raw(o.get("message"))).or_else(|| raw(o.get("text"))).unwrap_or_default();
    Some(format!("[{}] [{}:{}] {msg}", f("ts"), f("from"), f("repo")))
}

pub(crate) fn poll(env: &Env, _args: &[String]) -> Out {
    let mut o = Out::default();
    if let Err((_, why)) = gate(env) {
        o.err(format!("comm-poll: {why}"));
        return o.exit(1);
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
    let me = ctx.name.clone();
    if me.is_empty() {
        o.err("No handle for this process \u{2014} not this row's inbox (see any comm-context line above), or not joined: run comm-join.sh first.");
        return o.exit(1);
    }
    // The one reader of bodies asks the registry itself whose handle it is.
    match binding(&ctx.registry(), &me, &ctx.row_id) {
        Binding::Unreadable => {
            o.err("comm-poll: the registry could not be read; nothing was read");
            return o.exit(1);
        }
        Binding::Other { ws, .. } => {
            let carries = if ctx.row_id.is_empty() { "no row id".to_string() } else { ctx.row_id.clone() };
            o.err(format!("comm-poll: @{me} belongs to row {ws}, not to this process ({carries}); not this row's inbox, nothing was read"));
            return o.exit(1);
        }
        Binding::Owner | Binding::Unbound => {}
    }
    let path = ctx.inbox_dir().join(format!("{me}.jsonl"));
    if !path.is_file() {
        o.out("No messages.");
        let _ = touch(env, &ctx.registry(), &me);
        return o;
    }
    // The batch is read once under the shared lock, the lock let go, and only then shown.
    let own = lock_identity(&ctx.inbox_dir());
    let lock = read_lock(&ctx.inbox_dir(), &me, &own, appends_here(&ctx.comm_home, &own), env);
    if let ReadLock::Busy = lock {
        o.out(format!("the inbox for @{me} is being written \u{2014} nothing was read; run comm-poll.sh again"));
        return o.exit(75);
    }
    if let ReadLock::Fault(warning) = &lock {
        o.out(warning);
    }
    let at = offset(&ctx.comm_home, &me);
    if at.stepped_back {
        o.err(format!("note: the last line read from @{me}'s inbox was cut back; reading from the line before it"));
    }
    let bytes = std::fs::read(&path).unwrap_or_default();
    drop(lock);
    let lines: Vec<&[u8]> = complete(&bytes).split(|b| *b == b'\n').collect();
    let total = lines.len().saturating_sub(1) as u64;
    let mut count = 0;
    if total > at.lines {
        let batch = &lines[at.lines as usize..total as usize];
        for line in batch.iter().filter(|l| !l.is_empty()) {
            if let Some(s) = shown(line) {
                o.out(s);
                count += 1;
            }
        }
        if let Err(e) = write(&ctx.comm_home, &me, total, batch.last().copied().unwrap_or_default()) {
            o.err(format!("comm-poll: the read cursor could not be written ({e}); these lines show again at the next poll"));
        }
    }
    if count == 0 {
        o.out("No new messages.");
    }
    let _ = touch(env, &ctx.registry(), &me);
    o
}

/// `sotd comm unread <h>`: one line, `<total> <unread>`, then the lock fault's warning when there is one; exit 75 when
/// the inbox is being written (check again at the next turn end), 1 when the count cannot be made. Takes the shared
/// lock as poll does, and reads nothing else.
pub(crate) fn unread(env: &Env, args: &[String]) -> Out {
    let mut o = Out::default();
    let Some(h) = args.first().filter(|h| crate::comm::identity::derive::valid_handle(h)) else {
        o.err("usage: sotd comm unread <handle>");
        return o.exit(2);
    };
    let Some(home) = crate::comm::home::comm_home(env) else {
        o.err("sot-comm: cannot count unread mail: no comm folder");
        return o.exit(1);
    };
    let dir = home.join("inbox");
    let own = lock_identity(&dir);
    let lock = read_lock(&dir, h, &own, appends_here(&home, &own), env);
    if let ReadLock::Busy = lock {
        return o.exit(75);
    }
    let warning = match &lock {
        ReadLock::Fault(w) => Some(w.clone()),
        _ => None,
    };
    let bytes = std::fs::read(dir.join(format!("{h}.jsonl"))).unwrap_or_default();
    let at = offset(&home, h).lines;
    drop(lock);
    let lines: Vec<&[u8]> = complete(&bytes).split(|b| *b == b'\n').collect();
    let total = lines.len().saturating_sub(1);
    let n = lines.iter().take(total).skip(at as usize).filter(|l| counts(l, h)).count();
    o.out(format!("{total} {n}"));
    if let Some(w) = warning {
        o.out(w);
    }
    o
}

#[cfg(test)]
#[path = "poll_tests.rs"]
mod tests;
