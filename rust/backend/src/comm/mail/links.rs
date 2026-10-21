//! The hub's linked daemons. A `link`-role connection is a daemon on a box with its own comm folder, which the hub
//! cannot append to; a `comm.file` the hub's own folder does not list is offered to each of them (`Links::offer`).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt};
use sot_protocol::{op, CommFileReq, Frame};
use tokio::sync::{mpsc, oneshot};

/// One `comm.file` the hub offers a linked daemon, and where that daemon's answer goes.
pub struct Offer {
    req: CommFileReq,
    answer: oneshot::Sender<serde_json::Value>,
}

/// Every attached link: its declared host and the channel its connection reads offers from.
#[derive(Clone, Default)]
pub struct Links(Arc<Mutex<Attached>>);

#[derive(Default)]
struct Attached {
    next_key: u64,
    by_key: BTreeMap<u64, (String, mpsc::Sender<Offer>)>,
}

/// A link connection's side of `Links`: the offers its connection writes, the answers still owed to them, and its
/// entry, which it removes when it drops.
pub struct LinkEnd {
    links: Links,
    key: u64,
    offers: mpsc::Receiver<Offer>,
    owed: HashMap<u64, oneshot::Sender<serde_json::Value>>,
    next_id: u64,
}

impl Links {
    /// Enters a `link`-role connection that declared `host`.
    pub fn attach(&self, host: String) -> LinkEnd {
        let (tx, offers) = mpsc::channel(16);
        let mut attached = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let key = attached.next_key;
        attached.next_key += 1;
        attached.by_key.insert(key, (host, tx));
        LinkEnd { links: self.clone(), key, offers, owed: HashMap::new(), next_id: 1 }
    }

    /// The hosts of the attached links.
    pub fn hosts(&self) -> Vec<String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).by_key.values().map(|(host, _)| host.clone()).collect()
    }

    /// Offers `req` to every attached link, marked forwarded so each files only into its own folder, and gives each
    /// `within` to answer. `None` when no link is attached. Otherwise the first `ok` is the verdict; with none, a
    /// refusal other than `not_here` (the box that lists the handle refused it), by host; else a link that did not
    /// answer, by host; else `not_here`.
    pub async fn offer(&self, req: &CommFileReq, within: Duration) -> Option<Result<(), (String, String)>> {
        let linked: Vec<(String, mpsc::Sender<Offer>)> =
            self.0.lock().unwrap_or_else(PoisonError::into_inner).by_key.values().cloned().collect();
        if linked.is_empty() {
            return None;
        }
        let mut asks: FuturesUnordered<_> = linked
            .into_iter()
            .map(|(host, tx)| {
                let req = CommFileReq { forwarded: true, ..req.clone() };
                async move {
                    let (answer, rx) = oneshot::channel();
                    let asked = async {
                        tx.send(Offer { req, answer }).await.ok()?;
                        rx.await.ok()
                    };
                    (host, tokio::time::timeout(within, asked).await.ok().flatten())
                }
            })
            .collect();
        let mut answers = Vec::new();
        while let Some((host, answer)) = asks.next().await {
            if answer.as_ref().and_then(|a| a.get("ok")).and_then(serde_json::Value::as_bool) == Some(true) {
                return Some(Ok(()));
            }
            answers.push((host, answer));
        }
        Some(Err(refusal(&req.to, answers, within)))
    }
}

/// The verdict when no link filed: `(code, sentence)`, the sentence what the sender prints after `FAILED -> @<to>: `.
fn refusal(to: &str, mut answers: Vec<(String, Option<serde_json::Value>)>, within: Duration) -> (String, String) {
    answers.sort_by(|a, b| a.0.cmp(&b.0));
    let field = |a: &serde_json::Value, k: &str| a.get(k).and_then(serde_json::Value::as_str).map(str::to_string);
    for answer in answers.iter().filter_map(|(_, a)| a.as_ref()) {
        match (field(answer, "code"), field(answer, "error")) {
            (Some(code), _) if code == "not_here" => {}
            (code, Some(error)) => return (code.unwrap_or_else(|| "file_failed".into()), error),
            _ => {}
        }
    }
    if let Some((host, _)) = answers.iter().find(|(_, a)| a.is_none()) {
        let secs = within.as_secs();
        return ("file_failed".into(), format!("the daemon on {host} did not answer within {secs}s; @{to} may still be filed"));
    }
    crate::comm::mail::filer::not_here(to)
}

impl LinkEnd {
    /// The `comm.file` request that carries `offer`; its answer is owed under the request's id from now on.
    pub fn request(&mut self, offer: Offer) -> anyhow::Result<Frame> {
        let id = self.next_id;
        self.next_id += 1;
        let frame = Frame::req(id, op::COMM_FILE, serde_json::to_value(&offer.req)?);
        self.owed.insert(id, offer.answer);
        Ok(frame)
    }

    /// A response the link sent: the answer to the offer whose request had its id. Any other is dropped.
    pub fn answer(&mut self, frame: Frame) {
        if let Some(owed) = self.owed.remove(&frame.id) {
            let _ = owed.send(frame.payload);
        }
    }
}

impl Drop for LinkEnd {
    fn drop(&mut self) {
        self.links.0.lock().unwrap_or_else(PoisonError::into_inner).by_key.remove(&self.key);
    }
}

/// The next offer for a connection that may be a link: pending forever for any other connection. A link's channel
/// never closes while its end lives: `Links` holds the sender until the end drops.
pub async fn next_offer(link: &mut Option<LinkEnd>) -> Offer {
    let Some(end) = link else {
        return std::future::pending().await;
    };
    end.offers.recv().await.expect("Links holds this end's sender until the end drops")
}

#[cfg(test)]
#[path = "links_tests.rs"]
mod tests;
