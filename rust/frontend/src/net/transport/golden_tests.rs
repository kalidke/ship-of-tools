//! The wire line of every request kind, and the events an error reply to each yields, pinned to a golden
//! file.

use super::*;

struct GoldenWindow;
impl Redraw for GoldenWindow {
    fn request_redraw(&self) {}
}

/// One of every `OutgoingReq` kind, in the order of `run_session`'s old
/// request match. Every field value is distinct, so two arguments of the
/// same type passed in the wrong order change the golden.
fn every_request_kind(download_dest: &std::path::Path) -> Vec<OutgoingReq> {
    let s = |v: &str| v.to_string();
    let o = |v: &str| Some(v.to_string());
    vec![
        OutgoingReq::TreeChildren { parent_id: s("tc.parent"), workspace_id: o("tc.ws") },
        OutgoingReq::TreeRoot { mode: s("tr.mode"), workspace_id: o("tr.ws") },
        OutgoingReq::ToggleHidden { workspace_id: o("th.ws") },
        OutgoingReq::WorkspaceActivate { workspace_id: o("wa.ws"), read: true },
        OutgoingReq::FePresence,
        OutgoingReq::FeSessions(vec![sot_protocol::DeclaredSession {
            handle: s("fs.handle"),
            state: s("fs.state"),
            summary: s("fs.summary"),
            status_at: s("fs.at"),
        }]),
        OutgoingReq::ProjectScan { workspace_id: o("ps.ws"), generation: 101 },
        OutgoingReq::MarkdownTokenize { lang: s("mt.lang"), source_hash: 102, source: s("mt.source") },
        OutgoingReq::ConceptRead { target: s("cr.target"), workspace_id: o("cr.ws"), generation: 103 },
        OutgoingReq::MathRender { latex: s("mr.latex"), display: true },
        OutgoingReq::ImageCrop { node_id: s("ic.node"), x: 104, y: 105, w: 106, h: 107, workspace_id: o("ic.ws") },
        OutgoingReq::ConceptWrite {
            target: s("cw.target"),
            content: s("cw.content"),
            expected_ast_hash: o("cw.hash"),
            workspace_id: o("cw.ws"),
        },
        OutgoingReq::FileRead { node_id: s("fr.node"), workspace_id: o("fr.ws") },
        OutgoingReq::FileWrite {
            node_id: s("fw.node"),
            content: s("fw.content"),
            expected_version: o("fw.version"),
            workspace_id: o("fw.ws"),
        },
        OutgoingReq::FileDelete { node_id: s("fd.node"), workspace_id: o("fd.ws") },
        OutgoingReq::DirCreate { node_id: s("dc.node"), workspace_id: o("dc.ws") },
        OutgoingReq::FileParse { path: s("fp.path"), workspace_id: o("fp.ws") },
        OutgoingReq::PreviewGet {
            node_id: s("pg.node"),
            workspace_id: o("pg.ws"),
            page: Some(108),
            fit_w: Some(109),
            fit_h: Some(110),
            generation: 111,
        },
        OutgoingReq::PreviewSetScale { node_id: s("pss.node"), nm_per_px: 1.5, workspace_id: o("pss.ws"), generation: 112 },
        OutgoingReq::FigureGet { url: s("fg.url"), node_id: s("fg.node"), workspace_id: o("fg.ws") },
        OutgoingReq::FunctionMethods { module: s("fm.module"), name: s("fm.name"), workspace_id: o("fm.ws") },
        OutgoingReq::ReplEval { eval_id: 113, code: s("re.code"), mode: o("re.mode"), workspace_id: o("re.ws") },
        OutgoingReq::ReplInterrupt { workspace_id: o("ri.ws") },
        OutgoingReq::PtyOpen { cols: 114, rows: 115, target: o("po.target"), user_switch: true },
        OutgoingReq::DirectoryList { path: s("dl.path"), include_hidden: true },
        OutgoingReq::WorkspaceCreate {
            label: s("wc.label"),
            project_root: s("wc.root"),
            autostart_claude: true,
            agent: s("wc.agent"),
            account: o("wc.account"),
        },
        OutgoingReq::WorkspaceList,
        OutgoingReq::AccountsList,
        OutgoingReq::WorkspaceDestroy { workspace_id: s("wd.ws") },
        OutgoingReq::PlutoOpen { path: s("pl.path") },
        OutgoingReq::VideoOpen { path: s("vo.path") },
        OutgoingReq::DocsOpen { path: s("do.path") },
        OutgoingReq::QuartoOpen { path: s("qo.path"), execute: true },
        OutgoingReq::ReplRunFile { eval_id: 116, path: s("rf.path"), fresh: true, workspace_id: o("rf.ws") },
        OutgoingReq::FileDownload { path: s("fdl.path"), dest: download_dest.to_path_buf() },
        OutgoingReq::FileUpload {
            dir: s("fu.dir"),
            name: s("fu.name"),
            offset: 117,
            total: 118,
            eof: true,
            bytes: vec![1, 2, 3],
        },
        OutgoingReq::MonitorSubscribe,
        OutgoingReq::MonitorUnsubscribe,
        OutgoingReq::AgentSend { from: s("as.from"), to: s("as.to"), text: s("as.text") },
        OutgoingReq::MonitorHistory { window_s: 2.5, points: 119, until: Some(3.5), host: o("mh.host") },
    ]
}

/// Every request kind's wire line, and the events an `{error, code}`
/// reply to each one yields, match `testdata/every_kind.golden`. To
/// regenerate it after a deliberate wire change, run this test once
/// with `SOT_BLESS_GOLDEN=1` and review the diff.
#[tokio::test]
async fn every_request_kind_and_its_error_reply_match_the_golden() {
    let _env = crate::state::test_env::set_test_env();
    let reqs = every_request_kind(&std::env::temp_dir().join("sot-golden-never-written"));
    let n = reqs.len();
    let (near, far) = tokio::io::duplex(1 << 20);
    // The fake daemon answers the hello, then answers the two preamble
    // requests and every request with an error envelope, recording each
    // request line as written; then it pushes `test.end`.
    let daemon = tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let (rx, mut tx) = tokio::io::split(far);
        let mut rx = codec::buffered(rx);
        let (hello, _) = codec::read_frame(&mut rx).await.unwrap();
        let hello_ok = serde_json::json!({ "session_id": "sess-1", "revision": 0, "snapshot_pending": false });
        codec::write_frame(&mut tx, &Frame::res(hello.id, op::HELLO, hello_ok).with_rev(0), None).await.unwrap();
        let mut wire = Vec::new();
        for _ in 0..(2 + n) {
            let mut line = Vec::new();
            rx.read_until(b'\n', &mut line).await.unwrap();
            let req: Frame = serde_json::from_slice(&line).unwrap();
            wire.push(String::from_utf8(line).unwrap().trim_end().to_string());
            let err = serde_json::json!({ "error": format!("err-{}", req.op), "code": format!("code-{}", req.op) });
            codec::write_frame(&mut tx, &Frame::res(req.id, &req.op, err), None).await.unwrap();
        }
        codec::write_frame(&mut tx, &Frame::evt("test.end", serde_json::json!({})), None).await.unwrap();
        wire
    });
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel();
    for r in reqs {
        out_tx.send(r).unwrap();
    }
    let session = tokio::spawn(async move {
        let (rx, tx) = tokio::io::split(near);
        let mut backoff_ms = 200;
        run_protocol(
            "golden".to_string(),
            codec::buffered(rx),
            tx,
            None,
            &evt_tx,
            &mut out_rx,
            &GoldenWindow,
            &mut backoff_ms,
            ResolvedDial::Local,
            None,
        )
        .await
    });
    let mut events = Vec::new();
    let t0 = std::time::Instant::now();
    loop {
        match evt_rx.try_recv() {
            Ok((_, IncomingEvt::Event { op, .. })) if op == "test.end" => break,
            Ok((host, ev)) => events.push(format!("< {host} {ev:?}")),
            Err(_) => {
                assert!(t0.elapsed() < std::time::Duration::from_secs(10), "no test.end; got {events:#?}");
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
    }
    let wire = daemon.await.unwrap();
    session.abort();
    drop(out_tx);
    let mut actual: Vec<String> = wire.into_iter().map(|l| format!("> {l}")).collect();
    actual.extend(events);
    let actual = actual.join("\n") + "\n";
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/net/transport/testdata/every_kind.golden");
    if std::env::var_os("SOT_BLESS_GOLDEN").is_some() {
        std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
        std::fs::write(&golden, &actual).unwrap();
    }
    let want = std::fs::read_to_string(&golden).expect("golden missing: run once with SOT_BLESS_GOLDEN=1");
    assert!(actual == want, "wire or events differ from {}:\n{actual}", golden.display());
}
