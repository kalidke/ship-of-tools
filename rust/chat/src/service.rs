//! The chat service: the one running listener for the one configured workspace, its Slack API, a status and a bounded
//! stop. The workspace's lock is taken before any socket opens or any call is made, and it is owned by the listener's
//! task, so it is released only when no listener can still be running.

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sot_platform::host::lock_supervisor;
use sot_platform::Error as PlatformError;

use crate::api::{Api, ApiError};
use crate::config::{Config, Credentials};
use crate::http::{Http, Secret};
use crate::socket::{self, Socket, SocketError, Timing, BASE};

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
    /// `gap`: a reconciliation is owed (every hello owes one, and nothing reconciles until the watch lands).
    Running { socket: socket::Status, gap: bool },
    /// Stop was asked and has not returned: the listener may still be running, and the lock is held.
    Stopping,
    Stopped,
}

enum Phase {
    Running(Socket),
    Stopping,
    Stopped,
}

pub struct Service {
    api: Arc<Api>,
    phase: Mutex<Phase>,
}

/// The lock file's name: built from the validated workspace id alone.
fn lock_name(workspace: &str) -> String {
    format!("chat-{workspace}.lock")
}

impl Service {
    /// Take the workspace's lock in `dir` (a private folder the caller owns), then read the bot's identity, then
    /// listen. A second service for the workspace is `Held` before any socket opens or any call is made. Opens no
    /// secret file: the caller reads `Credentials`.
    pub async fn start(
        config: &Config,
        credentials: Credentials,
        http: Http,
        dir: &Path,
    ) -> Result<Service, ServiceError> {
        let api = Api::new(http.clone(), credentials.bot, config.workspace()).map_err(ServiceError::Api)?;
        Service::launch(config.workspace(), credentials.app, api, http, dir, BASE.into(), Timing::PRODUCT).await
    }

    #[cfg(test)]
    pub(crate) async fn start_for_test(
        config: &Config,
        credentials: Credentials,
        http: Http,
        dir: &Path,
        base: String,
        timing: Timing,
    ) -> Result<Service, ServiceError> {
        let api = Api::new(http.clone(), credentials.bot, config.workspace())
            .map_err(ServiceError::Api)?
            .with_base(base.clone());
        Service::launch(config.workspace(), credentials.app, api, http, dir, base, timing).await
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
        // From here an early return drops the lock; the socket, once started, owns it.
        let own = api.auth_test().await.map_err(ServiceError::Api)?;
        let socket = Socket::launch(http, app, own, workspace, base, timing, Box::new(lock)).map_err(ServiceError::Socket)?;
        Ok(Service { api: Arc::new(api), phase: Mutex::new(Phase::Running(socket)) })
    }

    /// The Slack API, or `Stopped`. A call already begun runs to its own deadline; a mutation already sent is neither
    /// cancelled nor resent.
    pub fn api(&self) -> Result<Arc<Api>, ServiceError> {
        match *self.phase.lock().unwrap() {
            Phase::Running(_) => Ok(self.api.clone()),
            _ => Err(ServiceError::Stopped),
        }
    }

    pub fn status(&self) -> Status {
        match &*self.phase.lock().unwrap() {
            Phase::Running(s) => Status::Running { socket: s.status(), gap: s.gap().is_some() },
            Phase::Stopping => Status::Stopping,
            Phase::Stopped => Status::Stopped,
        }
    }

    /// End the listener. The call that begins the stop closes the API for every handle, then returns after the
    /// task is gone (ended, or aborted and awaited once `within` passes) and the lock is released. Any other call,
    /// including one made while that stop is waiting, returns at once. A stop future dropped before it finishes
    /// leaves the status `Stopping`; dropping it drops the socket, which aborts the task.
    pub async fn stop(&self, within: Duration) {
        let socket = {
            let mut phase = self.phase.lock().unwrap();
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
}
