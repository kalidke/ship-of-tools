//! Checked Unix signal installation on an independent runtime thread, before daemon child work. INT and TERM end the
//! daemon through the terminal's fire and then by the signal itself, so whatever started the daemon (a service manager,
//! a shell, the guard, which mirrors a signal death) sees the end it would see with no handler.

#[cfg(not(unix))]
pub(crate) fn install() -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn install() -> std::io::Result<()> {
    unix::install_with(
        |caught| match caught {
            Some(signum) => super::shutdown::exit_by_signal(signum),
            None => super::shutdown::exit(1),
        },
        unix::Ops::default(),
    )
}

#[cfg(unix)]
mod unix {
    use std::io;
    use std::sync::mpsc;
    use tokio::signal::unix::{signal, SignalKind};

    /// Test-only: a registration that fails, to show an installation failure is a concrete failure exit.
    #[derive(Clone, Copy, Default)]
    pub(super) struct Ops {
        #[cfg(test)]
        pub(super) fail_registration: bool,
    }

    impl Ops {
        fn register(self, kind: SignalKind, name: &str) -> io::Result<tokio::signal::unix::Signal> {
            #[cfg(test)]
            if self.fail_registration {
                return Err(io::Error::other(format!(
                    "injected {name} registration failure"
                )));
            }
            signal(kind).map_err(|e| io::Error::other(format!("register {name}: {e}")))
        }
    }

    /// INT and TERM as a set. The calls cannot fail with these valid signal numbers.
    fn int_and_term() -> libc::sigset_t {
        // SAFETY: a set built in place from valid signal numbers.
        unsafe {
            let mut set = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGINT);
            libc::sigaddset(&mut set, libc::SIGTERM);
            set
        }
    }

    /// Set this thread's mask by `how`; the old mask is returned. It cannot fail with a valid `how` and local sets.
    fn mask(how: libc::c_int, set: &libc::sigset_t) -> libc::sigset_t {
        // SAFETY: valid local sets; pthread_sigmask edits only this thread's mask.
        unsafe {
            let mut old = std::mem::zeroed();
            libc::pthread_sigmask(how, set, &mut old);
            old
        }
    }

    /// Whether INT and TERM reach this thread: the readback of its mask after the unblock.
    fn deliverable() -> io::Result<()> {
        // SAFETY: a read of this thread's mask into a local set (a null new set changes nothing), then membership tests.
        unsafe {
            let mut current = std::mem::zeroed();
            libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut current);
            for signum in [libc::SIGINT, libc::SIGTERM] {
                if libc::sigismember(&current, signum) != 0 {
                    return Err(io::Error::other(format!(
                        "signal {signum} remains blocked in watcher"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Block INT and TERM in this thread while the watcher starts (it inherits the block, registers, then unblocks them
    /// in itself), and restore this thread's mask after. `terminate` gets the caught signal, or `None` for a watcher that
    /// lost its stream.
    pub(super) fn install_with(
        terminate: impl Fn(Option<i32>) + Send + 'static,
        ops: Ops,
    ) -> io::Result<()> {
        let old = mask(libc::SIG_BLOCK, &int_and_term());
        let (sent, ready) = mpsc::sync_channel(1);
        let started = std::thread::Builder::new()
            .name("sot-signal-exit".into())
            .spawn(move || watch(terminate, ops, sent));
        let installed = match started {
            Ok(_) => ready
                .recv()
                .map_err(|e| io::Error::other(format!("signal watcher readiness: {e}")))
                .and_then(|r| r),
            Err(e) => Err(io::Error::other(format!("signal watcher start: {e}"))),
        };
        mask(libc::SIG_SETMASK, &old);
        installed
    }

    fn watch(terminate: impl Fn(Option<i32>), ops: Ops, ready: mpsc::SyncSender<io::Result<()>>) {
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
                let int = ops.register(SignalKind::interrupt(), "SIGINT")?;
                let term = ops.register(SignalKind::terminate(), "SIGTERM")?;
                mask(libc::SIG_UNBLOCK, &int_and_term());
                deliverable()?;
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
                terminate(None);
                return;
            }
            let (received, signum) = tokio::select! {
                received = int.recv() => (received, libc::SIGINT),
                received = term.recv() => (received, libc::SIGTERM),
            };
            if received.is_none() {
                tracing::error!("signal watcher notification stream ended");
                eprintln!("sotd: signal watcher notification stream ended");
                terminate(None);
            } else {
                terminate(Some(signum));
            }
        });
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::lifecycle::{child_signal::Signal, shutdown};

        /// A failed installation is a concrete failure exit: the reason reaches the log and the child signal is fired
        /// before the exit code is handed on. The delivery of INT and TERM to a real daemon (inherited blocked masks and a
        /// stalled runtime included) is the daemon-lifetime harness's, `outcomes.rs`.
        #[test]
        fn an_installation_failure_is_a_concrete_failure_exit() {
            let name = "lifecycle::signal_exit::unix::tests::an_installation_failure_is_a_concrete_failure_exit";
            if !sot_log::test_isolated::run_isolated(name) {
                return;
            }
            let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
            let error = install_with(
                |_| panic!("installation failed after acknowledging readiness"),
                Ops {
                    fail_registration: true,
                },
            )
            .expect_err("installation failure was silently accepted");
            let log = sot_log::test_log::capture();
            tracing::error!(%error, "signal watcher installation failed");
            let observed =
                shutdown::terminal(signal, 1, |code| (code, signal.is_fired(), log.text()));
            assert_eq!(observed.0, 1);
            assert!(observed.1);
            assert!(
                observed.2.contains("injected SIGINT registration failure"),
                "installation reason was discarded: {}",
                observed.2
            );
        }
    }
}
