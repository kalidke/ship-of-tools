//! The capsule: one process babysitting one producer, writing its voyage
//! (ADR 0037 P1 — Linux, PTY adapter; ADR 0039 is the format it writes).
//!
//! Design, in five sentences. A single WRITER LOOP owns the `VoyageStore`
//! and the open `SegmentWriter` — the one-writer invariant holds by
//! construction, because every event (producer output, controller input,
//! child exit) funnels through one mpsc channel into one thread. The
//! producer runs on a real PTY (`openpty` via libc — terminals need a tty;
//! no new dependencies). This is a RAW-TERMINAL capsule: it emits ZERO turn
//! frames (ADR 0037's two classes of program — we never guess turns from
//! bytes), and input is redacted by default (presence + length recorded,
//! bytes forwarded but not stored). Producer output group-commits (50 ms /
//! 256 KiB) and is echoed to the capsule's stdout only AFTER the fsync that
//! covers it — the visibility watermark as observable behavior, not policy
//! prose.
//!
//! Frame protocol per run (one epoch = this one producer-run attempt):
//! `take_state {holder: null}` (recovery revoke-first, ADR take predicate) →
//! `take_state {holder: "local"}` → `producer_attached` → lifecycle
//! `producer_spawn` → producer/input frames → lifecycle `producer_dead`
//! (exit status) → seal.

#![cfg(target_os = "linux")]

use crate::envelope::*;
use crate::segment::{Commit, RetentionClass};
use crate::voyage::VoyageStore;
use crate::{Error, Result};
use serde_json::json;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const GROUP_COMMIT_WINDOW: Duration = Duration::from_millis(50);
const GROUP_COMMIT_BYTES: usize = 256 * 1024;
const SEGMENT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const READ_CHUNK: usize = 8192;
const LOCAL_CONTROLLER: &str = "local";

pub struct CapsuleConfig {
    pub voyage_root: PathBuf,
    pub voyage_id: String,
    pub retention: RetentionClass,
    pub producer_kind: String,
    /// argv[0] is the program; must be non-empty.
    pub argv: Vec<String>,
    /// Echo producer output to the capsule's stdout after commit.
    pub echo: bool,
}

#[derive(Debug)]
pub struct ExitSummary {
    pub exit_code: Option<i32>,
    pub frames_written: u64,
    pub segments_sealed: u64,
}

enum Event {
    Output(Vec<u8>),
    Input(Vec<u8>),
    ProducerEof,
}

/// 16 random bytes from the OS, as lowercase hex32 (the ADR idem_key shape).
fn random_idem_key() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(std::io::Error::from)?;
    Ok(b.iter().map(|x| format!("{:02x}", x)).collect())
}

fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// PTY pair + child. The slave becomes the child's controlling tty.
struct PtyChild {
    master: OwnedFd,
    child: std::process::Child,
}

fn spawn_on_pty(argv: &[String]) -> Result<PtyChild> {
    use std::os::unix::process::CommandExt;
    if argv.is_empty() {
        return Err(Error::State("capsule argv is empty".into()));
    }
    let mut master_fd: libc::c_int = -1;
    let mut slave_fd: libc::c_int = -1;
    let rc = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }
    let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };

    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    let slave_raw = slave.as_raw_fd();
    unsafe {
        cmd.pre_exec(move || {
            // New session; slave becomes the controlling tty; stdio on it.
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(slave_raw, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            for fd in 0..=2 {
                if libc::dup2(slave_raw, fd) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            // Close EVERY inherited fd ≥ 3 before exec. O_CLOEXEC alone is
            // not enough: between fork and exec this child holds copies of
            // all parent fds, and a flock lives on the open file
            // description — so a capsule-host thread dropping and reopening
            // a voyage lock during this window would collide with its own
            // lock through us (observed as a rare parallel-test flake).
            // close_range severs those references at the earliest point.
            if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    drop(slave); // parent keeps only the master
    Ok(PtyChild { master, child })
}

/// The writer loop's frame factory: sequential seq, capsule clocks, and the
/// per-run refs (attached_to / input WAL) threaded through one small state.
struct FrameCtx {
    epoch: u64,
    next_n: u64,
    t0: Instant,
    take_epoch: u64,
    attached: Option<Seq>,
}

impl FrameCtx {
    fn seq(&mut self) -> Seq {
        let s = Seq {
            epoch: self.epoch,
            n: self.next_n,
        };
        self.next_n += 1;
        s
    }
    fn mono_us(&self) -> u64 {
        self.t0.elapsed().as_micros() as u64
    }
    fn capsule_frame(&mut self, class: Class, payload: serde_json::Value) -> Envelope {
        Envelope {
            seq: self.seq(),
            class,
            source: Source {
                emitter: Emitter::Capsule,
                actor: Actor {
                    kind: ActorKind::Unknown,
                    controller_id: None,
                    take_epoch: None,
                },
                derivation: Derivation::Synthetic,
            },
            t_wall_ms: wall_ms(),
            t_mono_us: self.mono_us(),
            stream: None,
            transformed: None,
            refs: vec![],
            payload: Some(payload),
            payload_ref: None,
        }
    }
    fn controller_frame(&mut self, class: Class, payload: serde_json::Value) -> Envelope {
        let mut e = self.capsule_frame(class, payload);
        e.source.actor = Actor {
            kind: ActorKind::Controller,
            controller_id: Some(LOCAL_CONTROLLER.into()),
            take_epoch: Some(self.take_epoch),
        };
        e
    }
    fn producer_frame(&mut self, payload: serde_json::Value) -> Envelope {
        let mut e = self.capsule_frame(Class::Producer, payload);
        e.source.emitter = Emitter::Producer;
        e.source.actor.kind = ActorKind::Producer;
        e.source.derivation = Derivation::Native;
        e.refs = vec![FrameRef {
            kind: RefKind::AttachedTo,
            frame: self.attached.expect("attached before producer frames"),
        }];
        e
    }
}

/// Run one producer under a capsule. Blocks until the producer exits (or the
/// input side reports EOF and the producer then dies).
// unused_assignments: flush_output!'s state reset is dead only at its FINAL
// expansion (after the loop); the reset is load-bearing at every other site.
#[allow(unused_assignments)]
pub fn run(config: CapsuleConfig) -> Result<ExitSummary> {
    // Resolve ONCE: every later use must name the same directory. Resolving
    // a relative config path repeatedly lets a concurrent `set_current_dir`
    // send the existence check, bootstrap, and open to different stores.
    let voyage_root = crate::fsutil::ensure_container(&config.voyage_root)?;
    if !voyage_root.exists() {
        VoyageStore::bootstrap(&voyage_root, &config.voyage_id, config.retention)?;
    }
    let mut store = VoyageStore::open_for_writing(&voyage_root, &config.voyage_id)?;
    store.seal_survivor()?;

    let mut ctx = FrameCtx {
        epoch: store.epoch,
        next_n: 1,
        t0: Instant::now(),
        take_epoch: 0,
        attached: None,
    };
    let mut w = store.open_segment(wall_ms())?;
    let mut seg_bytes: u64 = 0;
    let mut frames_written: u64 = 0;
    let mut segments_sealed: u64 = 0;

    // Control preamble — every frame here commits immediately.
    // take: revoke-first (null holder, bumped epoch), then grant local.
    let prior_take = store.last_take_epoch; // resumes continue the sequence
    let f = ctx.capsule_frame(
        Class::Lifecycle,
        json!({"kind": "take_state", "take": {"take_epoch": prior_take + 1, "holder": null}}),
    );
    w.append(&f, Commit::Immediate)?;
    frames_written += 1;
    ctx.take_epoch = prior_take + 2;
    let f = ctx.capsule_frame(
        Class::Lifecycle,
        json!({"kind": "take_state", "take": {"take_epoch": ctx.take_epoch, "holder": LOCAL_CONTROLLER}}),
    );
    w.append(&f, Commit::Immediate)?;
    frames_written += 1;

    // producer_attached: the raw-terminal redaction profile, content-hashed.
    let rules = json!({"input_content": "redacted", "turns": "none"});
    let rules_bytes = serde_json::to_vec(&rules)?;
    let rules_sha = {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(&rules_bytes);
        h.finalize().iter().map(|b| format!("{:02x}", b)).collect::<String>()
    };
    let attached_seq_holder = ctx.capsule_frame(
        Class::ProducerAttached,
        json!({
            "producer_kind": config.producer_kind,
            "version": "raw-pty-1",
            "profile_def": {"id": "raw-terminal-default", "sha256": rules_sha, "rules": rules},
        }),
    );
    let attached_seq = attached_seq_holder.seq;
    w.append(&attached_seq_holder, Commit::Immediate)?;
    frames_written += 1;
    ctx.attached = Some(attached_seq);

    let f = ctx.capsule_frame(
        Class::Lifecycle,
        json!({"kind": "producer_spawn", "detail": {"argv": config.argv}}),
    );
    w.append(&f, Commit::Immediate)?;
    frames_written += 1;

    // Spawn the producer and the two reader threads.
    let mut pty = spawn_on_pty(&config.argv)?;
    let (tx, rx) = mpsc::channel::<Event>();
    {
        let tx = tx.clone();
        let mut master = std::fs::File::from(pty.master.try_clone()?);
        std::thread::spawn(move || {
            let mut buf = [0u8; READ_CHUNK];
            loop {
                match master.read(&mut buf) {
                    Ok(0) | Err(_) => break, // EIO at child exit is the normal PTY EOF
                    Ok(n) => {
                        if tx.send(Event::Output(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = tx.send(Event::ProducerEof);
        });
    }
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; READ_CHUNK];
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 || tx.send(Event::Input(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);

    // Writer loop: single owner of the store. Output buffers behind the
    // group-commit watermark; input and lifecycle are synchronous.
    let mut master_w = std::fs::File::from(pty.master.try_clone()?);
    let mut stdout = std::io::stdout();
    let mut pending_echo: Vec<u8> = Vec::new();
    let mut pending_bytes: usize = 0;
    let mut last_commit = Instant::now();

    macro_rules! flush_output {
        ($w:expr) => {
            if pending_bytes > 0 {
                $w.commit()?; // the watermark: fsync BEFORE anything is echoed
                if config.echo {
                    let _ = stdout.write_all(&pending_echo);
                    let _ = stdout.flush();
                }
                pending_echo.clear();
                pending_bytes = 0;
            }
            last_commit = Instant::now();
        };
    }

    macro_rules! maybe_rotate {
        ($w:ident) => {
            if seg_bytes >= SEGMENT_MAX_BYTES {
                flush_output!($w);
                let digest = $w.seal(None)?;
                store.advance_chain(digest);
                segments_sealed += 1;
                $w = store.open_segment(wall_ms())?;
                seg_bytes = 0;
            }
        };
    }

    loop {
        match rx.recv_timeout(GROUP_COMMIT_WINDOW) {
            Ok(Event::Output(bytes)) => {
                use base64_engine::encode_b64;
                let f = ctx.producer_frame(json!({"bytes_b64": encode_b64(&bytes)}));
                w.append(&f, Commit::Buffered)?;
                frames_written += 1;
                seg_bytes += bytes.len() as u64 + 128;
                pending_echo.extend_from_slice(&bytes);
                pending_bytes += bytes.len();
                if pending_bytes >= GROUP_COMMIT_BYTES {
                    flush_output!(w);
                }
                maybe_rotate!(w);
            }
            Ok(Event::Input(bytes)) => {
                // Order is the ADR WAL: input → intent → syscall → forwarded.
                flush_output!(w); // never let redacted input frames pass uncommitted output
                let input = ctx.controller_frame(
                    Class::Input,
                    json!({"idem_key": random_idem_key()?, "content": "redacted",
                           "length": bytes.len()}),
                );
                let input_seq = input.seq;
                w.append(&input, Commit::Immediate)?;
                let mut intent = ctx.controller_frame(
                    Class::Lifecycle,
                    json!({"kind": "input_fact",
                           "fact": {"input": {"epoch": input_seq.epoch, "n": input_seq.n},
                                     "fact": "forward_intent"}}),
                );
                intent.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: input_seq }];
                let intent_seq = intent.seq;
                w.append(&intent, Commit::Immediate)?;
                master_w.write_all(&bytes)?; // the forward syscall
                let mut fwd = ctx.controller_frame(
                    Class::Lifecycle,
                    json!({"kind": "input_fact",
                           "fact": {"input": {"epoch": input_seq.epoch, "n": input_seq.n},
                                     "fact": "forwarded",
                                     "intent": {"epoch": intent_seq.epoch, "n": intent_seq.n}}}),
                );
                fwd.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: input_seq }];
                w.append(&fwd, Commit::Immediate)?;
                frames_written += 3;
                maybe_rotate!(w);
            }
            Ok(Event::ProducerEof) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if last_commit.elapsed() >= GROUP_COMMIT_WINDOW {
                    flush_output!(w);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    flush_output!(w);

    let status = pty.child.wait()?;
    let exit_code = status.code();
    let f = ctx.capsule_frame(
        Class::Lifecycle,
        json!({"kind": "producer_dead", "detail": {"exit_code": exit_code}}),
    );
    w.append(&f, Commit::Immediate)?;
    frames_written += 1;

    let digest = w.seal(None)?;
    store.advance_chain(digest);
    segments_sealed += 1;

    Ok(ExitSummary {
        exit_code,
        frames_written,
        segments_sealed,
    })
}

/// Minimal base64 (standard alphabet, padded) — 20 lines beats a dependency
/// for encode-only.
mod base64_engine {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    pub fn encode_b64(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            out.push(TABLE[(n >> 18) as usize & 63] as char);
            out.push(TABLE[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
            out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::{SegmentReader, SegmentState};
    use crate::verify::verify_voyage;

    fn config(dir: &std::path::Path, name: &str, argv: &[&str]) -> CapsuleConfig {
        CapsuleConfig {
            voyage_root: dir.join(name),
            voyage_id: name.to_string(),
            retention: RetentionClass::Discard,
            producer_kind: "test-shell".into(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            echo: false,
        }
    }

    fn sealed_frames(root: &std::path::Path, voyage: &str) -> Vec<Envelope> {
        let seg_dir = root.join("seg");
        let mut out = Vec::new();
        let mut names: Vec<String> = std::fs::read_dir(&seg_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for n in names {
            if n.ends_with(".sotseg") {
                let r = SegmentReader::read(&seg_dir.join(&n), true).unwrap();
                assert_eq!(r.header.voyage_id, voyage);
                out.extend(r.frames);
            }
        }
        let _ = SegmentState::Sealed;
        out
    }

    #[test]
    fn echo_producer_records_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path(), "cap1", &["/bin/sh", "-c", "printf sealed-cargo; exit 3"]);
        let root = cfg.voyage_root.clone();
        let summary = run(cfg).unwrap();
        assert_eq!(summary.exit_code, Some(3));
        assert_eq!(summary.segments_sealed, 1);
        verify_voyage(&root, "cap1").unwrap();

        let frames = sealed_frames(&root, "cap1");
        // Output made it into producer frames (b64 of the full stream
        // contains our marker once concatenated and decoded).
        let mut all = Vec::new();
        for f in &frames {
            if f.class == Class::Producer {
                let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
                all.extend(decode_b64(b64));
            }
        }
        let text = String::from_utf8_lossy(&all);
        assert!(text.contains("sealed-cargo"), "got: {text:?}");
        // Raw terminal: zero turn frames; dead lifecycle carries the code.
        assert!(frames.iter().all(|f| f.class != Class::TurnOpen && f.class != Class::TurnClose));
        let dead = frames
            .iter()
            .find(|f| {
                f.class == Class::Lifecycle
                    && f.payload.as_ref().unwrap()["kind"] == "producer_dead"
            })
            .unwrap();
        assert_eq!(dead.payload.as_ref().unwrap()["detail"]["exit_code"], 3);
    }

    /// Unit-level WAL shape check: drive the frame factory + writer directly
    /// (no child), then verify the voyage — proves the input WAL frames the
    /// capsule writes satisfy the verifier's lattice + matrix rules.
    #[test]
    fn wal_frames_shape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cap3");
        VoyageStore::bootstrap(&root, "cap3", RetentionClass::Discard).unwrap();
        let mut store = VoyageStore::open_for_writing(&root, "cap3").unwrap();
        let mut ctx = FrameCtx {
            epoch: store.epoch,
            next_n: 1,
            t0: Instant::now(),
            take_epoch: 0,
            attached: None,
        };
        let mut w = store.open_segment(wall_ms()).unwrap();
        let f = ctx.capsule_frame(
            Class::Lifecycle,
            json!({"kind": "take_state", "take": {"take_epoch": 1, "holder": null}}),
        );
        w.append(&f, Commit::Immediate).unwrap();
        ctx.take_epoch = 2;
        let f = ctx.capsule_frame(
            Class::Lifecycle,
            json!({"kind": "take_state", "take": {"take_epoch": 2, "holder": "local"}}),
        );
        w.append(&f, Commit::Immediate).unwrap();

        let input = ctx.controller_frame(
            Class::Input,
            json!({"idem_key": random_idem_key().unwrap(), "content": "redacted", "length": 5}),
        );
        let iseq = input.seq;
        w.append(&input, Commit::Immediate).unwrap();
        let mut intent = ctx.controller_frame(
            Class::Lifecycle,
            json!({"kind": "input_fact",
                   "fact": {"input": {"epoch": iseq.epoch, "n": iseq.n}, "fact": "forward_intent"}}),
        );
        intent.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: iseq }];
        let tseq = intent.seq;
        w.append(&intent, Commit::Immediate).unwrap();
        let mut fwd = ctx.controller_frame(
            Class::Lifecycle,
            json!({"kind": "input_fact",
                   "fact": {"input": {"epoch": iseq.epoch, "n": iseq.n}, "fact": "forwarded",
                             "intent": {"epoch": tseq.epoch, "n": tseq.n}}}),
        );
        fwd.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: iseq }];
        w.append(&fwd, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        verify_voyage(&root, "cap3").unwrap();
    }

    fn decode_b64(s: &str) -> Vec<u8> {
        // Test-only decoder for the encode-only engine above.
        let val = |c: u8| -> u32 {
            match c {
                b'A'..=b'Z' => (c - b'A') as u32,
                b'a'..=b'z' => (c - b'a' + 26) as u32,
                b'0'..=b'9' => (c - b'0' + 52) as u32,
                b'+' => 62,
                b'/' => 63,
                _ => 0,
            }
        };
        let bytes: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
        let mut out = Vec::new();
        for chunk in bytes.chunks(4) {
            let mut n = 0u32;
            for (i, &c) in chunk.iter().enumerate() {
                n |= val(c) << (18 - 6 * i);
            }
            out.push((n >> 16) as u8);
            if chunk.len() > 2 {
                out.push((n >> 8) as u8);
            }
            if chunk.len() > 3 {
                out.push(n as u8);
            }
        }
        out
    }
}
