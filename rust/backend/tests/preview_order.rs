#![cfg(any(windows, target_os = "linux"))]
//! `preview.set_scale` keeps its place in a connection's request order: a `preview.get` written right
//! behind it, before its reply is read, carries the new scale. The daemon answers `preview.get` off the
//! connection's loop and `preview.set_scale` on it (`server/dispatch.rs`), and the sidecar the set writes is
//! what the get reads, so a set moved off the loop would race the get that follows it.

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;

use serde_json::json;
use sot_protocol::{codec, op, Frame, Kind};
use support::{call, connect_and_hello, Env, BOUND};

/// Pipelined set-then-get pairs. One pair can pass by luck if the set were off the loop; this many do not.
const ROUNDS: u64 = 40;

fn nm_per_px_of(reply: &Frame) -> Option<f64> {
    reply.payload["extras"]["physical_scale"]["axes"][0]["nm_per_px"].as_f64()
}

#[tokio::test]
async fn a_get_written_behind_a_set_scale_carries_the_new_scale() {
    let env = Env::new("prevord");
    image::RgbaImage::new(4, 4).save(env.daemon_project_root.join("cal.png")).expect("write cal.png");
    env.spawn_sotd();
    let (mut conn, mut id) = connect_and_hello(&env.socket_path).await;
    let node = "files:cal.png";

    let first = call(&mut conn, id, op::PREVIEW_GET, json!({"node_id": node})).await;
    id += 1;
    assert!(first.payload.get("error").is_none(), "preview.get on the PNG: {:?}", first.payload);
    assert_eq!(nm_per_px_of(&first), None, "no sidecar yet: {:?}", first.payload);

    for round in 0..ROUNDS {
        let nm_per_px = 2.0 + round as f64;
        let scale = json!({"unit": "nm", "axes": [{"name": "x", "nm_per_px": nm_per_px}]});
        let (set_id, get_id) = (id, id + 1);
        id += 2;
        // Both requests are on the wire before either reply is read.
        codec::write_frame(&mut conn, &Frame::req(set_id, op::PREVIEW_SET_SCALE, json!({"node_id": node, "physical_scale": scale})), None)
            .await
            .expect("write preview.set_scale");
        codec::write_frame(&mut conn, &Frame::req(get_id, op::PREVIEW_GET, json!({"node_id": node})), None)
            .await
            .expect("write preview.get");
        let mut get_reply = None;
        let mut set_replied = false;
        while get_reply.is_none() || !set_replied {
            let (frame, _blob) = tokio::time::timeout(BOUND, codec::read_frame(&mut conn))
                .await
                .unwrap_or_else(|_| panic!("round {round}: no reply within {BOUND:?}"))
                .expect("read_frame");
            if frame.kind == Kind::Evt {
                continue;
            }
            assert!(frame.payload.get("error").is_none(), "round {round}: {frame:?}");
            if frame.id == set_id {
                set_replied = true;
            } else if frame.id == get_id {
                get_reply = Some(frame);
            }
        }
        let got = nm_per_px_of(&get_reply.expect("the get's reply"));
        assert_eq!(got, Some(nm_per_px), "round {round}: the get written behind the set must see its scale");
    }
    env.kill_daemon_bounded().await;
}
