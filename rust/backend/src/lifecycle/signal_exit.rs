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
        use crate::lifecycle::start_tests::unix::Watched;
        use crate::lifecycle::{child_signal::Signal, contain, exit_tests::ReadyTree, shutdown};
        use std::path::PathBuf;
        use std::time::{Duration, Instant};

        const FIXTURE: &str = "lifecycle::signal_exit::unix::tests::signal_fixture";

        #[test]
        fn signal_fixture() {
            let Some(root) = std::env::var_os("SOT_TEST_L2_SIGNAL_ROOT") else {
                return;
            };
            sot_log::test_isolated::enter(FIXTURE);
            crate::lifecycle::child_signal::reset_child_signal();
            let root = PathBuf::from(root);
            let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
            let trace = root.join("terminal");
            install_with(
                move |code| {
                    shutdown::terminal(signal, code, |code| {
                        let requests =
                            contain::REQUEST_EVENTS.with(|events| events.borrow().clone());
                        std::fs::write(
                            &trace,
                            format!("{code} {} {requests:?}", signal.is_fired()),
                        )
                        .unwrap();
                        std::process::exit(code);
                    });
                },
                Ops::default(),
            )
            .expect("production watcher installation");
            let tree = ReadyTree::start(signal);
            assert_ne!(
                unsafe { libc::getpgrp() },
                tree.child.id() as i32,
                "fixture shares its owner's process group"
            );
            std::fs::write(
                root.join("identities"),
                format!("{} {}", tree.child.id(), tree.descendant_pid()),
            )
            .unwrap();
            // The owner and runtime stay alive; the signal watcher must make progress independently.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                if std::env::var_os("SOT_TEST_L2_STALL_RUNTIME").is_some() {
                    std::thread::sleep(Duration::from_secs(60));
                } else {
                    std::future::pending::<()>().await;
                }
            });
            tree.finish();
        }

        fn inherited_mask(command: &mut std::process::Command) {
            use std::os::unix::process::CommandExt;
            // SAFETY: before exec, only async-signal-safe sigset/mask operations on local memory.
            unsafe {
                command.pre_exec(|| {
                    let mut set = std::mem::zeroed();
                    if libc::sigemptyset(&mut set) != 0
                        || libc::sigaddset(&mut set, libc::SIGINT) != 0
                        || libc::sigaddset(&mut set, libc::SIGTERM) != 0
                        || libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

        fn exercise(signum: i32, blocked: bool, stalled: bool) {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let (mut command, entered) = sot_log::test_isolated::test_command(FIXTURE);
            command
                .env("SOT_TEST_L2_SIGNAL_ROOT", root)
                .env("SOT_TEST_L2_FIXTURE_CLEANUP", root.join("cleanup"))
                .stdout(std::process::Stdio::null())
                .stderr(std::fs::File::create(root.join("stderr")).unwrap());
            if blocked {
                inherited_mask(&mut command);
            }
            if stalled {
                command.env("SOT_TEST_L2_STALL_RUNTIME", "1");
            }
            let mut owner = command.spawn().expect("owned signal fixture");
            let began = Instant::now();
            while !root.join("identities").exists() && began.elapsed() < Duration::from_secs(15) {
                if owner.try_wait().unwrap().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if !root.join("identities").exists() {
                let _ = owner.kill();
                let _ = owner.wait();
                std::fs::write(root.join("cleanup"), "cleanup").unwrap();
                panic!(
                    "signal fixture did not reach ready contained work: {}",
                    std::fs::read_to_string(root.join("stderr")).unwrap_or_default()
                );
            }
            entered.assert_once(owner.id());
            let ids: Vec<u32> = std::fs::read_to_string(root.join("identities"))
                .unwrap()
                .split_whitespace()
                .map(|p| p.parse().unwrap())
                .collect();
            let leader = Watched::open(ids[0]);
            let descendant = Watched::open(ids[1]);
            assert_eq!(
                unsafe { libc::kill(owner.id() as i32, signum) },
                0,
                "signal only the unreaped owned test process"
            );
            let began = Instant::now();
            let status = loop {
                if let Some(status) = owner.try_wait().unwrap() {
                    break Some(status);
                }
                if began.elapsed() >= Duration::from_secs(3) {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            let trace = std::fs::read_to_string(root.join("terminal")).unwrap_or_default();
            let delivered = status.is_some();
            if !delivered {
                owner.kill().expect("kill retained test owner");
                owner.wait().unwrap();
            }
            let leader_gone = leader.dead();
            let descendant_gone = descendant.dead();
            let ended = leader_gone && descendant_gone;
            // A failed/default-mask parent has no fire. Its owned fixtures exit on this private marker;
            // no reusable process number is signalled for cleanup, including on macOS.
            std::fs::write(root.join("cleanup"), "cleanup").unwrap();
            if !leader_gone {
                assert!(
                    leader.dead(),
                    "signal leader fixture cleanup did not complete"
                );
            }
            if !descendant_gone {
                assert!(
                    descendant.dead(),
                    "signal descendant fixture cleanup did not complete"
                );
            }
            assert!(
                delivered,
                "inherited blocked signal never reached the watcher"
            );
            let expected = if signum == libc::SIGINT { 130 } else { 143 };
            assert_eq!(
                trace,
                format!("{expected} true [\"group\", \"leader\"]"),
                "signal terminal event lacked checked request-before-exit trace"
            );
            assert_eq!(
                status.unwrap().code(),
                Some(expected),
                "signal bypassed the controlled terminal code"
            );
            assert!(
                ended,
                "signal-requested tree outlived its separate death observation"
            );
        }

        #[test]
        fn sigint_fires_before_exit() {
            exercise(libc::SIGINT, false, false);
        }
        #[test]
        fn sigterm_fires_before_exit() {
            exercise(libc::SIGTERM, false, false);
        }
        #[test]
        fn inherited_blocked_sigint_is_deliverable() {
            exercise(libc::SIGINT, true, false);
        }
        #[test]
        fn inherited_blocked_sigterm_is_deliverable() {
            exercise(libc::SIGTERM, true, false);
        }
        #[test]
        fn a_stalled_main_runtime_does_not_stall_signal_fire() {
            exercise(libc::SIGTERM, true, true);
        }

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
