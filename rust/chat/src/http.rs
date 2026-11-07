//! The one way out of this crate to the network (docs/history/slack-client-plan.md, "Configuration, credentials and
//! the network boundary"): one client and one TLS policy, the destination and redirect policy, when the bot
//! authorization is sent, one owner-held deadline per operation, the admission bound, the raw response bound and
//! fixed errors. It knows no Slack method; callers build a `Request` and read a `Response`.
//!
//! The product constructor (`Http::new`) trusts the bundled web roots and sends only to Slack hosts. There is no
//! endpoint override, insecure mode or plaintext path; the one test constructor exists under `cfg(test)` or the `test-support` feature alone.

use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, LOCATION};
use reqwest::{Method, StatusCode, Url};
use tokio::net::{TcpSocket, TcpStream};
use tokio::sync::{oneshot, Semaphore};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

/// Operations admitted at once; the next one is `Busy`.
pub const MAX_OPERATIONS: usize = 16;
/// A raw response body, counted as it streams, whatever `Content-Length` claims.
pub const MAX_BODY: usize = 1 << 20;
/// Redirects followed by one operation.
pub const MAX_REDIRECTS: usize = 3;
/// The only port a product destination may name.
const HTTPS_PORT: u16 = 443;
/// A websocket message, enforced by the library from the frame header on, before a message is read whole.
pub const MAX_MESSAGE: usize = 1 << 20;

/// A connected websocket, over the one TLS policy.
pub(crate) type Websocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A credential (cloned only into the requests that carry it). It formats as a fixed mask, so no log, error or debug print of anything holding one shows it.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(value.into())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Why an operation failed. A code and a fixed explanation: no URL, header, token or library text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpError {
    /// Sixteen operations are in flight already.
    Busy,
    /// The operation's deadline passed (connection, headers, redirects or body).
    Timeout,
    /// A URL, first or redirected, is not an HTTPS Slack destination.
    Destination,
    /// Too many redirects, a redirect without a usable location, or a redirect of a mutation.
    Redirect,
    /// The server's certificate or name was refused.
    Tls,
    /// No connection could be made.
    Connect,
    /// The server's answer was malformed or ended early.
    Protocol,
    /// The body passed `MAX_BODY`.
    TooLarge,
    /// The request itself cannot be sent (a credential that is not a header value).
    Invalid,
}

impl HttpError {
    pub fn code(self) -> &'static str {
        match self {
            HttpError::Busy => "busy",
            HttpError::Timeout => "timeout",
            HttpError::Destination => "destination",
            HttpError::Redirect => "redirect",
            HttpError::Tls => "tls",
            HttpError::Connect => "connect",
            HttpError::Protocol => "protocol",
            HttpError::TooLarge => "too_large",
            HttpError::Invalid => "invalid",
        }
    }

    pub fn explanation(self) -> &'static str {
        match self {
            HttpError::Busy => "too many chat operations are in flight",
            HttpError::Timeout => "the operation passed its deadline",
            HttpError::Destination => "the destination is not an HTTPS Slack host",
            HttpError::Redirect => "the redirect was refused",
            HttpError::Tls => "the server's certificate or name was refused",
            HttpError::Connect => "no connection could be made",
            HttpError::Protocol => "the server's answer was malformed or ended early",
            HttpError::TooLarge => "the response passed its size bound",
            HttpError::Invalid => "the request cannot be sent",
        }
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.explanation())
    }
}

impl Error for HttpError {}

/// One operation: a method, a URL, an optional bot authorization, an optional body and its deadline. The URL may be
/// private (a file download); it never formats.
pub struct Request {
    method: Method,
    url: String,
    bearer: Option<Secret>,
    body: Vec<u8>,
    content_type: Option<&'static str>,
    deadline: Duration,
}

impl Request {
    /// A read. Only reads follow redirects.
    pub fn get(url: impl Into<String>, deadline: Duration) -> Request {
        Request {
            method: Method::GET,
            url: url.into(),
            bearer: None,
            body: Vec::new(),
            content_type: None,
            deadline,
        }
    }

    /// A mutation: sent once, never redirected, never retried.
    pub fn post(
        url: impl Into<String>,
        deadline: Duration,
        content_type: &'static str,
        body: Vec<u8>,
    ) -> Request {
        Request {
            method: Method::POST,
            url: url.into(),
            bearer: None,
            body,
            content_type: Some(content_type),
            deadline,
        }
    }

    /// Send the bot authorization with each hop of this operation.
    pub fn bearer(mut self, token: Secret) -> Request {
        self.bearer = Some(token);
        self
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method.as_str())
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

/// A finished answer: status, headers and at most `MAX_BODY` bytes.
pub struct Response {
    status: u16,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Response {
    pub fn status(&self) -> u16 {
        self.status
    }

    /// A header's value, when it is present and text.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)?.to_str().ok()
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

/// The client, the destination policy and the admission bound, shared by every operation of one chat service.
#[derive(Clone)]
pub struct Http {
    inner: Arc<Inner>,
}

struct Inner {
    client: reqwest::Client,
    /// The one port a destination may name: 443, or the fixture's under test.
    port: u16,
    permits: Arc<Semaphore>,
    /// The one rustls configuration: the client above and every websocket share it.
    tls: Arc<rustls::ClientConfig>,
    /// The name resolver for a websocket dial: `None` in the product (the system resolver), the fixture's loopback
    /// map under test. Set only by `for_test`; there is no setter.
    resolver: Option<Arc<dyn reqwest::dns::Resolve>>,
}

impl Http {
    /// The product client: the bundled web roots, Slack destinations only.
    pub fn new() -> Result<Http, HttpError> {
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Http::build(roots, HTTPS_PORT, None)
    }

    /// The test client: the fixture's CA as the only root, its port as the only port, and a resolver that maps the
    /// fixture's logical hosts to loopback and refuses every other name.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(
        ca: rustls::pki_types::CertificateDer<'static>,
        port: u16,
        hosts: &'static [&'static str],
    ) -> Http {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca).expect("test CA");
        Http::build(roots, port, Some(Arc::new(test_resolver::Loopback(hosts))))
            .expect("test client")
    }

    fn build(
        roots: rustls::RootCertStore,
        port: u16,
        resolver: Option<Arc<dyn reqwest::dns::Resolve>>,
    ) -> Result<Http, HttpError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|_| HttpError::Invalid)?
            .with_root_certificates(roots)
            .with_no_client_auth();
        // No idle connection is kept, so no request can be re-sent on a stale pooled connection; HTTP/1 has no
        // protocol-level retry; redirects and ambient proxies are off and the library's retry policy is "never".
        let mut builder = reqwest::Client::builder()
            .tls_backend_preconfigured(tls.clone())
            .https_only(true)
            .http1_only()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .pool_max_idle_per_host(0);
        if let Some(resolver) = &resolver {
            builder = builder.dns_resolver(resolver.clone());
        }
        let client = builder.build().map_err(|_| HttpError::Invalid)?;
        Ok(Http {
            inner: Arc::new(Inner {
                client,
                port,
                permits: Arc::new(Semaphore::new(MAX_OPERATIONS)),
                tls: Arc::new(tls),
                resolver,
            }),
        })
    }

    /// Run one operation. The operation runs as a task of its own that holds the permit and the deadline, so a
    /// caller that stops waiting neither releases the permit early nor cancels a mutation already on the wire.
    pub async fn send(&self, request: Request) -> Result<Response, HttpError> {
        let permit = self
            .inner
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| HttpError::Busy)?;
        let inner = self.inner.clone();
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let result = match tokio::time::timeout(request.deadline, inner.run(&request)).await {
                Ok(result) => result,
                Err(_) => Err(HttpError::Timeout),
            };
            drop(permit);
            let _ = tx.send(result);
        });
        // The task ends only by returning or by the runtime's shutdown; the latter is a lost answer.
        rx.await.unwrap_or(Err(HttpError::Protocol))
    }

    /// Open a websocket: the same destination policy as a request (scheme `wss`), the same TLS configuration, no
    /// ambient proxy, the connection and handshake together under `deadline`, and messages bounded by `MAX_MESSAGE`.
    /// The URL may be private; it reaches no error.
    pub(crate) async fn websocket(
        &self,
        url: &str,
        deadline: Duration,
    ) -> Result<Websocket, HttpError> {
        let url = Url::parse(url).map_err(|_| HttpError::Destination)?;
        self.inner.check_url(&url, "wss")?;
        let host = url.domain().ok_or(HttpError::Destination)?.to_string();
        let inner = self.inner.clone();
        let handshake = async {
            let tcp = inner.dial(&host).await?;
            let config = WebSocketConfig::default()
                .max_message_size(Some(MAX_MESSAGE))
                .max_frame_size(Some(MAX_MESSAGE));
            let connector = Connector::Rustls(inner.tls.clone());
            tokio_tungstenite::client_async_tls_with_config(
                url.as_str(),
                tcp,
                Some(config),
                Some(connector),
            )
            .await
            .map(|(ws, _)| ws)
            .map_err(classify_ws)
        };
        tokio::time::timeout(deadline, handshake)
            .await
            .unwrap_or(Err(HttpError::Timeout))
    }
}

impl Inner {
    async fn run(&self, request: &Request) -> Result<Response, HttpError> {
        let mut url = Url::parse(&request.url).map_err(|_| HttpError::Destination)?;
        let mut redirects = 0;
        loop {
            self.check_destination(&url)?;
            // Built anew for each hop; the library carries nothing across a redirect.
            let mut hop = self.client.request(request.method.clone(), url.clone());
            if let Some(token) = &request.bearer {
                let mut value = HeaderValue::from_str(&format!("Bearer {}", token.0))
                    .map_err(|_| HttpError::Invalid)?;
                value.set_sensitive(true);
                hop = hop.header(AUTHORIZATION, value);
            }
            if request.method == Method::POST {
                if let Some(kind) = request.content_type {
                    hop = hop.header(CONTENT_TYPE, kind);
                }
                hop = hop.body(request.body.clone());
            }
            let mut response = hop.send().await.map_err(classify)?;
            if is_redirect(response.status()) {
                redirects += 1;
                if request.method != Method::GET || redirects > MAX_REDIRECTS {
                    return Err(HttpError::Redirect);
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or(HttpError::Redirect)?;
                url = url.join(location).map_err(|_| HttpError::Redirect)?;
                continue;
            }
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(classify)? {
                if body.len() + chunk.len() > MAX_BODY {
                    return Err(HttpError::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(Response {
                status,
                headers,
                body,
            });
        }
    }

    /// HTTPS, no userinfo, the one port, and a Slack host: refused before any connection, for the first URL and for
    /// each redirect alike. Bot authorization rides on exactly the hops this admits.
    fn check_destination(&self, url: &Url) -> Result<(), HttpError> {
        self.check_url(url, "https")
    }

    /// `check_destination` for a scheme: `https` for a request, `wss` for a websocket.
    fn check_url(&self, url: &Url, scheme: &str) -> Result<(), HttpError> {
        let plain = url.scheme() == scheme && url.username().is_empty() && url.password().is_none();
        match url.domain() {
            Some(host)
                if plain
                    && url.port_or_known_default() == Some(self.port)
                    && is_slack_host(host) =>
            {
                Ok(())
            }
            _ => Err(HttpError::Destination),
        }
    }
}

impl Inner {
    /// A TCP connection to a host already admitted by `check_url`, on the one port. Direct: no proxy.
    async fn dial(&self, host: &str) -> Result<TcpStream, HttpError> {
        let addr = self.address(host).await?;
        // A TcpSocket, not TcpStream::connect: the workspace's rule against inheritable sockets on Windows.
        let socket = if addr.is_ipv4() {
            TcpSocket::new_v4()
        } else {
            TcpSocket::new_v6()
        }
        .map_err(|_| HttpError::Connect)?;
        #[allow(
            clippy::disallowed_methods,
            reason = "the chat's one outbound dial, to a Slack host admitted by check_url, then TLS: not the control plane (ADR 0049); a TcpSocket because mio's connect makes the socket inheritable on Windows"
        )]
        let connected = socket.connect(addr).await;
        connected.map_err(|_| HttpError::Connect)
    }

    /// The resolver's first address for `host`, on the one port: the fixture's map when one was set, else the system's.
    async fn address(&self, host: &str) -> Result<std::net::SocketAddr, HttpError> {
        let mut addr = match &self.resolver {
            Some(resolver) => {
                let name = host.parse().map_err(|_| HttpError::Connect)?;
                resolver
                    .resolve(name)
                    .await
                    .map_err(|_| HttpError::Connect)?
                    .next()
            }
            None => tokio::net::lookup_host((host, self.port))
                .await
                .map_err(|_| HttpError::Connect)?
                .next(),
        }
        .ok_or(HttpError::Connect)?;
        addr.set_port(self.port);
        Ok(addr)
    }
}

/// `slack.com` or a subdomain at a dot boundary, every label non-empty: not `evilslack.com`, not
/// `slack.com.evil.example`.
pub fn is_slack_host(host: &str) -> bool {
    host == "slack.com"
        || host
            .strip_suffix(".slack.com")
            .is_some_and(|rest| rest.split('.').all(|label| !label.is_empty()))
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

/// A library error becomes a fixed code; its text, which can hold a URL, is dropped here.
fn classify(error: reqwest::Error) -> HttpError {
    let mut source: Option<&(dyn Error + 'static)> = Some(&error);
    while let Some(err) = source {
        if is_tls(err) {
            return HttpError::Tls;
        }
        source = err.source();
    }
    if error.is_connect() {
        HttpError::Connect
    } else if error.is_timeout() {
        HttpError::Timeout
    } else {
        HttpError::Protocol
    }
}

/// A websocket library error becomes a fixed code; its text, which can hold a URL, is dropped here.
fn classify_ws(error: tokio_tungstenite::tungstenite::Error) -> HttpError {
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::Io(io) => {
            if is_tls(&io) {
                HttpError::Tls
            } else {
                HttpError::Connect
            }
        }
        Error::Tls(_) => HttpError::Tls,
        Error::Capacity(_) => HttpError::TooLarge,
        Error::Url(_) => HttpError::Destination,
        _ => HttpError::Protocol,
    }
}

/// A rustls error, possibly wrapped in the `io::Error`s the connector nests it in.
fn is_tls(err: &(dyn Error + 'static)) -> bool {
    err.is::<rustls::Error>()
        || err
            .downcast_ref::<std::io::Error>()
            .and_then(|io| io.get_ref())
            .is_some_and(|inner| is_tls(inner))
}

#[cfg(any(test, feature = "test-support"))]
mod test_resolver {
    use reqwest::dns::{Addrs, Name, Resolve, Resolving};
    use std::net::SocketAddr;

    /// The fixture's logical hosts resolve to loopback (the port is the URL's); any other name is refused.
    pub(super) struct Loopback(pub(super) &'static [&'static str]);

    impl Resolve for Loopback {
        fn resolve(&self, name: Name) -> Resolving {
            let known = self.0.contains(&name.as_str());
            Box::pin(async move {
                if known {
                    Ok(Box::new(std::iter::once(SocketAddr::from(([127, 0, 0, 1], 0)))) as Addrs)
                } else {
                    Err("name refused".into())
                }
            })
        }
    }
}
