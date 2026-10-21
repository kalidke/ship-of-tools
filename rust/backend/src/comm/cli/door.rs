//! The shell door's verbs: what `comm/lib/comm-lib-sotd.sh` turns into the functions rows' work-state scripts and the
//! agents' CLIs call by their 0.6.6 names, each answering as the function did (its exit codes, its one-line reasons).

use std::io::Read;

use serde_json::Value;

use super::{gate, routable as routable_check, Out};
use crate::comm::identity::derive::{derive as derive_handle, Kind, Mode};
use crate::comm::registry::registry::{command_lock_wait, iso8601_utc_now, read_doc, touch, update, TOUCH_LOCK_WAIT};

fn registry_path() -> std::path::PathBuf {
    crate::comm::home::comm_home().unwrap_or_default().join("registry.json")
}

fn usage(o: Out, text: &str) -> Out {
    let mut o = o;
    o.err(format!("usage: sotd comm {text}"));
    o.exit(2)
}

fn stdin_text() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

/// `require-agent` (`sot_require_agent`): exit 0 when this process may act as its row's handle; else its one reason on
/// stdout, exit 1 for a second agent inside the session, 2 for an unreadable ancestry or a row it does not run in.
pub(crate) fn require_agent(_args: &[String]) -> Out {
    let mut o = Out::default();
    match gate() {
        Ok(()) => o,
        Err((rc, why)) => {
            o.out(why);
            o.exit(rc)
        }
    }
}

/// `routable <name> <project root>`: exit 0, or its one reason on stdout and exit 1.
pub(crate) fn routable(args: &[String]) -> Out {
    let mut o = Out::default();
    let [name, root] = args else { return usage(o, "routable <name> <project root>") };
    match routable_check(name, root, &registry_path()) {
        Ok(()) => o,
        Err(why) => {
            o.out(why);
            o.exit(1)
        }
    }
}

/// `registry read [h] | put <h> | del <h> | touch <h> | del-provisional <h> <root> <nonce>`: `read` exits 0 present,
/// 1 absent, 2 unreadable, the JSON on stdout; `put` takes the entry on stdin; `del-provisional` exits 0 deleted, 2 not
/// the spawn's own any more, 1 failed.
/// Every write is one locked step; a failed one prints its FAILED line on stderr and exits 1.
pub(crate) fn registry(args: &[String]) -> Out {
    let mut o = Out::default();
    let path = registry_path();
    let failed = |mut o: Out, e: String| {
        o.err(format!("FAILED: {e}"));
        o.exit(1)
    };
    match args {
        [v] if v == "read" => match read_doc(&path) {
            Ok(doc) => {
                o.out(doc.to_string());
                o
            }
            Err(_) => o.exit(2),
        },
        [v, h] if v == "read" => match read_doc(&path) {
            Ok(doc) => match doc["agents"].get(h) {
                Some(e) => {
                    o.out(e.to_string());
                    o
                }
                None => o.exit(1),
            },
            Err(_) => o.exit(2),
        },
        [v, h] if v == "put" => {
            if h.is_empty() {
                o.err("registry_put: refusing to write an empty/blank handle");
                return o.exit(1);
            }
            let Ok(entry) = serde_json::from_str::<Value>(&stdin_text()) else {
                return failed(o, "the entry to write is not JSON, so nothing was written".into());
            };
            match update(&path, command_lock_wait(), |agents| {
                agents.insert(h.clone(), entry);
                Ok(((), true))
            }) {
                Ok(()) => o,
                Err(e) => failed(o, e),
            }
        }
        [v, h] if v == "del" => match update(&path, command_lock_wait(), |agents| Ok(((), agents.remove(h).is_some()))) {
            Ok(()) => o,
            Err(e) => failed(o, e),
        },
        [v, h] if v == "touch" => match touch(&path, h) {
            Ok(()) => o,
            Err(e) => failed(o, e),
        },
        [v, h, root, nonce] if v == "del-provisional" => {
            if h.is_empty() || nonce.is_empty() {
                return o.exit(2);
            }
            let deleted = update(&path, command_lock_wait(), |agents| {
                let ours = agents.get(h).is_some_and(|e| {
                    let s = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("");
                    s("status") == "spawning" && s("root") == root && s("nonce") == nonce
                });
                if ours {
                    agents.remove(h);
                }
                Ok((ours, ours))
            });
            match deleted {
                Ok(true) => o,
                Ok(false) => o.exit(2),
                Err(e) => failed(o, e),
            }
        }
        _ => usage(o, "registry <read [h]|put <h>|del <h>|touch <h>|del-provisional <h> <root> <nonce>>"),
    }
}

/// `derive <reclaim|fresh> <root> <raw host> [--claim]` (with `--claim`, the entry on stdin and its registry write in
/// the same locked step): three lines, the handle, its qualifier and tier 1's name; the derivation's lines on
/// stderr; exit 1 with nothing written when no tier can be taken.
pub(crate) fn derive(args: &[String]) -> Out {
    let mut o = Out::default();
    let (mode, root, host, claim) = match args {
        [m, r, h] => (m, r, h, false),
        [m, r, h, c] if c == "--claim" => (m, r, h, true),
        _ => return usage(o, "derive <reclaim|fresh> <root> <raw host> [--claim]"),
    };
    let mode = match mode.as_str() {
        "reclaim" => Mode::Reclaim,
        "fresh" => Mode::Fresh,
        other => {
            o.err(format!("sot_derive_handle: invalid mode '{other}' (want reclaim or fresh)"));
            return o.exit(1);
        }
    };
    let entry = if claim {
        match serde_json::from_str::<Value>(&stdin_text()) {
            Ok(e) => Some(e),
            Err(_) => {
                o.err("claim_derived_handle: the entry to write is not JSON; nothing was written");
                return o.exit(1);
            }
        }
    } else {
        None
    };
    let mut notes = Vec::new();
    let unreadable = "comm: the registry could not be read, so no handle was derived; nothing was written".to_string();
    let got = if let Some(entry) = entry {
        update(&registry_path(), command_lock_wait(), |agents| {
            let d = derive_handle(mode, root, host, Kind::Claude, agents, None, &mut notes)?;
            agents.insert(d.name.clone(), entry);
            Ok((d, true))
        })
        .map_err(|e| if e.starts_with("the registry could not be read") { unreadable.clone() } else { e })
    } else {
        read_doc(&registry_path())
            .map_err(|_| unreadable.clone())
            .and_then(|doc| derive_handle(mode, root, host, Kind::Claude, &doc["agents"].as_object().cloned().unwrap_or_default(), None, &mut notes))
    };
    o.errs(&notes);
    match got {
        Ok(d) => {
            o.out(&d.name);
            o.out(&d.qualifier);
            o.out(&d.tier1);
            o
        }
        Err(e) => {
            o.err(e);
            o.exit(1)
        }
    }
}

/// `self-slot <host> <workspace id>`: the unpinned self file's leaf, or its reason and exit 1.
pub(crate) fn self_slot(args: &[String]) -> Out {
    let mut o = Out::default();
    let (host, ws) = match args {
        [h] => (h.as_str(), ""),
        [h, w] => (h.as_str(), w.as_str()),
        _ => return usage(o, "self-slot <host> [workspace id]"),
    };
    match crate::comm::identity::self_file::slot_leaf(host, ws) {
        Ok(leaf) => {
            o.stdout = leaf;
            o
        }
        Err(e) => {
            o.err(e);
            o.exit(1)
        }
    }
}

/// `slug <label>`: the row slug a label gives, for the spawn's same-label refusal.
pub(crate) fn slug(args: &[String]) -> Out {
    let mut o = Out::default();
    match args {
        [label] => o.out(sot_protocol::topology::endpoint::slug(label)),
        _ => return usage(o, "slug <label>"),
    }
    o
}

/// `handoff-line <h>`: the line naming the session's recovery file, when it exists.
pub(crate) fn handoff_line(args: &[String]) -> Out {
    let mut o = Out::default();
    if let [h] = args {
        if let Some(line) = super::start::handoff_line(h, &std::env::current_dir().unwrap_or_default()) {
            o.out(line);
        }
    }
    o
}

/// `ensure-home`: the comm folder made whole, as the first command of a session makes it.
pub(crate) fn ensure_home(_args: &[String]) -> Out {
    let mut o = Out::default();
    match crate::comm::home::comm_home() {
        Some(h) => {
            o.errs(&crate::comm::home::ensure(&h));
            o
        }
        None => {
            o.err("ensure_home: no comm folder: none of SOT_COMM_HOME, HOME or USERPROFILE is set");
            o.exit(1)
        }
    }
}

/// `stop-mark <h>`: the Stop hook's mark, `stop_at`, which holds the wake off the row while its hook runs (rows'
/// `stop` deletes it). One try within a second; silent, as a missed mark only lets the wake look.
pub(crate) fn stop_mark(args: &[String]) -> Out {
    let o = Out::default();
    let [h] = args else { return usage(o, "stop-mark <handle>") };
    let marked = update(&registry_path(), TOUCH_LOCK_WAIT, |agents| {
        let Some(e) = agents.get_mut(h).and_then(Value::as_object_mut) else { return Ok(((), false)) };
        e.insert("stop_at".into(), iso8601_utc_now().into());
        Ok(((), true))
    });
    if marked.is_err() { o.exit(1) } else { o }
}

#[cfg(test)]
#[path = "door_tests.rs"]
mod tests;
