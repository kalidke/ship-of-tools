//! Work-state: the facts a row's session and the daemon set on the row's comm registry entry, the one reduction that
//! turns them into the state the window shows, the one write every setter takes ([`stamp`]) and the one read every
//! reader takes ([`shown`]). The contract is rows' (rows/CLAUDE.md, work-state).

pub(crate) mod cli;
pub(crate) mod dialog;
pub(crate) mod poll;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::comm::registry::registry::{edit_entry, host_matches, iso8601_utc_now, EditError};
use crate::rows::handle::bound_handle;
use crate::rows::Workspace;

/// Who started a turn: the `floor` fact's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    User,
    Machine,
}

impl Origin {
    fn word(self) -> &'static str {
        match self {
            Origin::User => "user",
            Origin::Machine => "machine",
        }
    }
}

/// A session's own word on its state: `comm-status.sh working|idle|blocked|waiting|done`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Declaration {
    Working,
    Idle,
    Blocked,
    Waiting,
    Done,
}

/// One write of work-state facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// A turn starts: sets `floor` to its origin; a person's prompt answers a question and reads a result.
    Prompt(Origin),
    /// A turn ends: `done` when a person's turn ended with nothing else pending; the floor and any dialog go.
    Stop,
    /// A dialog that waits on a person shows (a Codex hook's, or the daemon's read of a Claude screen).
    Dialog(String),
    /// The dialog is gone.
    Resumed,
    /// A busy turn's heartbeat: restamps the time of a row whose turn runs, and sets no fact.
    Tick,
    /// A person looked at the row (`workspace.activate` with `read: true`): clears `done`, restamps nothing.
    Read,
    /// A declaration, with its text when one was given; `None` keeps the last line (`note`).
    Declare(Declaration, Option<String>),
}

/// A question or a wait with no text of its own: nothing is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Refused;

/// Whether `entry` holds `key` with a value that is not null (jq's `.key != null`).
fn is_set(entry: &Map<String, Value>, key: &str) -> bool {
    entry.get(key).is_some_and(|v| !v.is_null())
}

fn text_of<'a>(entry: &'a Map<String, Value>, key: &str) -> &'a str {
    entry.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The state the facts reduce to, by precedence: a dialog, a question while no turn runs, a turn, a wait, done, idle.
fn reduce_state(entry: &Map<String, Value>) -> &'static str {
    if is_set(entry, "dialog") || (is_set(entry, "question") && !is_set(entry, "floor")) {
        "blocked"
    } else if is_set(entry, "floor") {
        "working"
    } else if is_set(entry, "waiting") {
        "waiting"
    } else if entry.get("done") == Some(&Value::Bool(true)) {
        "done"
    } else {
        "idle"
    }
}

/// `state` and `summary` from the facts: the one reduction (rows/CLAUDE.md, work-state).
fn reduce(entry: &mut Map<String, Value>) {
    let state = reduce_state(entry);
    let summary = if is_set(entry, "dialog") {
        entry.get("dialog").cloned().unwrap_or(Value::Null)
    } else if state == "blocked" {
        entry.get("question").cloned().unwrap_or(Value::Null)
    } else if state == "waiting" {
        entry.get("waiting").cloned().unwrap_or(Value::Null)
    } else {
        entry.get("note").filter(|v| !v.is_null()).cloned().unwrap_or_else(|| Value::String(String::new()))
    };
    entry.insert("state".into(), Value::String(state.into()));
    entry.insert("summary".into(), summary);
}

fn set(entry: &mut Map<String, Value>, key: &str, text: &str) {
    entry.insert(key.into(), Value::String(text.into()));
}

fn drop_keys(entry: &mut Map<String, Value>, keys: &[&str]) {
    for k in keys {
        entry.remove(*k);
    }
}

/// Apply `event` to one registry entry at `now`, re-reducing its state; whether the entry changed and must be written.
/// A `blocked` or `waiting` with no text, on an entry whose question or wait has none of its own, is [`Refused`], as is
/// a dialog with no text.
pub(crate) fn apply(entry: &mut Map<String, Value>, event: &Event, now: &str) -> Result<bool, Refused> {
    match event {
        Event::Tick => {
            if !is_set(entry, "floor") {
                return Ok(false);
            }
            set(entry, "status_at", now);
            set(entry, "last_seen", now);
            return Ok(true);
        }
        Event::Read => {
            if !entry.contains_key("done") {
                return Ok(false);
            }
            entry.remove("done");
            let state = reduce_state(entry);
            set(entry, "state", state);
            return Ok(true);
        }
        Event::Dialog(text) if text.is_empty() => return Err(Refused),
        Event::Declare(d @ (Declaration::Blocked | Declaration::Waiting), text) if text.as_deref().unwrap_or("").is_empty() => {
            let own = if *d == Declaration::Blocked { "question" } else { "waiting" };
            if text_of(entry, own).chars().all(|c| c == '\n') {
                return Err(Refused);
            }
        }
        _ => {}
    }
    drop_keys(entry, &["turn_origin", "sticky", "sticky_at"]);
    match event {
        Event::Prompt(origin) => {
            entry.remove("dialog");
            set(entry, "floor", origin.word());
            if *origin == Origin::User {
                drop_keys(entry, &["question", "done"]);
            }
        }
        Event::Stop => {
            if text_of(entry, "floor") == "user" && !is_set(entry, "question") && !is_set(entry, "waiting") {
                entry.insert("done".into(), Value::Bool(true));
            }
            drop_keys(entry, &["floor", "stop_at", "dialog"]);
        }
        Event::Dialog(text) => set(entry, "dialog", text),
        Event::Resumed => {
            entry.remove("dialog");
        }
        // A question's or a wait's text supersedes the older line, which goes; neither copies its text into `note`.
        Event::Declare(Declaration::Blocked, text) => {
            if let Some(t) = text.as_deref().filter(|t| !t.is_empty()) {
                set(entry, "question", t);
                entry.remove("note");
            }
        }
        Event::Declare(Declaration::Waiting, text) => {
            if let Some(t) = text.as_deref().filter(|t| !t.is_empty()) {
                set(entry, "waiting", t);
                entry.remove("note");
            }
            entry.remove("question");
        }
        Event::Declare(Declaration::Done, text) => {
            if let Some(t) = text {
                set(entry, "note", t);
            }
            entry.insert("done".into(), Value::Bool(true));
            drop_keys(entry, &["question", "waiting"]);
        }
        Event::Declare(Declaration::Working | Declaration::Idle, text) => {
            if let Some(t) = text {
                set(entry, "note", t);
            }
            drop_keys(entry, &["question", "waiting", "done"]);
        }
        Event::Tick | Event::Read => unreachable!("answered above"),
    }
    reduce(entry);
    set(entry, "status_at", now);
    set(entry, "last_seen", now);
    Ok(true)
}

/// What [`stamp`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Stamped {
    Written,
    /// The event changed nothing (a tick with no turn running, a read with nothing to clear).
    Unchanged,
    /// No registry, or no entry for the handle: nothing to stamp.
    NoEntry,
}

/// Why [`stamp`] wrote nothing.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StampError {
    Refused,
    Registry(EditError),
}

/// The one write: `event` applied to `.agents[handle]` under the registry lock, waiting at most `lock_wait` for it.
pub(crate) fn stamp(handle: &str, event: &Event, lock_wait: Duration) -> Result<Stamped, StampError> {
    let now = iso8601_utc_now();
    match edit_entry(handle, lock_wait, |entry| match apply(entry, event, &now) {
        Ok(write) => (Ok(write), write),
        Err(refused) => (Err(refused), false),
    }) {
        Ok(None) => Ok(Stamped::NoEntry),
        Ok(Some(Ok(true))) => Ok(Stamped::Written),
        Ok(Some(Ok(false))) => Ok(Stamped::Unchanged),
        Ok(Some(Err(Refused))) => Err(StampError::Refused),
        Err(e) => Err(StampError::Registry(e)),
    }
}

/// The read-clear's lock bound: `workspace.activate` waits for it inline, so it is short; a person's next look retries.
const READ_LOCK_WAIT: Duration = Duration::from_secs(1);

/// A person looked at row `ws` (`workspace.activate` with `read: true`): its `done` fact cleared through [`stamp`], on
/// its bound handle's entry and only an entry of `host`. Best-effort: a held lock or an unreadable registry is a no-op
/// the next look retries. Blocks: run it on a blocking thread.
pub(crate) fn read_clear(ws: &Workspace, rows: &[Arc<Workspace>], host: &str) {
    let handle = bound_handle(ws, rows);
    if handle.is_empty() {
        return;
    }
    // A cheap unlocked filter: only this host's entry with a `done` fact is worth the lock.
    let worth = crate::comm::registry::registry::read_comm_agents()
        .and_then(|a| a.get(&handle).cloned())
        .is_some_and(|e| host_matches(&e, host) && e.get("done").is_some());
    if !worth {
        return;
    }
    let host = host.to_string();
    let _ = edit_entry(&handle, READ_LOCK_WAIT, |entry| {
        if !host_matches(&Value::Object(entry.clone()), &host) {
            return ((), false);
        }
        let write = apply(entry, &Event::Read, "").unwrap_or(false);
        ((), write)
    });
}

/// What the window shows for one row: its state, its summary and the time it was set, each `""` when unknown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Shown {
    pub state: String,
    pub summary: String,
    pub status_at: String,
}

/// The one read: what row `ws` shows, from `agents` (the registry's `.agents`, read once per caller) through the row's
/// bound handle, and only from an entry of `host`, so another host's same-named entry never colors this host's row.
pub(crate) fn shown(ws: &Workspace, rows: &[Arc<Workspace>], agents: Option<&Value>, host: &str) -> Shown {
    let handle = bound_handle(ws, rows);
    if handle.is_empty() {
        return Shown::default();
    }
    let Some(entry) = agents.and_then(|a| a.get(&handle)).filter(|e| host_matches(e, host)) else {
        return Shown::default();
    };
    let field = |k: &str| entry.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Shown { state: field("state"), summary: field("summary"), status_at: field("status_at") }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
