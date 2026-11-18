//! Several Slack workspaces in one fake, each answering by the request's bearer: every workspace has its own app, with
//! its bot, user and app-level tokens, its channels and its socket. `auth.test` names each bearer's workspace and its
//! name, `bots.info` the bot's app; `apps.connections.open` under a workspace's app token answers a URL whose ticket names
//! that workspace, and the upgrade's ticket picks its socket, whose hello and envelopes name its app. A bearer that is no
//! workspace's, or a bot token `revoke` was called for, is refused with Slack's codes. The tokens are built at run time
//! from the workspace's id and a fixed body, so no token-shaped literal is written.
//!
//! The fake is the witness that no request crosses workspaces: every request whose bearer is one workspace's token and
//! whose channel or user is another workspace's own (a channel listed for it alone, its bot user) is recorded
//! (`crossed`), and a `Teams` dropped holding one fails its test, unless the test said it crosses on purpose
//! (`expect_crossing`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::{FakeSlack, Identity, Reply, Seen};

/// One listed channel: its id, its name, whether it is private, whether the workspace's app is in it.
pub type TeamChannel = (&'static str, &'static str, bool, bool);

struct Team {
    id: &'static str,
    name: &'static str,
    channels: Mutex<Vec<TeamChannel>>,
    revoked: AtomicBool,
    feed: mpsc::UnboundedSender<Value>,
    rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Value>>>,
}

/// The token bodies, the same in every workspace (the id tells them apart).
const BODIES: [&str; 3] = ["9b8c7d6e", "8c7d6e5f", "7d6e5f4a"];

/// A token of `kind` (`xoxb`, `xoxp`, `xapp`) for workspace `id`.
fn token(kind: &str, id: &str) -> String {
    let body = match kind {
        "xoxb" => BODIES[0],
        "xoxp" => BODIES[1],
        _ => BODIES[2],
    };
    [kind, "-", id, "-", body].concat()
}

/// An id of the workspace's own, under `prefix` (`A` for its app, `B` for its bot): the workspace's id with its first
/// letter replaced.
fn own(prefix: &str, id: &str) -> String {
    [prefix, &id[1..]].concat()
}

pub struct Teams {
    fake: FakeSlack,
    teams: Arc<Vec<Team>>,
    crossed: Arc<Mutex<Vec<String>>>,
    crossing: AtomicBool,
}

impl Teams {
    /// A fake answering each `(id, name)` workspace.
    pub async fn start(workspaces: &[(&'static str, &'static str)]) -> Teams {
        let teams: Vec<Team> = workspaces
            .iter()
            .map(|&(id, name)| {
                let (feed, rx) = mpsc::unbounded_channel();
                Team { id, name, channels: Mutex::new(Vec::new()), revoked: AtomicBool::new(false), feed, rx: Arc::new(tokio::sync::Mutex::new(rx)) }
            })
            .collect();
        let teams = Arc::new(teams);
        let crossed = Arc::new(Mutex::new(Vec::new()));
        let (held, seen_crossed) = (teams.clone(), crossed.clone());
        let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
            if let Some(what) = crossing(&held, seen) {
                seen_crossed.lock().unwrap().push(what);
            }
            answer(&held, seen, port)
        })
        .await;
        Teams { fake, teams, crossed, crossing: AtomicBool::new(false) }
    }

    /// Each request so far that carried one workspace's token for another workspace's channel or user, as
    /// `<bearer's workspace> <kind> <method> <the other's id>`.
    pub fn crossed(&self) -> Vec<String> {
        self.crossed.lock().unwrap().clone()
    }

    /// The test sends a crossing request on purpose (a channel of another workspace, to be refused): dropping the fake
    /// then fails nothing.
    pub fn expect_crossing(&self) {
        self.crossing.store(true, Ordering::SeqCst);
    }

    pub fn fake(&self) -> &FakeSlack {
        &self.fake
    }

    pub fn bot(&self, id: &str) -> String {
        token("xoxb", id)
    }

    pub fn user(&self, id: &str) -> String {
        token("xoxp", id)
    }

    pub fn app_token(&self, id: &str) -> String {
        token("xapp", id)
    }

    /// The workspace's app id, which its hello and envelopes name.
    pub fn app(&self, id: &str) -> String {
        own("A", id)
    }

    /// The bodies every token carries, for a test that no output holds one.
    pub fn bodies(&self) -> [&'static str; 3] {
        BODIES
    }

    fn team(&self, id: &str) -> &Team {
        self.teams.iter().find(|t| t.id == id).expect("a workspace of this fake")
    }

    pub fn channels(&self, id: &str, channels: &[TeamChannel]) {
        self.team(id).channels.lock().unwrap().extend_from_slice(channels);
    }

    /// From now on the workspace's bot token is refused `token_revoked` on every call.
    pub fn revoke(&self, id: &str) {
        self.team(id).revoked.store(true, Ordering::SeqCst);
    }

    /// Sends an envelope down the workspace's socket (the one open now, or the next).
    pub fn feed(&self, id: &str, envelope: Value) {
        self.team(id).feed.send(envelope).expect("the fake holds its feed");
    }

    /// A member's new message in `channel` of workspace `id`, as its socket delivers it, numbered `n`.
    pub fn message(&self, id: &str, channel: &str, n: u32, ts: &str) -> Value {
        json!({
            "type": "events_api",
            "envelope_id": format!("env-{id}-{n}"),
            "payload": {"team_id": id, "api_app_id": self.app(id), "event": {"type": "message", "channel": channel, "user": "U0MEMBER", "ts": ts, "text": "hello"}},
        })
    }

    /// The requests made under workspace `id`'s token of `kind` (`xoxb`, `xoxp`, `xapp`) to a path ending `method`.
    pub fn calls(&self, id: &str, kind: &str, method: &str) -> Vec<Seen> {
        let bearer = format!("Bearer {}", token(kind, id));
        self.fake.seen().into_iter().filter(|s| s.authorization.as_deref() == Some(bearer.as_str()) && s.path().ends_with(method)).collect()
    }
}

impl Drop for Teams {
    fn drop(&mut self) {
        let crossed = self.crossed();
        if !std::thread::panicking() && !self.crossing.load(Ordering::SeqCst) {
            assert!(crossed.is_empty(), "requests crossed workspaces: {crossed:?}");
        }
    }
}

/// A request under one workspace's token naming another's own channel (listed for it and not for the bearer's) or its
/// bot user: `<bearer's workspace> <kind> <method> <id>`.
fn crossing(teams: &[Team], seen: &Seen) -> Option<String> {
    let (team, kind) = bearer(teams, seen)?;
    let named = |key: &str| seen.param(key).or_else(|| seen.json().and_then(|j| j[key].as_str().map(str::to_string)));
    let has = |t: &Team, c: &str| t.channels.lock().unwrap().iter().any(|x| x.0 == c);
    let others = || teams.iter().filter(|t| t.id != team.id);
    let method = seen.path().rsplit('/').next().unwrap_or_default().to_string();
    if let Some(c) = named("channel").filter(|c| !has(team, c) && others().any(|t| has(t, c))) {
        return Some(format!("{} {kind} {method} {c}", team.id));
    }
    let u = named("user").filter(|u| others().any(|t| *u == own("U", t.id)))?;
    Some(format!("{} {kind} {method} {u}", team.id))
}

/// The workspace and the kind of a request's bearer, when it is one of the fake's tokens.
fn bearer<'a>(teams: &'a [Team], seen: &Seen) -> Option<(&'a Team, &'static str)> {
    let token = seen.authorization.as_deref()?.strip_prefix("Bearer ")?;
    teams.iter().find_map(|t| ["xoxb", "xoxp", "xapp"].into_iter().find(|k| self::token(k, t.id) == token).map(|k| (t, k)))
}

fn refusal(code: &str) -> Reply {
    Reply::json(&json!({"ok": false, "error": code}))
}

fn channel_json((id, name, private, member): TeamChannel) -> Value {
    json!({"id": id, "name": name, "is_private": private, "is_member": member})
}

fn answer(teams: &Arc<Vec<Team>>, seen: &Seen, port: u16) -> Reply {
    let path = seen.path();
    if seen.upgrade {
        let Some(team) = seen.param("ticket").and_then(|id| teams.iter().position(|t| t.id == id)) else { return Reply::Drop };
        let teams = teams.clone();
        return Reply::socket(move |mut peer| {
            let teams = teams.clone();
            async move {
                let t = &teams[team];
                peer.send(&json!({"type": "hello", "num_connections": 1, "connection_info": {"app_id": own("A", t.id)}})).await;
                // A socket that goes while another holds the feed (a connect's probe beside the listener's) never takes it.
                let mut rx = tokio::select! {
                    rx = t.rx.lock() => rx,
                    () = peer.drain() => return,
                };
                // Until the client goes, so a restarted listener's socket takes the feed.
                loop {
                    let envelope = tokio::select! {
                        e = rx.recv() => e,
                        () = peer.drain() => None,
                    };
                    let Some(envelope) = envelope else { break };
                    peer.send(&envelope).await;
                    let _ = peer.recv(std::time::Duration::from_secs(10)).await;
                }
            }
        });
    }
    let Some((team, kind)) = bearer(teams, seen) else { return refusal("invalid_auth") };
    if kind == "xoxb" && team.revoked.load(Ordering::SeqCst) {
        return refusal("token_revoked");
    }
    let wanted = seen.param("channel").or_else(|| seen.json().and_then(|j| j["channel"].as_str().map(str::to_string))).unwrap_or_default();
    let known = || team.channels.lock().unwrap().iter().copied().find(|c| c.0 == wanted);
    match (kind, path.rsplit('/').next().unwrap_or_default()) {
        ("xoxb", "auth.test") => Reply::json(&json!({"ok": true, "team_id": team.id, "team": team.name, "user_id": own("U", team.id), "bot_id": own("B", team.id)})),
        ("xoxp", "auth.test") => Reply::json(&json!({"ok": true, "team_id": team.id, "team": team.name, "user_id": "U0PERSON"})),
        ("xoxb", "bots.info") => Reply::json(&json!({"ok": true, "bot": {"id": own("B", team.id), "app_id": own("A", team.id)}})),
        ("xapp", "apps.connections.open") => {
            Reply::json(&json!({"ok": true, "url": format!("wss://wss-primary.slack.com:{port}/link/?ticket={}", team.id)}))
        }
        ("xoxb", "conversations.list") => {
            let listed: Vec<Value> = team.channels.lock().unwrap().iter().filter(|c| !c.2 || c.3).copied().map(channel_json).collect();
            Reply::json(&json!({"ok": true, "channels": listed, "response_metadata": {"next_cursor": ""}}))
        }
        ("xoxb", "conversations.info") => match known() {
            Some(c) => Reply::json(&json!({"ok": true, "channel": channel_json(c)})),
            None => refusal("channel_not_found"),
        },
        ("xoxb", "conversations.join") => {
            team.channels.lock().unwrap().iter_mut().filter(|c| c.0 == wanted).for_each(|c| c.3 = true);
            Reply::json(&json!({"ok": true}))
        }
        ("xoxb", "conversations.history" | "conversations.replies") => match known() {
            Some(_) => Reply::json(&json!({"ok": true, "messages": [], "has_more": false})),
            None => refusal("channel_not_found"),
        },
        ("xoxb" | "xoxp", "chat.postMessage") => match known() {
            Some(_) => Reply::json(&json!({"ok": true, "ts": "1700000999.000100"})),
            None => refusal("channel_not_found"),
        },
        ("xoxb", "users.info") => Reply::json(&json!({"ok": true, "user": {"profile": {"display_name": "Name"}}})),
        _ => refusal("unknown_method"),
    }
}
