//! A sidecar child's stderr: read to its end on a task of its own, each line logged at debug under its owner's target
//! (a healthy Julia is chatty), the last lines kept, so the owner of a child that failed logs the child's own words at
//! warn, and a caller still waiting on a child that has not said it is ready is told how long it has been starting and
//! what it last wrote (`Start`; `last_lines` is the one way a caller words a child's last lines). The daemon logs at info by default, where the debug lines are dropped.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How many of a child's last stderr lines are kept.
const TAIL_LINES: usize = 30;
/// How long a failed child's owner waits for its stderr to end, once the child is ended, so its last words are read:
/// a descendant that still holds the pipe never lets it end.
const END_WAIT: Duration = Duration::from_secs(1);
/// How many of a starting child's last stderr lines a caller is told, and the characters kept of each: an answer stays
/// one short line.
const STARTING_LINES: usize = 5;
const STARTING_LINE_CHARS: usize = 200;

pub(crate) struct StderrTail {
    /// The last `TAIL_LINES` lines, newest first.
    lines: Arc<Mutex<VecDeque<String>>>,
    ended: tokio::sync::watch::Receiver<bool>,
    /// When the drain began, right after the child's spawn: the age of its start.
    since: Instant,
}

/// A child's start as a caller still waiting on it sees it: when the child was started and the lines its stderr tail
/// keeps, read while the child runs.
#[derive(Clone)]
pub(crate) struct Start {
    since: Instant,
    lines: Arc<Mutex<VecDeque<String>>>,
}

impl Start {
    /// The one phrase for a child that has not said it is ready, in an answer and in the log: `<who> is still starting
    /// after <N> s; its last stderr lines: <oldest> / ... / <newest>` (`Start::last_lines`), or `<who> is still
    /// starting after <N> s; it has written nothing to stderr`.
    pub(crate) fn still_starting(&self, who: &str) -> String {
        let secs = self.since.elapsed().as_secs();
        match self.last_lines() {
            said if said.is_empty() => format!("{who} is still starting after {secs} s; it has written nothing to stderr"),
            said => format!("{who} is still starting after {secs} s; its last stderr lines: {said}"),
        }
    }

    /// The last `STARTING_LINES` lines its stderr has written that hold text (`last_lines`), or nothing when it has
    /// written none.
    pub(crate) fn last_lines(&self) -> String {
        let kept = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        last_lines(kept.iter().rev().map(String::as_str), STARTING_LINES, STARTING_LINE_CHARS)
    }
}

/// The last `count` of `lines`, oldest first, that hold text: each trimmed and cut at `chars` characters, joined by
/// ` / `. A caller names what a child last said in one short line; none held gives the empty string.
pub(crate) fn last_lines<'a>(lines: impl DoubleEndedIterator<Item = &'a str>, count: usize, chars: usize) -> String {
    let mut said: Vec<String> = lines
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rev()
        .take(count)
        .map(|line| line.chars().take(chars).collect())
        .collect();
    said.reverse();
    said.join(" / ")
}

impl StderrTail {
    /// Reads `stderr` until it ends, through the wire's `ChildLines`; `log` writes each line at debug.
    pub(crate) fn drain(stderr: tokio::process::ChildStderr, log: fn(&str)) -> StderrTail {
        let (lines, (end, ended)) = (Arc::new(Mutex::new(VecDeque::new())), tokio::sync::watch::channel(false));
        let kept = Arc::clone(&lines);
        tokio::spawn(async move {
            let mut reader = sot_protocol::child_lines::ChildLines::new(stderr);
            while let Ok(Some(line)) = reader.next_line().await {
                log(&line);
                let mut kept = kept.lock().unwrap_or_else(|e| e.into_inner());
                kept.truncate(TAIL_LINES - 1);
                kept.push_front(line);
            }
            let _ = end.send(true);
        });
        StderrTail { lines, ended, since: Instant::now() }
    }

    /// This child's start, for a caller still waiting on it (`Start::still_starting`).
    pub(crate) fn start(&self) -> Start {
        Start { since: self.since, lines: Arc::clone(&self.lines) }
    }

    /// Logs `what` at warn with the kept lines, oldest first, once stderr has ended or `END_WAIT` has passed; nothing
    /// when the child wrote no stderr. Its owner calls it after the child is ended.
    pub(crate) async fn warn(mut self, what: &str) {
        let _ = tokio::time::timeout(END_WAIT, self.ended.wait_for(|ended| *ended)).await;
        let lines: Vec<String> = self.lines.lock().unwrap_or_else(|e| e.into_inner()).iter().rev().cloned().collect();
        if !lines.is_empty() {
            tracing::warn!(stderr = ?lines, "{what}; the child's last stderr lines follow");
        }
    }
}

#[cfg(all(test, unix))]
#[path = "stderr_tail_tests.rs"]
mod tests;
