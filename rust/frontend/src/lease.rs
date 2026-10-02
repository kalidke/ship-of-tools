// The window lease (ADR 0050): one dedicated connection per local daemon,
// held for the window's life, so the daemon knows a window is attached and
// the window can say, on leaving, what should happen to the computer's
// sessions. This file is the lease's whole client side; `transport.rs` calls
// `before_data_connection` before each local data connection, and `gpu.rs`
// reads `notice()` and (later) leaves through it.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use interprocess::local_socket::tokio::prelude::*;
use sot_log::challenge::self_identity;
use sot_protocol::ops::{lease, op, FeLeaseReq, FeLeaseRes, FeLeavingReq, FeLeavingRes, FeNoticeSeenReq, LeaveIntent, LeaseOutcome};
use sot_protocol::{codec, Frame};
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio::sync::oneshot::error::TryRecvError;

use crate::dial::HostKey;
use crate::transport::{connect_pipe, Dial, TransportConfig};

const NOTICE_UNDETERMINED: &str =
    "closing will not end sessions: this computer's backend could not verify this window";
const NOTICE_UNSUPPORTED: &str =
    "closing will not end sessions: this computer's backend is older than this window";
const NOTICE_NO_BACKEND: &str =
    "closing will not end sessions: there is no backend on this computer";

/// A window that never leases: `--ephemeral`, `--capture`, `--no-lease`.
pub fn lease_exempt(ephemeral: bool, capture: bool, no_lease: bool) -> bool {
    ephemeral || capture || no_lease
}

/// The hosts a lease can exist for: those dialled over a local pipe.
pub fn pipe_hosts(connections: &[(HostKey, TransportConfig)]) -> Vec<HostKey> {
    connections
        .iter()
        .filter(|(_, c)| matches!(c.dial, Dial::Pipe(_)))
        .map(|(h, _)| h.clone())
        .collect()
}

/// Where one host's lease stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    Pending,
    Granted { state_root: Option<String> },
    Foreign,
    Undetermined,
    Unsupported,
    Unreached,
}

enum HolderCmd {
    Leave(LeaveIntent, oneshot::Sender<u32>),
    NoticeSeen(u32),
}

struct Slot {
    standing: Standing,
    holder: Option<mpsc::UnboundedSender<HolderCmd>>,
}

/// Every lease this window holds or wants, one slot per local host.
pub struct Leases {
    exempt: bool,
    slots: Mutex<HashMap<HostKey, Slot>>,
    /// `lease::LEASE_REPLY_WAIT` and `lease::NOTICE_ACK_WAIT`; tests shorten them.
    reply_wait: Duration,
    notice_wait: Duration,
}

impl Leases {
    pub fn new(exempt: bool, pipe_hosts: Vec<HostKey>) -> Arc<Self> {
        let slots = pipe_hosts
            .into_iter()
            .map(|h| (h, Slot { standing: Standing::Pending, holder: None }))
            .collect();
        Arc::new(Self {
            exempt,
            slots: Mutex::new(slots),
            reply_wait: lease::LEASE_REPLY_WAIT,
            notice_wait: lease::NOTICE_ACK_WAIT,
        })
    }

    /// Granted, and the holder task that owns the stream is still running.
    pub(crate) fn held(&self, host: &HostKey) -> bool {
        let slots = self.slots.lock().unwrap();
        slots.get(host).is_some_and(|s| {
            matches!(s.standing, Standing::Granted { .. })
                && s.holder.as_ref().is_some_and(|h| !h.is_closed())
        })
    }

    fn resolved(slot: &Slot) -> Standing {
        match (&slot.standing, &slot.holder) {
            (Standing::Granted { .. }, Some(h)) if h.is_closed() => Standing::Unreached,
            (s, _) => s.clone(),
        }
    }

    #[cfg(test)]
    fn standing(&self, host: &HostKey) -> Option<Standing> {
        self.slots.lock().unwrap().get(host).map(Self::resolved)
    }

    fn set(&self, host: &HostKey, standing: Standing, holder: Option<mpsc::UnboundedSender<HolderCmd>>) {
        self.slots.lock().unwrap().insert(host.clone(), Slot { standing, holder });
    }

    /// Claim the daemon on a dedicated connection before a data connection is
    /// made. Returns the count of sessions an earlier close could not end.
    /// An `Err` is the transport's cue to back off and retry.
    pub async fn before_data_connection(
        &self,
        host: &HostKey,
        path: &Path,
        token: Option<&str>,
    ) -> Result<u32> {
        if self.exempt || self.held(host) {
            return Ok(0);
        }
        let ident = match self_identity() {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(%host, error = %e, "window lease: cannot identify this process");
                self.set(host, Standing::Undetermined, None);
                return Ok(0);
            }
        };
        let req = FeLeaseReq { boot: ident.boot, pid: ident.pid, created: ident.created, token: token.map(str::to_string) };
        let attempt = tokio::time::timeout(self.reply_wait, async {
            let stream = connect_pipe(path).await?;
            let (rx, mut tx) = stream.split();
            let mut rx = codec::buffered(rx);
            let frame = Frame::req(1, op::FE_LEASE, serde_json::to_value(&req)?);
            codec::write_frame(&mut tx, &frame, None).await?;
            let (reply, _) = codec::read_frame(&mut rx).await?;
            Ok::<_, anyhow::Error>((rx, tx, reply))
        })
        .await;
        let (rx, tx, reply) = match attempt {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                self.set(host, Standing::Unreached, None);
                tracing::warn!(%host, error = %format_args!("{e:#}"), "window lease: not taken");
                return Err(e.context("window lease"));
            }
            Err(_) => {
                self.set(host, Standing::Unreached, None);
                tracing::warn!(%host, "window lease: no reply in time");
                return Err(anyhow!("no reply to the window lease in time").context("window lease"));
            }
        };
        let res: FeLeaseRes = match serde_json::from_value(reply.payload) {
            Ok(r) => r,
            Err(_) => {
                self.set(host, Standing::Unsupported, None);
                tracing::warn!(%host, "window lease: this backend does not know it");
                return Ok(0);
            }
        };
        match res.outcome {
            LeaseOutcome::Granted => {
                let holder = spawn_holder(rx, tx, self.notice_wait);
                self.set(host, Standing::Granted { state_root: res.state_root }, Some(holder));
                tracing::info!(%host, not_ended = res.not_ended, "window lease: granted");
                Ok(res.not_ended)
            }
            LeaseOutcome::Foreign => {
                self.set(host, Standing::Foreign, None);
                tracing::warn!(%host, "window lease: another user's backend");
                Ok(0)
            }
            LeaseOutcome::Undetermined => {
                self.set(host, Standing::Undetermined, None);
                tracing::warn!(%host, "window lease: the backend could not verify this window");
                Ok(0)
            }
            LeaseOutcome::Closing => {
                self.set(host, Standing::Unreached, None);
                tracing::warn!(%host, "window lease: the backend is shutting down");
                Err(anyhow!("this computer's backend is shutting down"))
            }
        }
    }

    /// The status-line notice for the current standings, if any.
    pub fn notice(&self) -> Option<&'static str> {
        let standings: Vec<Standing> =
            self.slots.lock().unwrap().values().map(Self::resolved).collect();
        lease_notice(self.exempt, &standings)
    }

    /// The state roots of every granted lease.
    pub fn granted_state_roots(&self) -> Vec<String> {
        self.slots
            .lock()
            .unwrap()
            .values()
            .filter_map(|s| match Self::resolved(s) {
                Standing::Granted { state_root } => state_root,
                _ => None,
            })
            .collect()
    }

    /// Tell the daemon the not-ended line for `host` has been shown.
    pub fn notice_seen(&self, host: &HostKey, n: u32) {
        let slots = self.slots.lock().unwrap();
        if let Some(h) = slots.get(host).and_then(|s| s.holder.as_ref()) {
            let _ = h.send(HolderCmd::NoticeSeen(n));
        }
    }

    /// Ask every held lease's daemon to end (or keep) its sessions. None when
    /// no lease is held: the window can exit at once.
    pub fn leave_all(&self, intent: LeaveIntent, exit_code: i32, now: Instant) -> Option<Leaving> {
        let mut pending = Vec::new();
        for (host, slot) in self.slots.lock().unwrap().iter() {
            if !matches!(Self::resolved(slot), Standing::Granted { .. }) {
                continue;
            }
            if let Some(h) = &slot.holder {
                let (tx, rx) = oneshot::channel();
                if h.send(HolderCmd::Leave(intent, tx)).is_ok() {
                    pending.push((host.clone(), rx));
                }
            }
        }
        if pending.is_empty() {
            None
        } else {
            Some(Leaving::new(intent, exit_code, pending, now))
        }
    }
}

/// How long the not-ended line holds before the window exits.
pub const NOT_ENDED_EXIT_HOLD: Duration = Duration::from_secs(3);
/// How often the window polls the acks while leaving.
pub const LEAVE_POLL: Duration = Duration::from_millis(250);

/// A window on its way out: waiting for the daemons' acks.
pub struct Leaving {
    pub intent: LeaveIntent,
    pub exit_code: i32,
    pending: Vec<(HostKey, oneshot::Receiver<u32>)>,
    acks: Vec<(HostKey, u32)>,
    deadline: Instant,
    shown: Option<u32>,
    hold_until: Option<Instant>,
}

/// What the event loop does next while leaving.
#[derive(Debug, PartialEq, Eq)]
pub enum LeaveStep {
    Wait(Instant),
    Show(Vec<(HostKey, u32)>),
    Exit,
}

impl Leaving {
    pub fn new(
        intent: LeaveIntent,
        exit_code: i32,
        pending: Vec<(HostKey, oneshot::Receiver<u32>)>,
        now: Instant,
    ) -> Self {
        let wait = match intent {
            LeaveIntent::Close => lease::CLOSE_ACK_WAIT,
            _ => lease::KEEP_ACK_WAIT,
        };
        Self { intent, exit_code, pending, acks: Vec::new(), deadline: now + wait, shown: None, hold_until: None }
    }

    pub fn poll(&mut self, now: Instant) -> LeaveStep {
        if let Some(t) = self.hold_until {
            return if now >= t { LeaveStep::Exit } else { LeaveStep::Wait(t) };
        }
        let mut still = Vec::new();
        for (host, mut rx) in std::mem::take(&mut self.pending) {
            match rx.try_recv() {
                Ok(n) => self.acks.push((host, n)),
                Err(TryRecvError::Closed) => self.acks.push((host, 0)),
                Err(TryRecvError::Empty) => still.push((host, rx)),
            }
        }
        self.pending = still;
        if !self.pending.is_empty() && now < self.deadline {
            return LeaveStep::Wait(self.deadline.min(now + LEAVE_POLL));
        }
        let nonzero: Vec<(HostKey, u32)> = self.acks.iter().filter(|(_, n)| *n > 0).cloned().collect();
        if nonzero.is_empty() {
            return LeaveStep::Exit;
        }
        self.shown = Some(nonzero.iter().map(|(_, n)| n).sum());
        self.hold_until = Some(now + NOT_ENDED_EXIT_HOLD);
        LeaveStep::Show(nonzero)
    }

    /// The status line while leaving: the not-ended count once shown,
    /// otherwise `closing…` (a handover has none).
    pub fn line(&self) -> Option<String> {
        match (self.shown, self.intent) {
            (Some(n), _) => not_ended_line(n),
            (None, LeaveIntent::Handover) => None,
            (None, _) => Some("closing…".to_string()),
        }
    }
}

/// The text for the standings, or None when closing will end the sessions
/// (or when it is too early to know).
pub fn lease_notice(exempt: bool, standings: &[Standing]) -> Option<&'static str> {
    if exempt
        || standings.iter().any(|s| matches!(s, Standing::Granted { .. } | Standing::Pending))
    {
        return None;
    }
    if standings.contains(&Standing::Undetermined) {
        Some(NOTICE_UNDETERMINED)
    } else if standings.contains(&Standing::Unsupported) {
        Some(NOTICE_UNSUPPORTED)
    } else {
        Some(NOTICE_NO_BACKEND)
    }
}

/// What the status line says about sessions an earlier close could not end.
pub fn not_ended_line(n: u32) -> Option<String> {
    match n {
        0 => None,
        1 => Some("1 session could not be ended and is still running".to_string()),
        n => Some(format!("{n} sessions could not be ended and are still running")),
    }
}

/// Own the stream for the process's life. A reader task forwards frames into
/// a channel so the holder's `select!` never cancels a read mid-line.
fn spawn_holder<R, W>(mut rx: R, mut tx: W, notice_wait: Duration) -> mpsc::UnboundedSender<HolderCmd>
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<HolderCmd>();
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Frame>();
    tokio::spawn(async move {
        while let Ok((f, _)) = codec::read_frame(&mut rx).await {
            if frame_tx.send(f).is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        let mut next_id: u64 = 2;
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    let id = next_id;
                    next_id += 1;
                    let (frame, ack, wait) = match cmd {
                        HolderCmd::Leave(intent, ack) => (
                            Frame::req(id, op::FE_LEAVING, serde_json::json!(FeLeavingReq { intent })),
                            Some(ack),
                            None,
                        ),
                        HolderCmd::NoticeSeen(n) => (
                            Frame::req(id, op::FE_NOTICE_SEEN, serde_json::json!(FeNoticeSeenReq { not_ended: n })),
                            None,
                            Some(notice_wait),
                        ),
                    };
                    if codec::write_frame(&mut tx, &frame, None).await.is_err() {
                        break;
                    }
                    let reply = async {
                        while let Some(f) = frame_rx.recv().await {
                            if f.id == id {
                                return Some(f);
                            }
                        }
                        None
                    };
                    // A slow reply is not a dead daemon: ending the holder here
                    // would close the stream, which the daemon reads as Close.
                    // Only a closed frame channel or a write error ends it.
                    let reply = match wait {
                        Some(w) => match tokio::time::timeout(w, reply).await {
                            Ok(r) => r,
                            Err(_) => {
                                tracing::warn!("window lease: no reply to fe.notice_seen in time");
                                continue;
                            }
                        },
                        None => reply.await,
                    };
                    let Some(reply) = reply else { break };
                    if let Some(ack) = ack {
                        let res: FeLeavingRes = serde_json::from_value(reply.payload).unwrap_or_default();
                        let _ = ack.send(res.not_ended);
                    }
                }
                f = frame_rx.recv() => {
                    if f.is_none() {
                        break;
                    }
                }
            }
        }
    });
    cmd_tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use interprocess::local_socket::{GenericFilePath, ListenerOptions};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn bind(tag: &str) -> (interprocess::local_socket::tokio::Listener, PathBuf) {
        let unique = format!(
            "sot-lease-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        #[cfg(windows)]
        let path = PathBuf::from(format!(r"\\.\pipe\{unique}"));
        #[cfg(not(windows))]
        let path = std::env::temp_dir().join(format!("{unique}.sock"));
        let _ = std::fs::remove_file(&path);
        let name = path.to_str().unwrap().to_fs_name::<GenericFilePath>().unwrap();
        let listener = ListenerOptions::new().name(name).create_tokio().expect("bind");
        (listener, path)
    }

    fn pipe_config(path: &Path) -> TransportConfig {
        TransportConfig { dial: Dial::Pipe(path.to_path_buf()), token: None }
    }

    #[test]
    fn harness_never_leases() {
        for bits in 0..8u8 {
            let (e, c, n) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
            assert_eq!(lease_exempt(e, c, n), bits != 0, "row {bits}");
        }
        let ssh = sot_protocol::ssh_bridge::SshRecipe::new("somehost", None).unwrap();
        let conns = vec![
            ("local".to_string(), pipe_config(Path::new("/x"))),
            ("far".to_string(), TransportConfig { dial: Dial::Ssh(ssh), token: None }),
        ];
        assert_eq!(pipe_hosts(&conns), vec!["local".to_string()]);

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (listener, path) = bind("exempt");
            let host = "local".to_string();
            let leases = Leases::new(true, vec![host.clone()]);
            assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
            let accepted = tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
            assert!(accepted.is_err(), "an exempt window must not connect");
            assert_eq!(leases.notice(), None);
        });
    }

    #[tokio::test]
    async fn lease_retried_each_data_connection() {
        let (listener, path) = bind("retry");
        let count = Arc::new(AtomicUsize::new(0));
        let (drop_tx, drop_rx) = oneshot::channel::<()>();
        let c = count.clone();
        tokio::spawn(async move {
            let mut drop_rx = Some(drop_rx);
            loop {
                let conn = listener.accept().await.unwrap();
                let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                let (rx, mut tx) = conn.split();
                let mut rx = codec::buffered(rx);
                let (req, _) = codec::read_frame(&mut rx).await.unwrap();
                let payload = match n {
                    1 => serde_json::json!({"error": "unknown op: fe.lease"}),
                    2 => serde_json::json!({"outcome": "closing"}),
                    3 => {
                        // No reply: hold the connection open past the wait.
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            drop((rx, tx));
                        });
                        continue;
                    }
                    4 => serde_json::json!({"outcome": "foreign"}),
                    5 => serde_json::json!({"outcome": "undetermined"}),
                    _ => serde_json::json!({"outcome": "granted", "state_root": "r"}),
                };
                codec::write_frame(&mut tx, &Frame::res(req.id, op::FE_LEASE, payload), None)
                    .await
                    .unwrap();
                if n == 6 {
                    let rx_drop = drop_rx.take().unwrap();
                    tokio::spawn(async move {
                        let _ = rx_drop.await;
                        drop((rx, tx));
                    });
                } else if n > 6 {
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        drop((rx, tx));
                    });
                }
            }
        });
        let host = "local".to_string();
        let mut leases = Leases::new(false, vec![host.clone()]);
        Arc::get_mut(&mut leases).unwrap().reply_wait = Duration::from_millis(100);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(leases.standing(&host), Some(Standing::Unsupported));
        // Closing, and no reply in time: each a failed connect.
        assert!(leases.before_data_connection(&host, &path, None).await.is_err());
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(leases.standing(&host), Some(Standing::Unreached));
        assert!(leases.before_data_connection(&host, &path, None).await.is_err());
        assert_eq!(count.load(Ordering::SeqCst), 3);
        assert_eq!(leases.standing(&host), Some(Standing::Unreached));
        // Foreign and Undetermined proceed unleased, and the next connection asks again.
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        assert_eq!(count.load(Ordering::SeqCst), 4);
        assert_eq!(leases.standing(&host), Some(Standing::Foreign));
        assert!(!leases.held(&host));
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        assert_eq!(count.load(Ordering::SeqCst), 5);
        assert_eq!(leases.standing(&host), Some(Standing::Undetermined));
        assert!(!leases.held(&host));
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        assert_eq!(count.load(Ordering::SeqCst), 6);
        assert_eq!(leases.standing(&host), Some(Standing::Granted { state_root: Some("r".into()) }));
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        assert_eq!(count.load(Ordering::SeqCst), 6, "a held lease is not retaken");
        drop_tx.send(()).unwrap();
        for _ in 0..40 {
            if !leases.held(&host) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!leases.held(&host), "the dropped stream ends the lease");
        leases.before_data_connection(&host, &path, None).await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 7);
    }

    #[tokio::test]
    async fn notice_seen_timeout_keeps_the_lease() {
        let (listener, path) = bind("slowseen");
        let (eof_tx, eof_rx) = oneshot::channel::<bool>();
        tokio::spawn(async move {
            let conn = listener.accept().await.unwrap();
            let (rx, mut tx) = conn.split();
            let mut rx = codec::buffered(rx);
            let (req, _) = codec::read_frame(&mut rx).await.unwrap();
            let granted = serde_json::json!({"outcome": "granted"});
            codec::write_frame(&mut tx, &Frame::res(req.id, op::FE_LEASE, granted), None).await.unwrap();
            let (seen, _) = codec::read_frame(&mut rx).await.unwrap();
            assert_eq!(seen.op, op::FE_NOTICE_SEEN);
            // Never answer; report whether the window's end of the stream closes.
            let eof = tokio::time::timeout(Duration::from_millis(500), codec::read_frame(&mut rx)).await.is_ok();
            let _ = eof_tx.send(eof);
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(tx);
        });
        let host = "local".to_string();
        let mut leases = Leases::new(false, vec![host.clone()]);
        Arc::get_mut(&mut leases).unwrap().notice_wait = Duration::from_millis(50);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        leases.notice_seen(&host, 2);
        assert!(!eof_rx.await.unwrap(), "the fake daemon read an EOF after a slow notice_seen reply");
        assert!(leases.held(&host), "a slow notice_seen reply must not end the lease");
    }

    #[tokio::test]
    async fn lease_wire_round_trip() {
        let (listener, path) = bind("wire");
        let (seen_tx, seen_rx) = oneshot::channel::<Frame>();
        let (leaving_tx, leaving_rx) = oneshot::channel::<Frame>();
        tokio::spawn(async move {
            let conn = listener.accept().await.unwrap();
            let (rx, mut tx) = conn.split();
            let mut rx = codec::buffered(rx);
            let (req, _) = codec::read_frame(&mut rx).await.unwrap();
            assert_eq!(req.op, op::FE_LEASE);
            assert_eq!(req.id, 1);
            let me = self_identity().unwrap();
            assert_eq!(req.payload["boot"], serde_json::json!(me.boot));
            assert_eq!(req.payload["pid"], serde_json::json!(me.pid));
            assert_eq!(req.payload["created"], serde_json::json!(me.created));
            assert_eq!(req.payload["token"], serde_json::json!("tok"));
            let granted = serde_json::json!({"outcome": "granted", "state_root": "r", "not_ended": 1});
            codec::write_frame(&mut tx, &Frame::res(1, op::FE_LEASE, granted), None).await.unwrap();
            let (seen, _) = codec::read_frame(&mut rx).await.unwrap();
            codec::write_frame(&mut tx, &Frame::res(seen.id, op::FE_NOTICE_SEEN, serde_json::json!({})), None)
                .await
                .unwrap();
            let _ = seen_tx.send(seen);
            let (leaving, _) = codec::read_frame(&mut rx).await.unwrap();
            let _ = leaving_tx.send(leaving.clone());
            codec::write_frame(&mut tx, &Frame::res(leaving.id, op::FE_LEAVING, serde_json::json!({"not_ended": 3})), None)
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let host = "local".to_string();
        let leases = Leases::new(false, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, Some("tok")).await.unwrap(), 1);
        assert_eq!(leases.granted_state_roots(), vec!["r".to_string()]);
        leases.notice_seen(&host, 1);
        let seen = tokio::time::timeout(Duration::from_secs(2), seen_rx).await.unwrap().unwrap();
        assert_eq!(seen.op, op::FE_NOTICE_SEEN);
        assert_eq!(seen.payload, serde_json::json!({"not_ended": 1}));
        let mut leaving = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(2), leaving_rx).await.unwrap().unwrap();
        assert_eq!(frame.op, op::FE_LEAVING);
        assert_eq!(frame.payload, serde_json::json!({"intent": "close"}));
        let mut step = leaving.poll(Instant::now());
        for _ in 0..40 {
            if !matches!(step, LeaveStep::Wait(_)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            step = leaving.poll(Instant::now());
        }
        assert_eq!(step, LeaveStep::Show(vec![(host.clone(), 3)]));
    }

    /// A daemon that grants, logs the leave frame and then `eof` when the
    /// stream ends, and replies to the leave after `delay` (never when None).
    fn leave_fake(
        listener: interprocess::local_socket::tokio::Listener,
        delay: Option<Duration>,
        not_ended: u32,
    ) -> (Arc<std::sync::Mutex<Vec<String>>>, Arc<std::sync::atomic::AtomicBool>) {
        let log = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let replied = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (l, r) = (log.clone(), replied.clone());
        tokio::spawn(async move {
            let conn = listener.accept().await.unwrap();
            let (rx, mut tx) = conn.split();
            let mut rx = codec::buffered(rx);
            let (req, _) = codec::read_frame(&mut rx).await.unwrap();
            let granted = serde_json::json!({"outcome": "granted"});
            codec::write_frame(&mut tx, &Frame::res(req.id, op::FE_LEASE, granted), None).await.unwrap();
            let (leaving, _) = codec::read_frame(&mut rx).await.unwrap();
            l.lock().unwrap().push(format!("{} {}", leaving.op, leaving.payload));
            if let Some(d) = delay {
                tokio::time::sleep(d).await;
                r.store(true, Ordering::SeqCst);
                let res = serde_json::json!({"not_ended": not_ended});
                codec::write_frame(&mut tx, &Frame::res(leaving.id, op::FE_LEAVING, res), None).await.unwrap();
            }
            while codec::read_frame(&mut rx).await.is_ok() {}
            l.lock().unwrap().push("eof".to_string());
        });
        (log, replied)
    }

    /// Poll the way the event loop does until the step is not a wait.
    async fn poll_out(leaving: &mut Leaving) -> LeaveStep {
        loop {
            let step = leaving.poll(Instant::now());
            if !matches!(step, LeaveStep::Wait(_)) {
                return step;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn leave_is_logged(log: &Arc<std::sync::Mutex<Vec<String>>>) {
        for _ in 0..100 {
            if !log.lock().unwrap().is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn keep_reaches_the_daemon_before_eof() {
        let (listener, path) = bind("keepleave");
        let (log, replied) = leave_fake(listener, Some(Duration::from_millis(300)), 0);
        let host = "local".to_string();
        let leases = Leases::new(false, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        let mut leaving = leases.leave_all(LeaveIntent::Keep, 0, Instant::now()).unwrap();
        assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)));
        leave_is_logged(&log).await;
        {
            let seen = log.lock().unwrap();
            assert_eq!(seen.len(), 1, "the leave line reaches the daemon before any eof: {seen:?}");
            assert!(seen[0].starts_with(op::FE_LEAVING) && seen[0].contains(r#""intent":"keep""#), "{seen:?}");
        }
        assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "no reply yet");
        assert_eq!(poll_out(&mut leaving).await, LeaveStep::Exit);
        assert!(replied.load(Ordering::SeqCst), "done only after the reply");
        drop((leaving, leases));
    }

    #[tokio::test]
    async fn close_waits_for_its_reply() {
        let (listener, path) = bind("closeleave");
        let (log, replied) = leave_fake(listener, Some(Duration::from_millis(300)), 0);
        let host = "local".to_string();
        let leases = Leases::new(false, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        let mut leaving = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
        assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)));
        leave_is_logged(&log).await;
        {
            let seen = log.lock().unwrap();
            assert_eq!(seen.len(), 1, "the leave line reaches the daemon before any eof: {seen:?}");
            assert!(seen[0].starts_with(op::FE_LEAVING) && seen[0].contains(r#""intent":"close""#), "{seen:?}");
        }
        assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "no reply yet");
        assert_eq!(poll_out(&mut leaving).await, LeaveStep::Exit);
        assert!(replied.load(Ordering::SeqCst), "done only after the reply");
        drop((leaving, leases));

        // A withheld reply ends the wait only at the close-ack bound (shortened here).
        let (listener, path) = bind("closenoreply");
        let (_log, replied) = leave_fake(listener, None, 0);
        let leases = Leases::new(false, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        let start = Instant::now();
        let mut leaving = leases.leave_all(LeaveIntent::Close, 0, start).unwrap();
        leaving.deadline = start + Duration::from_millis(400);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "waits inside the bound");
        assert_eq!(poll_out(&mut leaving).await, LeaveStep::Exit);
        assert!(start.elapsed() >= Duration::from_millis(400), "done only once the bound passed");
        assert!(!replied.load(Ordering::SeqCst));
    }

    // PIN, NOT FAIL-FIRST: interprocess 2.4.2 already creates the socket
    // SOCK_CLOEXEC (os/unix/c_wrappers.rs:177), so this passed on its first
    // run. On Windows the pipe client handle is non-inheritable because
    // CreateFileW is given null security attributes
    // (os/windows/named_pipe/c_wrappers.rs:158-165); there is no Windows half.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn lease_stream_not_inherited() {
        fn sockets() -> std::collections::HashSet<String> {
            std::fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
                .map(|p| p.to_string_lossy().into_owned())
                .filter(|p| p.starts_with("socket:"))
                .collect()
        }
        let (_listener, path) = bind("cloexec");
        let before = sockets();
        let _stream = connect_pipe(&path).await.unwrap();
        let ours: Vec<String> = sockets().difference(&before).cloned().collect();
        assert!(!ours.is_empty(), "the connect must have opened a socket");
        let out = std::process::Command::new("sh")
            .args(["-c", "for f in /proc/self/fd/*; do readlink $f; done"])
            .output()
            .unwrap();
        let seen = String::from_utf8_lossy(&out.stdout);
        for s in &ours {
            assert!(!seen.contains(s.as_str()), "{s} leaked into a child");
        }
    }

    #[test]
    fn no_lease_status_line() {
        use Standing::*;
        let granted = || Granted { state_root: None };
        assert_eq!(lease_notice(true, &[Foreign]), None);
        assert_eq!(lease_notice(true, &[]), None);
        assert_eq!(lease_notice(false, &[granted()]), None);
        assert_eq!(lease_notice(false, &[granted(), Foreign]), None);
        assert_eq!(lease_notice(false, &[Pending]), None);
        assert_eq!(lease_notice(false, &[Undetermined]), Some(NOTICE_UNDETERMINED));
        assert_eq!(lease_notice(false, &[Unsupported]), Some(NOTICE_UNSUPPORTED));
        assert_eq!(lease_notice(false, &[Foreign]), Some(NOTICE_NO_BACKEND));
        assert_eq!(lease_notice(false, &[Unreached]), Some(NOTICE_NO_BACKEND));
        assert_eq!(lease_notice(false, &[]), Some(NOTICE_NO_BACKEND));
        assert_eq!(lease_notice(false, &[Undetermined, Unsupported]), Some(NOTICE_UNDETERMINED));
        // A granted slot whose holder has ended reads as unreached.
        let (tx, rx) = mpsc::unbounded_channel::<HolderCmd>();
        drop(rx);
        let leases = Leases::new(false, vec!["h".to_string()]);
        leases.set(&"h".to_string(), granted(), Some(tx));
        assert_eq!(leases.standing(&"h".to_string()), Some(Unreached));
        assert_eq!(leases.notice(), Some(NOTICE_NO_BACKEND));
    }

    #[test]
    fn not_ended_status_line() {
        assert_eq!(not_ended_line(0), None);
        assert_eq!(not_ended_line(1).unwrap(), "1 session could not be ended and is still running");
        assert_eq!(not_ended_line(3).unwrap(), "3 sessions could not be ended and are still running");

        let h = "h".to_string();
        let t0 = Instant::now();
        let s = Duration::from_secs;

        let (tx, rx) = oneshot::channel();
        let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(h.clone(), rx)], t0);
        assert_eq!(l.line().as_deref(), Some("closing…"));
        tx.send(2).unwrap();
        assert_eq!(l.poll(t0), LeaveStep::Show(vec![(h.clone(), 2)]));
        assert_eq!(l.line().unwrap(), "2 sessions could not be ended and are still running");
        assert_eq!(l.poll(t0 + s(1)), LeaveStep::Wait(t0 + s(3)));
        assert_eq!(l.poll(t0 + s(3)), LeaveStep::Exit);

        let (tx, rx) = oneshot::channel();
        let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(h.clone(), rx)], t0);
        tx.send(0).unwrap();
        assert_eq!(l.poll(t0), LeaveStep::Exit);

        let (tx, rx) = oneshot::channel::<u32>();
        let mut l = Leaving::new(LeaveIntent::Keep, 0, vec![(h.clone(), rx)], t0);
        drop(tx);
        assert_eq!(l.poll(t0), LeaveStep::Exit);

        let (_tx, rx) = oneshot::channel::<u32>();
        let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(h.clone(), rx)], t0);
        assert_eq!(l.poll(t0 + s(124)), LeaveStep::Wait(t0 + s(124) + LEAVE_POLL));
        assert_eq!(l.poll(t0 + s(125)), LeaveStep::Exit);

        let (_tx, rx) = oneshot::channel::<u32>();
        let mut l = Leaving::new(LeaveIntent::Keep, 0, vec![(h.clone(), rx)], t0);
        assert_eq!(l.line().as_deref(), Some("closing…"));
        assert_eq!(l.poll(t0 + s(10)), LeaveStep::Exit);

        let l = Leaving::new(LeaveIntent::Handover, 75, vec![], t0);
        assert_eq!(l.line(), None);
    }
}
