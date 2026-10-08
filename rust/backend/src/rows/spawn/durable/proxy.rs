//! The daemon's end of the durable parent: start it lazily, send it a launch, and wait for the supervisor's exit it
//! forwards. One reader thread routes the parent's replies to whoever asked.

use super::wire::{decode, encode, Channel, LaunchSpec, Reply, Request};
use std::collections::HashMap;
use std::io;
use std::os::fd::RawFd;
use std::process::Stdio;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the daemon waits for the parent to answer a launch or a release.
const ANSWER_BOUND: Duration = Duration::from_secs(30);
/// How long a new parent gets to say hello.
const HELLO_BOUND: Duration = Duration::from_secs(10);

/// What the parent said about a launch, in the order it says it.
pub enum Answer {
    Contended,
    Born,
}

/// The ways a supervisor ended, as the parent saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ended {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// The daemon's handle on its durable parent.
pub struct Client {
    channel: Mutex<Channel>,
    routes: Mutex<Routes>,
    next_id: std::sync::atomic::AtomicU64,
    alive: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
struct Routes {
    /// Replies to a request that is waiting for them.
    replies: HashMap<u64, Sender<Reply>>,
    /// The exits of supervisors the parent will report.
    exits: HashMap<u64, tokio::sync::oneshot::Sender<Ended>>,
}

/// A supervisor the parent forked: wait for it to end.
pub struct DurableChild {
    exit: tokio::sync::oneshot::Receiver<Ended>,
}

impl DurableChild {
    /// The supervisor's exit code, once it ends (`None` when a signal ended it). An error when the parent was lost
    /// before it could say.
    pub async fn wait(self) -> io::Result<Option<i32>> {
        match self.exit.await {
            Ok(ended) => Ok(ended.code),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "the durable parent was lost before it reported the supervisor's exit",
            )),
        }
    }
}

impl Client {
    /// Start a parent from this executable and wait for its hello.
    pub fn start() -> io::Result<Arc<Client>> {
        let (ours, theirs) = Channel::pair()?;
        // The daemon's own path as it started (`own_sotd_bin`), not `current_exe`: after an in-place update the running
        // image's link reads `(deleted)`, while the path still names the installed `sotd`.
        let exe = match crate::agents::env::own_sotd_bin() {
            Some(own) => std::path::PathBuf::from(own),
            None => std::env::current_exe()?,
        };
        let mut command = std::process::Command::new(exe);
        command
            .arg("durable-parent")
            .stdin(Stdio::from(theirs.into_owned()))
            .stdout(Stdio::null())
            .stderr(crate::rows::spawn::detach::supervisor_stderr_std());
        #[allow(
            clippy::disallowed_methods,
            reason = "the durable parent outlives the daemon by design: it finishes an accepted capsule launch (rows/spawn/durable)"
        )]
        let spawned = command.spawn();
        let mut child = spawned?;
        let reader = ours.try_clone()?;
        let client = Arc::new(Client {
            channel: Mutex::new(ours),
            routes: Mutex::new(Routes::default()),
            next_id: std::sync::atomic::AtomicU64::new(1),
            alive: std::sync::atomic::AtomicBool::new(true),
        });
        let (hello_tx, hello_rx) = channel();
        client.routes.lock().unwrap().replies.insert(0, hello_tx);
        let router = client.clone();
        std::thread::Builder::new()
            .name("sot-durable-reader".into())
            .spawn(move || {
                router.route(reader);
                // The parent is the daemon's child: reap it when it ends.
                let _ = child.wait();
            })?;
        match hello_rx.recv_timeout(HELLO_BOUND) {
            Ok(Reply::Hello { pid }) => {
                tracing::info!(pid, "durable parent: started");
                Ok(client)
            }
            _ => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the durable parent did not say hello",
            )),
        }
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Route every reply to the request it answers, until the channel ends.
    fn route(&self, reader: Channel) {
        loop {
            let message = match reader.recv() {
                Ok(Some(message)) => message,
                Ok(None) | Err(_) => break,
            };
            let Ok(reply) = decode::<Reply>(&message.payload) else {
                continue;
            };
            let mut routes = self.routes.lock().unwrap();
            match reply {
                Reply::Hello { .. } => {
                    if let Some(tx) = routes.replies.remove(&0) {
                        let _ = tx.send(reply);
                    }
                }
                Reply::Exited { id, code, signal } => {
                    if let Some(tx) = routes.exits.remove(&id) {
                        let _ = tx.send(Ended { code, signal });
                    }
                }
                Reply::Contended { id }
                | Reply::Failed { id, .. }
                | Reply::Born { id, .. }
                | Reply::Released { id }
                | Reply::ExecFailed { id, .. } => {
                    if let Some(tx) = routes.replies.get(&id) {
                        let _ = tx.send(reply);
                    }
                }
            }
        }
        self.alive.store(false, std::sync::atomic::Ordering::SeqCst);
        // Everyone waiting hears the end: their senders drop.
        let mut routes = self.routes.lock().unwrap();
        routes.replies.clear();
        routes.exits.clear();
    }

    fn send(&self, request: &Request, fds: &[RawFd]) -> io::Result<()> {
        let bytes = encode(request)?;
        self.channel.lock().unwrap().send(&bytes, fds)
    }

    fn expect(rx: &Receiver<Reply>, what: &str) -> io::Result<Reply> {
        rx.recv_timeout(ANSWER_BOUND).map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("the durable parent did not answer ({what})"),
            )
        })
    }

    /// Launch the supervisor `spec` describes: the parent claims the row's fence, forks it held at its gate and says so;
    /// the daemon then releases it. `Ok(None)` when the fence is held and nothing was forked.
    pub fn launch(&self, mut spec: LaunchSpec, stderr: RawFd) -> io::Result<Option<DurableChild>> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        spec.id = id;
        let (tx, rx) = channel();
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
        {
            let mut routes = self.routes.lock().unwrap();
            routes.replies.insert(id, tx);
            routes.exits.insert(id, exit_tx);
        }
        let outcome = self.launch_routed(spec, stderr, &rx);
        let mut routes = self.routes.lock().unwrap();
        routes.replies.remove(&id);
        match outcome {
            Ok(Answer::Born) => Ok(Some(DurableChild { exit: exit_rx })),
            Ok(Answer::Contended) => {
                routes.exits.remove(&id);
                Ok(None)
            }
            Err(e) => {
                routes.exits.remove(&id);
                Err(e)
            }
        }
    }

    fn launch_routed(
        &self,
        spec: LaunchSpec,
        stderr: RawFd,
        rx: &Receiver<Reply>,
    ) -> io::Result<Answer> {
        let id = spec.id;
        self.send(&Request::Launch(spec), &[stderr])?;
        match Self::expect(rx, "launch")? {
            Reply::Contended { .. } => return Ok(Answer::Contended),
            Reply::Failed { text, .. } => return Err(io::Error::other(text)),
            Reply::Born { .. } => {}
            other => {
                return Err(io::Error::other(format!(
                    "the durable parent answered a launch with {other:?}"
                )))
            }
        };
        // The birth is accepted. It is published: the caller holds the permit and the row's guard, and the parent holds
        // the claim. An accepted birth the daemon will not release is cancelled, or the parent would hold its claim.
        let released = self
            .send(&Request::Release { id }, &[])
            .and_then(|()| Self::expect(rx, "release"));
        let released = match released {
            Ok(reply) => reply,
            Err(e) => {
                let _ = self.send(&Request::Cancel { id }, &[]);
                return Err(e);
            }
        };
        match released {
            Reply::Released { .. } => Ok(Answer::Born),
            Reply::ExecFailed { text, .. } => Err(io::Error::other(format!(
                "the supervisor could not be started: {text}"
            ))),
            other => Err(io::Error::other(format!(
                "the durable parent answered a release with {other:?}"
            ))),
        }
    }
}

/// The daemon's one durable parent, started on first use and again if it was lost.
static CLIENT: Mutex<Option<Arc<Client>>> = Mutex::new(None);

pub fn client() -> io::Result<Arc<Client>> {
    let mut slot = CLIENT.lock().unwrap();
    if let Some(client) = slot.as_ref().filter(|c| c.is_alive()) {
        return Ok(client.clone());
    }
    let started = Client::start()?;
    *slot = Some(started.clone());
    Ok(started)
}
