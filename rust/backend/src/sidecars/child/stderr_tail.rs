//! A sidecar child's stderr: read to its end on a task of its own, each line logged at debug under its owner's target
//! (a healthy Julia is chatty), the last lines kept, so the owner of a child that failed logs the child's own words at
//! warn. The daemon logs at info by default, where the debug lines are dropped.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How many of a child's last stderr lines are kept.
const TAIL_LINES: usize = 30;
/// How long a failed child's owner waits for its stderr to end, once the child is ended, so its last words are read:
/// a descendant that still holds the pipe never lets it end.
const END_WAIT: Duration = Duration::from_secs(1);

pub(crate) struct StderrTail {
    /// The last `TAIL_LINES` lines, newest first.
    lines: Arc<Mutex<VecDeque<String>>>,
    ended: tokio::sync::watch::Receiver<bool>,
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
        StderrTail { lines, ended }
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
    use super::{StderrTail, TAIL_LINES};
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
}
