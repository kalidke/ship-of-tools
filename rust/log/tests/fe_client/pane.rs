//! The pane rulings: watcher attach, pen and resize order, end_run, reconnect, re-attach input.

use super::*;

// -----------------------------------------------------------------------
// Ruling: attach as a watcher and receive the checkpoint
// -----------------------------------------------------------------------

#[test]
fn attach_as_watcher_receives_the_checkpoint() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let (woke, wake) = wake_flag();
    let mut client = FeAttachClient::attach(
        PlatformEndpoint::default(),
        h.clone(),
        80,
        24,
        "fe-client-win-test-a".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");

    let banner = poll_screen(&mut client, Duration::from_secs(30), |t| t.trim().chars().any(|c| !c.is_whitespace()));
    assert!(banner, "no checkpoint content ever reached the client's screen (dead={}, status={})", client.is_dead(), client.status_line());
    assert!(!client.is_dead());
    assert!(woke.load(Ordering::Relaxed), "wake() was never called");

    end_run_and_wait_verified(&conn, &voyage);
    let _ = command(&conn, "test-a-stop", SupervisorOp::Stop);
    wait_for_exit(&mut guard, Duration::from_secs(30));
}

// -----------------------------------------------------------------------
// A missed liveness probe is a blink, not a retitle: the next answered
// probe restores "attached"
// -----------------------------------------------------------------------

#[test]
#[cfg(target_os = "linux")]
fn a_missed_liveness_probe_clears_on_the_next_answer() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let sup_pid = guard.id();
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let (_woke, wake) = wake_flag();
    let mut client = FeAttachClient::attach(
        PlatformEndpoint::default(),
        h.clone(),
        80,
        24,
        "fe-client-probe-blink".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");
    let banner = poll_screen(&mut client, Duration::from_secs(30), |t| t.trim().chars().any(|c| !c.is_whitespace()));
    assert!(banner, "no checkpoint reached the client (status={})", client.status_line());
    assert_eq!(client.status_line(), "attached");
    // A fresh attach episode emits its own Notice; holding it here makes
    // "no reattach happened" an observation rather than an inference from
    // a status sequence that one `pump()` can swallow whole (Codex review).
    let notice_before = client.notice().map(str::to_string);

    // Freeze the supervisor past the probe budget: the lane is up, the
    // pipe is alive, nothing answers -- a stalled link looks the same.
    assert_eq!(unsafe { libc::kill(sup_pid as libc::pid_t, libc::SIGSTOP) }, 0);
    poll_until(
        || {
            client.pump();
            client.status_line().contains("not answering").then_some(())
        },
        Duration::from_secs(20),
        "the missed-probe status to appear",
    );
    assert!(!client.is_dead(), "a missed probe on a live pipe is never terminal");
    assert_eq!(unsafe { libc::kill(sup_pid as libc::pid_t, libc::SIGCONT) }, 0);

    // The very next answered probe restores the header -- through the
    // re-dialed lane, never a reattach: no other status text may show
    // between the blink and "attached". Before the fix the blink stayed
    // until the next reattach.
    let mut seen: Vec<String> = Vec::new();
    poll_until(
        || {
            client.pump();
            let s = client.status_line().to_string();
            if seen.last() != Some(&s) {
                seen.push(s.clone());
            }
            (s == "attached").then_some(())
        },
        Duration::from_secs(20),
        "the header to read attached again after the supervisor answers",
    );
    assert!(!client.is_dead());
    assert!(seen.len() == 2 && seen[0].contains("not answering"), "expected blink then attached, saw {seen:?}");
    assert_eq!(
        client.notice().map(str::to_string),
        notice_before,
        "the header recovered through a NEW attach episode, not the re-dialed lane"
    );

    // Teardown goes over a FRESH control lane. SIGSTOP freezes every
    // deadline this supervisor holds and they all come due at once on
    // SIGCONT, so the control connection opened before the freeze is
    // closed from under us -- an artifact of how the test stalls the
    // peer, not of anything the client did (verified: the supervisor is
    // alive and answering, only this one pre-freeze connection is gone).
    // Re-dialing also proves the supervisor is healthy enough to accept a
    // new lane after the stall.
    drop(conn);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    end_run_and_wait_verified(&conn, &voyage);
    let _ = command(&conn, "probe-blink-stop", SupervisorOp::Stop);
    wait_for_exit(&mut guard, Duration::from_secs(30));
}

// -----------------------------------------------------------------------
// Ruling: first input takes the pen and the resize precedes the flush
// -----------------------------------------------------------------------

#[test]
fn first_input_takes_the_pen_and_resize_precedes_the_flush() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let (_woke, wake) = wake_flag();
    let mut client = FeAttachClient::attach(
        PlatformEndpoint::default(),
        h.clone(),
        80,
        24,
        "fe-client-win-test-b".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");

    assert!(
        poll_screen(&mut client, Duration::from_secs(30), |t| t.trim().chars().any(|c| !c.is_whitespace())),
        "no checkpoint content ever reached the client's screen"
    );

    // First input while WATCHING: enters the take transaction, sends
    // `take`, and on `take_ok` sends `resize` FIRST, then flushes this
    // exact payload as the ONE `input` frame.
    let marker: &[u8] = b"echo SOT_FE_MARKER\r\n";
    client.send_input(marker);

    let found = poll_screen(&mut client, Duration::from_secs(30), |t| t.contains("SOT_FE_MARKER"));
    assert!(found, "input never reached the shell (dead={}, status={})", client.is_dead(), client.status_line());

    // Ruling (b): resize precedes the flush -- proven at the wire level
    // via the sealed voyage record (the client's own public surface has
    // no other way to observe SEND order). `Class::Input`'s payload
    // REDACTS content, so the match is by exact byte length -- unique in
    // this run since no other command of this length is ever sent.
    drop(client);
    end_run_and_wait_verified(&conn, &voyage);

    let frames = sealed_frames(&state_dir, &voyage);
    let resize_seq = frames
        .iter()
        .find_map(|f| {
            if f.class != sot_log::store::envelope::Class::ControlExchange {
                return None;
            }
            let p = f.payload.as_ref()?;
            if p.get("phase")?.as_str()? == "request" && p.get("kind_ns")?.as_str()? == "conpty/resize" {
                Some(f.seq.n)
            } else {
                None
            }
        })
        .expect("no resize control_exchange request frame found in the sealed voyage");
    let input_seq = frames
        .iter()
        .find_map(|f| {
            if f.class != sot_log::store::envelope::Class::Input {
                return None;
            }
            let p = f.payload.as_ref()?;
            if p.get("length")?.as_u64()? == marker.len() as u64 {
                Some(f.seq.n)
            } else {
                None
            }
        })
        .expect("no matching input frame found in the sealed voyage");
    assert!(
        resize_seq < input_seq,
        "resize (seq {resize_seq}) must precede the flushed input (seq {input_seq})"
    );

    let _ = command(&conn, "test-b-stop", SupervisorOp::Stop);
    wait_for_exit(&mut guard, Duration::from_secs(30));
}

// -----------------------------------------------------------------------
// Ruling: end_run from the quit dispatcher reaches record_verified
// -----------------------------------------------------------------------

/// Codex review round, finding 1 + finding 14: `should_exit()` is now
/// gated on `record_verified`, not merely `record_closed` (the original
/// bug: the dispatcher exited the instant the command's own DEFERRED
/// reply arrived, without ever querying for verification) — so THIS
/// TEST's own `exited` assertion below is itself the client-visible
/// proof of `record_verified`, not just `record_closed`.
#[test]
fn end_run_from_the_quit_dispatcher_reaches_client_visible_record_verified() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (_voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let (_woke, wake) = wake_flag();
    let mut client = FeAttachClient::attach(
        PlatformEndpoint::default(),
        h.clone(),
        80,
        24,
        "fe-client-win-test-c".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");
    assert!(
        poll_screen(&mut client, Duration::from_secs(30), |t| t.trim().chars().any(|c| !c.is_whitespace())),
        "no checkpoint content ever reached the client's screen"
    );

    client.request_quit("integration test quit");

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut exited = false;
    let mut last_msg: Option<String> = None;
    while Instant::now() < deadline {
        client.pump();
        if client.should_exit() {
            exited = true;
            break;
        }
        let msg: Option<String> = client.quit_message().map(|m| m.to_string());
        if msg != last_msg {
            eprintln!("[quit test] quit message: {msg:?}");
            last_msg = msg.clone();
        }
        assert_ne!(
            msg.as_deref(),
            Some("ending the session did not complete \u{2014} outcome unknown"),
            "the quit dispatcher timed out instead of observing record_closed"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    if !exited {
        // Codex review round (evidence from 285ad0d9's real-Windows run):
        // `conn` (opened before the 60s quit-wait loop, never itself used
        // during it) can ALSO have idled out under the supervisor lane's
        // own 5s deadline by now — using it here panicked INSIDE the
        // diagnostic, hiding the real failure. A FRESH connection (ruling
        // 4) is the honest way to ask the authority what it thinks right
        // now; `try_status` (never panics) keeps this path from replacing
        // the real panic message with a connection error instead.
        let diag = wait_for_lane(&h, Duration::from_secs(10));
        panic!(
            "quit dispatcher never reached should_exit (client-visible record_verified) within 60s; \
             quit message: {:?}; authority status (voyage, leg, phase): {:?}",
            client.quit_message(),
            try_status(&diag)
        );
    }

    // Independent corroboration: the supervisor ends up serving
    // ENDED-NO-RESPAWN, exactly what a `record_closed` end_run leaves
    // behind (ADR 0041 Lifecycle: "An ended authority stays
    // serviceable"). A FRESH connection (ruling 4, same reasoning as the
    // diagnostic above) — `conn` has been idle since before the quit was
    // even requested. POLLED, not asserted at once: the lane replies
    // `record_closed` the moment the record is closed (B3, the deferred
    // reply) while the authority is still ENDING — verification
    // (`record_verified`) and the phase transition land after it. First
    // real-Windows run caught exactly that: Ending.
    let conn = wait_for_lane(&h, Duration::from_secs(10));
    let corroborated = {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let (_v, _l, phase) = status(&conn);
            if phase == SupervisorPhase::EndedNoRespawn {
                break true;
            }
            if Instant::now() >= deadline {
                eprintln!("[quit test] last phase before giving up: {phase:?}");
                break false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    assert!(corroborated, "the authority never reached EndedNoRespawn after record_closed");

    let _ = command(&conn, "test-c-stop", SupervisorOp::Stop);
    wait_for_exit(&mut guard, Duration::from_secs(30));
}

// -----------------------------------------------------------------------
// Ruling: reconnect after the capsule is killed and a fresh supervisor
// takes over restores the screen from the new checkpoint
// -----------------------------------------------------------------------

#[test]
fn reconnect_after_the_capsule_is_killed_restores_the_screen_from_the_new_checkpoint() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard1 = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn1 = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn1, Duration::from_secs(90));

    let (_woke, wake) = wake_flag();
    let mut client = FeAttachClient::attach(
        PlatformEndpoint::default(),
        h.clone(),
        80,
        24,
        "fe-client-win-test-d".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");
    assert!(
        poll_screen(&mut client, Duration::from_secs(30), |t| t.trim().chars().any(|c| !c.is_whitespace())),
        "no checkpoint content ever reached the client's screen before the kill"
    );
    // A distinctive marker in the FIRST leg's screen, so the later
    // assertion can tell "still showing the old screen" apart from "a
    // genuinely fresh checkpoint arrived" -- proving RESTORE, not mere
    // silence.
    client.send_input(b"echo SOT_FE_OLD_LEG\r\n");
    assert!(
        poll_screen(&mut client, Duration::from_secs(30), |t| t.contains("SOT_FE_OLD_LEG")),
        "first-leg marker never reached the screen"
    );

    // "The capsule is killed": hard-terminate the capsule process
    // directly (learned via the voyage pipe's own mgmt status, the
    // honest hard-termination fallback the ADR itself names), then kill
    // the now-orphaned supervisor too so what comes next is genuinely a
    // FRESH supervisor process, not the same one respawning its own
    // child.
    let pid = capsule_pid(&voyage);
    taskkill(pid);
    let _ = guard1.child_mut().kill();
    let _ = guard1.child_mut().wait();

    // A fresh supervisor, `--resume` against the SAME state dir: no live
    // capsule survives to adopt, so it spawns a fresh leg under the SAME
    // (already-published, unchanged) voyage pointer.
    let mut guard2 = spawn_supervisor(&state_dir, "--resume", SHELL);
    let conn2 = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage2, _leg2) = wait_for_ready(&conn2, Duration::from_secs(90));
    assert_eq!(voyage2, voyage, "a fresh spawn under --resume must keep the SAME voyage pointer");

    // The client's own reconnect episode (ruling d) notices the dropped
    // attach connection, re-reads the pointer, reconnects the
    // supervisor lane, and re-attaches -- restoring the screen from the
    // NEW leg's checkpoint. Bounded generously: real backoff (250ms
    // doubling to 4s) plus a real second capsule spawn both cost real
    // wall time here.
    let fresh = poll_screen(&mut client, Duration::from_secs(120), |t| {
        t.trim().chars().any(|c| !c.is_whitespace()) && !t.contains("SOT_FE_OLD_LEG")
    });
    assert!(
        fresh,
        "the client never restored a fresh checkpoint after reconnect (dead={}, status={})",
        client.is_dead(),
        client.status_line()
    );
    assert!(!client.is_dead());

    drop(client);
    end_run_and_wait_verified(&conn2, &voyage2);
    let _ = command(&conn2, "test-d-stop", SupervisorOp::Stop);
    wait_for_exit(&mut guard2, Duration::from_secs(30));
}

/// Keys typed while an ATTACHED pane re-attaches are never delivered once it
/// attaches, even into a restarted session: only a key typed after the pane
/// reports attached reaches the capsule. The new capsule is frozen so the
/// attach handshake stalls while the keys are typed.
#[test]
#[cfg(target_os = "linux")]
fn keys_typed_while_a_pane_re_attaches_are_never_delivered() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard1 = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn1 = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn1, Duration::from_secs(90));

    let (_woke, wake) = wake_flag();
    let mut client = FeAttachClient::attach(
        PlatformEndpoint::default(),
        h.clone(),
        80,
        24,
        "fe-client-win-test-stale".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");
    assert!(
        poll_screen(&mut client, Duration::from_secs(30), |t| t.trim().chars().any(|c| !c.is_whitespace())),
        "no checkpoint content ever reached the client's screen before the kill"
    );
    let old_leg = b"echo SOT_FE_OLD_LEG\r\n";
    client.send_input(old_leg);
    assert!(
        poll_screen(&mut client, Duration::from_secs(30), |t| t.contains("SOT_FE_OLD_LEG")),
        "first-leg marker never reached the screen"
    );
    let r0 = poll_until(
        || {
            client.pump();
            (client.recorded_bytes() == old_leg.len() as u64).then(|| client.recorded_bytes())
        },
        Duration::from_secs(30),
        "the old leg's input to be fully recorded",
    );

    let pid = capsule_pid(&voyage);
    taskkill(pid);
    let _ = guard1.child_mut().kill();
    let _ = guard1.child_mut().wait();

    let mut guard2 = spawn_supervisor(&state_dir, "--resume", SHELL);
    let conn2 = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage2, _leg2) = wait_for_ready(&conn2, Duration::from_secs(90));
    assert_eq!(voyage2, voyage, "a fresh spawn under --resume must keep the SAME voyage pointer");

    // Freeze the new capsule: the re-attach cannot complete while the keys are typed.
    let capsule2 = capsule_pid(&voyage2);
    assert_eq!(unsafe { libc::kill(capsule2 as libc::pid_t, libc::SIGSTOP) }, 0);
    let typing_until = Instant::now() + Duration::from_secs(8);
    let mut n = 0usize;
    while Instant::now() < typing_until {
        client.send_input(b"echo SOT_FE_STALE\r");
        n += 1;
        client.pump();
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(n > 0);
    assert_eq!(unsafe { libc::kill(capsule2 as libc::pid_t, libc::SIGCONT) }, 0);

    poll_until(
        || {
            client.pump();
            let fresh = !screen_text(client.screen()).contains("SOT_FE_OLD_LEG");
            (fresh && client.status_line() == "attached").then_some(())
        },
        Duration::from_secs(120),
        "the client to re-attach to a fresh screen",
    );
    // The keys typed during the re-attach are counted, not silently lost.
    poll_until(
        || {
            client.pump();
            (client.inputs_discarded() == n).then_some(())
        },
        Duration::from_secs(5),
        "every key typed during the re-attach to be counted as discarded",
    );
    let after = b"echo SOT_FE_AFTER\r";
    client.send_input(after);
    assert!(
        poll_screen(&mut client, Duration::from_secs(30), |t| t.contains("SOT_FE_AFTER")),
        "the post-attach marker never reached the screen"
    );
    assert_eq!(
        client.recorded_bytes() - r0,
        after.len() as u64,
        "only the key typed after attach may be recorded; {n} keys typed during the re-attach were delivered"
    );
    assert!(
        !screen_text(client.screen()).contains("SOT_FE_STALE"),
        "a key typed before the pane attached reached the restarted session"
    );
    assert_eq!(client.inputs_discarded(), 0, "a delivered key clears the discard count");

    drop(client);
    // The first management connection idles out during the freeze; dial a fresh one.
    drop(conn2);
    let conn3 = wait_for_lane(&h, Duration::from_secs(30));
    end_run_and_wait_verified(&conn3, &voyage2);
    let _ = command(&conn3, "test-stale-stop", SupervisorOp::Stop);
    wait_for_exit(&mut guard2, Duration::from_secs(30));
}
