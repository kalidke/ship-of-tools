//! A sidecar child's stderr: read to its end on a task of its own, each line logged at debug under its owner's target
//! (a healthy Julia is chatty), the last lines kept, so the owner of a child that failed logs the child's own words at
//! warn, and a caller still waiting on a child that has not said it is ready is told how long it has been starting and
//! what it last wrote (`Start`). The daemon logs at info by default, where the debug lines are dropped.

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
    /// after <N> s; its last stderr lines: <oldest> / ... / <newest>` (the last `STARTING_LINES` that hold text, each
    /// trimmed and cut at `STARTING_LINE_CHARS`), or `<who> is still starting after <N> s; it has written nothing to
    /// stderr`.
    pub(crate) fn still_starting(&self, who: &str) -> String {
        let secs = self.since.elapsed().as_secs();
        let mut said: Vec<String> = {
            let kept = self.lines.lock().unwrap_or_else(|e| e.into_inner());
            kept.iter()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .take(STARTING_LINES)
                .map(|line| line.chars().take(STARTING_LINE_CHARS).collect())
                .collect()
        };
        if said.is_empty() {
            return format!("{who} is still starting after {secs} s; it has written nothing to stderr");
        }
        said.reverse();
        format!("{who} is still starting after {secs} s; its last stderr lines: {}", said.join(" / "))
    }
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
mod tests {
    use super::{StderrTail, STARTING_LINES, TAIL_LINES};
    use crate::lifecycle::child_signal::Signal;
    use std::process::Stdio;

    /// Ends a `/bin/sh -c script` child and logs its tail under `what`.
    async fn run(sig: &'static Signal, script: &str, what: &str) {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", script]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        let mut child = sig.spawn(&mut cmd).unwrap();
        let tail = StderrTail::drain(child.stderr.take().unwrap(), |_| {});
        child.wait().await.unwrap();
        tail.warn(what).await;
    }

    /// The last `TAIL_LINES` lines reach the log at warn, the earlier ones do not, and a child that wrote none logs
    /// nothing.
    #[tokio::test]
    async fn a_child_s_last_lines_are_logged_at_warn() {
        let log = sot_log::test_log::capture();
        let warned = |needle: &str| crate::sidecars::warned(&log.text(), needle);
        let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
        run(sig, "i=0; while [ $i -lt 40 ]; do echo \"line-$i.\" >&2; i=$((i + 1)); done", "the child ended").await;
        let first_kept = 40 - TAIL_LINES;
        assert!(warned("the child ended"), "the tail was not logged at warn: {}", log.text());
        assert!(warned(&format!("line-{first_kept}.")) && warned("line-39."), "the last lines are kept");
        assert!(!warned(&format!("line-{}.", first_kept - 1)), "an earlier line was kept");
        run(sig, "exit 1", "the quiet child ended").await;
        assert!(!warned("the quiet child ended"), "a child with no stderr logged a tail");
    }

    /// A child still starting is named with its age and its last `STARTING_LINES` lines that hold text, oldest first
    /// and trimmed, while it runs; a child that wrote nothing says so.
    #[tokio::test]
    async fn a_starting_child_is_named_with_its_age_and_last_lines() {
        let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
        let starting = |script: &str| {
            let mut cmd = tokio::process::Command::new("/bin/sh");
            cmd.args(["-c", script]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
            let mut child = sig.spawn(&mut cmd).unwrap();
            (StderrTail::drain(child.stderr.take().unwrap(), |_| {}).start(), child)
        };
        let (quiet, _quiet) = starting("exec sleep 30");
        let nothing = "the quiet child is still starting after 0 s; it has written nothing to stderr";
        assert_eq!(quiet.still_starting("the quiet child"), nothing);
        let script = "for l in one two '' '   three   ' four five six; do echo \"$l\" >&2; done; exec sleep 30";
        let (start, _child) = starting(script);
        assert_eq!(STARTING_LINES, 5);
        let lines = " s; its last stderr lines: two / three / four / five / six";
        crate::sidecars::contract_tests::within(std::time::Duration::from_secs(5), lines, || {
            start.still_starting("the child").ends_with(lines)
        })
        .await;
        let said = start.still_starting("the child");
        let age = said.strip_prefix("the child is still starting after ").and_then(|rest| rest.strip_suffix(lines));
        assert!(age.and_then(|n| n.parse::<u64>().ok()).is_some_and(|n| n < 5), "{said}");
        sig.fire().expect("fire");
    }
}
