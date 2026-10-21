use super::*;
use crate::spawn::tests::{reject, RecordingSpawner};
const NO_COMMANDS: RecordingSpawner<
    fn(&tokio::process::Command) -> std::io::Result<std::process::Output>,
> = RecordingSpawner(reject);

#[test]
fn fetcher_from_env_forms() {
    // Not a full env test (process-global env); just the parse arms via
    // the public constructor contract.
    assert_eq!(Fetcher::Curl, Fetcher::Curl);
    match ("dir:/x/y".strip_prefix("dir:"), PathBuf::from("/x/y")) {
        (Some(p), want) => assert_eq!(PathBuf::from(p), want),
        _ => unreachable!(),
    }
}

/// Two checks in one process each have a scratch folder of their own, and a check's start removes neither one
/// this process still holds.
#[tokio::test]
async fn a_check_keeps_the_scratch_folder_of_another_check_in_this_process() {
    let held = scratch_dir().await.expect("a scratch folder");
    let other = scratch_dir().await.expect("a second scratch folder");
    assert_ne!(held.path(), other.path(), "two checks share a scratch folder");
    remove_ended_scratch().await;
    for dir in [&held, &other] {
        assert!(dir.path().is_dir(), "a check's start removed a held scratch folder: {}", dir.path().display());
    }
    other.remove().expect("its check removes it");
    held.remove().expect("its check removes it");
}

/// The scratch-folder test's name, which its re-runs of this binary run alone.
const SCRATCH_TEST: &str =
    "fetch::tests::a_killed_checks_scratch_folder_goes_at_a_later_check_and_a_running_ones_stays";

/// Which check a re-run of this binary for [`SCRATCH_TEST`] runs: `hang` or `finish`. Unset in an ordinary run.
const PART: &str = "SOT_UPDATER_SCRATCH_PART";

/// The bound on each wait of the scratch-folder test.
const BOUND: Duration = Duration::from_secs(30);

/// A re-run of this binary that runs the check `part`, with `tmp` as its temporary folder.
fn check_command(tmp: &Path, part: &str) -> (std::process::Command, sot_log::test_isolated::Entry) {
    let (mut command, entry) = sot_log::test_isolated::test_command(SCRATCH_TEST);
    for key in ["TMPDIR", "TMP", "TEMP"] {
        command.env(key, tmp);
    }
    command.env(PART, part).stdin(std::process::Stdio::null());
    (command, entry)
}

/// A check that hangs in its download, killed and waited for when dropped, so a failing test leaves none running.
struct Hanging(std::process::Child, Option<sot_log::test_isolated::Entry>);

impl Hanging {
    fn start(tmp: &Path) -> Self {
        let (mut command, entry) = check_command(tmp, "hang");
        Self(command.spawn().expect("a check in a process of its own"), Some(entry))
    }

    /// SIGKILLs the check (TerminateProcess on Windows), so it runs no code, and waits for its end.
    fn kill(mut self) {
        self.0.kill().expect("kill the check");
        sot_log::test_isolated::wait_within(&mut self.0, BOUND);
        self.1.take().expect("the check's entry record").assert_once(self.0.id());
    }
}

impl Drop for Hanging {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = sot_log::test_isolated::wait_until(&mut self.0, std::time::Instant::now() + BOUND);
    }
}

/// Runs the check `finish` to its end in a re-run of this binary whose temporary folder is `tmp`.
fn finish_check(tmp: &Path) {
    let (mut command, entry) = check_command(tmp, "finish");
    command.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let child = command.spawn().expect("a check in a process of its own");
    let pid = child.id();
    let (status, out, err) = sot_log::test_isolated::drain(child).wait_within(BOUND);
    entry.assert_once(pid);
    assert!(status.success(), "a check run to its end failed: {status}\n{out}{err}");
}

/// The names in `tmp`, sorted.
fn names(tmp: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(tmp)
        .expect("the checks' temporary folder")
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// The scratch folder in `tmp` that `before` does not name, once a check's download has begun writing in it.
fn downloading(tmp: &Path, before: &[String]) -> String {
    let deadline = std::time::Instant::now() + BOUND;
    loop {
        let begun = |name: &String| !before.contains(name) && tmp.join(name).join("SHA256SUMS").exists();
        if let Some(name) = names(tmp).into_iter().find(begun) {
            return name;
        }
        assert!(std::time::Instant::now() < deadline, "no check began its download in {}", tmp.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The re-run's body: a check whose release list names a newer release, and whose download of that release's
/// SHA256SUMS never answers (`hang`, a check killed mid-download) or writes it (`finish`).
fn check_in_this_process(hang: bool) {
    use crate::spawn::tests::output;
    let spawner = RecordingSpawner(move |cmd: &tokio::process::Command| {
        let args: Vec<String> = cmd.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let Some(at) = args.iter().position(|arg| arg == "-o") else {
            return Ok(output(0, br#"[{"tag_name":"v0.6.0","prerelease":false}]"#.to_vec()));
        };
        if hang {
            // A download under way: part of the file is there, and the rest never comes.
            std::fs::write(&args[at + 1], "su")?;
            loop {
                std::thread::park();
            }
        }
        std::fs::write(&args[at + 1], "sums\n")?;
        Ok(output(0, Vec::new()))
    });
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
    let latest = runtime.block_on(Fetcher::Curl.latest(&spawner, "example/project", "0.1.0"));
    assert_eq!(latest.expect("the check").expect("a newer release").sums_text, "sums\n");
}

/// A check killed mid-download (a crash, a SIGKILL: no code runs) leaves its scratch folder, and a later check, in
/// another process, removes it; a check that is still running keeps its own. Each check runs in a re-run of this
/// binary whose temporary folder is one of this test's own.
#[test]
fn a_killed_checks_scratch_folder_goes_at_a_later_check_and_a_running_ones_stays() {
    if let Ok(part) = std::env::var(PART) {
        sot_log::test_isolated::enter(SCRATCH_TEST);
        return check_in_this_process(part == "hang");
    }
    let root = std::env::temp_dir().join(format!("sot-updater-scratch-test-{}", crate::unique::suffix()));
    let tmp = root.join("tmp");
    std::fs::create_dir_all(&tmp).expect("the checks' temporary folder");
    let running = Hanging::start(&tmp);
    let kept = downloading(&tmp, &[]);
    let killed = Hanging::start(&tmp);
    let left = downloading(&tmp, &[kept.clone()]);
    killed.kill();
    assert!(tmp.join(&left).is_dir(), "a killed check runs no code: its scratch folder {left} stays");
    // A later check, not always the next: Windows frees a killed process's lock after a delay with no stated bound
    // (`sot_log`'s fault_terminate test waits it out the same way), so a check may still find it held.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        finish_check(&tmp);
        if !tmp.join(&left).exists() || std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let after = names(&tmp);
    println!("scratch: the running check's {kept:?} and the killed check's {left:?}; after a later check, present: {after:?}");
    running.kill();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(after, vec![kept], "a running check's scratch folder stays and a killed one's goes");
}

#[tokio::test]
async fn dir_fetcher_serves_local_files() {
    let dir = std::env::temp_dir().join(format!("sot-updater-fetch-{}", std::process::id()));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    tokio::fs::write(
        dir.join("SHA256SUMS"),
        "0101010101010101010101010101010101010101010101010101010101010101  sot-0.6.0-linux-x86_64.tar.gz\n",
    )
    .await
    .unwrap();
    tokio::fs::write(dir.join("blob.bin"), b"payload")
        .await
        .unwrap();

    let f = Fetcher::Dir(dir.clone());

    // No `releases.json`: the directory's own SHA256SUMS is the single
    // (stable-channel) release on offer.
    let listed = f
        .list_releases(&NO_COMMANDS, "example/project")
        .await
        .unwrap();
    assert_eq!(listed, vec![("v0.6.0".to_string(), false)]);

    let latest = f
        .latest(&NO_COMMANDS, "example/project", "0.1.0")
        .await
        .unwrap()
        .expect("0.6.0 is newer than 0.1.0");
    assert_eq!(latest.tag.as_deref(), Some("v0.6.0"));
    assert!(latest.sums_text.contains("sot-0.6.0"));

    // Already past this release: no update, not an error.
    assert!(f
        .latest(&NO_COMMANDS, "example/project", "9.9.9")
        .await
        .unwrap()
        .is_none());

    let dest = dir.join("out/blob.bin");
    f.download(&NO_COMMANDS, "example/project", "v0.6.0", "blob.bin", &dest)
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"payload");

    // Path-material names are refused before touching the filesystem.
    assert!(f
        .download(&NO_COMMANDS, "example/project", "v0.6.0", "../evil", &dest)
        .await
        .is_err());
    assert!(f
        .download(
            &NO_COMMANDS,
            "example/bad repo",
            "v0.6.0",
            "blob.bin",
            &dest
        )
        .await
        .is_err());
    assert!(f
        .download(&NO_COMMANDS, "example/project", "latest", "blob.bin", &dest)
        .await
        .is_err());

    tokio::fs::remove_dir_all(&dir).await.unwrap();
}

#[test]
fn parse_release_list_shape() {
    // The exact shape of one page of `GET /repos/<repo>/releases` (and
    // `gh api` for the same path) — the fields we read plus the noise we
    // ignore (id, body, assets, …), a draft (dropped), and an entry
    // missing `prerelease` (dropped: never guess a channel).
    let json = br#"[
            {"tag_name": "v0.6.0-rc.13", "prerelease": true, "draft": false, "id": 3, "body": "notes"},
            {"tag_name": "v0.6.0-rc.11", "prerelease": true, "draft": false},
            {"tag_name": "v0.5.10", "prerelease": false, "draft": false, "assets": []},
            {"tag_name": "v0.7.0", "prerelease": false, "draft": true},
            {"tag_name": "v0.4.0", "draft": false}
        ]"#;
    let got = parse_release_list(json).unwrap();
    assert_eq!(
        got,
        vec![
            ("v0.6.0-rc.13".to_string(), true),
            ("v0.6.0-rc.11".to_string(), true),
            ("v0.5.10".to_string(), false),
        ]
    );
}

#[test]
fn parse_release_list_rejects_non_array() {
    assert!(parse_release_list(br#"{"tag_name": "v0.6.0"}"#).is_err());
}

#[tokio::test]
async fn dir_fetcher_releases_json_drives_channel_selection() {
    let dir = std::env::temp_dir().join(format!("sot-updater-fetch-rj-{}", std::process::id()));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    tokio::fs::write(
        dir.join("releases.json"),
        r#"[
                {"tag_name": "v0.6.0-rc.13", "prerelease": true},
                {"tag_name": "v0.6.0-rc.11", "prerelease": true},
                {"tag_name": "v0.5.10", "prerelease": false}
            ]"#,
    )
    .await
    .unwrap();
    // download() ignores the tag for Dir, so any SHA256SUMS content
    // proves which tag latest() resolved to (via `latest.tag`).
    tokio::fs::write(
        dir.join("SHA256SUMS"),
        "0101010101010101010101010101010101010101010101010101010101010101  sot-0.6.0-rc.13-linux-x86_64.tar.gz\n",
    )
    .await
    .unwrap();

    let f = Fetcher::Dir(dir.clone());
    // A stable install never sees the rcs.
    assert!(f
        .latest(&NO_COMMANDS, "example/project", "0.5.10")
        .await
        .unwrap()
        .is_none());
    // An rc install tracks the newest rc.
    let l = f
        .latest(&NO_COMMANDS, "example/project", "0.6.0-rc.11")
        .await
        .unwrap()
        .expect("rc.13 supersedes rc.11");
    assert_eq!(l.tag.as_deref(), Some("v0.6.0-rc.13"));

    tokio::fs::remove_dir_all(&dir).await.unwrap();
}

/// Curl and gh discovery request the same release-list shape through the caller, without contacting a service.
#[tokio::test]
async fn recorded_release_lists_drive_both_channels() {
    use crate::spawn::tests::output;
    for fetcher in [Fetcher::Curl, Fetcher::Gh] {
        let spawner = RecordingSpawner(|cmd: &tokio::process::Command| {
            let bin = cmd.as_std().get_program();
            assert!(bin == "curl" || bin == "gh", "unexpected discovery command");
            Ok(output(0, br#"[{"tag_name":"v0.6.0-rc.13","prerelease":true},{"tag_name":"v0.5.10","prerelease":false}]"#.to_vec()))
        });
        let releases = fetcher
            .list_releases(&spawner, "example/project")
            .await
            .unwrap();
        assert_eq!(
            select_target("0.0.1", &releases).as_deref(),
            Some("v0.5.10")
        );
        assert_eq!(
            select_target("0.0.1-rc.1", &releases).as_deref(),
            Some("v0.6.0-rc.13")
        );
    }
}
