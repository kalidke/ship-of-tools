//! The notice to a sender: one line into its inbox when a row keeps refusing the wake for mail the sender filed.

use super::unread::unread_senders;
use super::*;
use crate::comm::mail::filer::file_comm;
use sot_protocol::CommFileReq;

/// Who the notice is from. It is no handle a session holds, so a sender never mistakes it for a peer.
const NOTICE_FROM: &str = "sotd";

/// One `comm.file` request per distinct sender of `handle`'s unread mail, each a broadcast copy (`to:""` in the
/// inbox): it files silently for `comm-poll.sh` and `scan` never counts it, so no wake is ever tried for a notice and
/// a refused wake can never file a notice about one.
pub(super) fn notice_requests(comm_home: &Path, handle: &str, reason: &str) -> Vec<CommFileReq> {
    let text = format!("[sot-comm] your message to @{handle} is filed but @{handle} has not been woken for {} s: {reason}", REFUSED_FOR.as_secs());
    unread_senders(comm_home, handle)
        .into_iter()
        .map(|sender| CommFileReq { from: NOTICE_FROM.to_string(), to: sender, text: text.clone(), broadcast: true, forwarded: false })
        .collect()
}

/// Files each notice through `file_comm`, the filer `comm.file` and the hub link use, so a sender on another box is
/// reached by its forward to the hub and no new route exists. A sender it cannot file for is logged and left.
pub(super) async fn notify_senders(comm_home: &Path, workspaces: &Workspaces, handle: &str, reason: &'static str) {
    let (home, h) = (comm_home.to_path_buf(), handle.to_string());
    let reqs = tokio::task::spawn_blocking(move || notice_requests(&home, &h, reason)).await.unwrap_or_default();
    for req in reqs {
        let sender = req.to.clone();
        match file_comm(req, workspaces).await {
            Ok(Ok(())) => tracing::info!(handle, %sender, reason, "comm wake: told the sender its message has not woken the row"),
            Ok(Err((code, why))) => tracing::info!(handle, %sender, code, "comm wake: sender not told: {why}"),
            Err(e) => tracing::info!(handle, %sender, "comm wake: sender not told: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(inbox: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("inbox")).unwrap();
        std::fs::write(d.path().join("inbox/a.jsonl"), inbox).unwrap();
        d
    }

    fn line(from: &str, to: &str) -> String {
        format!("{{\"from\":\"{from}\",\"to\":\"{to}\",\"msg\":\"x\"}}\n")
    }

    #[test]
    fn one_notice_per_distinct_sender_of_unread_directed_mail() {
        // b twice, c once, a's own line, a line to someone else, a broadcast copy and a sender-less line: b and c only.
        let inbox = [line("b", "a"), line("c", "a"), line("b", "a"), line("a", "a"), line("d", "z"), line("e", ""), "{\"to\":\"a\"}\n".to_string()].concat();
        let d = home(&inbox);
        let reqs = notice_requests(d.path(), "a", "input not empty");
        let to: Vec<&str> = reqs.iter().map(|r| r.to.as_str()).collect();
        assert_eq!(to, ["b", "c"]);
        for r in &reqs {
            // Through the filer as any filing: not forwarded by us, a broadcast copy, from the daemon, naming both and the reason.
            assert!(r.broadcast && !r.forwarded && r.from == NOTICE_FROM);
            assert_eq!(r.text, "[sot-comm] your message to @a is filed but @a has not been woken for 60 s: input not empty");
        }
    }

    #[test]
    fn a_notice_is_never_unread_mail_for_anyone() {
        // The notice as the filer writes it (`to:""`, from the daemon): no handle's scan counts it, so it starts no wake
        // and no refusal streak, and `unread_senders` never names the daemon.
        let notice = line(NOTICE_FROM, "");
        let d = home(&notice);
        assert_eq!(scan(d.path(), "a", 0).unread, 0);
        assert!(unread_senders(d.path(), "a").is_empty());
    }

    #[test]
    fn nothing_unread_notifies_no_one() {
        let d = home(&line("b", "a"));
        std::fs::create_dir_all(d.path().join("read")).unwrap();
        std::fs::write(d.path().join("read/a.cursor"), "1\n").unwrap();
        assert!(notice_requests(d.path(), "a", "r").is_empty());
    }
}
