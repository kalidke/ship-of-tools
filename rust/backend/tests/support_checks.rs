#![cfg(target_os = "linux")]
//! The shared support's own answers: the capsule listing includes what is the test's own and excludes the rest, a read
//! that fails is never read as absence, a kill never signals a process whose identity changed after its pidfd opened,
//! and the command it gives a daemon names the Julia binary the runner found.

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;

/// The command the support gives a daemon hands it the Julia binary `julia_binary` names, as `SOT_JULIA_BIN`, and no
/// `SOT_JULIA_BIN` when none is found.
#[test]
fn a_daemon_is_handed_the_julia_binary_the_runner_found() {
    let given = support::sotd_command()
        .get_envs()
        .find(|(name, _)| *name == "SOT_JULIA_BIN")
        .and_then(|(_, value)| value)
        .map(|value| value.to_string_lossy().into_owned());
    assert_eq!(given, support::julia_binary());
}

/// A `/proc` the test writes: each pid's command line, or the error reading it gives.
struct FakeProc {
    entries: Vec<(u32, std::io::Result<Vec<u8>>)>,
    list_error: bool,
}

impl FakeProc {
    fn of(entries: Vec<(u32, std::io::Result<Vec<u8>>)>) -> Self {
        Self { entries, list_error: false }
    }
}

impl support::ProcFs for FakeProc {
    fn pids(&self) -> std::io::Result<Vec<u32>> {
        if self.list_error {
            return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        }
        Ok(self.entries.iter().map(|(pid, _)| *pid).collect())
    }

    fn cmdline(&self, pid: u32) -> std::io::Result<Vec<u8>> {
        match &self.entries.iter().find(|(p, _)| *p == pid).expect("a listed pid").1 {
            Ok(bytes) => Ok(bytes.clone()),
            Err(error) => Err(match error.raw_os_error() {
                Some(code) => std::io::Error::from_raw_os_error(code),
                None => std::io::Error::from(error.kind()),
            }),
        }
    }
}

fn argv(parts: &[&str]) -> std::io::Result<Vec<u8>> {
    Ok(parts.join("\0").into_bytes())
}

const EXE: &str = "/scratch/target/debug/sot-capsule";
const ROOT: &str = "/tmp/sotcw-abc/state";

fn select(fs: &FakeProc) -> Result<Vec<(bool, u32)>, String> {
    support::select_capsule_procs(fs, std::path::Path::new(EXE), std::path::Path::new(ROOT))
}

/// The listing includes a capsule of this build over the root (by the exe's path or its file name, supervisors first)
/// and excludes another program, another root, a root that only shares a prefix, and a process with no command line.
#[test]
fn the_capsule_listing_includes_its_own_and_excludes_the_rest() {
    let fs = FakeProc::of(vec![
        (10, argv(&[EXE, "run", "/tmp/sotcw-abc/state/sot/workspaces/a"])),
        (11, argv(&["sot-capsule", "supervise", "/tmp/sotcw-abc/state/sot/workspaces/a"])),
        (12, argv(&["/usr/bin/tail", "-f", ROOT])),
        (13, argv(&[EXE, "supervise", "/tmp/sotcw-other/state/sot/workspaces/a"])),
        (14, argv(&[EXE, "supervise", "/tmp/sotcw-abc/state-two/sot/workspaces/a"])),
        (15, Ok(Vec::new())),
        (16, argv(&["/elsewhere/sot-capsule", "supervise", "/tmp/sotcw-abc/state/x"])),
    ]);
    assert_eq!(select(&fs), Ok(vec![(false, 11), (true, 10)]));
}

/// A process that ended between the listing and the read is absent; any other failed read, or a failed listing, is an
/// error that names the failure, never "no capsule".
#[test]
fn a_failed_observation_is_an_error_and_a_vanished_process_is_absence() {
    let gone = FakeProc::of(vec![
        (1, Err(std::io::Error::from(std::io::ErrorKind::NotFound))),
        (2, Err(std::io::Error::from_raw_os_error(libc::ESRCH))),
        (3, argv(&[EXE, "supervise", "/tmp/sotcw-abc/state/x"])),
    ]);
    assert_eq!(select(&gone), Ok(vec![(false, 3)]));

    let denied = FakeProc::of(vec![(4, Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)))]);
    let error = select(&denied).expect_err("a read that failed for permission read as absence");
    assert!(error.contains("/proc/4/cmdline"), "{error}");

    let mut unlisted = FakeProc::of(Vec::new());
    unlisted.list_error = true;
    assert!(select(&unlisted).expect_err("a failed listing read as no capsule").contains("list /proc"));
}

/// A kill signals a process only when its command line, read after its pidfd is open, still names a capsule over the
/// root: a pid whose identity changed is left alone; one that matches is ended.
#[test]
fn a_kill_signals_only_a_process_that_still_names_the_capsule() {
    let mut child = std::process::Command::new("sleep").arg("60").spawn().expect("spawn sleep");
    let pid = child.id();
    let (exe, root) = (std::path::Path::new(EXE), std::path::Path::new(ROOT));

    let reused = FakeProc::of(vec![(pid, argv(&["/usr/bin/sleep", "60"]))]);
    support::kill_verified(&reused, &[pid], exe, root).expect("a pid that is not ours is left alone");
    assert!(child.try_wait().expect("try_wait").is_none(), "a process that is not a capsule was killed");

    let ours = FakeProc::of(vec![(pid, argv(&[EXE, "supervise", "/tmp/sotcw-abc/state/x"]))]);
    support::kill_verified(&ours, &[pid], exe, root).expect("kill the matching process");
    assert!(child.wait().expect("wait").code().is_none(), "the matching process must end by signal");
}
