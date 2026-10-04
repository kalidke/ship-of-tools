//! Capsule tests that exercise real Windows mechanism (ConPTY, job objects).

    use super::*;

/// Test 2: spawn failure (a nonexistent executable) is compensated, not
/// escaped unsealed (the Linux capsule's own known gap, deliberately not
/// inherited here), and `producer_dead` is still the last frame recorded.
#[test]
fn spawn_failure_is_compensated() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["Z:\\sot_capsule_win_test_no_such_exe_9f31.exe".to_string()];
    let cfg = config(dir.path(), "fail1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let mut transport = no_transport();
    let summary = capsule::run::<P>(cfg, rx, &mut transport).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::SpawnFailed);
    assert_eq!(summary.exit_code, None);
    assert_eq!(summary.segments_sealed, 1);
    verify_voyage(&root, "fail1").unwrap();

    let frames = sealed_frames(&root, "fail1");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["spawn_failed"], true);
    assert!(dead["exit_code"].is_null());
}


/// Test 4: resize is an ordered request+outcome exchange (no response
/// phase — ADR 0041), rejecting out-of-budget requests rather than
/// clamping them, with the outcome's `target` naming ITS OWN request (not
/// just some real request — review finding), and `ResizePseudoConsole`
/// actually invoked exactly once (the in-budget request only — review
/// finding: the disposition string alone doesn't prove the OS call was
/// really gated). Step 5 deletes `Command::Resize` (ADR 0041 spec gate: the
/// wire lane replaces it) — this test now drives resize the same way a real
/// driver would: hello -> attach -> wait for the attach checkpoint -> take
/// -> three `resize` wire frames -> `resize_ok`/`resize_refused` replies.
#[test]
fn resize_ordered_exchange_commits_and_rejects() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()];
    let cfg = config(dir.path(), "resize1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const CONN: ConnId = 1;
    transport.open(CONN);
    transport.feed(CONN, frame::hello());
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("driver hello_ok", CONN, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(CONN, frame::attach("driver"));
    watcher.collect_checkpoint("driver checkpoint", CONN, Duration::from_secs(10));
    transport.feed(CONN, frame::take("driver"));
    watcher.wait_for("driver take_ok", CONN, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { .. })).then_some(())
    });

    transport.feed(CONN, frame::resize(100, 40)); // in budget
    let ok1 = watcher.wait_for("resize1 in-budget outcome", CONN, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
        _ => None,
    });
    transport.feed(CONN, frame::resize(9999, 40)); // > 512 cols
    let ok2 = watcher.wait_for("resize2 over-512-cols outcome", CONN, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
        _ => None,
    });
    transport.feed(CONN, frame::resize(40, 1)); // < 2 rows
    let ok3 = watcher.wait_for("resize3 under-2-rows outcome", CONN, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
        _ => None,
    });
    assert!(ok1 && !ok2 && !ok3, "expected ok, refused, refused, got {ok1} {ok2} {ok3}");

    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    assert_eq!(summary.resize_os_calls, 1, "expected exactly one ResizePseudoConsole call (the valid request only)");
    verify_voyage(&root, "resize1").unwrap();

    let frames = sealed_frames(&root, "resize1");
    let phase_is = |f: &&Envelope, phase: &str| {
        f.class == Class::ControlExchange
            && f.payload.as_ref().unwrap()["kind_ns"] == "conpty/resize"
            && f.payload.as_ref().unwrap()["phase"] == phase
    };
    let requests: Vec<&Envelope> = frames.iter().filter(|f| phase_is(f, "request")).collect();
    let outcomes: Vec<&Envelope> = frames.iter().filter(|f| phase_is(f, "outcome")).collect();
    assert_eq!(requests.len(), 3, "expected 3 resize requests, got {}", requests.len());
    assert_eq!(outcomes.len(), 3, "expected 3 resize outcomes, got {}", outcomes.len());
    assert_eq!(outcomes[0].payload.as_ref().unwrap()["body"]["disposition"], "ok");
    assert_eq!(outcomes[1].payload.as_ref().unwrap()["body"]["disposition"], "failed");
    assert_eq!(outcomes[2].payload.as_ref().unwrap()["body"]["disposition"], "failed");

    // Each outcome must target its OWN request (by emission order, since
    // request[i] and outcome[i] commit as one uninterrupted pair) — not
    // just "some" real request, which the previous version's `.any(...)`
    // would have let a misattribution bug slip through undetected.
    for (req, outcome) in requests.iter().zip(outcomes.iter()) {
        let target = outcome.payload.as_ref().unwrap()["target"].as_str().unwrap().to_string();
        let expected = format!("{}:{}", req.seq.epoch, req.seq.n);
        assert_eq!(target, expected, "outcome does not target its own request");
    }
}

/// Test 5: flood. A producer emits well beyond the 8 MiB output budget;
/// the run must drain it all to a sealed, verify-green voyage without
/// deadlocking. Whether the budget ever actually BLOCKED during the flood
/// is deliberately NOT asserted here — engagement depends on conhost's
/// burst pacing on the runner, which nothing here controls (a runner-image
/// change turned exactly that assertion red on unchanged code); the
/// blocking property is proven deterministically by OutputBudget's own
/// unit tests in capsule.rs. Run on a background thread with a LOCAL
/// bounded wait: a teardown regression here is exactly a deadlock, and
/// this test must fail loud within its own bound rather than consume the
/// whole CI job's timeout.
#[test]
fn flood_drains_to_a_sealed_voyage_without_deadlock() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let helper = HELPER_EXE.to_string();
    let total: usize = 20 * 1024 * 1024; // > the 8 MiB producer-channel budget
    let argv = vec![helper, "--flood".to_string(), total.to_string()];
    let cfg = config(dir.path(), "flood1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let start = Instant::now();
    let handle = std::thread::spawn(move || {
        let mut transport = no_transport();
        capsule::run::<P>(cfg, rx, &mut transport)
    });
    let summary = wait_for_join(handle, Duration::from_secs(60))
        .expect("run did not return within the local deadline (deadlock?)")
        .unwrap();
    eprintln!("capsule_win flood finding: {total} bytes in {:?}", start.elapsed());
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    assert_eq!(summary.exit_code, Some(ExitStatus::Code(0)));
    verify_voyage(&root, "flood1").unwrap();

    // The right side of the transform boundary (review finding): hOutput
    // is conhost's own rendered VT stream, not a byte-for-byte copy of
    // what the child wrote to its own stdout — startup sequences and
    // line-wrap/scroll handling can legitimately change the total length,
    // so exact equality against `total` proves nothing; the half-of-total
    // bound below is the honest platform-behavior assertion.

    let frames = sealed_frames(&root, "flood1");
    let mut total_decoded = 0usize;
    for f in &frames {
        if f.class == Class::Producer {
            let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
            total_decoded += decode_b64(b64).len();
        }
    }
    assert!(
        total_decoded > total / 2,
        "captured far less output than the flood emitted: {total_decoded} of {total}"
    );
}

/// Test 6: a high-bit (NTSTATUS-shaped) exit code is preserved raw and
/// unsigned all the way through `ExitSummary` AND the sealed
/// `producer_dead` frame's JSON — the review finding that a `u32`-to-`i32`
/// cast anywhere in this path would turn it negative for no reason. Same
/// value `tests/conpty.rs` pins at the primitives layer; this proves the
/// capsule runtime doesn't reintroduce the cast above it.
#[test]
fn exit_code_high_bit_status_preserved_through_producer_dead() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv =
        vec![SHELL_ARGV.to_string(), "/d".to_string(), "/c".to_string(), "exit -1073741819".to_string()];
    let cfg = config(dir.path(), "exitcode1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let mut transport = no_transport();
    let summary = capsule::run::<P>(cfg, rx, &mut transport).unwrap();
    assert_eq!(summary.exit_code, Some(ExitStatus::Code(0xC000_0005)));
    verify_voyage(&root, "exitcode1").unwrap();
    let frames = sealed_frames(&root, "exitcode1");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["exit_code"], 0xC000_0005u32);
}


/// Test 7: attach mid-stream, on a producer emitting escape sequences and
/// multibyte UTF-8 continuously, reproduces a from-scratch replay
/// byte-for-byte.
///
/// Rebuilt (finding 13 — the review's own diagnosis of the prior version's
/// CI failure): the producer is now `sot-conpty-helper --script`, a
/// deterministic byte-emitting helper (see its module doc), not a
/// `cmd.exe /d /c for /l ... echo` loop whose own startup latency and
/// console rendering the test could not predict or control. One-byte
/// pacing across 1000 repeats of a block containing a CSI pair, a
/// BEL-terminated OSC, an ST-terminated DCS, and a 3-byte codepoint
/// immediately followed by a 4-byte one (no ASCII separator) makes it
/// likely some ConPTY read lands inside one of those sequence classes
/// somewhere across the run — but likely is not proof, and round-2 review
/// correctly called out that this test used to just assert probability in
/// prose and stop there. It no longer does: below, replaying the sealed
/// voyage's own `Class::Producer` frames one at a time (each one IS a real
/// reader-chunk boundary — see that assertion's own comment) and checking
/// `Parser::is_ground()` after each PROVES at least one interior cut
/// actually happened this run, failing loudly if it somehow didn't rather
/// than silently passing a run that exercised less than it claims to. The
/// vt100 fork's own unit tests already prove `is_ground` is safe to cut
/// any of these classes at any byte boundary (U0); what THIS test proves
/// is the WIRING.
///
/// Three things get proved, all stronger than the prior version's:
///
/// 1. Finding 8: an interior CSI/OSC/DCS/UTF-8 cut is observed to have
///    actually happened somewhere in this run — not merely asserted
///    likely — via the reader-chunk-boundary `is_ground()` replay below.
/// 2. Finding 3 (queuing, not dropping): this connection's sends are held
///    from before `attach` through a window where the producer keeps
///    emitting, so whichever chunk is last never completes yet — proving
///    newly committed output queues behind an in-flight checkpoint transfer
///    (`sent_frames` must show zero `output` frames for this connection
///    while held) rather than being sent ahead of it or dropped. Releasing
///    then delivers the transfer followed immediately by every queued
///    frame, in order — the FIFO contract documented on `TestTransport`
///    above.
/// 3. Finding 13 (the U0 oracle): rather than compare rendered
///    `Screen::contents()` strings — which cannot see cursor position,
///    attributes, or mode bits that aren't in the current viewport — this
///    compares raw bytes twice: the exact tail-byte-equality of what this
///    connection received against the voyage's own recorded producer
///    bytes, and the wire checkpoint against an independently computed
///    `Screen::checkpoint()` of the exact same prefix. Checkpoint bytes are
///    a pure function of screen state (magic/version/geometry/modes/attrs/
///    grid — see `vt100_ctt::Screen::checkpoint`), so two parsers fed
///    identical byte prefixes must produce identical checkpoints; anything
///    else is either a wiring bug or a checkpoint-format non-determinism
///    this crate depends on not existing.
#[test]
fn attach_mid_stream_checkpoint_reproduces_reference_screen() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let helper = HELPER_EXE.to_string();
    // --linger: the producer must be ALIVE for every step below (this test
    // ends the run with an explicit `Kill`, never by producer exit). The
    // previous version relied on 1000 repeats taking long enough — false
    // on a fast conhost: the emission finished in milliseconds, the
    // capsule entered teardown before the test's hello was serviced, and
    // the first wait timed out with the abandoned capsule later logging
    // PreAdmissionTimeout. Producer lifetime is now explicit, not an
    // emission-speed assumption.
    let argv = vec![helper, "--script".to_string(), "1000".to_string(), "--linger".to_string()];
    let (rows, cols) = (25u16, 80u16);
    let cfg = config(dir.path(), "midattach1", argv, cols, rows);
    let root = cfg.voyage_root.clone();
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    // Attach WHILE the producer is still actively emitting -- no attempt to
    // engineer a precise cut point; is_ground's own unit tests already
    // cover that. Real elapsed time only, no fixed assumption about where
    // the loop's ground boundary lands.
    //
    // Long enough that the scrollback-ring assertion below (added with the
    // ring itself) has real content to find: `SCRIPT_BLOCK` writes one
    // line roughly every 59 ms (one byte per 1 ms sleep, ~59 bytes/line),
    // so at this 25-row screen a nominal run scrolls a couple hundred
    // lines off in 15 s -- comfortably more than a screenful even if a
    // loaded runner's per-byte sleep runs several times its nominal length
    // (the windows-latest-vs-windows-2022 conhost timing gap measured
    // elsewhere in this file was ~2x, not 10x). Still not an engineered
    // cut point: nothing here pins how MANY lines land in the checkpoint,
    // only that it is comfortably more than a screenful.
    std::thread::sleep(Duration::from_millis(15_000));

    const CONN: ConnId = 1;
    transport.open(CONN);
    transport.feed(CONN, frame::hello());
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("conn hello_ok", CONN, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });

    // Finding 3: hold every send to this connection from before `attach`
    // through a window where the producer keeps emitting, so the
    // checkpoint transfer's own completion(s) never get reported while
    // more output is committed behind it.
    transport.set_hold_for(CONN, true);
    transport.feed(CONN, frame::attach("watcher"));
    // A throwaway cursor: only confirms a checkpoint chunk was actually
    // constructed and queued (`sent_frames` records the bytes at `send`
    // time, held or not) without disturbing `watcher`'s own cursor -- which
    // still needs to find that SAME chunk itself, below, once released.
    FrameWatcher::new(&transport).wait_for("checkpoint chunk queued (throwaway probe)", CONN, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::CheckpointChunk { .. })).then_some(())
    });

    // The producer keeps emitting while the transfer sits unconfirmed.
    std::thread::sleep(Duration::from_millis(500));
    let output_frames_while_held = transport
        .sent_frames()
        .into_iter()
        .filter(|(c, _)| *c == CONN)
        .flat_map(|(_, bytes)| wire::FrameSplitter::new().feed(&bytes).0)
        .filter(|f| matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::Output { .. })))
        .count();
    assert_eq!(
        output_frames_while_held, 0,
        "post-watermark output must queue behind an unconfirmed checkpoint transfer, never be sent ahead of it"
    );

    transport.set_hold_for(CONN, false);
    transport.release_held();
    let checkpoint_bytes = watcher.collect_checkpoint("post-hold checkpoint", CONN, Duration::from_secs(10));

    // Let more output flow post-watermark, then end the run.
    std::thread::sleep(Duration::from_millis(300));
    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    verify_voyage(&root, "midattach1").unwrap();

    // Every `output` frame this connection ever received, in arrival order
    // (the FrameWatcher's cursor already sits right after the checkpoint).
    let mut post_watermark = Vec::new();
    for (c, bytes) in transport.sent_frames() {
        if c != CONN {
            continue;
        }
        let mut s = wire::FrameSplitter::new();
        let (decoded, _) = s.feed(&bytes);
        for f in decoded {
            if let wire::DecodedFrame::AttachServer(wire::AttachServer::Output { bytes }) = f {
                post_watermark.push(bytes);
            }
        }
    }
    assert!(!post_watermark.is_empty(), "expected at least some post-watermark output");
    let suffix: Vec<u8> = post_watermark.into_iter().flatten().collect();

    // The full, from-scratch reference: every producer byte the voyage
    // ever recorded, in order.
    let frames = sealed_frames(&root, "midattach1");
    let mut total = Vec::new();
    for f in &frames {
        if f.class == Class::Producer {
            let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
            total.extend(decode_b64(b64));
        }
    }

    // Round-2 review, finding 8: PROVE an interior CSI/OSC/DCS/UTF-8 cut
    // actually occurred, rather than asserting repetition makes one "all
    // but certain". Each `Class::Producer` frame in the sealed voyage IS
    // exactly one real ConPTY-read chunk (`handle_output!` appends one
    // frame per `ReaderEvent::Output`, unmodified) -- so replaying those
    // frames one at a time into a fresh parser and checking
    // `Parser::is_ground()` after each one finds every point a REAL read
    // boundary fell in this run. `is_ground() == false` right after a
    // frame means that frame's own end sits strictly inside an
    // unterminated CSI/OSC/DCS/UTF-8 sequence -- the escape/multibyte
    // parser has consumed a partial sequence and is still waiting for the
    // rest, which only an interior cut produces. This does not touch the
    // checkpoint's own cut point (which `ground_reached` guarantees is
    // ALWAYS ground-safe, by design, so it can never itself be mid-
    // sequence) -- it is an independent, run-wide proof that at least one
    // real reader-chunk boundary landed inside one of these classes
    // somewhere in the run.
    let mut fragmentation_probe = vt100_ctt::Parser::new(rows, cols, 0);
    let mut interior_cut_found = false;
    for f in &frames {
        if f.class != Class::Producer {
            continue;
        }
        let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
        fragmentation_probe.process(&decode_b64(b64));
        if !fragmentation_probe.is_ground() {
            interior_cut_found = true;
            break;
        }
    }
    // A loud MARKER, deliberately not an assertion: both CI images turned
    // out to deliver conhost's rendered writes sequence-atomically and
    // aligned with the capsule's reads — zero interior cuts across 1000
    // repeats on BOTH, deterministically — so a panic here would be a
    // permanent red about conhost's internals, not about this capsule.
    // The mid-sequence carry property is pinned DETERMINISTICALLY where
    // it can be: the fork's ground/checkpoint tests cut inside every
    // sequence class by construction, and the wire splitter is fuzzed at
    // every byte boundary. What this e2e proves is end-to-end fidelity
    // over whatever chunking the real conhost produced; the marker below
    // records honestly how much fragmentation this run exercised.
    if !interior_cut_found {
        eprintln!(
            "capsule_win fidelity finding: NO reader-chunk boundary landed inside a \
             CSI/OSC/DCS/UTF-8 sequence this run — interior-cut coverage came only \
             from the deterministic parser/splitter suites, not this e2e"
        );
    }

    // Finding 13, part 1: the post-watermark stream this connection
    // received must be the EXACT byte-for-byte tail of the voyage's total
    // producer bytes -- proves nothing was dropped, duplicated, or
    // reordered across the watermark boundary.
    assert!(suffix.len() <= total.len(), "received more post-watermark bytes than the voyage ever recorded");
    let split = total.len() - suffix.len();
    assert_eq!(
        &total[split..],
        suffix.as_slice(),
        "post-watermark output must be the exact byte-for-byte tail of the voyage"
    );
    let prefix = &total[..split];

    // Finding 13, part 2 (the U0 oracle): the wire checkpoint must be
    // byte-identical to one computed independently by feeding a fresh
    // reference parser exactly the prefix. The reference's own scrollback
    // capacity must match the capsule's own live parser
    // (`CAPSULE_SCROLLBACK_ROWS`) -- the checkpoint now carries a
    // scrollback ring, so a reference built at a different capacity would
    // disagree about how much of it survives, independent of any real
    // divergence in what was actually recorded.
    let mut reference_at_watermark =
        vt100_ctt::Parser::new(rows, cols, capsule::CAPSULE_SCROLLBACK_ROWS);
    reference_at_watermark.process(prefix);
    let reference_checkpoint = reference_at_watermark
        .screen()
        .checkpoint()
        .expect("prefix screen must be representable");
    assert_eq!(
        checkpoint_bytes, reference_checkpoint,
        "the wire checkpoint must be byte-identical to an independently computed checkpoint of the same prefix"
    );

    // The scrollback ring itself must actually have arrived -- the defect
    // this fixed was an attach handing the client an empty ring every
    // time (`ring_len = 0` on every attach, regardless of how much had
    // scrolled off). Not a fixed expected count: real elapsed time drives
    // how many lines the producer got through before this checkpoint's cut
    // (see the sleep above), so this asserts only that SOME history rode
    // along, which is what the defect actually broke.
    let mut ring_check = vt100_ctt::Parser::new(rows, cols, capsule::CAPSULE_SCROLLBACK_ROWS);
    ring_check
        .restore_screen(&checkpoint_bytes)
        .expect("checkpoint must decode");
    ring_check.screen_mut().set_scrollback(usize::MAX);
    assert!(
        ring_check.screen().scrollback() > 0,
        "attach must hand over a nonempty scrollback ring; got an empty one"
    );

    // And the full round trip, at the same checkpoint-byte granularity: a
    // fresh parser restored from the wire checkpoint and replayed with the
    // exact suffix must reach a state whose OWN checkpoint is
    // byte-identical to a from-scratch parser's, fed the entire voyage --
    // both at the SAME scrollback capacity as the capsule's own parser,
    // for the same reason as above.
    let mut restored = vt100_ctt::Parser::new(rows, cols, capsule::CAPSULE_SCROLLBACK_ROWS);
    restored
        .restore_screen(&checkpoint_bytes)
        .expect("checkpoint must decode");
    restored.process(&suffix);

    let mut reference = vt100_ctt::Parser::new(rows, cols, capsule::CAPSULE_SCROLLBACK_ROWS);
    reference.process(&total);

    assert_eq!(
        restored.screen().checkpoint().expect("restored screen must be representable"),
        reference.screen().checkpoint().expect("reference screen must be representable"),
        "checkpoint + subsequent stream must reproduce the reference session byte-for-byte"
    );
    assert_eq!(summary.exit_kind, ExitKind::Requested);
}


/// Spawn `sot-conpty-helper spawn-breakaway cmd.exe /c timeout 30`
/// with piped stdin/stdout — blocked on its own "go" read (see the
/// helper's own module doc) until the caller sends one, so the
/// caller can assign the helper to whatever job(s) it wants it
/// contained by BEFORE the helper ever attempts its own breakaway
/// spawn. No `CREATE_SUSPENDED`/`ResumeThread` needed: the helper's
/// blocking stdin read closes the same race deterministically,
/// using only `std::process::Command`.
#[cfg(windows)]
fn spawn_gated_breakaway_helper() -> std::process::Child {
    std::process::Command::new(env!("CARGO_BIN_EXE_sot-conpty-helper"))
        .args(["spawn-breakaway", "cmd.exe", "/c", "timeout", "30"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn sot-conpty-helper spawn-breakaway")
}

/// Send the "go" signal, then read and return the helper's one reply
/// line (`pid=<n>` or `err=<n>`), trimmed.
#[cfg(windows)]
fn release_and_read_reply(helper: &mut std::process::Child) -> String {
    use std::io::{BufRead, Write};
    let mut stdin = helper.stdin.take().expect("helper stdin");
    writeln!(stdin, "go").expect("write go signal");
    drop(stdin);
    let mut line = String::new();
    std::io::BufReader::new(helper.stdout.take().expect("helper stdout"))
        .read_line(&mut line)
        .expect("read helper reply");
    line.trim().to_string()
}

/// ADR 0043 decision 32 as amended 2026-10-03: the leg job
/// (`AnonymousJob::create`, `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` alone)
/// refuses a breakaway. The helper is assigned to the job, released, and
/// asked to spawn a breakaway child; `CreateProcess` must fail with
/// `ERROR_ACCESS_DENIED` (5) — MSYS asks to break away on every spawn whose
/// job allows it, so a job that allowed it let a row's git-bash children
/// outlive the row.
#[test]
#[cfg(windows)]
fn the_leg_job_refuses_a_breakaway() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

    let job = sot_log::conpty::AnonymousJob::create().expect("create leg job");
    let mut helper = spawn_gated_breakaway_helper();
    let ok = unsafe { AssignProcessToJobObject(job.raw(), helper.as_raw_handle() as HANDLE) };
    assert!(ok != 0, "AssignProcessToJobObject: {}", std::io::Error::last_os_error());

    let reply = release_and_read_reply(&mut helper);
    if let Some(pid) = reply.strip_prefix("pid=") {
        let _ = std::process::Command::new("taskkill").args(["/F", "/PID", pid]).status();
    }
    let _ = helper.wait();
    assert_eq!(reply, "err=5", "the leg job let a child break away");
}
