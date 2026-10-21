// One pass of `select_once` per bus event, through the real arms: an item
// is one evt frame of the bus's op, a lagged or closed bus writes nothing,
// and with the two optional buses absent every arm stays pending.
use super::*;
use crate::files::watcher::ChangeKind;
use crate::topology::store::TopologyChanged;
use serde_json::json;

type ReadDone = (tokio::io::BufReader<tokio::io::Empty>, Result<(Frame, Option<Vec<u8>>)>);

/// The receivers `select_once` borrows, the writer it fills, and the
/// idle read future, jobs and reply channel it is given.
struct Wire {
    read_fut: Option<std::pin::Pin<Box<std::future::Pending<ReadDone>>>>,
    _out_tx: OutTx,
    out_rx: mpsc::Receiver<(Frame, Option<Vec<u8>>)>,
    watcher_rx: Option<broadcast::Receiver<PreviewChanged>>,
    ws_events_rx: broadcast::Receiver<WorkspaceChanged>,
    topo_changed_rx: broadcast::Receiver<TopologyChanged>,
    agent_events_rx: broadcast::Receiver<AgentMessage>,
    link: Option<crate::comm::mail::links::LinkEnd>,
    fe_command_rx: broadcast::Receiver<FeCommandEvt>,
    repl_frame_rx: broadcast::Receiver<ReplFrameMsg>,
    monitor_rx: Option<broadcast::Receiver<HostLatest>>,
    jobs: JoinSet<()>,
    workspaces: Workspaces,
    buf: Vec<u8>,
}

/// Every sender, kept by the test so a bus stays open until it drops one.
struct Senders {
    watcher: broadcast::Sender<PreviewChanged>,
    ws_events: broadcast::Sender<WorkspaceChanged>,
    topo_changed: broadcast::Sender<TopologyChanged>,
    agent_events: broadcast::Sender<AgentMessage>,
    fe_command: broadcast::Sender<FeCommandEvt>,
    repl_frame: broadcast::Sender<ReplFrameMsg>,
    monitor: broadcast::Sender<HostLatest>,
}

fn wire() -> (Wire, Senders) {
    let (watcher, watcher_rx) = broadcast::channel(1);
    let (ws_events, ws_events_rx) = broadcast::channel(1);
    let (topo_changed, topo_changed_rx) = broadcast::channel(1);
    let (agent_events, agent_events_rx) = broadcast::channel(1);
    let (fe_command, fe_command_rx) = broadcast::channel(1);
    let (repl_frame, repl_frame_rx) = broadcast::channel(1);
    let (monitor, monitor_rx) = broadcast::channel(1);
    let (out_tx, out_rx) = mpsc::channel(OFFLOOP_CONCURRENCY);
    let w = Wire {
        read_fut: Some(Box::pin(std::future::pending())),
        _out_tx: out_tx,
        out_rx,
        watcher_rx: Some(watcher_rx),
        ws_events_rx,
        topo_changed_rx,
        agent_events_rx,
        link: None,
        fe_command_rx,
        repl_frame_rx,
        monitor_rx: Some(monitor_rx),
        jobs: JoinSet::new(),
        workspaces: Workspaces::new(),
        buf: Vec::new(),
    };
    let s = Senders {
        watcher, ws_events, topo_changed, agent_events, fe_command, repl_frame, monitor,
    };
    (w, s)
}

impl Wire {
    async fn once(&mut self) -> Woke<tokio::io::Empty> {
        select_once(
            &mut self.buf, &mut self.read_fut, &mut self.out_rx, &mut self.watcher_rx,
            &mut self.ws_events_rx, &mut self.topo_changed_rx, &mut self.agent_events_rx,
            &mut self.link, &mut self.fe_command_rx, &mut self.repl_frame_rx,
            &mut self.monitor_rx, &mut self.jobs, tokio::time::Instant::now(), false, true, &None,
            &self.workspaces, &None, &Clients::new(), &Default::default(),
        )
        .await
        .expect("select_once")
    }

    /// `once`, bounded so a regression in a bus's arm fails its test
    /// instead of hanging the suite.
    async fn once_on(&mut self, bus: &str) -> Woke<tokio::io::Empty> {
        tokio::time::timeout(std::time::Duration::from_secs(5), self.once())
            .await
            .unwrap_or_else(|_| panic!("select_once on the {bus} bus did not return in 5 s"))
    }

    /// The one evt frame the writer holds: its op and payload.
    async fn the_evt(&self) -> (String, serde_json::Value) {
        let mut rest = &self.buf[..];
        let (f, blob) = codec::read_frame(&mut rest).await.expect("one frame");
        assert!(rest.is_empty(), "exactly one frame");
        assert!(blob.is_none());
        assert!(matches!(f.kind, Kind::Evt));
        (f.op, f.payload)
    }
}

/// Per bus: one item is one frame, two (capacity 1) lag and write
/// nothing, a dropped sender closes and writes nothing.
macro_rules! bus_wire_tests {
    ($name:ident, $tx:ident, $op:expr, $item:expr, $payload:expr) => {
        mod $name {
            use super::*;

            #[tokio::test]
            async fn one_item_is_one_evt_frame() {
                let (mut w, s) = wire();
                s.$tx.send($item).unwrap();
                assert!(matches!(w.once_on(stringify!($name)).await, Woke::Again));
                assert_eq!(w.the_evt().await, ($op.to_string(), $payload));
            }

            #[tokio::test]
            async fn a_lagged_bus_writes_nothing() {
                let (mut w, s) = wire();
                s.$tx.send($item).unwrap();
                s.$tx.send($item).unwrap();
                assert!(matches!(w.once_on(stringify!($name)).await, Woke::Again));
                assert!(w.buf.is_empty());
            }

            #[tokio::test]
            async fn a_closed_bus_writes_nothing() {
                let (mut w, s) = wire();
                drop(s.$tx);
                assert!(matches!(w.once_on(stringify!($name)).await, Woke::Again));
                assert!(w.buf.is_empty());
            }
        }
    };
}

bus_wire_tests!(
    watcher, watcher, op::PREVIEW_CHANGED,
    PreviewChanged {
        path: "/p/a.jl".into(),
        node_id: Some("files:a".into()),
        kind: ChangeKind::Modified,
        workspace_id: Some("alpha".into()),
    },
    json!({"path": "/p/a.jl", "node_id": "files:a", "kind": "modified", "workspace_id": "alpha"})
);
bus_wire_tests!(
    ws_events, ws_events, op::WORKSPACE_CHANGED,
    WorkspaceChanged { action: "created".into(), slug: "alpha".into(), workspace_id: "ws-1".into() },
    json!({"action": "created", "slug": "alpha", "workspace_id": "ws-1"})
);
bus_wire_tests!(
    topo_changed, topo_changed, op::TOPOLOGY_CHANGED,
    TopologyChanged { hash: "h1".into() },
    json!({"hash": "h1"})
);
bus_wire_tests!(
    agent_events, agent_events, op::AGENT_MESSAGE,
    AgentMessage { from: "a".into(), to: "".into(), text: "hi".into(), ts: "2026-01-01T00:00:00Z".into() },
    json!({"from": "a", "to": "", "text": "hi", "ts": "2026-01-01T00:00:00Z"})
);
bus_wire_tests!(
    fe_command, fe_command, op::FE_COMMAND,
    FeCommandEvt { v: 1, cmd: "show".into(), args: json!({"k": 1}), target: None, target_serial: None },
    json!({"v": 1, "cmd": "show", "args": {"k": 1}})
);
bus_wire_tests!(
    repl_frame, repl_frame, op::REPL_FRAME,
    ReplFrameMsg {
        eval_id: 7, workspace_id: Some("alpha".into()), frame: json!({"kind": "stdout"}), opener: Default::default(),
    },
    json!({"eval_id": 7, "workspace_id": "alpha", "frame": {"kind": "stdout"}})
);
bus_wire_tests!(
    monitor, monitor, op::MONITOR_TICK,
    HostLatest { host: "h".into(), stale: true, sample: None },
    json!({"hosts": [{"host": "h", "stale": true}]})
);

/// A link connection writes each offer as one `comm.file` request, marked forwarded, and the response it reads
/// back settles that offer.
#[tokio::test]
async fn a_link_writes_an_offer_as_a_comm_file_request_and_its_response_settles_it() {
    let (mut w, _s) = wire();
    let links = crate::comm::mail::links::Links::default();
    w.link = Some(links.attach("win-b".into()));
    let req = sot_protocol::CommFileReq { from: "s".into(), to: "h".into(), text: "hi".into(), broadcast: false, forwarded: false };
    let asking = tokio::spawn(async move { links.offer(&req, std::time::Duration::from_secs(4)).await });
    assert!(matches!(w.once_on("link").await, Woke::Again));
    let mut rest = &w.buf[..];
    let (f, _) = codec::read_frame(&mut rest).await.expect("one frame");
    assert!(matches!(f.kind, Kind::Req));
    assert_eq!(f.op, op::COMM_FILE);
    assert_eq!((f.payload["to"].as_str(), f.payload["forwarded"].as_bool()), (Some("h"), Some(true)));
    w.link.as_mut().expect("the link").answer(Frame::res(f.id, op::COMM_FILE, json!({"ok": true})));
    assert_eq!(asking.await.expect("offer task"), Some(Ok(())));
}

#[tokio::test(start_paused = true)]
async fn with_both_optional_buses_absent_and_every_bus_idle_nothing_wakes() {
    let (mut w, _s) = wire();
    w.watcher_rx = None;
    w.monitor_rx = None;
    let woke = tokio::time::timeout(std::time::Duration::from_secs(1), w.once()).await;
    assert!(woke.is_err(), "select_once returned with nothing to do");
    assert!(w.buf.is_empty());
}
