use super::*;
use serde_json::json;

fn req(to: &str) -> CommFileReq {
    CommFileReq { from: "s".into(), to: to.into(), text: "hi".into(), broadcast: false, forwarded: false }
}

/// A link that answers each offer with `answer(to)`, or never when it gives `None`.
fn serve(links: &Links, host: &str, answer: fn(&str) -> Option<serde_json::Value>) -> tokio::task::JoinHandle<()> {
    let mut link = Some(links.attach(host.to_string()));
    tokio::spawn(async move {
        let mut silent = Vec::new();
        loop {
            let offer = next_offer(&mut link).await;
            assert!(offer.req.forwarded, "an offer is marked forwarded");
            let end = link.as_mut().unwrap();
            let frame = end.request(offer).unwrap();
            assert_eq!(frame.op, op::COMM_FILE);
            let to = frame.payload["to"].as_str().unwrap().to_string();
            match answer(&to) {
                Some(payload) => end.answer(Frame::res(frame.id, op::COMM_FILE, payload)),
                None => silent.push(frame),
            }
        }
    })
}

fn not_here(to: &str) -> Option<serde_json::Value> {
    Some(json!({"error": format!("no box knows that handle: {to}"), "code": "not_here"}))
}

fn files_live(to: &str) -> Option<serde_json::Value> {
    match to {
        "live" => Some(json!({"ok": true})),
        "stale" => Some(json!({"error": "no live session holds @stale", "code": "no_live_session"})),
        _ => not_here(to),
    }
}

const WITHIN: Duration = Duration::from_secs(4);

#[tokio::test]
async fn no_link_attached_is_no_answer() {
    assert_eq!(Links::default().offer(&req("live"), WITHIN).await, None);
}

#[tokio::test]
async fn the_box_that_lists_the_handle_decides() {
    let links = Links::default();
    let _a = serve(&links, "a", not_here);
    let _b = serve(&links, "b", files_live);
    assert_eq!(links.offer(&req("live"), WITHIN).await, Some(Ok(())));
    assert_eq!(
        links.offer(&req("stale"), WITHIN).await,
        Some(Err(("no_live_session".into(), "no live session holds @stale".into())))
    );
    assert_eq!(
        links.offer(&req("nobody"), WITHIN).await,
        Some(Err(("not_here".into(), "no box knows that handle: nobody".into())))
    );
}

#[tokio::test(start_paused = true)]
async fn a_silent_link_is_named_and_an_ok_elsewhere_still_wins() {
    let links = Links::default();
    let _a = serve(&links, "a", |_| None);
    let _b = serve(&links, "b", files_live);
    assert_eq!(links.offer(&req("live"), WITHIN).await, Some(Ok(())));
    assert_eq!(
        links.offer(&req("nobody"), WITHIN).await,
        Some(Err(("file_failed".into(), "the daemon on a did not answer within 4s; @nobody may still be filed".into())))
    );
}

#[tokio::test]
async fn a_dropped_link_leaves_and_its_owed_answers_end() {
    let links = Links::default();
    let mut link = Some(links.attach("a".into()));
    let asking = tokio::spawn({
        let links = links.clone();
        async move { links.offer(&req("live"), WITHIN).await }
    });
    let offer = next_offer(&mut link).await;
    let _frame = link.as_mut().unwrap().request(offer).unwrap();
    assert_eq!(links.hosts(), ["a"], "an attached link is named");
    drop(link);
    assert_eq!(
        asking.await.unwrap(),
        Some(Err(("file_failed".into(), "the daemon on a did not answer within 4s; @live may still be filed".into())))
    );
    assert_eq!(links.offer(&req("live"), WITHIN).await, None, "the dropped link is no longer attached");
}
