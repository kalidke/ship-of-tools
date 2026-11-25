//! The queue runner against the repo's real launcher and a stand-in `codex` on its path. The stand-in answers the
//! launcher's help question, records each queue run (its arguments, folder and home) in `<dir>/queued`, and then does
//! what `<dir>/mode` says: accept (the default), `refuse`, `malformed`, `other-thread`, `status-69` or `hang` (with a
//! child of its own, whose pid it writes to `<dir>/child`). With `<dir>/no-queue` its help names no queue command.
//! Anything but a queue run or its help is a session start, which it records in `<dir>/session` and refuses.

use super::*;
use std::path::PathBuf;
use std::time::Instant;

const THREAD: &str = "aaaaaaaa-1111-4222-8333-444444444444";
const LINE: &str = "[sot-comm] you have mail: run comm-poll.sh";

struct Stand {
    tmp: tempfile::TempDir,
    ccx: PathBuf,
}

impl Stand {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for sub in ["bin", "home", "row"] {
            std::fs::create_dir(dir.join(sub)).unwrap();
        }
        let codex = format!(
            r#"#!/bin/sh
d='{d}'
if [ "$1 $2" = 'queue --help' ]; then
    if [ -f "$d/no-queue" ]; then echo 'Usage: codex [OPTIONS] [PROMPT]'; else echo 'Usage: codex queue [OPTIONS] --thread <THREAD> --message <TEXT>'; fi
    exit 0
fi
[ "$1" = queue ] || {{ echo "$*" >> "$d/session"; exit 64; }}
{{ printf 'cwd=%s\nhome=%s\n' "$PWD" "$HOME"; for a in "$@"; do printf 'arg=%s\n' "$a"; done; }} >> "$d/queued"
case "$(cat "$d/mode" 2>/dev/null)" in
    refuse) echo 'error: no such session' >&2; exit 1 ;;
    malformed) echo 'Queued.' ;;
    other-thread) echo 'Queued message bbbbbbbb-1111-4222-8333-444444444444 for thread cccccccc-1111-4222-8333-444444444444.' ;;
    status-69) echo 'error: unavailable' >&2; exit 69 ;;
    hang) sleep 300 & echo $! > "$d/child"; wait ;;
    *) echo "Queued message bbbbbbbb-1111-4222-8333-444444444444 for thread $3." ;;
esac
"#,
            d = dir.display()
        );
        sot_platform::test_exec::write_executable(&dir.join("bin/codex"), codex);
        let ccx = dir.join("bin/ccx");
        let real = concat!(env!("CARGO_MANIFEST_DIR"), "/../../agents/codex/bin/ccx");
        sot_platform::test_exec::write_executable(&ccx, std::fs::read(real).unwrap());
        Stand { tmp, ccx }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.file(name)).unwrap_or_default()
    }

    fn env(&self) -> Vec<(OsString, OsString)> {
        let path = format!("{}:/usr/bin:/bin", self.file("bin").display());
        vec![("PATH".into(), path.into()), ("HOME".into(), self.file("home").into())]
    }

    fn queue_on(&self, signal: &'static sot_containment::Signal, within: Duration) -> Queued {
        queue(signal, &self.ccx, &self.env(), &self.file("row"), THREAD, LINE, within)
    }

    fn queue(&self) -> Queued {
        self.queue_on(sot_containment::process(), Duration::from_secs(30))
    }

    /// The stand-in's child once it has started.
    fn child(&self) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(pid) = self.read("child").trim().parse() {
                return pid;
            }
            assert!(Instant::now() < deadline, "the stand-in started no child");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Whether `pid` has ended: gone, or a zombie its new parent has yet to reap.
fn ended(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        if stat.rsplit_once(") ").map_or(true, |(_, rest)| rest.starts_with('Z')) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Rule 1's command: the launcher's binary, Codex's queue with the conversation and the fixed line as its one message,
/// in the row's folder under the given home, once.
#[test]
fn the_queue_command_runs_once_for_the_conversation_in_the_rows_folder() {
    let s = Stand::new();
    assert_eq!(s.queue(), Queued::Accepted);
    let row = s.file("row").canonicalize().unwrap();
    let want = format!("cwd={}\nhome={}\narg=queue\narg=--thread\narg={THREAD}\narg=--message\narg={LINE}\n", row.display(), s.file("home").display());
    assert_eq!(s.read("queued"), want);
    assert_eq!(s.read("session"), "", "the launcher started a session");
    assert!(std::fs::read_dir(s.file("home")).unwrap().next().is_none(), "the queue mode wrote into the home");
}

/// Rule 4: a refusal is definite only when Codex's queue command never ran. No binary, a binary whose help names no
/// queue command, and an installed launcher older than the mode, which is not run at all.
#[test]
fn a_refusal_before_the_queue_command_ran_is_definite() {
    let s = Stand::new();
    std::fs::write(s.file("no-queue"), "").unwrap();
    assert!(matches!(s.queue(), Queued::Refused(said) if said.starts_with("ccx: no queue:") && said.contains("--thread")));
    std::fs::remove_file(s.file("bin/codex")).unwrap();
    assert_eq!(s.queue(), Queued::Refused("ccx: no queue: codex not found".into()));
    let old = format!("#!/bin/sh\necho ran >> '{}'\nexit 0\n", s.file("old-ran").display());
    sot_platform::test_exec::write_executable(&s.ccx, old);
    assert!(matches!(s.queue(), Queued::Refused(said) if said.contains("older than its --queue mode")));
    std::fs::remove_file(&s.ccx).unwrap();
    assert!(matches!(s.queue(), Queued::Refused(_)));
    assert_eq!(s.read("queued") + &s.read("session") + &s.read("old-ran"), "", "something ran");
}

/// Rules 3 and 4: whatever is not the acknowledgment for this conversation or the launcher's own refusal is unclear,
/// Codex's own refusal and its own status 69 among them.
#[test]
fn an_outcome_that_is_neither_the_acknowledgment_nor_the_launchers_refusal_is_unclear() {
    let s = Stand::new();
    for mode in ["refuse", "malformed", "other-thread", "status-69"] {
        std::fs::write(s.file("mode"), mode).unwrap();
        assert!(matches!(s.queue(), Queued::Unclear(_)), "{mode}");
    }
    assert_eq!(s.read("queued").matches("arg=queue\n").count(), 4);
}

/// Rule 5: at the bound the whole tree is ended and the outcome is unclear.
#[test]
fn a_queue_command_that_hangs_is_ended_with_its_child_at_the_bound() {
    let s = Stand::new();
    std::fs::write(s.file("mode"), "hang").unwrap();
    let began = Instant::now();
    assert!(matches!(s.queue_on(sot_containment::process(), Duration::from_secs(2)), Queued::Unclear(why) if why.contains("did not exit within")));
    assert!(began.elapsed() < Duration::from_secs(10), "the wait outran its bound: {:?}", began.elapsed());
    assert!(ended(s.child()), "the stand-in's child outlived the bound");
}

/// Rule 5: once the daemon's child signal has fired, a run in flight is ended and none starts.
#[test]
fn a_fired_signal_ends_a_run_in_flight_and_starts_none() {
    let s = Stand::new();
    std::fs::write(s.file("mode"), "hang").unwrap();
    let signal: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let began = Instant::now();
    std::thread::scope(|scope| {
        let run = scope.spawn(|| s.queue_on(signal, Duration::from_secs(120)));
        let child = s.child();
        signal.fire().unwrap();
        assert!(matches!(run.join().unwrap(), Queued::Unclear(_)));
        assert!(ended(child), "the stand-in's child outlived the fire");
    });
    assert!(began.elapsed() < Duration::from_secs(60), "the fire did not end the run: {:?}", began.elapsed());
    std::fs::remove_file(s.file("queued")).unwrap();
    assert!(matches!(s.queue_on(signal, Duration::from_secs(120)), Queued::Unclear(_)));
    assert_eq!(s.read("queued"), "", "a run started after the fire");
}

/// Rules 3 and 4, the decision alone: one request per unread episode, no reminder after an accepted or an unclear
/// one, and nothing sooner than the reminder interval after a refusal.
#[test]
fn a_new_request_goes_only_for_mail_not_yet_asked_for() {
    const OTHER: &str = "dddddddd-1111-4222-8333-444444444444";
    let t0 = Instant::now();
    let asked = Asked { thread: THREAD.into(), line: 3, at: t0, refused: false };
    let mail = |unread, fresh| Scan { total: 3 + fresh, unread, fresh };
    let v = |s: &Scan, prior: Option<&Asked>, thread: Option<&str>, now| verdict(s, prior, &|| thread.map(str::to_string), now);
    assert_eq!(v(&mail(3, 3), None, Some(THREAD), t0), Verdict::Ask);
    assert_eq!(v(&mail(0, 0), None, Some(THREAD), t0), Verdict::Clear);
    for later in [t0, t0 + REPEAT_AFTER * 30] {
        // The mail announced is still unread: no second request at any age, with more mail on top or with no binding.
        assert_eq!(v(&mail(3, 0), Some(&asked), Some(THREAD), later), Verdict::Hold);
        assert_eq!(v(&mail(4, 1), Some(&asked), Some(THREAD), later), Verdict::Hold);
        assert_eq!(v(&mail(3, 0), Some(&asked), None, later), Verdict::Hold);
    }
    // That mail was read and newer mail came, or the binding names another conversation.
    assert_eq!(v(&mail(1, 1), Some(&asked), Some(THREAD), t0), Verdict::Ask);
    assert_eq!(v(&mail(3, 0), Some(&asked), Some(OTHER), t0), Verdict::Ask);
    assert_eq!(v(&mail(0, 0), Some(&asked), Some(THREAD), t0), Verdict::Clear);
    // A refusal holds for the reminder interval whatever comes, a read inbox included; then the mail is asked for again.
    let refused = Asked { refused: true, ..asked.clone() };
    let before = t0 + REPEAT_AFTER - Duration::from_secs(1);
    for s in [mail(3, 0), mail(4, 1), mail(1, 1), mail(0, 0)] {
        for thread in [Some(THREAD), Some(OTHER), None] {
            assert_eq!(v(&s, Some(&refused), thread, before), Verdict::Hold, "{s:?} {thread:?}");
        }
    }
    assert_eq!(v(&mail(3, 0), Some(&refused), Some(THREAD), t0 + REPEAT_AFTER), Verdict::Ask);
    assert_eq!(v(&mail(0, 0), Some(&refused), Some(THREAD), t0 + REPEAT_AFTER), Verdict::Clear);
    // The binding is read only for unread mail already asked for: not with nothing unread, never asked or refused.
    let unread_binding = || -> Option<String> { panic!("the binding was read") };
    for (s, prior, now) in [(mail(0, 0), None, t0), (mail(3, 3), None, t0), (mail(0, 0), Some(&asked), t0), (mail(3, 0), Some(&refused), before), (mail(3, 0), Some(&refused), t0 + REPEAT_AFTER)] {
        verdict(&s, prior, &unread_binding, now);
    }
}

/// Rule 2: a binding is the one line of a small regular file, a conversation id; an absent file and any other content
/// are no binding. A fifo, a link (to a file that holds an id), a directory and a file longer than a line at that name
/// are none either, and none of them is opened to wait or read whole: each answer comes at once, where a plain read
/// of a fifo would wait for a writer for good.
#[test]
fn only_a_small_regular_file_holding_one_conversation_id_is_a_binding() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("codex-thread");
    let asked = |what: &str| {
        let (sent, got) = std::sync::mpsc::channel();
        let dir = tmp.path().to_path_buf();
        std::thread::spawn(move || sent.send(binding(&dir)));
        got.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| panic!("the read of {what} did not return"))
    };
    assert_eq!(asked("an absent file"), Err("its session has named no conversation in this run"));
    for (text, bound) in [(format!("{THREAD}\n"), true), (THREAD.to_string(), true), (String::new(), false), (format!("{THREAD}\n{THREAD}\n"), false), ("not an id\n".to_string(), false)] {
        std::fs::write(&file, &text).unwrap();
        assert_eq!(asked("a file").ok().as_deref(), bound.then_some(THREAD), "{text:?}");
    }
    std::fs::write(&file, format!("{THREAD}\n{}", " ".repeat(1 << 20))).unwrap();
    assert_eq!(asked("a long file"), Err("its binding file is not a small regular file"));
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    assert_eq!(asked("a directory"), Err("its binding file is not a small regular file"));
    std::fs::remove_dir(&file).unwrap();
    let name = std::ffi::CString::new(file.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
    assert_eq!(asked("a fifo"), Err("its binding file is not a small regular file"));
    std::fs::remove_file(&file).unwrap();
    std::fs::write(tmp.path().join("elsewhere"), format!("{THREAD}\n")).unwrap();
    std::os::unix::fs::symlink(tmp.path().join("elsewhere"), &file).unwrap();
    assert_eq!(asked("a link"), Err("its binding file is a link"));
}

#[test]
fn a_conversation_id_is_a_uuids_36_characters() {
    assert!(is_id(THREAD) && is_id("AAAAAAAA-1111-4222-8333-44444444444F"));
    for bad in ["", "aaaaaaaa-1111-4222-8333-44444444444", "aaaaaaaa-1111-4222-8333-4444444444444", "aaaaaaaa11111-4222-8333-444444444444", "aaaaaaaa-1111-4222-8333-44444444444g", "aaaaaaaa-1111-4222-8333-44444444444\n"] {
        assert!(!is_id(bad), "{bad:?}");
    }
}

/// Rule: a leg is not woken by mail when it has neither the hook marker nor a binding that names a conversation.
#[test]
fn a_leg_without_the_hooks_or_a_conversation_is_not_woken_by_mail() {
    let marker = crate::agents::selection::MARKER_NAME;
    let (binding_name, text) = (crate::agents::env::CODEX_THREAD_FILE, format!("{THREAD}\n"));
    for (files, not_woken) in [
        (vec![], true),
        (vec![(binding_name, text.as_str())], false),
        (vec![(marker, "")], false),
        (vec![(marker, ""), (binding_name, text.as_str())], false),
        (vec![(binding_name, "not an id\n")], true),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        for (name, body) in &files {
            std::fs::write(tmp.path().join(name), body).unwrap();
        }
        assert_eq!(not_woken_by_mail(tmp.path()), not_woken, "{files:?}");
    }
}
