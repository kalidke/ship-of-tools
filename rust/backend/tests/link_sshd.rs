#![cfg(any(windows, target_os = "linux"))]
//! The window's link through a real sshd: the release gate's proof of topology's guarantee 5 on a real login, run once
//! per release, not by the landing job (every test here is `#[ignore]`). It needs passwordless `ssh localhost` to this
//! computer's own sshd (OpenSSH on Linux; OpenSSH for Windows' sshd and its ssh client on Windows). Run with
//! `cargo test -p sot-backend --test link_sshd -- --ignored --nocapture --test-threads=1` after building
//! `sot-capsule` into the same target folder.
//!
//! The login is the recipe's own argv with its remote command aimed at a private daemon: `sotd stdio-bridge
//! --endpoint <its socket or pipe>`, the router form a private daemon is reached by. A wrapper counts the logins.

mod support;
use support::*;

use sot_capsule::attach_client::client::FeAttachClient;
use sot_protocol::{codec, op, Frame, Kind};
use sot_topology::lane_client::{DaemonLaneEndpoint, LaneDial};
use sot_topology::link::{Link, LinkHandle, LinkRoute};
use std::time::{Duration, Instant};

/// The recipe's argv to `localhost` with its remote command replaced by the private daemon's router, its program
/// `ssh`, and every start counted in `count`'s file.
fn private_login(env: &Env, count: &std::path::Path) -> sot_topology::ssh_bridge::SshRecipe {
    let scheme = if cfg!(windows) { "pipe:" } else { "unix:" };
    let remote = format!("\"{}\" stdio-bridge --endpoint {scheme}{}", sotd_program().display(), env.socket_path.display());
    let plain = sot_topology::ssh_bridge::SshRecipe::new("localhost", None).unwrap();
    let mut argv: Vec<std::ffi::OsString> = plain.command().get_args().map(|a| a.to_owned()).collect();
    argv.pop().expect("the remote command");
    argv.push(remote.into());
    // Each start of the login appends a line before it runs ssh, so the file's lines are the logins.
    #[cfg(unix)]
    let (program, args) = {
        let mut args: Vec<std::ffi::OsString> = vec!["-c".into(), format!("echo login >> '{}'; exec ssh \"$@\"", count.display()).into(), "ssh".into()];
        args.extend(argv);
        (std::path::PathBuf::from("sh"), args)
    };
    #[cfg(windows)]
    let (program, args) = {
        let mut args: Vec<std::ffi::OsString> = vec!["/c".into(), format!("echo login>>\"{}\" && ssh", count.display()).into()];
        args.extend(argv);
        (std::path::PathBuf::from("cmd"), args)
    };
    plain.with_test_command(program, args)
}

fn logins(count: &std::path::Path) -> usize {
    std::fs::read_to_string(count).map(|s| s.lines().count()).unwrap_or(0)
}

async fn create_row(env: &Env, conn: &mut Conn, next_id: &mut u64) -> String {
    let req = serde_json::json!({ "label": "link-sshd", "project_root": env.workspace_project_root.to_string_lossy(), "runtime": "capsule" });
    let res = call(conn, *next_id, op::WORKSPACE_CREATE, req).await;
    *next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    let id = res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = res.payload["session_name"].as_str().expect("session_name").to_string();
    poll_for_phase(conn, next_id, &id, "ready", Duration::from_secs(90)).await;
    target
}

/// One request on a channel that carries the wire's frames, its reply read past any event.
async fn channel_call(
    tx: &mut sot_topology::link::ChannelWriter,
    rx: &mut tokio::io::BufReader<sot_topology::link::ChannelReader>,
    id: u64,
    name: &str,
    payload: serde_json::Value,
) -> Frame {
    codec::write_frame(tx, &Frame::req(id, name, payload), None).await.expect("write a frame");
    loop {
        let (frame, _) = tokio::time::timeout(BOUND, codec::read_frame(rx)).await.expect("a reply within the bound").expect("read a frame");
        if frame.id == id && frame.kind == Kind::Res {
            return frame;
        }
    }
}

/// Topology 5 on a real login: one window link through sshd carries the control session, a row the window attaches
/// and a page the daemon serves, and the three cost one login.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "release gate: needs passwordless ssh to this computer's own sshd"]
async fn a_window_link_through_a_real_sshd_carries_rows_and_a_page() {
    let env = Env::new("link-sshd");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let target = create_row(&env, &mut conn, &mut next_id).await;
    let count = env._tmp.path().join("logins.txt");
    let started = Instant::now();
    let mut link = Link::start(&LinkRoute::Ssh(private_login(&env, &count))).await.expect("the link through sshd");
    println!("link-sshd: the link is up {:?} after its start, router build {}", started.elapsed(), link.build());
    let handle = LinkHandle::default();
    handle.set_up(&link);

    // Control on channel 0.
    let (r0, mut w0) = link.take_control().expect("channel 0").into_split();
    let mut r0 = tokio::io::BufReader::new(r0);
    let hello = sot_protocol::HelloReq::this_process("link-sshd", "cli", Some("link-sshd-host".to_string())).expect("this process's account");
    let reply = channel_call(&mut w0, &mut r0, 1, op::HELLO, serde_json::to_value(hello).unwrap()).await;
    assert!(reply.payload.get("error").is_none(), "the control hello over the link: {:?}", reply.payload);

    // A row, attached over a channel.
    let attach = {
        let endpoint = DaemonLaneEndpoint::new(LaneDial::Link(handle.clone()), None);
        tokio::task::spawn_blocking(move || {
            let mut client = FeAttachClient::attach(endpoint, target, 80, 24, Box::new(|| {})).expect("attach");
            let deadline = Instant::now() + Duration::from_secs(30);
            while !client.is_checkpointed() {
                client.pump();
                assert!(!client.is_dead(), "the attach over the link died: {}", client.status_line());
                assert!(Instant::now() < deadline, "the attach over the link never checkpointed");
                std::thread::sleep(Duration::from_millis(20));
            }
            client.shutdown(Duration::from_secs(5));
        })
    };
    attach.await.expect("the attach");

    // A page the daemon serves, fetched over a channel through its proxy.
    let file = env._tmp.path().join("page.mp4");
    let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&file, &body).expect("write the page's file");
    let opened = channel_call(&mut w0, &mut r0, 2, op::VIDEO_OPEN, serde_json::json!({ "path": file })).await;
    let url = opened.payload["url"].as_str().unwrap_or_else(|| panic!("video.open: {:?}", opened.payload)).to_string();
    let port = sot_protocol::page_url::loopback_port_from_url(&url).expect("a loopback page URL");
    let path = url.splitn(4, '/').nth(3).map(|p| format!("/{p}")).unwrap_or_else(|| "/".to_string());
    let (pr, mut pw) = handle.open().await.expect("a page channel").into_split();
    let mut pr = tokio::io::BufReader::new(pr);
    let handoff = sot_protocol::HelloReq::this_process("link-sshd-page", sot_protocol::HANDOFF_ROLE, Some("link-sshd-host".to_string())).expect("this process's account");
    let accepted = channel_call(&mut pw, &mut pr, 1, op::HELLO, serde_json::to_value(handoff).unwrap()).await;
    assert!(accepted.payload.get("error").is_none(), "the page's handoff hello: {:?}", accepted.payload);
    let proxied = channel_call(&mut pw, &mut pr, 2, op::PROXY_CONNECT, serde_json::json!({ "port": port })).await;
    assert_eq!(proxied.payload["ok"], true, "proxy.connect over the link: {:?}", proxied.payload);
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        pw.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        pw.flush().await.unwrap();
        let mut answer = Vec::new();
        tokio::time::timeout(BOUND, pr.read_to_end(&mut answer)).await.expect("the page within the bound").expect("read the page");
        let at = answer.windows(4).position(|w| w == b"\r\n\r\n").expect("an HTTP head") + 4;
        assert!(answer.starts_with(b"HTTP/1.1 200"), "{}", String::from_utf8_lossy(&answer[..at.min(200)]));
        assert!(answer[at..] == body[..], "the page's bytes came through whole");
    }

    assert_eq!(logins(&count), 1, "control, a row and a page cost one login");
    println!("link-sshd: control, a row and a page over {} login", logins(&count));
    drop((w0, r0));
    drop(link);
    env.kill_daemon_bounded().await;
}
