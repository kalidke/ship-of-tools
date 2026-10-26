//! The page channel. The server supplies facts and a command bus; pages never reaches into the row registry or roster.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use sot_protocol::FeCommandEvt;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, OwnedSemaphorePermit, Semaphore};

use crate::clients::ActiveFrontend;
use crate::pages::http::{read_body, write_json, Request};
use super::{Capability, links::Site};

const MAX_IN_FLIGHT: usize = 4;
const HANDLER_BOUND: Duration = Duration::from_secs(5);

/// The queue's view of an answer named in a question line.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PageOption {
    pub key: String,
    pub text: String,
}

/// The queue's view of a session. Empty strings mean that the registry has no value.
#[derive(Serialize)]
pub(crate) struct PageRow {
    pub handle: String,
    pub host: String,
    pub repo: String,
    pub row: String,
    pub state: String,
    pub summary: String,
    pub question: String,
    pub options: Vec<PageOption>,
    pub recommend: String,
    pub page: String,
    pub waiting: String,
    pub status_at: String,
    pub last_seen: String,
}

/// A handle resolved to a row of the serving daemon, with its freshly read question's page.
pub(crate) struct RowRef {
    pub workspace_id: String,
    pub project_root: PathBuf,
    pub page: String,
}

/// The four capabilities the server gives the page listeners at boot.
pub(crate) struct PageVerbs {
    /// Read this daemon's sessions. Blocks: called on a blocking thread.
    pub states: Arc<dyn Fn() -> Result<Vec<PageRow>> + Send + Sync>,
    /// Resolve a local row, or refuse with words naming a foreign daemon when known. Blocks.
    pub row_of: Arc<dyn Fn(&str) -> std::result::Result<RowRef, String> + Send + Sync>,
    /// The page owner's newest named, live frontend connection.
    pub window: Arc<dyn Fn(u64) -> Option<ActiveFrontend> + Send + Sync>,
    pub fe: broadcast::Sender<FeCommandEvt>,
    bodies: Arc<Semaphore>,
    states_in_flight: Arc<Semaphore>,
    actions_in_flight: Arc<Semaphore>,
}

impl PageVerbs {
    pub(crate) fn new(
        states: Arc<dyn Fn() -> Result<Vec<PageRow>> + Send + Sync>,
        row_of: Arc<dyn Fn(&str) -> std::result::Result<RowRef, String> + Send + Sync>,
        window: Arc<dyn Fn(u64) -> Option<ActiveFrontend> + Send + Sync>,
        fe: broadcast::Sender<FeCommandEvt>,
    ) -> Self {
        Self {
            states, row_of, window, fe,
            bodies: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
            states_in_flight: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
            actions_in_flight: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        }
    }

    #[cfg(test)]
    pub(super) fn empty() -> Arc<Self> {
        Arc::new(Self::new(Arc::new(|| Ok(Vec::new())), Arc::new(|_| Err("no row".into())),
            Arc::new(|_| None), broadcast::channel(16).0))
    }

    /// Both the site's capability and its Origin are checked before a body can dispatch a verb.
    pub(super) async fn serve(
        &self,
        stream: &mut TcpStream,
        buf: &mut Vec<u8>,
        request: &Request,
        capability: &Capability,
        set_cookie: Option<&str>,
    ) -> Result<bool> {
        let own_origin = request.host.as_ref().map(|host| format!("http://{host}"));
        if own_origin.is_none() || request.origin != own_origin {
            return refuse(stream, "403 Forbidden", "forbidden", "the request must come from this site's origin", false).await;
        }
        let Ok(reader) = self.bodies.clone().try_acquire_owned() else {
            return refuse(stream, "503 Service Unavailable", "server_busy", "all four page body readers are busy; try again", false).await;
        };
        let bytes = match read_body(stream, buf, request).await? {
            Ok(bytes) => bytes,
            Err(error) => return refuse(stream, error.status, error.code, error.words, false).await,
        };
        drop(reader);
        let body = serde_json::from_slice::<Value>(&bytes).ok().filter(Value::is_object);
        let Some(body) = body else {
            return bad_request(stream, request.keep).await;
        };
        // A blocked action must not spend polling capacity.
        let budget = if body.get("verb").and_then(Value::as_str) == Some("states") {
            &self.states_in_flight
        } else {
            &self.actions_in_flight
        };
        let Ok(permit) = budget.clone().try_acquire_owned() else {
            return refuse(stream, "503 Service Unavailable", "server_busy", "all four handlers for this verb class are busy; try again", request.keep).await;
        };
        let permit = Arc::new(permit);
        let (status, answer) = match tokio::time::timeout(HANDLER_BOUND, self.dispatch(capability, &body, permit.clone())).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(cause)) => {
                tracing::warn!(error = %cause, "page verb failed");
                ("500 Internal Server Error", error("handler_error", "the page request could not be completed; check the daemon log"))
            }
            Err(_) => ("503 Service Unavailable", error("handler_timeout", "the page handler exceeded its 5 s deadline; try again")),
        };
        write_json(stream, status, &answer, set_cookie, request.keep).await
    }

    async fn dispatch(&self, capability: &Capability, body: &Value, permit: Arc<OwnedSemaphorePermit>) -> Result<(&'static str, Value)> {
        if !capability.live() { return Ok(grant_gone()); }
        let verb = body.get("verb").and_then(Value::as_str).unwrap_or("");
        if verb == "states" {
            let states = self.states.clone();
            let rows = tokio::task::spawn_blocking(move || { let _permit = permit; states() }).await??;
            if !capability.live() { return Ok(grant_gone()); }
            return Ok(("200 OK", json!({"ok": true, "rows": rows})));
        }
        // No HTTP verb confirms or types a reply.
        if !matches!(verb, "goto" | "open" | "reply") {
            return Ok(bad_request_value());
        }
        let Some(handle) = body.get("handle").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return Ok(bad_request_value());
        };
        let row_of = self.row_of.clone();
        let lookup_handle = handle.to_string();
        let work = permit.clone();
        let row = match tokio::task::spawn_blocking(move || { let _permit = work; row_of(&lookup_handle) }).await? {
            Ok(row) => row,
            Err(words) => return Ok(("409 Conflict", error("not_a_row", &words))),
        };
        let mut page = None;
        let (cmd, mut args) = if verb == "goto" {
            ("goto_workspace", json!({"workspace": row.workspace_id}))
        } else if verb == "reply" {
            let Some(text) = body.get("text").and_then(Value::as_str) else {
                return Ok(bad_request_value());
            };
            let line = match sot_protocol::page_reply::line(text) {
                Ok(line) => line,
                Err(reason) => return Ok(("400 Bad Request", error("bad_text", &reason.to_string()))),
            };
            ("page_reply", json!({"workspace": row.workspace_id, "handle": handle, "line": line}))
        } else {
            if row.page.is_empty() {
                return Ok(("409 Conflict", error("no_page", "the row's question names no page")));
            }
            let workspace = row.workspace_id;
            let work = permit.clone();
            let prepared = tokio::task::spawn_blocking(move || {
                let _permit = work;
                page_inside(&row.project_root, &row.page)
            }).await?;
            let Some(prepared) = prepared else {
                return Ok(("403 Forbidden", error("page_outside", "the question's page must resolve under the row's project folder")));
            };
            page = Some(prepared);
            ("docs", json!({"workspace": workspace}))
        };
        if !capability.live() { return Ok(grant_gone()); }
        let Some(window) = (self.window)(capability.owner()) else {
            return Ok(window_gone());
        };
        if let Some((site, relative)) = page {
            let Some(path) = super::page_grant_path(capability.owner(), site, &relative) else {
                anyhow::bail!("could not mint a secure page grant");
            };
            args["path"] = path.into();
        }
        let evt = FeCommandEvt {
            v: 1, cmd: cmd.into(), args,
            target: Some(window.handle), target_serial: Some(window.serial),
        };
        if self.fe.send(evt).is_err() {
            return Ok(window_gone());
        }
        Ok(("202 Accepted", json!({"ok": true})))
    }
}

fn page_inside(root: &Path, page: &str) -> Option<(Site, String)> {
    let page = Path::new(page);
    if !page.is_absolute() { return None; }
    let site = Site::decision(root)?;
    if crate::pages::home_folder().is_some_and(|home| home.starts_with(&site.content_root)) { return None; }
    let page = page.canonicalize().ok()?;
    let relative = page.strip_prefix(&site.content_root).ok()?.to_str()?.replace(std::path::MAIN_SEPARATOR, "/");
    // No git link authority is attached to a decision page. The same checks run again when its bytes are served.
    super::links::resolve_and_open(&site, &relative).ok()?;
    Some((site, relative))
}

fn grant_gone() -> (&'static str, Value) {
    ("403 Forbidden", error("forbidden", "the page's grant has ended; reopen the page"))
}

fn error(code: &str, words: &str) -> Value { json!({"error": words, "code": code}) }

fn window_gone() -> (&'static str, Value) {
    ("503 Service Unavailable", error("window_gone", "the window that opened this page is away"))
}

fn bad_request_value() -> (&'static str, Value) {
    ("400 Bad Request", error("bad_request", "expected states, goto, open or reply with the required fields"))
}

async fn bad_request(stream: &mut TcpStream, keep: bool) -> Result<bool> {
    let (status, body) = bad_request_value();
    write_json(stream, status, &body, None, keep).await
}

pub(super) async fn refuse(stream: &mut TcpStream, status: &str, code: &str, words: &str, keep: bool) -> Result<bool> {
    write_json(stream, status, &error(code, words), None, keep).await
}
