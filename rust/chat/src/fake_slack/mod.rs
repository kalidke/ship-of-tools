//! The scripted fake Slack every test in this crate runs against: an HTTPS server on loopback over real TLS,
//! hand-written against `tokio-rustls` (no HTTP framework), that records what it receives and answers as a test's
//! script says. It reaches no network and needs no credentials. A request that asks to upgrade may be answered with
//! `Reply::socket`: the selected library's server handshake, then a script that drives the websocket (`ws.rs`).
//!
//! Test-only certificates, fixed so no run generates one. They were made once with OpenSSL 3.0 (`openssl ca` with
//! `-startdate 20250101000000Z`, so a host whose clock is behind still accepts them, and `-enddate 21261013000000Z`),
//! ECDSA P-256: `ca.pem`, a CA (basicConstraints CA:TRUE, pathlen 0) whose key was discarded after it signed
//! `server.pem` (serverAuth, subject alternative names for the logical hosts in `HOSTS` except `nosan.slack.com`); and
//! `rogue.pem`, a self-signed certificate for `slack.com`, `files.slack.com`, `api.slack.com` and
//! `wss-primary.slack.com` that no client trusts. The two `.key` files are test keys with no use outside this folder
//! (the same keys sign both generations of certificates); `.gitleaks.toml` allows exactly them.
//!
//! The listener and every connection task belong to the `FakeSlack`: `shutdown` stops them and joins within a
//! bound, and dropping it aborts them.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;

use crate::http::Http;

mod history;
mod ws;
pub use history::History;
pub use ws::{Flood, WsPeer};

/// The names the test resolver maps to loopback: Slack's, two lookalikes, a foreign host, and one Slack name the
/// trusted certificate does not cover.
pub const HOSTS: &[&str] = &[
    "slack.com",
    "api.slack.com",
    "files.slack.com",
    "wss-primary.slack.com",
    "evilslack.com",
    "slack.com.evil.example",
    "other.example",
    "nosan.slack.com",
];

/// The listener's send buffer: small enough that a large reply cannot leave in one write.
const SEND_BUFFER: u32 = 4096;

const CA_PEM: &str = include_str!("ca.pem");
const SERVER_PEM: &str = include_str!("server.pem");
const SERVER_KEY: &str = include_str!("server.key");
const ROGUE_PEM: &str = include_str!("rogue.pem");
const ROGUE_KEY: &str = include_str!("rogue.key");

/// One request head and body as the fake received it.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub target: String,
    pub host: String,
    pub authorization: Option<String>,
    pub body: Vec<u8>,
    /// The request asked to upgrade to a websocket.
    pub upgrade: bool,
}

impl Seen {
    fn url(&self) -> reqwest::Url {
        reqwest::Url::parse(&format!("https://fake{}", self.target)).expect("a request target")
    }

    /// The target without its query.
    pub fn path(&self) -> String {
        self.url().path().to_string()
    }

    /// A query parameter, decoded.
    pub fn param(&self, name: &str) -> Option<String> {
        self.url()
            .query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    }

    /// The body as JSON, when it is.
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

/// What the server does with a request.
pub enum Reply {
    /// A complete, well-formed answer.
    Status {
        code: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// These bytes verbatim, then close: malformed, lying or cut-short answers.
    Raw(Vec<u8>),
    /// Read the request and never answer.
    Hang,
    /// These bytes, then never another.
    HangAfter(Vec<u8>),
    /// These bytes (a head with no length), then an endless body until the peer is gone.
    Flood(Vec<u8>),
    /// Close the connection with no answer.
    Drop,
    /// Complete the websocket handshake, then run this script on the connection.
    Socket(ws::SocketScript),
}

impl Reply {
    pub fn ok(body: &[u8]) -> Reply {
        Reply::Status {
            code: 200,
            headers: Vec::new(),
            body: body.to_vec(),
        }
    }

    pub fn json(value: &serde_json::Value) -> Reply {
        Reply::ok(value.to_string().as_bytes())
    }

    /// A websocket: the script drives the server side of one connection.
    pub fn socket<F, Fut>(script: F) -> Reply
    where
        F: Fn(WsPeer) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        Reply::Socket(Arc::new(move |peer| Box::pin(script(peer))))
    }

    pub fn redirect(location: &str) -> Reply {
        Reply::Status {
            code: 302,
            headers: vec![("Location".into(), location.into())],
            body: Vec::new(),
        }
    }
}

/// The script: a request and the fixture's port in, a reply out.
pub type Script = Arc<dyn Fn(&Seen, u16) -> Reply + Send + Sync>;

#[derive(Clone, Copy)]
pub enum Identity {
    /// Signed by the test CA, for the names in `HOSTS` but `nosan.slack.com`.
    Trusted,
    /// Self-signed; no client trusts it.
    Rogue,
}

#[derive(Default)]
struct Log {
    seen: Mutex<Vec<Seen>>,
    connections: AtomicUsize,
    tls_failures: AtomicUsize,
    flooded: AtomicUsize,
    flood_ended: AtomicBool,
    sockets: ws::Sockets,
}

pub struct FakeSlack {
    port: u16,
    log: Arc<Log>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl FakeSlack {
    pub async fn start(
        identity: Identity,
        script: impl Fn(&Seen, u16) -> Reply + Send + Sync + 'static,
    ) -> FakeSlack {
        // A TcpSocket, not TcpListener::bind: the workspace's rule against inheritable sockets on Windows.
        let socket = TcpSocket::new_v4().expect("socket");
        socket
            .bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .expect("bind loopback");
        // A small send buffer, so a write that would block shows on Linux as it does on Windows: output left in the
        // TLS session is not sent until a flush (accepted sockets inherit the size).
        socket
            .set_send_buffer_size(SEND_BUFFER)
            .expect("send buffer");
        let listener = socket.listen(64).expect("listen");
        let port = listener.local_addr().expect("local addr").port();
        let log = Arc::new(Log::default());
        let (stop, stopped) = watch::channel(false);
        let acceptor = acceptor(identity);
        let script: Script = Arc::new(script);
        let task = tokio::spawn(accept_loop(
            listener,
            acceptor,
            script,
            port,
            log.clone(),
            stopped,
        ));
        FakeSlack {
            port,
            log,
            stop,
            task: Some(task),
        }
    }

    /// A client for this fake: its CA as the only root, its port as the only port, `HOSTS` as the only names.
    pub fn http(&self) -> Http {
        Http::for_test(ca(), self.port, HOSTS)
    }

    pub fn url(&self, host: &str, path: &str) -> String {
        format!("https://{host}:{}{path}", self.port)
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.log.seen.lock().unwrap().clone()
    }

    pub fn connections(&self) -> usize {
        self.log.connections.load(Ordering::SeqCst)
    }

    pub fn tls_failures(&self) -> usize {
        self.log.tls_failures.load(Ordering::SeqCst)
    }

    /// Websocket connections: open now, the most open at once, and ever accepted.
    pub fn sockets(&self) -> (usize, usize, usize) {
        self.log.sockets.counts()
    }

    /// Bytes a `Flood` reply wrote, and whether its writer has ended (the peer went away).
    pub fn flood(&self) -> (usize, bool) {
        (
            self.log.flooded.load(Ordering::SeqCst),
            self.log.flood_ended.load(Ordering::SeqCst),
        )
    }

    /// Stop the listener and every connection, joined within five seconds.
    pub async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("the fake stops within its bound")
                .expect("the fake's task");
        }
    }
}

impl Drop for FakeSlack {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Wait, bounded, until `check` holds; the caller's assertion follows.
pub async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let end = tokio::time::Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(
            tokio::time::Instant::now() < end,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub fn ca() -> CertificateDer<'static> {
    CertificateDer::from_pem_slice(CA_PEM.as_bytes()).expect("fixture CA")
}

fn acceptor(identity: Identity) -> TlsAcceptor {
    let (cert, key) = match identity {
        Identity::Trusted => (SERVER_PEM, SERVER_KEY),
        Identity::Rogue => (ROGUE_PEM, ROGUE_KEY),
    };
    let key = PrivateKeyDer::from_pem_slice(key.as_bytes()).expect("fixture key");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from_pem_slice(cert.as_bytes()).expect("fixture certificate")],
            key,
        )
        .expect("fixture certificate and key");
    TlsAcceptor::from(Arc::new(config))
}

async fn accept_loop(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    script: Script,
    port: u16,
    log: Arc<Log>,
    mut stop: watch::Receiver<bool>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            accepted = accept(&listener) => {
                let Some(tcp) = accepted else { break };
                log.connections.fetch_add(1, Ordering::SeqCst);
                connections.spawn(serve(tcp, acceptor.clone(), script.clone(), port, log.clone(), stop.clone()));
            }
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}

#[allow(
    clippy::disallowed_methods,
    reason = "the test fake accepts on loopback; it is not a daemon listener"
)]
async fn accept(listener: &TcpListener) -> Option<TcpStream> {
    listener
        .accept()
        .await
        .ok()
        .map(|(tcp, _): (TcpStream, SocketAddr)| tcp)
}

async fn wait_for_stop(stop: &mut watch::Receiver<bool>) {
    while !*stop.borrow() {
        if stop.changed().await.is_err() {
            break;
        }
    }
}

async fn serve(
    tcp: TcpStream,
    acceptor: TlsAcceptor,
    script: Script,
    port: u16,
    log: Arc<Log>,
    mut stop: watch::Receiver<bool>,
) {
    let Ok(mut tls) = acceptor.accept(tcp).await else {
        log.tls_failures.fetch_add(1, Ordering::SeqCst);
        return;
    };
    while let Some((seen, raw)) = read_request(&mut tls).await {
        log.seen.lock().unwrap().push(seen.clone());
        match script(&seen, port) {
            Reply::Socket(socket) => {
                let guard = log.sockets.enter();
                let run = ws::serve_socket(tls, raw, socket, guard);
                tokio::select! {
                    _ = run => {}
                    _ = wait_for_stop(&mut stop) => {}
                }
                return;
            }
            Reply::Status {
                code,
                headers,
                body,
            } => {
                let mut out = format!("HTTP/1.1 {code} Fake\r\nContent-Length: {}\r\n", body.len())
                    .into_bytes();
                for (name, value) in headers {
                    out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
                }
                out.extend_from_slice(b"\r\n");
                out.extend_from_slice(&body);
                if tls.write_all(&out).await.is_err() || tls.flush().await.is_err() {
                    return;
                }
            }
            Reply::Raw(bytes) => {
                let _ = tls.write_all(&bytes).await;
                let _ = tls.flush().await;
                let _ = tls.shutdown().await;
                return;
            }
            Reply::Hang => return wait_for_stop(&mut stop).await,
            Reply::HangAfter(bytes) => {
                let _ = tls.write_all(&bytes).await;
                let _ = tls.flush().await;
                return wait_for_stop(&mut stop).await;
            }
            Reply::Flood(head) => {
                let chunk = vec![b'x'; 64 * 1024];
                if tls.write_all(&head).await.is_ok() {
                    while tls.write_all(&chunk).await.is_ok() && tls.flush().await.is_ok() {
                        log.flooded.fetch_add(chunk.len(), Ordering::SeqCst);
                    }
                }
                log.flood_ended.store(true, Ordering::SeqCst);
                return;
            }
            Reply::Drop => return,
        }
    }
}

/// One request: the head, then `Content-Length` body bytes. `None` at end of stream or on a malformed head.
/// The bytes read are returned with it, so a websocket handshake can replay them.
async fn read_request<S: AsyncReadExt + Unpin>(stream: &mut S) -> Option<(Seen, Vec<u8>)> {
    let mut buf = Vec::new();
    let end = loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        if buf.len() > 64 * 1024 {
            return None;
        }
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let (method, target) = (
        request_line.next()?.to_string(),
        request_line.next()?.to_string(),
    );
    let (mut host, mut authorization, mut length) = (String::new(), None, 0usize);
    let mut upgrade = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        match name.to_ascii_lowercase().as_str() {
            "host" => host = value,
            "authorization" => authorization = Some(value),
            "content-length" => length = value.parse().ok()?,
            "upgrade" => upgrade = value.eq_ignore_ascii_case("websocket"),
            _ => {}
        }
    }
    let mut body = buf[end..].to_vec();
    while body.len() < length {
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(length);
    let seen = Seen {
        method,
        target,
        host,
        authorization,
        body,
        upgrade,
    };
    Some((seen, buf))
}
