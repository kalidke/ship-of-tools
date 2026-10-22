use super::*;
use crate::comm::Env;
use std::path::PathBuf;

thread_local! {
    /// The program a link starts in place of ssh, per test thread: each test's runtime, and every link it spawns,
    /// runs on its own thread.
    pub(super) static STUB_PROGRAM: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The shutdown signal kills the ssh child and everything it started; the loop neither reconnects nor leaves a registered tree.
#[cfg(unix)]
#[tokio::test]
async fn shutdown_kills_the_link_child_and_never_reconnects() {
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("spawns");
    let bg = dir.path().join("bg");
    let descendant = sot_containment::leftover::Leftover::of_file(bg.clone());
    let stub = dir.path().join("stub-ssh");
    sot_platform::test_exec::write_executable(
        &stub,
        format!(
            "#!/bin/sh\necho x >> {}\nsleep 3108 >/dev/null 2>&1 &\necho $! > {}\nexec sleep 30\n",
            counter.display(),
            bg.display()
        ),
    );
    STUB_PROGRAM.set(Some(stub.to_string_lossy().into_owned()));
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", None).unwrap();
    let task = tokio::spawn(async move { hold_link(&Env::empty(), &recipe, "self", "sotd-self", sig).await });
    let began = Instant::now();
    // Spawn completes before the stub's first line has run; fire only once
    // that line has written the counter, so the one-spawn precondition is true.
    while std::fs::read_to_string(&counter).map_or(0, |s| s.lines().count()) == 0 {
        assert!(began.elapsed() < Duration::from_secs(5), "the stub child never ran");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!task.is_finished(), "the ready owner finished before the fire");
    while std::fs::read_to_string(&bg).map(|s| s.trim().is_empty()).unwrap_or(true) {
        assert!(began.elapsed() < Duration::from_secs(5), "the stub child never wrote its descendant's pid");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    sig.fire().expect("fire");
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("the link loop outlived the shutdown")
        .expect("link task");
    assert!(sig.held_groups().is_empty());
    assert!(descendant.gone(), "the link's descendant survived the shutdown");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(std::fs::read_to_string(&counter).unwrap().lines().count(), 1, "reconnected after the fire");
    STUB_PROGRAM.set(None);
}

fn setup(registry: &str, fe: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let (dir, home) = (t.path().join("sot"), t.path().join("comm"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(home.join("read")).unwrap();
    std::fs::write(home.join("registry.json"), registry).unwrap();
    std::fs::write(dir.join("fe-inbox.jsonl"), fe).unwrap();
    (t, dir, home)
}

const REG: &str = r#"{"agents":{"a":{"host":"h"},"b":{"host":"h"},"gone":{"host":""}}}"#;

fn msgs(home: &Path, h: &str) -> Vec<String> {
    std::fs::read_to_string(home.join("inbox").join(format!("{h}.jsonl")))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["msg"].as_str().unwrap().to_string())
        .collect()
}

fn fe_lines() -> String {
    [
        r#"{"from":"x","to":"a","text":"a-read","ts":"1"}"#,
        r#"{"from":"x","to":"a","text":"a-unread","ts":"2"}"#,
        r#"{"from":"x","to":"b","text":"b-unread","ts":"3"}"#,
        r#"{"from":"a","to":"a","text":"a-self","ts":"4"}"#,
        r#"{"from":"sot-fe","to":"a","text":"possible relay gap","ts":"5","kind":"fe_down","window":{"last_evidence":"0"}}"#,
        r#"{"from":"x","to":"gone","text":"not-listed","ts":"6"}"#,
        "not json",
    ]
    .join("\n")
        + "\n"
}

#[test]
fn moves_exactly_the_unread_lines_once() {
    let (_t, dir, home) = setup(REG, &fe_lines());
    std::fs::write(home.join("read/a.fe.cursor"), "1\n").unwrap();
    assert_eq!(move_fe_inbox_in(&Env::empty(), &dir, &home), Ok(2));
    assert_eq!(msgs(&home, "a"), ["a-unread"]);
    assert_eq!(msgs(&home, "b"), ["b-unread"]);
    assert!(!dir.join("fe-inbox.jsonl").exists() && !dir.join("fe-inbox.jsonl.moving").exists());
    assert!(dir.join("fe-inbox.jsonl.moved").exists());
    assert!(!home.join("read/a.fe.cursor").exists(), "a stale cursor outlived the move");
    // A second start finds nothing to move.
    assert_eq!(move_fe_inbox_in(&Env::empty(), &dir, &home), Ok(0));
    assert_eq!(msgs(&home, "a"), ["a-unread"]);
}

#[test]
fn a_move_interrupted_after_the_rename_finishes_without_loss() {
    let (_t, dir, home) = setup(REG, &fe_lines());
    // Interrupted part-way: renamed, one line already appended, never finished.
    std::fs::rename(dir.join("fe-inbox.jsonl"), dir.join("fe-inbox.jsonl.moving")).unwrap();
    let inbox = home.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    crate::comm::mail::inbox::file_frame(&inbox, "x", "a", false, "a-read", "1", Duration::from_secs(5), "none").unwrap();
    assert!(move_fe_inbox_in(&Env::empty(), &dir, &home).is_ok());
    let a = msgs(&home, "a");
    for want in ["a-read", "a-unread"] {
        assert!(a.iter().any(|m| m == want), "{want} lost: {a:?}");
    }
    assert_eq!(msgs(&home, "b"), ["b-unread"]);
    assert!(dir.join("fe-inbox.jsonl.moved").exists() && !dir.join("fe-inbox.jsonl.moving").exists());
}

#[test]
fn an_unreadable_registry_leaves_the_mail_where_it_is() {
    let (_t, dir, home) = setup("not a registry", &fe_lines());
    assert!(move_fe_inbox_in(&Env::empty(), &dir, &home).is_err());
    assert!(dir.join("fe-inbox.jsonl").exists());
    assert!(!dir.join("fe-inbox.jsonl.moving").exists());
}

#[test]
fn off_windows_the_old_inbox_is_left_untouched() {
    let (_t, dir, home) = setup(REG, &fe_lines());
    let before = std::fs::read(dir.join("fe-inbox.jsonl")).unwrap();
    assert_eq!(move_fe_inbox_if(&Env::empty(), false, &dir, &home), Ok(0));
    assert_eq!(std::fs::read(dir.join("fe-inbox.jsonl")).unwrap(), before);
    assert!(!dir.join("fe-inbox.jsonl.moving").exists() && !dir.join("fe-inbox.jsonl.moved").exists());
    assert!(!home.join("inbox").exists(), "a line was filed off Windows");
}

/// The far end of a link under test: it answers the hello when `hello` is set and each ping when `pings` is, and
/// sends an event the link ignores at each of `events` (seconds after the hello). Returns the pings it answered.
async fn fake_hub(rx: tokio::io::DuplexStream, mut tx: tokio::io::DuplexStream, hello: bool, pings: bool, events: Vec<u64>) -> u32 {
    // Frames reach the loop through a channel, whose receive is cancellation-safe.
    let (frames_tx, mut frames) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut rx = codec::buffered(rx);
        while let Ok((frame, _)) = codec::read_frame(&mut rx).await {
            let _ = frames_tx.send(frame);
        }
    });
    let began = tokio::time::Instant::now();
    let mut events = events.into_iter();
    let mut next = events.next();
    let mut answered = 0;
    loop {
        let due = next.map_or(began + Duration::from_secs(1 << 20), |s| began + Duration::from_secs(s));
        tokio::select! {
            frame = frames.recv() => {
                let Some(frame) = frame else { return answered };
                let reply = match frame.op.as_str() {
                    op::HELLO if hello => serde_json::json!({ "ok": true }),
                    op::PING if pings => serde_json::json!({ "ok": true }),
                    _ => continue,
                };
                let _ = codec::write_frame(&mut tx, &Frame::res(frame.id, &frame.op, reply), None).await;
                answered += u32::from(frame.op == op::PING);
            }
            () = tokio::time::sleep_until(due) => {
                let evt = Frame::evt(op::AGENT_MESSAGE, serde_json::json!({ "from": "a", "to": "", "text": "x", "ts": "t" }));
                let _ = codec::write_frame(&mut tx, &evt, None).await;
                next = events.next();
            }
        }
    }
}

/// The answers a link gives, in the order they arrive, to one offer for each of `handles` (request ids from 100),
/// from a hub that answers its hello. Each answer comes within 20 s.
async fn answers_to_offers(env: &Env, self_host: &str, handles: &[&str]) -> Vec<(u64, serde_json::Value)> {
    let (link_tx, hub_rx) = tokio::io::duplex(64 * 1024);
    let (mut hub_tx, link_rx) = tokio::io::duplex(64 * 1024);
    let (host, env) = (self_host.to_string(), env.clone());
    let link = tokio::spawn(async move {
        let (mut tx, mut rx) = (link_tx, codec::buffered(link_rx));
        converse(&env, &mut tx, &mut rx, &host, "sotd-test").await
    });
    let mut hub_rx = codec::buffered(hub_rx);
    let (hello, _) = codec::read_frame(&mut hub_rx).await.expect("the link's hello");
    codec::write_frame(&mut hub_tx, &Frame::res(hello.id, op::HELLO, serde_json::json!({ "ok": true })), None).await.unwrap();
    for (id, to) in (100u64..).zip(handles) {
        let req = serde_json::json!({ "from": "s", "to": to, "text": "x", "forwarded": true });
        codec::write_frame(&mut hub_tx, &Frame::req(id, op::COMM_FILE, req), None).await.unwrap();
    }
    let mut answers = Vec::new();
    while answers.len() < handles.len() {
        let read = tokio::time::timeout(Duration::from_secs(20), codec::read_frame(&mut hub_rx)).await;
        let (frame, _) = read.expect("an answer within 20 s").expect("the link's answer");
        if frame.kind == Kind::Res && frame.op == op::COMM_FILE {
            answers.push((frame.id, frame.payload));
        }
    }
    link.abort();
    answers
}

/// A comm folder for a link under test, which `SOT_COMM_HOME` names in the environment returned with it: `agents` its registry;
/// with `record`, the inbox lock record that lets this daemon append.
fn link_home(agents: serde_json::Value, record: bool) -> (tempfile::TempDir, Env) {
    let home = tempfile::tempdir().unwrap();
    let inbox = home.path().join("inbox");
    std::fs::create_dir(&inbox).unwrap();
    std::fs::write(home.path().join("registry.json"), serde_json::json!({ "agents": agents }).to_string()).unwrap();
    if record {
        std::fs::write(home.path().join("inbox-lock-manager"), format!("{}\n", crate::comm::mail::inbox::lock_identity(&inbox))).unwrap();
    }
    let env = Env::empty()
        .with("SOT_COMM_HOME", home.path())
        .with("SOT_HOSTS", home.path().join("no-hosts.toml"))
        .with(crate::comm::mail::inbox::INBOX_LOCK_WAIT_ENV, "2");
    (home, env)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

/// A box answers the hub's offer for a handle its own folder does not list under its own host `not_here`, whatever
/// else it would refuse: here it has no inbox lock record, so every filing it attempted would fail. The handle it
/// does list under its host gets its own refusal. (The review's probe: a guest box's refusal had outranked the
/// holder's and `not_here`.)
#[test]
fn a_link_answers_not_here_for_a_handle_its_box_does_not_hold() {
    let agents = serde_json::json!({
        "stale": { "host": "b", "last_seen": "2026-01-01T00:00:00Z" },
        "here": { "host": "a-guest", "last_seen": "2026-01-01T00:00:00Z" },
    });
    let (_home, env) = link_home(agents, false);
    let mut answers = runtime().block_on(answers_to_offers(&env, "a-guest", &["stale", "nobody", "here"]));
    answers.sort_by_key(|(id, _)| *id);
    let not_here = |h: &str| serde_json::json!({ "error": format!("no box knows that handle: {h}"), "code": "not_here" });
    assert_eq!(answers[0].1, not_here("stale"), "a handle another box holds");
    assert_eq!(answers[1].1, not_here("nobody"), "a handle no box holds");
    let own = &answers[2].1;
    assert!(
        own["code"] == "file_failed" && own["error"].as_str().is_some_and(|e| e.starts_with("no inbox lock record at ")),
        "the handle this box holds gets this box's own refusal: {own}"
    );
}

/// Two offers in a row, the first for a handle whose inbox lock is held: the second is filed and answered while the
/// first still waits, so its answer arrives first. A link that filed its offers one at a time answered the second
/// only after the first's lock wait.
#[test]
fn an_offer_is_answered_without_waiting_behind_another() {
    let now = crate::comm::registry::registry::iso8601_utc_from_secs(crate::comm::registry::registry::unix_now_secs());
    let agents = serde_json::json!({ "held": { "host": "a", "last_seen": now }, "free": { "host": "a", "last_seen": now } });
    let (home, env) = link_home(agents, true);
    let held = std::fs::OpenOptions::new().read(true).create(true).append(true).open(home.path().join("inbox/held.lock")).unwrap();
    held.lock().unwrap();
    let answers = runtime().block_on(answers_to_offers(&env, "a", &["held", "free"]));
    assert_eq!(answers[0], (101, serde_json::json!({ "ok": true })), "the free offer was answered first: {answers:?}");
    assert_eq!(
        answers[1],
        (100, serde_json::json!({ "error": "the inbox lock for @held was held for 2s — nothing was appended", "code": "file_failed" }))
    );
}

/// A hub that sends no frame for three ping intervals ends the link, before the hello's reply or after it; a hub
/// that answers every ping keeps it, and so does one that only sends events, until 90 s after its last.
#[tokio::test(start_paused = true)]
async fn a_hub_silent_for_three_ping_intervals_ends_the_link() {
    let cases: [(&str, bool, bool, Vec<u64>, Option<u64>); 4] = [
        ("silent before the hello reply", false, false, vec![], Some(90)),
        ("silent after the hello reply", true, false, vec![], Some(90)),
        ("answering every ping", true, true, vec![], None),
        ("events every 80s until 560s, no ping answered", true, false, (1..=7).map(|n| n * 80).collect(), Some(650)),
    ];
    let mut wrong = Vec::new();
    for (what, hello, pings, events, ends_at) in cases {
        let (link_tx, hub_rx) = tokio::io::duplex(64 * 1024);
        let (hub_tx, link_rx) = tokio::io::duplex(64 * 1024);
        let hub = tokio::spawn(fake_hub(hub_rx, hub_tx, hello, pings, events));
        let (mut tx, mut rx) = (link_tx, codec::buffered(link_rx));
        let began = tokio::time::Instant::now();
        let ended = tokio::time::timeout(Duration::from_secs(1200), converse(&Env::empty(), &mut tx, &mut rx, "self", "sotd-self")).await;
        let took = began.elapsed();
        drop((tx, rx));
        let answered = hub.await.unwrap();
        let seen = match &ended {
            Ok(Err(why)) => format!("\"{why}\" after {took:?}"),
            Ok(Ok(())) => format!("closed after {took:?}"),
            Err(_) => format!("linked after {took:?}, {answered} pings answered"),
        };
        eprintln!("hub-silence: {what} -> {seen}");
        let right = match (&ended, ends_at) {
            (Ok(Err(why)), Some(at)) => {
                why == "nothing from the hub in 90s" && took >= Duration::from_secs(at) && took < Duration::from_secs(at + 1)
            }
            (Err(_), None) => answered >= 39,
            _ => false,
        };
        if !right {
            wrong.push(format!("{what}: {seen}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A stub ssh behind which the hub stopped: it prints a login warning on stderr, answers the hello, writes
/// `<dir>/said`, then stays up and silent.
#[cfg(unix)]
fn silent_hub_ssh(dir: &Path) -> String {
    let reply = serde_json::to_string(&Frame::res(1, op::HELLO, serde_json::json!({ "ok": true }))).unwrap();
    let (stub, said) = (dir.join("stub-ssh"), dir.join("said"));
    let script = format!(
        "#!/bin/sh\necho 'Warning: Permanently added hub (ED25519) to the list of known hosts.' >&2\nprintf '%s\\n' '{reply}'\n: > '{}'\nexec sleep 30\n",
        said.display()
    );
    sot_platform::test_exec::write_executable(&stub, script);
    stub.to_string_lossy().into_owned()
}

/// ssh's stderr may hold a byte that is not UTF-8 (a far host's banner, a remote's error): a link that fails still
/// names ssh's last line, with U+FFFD for the byte.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_link_names_an_ssh_line_that_is_not_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let stub = dir.path().join("stub-ssh");
    sot_platform::test_exec::write_executable(&stub, "#!/bin/sh\nprintf 'Permission denied \\351 (publickey).\\n' >&2\nexit 255\n".to_string());
    STUB_PROGRAM.set(Some(stub.to_string_lossy().into_owned()));
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", None).unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(30), link_once(&Env::empty(), &recipe, "self", "sotd-self", sig)).await;
    STUB_PROGRAM.set(None);
    let why = ended.expect("the link ends when its ssh exits").expect_err("a stub that exits fails the link");
    assert!(why.ends_with(": Permission denied \u{fffd} (publickey)."), "{why}");
}

/// A hub silent behind a live ssh ends the link with its own reason alone: ssh's last line is a stale login
/// warning, not the diagnosis. The cut ends the ssh tree, though ssh never exited.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_silent_hub_ends_the_link_without_a_stale_ssh_line() {
    let dir = tempfile::tempdir().unwrap();
    STUB_PROGRAM.set(Some(silent_hub_ssh(dir.path())));
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", None).unwrap();
    let link = tokio::spawn(async move { link_once(&Env::empty(), &recipe, "self", "sotd-self", sig).await });
    // A running blocking task holds the paused clock, so the link has the stub's warning and reply before it waits.
    let said = dir.path().join("said");
    tokio::task::spawn_blocking(move || {
        let began = Instant::now();
        while !said.exists() {
            assert!(began.elapsed() < Duration::from_secs(5), "the stub never spoke");
            std::thread::sleep(Duration::from_millis(10));
        }
    })
    .await
    .unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(1200), link).await.expect("the link outlived a silent hub").unwrap();
    assert_eq!(ended, Err("nothing from the hub in 90s".to_string()));
    assert!(sig.held_groups().is_empty(), "the cut link's ssh tree is still held");
}

/// A link cut for silence, its ssh still up, is dialled again, and the box never holds two links at once.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_link_cut_for_silence_is_dialled_again() {
    let dir = tempfile::tempdir().unwrap();
    STUB_PROGRAM.set(Some(silent_hub_ssh(dir.path())));
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", None).unwrap();
    let task = tokio::spawn(async move { hold_link(&Env::empty(), &recipe, "self", "sotd-self", sig).await });
    // Each link is one process group. The paused clock runs the 90 s cut and the redial in a moment of real time.
    let (began, mut seen) = (Instant::now(), Vec::new());
    while seen.len() < 2 {
        let held = sig.held_groups();
        assert!(held.len() <= 1, "two links held at once: {held:?}");
        seen.extend(held.into_iter().filter(|g| !seen.contains(g)).collect::<Vec<_>>());
        assert!(began.elapsed() < Duration::from_secs(10), "no second link after a silence cut: {seen:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    sig.fire().expect("fire");
    tokio::time::timeout(Duration::from_secs(3), task).await.expect("the link loop outlived the shutdown").expect("link task");
    assert!(sig.held_groups().is_empty());
}

#[test]
fn a_cursor_past_the_end_reads_from_the_start() {
    let (_t, dir, home) = setup(REG, &fe_lines());
    std::fs::write(home.join("read/a.fe.cursor"), "99").unwrap();
    assert_eq!(move_fe_inbox_in(&Env::empty(), &dir, &home), Ok(3));
    assert_eq!(msgs(&home, "a"), ["a-read", "a-unread"]);
}
