//! A reauth resumes only the target row's own conversation: the cross-row
//! refusal and the rule that a transcript belongs to the directory it started in.

use super::support_tests::*;
use super::*;
use crate::agents::accounts::claude_config_dir;

// THE DONE TEST. The defect's shape: one row's session asks for ANOTHER row
// with its OWN transcript id, and that transcript also carries lines written
// from the target's root (what a resume in the wrong row appends). A
// conversation belongs to the row it was started in: refused, nothing moves.
// The target's own transcript is accepted.
#[tokio::test]
async fn a_reauth_resumes_only_the_target_rows_own_conversation() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    pin_home(home.path(), scratch.path());
    seed_claude_binary(home.path());
    let caller_root = project_root(home.path(), "caller-row");
    let target_root = project_root(home.path(), "target-row");
    let reg = Workspaces::new();
    seed_row_into(&reg, "caller-row", &caller_root, "", "caller-handle");
    let (target, target_slug) =
        seed_row_into(&reg, "target-row", &target_root, "", "target-handle");
    let team = claude_config_dir(home.path(), "team");
    seed_transcript(&team, &sid(11), &[&caller_root, &target_root]);
    seed_transcript(&team, &sid(12), &[&target_root]);

    let (payload, restart) = reauth(&reg, &target, "team", &sid(11)).await;
    assert_eq!(payload["code"], "resume_not_this_row", "{payload:?}");
    assert!(
        payload["error"].as_str().unwrap().contains("caller-row"),
        "names where it started: {payload:?}"
    );
    assert!(restart.is_none(), "a refusal hands back no restart");
    assert_eq!(
        reg.resolve(Some(&target)).unwrap().account(),
        "",
        "the record never moved"
    );
    assert!(
        !crate::rows::store::toml_path_for(&target_slug).exists(),
        "nothing was persisted"
    );

    let (payload, restart) = reauth(&reg, &target, "team", &sid(12)).await;
    assert_eq!(
        payload["code"], ACCEPTED_CODE,
        "the row's own conversation still moves: {payload:?}"
    );
    drop(restart);
}

/// Another spelling of `root` that names the same directory: a symlink to it
/// on Unix; on Windows the same path upper-cased with `/` separators.
#[cfg(unix)]
fn another_spelling(root: &Path, home: &Path) -> PathBuf {
    let link = home.join("link-to-row");
    std::os::unix::fs::symlink(root, &link).unwrap();
    link
}
#[cfg(windows)]
fn another_spelling(root: &Path, _home: &Path) -> PathBuf {
    PathBuf::from(root.to_str().unwrap().replace('\\', "/").to_uppercase())
}

// The rule is the directory, not its spelling, and only the directory the
// session STARTED in. Refusals first, on one row: each changes nothing.
#[tokio::test]
async fn a_transcript_is_the_rows_by_the_directory_it_started_in() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    pin_home(home.path(), scratch.path());
    seed_claude_binary(home.path());
    let root = project_root(home.path(), "reauth-row");
    let sibling = project_root(home.path(), "sibling-row");
    let moved_away = home.path().join("proj").join("moved-away"); // never created
    let team = claude_config_dir(home.path(), "team");
    seed_transcript(&team, &sid(21), &[&sibling]);
    seed_transcript(&team, &sid(22), &[]);
    seed_transcript(&team, &sid(23), &[&moved_away]);
    // A path-shaped id: `-a-project-root/../escaped` reaches projects/escaped.jsonl.
    write_transcript(
        &team.join("projects").join("escaped.jsonl"),
        "escaped",
        &[&root],
    );
    seed_transcript(&team, &sid(24), &[&another_spelling(&root, home.path())]);
    let (reg, id, _slug) = seed_capsule_row(&root, "", "row-declared-handle");

    for (resume, code) in [
        (sid(21), "resume_not_this_row"),
        (sid(22), "resume_not_this_row"),
        (sid(23), "resume_not_this_row"),
        ("../escaped".to_string(), "resume_unreachable"),
    ] {
        let (payload, restart) = reauth(&reg, &id, "team", &resume).await;
        assert_eq!(payload["code"], code, "resume {resume:?}: {payload:?}");
        assert!(restart.is_none(), "resume {resume:?}");
        assert_eq!(
            reg.resolve(Some(&id)).unwrap().account(),
            "",
            "resume {resume:?}: a refusal changes nothing"
        );
    }
    let (payload, restart) = reauth(&reg, &id, "team", &sid(24)).await;
    assert_eq!(
        payload["code"], ACCEPTED_CODE,
        "another spelling of the row's root is its root: {payload:?}"
    );
    drop(restart);
}

// A resume id is a session id, a UUID, before it is joined into a path or
// handed to claude: a flag-shaped id (claude would read it as a flag) and a
// plain word are refused even when a transcript by that name, started in this
// row's root, exists.
#[tokio::test]
async fn a_resume_id_must_be_a_session_uuid() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    pin_home(home.path(), scratch.path());
    seed_claude_binary(home.path());
    let root = project_root(home.path(), "reauth-row");
    let team = claude_config_dir(home.path(), "team");
    let (reg, id, _slug) = seed_capsule_row(&root, "", "row-declared-handle");
    for resume in [
        "--dangerously-skip-permissions",
        "-0000000-0000-4000-8000-000000000031",
        "sid-plain",
        "00000000-0000-4000-8000-00000000003A",
    ] {
        seed_transcript(&team, resume, &[&root]);
        let (payload, restart) = reauth(&reg, &id, "team", resume).await;
        assert_eq!(
            payload["code"], "resume_unreachable",
            "resume {resume:?}: {payload:?}"
        );
        assert!(restart.is_none(), "resume {resume:?}");
    }
    seed_transcript(&team, &sid(31), &[&root]);
    let (payload, restart) = reauth(&reg, &id, "team", &sid(31)).await;
    assert_eq!(payload["code"], ACCEPTED_CODE, "{payload:?}");
    drop(restart);
}

// A refusal says what it found and gives no command to run: the caller is
// usually the row itself, which just ran the only command there is. A root
// that cannot be opened is named as such, with the error.
#[tokio::test]
async fn a_refusal_names_where_the_transcript_started_and_gives_no_command() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    pin_home(home.path(), scratch.path());
    seed_claude_binary(home.path());
    let caller_root = project_root(home.path(), "caller-row");
    let target_root = project_root(home.path(), "target-row");
    let gone_root = project_root(home.path(), "gone-row");
    let reg = Workspaces::new();
    let (target, _) = seed_row_into(&reg, "target-row", &target_root, "", "target-handle");
    let (gone, _) = seed_row_into(&reg, "gone-row", &gone_root, "", "gone-handle");
    let team = claude_config_dir(home.path(), "team");
    seed_transcript(&team, &sid(11), &[&caller_root]);
    seed_transcript(&team, &sid(13), &[&gone_root]);

    let (payload, _) = reauth(&reg, &target, "team", &sid(11)).await;
    let error = payload["error"].as_str().unwrap();
    assert!(
        error.contains("was started in")
            && error.contains("caller-row")
            && error.contains("target-row"),
        "names both directories: {error}"
    );
    assert!(!error.contains("sot-fe"), "no command to run: {error}");

    std::fs::remove_dir(&gone_root).unwrap();
    let (payload, _) = reauth(&reg, &gone, "team", &sid(13)).await;
    assert_eq!(payload["code"], "resume_not_this_row", "{payload:?}");
    let error = payload["error"].as_str().unwrap();
    assert!(
        error.contains("gone-row") && error.contains("cannot be opened"),
        "{error}"
    );
}

// A relative start directory says nothing about where the session ran: read
// against this daemon's own working directory it can reach any folder. It is
// refused even when, read that way, it reaches this row's root.
#[cfg(unix)]
#[tokio::test]
async fn a_relative_start_directory_is_refused() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    pin_home(home.path(), scratch.path());
    seed_claude_binary(home.path());
    let root = project_root(home.path(), "reauth-row");
    let here = std::env::current_dir().unwrap();
    let up = "../".repeat(here.components().count() - 1);
    let relative = PathBuf::from(format!("{up}{}", root.strip_prefix("/").unwrap().display()));
    assert_eq!(
        sot_log::host::dir_identity(&relative).ok(),
        sot_log::host::dir_identity(&root).ok(),
        "read from here, the relative spelling reaches the row's root"
    );
    seed_transcript(
        &claude_config_dir(home.path(), "team"),
        &sid(41),
        &[&relative],
    );
    let (reg, id, _slug) = seed_capsule_row(&root, "", "row-declared-handle");
    let (payload, restart) = reauth(&reg, &id, "team", &sid(41)).await;
    assert_eq!(payload["code"], "resume_not_this_row", "{payload:?}");
    assert!(restart.is_none(), "a refusal hands back no restart");
}

// A row replaced while its reauth waits for the row's guard: `check` proved
// the transcript against the old root, and a `workspace.create` re-inserted
// the slug at another root. Nothing moves on the replacement, whose root
// nothing checked.
#[tokio::test]
async fn a_row_replaced_while_its_reauth_waits_is_refused() {
    let _g = env_guarded();
    let home = home_with(true, &[("team", true)]);
    let scratch = tempfile::tempdir().unwrap();
    pin_home(home.path(), scratch.path());
    seed_claude_binary(home.path());
    let root = project_root(home.path(), "reauth-row");
    let moved = project_root(home.path(), "moved-row");
    seed_transcript(&claude_config_dir(home.path(), "team"), &sid(51), &[&root]);
    let (reg, id, _slug) = seed_capsule_row(&root, "", "row-declared-handle");
    let guard = reg
        .capsule_guard(&id)
        .expect("a registered row has a guard");
    let held = guard.clone().lock_owned().await;
    let waiting = tokio::spawn({
        let (reg, id) = (reg.clone(), id.clone());
        async move { reauth(&reg, &id, "team", &sid(51)).await }
    });
    // Four handles on the guard once the reauth waits for it: the registry's,
    // this test's, the held lock's and the waiting reauth's.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::sync::Arc::strong_count(&guard) < 4 {
        assert!(
            std::time::Instant::now() < deadline,
            "the reauth never reached the row's guard"
        );
        tokio::task::yield_now().await;
    }
    let (same_id, _) = seed_row_into(&reg, "reauth-row", &moved, "", "row-declared-handle");
    assert_eq!(same_id, id, "a re-inserted slug keeps its id");
    drop(held);

    let (payload, restart) = waiting.await.unwrap();
    assert_eq!(payload["code"], "unknown_workspace", "{payload:?}");
    assert!(restart.is_none(), "a refusal hands back no restart");
    let now = reg.resolve(Some(&id)).unwrap();
    assert_eq!(now.project_root, moved);
    assert_eq!(
        now.account(),
        "",
        "the replacement's account is not touched"
    );
}

// The start directory is the first line that parses and records a string
// `cwd`; every line before it that does not is skipped.
#[test]
fn lines_before_the_start_directory_that_record_none_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    let lines = [
        "not json",
        "[1, 2]",
        "{\"cwd\": 7}",
        "{\"cwd\": null}",
        "{\"type\": \"user\", \"cwd\": \"/first\"}",
        "{\"cwd\": \"/second\"}",
    ];
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    assert_eq!(started_in(&path), Some(PathBuf::from("/first")));
}

// `HEAD_SCAN_BYTES` bounds the read: a start directory whose line ends at the
// bound is found, and one byte more of what precedes it puts it out of reach.
#[test]
fn the_start_directory_is_read_only_from_the_first_head_scan_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let start = "{\"cwd\":\"/root\"}";
    let with_filler = |pad: usize| {
        let path = dir.path().join(format!("transcript-{pad}.jsonl"));
        let filler = format!(
            "{{\"type\":\"queue-operation\",\"pad\":\"{}\"}}\n",
            "x".repeat(pad)
        );
        std::fs::write(&path, format!("{filler}{start}\n")).unwrap();
        started_in(&path)
    };
    let fixed = "{\"type\":\"queue-operation\",\"pad\":\"\"}\n".len() + start.len();
    let fits = usize::try_from(HEAD_SCAN_BYTES).unwrap() - fixed;
    assert_eq!(
        with_filler(fits),
        Some(PathBuf::from("/root")),
        "a line that ends at the bound is read"
    );
    assert_eq!(with_filler(fits + 1), None, "a line the bound cuts is not");
}
