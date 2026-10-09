//! `sotd durable-parent`: the capsule-only birth parent. The daemon starts one, once, in its serving prologue before the
//! runtime, over a private channel; it lives in a session of its own, so a capsule it forks never inherits anything the
//! daemon's containment covers.
//!
//! The parent accepts a launch only with the row's fence in hand (`accept`), keeps the physical gate writer and the
//! kernel parent authority of the supervisor it forks until that supervisor has taken the claim over, and forwards the
//! supervisor's exit to the daemon while the daemon lives. If the daemon dies first, an accepted launch is finished
//! here under the same claim; nothing is cancelled and nothing is replayed.

use super::accept::{accept, barrier, parent_only_of, Accepted, Refusal};
use super::wire::{decode, encode, Channel, LaunchSpec, Reply, Request};
use sot_log::supervisor::birth_claim::read_takeover;
use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::time::Duration;

/// How long a released target gets to exec.
const EXEC_BOUND: Duration = Duration::from_secs(10);
/// The loop's tick: how long it waits for the channel or a takeover before looking at the children.
const TICK: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Accepted: the claim is held and the target waits at the gate.
    Gated,
    /// The target has exec'd; the claim is still the parent's until the supervisor takes it over.
    Released,
    /// The supervisor took the claim over; only its exit is left to report.
    TakenOver,
}

struct Slot {
    accepted: Accepted,
    phase: Phase,
    /// The supervisor's takeover channel still has something to say (it has not been read to its end).
    takeover_open: bool,
}

struct Parent {
    channel: Option<Channel>,
    slots: HashMap<u64, Slot>,
}

/// The mode's entry: the channel is this process's standard input. Returns the exit status.
pub fn run(args: &[String]) -> i32 {
    // A session of its own: the daemon's terminal, group and containment are not this process's.
    // SAFETY: setsid takes no arguments; it fails (and changes nothing) only if this process already leads a group.
    unsafe { libc::setsid() };
    // Into a user scope of its own, before it accepts anything, when the user manager grants one: a birth in flight is
    // then never inside the daemon's service cgroup. The exec keeps this pid and the channel; a refusal runs on here.
    #[cfg(target_os = "linux")]
    if !args.iter().any(|a| a == "--scoped") {
        move_into_scope();
    }
    #[cfg(not(target_os = "linux"))]
    let _ = args;
    // SAFETY: the daemon started this process with the channel as its standard input and nothing else owns it.
    let channel = Channel::from_owned(unsafe { OwnedFd::from_raw_fd(0) });
    let mut parent = Parent {
        channel: Some(channel),
        slots: HashMap::new(),
    };
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    if !parent.say(&Reply::Hello {
        pid: std::process::id(),
        cgroup,
    }) {
        return 1;
    }
    parent.serve()
}

/// Re-exec under `systemd-run --user --scope` when the manager answers; returns only if it did not.
#[cfg(target_os = "linux")]
fn move_into_scope() {
    use super::super::detach::{systemd_run_path, user_scope_available};
    use std::os::unix::process::CommandExt as _;
    let (Some(systemd_run), Ok(exe)) = (systemd_run_path(), std::env::current_exe()) else {
        return;
    };
    if let Err(e) = user_scope_available() {
        eprintln!(
            "sotd durable-parent: no user scope ({e}); staying in the daemon's control group"
        );
        return;
    }
    #[allow(
        clippy::disallowed_methods,
        reason = "the durable parent replaces itself with systemd-run, which execs the same parent in a scope of its own; no child is started"
    )]
    let error = std::process::Command::new(systemd_run)
        .args([
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--description",
            "sot durable parent",
            "--",
        ])
        .arg(exe)
        .args(["durable-parent", "--scoped"])
        .exec();
    eprintln!("sotd durable-parent: could not enter a user scope ({error})");
}

impl Parent {
    /// Send `reply` while the daemon is there; `false` once it is not.
    fn say(&mut self, reply: &Reply) -> bool {
        let Some(channel) = &self.channel else {
            return false;
        };
        let sent = encode(reply).and_then(|bytes| channel.send(&bytes, &[]));
        if let Err(e) = sent {
            eprintln!("sotd durable-parent: the daemon's channel is gone ({e})");
            self.channel = None;
            self.daemon_gone();
            return false;
        }
        true
    }

    fn serve(&mut self) -> i32 {
        loop {
            self.wait_for_activity();
            self.serve_channel();
            self.watch_slots();
            let pending = self.slots.values().any(|s| s.phase != Phase::TakenOver);
            if self.channel.is_none() && !pending {
                for (_, slot) in self.slots.drain() {
                    // A supervisor that took its claim over outlives this parent, as an ordinary orphan.
                    slot.accepted.birth.abandon();
                }
                return 0;
            }
        }
    }

    /// Sleep until the channel or a takeover channel has something, or a tick passes.
    fn wait_for_activity(&self) {
        let mut fds = Vec::new();
        if let Some(channel) = &self.channel {
            fds.push(libc::pollfd {
                fd: channel.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        for slot in self.slots.values().filter(|s| s.takeover_open) {
            fds.push(libc::pollfd {
                fd: slot.accepted.takeover.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        // SAFETY: a slice of valid pollfds; the timeout is a small constant. An error or a signal only shortens the wait.
        unsafe {
            libc::poll(
                fds.as_mut_ptr(),
                fds.len() as libc::nfds_t,
                TICK.as_millis() as libc::c_int,
            )
        };
    }

    fn serve_channel(&mut self) {
        loop {
            let Some(channel) = &self.channel else { return };
            match channel.readable(Duration::ZERO) {
                Ok(true) => {}
                Ok(false) => return,
                Err(_) => break,
            }
            match channel.recv() {
                Ok(Some(message)) => match decode::<Request>(&message.payload) {
                    Ok(request) => self.handle(request, message.fds),
                    Err(e) => eprintln!("sotd durable-parent: an undecodable request ({e})"),
                },
                Ok(None) | Err(_) => break,
            }
        }
        // EOF, or the channel broke: the daemon is gone.
        self.channel = None;
        self.daemon_gone();
    }

    fn handle(&mut self, request: Request, mut fds: Vec<OwnedFd>) {
        match request {
            Request::Launch(spec) => self.launch(spec, fds.pop()),
            Request::Release { id } => self.release(id),
            Request::Cancel { id } => {
                // The child ends and is reaped, then the claim is let go (`Accepted`'s field order).
                self.slots.remove(&id);
            }
        }
    }

    fn launch(&mut self, spec: LaunchSpec, stderr: Option<OwnedFd>) {
        let id = spec.id;
        let Some(stderr) = stderr else {
            self.say(&Reply::Failed {
                id,
                text: "the launch carried no standard error descriptor".into(),
            });
            return;
        };
        let mut parent_only: Vec<_> = self
            .slots
            .values()
            .flat_map(|s| parent_only_of(&s.accepted))
            .collect();
        if let Some(channel) = &self.channel {
            parent_only.push(channel.as_raw_fd());
        }
        match accept(&spec, stderr, &parent_only) {
            Ok(accepted) => {
                let ready = accepted.ready;
                self.slots.insert(
                    id,
                    Slot {
                        accepted,
                        phase: Phase::Gated,
                        takeover_open: true,
                    },
                );
                self.say(&Reply::Born {
                    id,
                    pid: ready.pid,
                    pgid: ready.pgid,
                    sid: ready.sid,
                });
            }
            Err(Refusal::Contended) => {
                self.say(&Reply::Contended { id });
            }
            Err(Refusal::Failed(text)) => {
                self.say(&Reply::Failed { id, text });
            }
        }
    }

    /// Open the gate of an accepted launch and wait for the target to exec.
    fn release(&mut self, id: u64) {
        barrier("parent_release");
        let Some(slot) = self.slots.get_mut(&id) else {
            self.say(&Reply::ExecFailed {
                id,
                text: "no such accepted launch".into(),
            });
            return;
        };
        let outcome = slot
            .accepted
            .birth
            .release()
            .and_then(|()| slot.accepted.birth.exec_result(EXEC_BOUND));
        match outcome {
            Ok(()) => {
                slot.phase = Phase::Released;
                self.say(&Reply::Released { id });
            }
            Err(e) => {
                // The child ends, is reaped, and then the claim is let go.
                self.slots.remove(&id);
                self.say(&Reply::ExecFailed {
                    id,
                    text: e.to_string(),
                });
            }
        }
    }

    /// The daemon died: every accepted launch is finished here, under the claim it already holds.
    fn daemon_gone(&mut self) {
        let gated: Vec<u64> = self
            .slots
            .iter()
            .filter(|(_, s)| s.phase == Phase::Gated)
            .map(|(id, _)| *id)
            .collect();
        for id in gated {
            eprintln!("sotd durable-parent: the daemon is gone; finishing accepted launch {id}");
            self.release(id);
        }
    }

    /// Look at every child and every takeover channel: let go of a claim the supervisor has taken over, and report a
    /// supervisor that ended.
    fn watch_slots(&mut self) {
        let ids: Vec<u64> = self.slots.keys().copied().collect();
        for id in ids {
            self.watch(id);
        }
    }

    fn watch(&mut self, id: u64) {
        let Some(slot) = self.slots.get_mut(&id) else {
            return;
        };
        if slot.phase == Phase::Released && slot.takeover_open && slot.accepted.takeover_readable()
        {
            match read_takeover(&mut slot.accepted.takeover) {
                Ok(Some(t)) if t.pid as i32 == slot.accepted.birth.pid() => {
                    // The supervisor holds the claim on its own descriptor now: this copy goes.
                    slot.accepted.claim = None;
                    slot.phase = Phase::TakenOver;
                    slot.takeover_open = false;
                }
                Ok(Some(t)) => {
                    eprintln!("sotd durable-parent: a takeover from pid {} for the child {}; keeping the claim", t.pid, slot.accepted.birth.pid());
                    slot.takeover_open = false;
                }
                // The channel closed with nothing on it: the claim stays with this parent until the child ends.
                Ok(None) | Err(_) => slot.takeover_open = false,
            }
        }
        if slot.phase == Phase::Gated || !slot.accepted.birth.exited(false).unwrap_or(false) {
            return;
        }
        let status = slot.accepted.birth.wait();
        self.slots.remove(&id);
        let (code, signal) = match status {
            Ok(status) => (status.code(), status.signal()),
            Err(_) => (None, None),
        };
        self.say(&Reply::Exited { id, code, signal });
    }
}

impl Accepted {
    fn takeover_readable(&self) -> bool {
        super::wire::poll_readable(self.takeover.as_raw_fd(), Duration::ZERO).unwrap_or(false)
    }
}
