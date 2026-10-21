use super::*;

const RECORD: &str = " WARN capture_contract: mathjax response parse failed error=Syntax error at line 1 column 29 | len=63\n";
const COLLISION_TIME: &str = "2026-10-06T17:46:38.401234Z";

struct CollisionTime;

impl tracing_subscriber::fmt::time::FormatTime for CollisionTime {
    fn format_time(
        &self,
        writer: &mut tracing_subscriber::fmt::format::Writer<'_>,
    ) -> std::fmt::Result {
        std::fmt::Write::write_str(writer, COLLISION_TIME)
    }
}

fn safe_warn() {
    tracing::warn!(
        target: "capture_contract",
        error = %"Syntax error at line 1 column 29 | len=63",
        "mathjax response parse failed"
    );
}

#[test]
fn capture_has_no_formatter_time_and_install_keeps_its_timer() {
    {
        let out = Arc::new(Mutex::new(Vec::new()));
        let sink = out.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || Sink(sink.clone()))
            .with_timer(CollisionTime)
            .with_ansi(false)
            .with_max_level(LevelFilter::TRACE)
            .finish();
        let _guard = install(subscriber);
        safe_warn();
        let text = String::from_utf8(out.lock().unwrap().clone()).unwrap();
        assert_eq!(text, format!("{COLLISION_TIME} {RECORD}"));
        assert_eq!(text.lines().filter(|line| line.contains("WARN")).count(), 1);
        assert!(text.contains("0123"));
        assert!(!text.contains("41234"));
        eprintln!(
            "capture contract: fixed-time install control passed; timestamp contains 0123"
        );
    }

    let log = capture();
    safe_warn();
    let text = log.text();
    assert_eq!(text.lines().filter(|line| line.contains("WARN")).count(), 1);
    eprintln!("capture contract: public capture WARN count 1");
    assert_eq!(text, RECORD);
    assert!(!text.contains("0123") && !text.contains("41234"));
    eprintln!("capture contract: exact timestamp-free record passed");
}

fn probe() {
    tracing::info!("test_log probe");
}

/// The callsite is reached first by a thread with no subscriber; the capture still sees the later event.
#[test]
fn a_callsite_another_thread_reaches_first_still_reaches_the_capture() {
    let log = capture();
    std::thread::spawn(probe).join().unwrap();
    probe();
    assert_eq!(log.text().matches("test_log probe").count(), 1, "{}", log.text());
}

fn marked_event(marker: &str) {
    tracing::info!(target: "capture_parallel", %marker, "parallel capture");
}

#[test]
fn parallel_captures_receive_only_their_own_events() {
    use std::sync::mpsc::channel;
    use std::time::Duration;

    const BOUND: Duration = Duration::from_secs(30);
    let (ready_tx, ready_rx) = channel();
    let (complete_tx, complete_rx) = channel();
    let (outer_release_tx, outer_release_rx) = channel();
    let (peer_release_tx, peer_release_rx) = channel();
    let peer_ready_tx = ready_tx.clone();
    let peer_complete_tx = complete_tx.clone();
    let outer = std::thread::spawn(move || {
        let log = capture();
        ready_tx.send(()).unwrap();
        outer_release_rx.recv_timeout(BOUND).unwrap();
        marked_event("outer_before");
        let inner = capture();
        marked_event("nested");
        let nested = inner.text();
        drop(inner);
        marked_event("outer_after");
        complete_tx.send(()).unwrap();
        outer_release_rx.recv_timeout(BOUND).unwrap();
        (log.text(), nested)
    });
    let peer = std::thread::spawn(move || {
        let log = capture();
        peer_ready_tx.send(()).unwrap();
        peer_release_rx.recv_timeout(BOUND).unwrap();
        marked_event("peer");
        peer_complete_tx.send(()).unwrap();
        peer_release_rx.recv_timeout(BOUND).unwrap();
        log.text()
    });
    for _ in 0..2 {
        ready_rx.recv_timeout(BOUND).unwrap();
    }
    outer_release_tx.send(()).unwrap();
    peer_release_tx.send(()).unwrap();
    for _ in 0..2 {
        complete_rx.recv_timeout(BOUND).unwrap();
    }
    outer_release_tx.send(()).unwrap();
    peer_release_tx.send(()).unwrap();
    let (outer, inner) = outer.join().unwrap();
    let peer = peer.join().unwrap();
    assert_eq!(outer.lines().count(), 2);
    assert_eq!(inner.lines().count(), 1);
    assert_eq!(peer.lines().count(), 1);
    assert_eq!(
        outer,
        " INFO capture_parallel: parallel capture marker=outer_before\n INFO capture_parallel: parallel capture marker=outer_after\n"
    );
    assert_eq!(
        inner,
        " INFO capture_parallel: parallel capture marker=nested\n"
    );
    assert_eq!(
        peer,
        " INFO capture_parallel: parallel capture marker=peer\n"
    );
}
