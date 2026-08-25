//! The Claude producer adapter (ADR 0040): records a Claude agent session —
//! via the `adapters/claude-sdk-helper` Node process — as an ADR 0039
//! voyage with real turns.
//!
//! Architecture recap (the ADR is normative): the HELPER is the epoch's
//! logical producer; one SDK query per operator turn; every helper NDJSON
//! line is a semantic-JSON producer frame; turn attribution follows six
//! ordered rules over a tool_use index; protocol corruption is TERMINAL;
//! the helper runs inside a nested cgroup whose locator is recorded before
//! spawn (feature `sot.capsule.cgroup-fence-v1`), and death is recorded
//! only after `cgroup.kill` + `populated=0` prove quiescence.

#![cfg(all(unix, target_os = "linux"))]

use crate::envelope::*;
use crate::segment::{Commit, RetentionClass, SegmentIdentity, SegmentReader, SegmentState};
use crate::voyage::VoyageStore;
use crate::{Error, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const HELPER_PROTOCOL: u64 = 1;
/// Features this adapter's segments declare. `json-f64-v1` always (semantic
/// JSON is the declared native representation); `cgroup-fence-v1` only when
/// the fence actually bears authority — an unfenced test rig's spawn frame
/// carries `{"scheme": "none"}` and its segments must verify WITHOUT the
/// feature (ADR 0039 registry, locator-must-declare).
pub fn features(fence: &Fence) -> Vec<String> {
    let mut f = vec!["sot.producer.json-f64-v1".to_string()];
    if matches!(fence, Fence::Cgroup(_)) {
        f.push("sot.capsule.cgroup-fence-v1".to_string());
    }
    f
}
const LINE_CAP: usize = 8 * 1024 * 1024;
const QUIESCENCE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct ClaudeConfig {
    pub voyage_root: PathBuf,
    pub voyage_id: String,
    pub retention: RetentionClass,
    /// Command that starts the helper (e.g. ["node", ".../dist/src/main.js"]).
    pub helper_argv: Vec<String>,
    /// The pinned SDK version the helper is expected to report in `hello`.
    pub expected_sdk_version: String,
    /// The kill-domain fence. Production callers use [`Fence::discover`];
    /// tests without cgroup delegation use [`Fence::test_unfenced`] (marked
    /// non-durable — the recording rig, not a real deployment).
    pub fence: Fence,
}

/// The kill domain (ADR 0040 §Kill domain).
pub enum Fence {
    /// A nested cgroup v2 dir. Created empty; the helper is moved in by its
    /// own pre-exec before any helper code runs.
    Cgroup(PathBuf),
    /// Test-only: no kernel fence. Refused outside test rigs by
    /// construction (the only constructor is `test_unfenced`).
    Unfenced,
}

impl Fence {
    /// Discover the delegated cgroup root from the unified hierarchy and
    /// create a nested producer cgroup. FAILS CLOSED without delegation.
    pub fn discover(tag: &str) -> Result<Fence> {
        let selfcg = std::fs::read_to_string("/proc/self/cgroup")?;
        let rel = selfcg
            .lines()
            .find_map(|l| l.strip_prefix("0::"))
            .ok_or_else(|| Error::State("no unified cgroup hierarchy".into()))?
            .trim();
        let own = PathBuf::from(format!("/sys/fs/cgroup{rel}"));
        let nested = own.join(format!("sot-producer-{tag}"));
        std::fs::create_dir(&nested).map_err(|e| {
            Error::State(format!(
                "cgroup delegation unavailable at {own:?} ({e}) — the claude adapter fails closed without a kill domain (ADR 0040)"
            ))
        })?;
        Ok(Fence::Cgroup(nested))
    }

    pub fn test_unfenced() -> Fence {
        Fence::Unfenced
    }

    /// The discriminated kill-domain locator (ADR 0039 registry). Scheme
    /// "cgroup" bears authority — successor epochs act on it destructively
    /// and the verifier requires the segment to declare `cgroup-fence-v1`;
    /// scheme "none" is the explicit no-authority record of a test rig.
    fn locator(&self) -> Value {
        match self {
            Fence::Cgroup(p) => json!({"scheme": "cgroup", "path": p.to_string_lossy()}),
            Fence::Unfenced => json!({"scheme": "none"}),
        }
    }

    /// Terminate the domain and PROVE quiescence (populated=0), per the ADR
    /// ordering: only after this may producer_dead be recorded.
    fn kill_and_wait(&self) -> Result<()> {
        match self {
            Fence::Unfenced => Ok(()),
            Fence::Cgroup(dir) => {
                let _ = std::fs::write(dir.join("cgroup.kill"), "1");
                let deadline = Instant::now() + QUIESCENCE_TIMEOUT;
                loop {
                    let ev = std::fs::read_to_string(dir.join("cgroup.events")).unwrap_or_default();
                    if ev.lines().any(|l| l.trim() == "populated 0") || ev.is_empty() {
                        break;
                    }
                    if Instant::now() > deadline {
                        return Err(Error::State("kill domain did not quiesce".into()));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                let _ = std::fs::remove_dir(dir);
                Ok(())
            }
        }
    }
}

#[derive(Debug)]
pub struct ClaudeSummary {
    pub turns: u64,
    pub frames_written: u64,
    pub terminal_reason: String,
    /// Frames left turn-free because a correlation id did not resolve —
    /// the signal the adapter's turn model drifted from the SDK's (A2; the
    /// warning is operational by design: the lifecycle vocabulary is closed).
    pub unresolved_correlation_warnings: u64,
    /// Operator turns refused (busy/draining/pre-ready) — each recorded as a
    /// bare input frame whose {input}-only lattice chain MEANS "recorded,
    /// never attempted" (C2: refusals are auditable, not invisible).
    pub refused_turns: u64,
}

enum Event {
    HelperLine(String),
    HelperStderr(Vec<u8>),
    HelperOversize,
    HelperEof,
    Operator(OperatorCmd),
}

/// Read one \n-terminated line with the cap enforced DURING the read (C1:
/// `read_line` grows unboundedly before any post-hoc check — a hostile line
/// must not OOM the one process that has to survive to write the log).
fn read_capped_line(r: &mut impl BufRead, out: &mut Vec<u8>) -> std::io::Result<CappedRead> {
    out.clear();
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Ok(if out.is_empty() { CappedRead::Eof } else { CappedRead::Line });
        }
        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            if out.len() + pos > LINE_CAP {
                r.consume(pos + 1);
                return Ok(CappedRead::CapBreached);
            }
            out.extend_from_slice(&buf[..pos]);
            r.consume(pos + 1);
            return Ok(CappedRead::Line);
        }
        let take = buf.len();
        if out.len() + take > LINE_CAP {
            r.consume(take);
            return Ok(CappedRead::CapBreached);
        }
        out.extend_from_slice(buf);
        r.consume(take);
    }
}

enum CappedRead {
    Line,
    Eof,
    CapBreached,
}

/// v1 operator surface: turns + interrupt + shutdown, from the capsule's
/// stdin (one JSON per line: {"turn": text} | {"interrupt": true} |
/// {"shutdown": true}) — the take-holding local controller.
pub enum OperatorCmd {
    Turn(String),
    Interrupt,
    Shutdown,
}

fn wall_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn random_hex32() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(std::io::Error::from)?;
    Ok(b.iter().map(|x| format!("{:02x}", x)).collect())
}

/// Frame factory sharing P1's shape, plus the claude adapter's refs.
struct Fx {
    epoch: u64,
    next_n: u64,
    t0: Instant,
    take_epoch: u64,
    attached: Option<Seq>,
}

impl Fx {
    fn seq(&mut self) -> Seq {
        let s = Seq { epoch: self.epoch, n: self.next_n };
        self.next_n += 1;
        s
    }
    fn base(&mut self, class: Class, payload: Value) -> Envelope {
        Envelope {
            seq: self.seq(),
            class,
            source: Source {
                emitter: Emitter::Capsule,
                actor: Actor { kind: ActorKind::Unknown, controller_id: None, take_epoch: None },
                derivation: Derivation::Synthetic,
            },
            t_wall_ms: wall_ms(),
            t_mono_us: self.t0.elapsed().as_micros() as u64,
            stream: None,
            transformed: None,
            refs: vec![],
            payload: Some(payload),
            payload_ref: None,
        }
    }
    fn controller(&mut self, class: Class, payload: Value) -> Envelope {
        let mut e = self.base(class, payload);
        e.source.actor = Actor {
            kind: ActorKind::Controller,
            controller_id: Some("local".into()),
            take_epoch: Some(self.take_epoch),
        };
        e
    }
    fn producer_msg(&mut self, body: Value, turn: Option<Seq>) -> Envelope {
        let mut e = self.base(Class::Producer, body);
        e.source.emitter = Emitter::Producer;
        e.source.actor.kind = ActorKind::Producer;
        e.source.derivation = Derivation::Native;
        e.refs = vec![FrameRef { kind: RefKind::AttachedTo, frame: self.attached.expect("attached") }];
        if let Some(t) = turn {
            e.refs.push(FrameRef { kind: RefKind::CausedBy, frame: t });
        }
        e
    }
}

/// Attribution per ADR 0040's six ordered rules. Returns (turn ref or None,
/// whether this is the current turn's RESULT, whether this is an operator
/// echo needing redaction, or a terminal protocol error).
enum Attribution {
    Frame { turn: Option<Seq>, is_result: bool, is_echo: bool, warn_unresolved: bool },
    UnknownType,
    Terminal(String),
}

struct TurnState {
    open_seq: Seq,
    query_id: u64,
    saw_result: bool,
}

fn attribute(
    body: &Value,
    current: Option<&TurnState>,
    tool_index: &mut HashMap<String, Seq>,
) -> Attribution {
    let ty = body.get("type").and_then(Value::as_str);
    let subtype = body.get("subtype").and_then(Value::as_str);

    // Collect correlation ids: parent_tool_use_id (present-but-null = absent)
    // + every tool_result block's tool_use_id.
    let mut corr: Vec<String> = vec![];
    if let Some(p) = body.get("parent_tool_use_id").and_then(Value::as_str) {
        corr.push(p.to_string());
    }
    let content = body.pointer("/message/content").and_then(Value::as_array);
    let mut has_tool_result = false;
    if let Some(blocks) = content {
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("tool_result") => {
                    has_tool_result = true;
                    if let Some(id) = b.get("tool_use_id").and_then(Value::as_str) {
                        corr.push(id.to_string());
                    }
                }
                Some("tool_use") => {} // indexed by the caller AFTER attribution
                _ => {}
            }
        }
    }

    match ty {
        // Rule 1: the current query's top-level result closes its turn.
        Some("result") if corr.is_empty() => match current {
            Some(t) => Attribution::Frame { turn: Some(t.open_seq), is_result: true, is_echo: false, warn_unresolved: false },
            None => Attribution::Terminal("result with no open query-turn".into()),
        },
        // Rule 3: session-scoped types are turn-free.
        Some("system") => match subtype {
            Some("init" | "compact_boundary" | "informational" | "worker_shutting_down"
                | "api_retry" | "rate_limit") =>
                Attribution::Frame { turn: None, is_result: false, is_echo: false, warn_unresolved: false },
            _ => Attribution::UnknownType,
        },
        Some("assistant") | Some("user") | Some("result") => {
            // Rule 2: ALL correlation ids resolve first; ANY disagreement is
            // terminal regardless of id order (review A1 — the old
            // first-unresolved short-circuit let block order pick the
            // verdict); any unresolved id => turn-free + WARN (A2).
            if !corr.is_empty() {
                let mut resolved: Option<Seq> = None;
                let mut any_unresolved = false;
                for id in &corr {
                    match tool_index.get(id) {
                        Some(t) => match resolved {
                            None => resolved = Some(*t),
                            Some(r) if r == *t => {}
                            Some(_) => {
                                return Attribution::Terminal(format!(
                                    "correlation ids disagree on the owning turn ({id})"
                                ))
                            }
                        },
                        None => any_unresolved = true,
                    }
                }
                if any_unresolved {
                    return Attribution::Frame { turn: None, is_result: false, is_echo: false, warn_unresolved: true };
                }
                return Attribution::Frame { turn: resolved, is_result: false, is_echo: false, warn_unresolved: false };
            }
            // Rule 4: operator echo (user role, no tool_result blocks).
            if ty == Some("user") && !has_tool_result {
                return Attribution::Frame { turn: None, is_result: false, is_echo: true, warn_unresolved: false };
            }
            // Rule 5: known mainline message -> the current query's turn.
            match current {
                Some(t) => Attribution::Frame { turn: Some(t.open_seq), is_result: false, is_echo: false, warn_unresolved: false },
                None => Attribution::Frame { turn: None, is_result: false, is_echo: false, warn_unresolved: false },
            }
        }
        // Rule 6: unknown type.
        _ => Attribution::UnknownType,
    }
}

/// Index every tool_use block of a turn-attributed assistant message.
/// Collision => terminal (first-write-wins was deleted in review).
fn index_tool_uses(body: &Value, turn: Seq, tool_index: &mut HashMap<String, Seq>) -> Result<()> {
    if let Some(blocks) = body.pointer("/message/content").and_then(Value::as_array) {
        for b in blocks {
            if b.get("type").and_then(Value::as_str) == Some("tool_use") {
                if let Some(id) = b.get("id").and_then(Value::as_str) {
                    if let Some(prev) = tool_index.insert(id.to_string(), turn) {
                        if prev != turn {
                            return Err(Error::State(format!("tool_use id collision: {id}")));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Redact an operator-echo user message per "claude-sdk-default": every
/// content block's operator-supplied fields replaced, with concrete ops.
fn redact_echo(body: &Value) -> (Value, Vec<Value>) {
    let mut out = body.clone();
    let mut ops = vec![];
    if let Some(blocks) = out.pointer_mut("/message/content") {
        match blocks {
            Value::Array(arr) => {
                for (i, b) in arr.iter_mut().enumerate() {
                    *b = json!({"type": b.get("type").cloned().unwrap_or(json!("unknown")),
                                 "redacted": "[redacted:input]"});
                    ops.push(json!({"op": "redact_field",
                                     "path": format!("/message/content/{i}")}));
                }
            }
            other => {
                *other = json!("[redacted:input]");
                ops.push(json!({"op": "redact_field", "path": "/message/content"}));
            }
        }
    }
    (out, ops)
}

/// Successor closure (ADR 0040): find unmatched turn_opens in retained
/// sealed history; the caller appends one synthesized_death close each.
fn unmatched_opens(root: &Path, voyage_id: &str) -> Result<Vec<Seq>> {
    let seg_dir = root.join("seg");
    let mut opens: HashMap<(u64, u64), Seq> = HashMap::new();
    let mut closed: Vec<(u64, u64)> = vec![];
    let mut names: Vec<String> = std::fs::read_dir(&seg_dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .collect();
    names.sort();
    for name in names {
        let Some((idx, ep, state)) = SegmentIdentity::parse_file_name(&name) else { continue };
        if state != SegmentState::Sealed {
            continue;
        }
        let id = SegmentIdentity { voyage_id: voyage_id.into(), segment_index: idx, epoch: ep };
        let r = SegmentReader::read(&id.path(&seg_dir, state), true)?;
        for f in &r.frames {
            if f.class == Class::TurnOpen {
                opens.insert((f.seq.epoch, f.seq.n), f.seq);
            }
            if f.class == Class::TurnClose {
                if let Some(t) = f.refs.iter().find(|r| r.kind == RefKind::CausedBy) {
                    if !f.refs.iter().any(|r| r.kind == RefKind::DuplicateOf) {
                        closed.push((t.frame.epoch, t.frame.n));
                    }
                }
            }
        }
    }
    for c in closed {
        opens.remove(&c);
    }
    let mut v: Vec<Seq> = opens.into_values().collect();
    v.sort_unstable_by_key(|s| (s.epoch, s.n));
    Ok(v)
}

/// Run one Claude producer leg. `operator` is the command stream (stdin in
/// the binary; a channel in tests).
pub fn run(config: ClaudeConfig, operator: mpsc::Receiver<OperatorCmd>) -> Result<ClaudeSummary> {
    if !config.voyage_root.exists() {
        VoyageStore::bootstrap(&config.voyage_root, &config.voyage_id, config.retention)?;
    }
    let mut store = VoyageStore::open_for_writing(&config.voyage_root, &config.voyage_id)?;
    store.seal_survivor()?;
    let prior_take = store.last_take_epoch;
    let stale_opens = unmatched_opens(&config.voyage_root, &config.voyage_id)?;

    let mut fx = Fx {
        epoch: store.epoch,
        next_n: 1,
        t0: Instant::now(),
        take_epoch: 0,
        attached: None,
    };
    let mut w = store.open_segment_with_features(wall_ms(), features(&config.fence))?;
    let mut frames: u64 = 0;
    macro_rules! put {
        ($e:expr) => {{
            w.append(&$e, Commit::Immediate)?;
            frames += 1;
        }};
    }

    // Take: revoke-first, then grant local (P1 protocol).
    let f = fx.base(Class::Lifecycle,
        json!({"kind": "take_state", "take": {"take_epoch": prior_take + 1, "holder": null}}));
    put!(f);
    fx.take_epoch = prior_take + 2;
    let f = fx.base(Class::Lifecycle,
        json!({"kind": "take_state", "take": {"take_epoch": fx.take_epoch, "holder": "local"}}));
    put!(f);

    // Successor closure BEFORE anything else may happen (ADR 0040).
    for open in stale_opens {
        let mut c = fx.base(Class::TurnClose, json!({"reason": "synthesized_death"}));
        c.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: open }];
        put!(c);
    }

    // producer_attached from PRE-SPAWN facts only.
    let rules = json!({"input_content": "redacted", "echo": "typed-block-redaction",
                        "config_values": "redacted", "turns": "admission-rule"});
    let rules_sha: String = {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(serde_json::to_vec(&rules)?);
        h.finalize().iter().map(|b| format!("{:02x}", b)).collect()
    };
    let att = fx.base(Class::ProducerAttached, json!({
        "producer_kind": "claude-sdk",
        "version": config.expected_sdk_version,
        "profile_def": {"id": "claude-sdk-default", "sha256": rules_sha, "rules": rules},
    }));
    let att_seq = att.seq;
    put!(att);
    fx.attached = Some(att_seq);

    // producer_spawn: allowlisted config + the authority-bearing locator.
    let f = fx.base(Class::Lifecycle, json!({
        "kind": "producer_spawn",
        "detail": {"helper": "claude-sdk-helper", "protocol": HELPER_PROTOCOL,
                    "kill_domain": config.fence.locator(),
                    "argv0": config.helper_argv.first().cloned().unwrap_or_default()},
    }));
    put!(f);

    // Spawn the helper — pre_exec moves the child into the nested cgroup
    // BEFORE any helper code runs, then close_range (the P1 lesson).
    let mut cmd = std::process::Command::new(&config.helper_argv[0]);
    cmd.args(&config.helper_argv[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Fence::Cgroup(dir) = &config.fence {
        let procs = std::ffi::CString::new(dir.join("cgroup.procs").to_string_lossy().as_bytes())
            .map_err(|_| Error::State("nul in cgroup path".into()))?;
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(move || {
                let fd = libc::open(procs.as_ptr(), libc::O_WRONLY);
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::write(fd, b"0\n".as_ptr().cast(), 2) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(fd);
                if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    } else {
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn()?;

    let (tx, rx) = mpsc::channel::<Event>();
    {
        let tx = tx.clone();
        let out = child.stdout.take().expect("piped");
        std::thread::spawn(move || {
            let mut r = BufReader::new(out);
            let mut buf: Vec<u8> = Vec::new();
            loop {
                match read_capped_line(&mut r, &mut buf) {
                    Err(_) | Ok(CappedRead::Eof) => break,
                    Ok(CappedRead::CapBreached) => {
                        let _ = tx.send(Event::HelperOversize);
                        break;
                    }
                    Ok(CappedRead::Line) => {
                        let line = String::from_utf8_lossy(&buf).into_owned();
                        if tx.send(Event::HelperLine(line)).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = tx.send(Event::HelperEof);
        });
    }
    {
        let tx = tx.clone();
        let errs = child.stderr.take().expect("piped");
        std::thread::spawn(move || {
            let mut r = BufReader::new(errs);
            let mut buf = [0u8; 8192];
            while let Ok(n) = r.read(&mut buf) {
                if n == 0 || tx.send(Event::HelperStderr(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        });
    }
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            while let Ok(cmd) = operator.recv() {
                if tx.send(Event::Operator(cmd)).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let mut helper_in = child.stdin.take().expect("piped");

    // ---- the writer loop ----
    let mut hello_seen = false;
    let mut turn: Option<TurnState> = None;
    let mut tool_index: HashMap<String, Seq> = HashMap::new();
    let mut turns_done: u64 = 0;
    let mut next_query_id: u64 = 1;
    let mut next_interrupt_id: u64 = 1;
    let mut pending_interrupt_req: Option<(u64, Seq)> = None;
    let mut interrupt_answered_this_turn = false;
    let mut terminal_reason = String::from("shutdown");
    let mut refusing_input = false;
    let mut warnings: u64 = 0;
    let mut refused: u64 = 0;
    const LIVENESS_BOUND: Duration = Duration::from_secs(600);

    'main: loop {
        let bounded = turn.is_some() || refusing_input;
        let ev = if bounded {
            match rx.recv_timeout(LIVENESS_BOUND) {
                Ok(e) => e,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    terminal_reason = "helper silent past the liveness bound mid-turn".into();
                    break 'main;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break 'main,
            }
        } else {
            match rx.recv() {
                Ok(e) => e,
                Err(_) => break 'main,
            }
        };
        match ev {
            Event::HelperStderr(bytes) => {
                // Capture-off posture: presence + length only (ADR 0040).
                let mut e = fx.base(Class::Producer, json!({"stderr_len": bytes.len()}));
                e.source.emitter = Emitter::Producer;
                e.source.actor.kind = ActorKind::Producer;
                e.source.derivation = Derivation::Native;
                e.refs = vec![FrameRef { kind: RefKind::AttachedTo, frame: att_seq }];
                e.transformed = Some(Transformed {
                    ops: vec![TransformOp {
                        op: TransformKind::RedactField,
                        path: "/stderr".into(),
                        note: Some("capture-off: presence+length".into()),
                    }],
                });
                put!(e);
            }
            Event::HelperLine(line) => {
                let parsed: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => {
                        terminal_reason = "helper protocol: unparseable line".into();
                        break 'main;
                    }
                };
                let ev_name = parsed.get("ev").and_then(Value::as_str).unwrap_or("");
                match ev_name {
                    "hello" => {
                        if hello_seen
                            || parsed.get("protocol").and_then(Value::as_u64) != Some(HELPER_PROTOCOL)
                        {
                            terminal_reason = "helper protocol: bad hello".into();
                            break 'main;
                        }
                        // B1: the attested producer version must be the one
                        // actually running — attestation is the point.
                        let reported = parsed.get("sdk_version").and_then(Value::as_str).unwrap_or("");
                        if reported != config.expected_sdk_version {
                            terminal_reason = format!(
                                "sdk version mismatch: attested {:?}, helper reports {:?}",
                                config.expected_sdk_version, reported
                            );
                            break 'main;
                        }
                        hello_seen = true;
                        let f = fx.base(Class::Lifecycle, json!({"kind": "producer_ready"}));
                        put!(f);
                    }
                    "msg" if hello_seen => {
                        let body = parsed.get("body").cloned().unwrap_or(Value::Null);
                        match attribute(&body, turn.as_ref(), &mut tool_index) {
                            Attribution::Terminal(why) => {
                                terminal_reason = why;
                                break 'main;
                            }
                            Attribution::UnknownType => {
                                // Rule 6: record turn-free redacted, refuse
                                // new turns, drain current, then terminal.
                                let mut e = fx.producer_msg(
                                    json!({"unknown_type": true, "len": line.len()}),
                                    None,
                                );
                                e.transformed = Some(Transformed {
                                    ops: vec![TransformOp {
                                        op: TransformKind::RedactField,
                                        path: "/".into(),
                                        note: Some("unknown type under capture-off".into()),
                                    }],
                                });
                                put!(e);
                                refusing_input = true;
                                if turn.is_none() {
                                    terminal_reason = "unknown producer message type".into();
                                    break 'main;
                                }
                            }
                            Attribution::Frame { turn: t, is_result, is_echo, warn_unresolved } => {
                                if warn_unresolved {
                                    warnings += 1; // operational signal (A2)
                                }
                                // B3: a second top-level result for the same
                                // turn must never yield a second close — the
                                // capsule's invariant is "never emit a log
                                // that fails verify".
                                if is_result && turn.as_ref().map(|ts| ts.saw_result) == Some(true) {
                                    terminal_reason = "second result within one turn".into();
                                    break 'main;
                                }
                                let assigned = t;
                                let (body_out, transformed) = if is_echo {
                                    let (b, ops) = redact_echo(&body);
                                    (b, Some(ops))
                                } else {
                                    (body.clone(), None)
                                };
                                let mut e = fx.producer_msg(body_out, assigned);
                                if let Some(ops) = transformed {
                                    e.transformed = Some(Transformed {
                                        ops: ops
                                            .into_iter()
                                            .map(|o| TransformOp {
                                                op: TransformKind::RedactField,
                                                path: o["path"].as_str().unwrap_or("/").into(),
                                                note: None,
                                            })
                                            .collect(),
                                    });
                                }
                                put!(e);
                                if let Some(t) = assigned {
                                    if let Err(e) = index_tool_uses(&body, t, &mut tool_index) {
                                        terminal_reason = format!("{e}");
                                        break 'main;
                                    }
                                }
                                if is_result {
                                    let ts = turn.as_mut().expect("result implies turn");
                                    ts.saw_result = true;
                                    let subtype_ok = body.get("subtype").and_then(Value::as_str)
                                        == Some("success");
                                    let mut c = fx.base(
                                        Class::TurnClose,
                                        json!({"reason": if subtype_ok { "producer_done" } else { "failed" }}),
                                    );
                                    c.refs = vec![FrameRef {
                                        kind: RefKind::CausedBy,
                                        frame: ts.open_seq,
                                    }];
                                    put!(c);
                                }
                            }
                        }
                    }
                    "turn_end" if hello_seen => {
                        let qid = parsed.get("query_id").and_then(Value::as_u64);
                        let results = parsed.get("results").and_then(Value::as_u64);
                        match turn.take() {
                            None => {
                                // B4b: its own reason, not "without a result".
                                terminal_reason = "turn_end with no open turn".into();
                                break 'main;
                            }
                            Some(ts) if qid != Some(ts.query_id) => {
                                let _ = ts;
                                terminal_reason = "turn_end query_id mismatch".into();
                                break 'main;
                            }
                            Some(ts) if ts.saw_result => {
                                turns_done += 1;
                                interrupt_answered_this_turn = false;
                                if refusing_input {
                                    terminal_reason = "unknown producer message type (drained)".into();
                                    break 'main;
                                }
                            }
                            Some(ts) if results == Some(0) && interrupt_answered_this_turn => {
                                // H2: an interrupted query may exhaust with
                                // no result — the helper reports it honestly
                                // and the adapter closes the turn as
                                // interrupted, then emits the OUTCOME frame
                                // (disposition now known — the ADR's third
                                // exchange frame).
                                let mut c = fx.base(Class::TurnClose, json!({"reason": "interrupted"}));
                                c.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: ts.open_seq }];
                                put!(c);
                                let out = fx.controller(Class::ControlExchange, json!({
                                    "phase": "outcome", "kind_ns": "claude-sdk/interrupt",
                                    "scope": "turn",
                                    "target": format!("{}:{}", ts.open_seq.epoch, ts.open_seq.n),
                                    "body": {"disposition": "interrupted"}}));
                                put!(out);
                                turns_done += 1;
                                interrupt_answered_this_turn = false;
                            }
                            Some(_) => {
                                terminal_reason = "turn_end without a result".into();
                                break 'main;
                            }
                        }
                    }
                    "interrupted" if hello_seen => {
                        if let Some((id, req_seq)) = pending_interrupt_req.take() {
                            if parsed.get("id").and_then(Value::as_u64) != Some(id) {
                                terminal_reason = "interrupted id mismatch".into();
                                break 'main;
                            }
                            interrupt_answered_this_turn = turn.is_some();
                            let mut resp = fx.controller(
                                Class::ControlExchange,
                                json!({"phase": "response", "kind_ns": "claude-sdk/interrupt",
                                        "body": {"ok": parsed.get("ok").cloned().unwrap_or(json!(false)),
                                                  "sdk_return": parsed.get("sdk_return").cloned().unwrap_or(Value::Null),
                                                  "note": "adapter-derived"}}),
                            );
                            resp.refs = vec![FrameRef { kind: RefKind::RespondsTo, frame: req_seq }];
                            put!(resp);
                        } else {
                            terminal_reason = "unsolicited interrupted event".into();
                            break 'main;
                        }
                    }
                    "msg" | "turn_end" | "interrupted" => {
                        // Known ev, wrong time (pre-hello): B4 legibility.
                        terminal_reason = format!("helper protocol: {ev_name} before hello");
                        break 'main;
                    }
                    "fatal" => {
                        terminal_reason = format!(
                            "helper fatal: {}",
                            parsed.get("reason").and_then(Value::as_str).unwrap_or("?")
                        );
                        break 'main;
                    }
                    _ => {
                        terminal_reason = "helper protocol: unknown ev".into();
                        break 'main;
                    }
                }
            }
            Event::HelperOversize => {
                terminal_reason = "helper line cap breached".into();
                break 'main;
            }
            Event::HelperEof => {
                terminal_reason = if turn.is_some() {
                    "helper died mid-turn".into()
                } else {
                    "helper exited".into()
                };
                break 'main;
            }
            Event::Operator(OperatorCmd::Shutdown) => {
                let _ = serde_json::to_writer(&mut helper_in, &json!({"op": "shutdown"}));
                let _ = helper_in.write_all(b"\n");
                terminal_reason = "shutdown".into();
                break 'main;
            }
            Event::Operator(OperatorCmd::Interrupt) => {
                let id = next_interrupt_id;
                next_interrupt_id += 1;
                let req = fx.controller(
                    Class::ControlExchange,
                    json!({"phase": "request", "kind_ns": "claude-sdk/interrupt",
                            "to": {"kind": "producer"}, "body": {"id": id}}),
                );
                let req_seq = req.seq;
                put!(req); // fsynced BEFORE the op is sent (Commit::Immediate)
                pending_interrupt_req = Some((id, req_seq));
                let _ = serde_json::to_writer(&mut helper_in, &json!({"op": "interrupt", "id": id}));
                let _ = helper_in.write_all(b"\n");
            }
            Event::Operator(OperatorCmd::Turn(text)) => {
                if turn.is_some() || refusing_input || !hello_seen {
                    // Refused at the boundary (queued input was deleted in
                    // review) — but RECORDED (C2): a bare input frame with
                    // no forward_intent is the lattice's honest encoding of
                    // "received, never attempted".
                    let input = fx.controller(
                        Class::Input,
                        json!({"idem_key": random_hex32()?, "content": "redacted",
                                "length": text.len()}),
                    );
                    put!(input);
                    refused += 1;
                    continue;
                }
                // WAL + turn order per ADR 0040:
                // input -> intent -> turn_open(responds_to) -> fence recheck
                // -> write -> forwarded.
                let input = fx.controller(
                    Class::Input,
                    json!({"idem_key": random_hex32()?, "content": "redacted",
                            "length": text.len()}),
                );
                let input_seq = input.seq;
                put!(input);
                let mut intent = fx.controller(
                    Class::Lifecycle,
                    json!({"kind": "input_fact",
                            "fact": {"input": {"epoch": input_seq.epoch, "n": input_seq.n},
                                      "fact": "forward_intent"}}),
                );
                intent.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: input_seq }];
                let intent_seq = intent.seq;
                put!(intent);
                let mut open = fx.base(Class::TurnOpen,
                    json!({"admitted_by": "claude-sdk/user-turn-submitted"}));
                open.refs = vec![FrameRef { kind: RefKind::RespondsTo, frame: input_seq }];
                let open_seq = open.seq;
                put!(open);
                // Fence recheck: single local controller, epoch unchanged by
                // construction — coded as an assert-equivalent.
                let query_id = next_query_id;
                next_query_id += 1;
                let wrote: std::io::Result<()> = (|| {
                    let line = serde_json::to_string(
                        &json!({"op": "user_turn", "query_id": query_id, "text": text}))?;
                    helper_in.write_all(line.as_bytes())?;
                    helper_in.write_all(b"\n")
                })();
                match wrote {
                    Ok(()) => {
                        let mut fwd = fx.controller(
                            Class::Lifecycle,
                            json!({"kind": "input_fact",
                                    "fact": {"input": {"epoch": input_seq.epoch, "n": input_seq.n},
                                              "fact": "forwarded",
                                              "intent": {"epoch": intent_seq.epoch, "n": intent_seq.n}}}),
                        );
                        fwd.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: input_seq }];
                        put!(fwd);
                        turn = Some(TurnState { open_seq, query_id, saw_result: false });
                    }
                    Err(_) => {
                        // Delivery-unknown (never refused after intent);
                        // the admitted turn closes failed.
                        let mut c = fx.base(Class::TurnClose, json!({"reason": "failed"}));
                        c.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: open_seq }];
                        put!(c);
                        terminal_reason = "forward write failed".into();
                        break 'main;
                    }
                }
            }
        }
    }

    // Termination order (ADR 0040): refuse input (loop exited) -> kill the
    // domain -> quiescence proven -> reap -> THEN producer_dead + closes -> seal.
    drop(helper_in);
    config.fence.kill_and_wait()?;
    let _ = child.kill();
    let _ = child.wait();
    if let Some(ts) = turn.take() {
        // Only synthesize a close for a turn that never got one — a turn
        // whose result already closed it (e.g. terminal fired between the
        // close and turn_end) must not receive a second close (the same
        // invariant review B3 demanded of the msg path).
        if !ts.saw_result {
            let mut c = fx.base(Class::TurnClose, json!({"reason": "synthesized_death"}));
            c.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: ts.open_seq }];
            w.append(&c, Commit::Immediate)?;
            frames += 1;
        }
    }
    let f = fx.base(Class::Lifecycle,
        json!({"kind": "producer_dead", "detail": {"reason": terminal_reason.clone()}}));
    w.append(&f, Commit::Immediate)?;
    frames += 1;
    let digest = w.seal(None)?;
    store.advance_chain(digest);

    Ok(ClaudeSummary {
        turns: turns_done,
        frames_written: frames,
        terminal_reason,
        unresolved_correlation_warnings: warnings,
        refused_turns: refused,
    })
}
