// hub_link.rs — the daemon's own link to the hub (decision 0031: a Windows box's daemon holds its own link to the hub,
// so its rows receive with the window closed).
//
// A box whose topology entry is `frontend = true` reaches the hub over one `ssh:` stdio child, as a `link`-role
// connection. The daemon holds it for as long as it runs. The hub sends it a `comm.file` request for each handle the
// hub's own folder does not list (`comm::mail::links`); this daemon answers it through `filer::file_offer`: `not_here`
// unless its own folder lists the handle under this box's host, else the filing's verdict, `ok` or the refusal. It
// sends the hub nothing but its hello and a `ping` every 30 s. A hub that sends no frame for three of those intervals
// ends the link, which then redials: ssh's ServerAlive covers a dead network or sshd, never a hub daemon or bridge
// stopped behind a live sshd (`SILENT_PINGS`).
//
// The topology is read once at start, like the frontend's tunnel set; which box holds a link is `link_target`'s one
// rule: not the hub, and its relay endpoint is `ssh:`.

use std::path::Path;
use std::time::{Duration, Instant};

use sot_protocol::{codec, op, CommFileReq, Frame, HelloReq, Kind};


const BACKOFF_FLOOR: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
/// How often the link pings the hub once its hello is answered: the window's interval, a third of the daemon's own
/// `PING_READ_DEADLINE`.
const PING_EVERY: Duration = Duration::from_secs(30);
/// How many ping intervals with no frame from the hub end the link, the hello's reply included. A live hub answers
/// each ping at once only while every request the link sends is one the hub answers at once in its connection loop:
/// `hello` and `ping`. A request the hub could make wait must not ride this link, or a busy hub is cut as a silent
/// one. Frames, not bytes: nothing the hub sends this connection carries a blob, so each frame is one envelope under
/// the codec's 1 MiB cap.
const SILENT_PINGS: u32 = 3;

pub async fn run() {
    // Before the first connection: a failed move leaves the old inbox where it was.
    if let Err(e) = tokio::task::spawn_blocking(move_fe_inbox).await.unwrap_or_else(|e| Err(e.to_string())) {
        tracing::warn!("hub link: the old frontend inbox was not moved: {e}");
    }
    let self_host = crate::comm::mail::filer::comm_self_host();
    let recipe = match recipe_for(&self_host) {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!("hub link not started: {e}");
            return;
        }
    };
    let name = format!("sotd-{self_host}");
    tracing::info!(%recipe, %name, "hub link starting");
    hold_link(&recipe, &self_host, &name, crate::lifecycle::child_signal::process()).await;
}

/// Keep the link up until `sig` fires; it never reconnects after.
async fn hold_link(
    recipe: &sot_protocol::topology::ssh_bridge::SshRecipe,
    self_host: &str,
    name: &str,
    sig: &'static crate::lifecycle::child_signal::Signal,
) {
    // One rule with the window's control transport and the attach worker: the wait starts over only after a link
    // that lasted `STABLE`.
    let mut redial = sot_log::host::redial::Redial::new(BACKOFF_FLOOR, BACKOFF_CAP);
    loop {
        if sig.is_fired() {
            return;
        }
        let began = Instant::now();
        match link_once(recipe, self_host, name, sig).await {
            Ok(()) => tracing::info!("hub link closed"),
            Err(e) => tracing::warn!("hub link dropped: {e}"),
        }
        let wait = redial.after(began.elapsed());
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = sig.fired() => return,
        }
    }
}

/// The ssh child's recipe, or `None` when this box has no link to hold: no
/// topology, or `link_target` names none.
fn recipe_for(self_host: &str) -> Result<Option<sot_protocol::topology::ssh_bridge::SshRecipe>, String> {
    let Some((_, topo)) = sot_protocol::topology::load()? else {
        return Ok(None);
    };
    match link_target(&topo, self_host)? {
        Some(target) => sot_protocol::topology::ssh_bridge::SshRecipe::new(&target, None).map(Some),
        None => Ok(None),
    }
}

/// The one rule for which box holds a link: the ssh target of the link `host` holds, or `None` when it holds none (it
/// is the hub, or its relay endpoint is not an `ssh:` one).
fn link_target(topo: &sot_protocol::topology::Topology, host: &str) -> Result<Option<String>, String> {
    if host.is_empty() || topo.hub == host {
        return Ok(None);
    }
    Ok(sot_protocol::topology::relay_endpoint(topo, host)?.strip_prefix("ssh:").map(str::to_string))
}

/// On the topology's hub, the boxes that hold a link by `link_target` but have none among `attached`; empty on any
/// other daemon, or with no topology.
pub(crate) fn unlinked(attached: &[String]) -> Vec<String> {
    let self_host = crate::comm::mail::filer::comm_self_host();
    let Ok(Some((_, topo))) = sot_protocol::topology::load() else {
        return Vec::new();
    };
    if self_host.is_empty() || topo.hub != self_host {
        return Vec::new();
    }
    topo.hosts
        .iter()
        .map(|h| h.name.clone())
        .filter(|name| matches!(link_target(&topo, name), Ok(Some(_))) && !attached.contains(name))
        .collect()
}

/// The ssh command, for the signal to start. Tests swap the program for a stub, nothing else.
fn spawn_link(recipe: &sot_protocol::topology::ssh_bridge::SshRecipe) -> Result<tokio::process::Command, String> {
    #[cfg(test)]
    if let Some(program) = tests::STUB_PROGRAM.with_borrow(Clone::clone) {
        let mut cmd = tokio::process::Command::new(program);
        cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        return Ok(cmd);
    }
    sot_protocol::topology::ssh_bridge::LinkGate::default()
        .command(recipe)
        .map(tokio::process::Command::from)
        .map_err(|e| format!("spawn {recipe}: {e}"))
}

/// One connection, from spawn to the first failure. The child and what it
/// started die with `contained`.
async fn link_once(
    recipe: &sot_protocol::topology::ssh_bridge::SshRecipe,
    self_host: &str,
    name: &str,
    sig: &'static crate::lifecycle::child_signal::Signal,
) -> Result<(), String> {
    let mut cmd = spawn_link(recipe)?;
    let mut contained = sig.spawn(&mut cmd).map_err(|e| format!("spawn {recipe}: {e}"))?;
    let mut tx = contained.stdin.take().ok_or("no stdin")?;
    let mut rx = codec::buffered(contained.stdout.take().ok_or("no stdout")?);
    let stderr = contained.stderr.take().ok_or("no stderr")?;
    // ssh's last line is the diagnosis a dead link leaves behind; the pipe must be drained anyway.
    let last_stderr = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    {
        let last_stderr = std::sync::Arc::clone(&last_stderr);
        tokio::spawn(async move {
            let mut lines = sot_protocol::child_lines::ChildLines::new(stderr);
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    if let Ok(mut g) = last_stderr.lock() {
                        *g = Some(line);
                    }
                }
            }
        });
    }
    let result = tokio::select! {
        result = converse(&mut tx, &mut rx, self_host, name) => result,
        // The daemon is shutting down: the signal has already killed the
        // child's tree.
        _ = sig.fired() => return Ok(()),
    };
    // A silent hub leaves ssh up: its last line is stale then, not the diagnosis.
    if result.as_ref().is_err_and(|why| *why != silent()) {
        if let Some(line) = sot_protocol::topology::ssh_bridge::last_stderr_after_failure(&last_stderr).await {
            return Err(format!("{}: {line}", result.unwrap_err()));
        }
    }
    result
}

/// Why a hub silent for `SILENT_PINGS` intervals ended the link.
fn silent() -> String {
    format!("nothing from the hub in {}s", (PING_EVERY * SILENT_PINGS).as_secs())
}

/// Read one frame, owning the reader's borrow and handing it back, so the loop below holds one read across its select:
/// `read_frame` is not cancellation-safe.
async fn read_owned<R: tokio::io::AsyncBufRead + Unpin>(rx: &mut R) -> (&mut R, anyhow::Result<(Frame, Option<Vec<u8>>)>) {
    let read = codec::read_frame(rx).await;
    (rx, read)
}

async fn converse<W, R>(tx: &mut W, rx: &mut R, self_host: &str, name: &str) -> Result<(), String>
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
{
    // The hub reaps the link after 90 s with no frame from it, as it reaps any connection that has pinged, and the
    // link holds its own deadline for the hub (`SILENT_PINGS`). It answers the hub's `comm.file` requests, each filed in
    // a task of its own so one held lock delays no other answer, and ignores every other frame. The select polls the
    // ping tick first, then the read, then the finished filings, then the deadline, so a burst of frames cannot
    // starve the ping that keeps this link alive on the hub.
    let hello = HelloReq {
        name: Some(name.to_string()),
        ..HelloReq::this_process(format!("sotd-hub-link-{}", std::process::id()), sot_protocol::LINK_ROLE, Some(self_host.to_string()))
            .map_err(|err| err.to_string())?
    };
    let e = |what: &'static str| move |err: anyhow::Error| format!("{what}: {err}");
    let payload = serde_json::to_value(hello).map_err(|err| err.to_string())?;
    codec::write_frame(tx, &Frame::req(1, op::HELLO, payload), None).await.map_err(e("hello"))?;
    let silence = PING_EVERY * SILENT_PINGS;
    let mut heard = tokio::time::Instant::now();
    let mut ping = tokio::time::interval_at(heard + PING_EVERY, PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut read = Box::pin(read_owned(rx));
    let mut answered = false;
    let mut next_id = 2u64;
    let mut filings = tokio::task::JoinSet::new();
    loop {
        let frame = tokio::select! {
            biased;
            _ = ping.tick(), if answered => {
                let req = Frame::req(next_id, op::PING, serde_json::json!({}));
                codec::write_frame(tx, &req, None).await.map_err(e("ping"))?;
                next_id += 1;
                continue;
            }
            (rx, got) = &mut read => {
                read.set(read_owned(rx));
                heard = tokio::time::Instant::now();
                got.map_err(e(if answered { "read" } else { "hello reply" }))?.0
            }
            Some(filed) = filings.join_next(), if !filings.is_empty() => {
                let (id, answer) = filed.map_err(|err| format!("comm.file: {err}"))?;
                codec::write_frame(tx, &Frame::res(id, op::COMM_FILE, answer), None).await.map_err(e("comm.file"))?;
                continue;
            }
            () = tokio::time::sleep_until(heard + silence) => return Err(silent()),
        };
        if !answered {
            if frame.kind == Kind::Res && frame.id == 1 {
                if let Some(err) = frame.payload.get("error") {
                    return Err(format!("hello refused: {err}"));
                }
                answered = true;
            }
            continue;
        }
        if frame.kind != Kind::Req || frame.op != op::COMM_FILE {
            continue;
        }
        let (id, self_host) = (frame.id, self_host.to_string());
        filings.spawn(async move { (id, offer_answer(frame.payload, &self_host).await) });
    }
}

/// This box's answer to one offer from the hub (`filer::file_offer`): `ok`, or the refusal `{error, code}`.
async fn offer_answer(payload: serde_json::Value, self_host: &str) -> serde_json::Value {
    let req = match serde_json::from_value::<CommFileReq>(payload) {
        Ok(req) => req,
        Err(err) => return serde_json::json!({ "error": format!("comm.file payload: {err}"), "code": "bad_request" }),
    };
    let to = req.to.clone();
    match crate::comm::mail::filer::file_offer(req, self_host).await {
        Ok(Ok(())) => {
            tracing::info!(%to, "hub link filed");
            serde_json::json!({ "ok": true })
        }
        Ok(Err((code, error))) => serde_json::json!({ "error": error, "code": code }),
        Err(err) => serde_json::json!({ "error": format!("the filing failed: {err}"), "code": "file_failed" }),
    }
}

// The one-time move of the old frontend inbox. Deleted with this file's caller
// after the candidate that ships it.

/// Only a Windows frontend ever read `fe-inbox.jsonl` through a cursor. Elsewhere
/// the frontend wrote it too, but `comm-listen` had already delivered that mail,
/// so moving it would file it a second time.
fn move_fe_inbox() -> Result<(), String> {
    let (Some(dir), Some(home)) = (sot_log::host::state_dir::sot_state_dir(), crate::comm::sot_comm_home()) else {
        return Ok(());
    };
    move_fe_inbox_if(cfg!(windows), &dir, &home).map(|n| {
        if n > 0 {
            tracing::info!(lines = n, "moved unread frontend mail into the inboxes");
        }
    })
}

/// `move_fe_inbox_in` when `on_windows`, else nothing and nothing touched.
fn move_fe_inbox_if(on_windows: bool, dir: &Path, home: &Path) -> Result<usize, String> {
    if !on_windows {
        return Ok(0);
    }
    move_fe_inbox_in(dir, home)
}

/// Moves each unread line of `<dir>/fe-inbox.jsonl` into `<home>/inbox/<to>.jsonl`.
/// The file is renamed to `.moving` first, so only one start does the move; a
/// start that finds `.moving` repeats the appends (a line may then be doubled,
/// never lost), and the last step renames it to `.moved`. A registry that
/// cannot be read leaves everything where it is: with no handles, the rename
/// would drop the mail. Once moved, the frontend's `read/*.fe.cursor` files are
/// deleted: they counted lines of a file that no longer exists.
fn move_fe_inbox_in(dir: &Path, home: &Path) -> Result<usize, String> {
    let (inbox, moving, moved) = (dir.join("fe-inbox.jsonl"), dir.join("fe-inbox.jsonl.moving"), dir.join("fe-inbox.jsonl.moved"));
    if !moving.exists() && !inbox.exists() {
        return Ok(0);
    }
    let handles = registry_handles(home)?;
    if !moving.exists() {
        std::fs::rename(&inbox, &moving).map_err(|e| format!("rename to {}: {e}", moving.display()))?;
    }
    let bytes = std::fs::read(&moving).map_err(|e| format!("read {}: {e}", moving.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    // Whole lines only, as `wc -l` counts them: a torn tail is not a line.
    let lines: Vec<&str> = text.split_inclusive('\n').filter(|l| l.ends_with('\n')).collect();
    let inbox_dir = home.join("inbox");
    std::fs::create_dir_all(&inbox_dir).map_err(|e| format!("create {}: {e}", inbox_dir.display()))?;
    let own = crate::comm::mail::inbox::lock_identity(&inbox_dir);
    let mut moved_lines = 0;
    for h in &handles {
        // `sot_fe_cursor_offset`: a line count; unset, unreadable, non-numeric or past the end is 0.
        let cur = std::fs::read_to_string(home.join("read").join(format!("{h}.fe.cursor")))
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .filter(|c| *c <= lines.len())
            .unwrap_or(0);
        for l in &lines[cur..] {
            // A line that is not an object is skipped, like a torn one; so is an `fe_down`
            // marker, which is addressed to a handle but is not mail.
            let Ok(serde_json::Value::Object(o)) = serde_json::from_str::<serde_json::Value>(l) else {
                continue;
            };
            let s = |k: &str| o.get(k).and_then(|v| v.as_str()).unwrap_or("");
            if s("kind") == "fe_down" || s("to") != h || s("from") == h {
                continue;
            }
            let text = if o.contains_key("msg") { s("msg") } else { s("text") };
            crate::comm::mail::inbox::file_frame(&inbox_dir, s("from"), h, false, text, s("ts"), crate::comm::mail::inbox::inbox_lock_wait(), &own)?;
            moved_lines += 1;
        }
    }
    std::fs::rename(&moving, &moved).map_err(|e| format!("rename to {}: {e}", moved.display()))?;
    if let Ok(rd) = std::fs::read_dir(home.join("read")) {
        for ent in rd.flatten() {
            if ent.file_name().to_string_lossy().ends_with(".fe.cursor") {
                let _ = std::fs::remove_file(ent.path());
            }
        }
    }
    Ok(moved_lines)
}

/// The handles `registry.json` lists with a host: the question `comm.file` asks.
fn registry_handles(home: &Path) -> Result<Vec<String>, String> {
    let path = home.join("registry.json");
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let root: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let agents = root.get("agents").and_then(|a| a.as_object()).ok_or_else(|| format!("{}: no agents object", path.display()))?;
    Ok(agents
        .iter()
        .filter(|(h, e)| crate::paths::valid_name(h) && e.get("host").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()))
        .map(|(h, _)| h.clone())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let descendant = crate::lifecycle::child_signal::tests::Leftover::of_file(bg.clone());
        let stub = dir.path().join("stub-ssh");
        sot_log::test_exec::write_executable(
            &stub,
            format!(
                "#!/bin/sh\necho x >> {}\nsleep 3108 >/dev/null 2>&1 &\necho $! > {}\nexec sleep 30\n",
                counter.display(),
                bg.display()
            ),
        );
        STUB_PROGRAM.set(Some(stub.to_string_lossy().into_owned()));
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", None).unwrap();
        let task = tokio::spawn(async move { hold_link(&recipe, "self", "sotd-self", sig).await });
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
        assert_eq!(move_fe_inbox_in(&dir, &home), Ok(2));
        assert_eq!(msgs(&home, "a"), ["a-unread"]);
        assert_eq!(msgs(&home, "b"), ["b-unread"]);
        assert!(!dir.join("fe-inbox.jsonl").exists() && !dir.join("fe-inbox.jsonl.moving").exists());
        assert!(dir.join("fe-inbox.jsonl.moved").exists());
        assert!(!home.join("read/a.fe.cursor").exists(), "a stale cursor outlived the move");
        // A second start finds nothing to move.
        assert_eq!(move_fe_inbox_in(&dir, &home), Ok(0));
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
        assert!(move_fe_inbox_in(&dir, &home).is_ok());
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
        assert!(move_fe_inbox_in(&dir, &home).is_err());
        assert!(dir.join("fe-inbox.jsonl").exists());
        assert!(!dir.join("fe-inbox.jsonl.moving").exists());
    }

    #[test]
    fn off_windows_the_old_inbox_is_left_untouched() {
        let (_t, dir, home) = setup(REG, &fe_lines());
        let before = std::fs::read(dir.join("fe-inbox.jsonl")).unwrap();
        assert_eq!(move_fe_inbox_if(false, &dir, &home), Ok(0));
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
    async fn answers_to_offers(self_host: &str, handles: &[&str]) -> Vec<(u64, serde_json::Value)> {
        let (link_tx, hub_rx) = tokio::io::duplex(64 * 1024);
        let (mut hub_tx, link_rx) = tokio::io::duplex(64 * 1024);
        let host = self_host.to_string();
        let link = tokio::spawn(async move {
            let (mut tx, mut rx) = (link_tx, codec::buffered(link_rx));
            converse(&mut tx, &mut rx, &host, "sotd-test").await
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

    /// A comm folder for a link under test, which `SOT_COMM_HOME` names while the guards live: `agents` its registry;
    /// with `record`, the inbox lock record that lets this daemon append.
    fn link_home(agents: serde_json::Value, record: bool) -> (tempfile::TempDir, [crate::paths::EnvGuard; 3]) {
        let home = tempfile::tempdir().unwrap();
        let inbox = home.path().join("inbox");
        std::fs::create_dir(&inbox).unwrap();
        std::fs::write(home.path().join("registry.json"), serde_json::json!({ "agents": agents }).to_string()).unwrap();
        if record {
            std::fs::write(home.path().join("inbox-lock-manager"), format!("{}\n", crate::comm::mail::inbox::lock_identity(&inbox))).unwrap();
        }
        let guards = [
            crate::paths::EnvGuard::capture("SOT_COMM_HOME"),
            crate::paths::EnvGuard::capture("SOT_HOSTS"),
            crate::paths::EnvGuard::capture(crate::comm::mail::inbox::INBOX_LOCK_WAIT_ENV),
        ];
        std::env::set_var("SOT_COMM_HOME", home.path());
        std::env::set_var("SOT_HOSTS", home.path().join("no-hosts.toml"));
        std::env::set_var(crate::comm::mail::inbox::INBOX_LOCK_WAIT_ENV, "2");
        (home, guards)
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
        let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let agents = serde_json::json!({
            "stale": { "host": "b", "last_seen": "2026-01-01T00:00:00Z" },
            "here": { "host": "a-guest", "last_seen": "2026-01-01T00:00:00Z" },
        });
        let (_home, _env) = link_home(agents, false);
        let mut answers = runtime().block_on(answers_to_offers("a-guest", &["stale", "nobody", "here"]));
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
        let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let now = crate::comm::registry::registry::iso8601_utc_from_secs(crate::comm::registry::registry::unix_now_secs());
        let agents = serde_json::json!({ "held": { "host": "a", "last_seen": now }, "free": { "host": "a", "last_seen": now } });
        let (home, _env) = link_home(agents, true);
        let held = std::fs::OpenOptions::new().read(true).create(true).append(true).open(home.path().join("inbox/held.lock")).unwrap();
        held.lock().unwrap();
        let answers = runtime().block_on(answers_to_offers("a", &["held", "free"]));
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
            let ended = tokio::time::timeout(Duration::from_secs(1200), converse(&mut tx, &mut rx, "self", "sotd-self")).await;
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
        sot_log::test_exec::write_executable(&stub, script);
        stub.to_string_lossy().into_owned()
    }

    /// ssh's stderr may hold a byte that is not UTF-8 (a far host's banner, a remote's error): a link that fails still
    /// names ssh's last line, with U+FFFD for the byte.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_link_names_an_ssh_line_that_is_not_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("stub-ssh");
        sot_log::test_exec::write_executable(&stub, "#!/bin/sh\nprintf 'Permission denied \\351 (publickey).\\n' >&2\nexit 255\n".to_string());
        STUB_PROGRAM.set(Some(stub.to_string_lossy().into_owned()));
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", None).unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(30), link_once(&recipe, "self", "sotd-self", sig)).await;
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
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", None).unwrap();
        let link = tokio::spawn(async move { link_once(&recipe, "self", "sotd-self", sig).await });
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
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", None).unwrap();
        let task = tokio::spawn(async move { hold_link(&recipe, "self", "sotd-self", sig).await });
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
        assert_eq!(move_fe_inbox_in(&dir, &home), Ok(3));
        assert_eq!(msgs(&home, "a"), ["a-read", "a-unread"]);
    }
}
