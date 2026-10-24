use super::*;
use std::collections::VecDeque;

const V: &str = "01900000-0000-7000-8000-000000000001";
const V1_FIXTURE: &[u8] = include_bytes!("retire_06_fixtures/supervisor-lane-v1.bin");
const COVERAGE_FIXTURE: &[u8] = include_bytes!("retire_06_fixtures/supervisor-lane-v1-coverage.bin");

/// A fixture's frames, each with its header.
fn frames(mut bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        out.push(&bytes[..8 + len]);
        bytes = &bytes[8 + len..];
    }
    out
}

fn reply_of(frame: &[u8]) -> Reply {
    let (reply, used) = split_reply(frame).unwrap().expect("a whole frame");
    assert_eq!(used, frame.len());
    reply
}

fn status_ok(voyage: Option<&str>, leg: Option<u64>, phase: Phase) -> Reply {
    Reply::StatusOk { pid: 4242, created: 1_756_000_000, voyage: voyage.map(str::to_string), leg, phase }
}

/// The retire sends the bytes 0.6 froze, and reads every reply 0.6 can send.
#[test]
fn the_frozen_frames_are_the_retires_bytes() {
    let v1 = frames(V1_FIXTURE);
    assert_eq!(v1[0], hello_frame("fixture-build"));
    assert_eq!(v1[1], end_run_frame("op-1", "fixture", V));
    assert_eq!(v1[2], status_frame());
    assert_eq!(reply_of(v1[4]), Reply::HelloOk { proto: 1, build: "fixture-build".into(), pid: 4242, created: 1_756_000_000 });
    assert_eq!(reply_of(v1[5]), Reply::Refused(0));
    assert_eq!(reply_of(v1[6]), Reply::Operation(OpState::Accepted));
    assert_eq!(reply_of(v1[7]), status_ok(Some(V), Some(7), Phase::Ready));

    let cov = frames(COVERAGE_FIXTURE);
    assert_eq!(cov[2], stop_frame("op-4"));
    let v3 = "01900000-0000-7000-8000-000000000003";
    let expected = [
        Reply::Operation(OpState::RecordClosed),
        Reply::Operation(OpState::RecordVerified),
        Reply::Operation(OpState::ResetDone(v3.into())),
        Reply::Operation(OpState::Stopping),
        Reply::Operation(OpState::Failed("fixture failure".into())),
        Reply::Operation(OpState::Refused(1)),
        Reply::Operation(OpState::Refused(2)),
        Reply::Operation(OpState::Unknown),
        Reply::Refused(1),
        Reply::Refused(2),
        status_ok(None, None, Phase::Starting),
        status_ok(Some(V), None, Phase::Ready),
        status_ok(Some(V), Some(7), Phase::Ending),
        status_ok(Some(V), Some(7), Phase::EndedNoRespawn),
        status_ok(Some(V), Some(7), Phase::Terminal),
    ];
    assert_eq!(cov.len(), 3 + expected.len());
    for (frame, want) in cov[3..].iter().zip(expected) {
        assert_eq!(reply_of(frame), want);
    }
}

#[test]
fn the_challenge_accepts_one_hello_ok_and_nothing_else() {
    let v1 = frames(V1_FIXTURE);
    let mut hello = OldHello { buf: Vec::new() };
    assert_eq!(hello.encode_request(), hello_frame("sotd"));
    let ok = v1[4];
    assert!(matches!(hello.feed(&ok[..5]), ExchangeDecode::Incomplete));
    assert!(matches!(hello.feed(&ok[5..]), ExchangeDecode::Identity { pid: 4242, created: 1_756_000_000 }));
    // A second frame in the same exchange, or another frame instead, is foreign.
    let mut twice = OldHello { buf: Vec::new() };
    assert!(matches!(twice.feed(&[ok, v1[6]].concat()), ExchangeDecode::Foreign));
    let mut other = OldHello { buf: Vec::new() };
    assert!(matches!(other.feed(v1[6]), ExchangeDecode::Foreign));
    let mut noise = OldHello { buf: Vec::new() };
    assert!(matches!(noise.feed(b"HTTP/1.1 200 OK"), ExchangeDecode::Foreign));
}

// --- the journal ---

fn digest(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

struct Folder {
    dir: tempfile::TempDir,
}

impl Folder {
    fn new(pointer: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        if let Some(pointer) = pointer {
            std::fs::write(dir.path().join("drawer.voyage"), pointer).unwrap();
        }
        std::fs::create_dir(dir.path().join("supervisor-journal")).unwrap();
        Folder { dir }
    }
    fn put(&self, name: &str, body: &str) {
        std::fs::write(self.dir.path().join("supervisor-journal").join(name), body).unwrap();
    }
    fn active(&self, n: u8, id: &str, op: &str, voyage: &str) {
        self.put(
            &format!("{}.active", digest(n)),
            &format!(r#"{{"schema_version":1,"operation_id":"{id}","digest":"{}","op":{{"op":"{op}","voyage":"{voyage}","epoch":3}}}}"#, digest(n)),
        );
    }
    fn closed(&self, n: u8) {
        self.put(&format!("{}.closed", digest(n)), r#"{"schema_version":1}"#);
    }
    fn terminal(&self, n: u8, state: &str) {
        self.put(&format!("{}.terminal", digest(n)), &format!(r#"{{"schema_version":1,"state":"{state}"}}"#));
    }
    fn says(&self) -> Result<bool, String> {
        journal_says_requested(self.dir.path())
    }
}

#[test]
fn a_0_6_requested_end_is_read_from_the_journal() {
    // No pointer, no journal: no evidence.
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(journal_says_requested(empty.path()), Ok(false));
    let no_pointer = Folder::new(None);
    no_pointer.active(1, "sot-backend-end-run-a", "end_run", V);
    no_pointer.closed(1);
    assert_eq!(no_pointer.says(), Ok(false));

    // A closed end on the pointer's voyage.
    let closed = Folder::new(Some(V));
    closed.active(1, "sot-backend-end-run-a", "end_run", V);
    closed.closed(1);
    assert_eq!(closed.says(), Ok(true));

    // A verified end whose record never closed.
    let verified = Folder::new(Some(V));
    verified.active(1, "sot-backend-end-run-a", "end_run", V);
    verified.terminal(1, "record_verified");
    assert_eq!(verified.says(), Ok(true));
    let other_state = Folder::new(Some(V));
    other_state.active(1, "sot-backend-end-run-a", "end_run", V);
    other_state.terminal(1, "record_failed");
    assert_eq!(other_state.says(), Ok(false));

    // An end on another voyage, a stop, an end by this retire, an end still active.
    let elsewhere = Folder::new(Some(V));
    elsewhere.active(1, "sot-backend-end-run-a", "end_run", "01900000-0000-7000-8000-000000000009");
    elsewhere.closed(1);
    assert_eq!(elsewhere.says(), Ok(false));
    let stop = Folder::new(Some(V));
    stop.active(1, "sot-backend-stop-a", "stop", V);
    stop.closed(1);
    assert_eq!(stop.says(), Ok(false));
    let ours = Folder::new(Some(V));
    ours.active(1, "sot-retire-end-abc", "end_run", V);
    ours.closed(1);
    assert_eq!(ours.says(), Ok(false));
    let active_only = Folder::new(Some(V));
    active_only.active(1, "sot-backend-end-run-a", "end_run", V);
    assert_eq!(active_only.says(), Ok(false));

    // One person's end among others' records counts.
    let mixed = Folder::new(Some(V));
    mixed.active(1, "sot-retire-end-abc", "end_run", V);
    mixed.closed(1);
    mixed.active(2, "sot-backend-end-run-a", "end_run", V);
    mixed.closed(2);
    assert_eq!(mixed.says(), Ok(true));

    // Anything present that cannot be read is an error naming the file.
    for pointer in ["", "not-a-uuid", "01900000-0000-7000-8000-00000000000A", "01900000000070008000000000000001", "01900000-0000-7000-8000-000000000001\n"] {
        let bad = Folder::new(Some(pointer));
        assert!(bad.says().is_err(), "pointer {pointer:?}");
    }
    let bad_record = Folder::new(Some(V));
    bad_record.put(&format!("{}.active", digest(1)), "{not json");
    assert!(bad_record.says().unwrap_err().contains(".active"));
    let schema_2 = Folder::new(Some(V));
    schema_2.put(&format!("{}.active", digest(1)), r#"{"schema_version":2,"operation_id":"x","op":{"op":"end_run","voyage":"x"}}"#);
    assert!(schema_2.says().is_err());
    let bad_closed = Folder::new(Some(V));
    bad_closed.active(1, "sot-backend-end-run-a", "end_run", V);
    bad_closed.put(&format!("{}.closed", digest(1)), "[]");
    assert!(bad_closed.says().is_err());
    // A matching end's completion entry that is present and not a regular file rests the row, never starts it.
    let closed_dir = Folder::new(Some(V));
    closed_dir.active(1, "sot-backend-end-run-a", "end_run", V);
    std::fs::create_dir(closed_dir.dir.path().join("supervisor-journal").join(format!("{}.closed", digest(1)))).unwrap();
    assert!(closed_dir.says().is_err());
    #[cfg(unix)]
    {
        let terminal_link = Folder::new(Some(V));
        terminal_link.active(1, "sot-backend-end-run-a", "end_run", V);
        terminal_link.terminal(2, "record_verified");
        let journal = terminal_link.dir.path().join("supervisor-journal");
        std::os::unix::fs::symlink(journal.join(format!("{}.terminal", digest(2))), journal.join(format!("{}.terminal", digest(1)))).unwrap();
        assert!(terminal_link.says().is_err());
    }
    let no_op = Folder::new(Some(V));
    no_op.put(&format!("{}.active", digest(1)), r#"{"schema_version":1,"operation_id":"x","op":{}}"#);
    assert!(no_op.says().is_err());
}

// --- the conversation ---

#[derive(Default)]
struct Script {
    statuses: VecDeque<Result<Status, String>>,
    end: Option<Result<(), String>>,
    exits: VecDeque<bool>,
    calls: Vec<String>,
}

impl Old for Script {
    fn status(&mut self, _: Duration) -> Result<Status, String> {
        self.calls.push("status".into());
        self.statuses.pop_front().unwrap_or_else(|| Ok(status(Phase::Starting, None)))
    }
    fn end_run(&mut self, voyage: &str, _: Duration) -> Result<(), String> {
        self.calls.push(format!("end_run {voyage}"));
        self.end.take().unwrap_or(Ok(()))
    }
    fn stop(&mut self, _: Duration) -> Result<(), String> {
        self.calls.push("stop".into());
        Ok(())
    }
    fn exited(&mut self, _: Duration) -> Result<bool, String> {
        self.calls.push("exited".into());
        Ok(self.exits.pop_front().unwrap_or(true))
    }
    fn terminate(&mut self) -> Result<(), String> {
        self.calls.push("terminate".into());
        Ok(())
    }
    fn pause(&mut self, _: Duration) {
        self.calls.push("pause".into());
    }
    fn record_rest(&mut self, _: Option<&str>) -> Result<bool, String> {
        self.calls.push("record_rest".into());
        Ok(true)
    }
}

fn status(phase: Phase, voyage: Option<&str>) -> Status {
    Status { phase, voyage: voyage.map(str::to_string) }
}

fn run(script: &mut Script, budget: Duration) -> Result<Driven, String> {
    drive(script, budget, Duration::ZERO)
}

#[test]
fn the_retire_ends_a_ready_run_before_it_stops_the_supervisor() {
    let mut s = Script { statuses: [Ok(status(Phase::Ready, Some(V)))].into(), ..Script::default() };
    let driven = run(&mut s, QUIT_CUTOFF).unwrap();
    assert_eq!(s.calls, ["status", &format!("end_run {V}"), "stop", "exited"]);
    assert_eq!((driven.phase, driven.ended_run, driven.live_rest, driven.terminated), ("ready", true, false, false));

    // Starting, then ready: waited out.
    let mut s = Script { statuses: [Ok(status(Phase::Starting, None)), Ok(status(Phase::Ready, Some(V)))].into(), ..Script::default() };
    run(&mut s, QUIT_CUTOFF).unwrap();
    assert_eq!(s.calls, ["status", "pause", "status", &format!("end_run {V}"), "stop", "exited"]);

    // Ending, then resting after an end somebody asked for: no end of ours, and the rest is live.
    let mut s = Script { statuses: [Ok(status(Phase::Ending, Some(V))), Ok(status(Phase::EndedNoRespawn, Some(V)))].into(), ..Script::default() };
    let driven = run(&mut s, QUIT_CUTOFF).unwrap();
    // The person's end is recorded before the supervisor, its one witness, is stopped.
    assert_eq!(s.calls, ["status", "pause", "status", "record_rest", "stop", "exited"]);
    assert!(driven.live_rest && !driven.ended_run);

    // Terminal: stopped, nothing to end.
    let mut s = Script { statuses: [Ok(status(Phase::Terminal, None))].into(), ..Script::default() };
    let driven = run(&mut s, QUIT_CUTOFF).unwrap();
    assert_eq!(s.calls, ["status", "stop", "exited"]);
    assert!(!driven.live_rest && !driven.ended_run);

    // An end that is not confirmed is logged, and the supervisor is still stopped.
    let mut s = Script { statuses: [Ok(status(Phase::Ready, Some(V)))].into(), end: Some(Err("failed".into())), ..Script::default() };
    let driven = run(&mut s, QUIT_CUTOFF).unwrap();
    assert_eq!(s.calls.last().map(String::as_str), Some("exited"));
    assert!(!driven.ended_run);

    // A supervisor that never settles is stopped at the budget.
    let mut s = Script::default();
    run(&mut s, Duration::ZERO).unwrap();
    assert_eq!(s.calls, ["status", "stop", "exited"]);

    // A status that fails is not fatal.
    let mut s = Script { statuses: [Err("no reply".into())].into(), ..Script::default() };
    assert_eq!(run(&mut s, QUIT_CUTOFF).unwrap().phase, "unanswered");
    assert_eq!(s.calls, ["status", "stop", "exited"]);
}

#[test]
fn a_stop_that_does_not_exit_is_terminated_and_then_an_error() {
    let mut s = Script { statuses: [Ok(status(Phase::Terminal, None))].into(), exits: [false, true].into(), ..Script::default() };
    let driven = run(&mut s, QUIT_CUTOFF).unwrap();
    assert_eq!(s.calls, ["status", "stop", "exited", "terminate", "exited"]);
    assert!(driven.terminated);

    let mut s = Script { statuses: [Ok(status(Phase::Terminal, None))].into(), exits: [false, false].into(), ..Script::default() };
    assert!(run(&mut s, QUIT_CUTOFF).unwrap_err().contains("did not exit"));
}

// --- the folder ---

/// A 0.6 state folder: the fence file, the pointer and a voyage with its writer lock.
fn old_folder() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::write(p.join("supervisor.lock"), b"{}").unwrap();
    std::fs::write(p.join("drawer.voyage"), V).unwrap();
    std::fs::write(p.join("drawer.voyage.tmp-1"), b"x").unwrap();
    std::fs::create_dir_all(p.join("voyages").join(V)).unwrap();
    std::fs::write(p.join("voyages").join(V).join("writer.lock"), b"").unwrap();
    std::fs::create_dir(p.join("supervisor-journal")).unwrap();
    std::fs::write(p.join("rollout-evidence.json"), b"{}").unwrap();
    dir
}

#[test]
fn the_layout_is_what_0_6_leaves_and_nothing_a_0_7_row_has() {
    let dir = old_folder();
    let names: Vec<String> = layout(dir.path()).unwrap().iter().map(|n| n.to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["drawer.voyage", "drawer.voyage.tmp-1", "rollout-evidence.json", "supervisor-journal", "voyages"]);
    let new = tempfile::tempdir().unwrap();
    for name in ["supervisor.lock", "ended", "row-scopes", ".retired-voyages", "drawer"] {
        std::fs::write(new.path().join(name), b"").unwrap();
    }
    assert!(layout(new.path()).unwrap().is_empty());
    assert!(layout(&new.path().join("missing")).unwrap().is_empty());
    // A 0.7 folder is retired by doing nothing.
    assert_eq!(retire_06(new.path()), Ok(()));
}

/// Premise 9: Windows refuses to rename a folder that holds an open handle, and the fence file is no entry that moves.
#[test]
fn the_old_layout_moves_while_the_fence_is_held() {
    let dir = old_folder();
    let found = layout(dir.path()).unwrap();
    let _fence = sot_capsule::supervisor::fence::lock_supervisor(dir.path()).unwrap();
    let moved = move_layout(dir.path(), &found).unwrap();
    assert_eq!(moved.names.len(), 5);
    assert!(!moved.requested);
    assert!(layout(dir.path()).unwrap().is_empty());
    for name in &found {
        assert!(dir.path().join(RETIRED_DIR).join(name).exists(), "{name:?}");
    }
    assert!(dir.path().join("supervisor.lock").is_file());
    assert!(!dir.path().join("ended").exists());
}

/// A closed `end_run` on `V` in `dir`'s journal under operation id `id`, record `n`.
fn add_end(dir: &Path, n: u8, id: &str) {
    let journal = dir.join("supervisor-journal");
    std::fs::write(
        journal.join(format!("{}.active", digest(n))),
        format!(r#"{{"schema_version":1,"operation_id":"{id}","digest":"{}","op":{{"op":"end_run","voyage":"{V}","epoch":3}}}}"#, digest(n)),
    )
    .unwrap();
    std::fs::write(journal.join(format!("{}.closed", digest(n))), r#"{"schema_version":1}"#).unwrap();
}

#[test]
fn a_requested_end_is_recorded_before_the_files_move_and_a_held_terminal_stops_the_move() {
    let dir = old_folder();
    add_end(dir.path(), 1, "sot-backend-end-run-a");
    let found = layout(dir.path()).unwrap();
    let moved = move_layout(dir.path(), &found).unwrap();
    assert!(moved.requested);
    assert!(sot_capsule::capsule::read_ended(dir.path()).unwrap().is_some_and(|e| e.requested));

    // A terminal that holds its writer lock: nothing moves and nothing is recorded.
    let dir = old_folder();
    add_end(dir.path(), 1, "sot-backend-end-run-a");
    let found = layout(dir.path()).unwrap();
    let _held = sot_platform::host::lock_writer(&dir.path().join("voyages").join(V).join("writer.lock")).unwrap();
    let err = move_layout(dir.path(), &found).unwrap_err();
    assert!(err.contains("a 0.6 terminal still runs"), "{err}");
    assert_eq!(layout(dir.path()).unwrap().len(), 5);
    assert!(!dir.path().join("ended").exists());

    // A name already retired is not overwritten.
    drop(_held);
    std::fs::create_dir(dir.path().join(RETIRED_DIR)).unwrap();
    std::fs::write(dir.path().join(RETIRED_DIR).join("voyages"), b"").unwrap();
    assert!(move_layout(dir.path(), &found).unwrap_err().contains("already exists"));
}

/// The record comes before the first rename: a move that fails at its first rename (a name already retired) still leaves
/// the requested end written, so a retry cannot start the row.
#[test]
fn the_requested_end_is_written_before_the_first_rename_can_fail() {
    let dir = old_folder();
    add_end(dir.path(), 1, "sot-backend-end-run-a");
    let found = layout(dir.path()).unwrap();
    std::fs::create_dir(dir.path().join(RETIRED_DIR)).unwrap();
    std::fs::write(dir.path().join(RETIRED_DIR).join("drawer.voyage"), b"").unwrap();
    assert!(move_layout(dir.path(), &found).unwrap_err().contains("already exists"));
    assert_eq!(layout(dir.path()).unwrap().len(), 5, "nothing moved");
    assert!(sot_capsule::capsule::read_ended(dir.path()).unwrap().is_some_and(|e| e.requested));
}

/// A live 0.6 supervisor's rest is decided before it is stopped: a person's unless the journal shows it is the retire's own
/// end (a retire whose `end_run` landed and whose stop failed), which resumes the row on retry.
#[test]
fn a_live_rest_is_a_persons_unless_the_journal_shows_it_is_the_retires_own() {
    // No journal evidence: the live supervisor is the only witness, and the end is recorded at once.
    let dir = old_folder();
    assert_eq!(record_live_rest(dir.path(), Some(V)), Ok(true));
    assert!(sot_capsule::capsule::read_ended(dir.path()).unwrap().is_some_and(|e| e.requested));
    // The retire's own end: nothing is recorded.
    let dir = old_folder();
    add_end(dir.path(), 1, "sot-retire-end-abc");
    assert_eq!(record_live_rest(dir.path(), Some(V)), Ok(false));
    assert!(!dir.path().join("ended").exists());
    assert_eq!(move_layout(dir.path(), &layout(dir.path()).unwrap()).unwrap().requested, false);
    // Both: a person's end is in the journal too.
    let dir = old_folder();
    add_end(dir.path(), 1, "sot-retire-end-abc");
    add_end(dir.path(), 2, "sot-backend-end-run-a");
    assert_eq!(record_live_rest(dir.path(), Some(V)), Ok(true));
    // A status that names no voyage falls back to the pointer.
    let dir = old_folder();
    add_end(dir.path(), 1, "sot-retire-end-abc");
    assert_eq!(record_live_rest(dir.path(), None), Ok(false));
}

// --- the retire through its two callers, and its boundaries ---

/// A fake cgroup root with this row's remembered scope: a process (populated) the retire must kill. The scope is named
/// in the durable `row-scopes` record only; no supervisor lives to be captured.
#[cfg(target_os = "linux")]
#[test]
fn remembered_scopes_are_ended_when_no_supervisor_lives() {
    let (dir, root) = (old_folder(), tempfile::tempdir().unwrap());
    let rel = format!("/a/app.slice/sot-row-{}-x.scope", sot_platform::host::state_dir::state_dir_hash(dir.path()));
    let scope = root.path().join(rel.trim_start_matches('/'));
    std::fs::create_dir_all(&scope).unwrap();
    std::fs::write(scope.join("cgroup.kill"), "").unwrap();
    std::fs::write(scope.join("cgroup.events"), "populated 0\nfrozen 0\n").unwrap();
    std::fs::write(dir.path().join("row-scopes"), format!("{rel}\n")).unwrap();
    crate::rows::spawn::row_scope::TEST_ROOT.with(|r| *r.borrow_mut() = Some(root.path().to_path_buf()));
    let result = retire_06(dir.path());
    crate::rows::spawn::row_scope::TEST_ROOT.with(|r| *r.borrow_mut() = None);
    result.unwrap();
    assert_eq!(std::fs::read_to_string(scope.join("cgroup.kill")).unwrap(), "1", "the remembered scope was not killed");
    assert!(layout(dir.path()).unwrap().is_empty());
}

/// A scope that will not empty is an error, and nothing moves.
#[cfg(target_os = "linux")]
#[test]
fn a_remembered_scope_that_will_not_empty_keeps_the_layout_in_place() {
    let (dir, root) = (old_folder(), tempfile::tempdir().unwrap());
    let rel = format!("/a/app.slice/sot-row-{}-x.scope", sot_platform::host::state_dir::state_dir_hash(dir.path()));
    let scope = root.path().join(rel.trim_start_matches('/'));
    std::fs::create_dir_all(&scope).unwrap();
    std::fs::write(scope.join("cgroup.kill"), "").unwrap();
    std::fs::write(scope.join("cgroup.events"), "populated 1\n").unwrap();
    std::fs::write(dir.path().join("row-scopes"), format!("{rel}\n")).unwrap();
    crate::rows::spawn::row_scope::TEST_ROOT.with(|r| *r.borrow_mut() = Some(root.path().to_path_buf()));
    let result = retire_06(dir.path());
    crate::rows::spawn::row_scope::TEST_ROOT.with(|r| *r.borrow_mut() = None);
    assert!(result.unwrap_err().contains("did not empty"));
    assert_eq!(layout(dir.path()).unwrap().len(), 5);
}

/// A folder that holds the 0.6 layout and no fence file never started: the retire moves the layout and does not make the
/// fence file, the start marker. Reached through `end_run`, the close path.
#[test]
fn the_retire_never_creates_the_start_marker() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("voyages").join(V)).unwrap();
    let outcome = crate::rows::run::end_run::end_run(dir.path(), "close", false).unwrap();
    assert!(matches!(outcome, crate::rows::run::end_run::EndRunOutcome::Unheld), "{outcome:?}");
    assert!(layout(dir.path()).unwrap().is_empty(), "the layout moved");
    assert!(dir.path().join(RETIRED_DIR).join("voyages").is_dir());
    assert!(!dir.path().join("supervisor.lock").exists(), "the retire made the start marker");
}

/// `end_run` meets an old row first: it retires it (a Cleanup boot ends rows through here) and keeps the person's end.
#[test]
fn end_run_retires_an_old_row_it_meets() {
    let dir = old_folder();
    add_end(dir.path(), 1, "sot-backend-end-run-a");
    let outcome = crate::rows::run::end_run::end_run(dir.path(), "close", false).unwrap();
    assert!(matches!(outcome, crate::rows::run::end_run::EndRunOutcome::Unheld), "{outcome:?}");
    assert!(layout(dir.path()).unwrap().is_empty());
    assert!(sot_capsule::capsule::read_ended(dir.path()).unwrap().is_some_and(|e| e.requested));
    // One that cannot be retired is not ended.
    let dir = old_folder();
    let _held = sot_platform::host::lock_writer(&dir.path().join("voyages").join(V).join("writer.lock")).unwrap();
    let outcome = crate::rows::run::end_run::end_run(dir.path(), "close", false).unwrap();
    assert!(matches!(outcome, crate::rows::run::end_run::EndRunOutcome::NotEnded(ref d) if d.contains("0.6 terminal")), "{outcome:?}");
}
