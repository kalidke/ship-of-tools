//! The chat service: the one running listener for one workspace, its Slack API, a status and a bounded
//! stop. The workspace's lock is taken before any socket opens or any call is made, and it is owned by the listener's
//! task, so it is released only when no listener can still be running. A service also ends on its own, for good, when
//! its tokens are refused or its socket is another app's (`End`).

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::watch;

use sot_platform::host::lock_supervisor;
use sot_platform::Error as PlatformError;

use crate::api::{Api, ApiError, BotApp, Transfers};
use crate::config::Credentials;
use crate::http::{Http, Secret};
use crate::socket::{self, Gap, Socket, SocketError, Taken, Timing, Waiter, BASE};
use crate::watch::Watches;

/// Why a service did not start, or why a call was refused. Fixed codes: no path, token or Slack text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceError {
    /// Another service holds this workspace's lock: nothing was opened and nothing was sent.
    Held,
    /// The lock could not be made or taken for another reason.
    Lock,
    /// `auth.test` failed, a token naming another workspace (`wrong_workspace`) included.
    Api(ApiError),
    /// The listener could not start.
    Socket(SocketError),
    /// The service was stopped.
    Stopped,
}

impl ServiceError {
    pub fn code(&self) -> &'static str {
        match self {
            ServiceError::Held => "held",
            ServiceError::Lock => "lock",
            ServiceError::Api(e) => e.code(),
            ServiceError::Socket(e) => e.code(),
            ServiceError::Stopped => "stopped",
        }
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for ServiceError {}

/// A service that exists is running or on its way to stopped; one that was refused or never started is `start`'s error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// `gap`: a reconciliation is owed (every hello owes one; `Watches::reconcile` clears it).
    Running { socket: socket::Status, gap: bool },
    /// Stop was asked and has not returned: the listener may still be running, and the lock is held.
    Stopping,
    Stopped,
}

/// The claim's states: no `Watches` made, one alive, the service no longer running.
pub(crate) const FREE: u8 = 0;
pub(crate) const HELD: u8 = 1;
pub(crate) const ENDED: u8 = 2;

// The ends a service comes to on its own (`Service::ended`): fixed codes for the daemon to log and act on.
/// A `hello` or an envelope named another app than the bot's: the app-level token is another app's.
pub const WRONG_APP: &str = "wrong_app";
/// Slack refused `apps.connections.open` as a bad app-level token.
pub const APP_TOKEN_REFUSED: &str = "app_token_refused";
/// Slack refused a bot-token call with a code of `api::TOKEN_REFUSALS`.
pub const BOT_TOKEN_REFUSED: &str = "bot_token_refused";

/// A service's end of its own: the first code, never undone, seen by its `Api`, its listener and every caller. It ends
/// the claim, so every channel of the service's `Watches` owes a snapshot, and raises the listener's stop signal, so
/// its task ends and releases the lock. The claim is the one arbiter: whichever of a stop and an end marks it first
/// wins, so a stop that began first is no end.
pub(crate) struct End {
    code: OnceLock<&'static str>,
    claim: Arc<AtomicU8>,
    /// The listener's one stop signal: a stop and an end both raise it.
    signal: watch::Sender<bool>,
}

impl End {
    pub(crate) fn new(claim: Arc<AtomicU8>) -> Arc<End> {
        Arc::new(End { code: OnceLock::new(), claim, signal: watch::channel(false).0 })
    }

    /// An end no service reads: an `Api` or a `Socket` made alone.
    pub(crate) fn detached() -> Arc<End> {
        End::new(Arc::new(AtomicU8::new(FREE)))
    }

    pub(crate) fn end(&self, code: &'static str) {
        if self.claim.swap(ENDED, Ordering::SeqCst) != ENDED {
            let _ = self.code.set(code);
            self.signal.send_replace(true);
        }
    }

    pub(crate) fn code(&self) -> Option<&'static str> {
        self.code.get().copied()
    }

    /// Raise the stop signal with no end.
    pub(crate) fn stop(&self) {
        self.signal.send_replace(true);
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<bool> {
        self.signal.subscribe()
    }
}

enum Phase {
    Running(Socket),
    Stopping,
    Stopped,
}

pub struct Service {
    api: Arc<Api>,
    phase: Mutex<Phase>,
    /// Set while a `Watches` made by `watches` lives: the one reader of this service's events.
    claim: Arc<AtomicU8>,
    end: Arc<End>,
    /// Why the bot's app is unknown, so no frame's app is checked (`app_unchecked`).
    unchecked: Option<&'static str>,
    /// The bot's app as `bots.info` gave it at the start, its id and its name (`app`).
    app: Option<BotApp>,
    /// Why the person's user token was dropped at the start (`person_dropped`).
    dropped: Option<&'static str>,
}

/// The lock file's name: built from the validated workspace id alone.
fn lock_name(workspace: &str) -> String {
    format!("chat-{workspace}.lock")
}

impl Service {
    /// Take `workspace`'s lock in `dir` (a private folder the caller owns), then read the bot's identity and its app and
    /// check the person's user token, then listen. A second service for the workspace is `Held` before any socket opens or
    /// any call is made. Its file transfers draw on `transfers`, which the caller shares among its services. Opens no
    /// secret file: the caller reads `Credentials`.
    pub async fn start(
        workspace: &str,
        credentials: Credentials,
        http: Http,
        dir: &Path,
        transfers: Transfers,
    ) -> Result<Service, ServiceError> {
        let (app, api) = Service::api_of(workspace, credentials, &http)?;
        Service::launch(workspace, app, api.with_transfers(transfers), http, dir, BASE.into(), Timing::PRODUCT).await
    }

    /// `start` against the fixture's base URL (`FakeSlack::url("slack.com", "/api/")`), with the product's timing.
    /// Exists under `cfg(test)` or the `test-support` feature alone, like `Http::for_test`; an `Http` made by
    /// `Http::new` still refuses every host that is not Slack's and every port but 443.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn start_at(
        workspace: &str,
        credentials: Credentials,
        http: Http,
        dir: &Path,
        base: String,
        transfers: Transfers,
    ) -> Result<Service, ServiceError> {
        Service::start_for_test(workspace, credentials, http, dir, base, Timing::PRODUCT, transfers).await
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) async fn start_for_test(
        workspace: &str,
        credentials: Credentials,
        http: Http,
        dir: &Path,
        base: String,
        timing: Timing,
        transfers: Transfers,
    ) -> Result<Service, ServiceError> {
        let (app, api) = Service::api_of(workspace, credentials, &http)?;
        Service::launch(workspace, app, api.with_base(base.clone()).with_transfers(transfers), http, dir, base, timing).await
    }

    /// The one place the credentials become an `Api` (the bot's token and the person's), and the app-level token left
    /// for the listener. The product's start and the test's both go through it.
    fn api_of(workspace: &str, credentials: Credentials, http: &Http) -> Result<(Secret, Api), ServiceError> {
        let api = Api::new(http.clone(), credentials.bot, workspace)
            .map_err(ServiceError::Api)?
            .with_person(credentials.user);
        Ok((credentials.app, api))
    }

    async fn launch(
        workspace: &str,
        app: Secret,
        api: Api,
        http: Http,
        dir: &Path,
        base: String,
        timing: Timing,
    ) -> Result<Service, ServiceError> {
        let path = dir.join(lock_name(workspace));
        // The take waits up to platform's short retry, so it runs off the runtime's threads.
        let lock = tokio::task::spawn_blocking(move || lock_supervisor(&path))
            .await
            .map_err(|_| ServiceError::Lock)?
            .map_err(|e| match e {
                // Platform's only `State` refusal here is a lock another holder keeps.
                PlatformError::State(_) => ServiceError::Held,
                _ => ServiceError::Lock,
            })?;
        let claim = Arc::new(AtomicU8::new(FREE));
        let end = End::new(claim.clone());
        let api = api.with_end(end.clone());
        // From here an early return drops the lock; the socket, once started, owns it.
        let mut own = api.auth_test().await.map_err(ServiceError::Api)?;
        // The bot's app, which every frame's is checked against; unknown, nothing is checked (fails open). A window names
        // the connected app by its name and opens its page by its id.
        let (unchecked, bot_app) = match api.bot_app().await {
            Ok(bot) => {
                own.app_id = Some(bot.id.clone());
                (None, Some(bot))
            }
            Err(e) => (Some(e.code()), None),
        };
        let dropped = api.check_person().await;
        let socket =
            Socket::launch(http, app, own, workspace, base, timing, Box::new(lock), end.clone()).map_err(ServiceError::Socket)?;
        Ok(Service { api: Arc::new(api), phase: Mutex::new(Phase::Running(socket)), claim, end, unchecked, app: bot_app, dropped })
    }

    /// The workspace this service serves.
    pub fn workspace(&self) -> &str {
        self.api.workspace()
    }

    /// The workspace's name as the start's `auth.test` gave it, whether the service still runs or not.
    pub fn workspace_name(&self) -> Option<String> {
        self.api.workspace_name()
    }

    /// Why the person's user token was dropped at the start, a fixed code (`user_token_other_workspace`,
    /// `user_token_refused`), for the daemon to log; `None` when it is held or none was given.
    pub fn person_dropped(&self) -> Option<&'static str> {
        self.dropped
    }

    /// The first end this service came to on its own (`WRONG_APP`, `APP_TOKEN_REFUSED`, `BOT_TOKEN_REFUSED`), for
    /// good; an ended service answers as a stopped one. `None` for one that runs or was stopped.
    pub fn ended(&self) -> Option<&'static str> {
        self.end.code()
    }

    /// Why the bot's app is unknown (`bots.info`'s failure as `ApiError::code`, `no_identity` when `auth.test` named no
    /// bot), so no frame's app is checked; `None` when it is known.
    pub fn app_unchecked(&self) -> Option<&'static str> {
        self.unchecked
    }

    /// The bot's app as `bots.info` gave it at the start; `None` when `bots.info` failed (`app_unchecked` says why).
    pub fn app(&self) -> Option<BotApp> {
        self.app.clone()
    }

    /// Running, and not ended on its own.
    fn live<'a>(&self, phase: &'a Phase) -> Option<&'a Socket> {
        match phase {
            Phase::Running(s) if self.end.code().is_none() => Some(s),
            _ => None,
        }
    }

    /// The Slack API, or `Stopped`. A call already begun runs to its own deadline; a mutation already sent is neither
    /// cancelled nor resent.
    pub fn api(&self) -> Result<Arc<Api>, ServiceError> {
        match self.live(&self.phase.lock().unwrap()) {
            Some(_) => Ok(self.api.clone()),
            None => Err(ServiceError::Stopped),
        }
    }

    /// The one `Watches` of this service, or none while another made here is alive: a second would take events the
    /// first never sees and hold stale content as complete. The claim is released when the `Watches` is dropped, and
    /// never after a stop: a stopped service has no `Watches` to give, and the one it gave reads every channel as
    /// owing a snapshot.
    pub fn watches(&self) -> Option<Watches> {
        let free = self.claim.compare_exchange(FREE, HELD, Ordering::SeqCst, Ordering::SeqCst).is_ok();
        free.then(|| Watches::claimed(self.claim.clone()))
    }

    /// Whether `by` is this service's `Watches`.
    pub(crate) fn owns(&self, by: &Watches) -> bool {
        by.claims(&self.claim)
    }

    /// The wake, for this service's `Watches` while running; `Watches::wait` is its one caller, so one wait is ever in
    /// flight.
    pub(crate) fn waiter(&self, by: &Watches) -> Option<Waiter> {
        self.live(&self.phase.lock().unwrap()).filter(|_| self.owns(by)).map(Socket::waiter)
    }

    pub(crate) fn running(&self) -> bool {
        self.live(&self.phase.lock().unwrap()).is_some()
    }

    /// The socket's gap, for `Watches::loaded`; none once stopped or ended.
    pub fn gap(&self) -> Option<Gap> {
        self.live(&self.phase.lock().unwrap()).and_then(Socket::gap)
    }

    /// `Socket::take` for the `Watches` this service made, which takes and applies in one call; empty for any other
    /// and once the service is no longer running.
    pub(crate) fn take(&self, by: &Watches) -> Taken {
        if !self.owns(by) {
            return Taken::default();
        }
        self.live(&self.phase.lock().unwrap()).map(Socket::take).unwrap_or_default()
    }

    /// `Socket::reconciled`, for `Watches::reconcile`; a service that is stopping, stopped or ended clears nothing.
    pub(crate) fn reconciled(&self, gap: Gap) -> bool {
        self.live(&self.phase.lock().unwrap()).is_some_and(|s| s.reconciled(gap))
    }

    /// An ended service is `Stopping` until its listener's task is gone (the lock with it), then `Stopped`.
    pub fn status(&self) -> Status {
        match &*self.phase.lock().unwrap() {
            Phase::Running(s) if self.end.code().is_some() => {
                if s.finished() {
                    Status::Stopped
                } else {
                    Status::Stopping
                }
            }
            Phase::Running(s) => Status::Running { socket: s.status(), gap: s.gap().is_some() },
            Phase::Stopping => Status::Stopping,
            Phase::Stopped => Status::Stopped,
        }
    }

    /// End the listener. The call that begins the stop closes the API for every handle, then returns after the
    /// task is gone (ended, or aborted and awaited once `within` passes) and the lock is released. Any other call,
    /// including one made while that stop is waiting, returns at once. A stop future dropped before it finishes
    /// leaves the status `Stopping`, and dropping it aborts the task, so the lock is released with it.
    pub async fn stop(&self, within: Duration) {
        let socket = {
            let mut phase = self.phase.lock().unwrap();
            // First, so an end that comes after the stop began is no end (`End::end`). Every channel of the service's
            // `Watches` reads as owing a snapshot from here on.
            self.claim.swap(ENDED, Ordering::SeqCst);
            match std::mem::replace(&mut *phase, Phase::Stopping) {
                Phase::Running(s) => s,
                other => {
                    *phase = other;
                    return;
                }
            }
        };
        self.api.close();
        socket.stop_within(within).await;
        *self.phase.lock().unwrap() = Phase::Stopped;
    }

    /// A service over a socket with no task, for the test to push events and latch gaps into.
    #[cfg(test)]
    pub(crate) fn inert(api: Arc<Api>) -> Service {
        let claim = Arc::new(AtomicU8::new(FREE));
        let end = End::new(claim.clone());
        Service { api, phase: Mutex::new(Phase::Running(Socket::inert())), claim, end, unchecked: None, app: None, dropped: None }
    }

    #[cfg(test)]
    pub(crate) fn feed(&self, event: crate::normalize::Normalized) {
        if let Phase::Running(s) = &*self.phase.lock().unwrap() {
            s.feed(event);
        }
    }

    #[cfg(test)]
    pub(crate) fn lose(&self, cause: socket::GapCause) {
        if let Phase::Running(s) = &*self.phase.lock().unwrap() {
            s.lose(cause);
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.claim.store(ENDED, Ordering::SeqCst);
    }
}
