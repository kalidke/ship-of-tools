// hub_link.rs — the daemon's own link to the hub (decision 0031 Part 3, B2).
//
// A box whose topology entry is `frontend = true` reaches the hub over one
// `ssh:` stdio child. The daemon holds it for as long as it runs, so mail for
// a session here is filed with the frontend window open or closed. On it the
// hub broadcasts every `agent.message`; this daemon files the ones whose `to`
// its own comm folder lists, through the same function `comm.file` uses
// (`comm::mail::filer::file_comm`), and answers `agent.filed {id}`. Nothing else rides
// the link: only a positive claim exists (`op::AGENT_FILED`).
//
// The topology is read once at start, like the frontend's tunnel set: a box
// that is the hub, or has no `ssh:` endpoint to it, runs no link.

use std::path::Path;
use std::time::{Duration, Instant};

use sot_protocol::{codec, op, Frame, HelloReq, Kind};

use crate::rows::Workspaces;

const BACKOFF_FLOOR: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
/// A connection that lasted this long was a working one: the next wait starts over.
const STABLE: Duration = Duration::from_secs(60);

pub async fn run(workspaces: Workspaces) {
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
    hold_link(&recipe, &self_host, &name, &workspaces, crate::lifecycle::child_signal::process()).await;
}

/// Keep the link up until `sig` fires; it never reconnects after.
async fn hold_link(
    recipe: &sot_protocol::topology::ssh_bridge::SshRecipe,
    self_host: &str,
    name: &str,
    workspaces: &Workspaces,
    sig: &'static crate::lifecycle::child_signal::Signal,
) {
    let mut wait = BACKOFF_FLOOR;
    loop {
        if sig.is_fired() {
            return;
        }
        let began = Instant::now();
        match link_once(recipe, self_host, name, workspaces, sig).await {
            Ok(()) => tracing::info!("hub link closed"),
            Err(e) => tracing::warn!("hub link dropped: {e}"),
        }
        if began.elapsed() >= STABLE {
            wait = BACKOFF_FLOOR;
        }
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = sig.fired() => return,
        }
        wait = (wait * 2).min(BACKOFF_CAP);
    }
}

/// The ssh child's recipe, or `None` when this box has no link to hold: no
/// topology, it is the hub, or its relay endpoint is not an `ssh:` one.
fn recipe_for(self_host: &str) -> Result<Option<sot_protocol::topology::ssh_bridge::SshRecipe>, String> {
    let Some((_, topo)) = sot_protocol::topology::load()? else {
        return Ok(None);
    };
    if self_host.is_empty() || topo.hub == self_host {
        return Ok(None);
    }
    let endpoint = sot_protocol::topology::relay_endpoint(&topo, self_host)?;
    let Some(target) = endpoint.strip_prefix("ssh:") else {
        return Ok(None);
    };
    sot_protocol::topology::ssh_bridge::SshRecipe::new(target, None).map(Some)
}

/// The ssh child. Tests swap the program for a stub, nothing else.
fn spawn_link(recipe: &sot_protocol::topology::ssh_bridge::SshRecipe) -> Result<tokio::process::Child, String> {
    #[cfg(test)]
    if let Some(program) = tests::STUB_PROGRAM.lock().unwrap().as_ref() {
        return tokio::process::Command::new(program)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("spawn {recipe}: {e}"));
    }
    sot_protocol::topology::ssh_bridge::LinkGate::default().spawn_async(recipe).map_err(|e| format!("spawn {recipe}: {e}"))
}

/// One connection, from spawn to the first failure. The child dies with `child`.
async fn link_once(
    recipe: &sot_protocol::topology::ssh_bridge::SshRecipe,
    self_host: &str,
    name: &str,
    workspaces: &Workspaces,
    sig: &'static crate::lifecycle::child_signal::Signal,
) -> Result<(), String> {
    let mut child = spawn_link(recipe)?;
    let _child_guard = sig.guard();
    let mut tx = child.stdin.take().ok_or("no stdin")?;
    let mut rx = codec::buffered(child.stdout.take().ok_or("no stdout")?);
    let stderr = child.stderr.take().ok_or("no stderr")?;
    // ssh's last line is the diagnosis a dead link leaves behind; the pipe must be drained anyway.
    let last_stderr = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    {
        let last_stderr = std::sync::Arc::clone(&last_stderr);
        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut lines = tokio::io::BufReader::new(stderr).lines();
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
        result = converse(&mut tx, &mut rx, self_host, name, workspaces) => result,
        // The daemon is shutting down: nothing kills this child at
        // `process::exit`, so it is killed here.
        _ = sig.fired() => {
            let _ = child.kill().await;
            return Ok(());
        }
    };
    if result.is_err() {
        if let Some(line) = sot_protocol::topology::ssh_bridge::last_stderr_after_failure(&last_stderr).await {
            return Err(format!("{}: {line}", result.unwrap_err()));
        }
    }
    result
}

async fn converse<W, R>(tx: &mut W, rx: &mut R, self_host: &str, name: &str, workspaces: &Workspaces) -> Result<(), String>
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
{
    // One-shot roles are never read-deadline reaped; the link ignores every event but one.
    let hello = HelloReq {
        name: Some(name.to_string()),
        ..HelloReq::this_process(format!("sotd-hub-link-{}", std::process::id()), "cli", Some(self_host.to_string()))
            .map_err(|err| err.to_string())?
    };
    let e = |what: &'static str| move |err: anyhow::Error| format!("{what}: {err}");
    let payload = serde_json::to_value(hello).map_err(|err| err.to_string())?;
    codec::write_frame(tx, &Frame::req(1, op::HELLO, payload), None).await.map_err(e("hello"))?;
    loop {
        let (frame, _) = codec::read_frame(rx).await.map_err(e("hello reply"))?;
        if frame.kind == Kind::Res && frame.id == 1 {
            if let Some(err) = frame.payload.get("error") {
                return Err(format!("hello refused: {err}"));
            }
            break;
        }
    }
    let mut next_id = 2u64;
    loop {
        let (frame, _) = codec::read_frame(rx).await.map_err(e("read"))?;
        if frame.kind != Kind::Evt || frame.op != op::AGENT_MESSAGE {
            continue;
        }
        let field = |k: &str| frame.payload.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let (id, from, to, text) = (field("id"), field("from"), field("to"), field("text"));
        // No id: nothing to claim. No `to`: a broadcast, never mail.
        if id.is_empty() || to.is_empty() {
            continue;
        }
        let req = hub_frame_req(from, to.clone(), text);
        match crate::comm::mail::filer::file_comm(req, workspaces).await {
            Ok(Ok(())) => {
                tracing::info!(%to, %id, "hub link filed");
                let filed = serde_json::json!({ "id": id });
                codec::write_frame(tx, &Frame::req(next_id, op::AGENT_FILED, filed), None).await.map_err(e("agent.filed"))?;
                next_id += 1;
            }
            // Every linked daemon sees every message; a handle this box does not hold is the usual case.
            Ok(Err((code, _))) if code == "not_here" => tracing::debug!(%to, "hub link: not here"),
            Ok(Err((code, error))) => tracing::info!(%to, code, %error, "hub link refused"),
            Err(err) => tracing::warn!(%to, "hub link: filing failed: {err}"),
        }
    }
}

/// The filing request for a frame the hub sent. `forwarded: true` because it
/// came from the hub: a box whose comm folder is not on its own disk refuses it
/// rather than forwarding it back to the hub over ssh.
fn hub_frame_req(from: String, to: String, text: String) -> sot_protocol::CommFileReq {
    sot_protocol::CommFileReq { from, to, text, broadcast: false, forwarded: true }
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

    pub(super) static STUB_PROGRAM: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

    /// The shutdown signal kills the ssh child; the loop neither reconnects nor leaves a guard counted.
    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_kills_the_link_child_and_never_reconnects() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("spawns");
        let stub = dir.path().join("stub-ssh");
        std::fs::write(&stub, format!("#!/bin/sh\necho x >> {}\nexec sleep 30\n", counter.display())).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        *STUB_PROGRAM.lock().unwrap() = Some(stub.to_string_lossy().into_owned());
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", None).unwrap();
        let workspaces = Workspaces::new();
        let task = tokio::spawn(async move { hold_link(&recipe, "self", "sotd-self", &workspaces, sig).await });
        let began = Instant::now();
        while sig.live() == 0 {
            assert!(began.elapsed() < Duration::from_secs(5), "the stub child never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        sig.fire();
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("the link loop outlived the shutdown")
            .expect("link task");
        assert_eq!(sig.live(), 0);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(std::fs::read_to_string(&counter).unwrap().lines().count(), 1, "reconnected after the fire");
        *STUB_PROGRAM.lock().unwrap() = None;
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

    #[test]
    fn a_hub_frame_is_filed_as_forwarded() {
        assert!(hub_frame_req("x".into(), "a".into(), "t".into()).forwarded);
    }

    #[test]
    fn a_cursor_past_the_end_reads_from_the_start() {
        let (_t, dir, home) = setup(REG, &fe_lines());
        std::fs::write(home.join("read/a.fe.cursor"), "99").unwrap();
        assert_eq!(move_fe_inbox_in(&dir, &home), Ok(3));
        assert_eq!(msgs(&home, "a"), ["a-read", "a-unread"]);
    }
}
