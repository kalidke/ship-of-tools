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
    // The daemon's entry for the link: everything below reads this value, not the process.
    let env = crate::comm::Env::process();
    // Before the first connection: a failed move leaves the old inbox where it was.
    let moving = env.clone();
    if let Err(e) = tokio::task::spawn_blocking(move || move_fe_inbox(&moving)).await.unwrap_or_else(|e| Err(e.to_string())) {
        tracing::warn!("hub link: the old frontend inbox was not moved: {e}");
    }
    let self_host = crate::comm::mail::filer::comm_self_host_in(&env);
    let recipe = match recipe_for(&env, &self_host) {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!("hub link not started: {e}");
            return;
        }
    };
    let name = format!("sotd-{self_host}");
    tracing::info!(%recipe, %name, "hub link starting");
    hold_link(&env, &recipe, &self_host, &name, crate::lifecycle::child_signal::process()).await;
}

/// Keep the link up until `sig` fires; it never reconnects after.
async fn hold_link(
    env: &crate::comm::Env,
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
        match link_once(env, recipe, self_host, name, sig).await {
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
fn recipe_for(env: &crate::comm::Env, self_host: &str) -> Result<Option<sot_protocol::topology::ssh_bridge::SshRecipe>, String> {
    let Some((_, topo)) = sot_protocol::topology::load_in(env)? else {
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
pub(crate) fn unlinked(env: &crate::comm::Env, attached: &[String]) -> Vec<String> {
    let self_host = crate::comm::mail::filer::comm_self_host_in(env);
    let Ok(Some((_, topo))) = sot_protocol::topology::load_in(env) else {
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
    env: &crate::comm::Env,
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
        result = converse(env, &mut tx, &mut rx, self_host, name) => result,
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

async fn converse<W, R>(env: &crate::comm::Env, tx: &mut W, rx: &mut R, self_host: &str, name: &str) -> Result<(), String>
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
        let (id, self_host, env) = (frame.id, self_host.to_string(), env.clone());
        filings.spawn(async move { (id, offer_answer(env, frame.payload, &self_host).await) });
    }
}

/// This box's answer to one offer from the hub (`filer::file_offer`): `ok`, or the refusal `{error, code}`.
async fn offer_answer(env: crate::comm::Env, payload: serde_json::Value, self_host: &str) -> serde_json::Value {
    let req = match serde_json::from_value::<CommFileReq>(payload) {
        Ok(req) => req,
        Err(err) => return serde_json::json!({ "error": format!("comm.file payload: {err}"), "code": "bad_request" }),
    };
    let to = req.to.clone();
    match crate::comm::mail::filer::file_offer(env, req, self_host).await {
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
fn move_fe_inbox(env: &crate::comm::Env) -> Result<(), String> {
    let (Some(dir), Some(home)) = (sot_log::host::state_dir::sot_state_dir_in(env), crate::comm::sot_comm_home_in(env)) else {
        return Ok(());
    };
    move_fe_inbox_if(env, cfg!(windows), &dir, &home).map(|n| {
        if n > 0 {
            tracing::info!(lines = n, "moved unread frontend mail into the inboxes");
        }
    })
}

/// `move_fe_inbox_in` when `on_windows`, else nothing and nothing touched.
fn move_fe_inbox_if(env: &crate::comm::Env, on_windows: bool, dir: &Path, home: &Path) -> Result<usize, String> {
    if !on_windows {
        return Ok(0);
    }
    move_fe_inbox_in(env, dir, home)
}

/// Moves each unread line of `<dir>/fe-inbox.jsonl` into `<home>/inbox/<to>.jsonl`.
/// The file is renamed to `.moving` first, so only one start does the move; a
/// start that finds `.moving` repeats the appends (a line may then be doubled,
/// never lost), and the last step renames it to `.moved`. A registry that
/// cannot be read leaves everything where it is: with no handles, the rename
/// would drop the mail. Once moved, the frontend's `read/*.fe.cursor` files are
/// deleted: they counted lines of a file that no longer exists.
fn move_fe_inbox_in(env: &crate::comm::Env, dir: &Path, home: &Path) -> Result<usize, String> {
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
            crate::comm::mail::inbox::file_frame(&inbox_dir, s("from"), h, false, text, s("ts"), crate::comm::mail::inbox::inbox_lock_wait(env), &own)?;
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
#[path = "hub_link_tests.rs"]
mod tests;
