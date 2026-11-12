use super::*;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use sot_chat::state::State;
use sot_protocol::chat::{ChatAuthor, Origin};

use crate::comm::registry::registry::iso8601_utc_from_secs;
use crate::comm::test_home::TestHome;
use crate::rows::Workspace;

fn record(workspace: &str, channel: &str, ts: &str, text: &str) -> ChatRecord {
    ChatRecord {
        workspace: workspace.into(),
        channel: channel.into(),
        ts: Ts::parse(ts).unwrap(),
        thread_ts: Ts::parse(ts).unwrap(),
        author: ChatAuthor { id: "U0MEMBER".into(), name: "Ada".into() },
        origin: Origin::Member,
        text: text.into(),
        truncated: false,
        reply_count: 0,
        files: Vec::new(),
        latest_reply: None,
        limited: false,
        omitted: false,
        files_omitted: 0,
        edited: false,
    }
}

/// A scratch comm folder (the registry turn held for the test's life), the daemon's rows, and a mapped folder.
struct World {
    th: TestHome,
    rows: Workspaces,
    folder: PathBuf,
    _dir: tempfile::TempDir,
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("mapped");
        std::fs::create_dir_all(&folder).unwrap();
        World { th: TestHome::new(), rows: Workspaces::new(), folder: super::super::canonical(&folder).unwrap(), _dir: dir }
    }

    fn name(&self) -> String {
        self.folder.to_string_lossy().into_owned()
    }

    /// A row of `agent` named `name` (so its handle) in the mapped folder, or in `folder`.
    fn row_in(&self, folder: &PathBuf, agent: &str, name: &str) -> Arc<Workspace> {
        self.rows.insert(Workspace::from_label(&format!("{agent}-{name}"), folder.clone(), false, agent.into(), name.into(), String::new()))
    }

    fn row(&self, agent: &str, name: &str) -> Arc<Workspace> {
        self.row_in(&self.folder, agent, name)
    }

    /// The registry's entry for `handle`: this computer's, bound to `row`, last seen `age` seconds ago.
    fn session(&self, handle: &str, row: &str, age: u64) {
        let seen = iso8601_utc_from_secs(unix_now_secs() - age);
        self.th.put(handle, json!({"host": "box1", "workspace_id": row, "repo": "r", "root": "/", "last_seen": seen}));
    }

    fn recipient(&self) -> Result<Recipient, &'static str> {
        recipient(&self.rows, &self.name(), &self.th.env, unix_now_secs())
    }

    /// Lets this process append to the inboxes itself, as the folder's hub would.
    fn appends(&self) {
        self.th.own_record();
    }

    fn hosts(&self, text: &str) {
        std::fs::write(self.th.home.parent().unwrap().join("hosts.toml"), text).unwrap();
    }
}

fn is(r: Result<Recipient, &'static str>, handle: &str, row: &Arc<Workspace>) {
    assert_eq!(r, Ok(Recipient { handle: handle.into(), row: row.workspace_id.clone() }));
}

#[test]
fn the_mail_goes_to_the_folders_one_claude_row_else_its_one_codex_row() {
    let w = World::new();
    let elsewhere = w._dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let elsewhere = super::super::canonical(&elsewhere).unwrap();
    // A row of another folder, a row with no handle, and the inert anchor (no agent runs in it) are never the recipient.
    let far = w.row_in(&elsewhere, "claude", "far");
    w.session("far", &far.workspace_id, 5);
    w.row("claude", "");
    let anchor = w.row("none", "anchor");
    w.rows.set_default(&anchor.workspace_id);
    w.session("anchor", &anchor.workspace_id, 5);
    assert_eq!(w.recipient(), Err("no_row"));
    // A Codex row alone is the recipient; with a Claude row beside it, the Claude row; two Claude rows are ambiguous.
    let codex = w.row("codex", "cx");
    w.session("cx", &codex.workspace_id, 5);
    is(w.recipient(), "cx", &codex);
    let claude = w.row("claude", "cl");
    w.session("cl", &claude.workspace_id, 5);
    is(w.recipient(), "cl", &claude);
    w.row("claude", "cl2");
    assert_eq!(w.recipient(), Err("ambiguous_row"));
}

#[tokio::test]
async fn a_handle_moved_before_the_append_is_not_filed() {
    let w = World::new();
    w.appends();
    let row = w.row("claude", "cl");
    w.session("cl", &row.workspace_id, 5);
    let to = w.recipient().unwrap();
    let rec = record("T0AAAA", "C0AAAA", "1700000100.000100", "hello");
    assert_eq!(append(&w.th.env, &to, &rec).await, Ok(()));
    let lines = w.th.inbox("cl");
    assert_eq!((lines.len(), lines[0]["from"].as_str(), lines[0]["repo"].as_str(), lines[0]["msg"].as_str()), (1, Some("#chat"), Some("daemon"), Some(body(&rec).as_str())));
    // The handle's session now binds another row: the recipient was read before, and the append checks again.
    w.session("cl", "ws-another-0", 5);
    let later = record("T0AAAA", "C0AAAA", "1700000200.000100", "again");
    let refused = append(&w.th.env, &to, &later).await.unwrap_err();
    assert!(refused.starts_with("not_here: ") && refused.contains("no longer bound"), "the code and its sentence: {refused}");
    assert_eq!(w.th.inbox("cl").len(), 1, "a moved handle was filed to");
}

#[test]
fn a_handle_with_no_live_session_is_absent() {
    let w = World::new();
    let row = w.row("claude", "cl");
    assert_eq!(w.recipient(), Err("no_live_session"), "no registry entry");
    w.session("cl", &row.workspace_id, 3600);
    assert_eq!(w.recipient(), Err("no_live_session"), "a heartbeat an hour old");
    w.session("cl", "ws-another-0", 5);
    assert_eq!(w.recipient(), Err("no_live_session"), "a live session of another row");
    // A Claude row whose session is gone is absent: a live Codex row in the folder never takes its mail.
    let codex = w.row("codex", "cx");
    w.session("cx", &codex.workspace_id, 5);
    w.session("cl", &row.workspace_id, 3600);
    assert_eq!(w.recipient(), Err("no_live_session"), "a dead Claude row fell through to the Codex row");
    w.session("cl", &row.workspace_id, 5);
    is(w.recipient(), "cl", &row);
}

#[tokio::test]
async fn a_filing_job_files_nothing_once_the_topology_names_another_hub() {
    let w = World::new();
    w.appends();
    let row = w.row("claude", "cl");
    w.session("cl", &row.workspace_id, 5);
    let rec = record("T0AAAA", "C0AAAA", "1700000100.000100", "hello");
    w.hosts("hub = \"box1\"\n[host.box1]\ndaemon = true\n");
    assert_eq!(file(&w.th.env, &w.rows, &w.name(), &rec).await, Ok(()));
    // The hub moved to another computer between the queueing and the job.
    w.hosts("hub = \"far\"\n[host.far]\ndaemon = true\n[host.box1]\ndaemon = true\n");
    let next = record("T0AAAA", "C0AAAA", "1700000200.000100", "again");
    assert_eq!(file(&w.th.env, &w.rows, &w.name(), &next).await, Err("other_hub".into()));
    assert_eq!(w.th.inbox("cl").len(), 1, "a job filed after the hub moved");
    // A topology that cannot be read names no one else: the job files as before.
    w.hosts("this is [not a topology");
    assert_eq!(file(&w.th.env, &w.rows, &w.name(), &next).await, Ok(()));
    assert_eq!(w.th.inbox("cl").len(), 2);
}

/// One inbox line from `from` as the daemon writes it, `\n`-terminated unless `whole` is false.
fn line(from: &str, msg: &str, whole: bool) -> String {
    let v = json!({"from": from, "to": "h", "repo": "daemon", "msg": msg, "ts": "2026-01-01T00:00:00Z"});
    format!("{v}{}", if whole { "\n" } else { "" })
}

fn inbox(w: &World, handle: &str, lines: &[String]) {
    std::fs::write(w.th.home.join("inbox").join(format!("{handle}.jsonl")), lines.concat()).unwrap();
}

#[test]
fn the_starts_read_gives_each_channels_filings_from_every_inbox() {
    let w = World::new();
    let mail = |ws: &str, ch: &str, ts: &str| body(&record(ws, ch, ts, "text"));
    let (a, b) = (mail("T0AAAA", "C0AAAA", "1700000100.000100"), mail("T0AAAA", "C0BBBB", "1700000200.000100"));
    // The first channel's filing is in the inbox of a handle no row holds now.
    inbox(&w, "gone", &[line("#chat", &a, true), line("someone", &mail("T0AAAA", "C0AAAA", "1700000500.000100"), true)]);
    inbox(
        &w,
        "live",
        &[
            line("#chat", &b, true),
            line("#chat", &mail("T0ZZZZ", "C0AAAA", "1700000300.000100"), true),
            line("#chat", &mail("T0AAAA", "C0CCCC", "1700000400.000100"), true),
            "not json\n".into(),
            line("#chat", &mail("T0AAAA", "C0AAAA", "1700000600.000100"), false),
        ],
    );
    // Every well-formed `#chat` line of every inbox, in file-name order; `Delivery::start` keeps each channel's own.
    let got: Vec<_> = read_filings(&w.th.env).unwrap().into_iter().map(|f| (f.workspace, f.channel, f.ts.as_str().to_string())).collect();
    let want = [("T0AAAA", "C0AAAA", "1700000100.000100"), ("T0AAAA", "C0BBBB", "1700000200.000100"), ("T0ZZZZ", "C0AAAA", "1700000300.000100"), ("T0AAAA", "C0CCCC", "1700000400.000100")];
    assert_eq!(got, want.map(|(w, c, t)| (w.to_string(), c.to_string(), t.to_string())));
    // An inbox that cannot be read fails the whole read: no filings is not what it found.
    std::fs::create_dir(w.th.home.join("inbox").join("b.jsonl")).unwrap();
    let failed = read_filings(&w.th.env).unwrap_err();
    assert!(failed.contains("@b"), "{failed}");
}

#[test]
fn the_commit_writes_the_boundary_into_a_private_folder_it_makes() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state").join("chat");
    let (floor, to) = (Ts::parse("1700000000.000000").unwrap(), Ts::parse("1700000300.000100").unwrap());
    assert_eq!(commit(&state, "T0AAAA", "C0AAAA", &to), Ok(()));
    assert_eq!(State::load(&state, "T0AAAA", "C0AAAA", &floor).unwrap().boundary(), &to);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&state).unwrap().permissions().mode() & 0o777, 0o700);
    }
    // A folder that cannot be made is a failure the owner logs.
    let file = dir.path().join("plain");
    std::fs::write(&file, "x").unwrap();
    assert_eq!(commit(&file, "T0AAAA", "C0AAAA", &to), Err(StateError::Unwritable));
}
