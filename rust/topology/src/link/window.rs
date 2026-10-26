//! The window's end of a link: [`Link::start`], the window's one login to a computer (or its one connection to the
//! hub's relay socket), and [`LinkHandle`], through which its control connection, its pages and its rows open
//! channels instead of logins.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sot_protocol::ops::link::{LinkHelloReq, LinkHelloRes, LinkRefusal, LINK};
use sot_protocol::{codec, op, Frame, Kind};
use tokio::io::{AsyncRead, AsyncWrite};

use super::channel::Channel;
use super::table::{End, Opened, Shared, Side};
use crate::ssh_bridge::{SshRecipe, SSH_LOGIN_BOUND};

/// Where a window's link goes: an ssh login, or a generated hub relay socket reached locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRoute {
    Ssh(SshRecipe),
    Relay(std::path::PathBuf),
}

impl std::fmt::Display for LinkRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkRoute::Ssh(recipe) => recipe.fmt(f),
            LinkRoute::Relay(path) => write!(f, "relay:{}", path.display()),
        }
    }
}

/// Why a link did not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkStartError {
    /// The far end answered `link.hello` with a refusal a person acts on: another link number, or a backend whose
    /// bridge predates the link and handed the hello to its daemon. `message` is for the person.
    Refused { code: String, message: String },
    /// The login, the relay connection or the far daemon failed; the caller retries on its pace.
    Failed(String),
}

impl std::fmt::Display for LinkStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkStartError::Refused { message, .. } => f.write_str(message),
            LinkStartError::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for LinkStartError {}

/// Why an open did not give a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// The host's link is down; nothing was sent.
    LinkDown,
    /// The router refused the channel (`unreachable`: its daemon did not take the connection; `too_many_channels`).
    Refused { code: String, detail: String },
    /// No answer within the caller's bound.
    TimedOut,
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::LinkDown => f.write_str("the host's link is down"),
            OpenError::Refused { code, detail } => write!(f, "the link refused the channel ({code}): {detail}"),
            OpenError::TimedOut => f.write_str("the link did not answer the channel's open in time"),
        }
    }
}

impl std::error::Error for OpenError {}

/// A running link. Dropping it ends the link and every channel on it, and ends its ssh child (`kill_on_drop`) or
/// closes its relay connection; the far router then closes every local connection it made and exits.
pub struct Link {
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
    control: Option<Channel>,
    build: String,
    child: Option<tokio::process::Child>,
    last_stderr: Option<Arc<Mutex<Option<String>>>>,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.shared.end(End::clean("the window closed the link"));
        self.task.abort();
    }
}

impl Link {
    /// The window's one start of a login to a computer, or of a connection to the hub's relay socket for one: the
    /// login, then `link.hello` answered within `SSH_LOGIN_BOUND`. Every connection the window makes to that
    /// computer from then on is a channel on this link.
    pub async fn start(route: &LinkRoute) -> Result<Link, LinkStartError> {
        match route {
            LinkRoute::Ssh(recipe) => {
                #[allow(clippy::disallowed_methods, reason = "the window's one ssh login per computer, owned by the window: its drop kills it")]
                let spawned = tokio::process::Command::from(recipe.command()).kill_on_drop(true).spawn();
                let mut child = spawned.map_err(|e| LinkStartError::Failed(format!("spawn ssh {recipe}: {e}")))?;
                let stdin = child.stdin.take().expect("spawned with a piped stdin");
                let stdout = child.stdout.take().expect("spawned with a piped stdout");
                let last_stderr = drain_stderr(child.stderr.take().expect("spawned with a piped stderr"));
                match Self::over_within(stdout, stdin, super::SILENCE_BOUND).await {
                    Ok(mut link) => {
                        link.child = Some(child);
                        link.last_stderr = Some(last_stderr);
                        Ok(link)
                    }
                    // The child's last stderr line is ssh's own diagnosis of a login that ended before the answer.
                    Err(LinkStartError::Failed(why)) => match crate::ssh_bridge::last_stderr_after_failure(&last_stderr).await {
                        Some(line) => Err(LinkStartError::Failed(format!("{why} (ssh: {line})"))),
                        None => Err(LinkStartError::Failed(why)),
                    },
                    Err(refused) => Err(refused),
                }
            }
            LinkRoute::Relay(path) => {
                let stream = super::local::connect_local(path)
                    .await
                    .map_err(|e| LinkStartError::Failed(format!("connect {}: {e}", path.display())))?;
                let (rx, tx) = tokio::io::split(stream);
                Self::over_within(rx, tx, super::SILENCE_BOUND).await
            }
        }
    }

    /// A link over a stream already made: `link.hello`, its answer within `SSH_LOGIN_BOUND`, then the link runs on
    /// the current runtime until it is dropped or ends.
    pub async fn over<R, W>(rx: R, tx: W) -> Result<Link, LinkStartError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::over_within(rx, tx, super::SILENCE_BOUND).await
    }

    pub(super) async fn over_within<R, W>(rx: R, mut tx: W, silence: Duration) -> Result<Link, LinkStartError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (heard, last) = super::io::Heard::new(rx);
        let mut rx = tokio::io::BufReader::new(heard);
        let hello = Frame::req(1, op::LINK_HELLO, serde_json::to_value(LinkHelloReq { link: LINK }).expect("a hello serializes"));
        codec::write_frame(&mut tx, &hello, None).await.map_err(|e| LinkStartError::Failed(format!("link.hello: {e:#}")))?;
        let answer = tokio::time::timeout(SSH_LOGIN_BOUND, codec::read_envelope(&mut rx))
            .await
            .map_err(|_| LinkStartError::Failed(format!("link.hello reply timed out after {SSH_LOGIN_BOUND:?}")))?
            .map_err(|e| LinkStartError::Failed(format!("link.hello reply: {e:#}")))?;
        let build = hello_answer(answer)?;
        let shared = Shared::new(Side::Window);
        let task = tokio::spawn(super::io::run(Arc::clone(&shared), rx, last, tx, None, silence));
        let control = Some(Channel::new(Arc::clone(&shared), 0));
        Ok(Link { shared, task, control, build, child: None, last_stderr: None })
    }

    /// Channel 0, the control connection; once.
    pub fn take_control(&mut self) -> Option<Channel> {
        self.control.take()
    }

    /// The far router's build, from its hello.
    pub fn build(&self) -> &str {
        &self.build
    }

    /// Why the link ended; `None` while it is up.
    pub fn ended(&self) -> Option<String> {
        self.shared.lock().down().map(|end| end.reason.clone())
    }

    /// The ssh child's last stderr line, after a failure; `None` on a relay route or a child that said nothing.
    pub async fn last_stderr(&self) -> Option<String> {
        match &self.last_stderr {
            Some(cell) => crate::ssh_bridge::last_stderr_after_failure(cell).await,
            None => None,
        }
    }
}

/// The hello's answer: the router's build, or why there is no link.
fn hello_answer(answer: Frame) -> Result<String, LinkStartError> {
    let refused = |code: &str, message: String| LinkStartError::Refused { code: code.to_string(), message };
    if answer.kind != Kind::Res || answer.op != op::LINK_HELLO {
        return Err(LinkStartError::Failed(format!("link.hello: an unexpected answer ({} {:?})", answer.op, answer.kind)));
    }
    if answer.payload.get("error").is_some() {
        let refusal: LinkRefusal = serde_json::from_value(answer.payload)
            .map_err(|e| LinkStartError::Failed(format!("link.hello: a refusal that does not parse: {}", codec::unparsed(&e, 0))))?;
        return Err(match refusal.code.as_str() {
            "link_mismatch" => refused(
                "link_mismatch",
                format!("the backend there speaks link {}, this window link {LINK}: update the older side", refusal.link.unwrap_or_default()),
            ),
            // The daemon's answer to a first frame that is not a hello: a bridge from before the link handed it on.
            "unauthenticated" => refused("predates_link", "the backend there predates the link (0.7.0); update it".to_string()),
            "unreachable" => LinkStartError::Failed(format!("the far router could not reach its daemon: {}", refusal.error)),
            code => refused(code, format!("the far router refused the link ({code}): {}", refusal.error)),
        });
    }
    let res: LinkHelloRes = serde_json::from_value(answer.payload)
        .map_err(|e| LinkStartError::Failed(format!("link.hello: an answer that does not parse: {}", codec::unparsed(&e, 0))))?;
    if res.link != LINK {
        return Err(refused("link_mismatch", format!("the backend there speaks link {}, this window link {LINK}: update the older side", res.link)));
    }
    Ok(res.build)
}

/// The child's last non-empty stderr line, drained on its own task for as long as the child writes.
fn drain_stderr(stderr: tokio::process::ChildStderr) -> Arc<Mutex<Option<String>>> {
    let last = Arc::new(Mutex::new(None::<String>));
    let cell = Arc::clone(&last);
    tokio::spawn(async move {
        let mut lines = sot_protocol::child_lines::ChildLines::new(stderr);
        while let Ok(Some(line)) = lines.next_line().await {
            if !line.trim().is_empty() {
                *cell.lock().unwrap_or_else(|p| p.into_inner()) = Some(line);
            }
        }
    });
    last
}

/// One host's link as its callers see it: down, or the link its control session holds up. The host's transport is
/// its only writer (up at an accepted hello, down when the session ends); every page connection and every lane dial
/// opens a channel through it. A default handle is down.
#[derive(Clone, Default)]
pub struct LinkHandle {
    slot: Arc<Mutex<Option<Arc<Shared>>>>,
}

impl std::fmt::Debug for LinkHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LinkHandle({})", if self.is_up() { "up" } else { "down" })
    }
}

impl LinkHandle {
    /// The transport only: the session over `link`'s channel 0 was admitted.
    pub fn set_up(&self, link: &Link) {
        *self.slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::clone(&link.shared));
    }

    /// The transport only: the session ended.
    pub fn set_down(&self) {
        *self.slot.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    fn current(&self) -> Option<Arc<Shared>> {
        self.slot.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn is_up(&self) -> bool {
        self.current().is_some_and(|shared| !shared.is_down())
    }

    /// Opens a channel; a down link fails at once and sends nothing. The caller bounds the wait.
    pub async fn open(&self) -> Result<Channel, OpenError> {
        let (shared, ch) = self.begin()?;
        let mut opening = Opening { shared: &shared, ch, done: false };
        let opened = std::future::poll_fn(|cx| match shared.with(|t| t.poll_open(ch, Some(cx.waker()))) {
            Opened::Pending => std::task::Poll::Pending,
            other => std::task::Poll::Ready(other),
        })
        .await;
        opening.finish(opened)
    }

    /// [`Self::open`] for a blocking caller, waiting at most `bound`.
    pub fn open_blocking(&self, bound: Duration) -> Result<Channel, OpenError> {
        let deadline = Instant::now() + bound;
        let (shared, ch) = self.begin()?;
        let mut opening = Opening { shared: &shared, ch, done: false };
        let mut table = shared.lock();
        let opened = loop {
            match table.poll_open(ch, None) {
                Opened::Pending => {}
                other => break other,
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                drop(table);
                return Err(OpenError::TimedOut);
            }
            table = shared.changed.wait_timeout(table, left).unwrap_or_else(|p| p.into_inner()).0;
        };
        drop(table);
        opening.finish(opened)
    }

    fn begin(&self) -> Result<(Arc<Shared>, u32), OpenError> {
        let shared = self.current().ok_or(OpenError::LinkDown)?;
        let ch = shared.with(|t| t.begin_open()).map_err(|_| OpenError::LinkDown)?;
        Ok((shared, ch))
    }
}

/// An open in flight: dropped before its answer, the answer closes the channel when it comes.
struct Opening<'a> {
    shared: &'a Arc<Shared>,
    ch: u32,
    done: bool,
}

impl Opening<'_> {
    fn finish(&mut self, opened: Opened) -> Result<Channel, OpenError> {
        self.done = true;
        match opened {
            Opened::Open => Ok(Channel::new(Arc::clone(self.shared), self.ch)),
            Opened::Refused { code, error } => Err(OpenError::Refused { code, detail: error }),
            Opened::LinkDown => Err(OpenError::LinkDown),
            Opened::Pending => unreachable!("an open finishes only on an answer"),
        }
    }
}

impl Drop for Opening<'_> {
    fn drop(&mut self) {
        if !self.done {
            let ch = self.ch;
            self.shared.with(|t| t.abandon(ch));
        }
    }
}
