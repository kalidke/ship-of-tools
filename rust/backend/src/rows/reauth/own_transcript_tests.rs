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
