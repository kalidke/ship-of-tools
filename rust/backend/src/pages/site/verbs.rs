//! The page channel. The server supplies facts and a command bus; pages never reaches into the row registry or roster.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use sot_protocol::FeCommandEvt;
use tokio::net::TcpStream;
use tokio::sync::broadcast;

use crate::clients::ActiveFrontend;
use crate::pages::http::{read_body, write_json, Request};

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
    pub states: Arc<dyn Fn() -> Vec<PageRow> + Send + Sync>,
    /// Resolve a local row, or refuse with words naming a foreign daemon when known. Blocks.
    pub row_of: Arc<dyn Fn(&str) -> std::result::Result<RowRef, String> + Send + Sync>,
    /// The page owner's newest named, live frontend connection.
    pub window: Arc<dyn Fn(u64) -> Option<ActiveFrontend> + Send + Sync>,
    pub fe: broadcast::Sender<FeCommandEvt>,
}

impl PageVerbs {
    #[cfg(test)]
    pub(super) fn empty() -> Arc<Self> {
        Arc::new(Self {
            states: Arc::new(Vec::new),
            row_of: Arc::new(|_| Err("no row".into())),
            window: Arc::new(|_| None),
            fe: broadcast::channel(16).0,
        })
    }

    /// Both the site's capability and its Origin are checked before a body can dispatch a verb.
    pub(super) async fn serve(
        &self,
        stream: &mut TcpStream,
        buf: &mut Vec<u8>,
        request: &Request,
        owner: u64,
        set_cookie: Option<&str>,
    ) -> Result<bool> {
        let own_origin = request.host.as_ref().map(|host| format!("http://{host}"));
        if own_origin.is_none() || request.origin != own_origin {
            return refuse(stream, "403 Forbidden", "forbidden", "the request must come from this site's origin", false).await;
        }
        let bytes = match read_body(stream, buf, request).await? {
            Ok(bytes) => bytes,
            Err(error) => return refuse(stream, error.status, error.code, error.words, false).await,
        };
        let body = serde_json::from_slice::<Value>(&bytes).ok().filter(Value::is_object);
        let Some(body) = body else {
            return bad_request(stream, request.keep).await;
        };
        let (status, answer) = match self.dispatch(owner, &body).await {
            Ok(answer) => answer,
            Err(cause) => {
                tracing::warn!(error = %cause, "page verb failed");
                ("500 Internal Server Error", error("handler_error", "the page request could not be completed; check the daemon log"))
            }
        };
        write_json(stream, status, &answer, set_cookie, request.keep).await
    }

    async fn dispatch(&self, owner: u64, body: &Value) -> Result<(&'static str, Value)> {
        let verb = body.get("verb").and_then(Value::as_str).unwrap_or("");
        if verb == "states" {
            let states = self.states.clone();
            let rows = tokio::task::spawn_blocking(move || states()).await?;
            return Ok(("200 OK", json!({"ok": true, "rows": rows})));
        }
        // Landing 2a adds reply here. No verb confirms or types a reply.
        if !matches!(verb, "goto" | "open") {
            return Ok(bad_request_value());
        }
        let Some(handle) = body.get("handle").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return Ok(bad_request_value());
        };
        let row_of = self.row_of.clone();
        let handle = handle.to_string();
        let row = match tokio::task::spawn_blocking(move || row_of(&handle)).await? {
            Ok(row) => row,
            Err(words) => return Ok(("409 Conflict", error("not_a_row", &words))),
        };
        let (cmd, args) = if verb == "goto" {
            ("goto_workspace", json!({"workspace": row.workspace_id}))
        } else {
            if row.page.is_empty() {
                return Ok(("409 Conflict", error("no_page", "the row's question names no page")));
            }
            let workspace = row.workspace_id;
            let path = tokio::task::spawn_blocking(move || page_inside(&row.project_root, &row.page)).await?;
            let Some(path) = path else {
                return Ok(("403 Forbidden", error("page_outside", "the question's page must resolve under the row's project folder")));
            };
            ("docs", json!({"workspace": workspace, "path": path}))
        };
        let Some(window) = (self.window)(owner) else {
            return Ok(window_gone());
        };
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

fn page_inside(root: &Path, page: &str) -> Option<PathBuf> {
    let page = Path::new(page);
    if !page.is_absolute() { return None; }
    let root = root.canonicalize().ok()?;
    let page = page.canonicalize().ok()?;
    page.starts_with(root).then_some(page)
}

fn error(code: &str, words: &str) -> Value { json!({"error": words, "code": code}) }

fn window_gone() -> (&'static str, Value) {
    ("503 Service Unavailable", error("window_gone", "the window that opened this page is away"))
}

fn bad_request_value() -> (&'static str, Value) {
    ("400 Bad Request", error("bad_request", "expected states, goto or open with the required fields"))
}

async fn bad_request(stream: &mut TcpStream, keep: bool) -> Result<bool> {
    let (status, body) = bad_request_value();
    write_json(stream, status, &body, None, keep).await
}

pub(super) async fn refuse(stream: &mut TcpStream, status: &str, code: &str, words: &str, keep: bool) -> Result<bool> {
    write_json(stream, status, &error(code, words), None, keep).await
}
