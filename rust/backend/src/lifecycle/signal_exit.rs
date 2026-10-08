//! Checked Unix signal installation on an independent runtime thread, before daemon child work.

#[cfg(not(unix))]
pub(crate) fn install() -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn install() -> std::io::Result<()> {
    unix::install_with(|code| super::shutdown::exit(code), unix::Ops::default())
}

#[cfg(unix)]
mod unix {
    use std::io;
    use std::sync::mpsc;
    use tokio::signal::unix::{signal, SignalKind};

    #[derive(Clone, Default)]
    pub(super) struct Ops {
        #[cfg(test)]
        pub(super) fail: Option<&'static str>,
    }
    impl Ops {
        fn before(&self, stage: &'static str) -> io::Result<()> {
            #[cfg(test)]
            if self.fail == Some(stage) {
                return Err(io::Error::other(format!("injected {stage} failure")));
            }
            let _ = stage;
            Ok(())
        }
        fn set(&self) -> io::Result<libc::sigset_t> {
            self.before("set")?;
            let mut set = unsafe { std::mem::zeroed() };
            for (name, code) in [
                ("sigemptyset", unsafe { libc::sigemptyset(&mut set) }),
                ("SIGINT sigaddset", unsafe {
                    libc::sigaddset(&mut set, libc::SIGINT)
                }),
                ("SIGTERM sigaddset", unsafe {
                    libc::sigaddset(&mut set, libc::SIGTERM)
                }),
            ] {
                if code != 0 {
                    return Err(io::Error::other(format!(
                        "{name}: {}",
                        io::Error::last_os_error()
                    )));
                }
            }
            Ok(set)
        }
        fn mask(
            &self,
            stage: &'static str,
            how: i32,
            set: *const libc::sigset_t,
            old: *mut libc::sigset_t,
        ) -> io::Result<()> {
            self.before(stage)?;
            // SAFETY: only valid local sigset pointers (or null); pthread returns its own error number.
            let code = unsafe { libc::pthread_sigmask(how, set, old) };
            if code == 0 {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "{stage}: {}",
                    io::Error::from_raw_os_error(code)
                )))
            }
        }
        fn deliverable(&self) -> io::Result<()> {
            let mut current = unsafe { std::mem::zeroed() };
            self.mask(
                "readback",
                libc::SIG_SETMASK,
                std::ptr::null(),
                &mut current,
            )?;
            self.before("membership")?;
            for signum in [libc::SIGINT, libc::SIGTERM] {
                let member = unsafe { libc::sigismember(&current, signum) };
                if member == -1 {
                    return Err(io::Error::other(format!(
                        "sigismember: {}",
                        io::Error::last_os_error()
                    )));
                }
                if member != 0 {
                    return Err(io::Error::other(format!(
                        "signal {signum} remains blocked in watcher"
                    )));
                }
            }
            Ok(())
        }
    }

    pub(super) fn install_with(
        terminate: impl Fn(i32) + Send + 'static,
        ops: Ops,
    ) -> io::Result<()> {
        let set = ops.set()?;
        let mut old = unsafe { std::mem::zeroed() };
        ops.mask("block", libc::SIG_BLOCK, &set, &mut old)?;
        let (sent, ready) = mpsc::sync_channel(1);
        let watcher_ops = ops.clone();
        let started = std::thread::Builder::new()
            .name("sot-signal-exit".into())
            .spawn(move || {
                watch(terminate, watcher_ops, sent);
            });
        let installed = match started {
            Ok(_) => ready
                .recv()
                .map_err(|e| io::Error::other(format!("signal watcher readiness: {e}")))
                .and_then(|r| r),
            Err(e) => Err(io::Error::other(format!("signal watcher start: {e}"))),
        };
        let restored = ops.mask("restore", libc::SIG_SETMASK, &old, std::ptr::null_mut());
        match (installed, restored) {
            (Ok(()), result) => result,
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(restore)) => Err(io::Error::other(format!(
                "{error}; restoring main mask: {restore}"
            ))),
        }
    }

    fn watch(terminate: impl Fn(i32), ops: Ops, ready: mpsc::SyncSender<io::Result<()>>) {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = ready.send(Err(io::Error::other(format!(
                    "signal watcher runtime: {error}"
                ))));
                return;
            }
        };
        runtime.block_on(async {
            let handlers = (|| {
                ops.before("register-int")?;
                let int = signal(SignalKind::interrupt())
                    .map_err(|e| io::Error::other(format!("register SIGINT: {e}")))?;
                ops.before("register-term")?;
                let term = signal(SignalKind::terminate())
                    .map_err(|e| io::Error::other(format!("register SIGTERM: {e}")))?;
                let set = ops.set()?;
                ops.mask("unblock", libc::SIG_UNBLOCK, &set, std::ptr::null_mut())?;
                ops.deliverable()?;
                Ok::<_, io::Error>((int, term))
            })();
            let (mut int, mut term) = match handlers {
                Ok(handlers) => handlers,
                Err(error) => {
                    let _ = ready.send(Err(error));
                    return;
                }
            };
            if ready.send(Ok(())).is_err() {
                eprintln!("sotd: signal watcher lost its startup receiver");
                terminate(1);
                return;
            }
            let (received, code) = tokio::select! {
                received = int.recv() => (received, 130),
                received = term.recv() => (received, 143),
            };
            if received.is_none() {
                tracing::error!("signal watcher notification stream ended");
                eprintln!("sotd: signal watcher notification stream ended");
                terminate(1);
            } else {
                terminate(code);
            }
        });
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::lifecycle::{child_signal::Signal, shutdown};

        /// Each stage of the installation that can fail does so as a concrete failure exit: the reason reaches the log and the
        /// child signal is fired before the exit code is handed on. The delivery of INT and TERM to a real daemon (inherited
        /// blocked masks and a stalled runtime included) is the daemon-lifetime harness's, `guard.rs`.
        #[test]
        fn installation_failures_are_concrete_failure_exits() {
            let name = "lifecycle::signal_exit::unix::tests::installation_failures_are_concrete_failure_exits";
            if !sot_log::test_isolated::run_isolated(name) {
                return;
            }
            for stage in [
                "set",
                "block",
                "register-int",
                "register-term",
                "unblock",
                "readback",
                "membership",
                "restore",
            ] {
                let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
                let error = install_with(
                    |_| panic!("installation failed after acknowledging readiness"),
                    Ops { fail: Some(stage) },
                )
                .expect_err("installation failure was silently accepted");
                let log = sot_log::test_log::capture();
                tracing::error!(%error, "signal watcher installation failed");
                let observed =
                    shutdown::terminal(signal, 1, |code| (code, signal.is_fired(), log.text()));
                assert_eq!(observed.0, 1);
                assert!(observed.1);
                assert!(
                    observed.2.contains(&format!("injected {stage} failure")),
                    "installation reason was discarded: {}",
                    observed.2
                );
            }
        }
    }
}
