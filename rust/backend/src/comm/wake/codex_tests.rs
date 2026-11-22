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

#[test]
fn a_conversation_id_is_a_uuids_36_characters() {
    assert!(is_id(THREAD) && is_id("AAAAAAAA-1111-4222-8333-44444444444F"));
    for bad in ["", "aaaaaaaa-1111-4222-8333-44444444444", "aaaaaaaa-1111-4222-8333-4444444444444", "aaaaaaaa11111-4222-8333-444444444444", "aaaaaaaa-1111-4222-8333-44444444444g", "aaaaaaaa-1111-4222-8333-44444444444\n"] {
        assert!(!is_id(bad), "{bad:?}");
    }
}
