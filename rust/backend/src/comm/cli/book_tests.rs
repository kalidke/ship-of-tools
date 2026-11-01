use super::*;
use crate::comm::test_home::TestHome;
use serde_json::json;

#[test]
fn leave_removes_this_sessions_entry_and_self_file_or_only_an_orphans_entry() {
    let t = TestHome::new();
    t.self_file(&t.nopane(), "me-box1");
    t.live("me-box1", &t.root());
    t.live("orphan-box1", "/o");
    let o = leave(&t.env, &["--name".into(), "orphan-box1".into()]);
    assert_eq!(o.stdout, "Removed @orphan-box1 from the registry (row only; comm-despawn.sh does full teardown).\n", "{}", o.stderr);
    assert_eq!(leave(&t.env, &["--name".into(), "orphan-box1".into()]).stdout, "@orphan-box1 not in registry \u{2014} nothing to do.\n");
    assert_eq!(leave(&t.env, &["--bogus".into()]).code, 1, "an unknown argument never removes the caller's own row");
    assert!(t.agents().get("me-box1").is_some());
    assert_eq!(leave(&t.env, &[]).stdout, "Left sot-comm (@me-box1 removed).\n");
    assert!(t.agents().get("me-box1").is_none() && !t.nopane().exists());
    assert_eq!(leave(&t.env, &[]).stdout, "Not joined \u{2014} nothing to do.\n");
}

/// A handle a row holds is removed only where the daemon says no row does.
#[test]
fn leave_refuses_a_handle_another_row_holds() {
    let t = TestHome::new();
    t.put("rows-box1", json!({"host": "box1", "workspace_id": "ws-q"}));
    let o = leave(&t.env, &["--name".into(), "rows-box1".into()]);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("REFUSING to remove @rows-box1") && o.stderr.contains("cannot remove it"), "{}", o.stderr);
    assert!(t.agents().get("rows-box1").is_some());
}

#[test]
fn list_names_each_handle_its_liveness_and_its_work_state() {
    let t = TestHome::new();
    t.self_file(&t.nopane(), "me-box1");
    t.live("me-box1", &t.root());
    t.put("old-box1", json!({"host": "box1", "repo": "o", "last_seen": "2020-01-01T00:00:00Z", "state": "blocked", "summary": "q?", "expertise": ["a", "b"]}));
    let o = list(&t.env, &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains("  @me-box1") && o.stdout.contains("live") && o.stdout.contains("<- me"), "{}", o.stdout);
    assert!(o.stdout.contains("[a/b]") && o.stdout.contains("    [blocked] q?") && o.stdout.contains("stale "), "{}", o.stdout);
    assert!(o.stdout.contains("attached frontend boxes"), "{}", o.stdout);
    // The list is this computer's registry, and says so: a session once read it as everyone it could reach.
    assert!(o.stdout.contains("in this computer's registry") && o.stdout.contains(super::super::join::REACH_NOTE), "{}", o.stdout);
    assert_eq!(epoch_of("1970-01-02T00:00:01Z"), Some(86_401));
    assert_eq!(fmt_age(7200), "2h ago");
}

/// A stand-in daemon on `path`: answers one connection's hello, then its `version.query` with `answer`.
#[cfg(unix)]
fn roster_daemon(path: &std::path::Path, answer: serde_json::Value) -> std::thread::JoinHandle<()> {
    use sot_protocol::{codec, Frame};
    let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        let (conn, _) = listener.accept().unwrap();
        let mut w = conn.try_clone().unwrap();
        let mut r = std::io::BufReader::new(conn);
        codec::read_frame_blocking(&mut r).unwrap();
        codec::write_frame_blocking(&mut w, &Frame::res(0, sot_protocol::op::HELLO, json!({}))).unwrap();
        let req = codec::read_frame_blocking(&mut r).unwrap();
        assert_eq!(req.op, sot_protocol::op::VERSION_QUERY);
        codec::write_frame_blocking(&mut w, &Frame::res(req.id, sot_protocol::op::VERSION_QUERY, answer)).unwrap();
    })
}

/// The windows come from this computer's daemon's own answer, never from another program's text within a bound: a
/// session in a row once read "none attached" from inside an attached window, when that program was slow.
#[cfg(unix)]
#[test]
fn list_shows_the_windows_this_computers_daemon_names_and_says_why_when_none() {
    let daemon = json!({"app_version": "0.7.0", "protocol": 4, "host": "box1", "uptime_s": 7200});
    let mut t = TestHome::new();
    t.self_file(&t.nopane(), "me-box1");
    t.live("me-box1", &t.root());
    // No daemon at all.
    let none = list(&t.env, &[]);
    assert!(none.stdout.contains("attached frontend boxes: none, since no daemon of this account runs on this computer"), "{}", none.stdout);

    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("d.sock");
    t.set("SOT_SOCKET", &sock);
    let served = roster_daemon(&sock, json!({
        "daemon": daemon,
        "clients": [
            {"client_id": "w1", "app_version": "0.7.0", "protocol": 4, "host": "box2", "role": "fe", "name": "fe@box2", "active": true,
             "sessions": [{"handle": "proj-box2", "state": "working", "summary": "building", "status_at": ""}]},
            {"client_id": "l1", "app_version": "0.7.0", "protocol": 4, "host": "box2", "role": "link", "name": "link@box2"},
            {"client_id": "w3", "app_version": "0.6.6", "protocol": 4, "host": "box3", "role": "fe", "name": "fe@box3"}
        ],
        "disconnected": [{"identity": "fe@box4", "since_s": 120}]
    }));
    let o = list(&t.env, &[]);
    served.join().unwrap();
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains(&format!("attached frontend boxes (this computer's daemon at unix:{}, up 2h):", sock.display())), "{}", o.stdout);
    assert!(o.stdout.contains("  fe@box2") && o.stdout.contains("active") && o.stdout.contains("    @proj-box2 [working] building"), "{}", o.stdout);
    assert!(!o.stdout.contains("link@box2"), "only windows are listed: {}", o.stdout);
    assert!(o.stdout.contains("  fe@box3") && o.stdout.contains("    (declares no sessions"), "{}", o.stdout);
    assert!(o.stdout.contains("  fe@box4  (frontend not connected since 2m ago"), "{}", o.stdout);

    std::fs::remove_file(&sock).unwrap();
    let served = roster_daemon(&sock, json!({"daemon": daemon, "clients": [], "disconnected": []}));
    let empty = list(&t.env, &[]);
    served.join().unwrap();
    assert!(empty.stdout.contains("  (none: no window is attached to this computer's daemon now"), "{}", empty.stdout);
}

/// A computer that reaches the hub over ssh (a window's computer) lists the hub's rows; the hub itself and a server
/// sharing its home (a socket relay) do not, since their registry is the hub's.
#[test]
fn only_a_computer_that_reaches_the_hub_over_ssh_lists_the_hubs_rows() {
    assert_eq!(hub_over_ssh("ssh:hub1"), Some("hub1"));
    assert_eq!(hub_over_ssh("ssh:hub1/hub1"), Some("hub1"));
    assert_eq!(hub_over_ssh("unix:/run/user/1000/sot/sot.sock"), None);
    assert_eq!(hub_over_ssh("pipe:\\\\.\\pipe\\sot-local"), None);
    assert_eq!(hub_over_ssh("ssh:"), None);
}

/// The hub's rows print each declared handle with its work-state line, leave out a row with no declared handle, and
/// say so when the hub did not answer, so a session finds a hub handle by name instead of guessing one.
#[test]
fn the_hubs_rows_list_each_declared_handle_and_say_why_when_none() {
    let row = |handle: &str, state: &str, summary: &str| {
        serde_json::from_value::<sot_protocol::WorkspaceListEntry>(serde_json::json!({
            "workspace_id": "ws-x", "slug": "x", "label": "x", "project_root": "/p", "session_name": "x",
            "kernel_running": false, "is_default": false, "agent_handle": handle, "agent_state": state, "agent_summary": summary,
        }))
        .expect("a list entry")
    };
    let lines = hub_row_lines("hub1", "ssh:hub1", Ok(vec![row("admiral-hub1", "working", "reviewing a merge"), row("", "", "")]), 0);
    let text = lines.join("\n");
    assert!(text.contains("the hub's rows (hub1") && text.contains("send to any by its handle"), "{text}");
    assert!(text.contains("@admiral-hub1") && text.contains("[working] reviewing a merge"), "{text}");
    assert_eq!(lines.iter().filter(|l| l.starts_with("  @")).count(), 1, "a row with no declared handle is left out: {text}");
    let lines = hub_row_lines("hub1", "ssh:hub1", Ok(vec![]), 0);
    assert!(lines.iter().any(|l| l.contains("(none: no row on the hub")), "{lines:?}");
    let lines = hub_row_lines("hub1", "ssh:hub1", Err("ssh: connect timed out".into()), 0);
    assert!(lines[0].contains("unknown, since the hub did not answer: ssh: connect timed out"), "{lines:?}");
}
