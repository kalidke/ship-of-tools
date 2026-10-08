//! `check`: every refusal reason, and the accept that passes them all.

use super::support_tests::*;
use super::*;
use crate::agents::accounts::claude_config_dir;

fn refusal_of(
    runtime: &str,
    agent_kind: &str,
    current_account: &str,
    account: &str,
    resume: &str,
    home: &Path,
) -> Refusal {
    let accounts = crate::agents::accounts::discover_accounts(home);
    check(runtime, agent_kind, current_account, account, resume, &project_root(home, "reauth-row"), home, &accounts)
        .err()
        .expect("this case must be refused")
}

// Every refusal below is decided with nothing touched: `check` reads
// the row's facts and the accounts on disk, and nothing else.
#[test]
fn a_non_capsule_row_is_refused() {
    let home = home_with(true, &[("team", true)]);
    let r = refusal_of("tmux", "claude", "", "team", "sid", home.path());
    assert_eq!(r.code, "runtime_not_capsule");
}

#[test]
fn a_row_whose_agent_is_not_claude_is_refused() {
    let home = home_with(true, &[("team", true)]);
    for kind in ["codex", "none"] {
        let r = refusal_of("capsule", kind, "", "team", "sid", home.path());
        assert_eq!(r.code, "agent_not_claude", "kind {kind}");
    }
}

// `account_env`'s own message, carried verbatim — including the exact
// one-line fix, which the skill prints rather than restating.
#[test]
fn an_undiscovered_account_is_refused_with_the_fix_line() {
    let home = home_with(true, &[("team", true)]);
    let r = refusal_of("capsule", "claude", "", "ghost", "sid", home.path());
    assert_eq!(r.code, "unknown_account");
    assert!(r.error.contains("mkdir -p"), "{}", r.error);
}

#[test]
fn an_invalid_account_name_is_refused_before_any_path_join() {
    let home = home_with(true, &[("team", true)]);
    let r = refusal_of("capsule", "claude", "", "../.claude", "sid", home.path());
    assert_eq!(r.code, "unknown_account");
    assert!(r.error.contains("invalid account name"), "{}", r.error);
}

// A folder with no login is a valid account to CREATE a row in and a
// refusal to move a live conversation into: the switch would strand it
// behind a login prompt.
#[test]
fn an_account_with_no_login_is_refused() {
    let home = home_with(true, &[("fresh", false)]);
    let r = refusal_of("capsule", "claude", "", "fresh", "sid", home.path());
    assert_eq!(r.code, "account_not_logged_in");
    let no_default_login = home_with(false, &[("team", true)]);
    let r = refusal_of("capsule", "claude", "team", "default", "sid", no_default_login.path());
    assert_eq!(r.code, "account_not_logged_in", "a DEFAULT folder with no login is refused too");
}

#[test]
fn the_account_the_row_already_runs_is_refused_in_both_spellings() {
    let home = home_with(true, &[("team", true)]);
    for (current, asked) in [("", "default"), ("", ""), ("team", "team")] {
        let r = refusal_of("capsule", "claude", current, asked, "sid", home.path());
        assert_eq!(r.code, "already_on_account", "current {current:?} asked {asked:?}");
    }
}

#[test]
fn an_empty_resume_is_refused_rather_than_meaning_continue() {
    let home = home_with(true, &[("team", true)]);
    let r = refusal_of("capsule", "claude", "", "team", "", home.path());
    assert_eq!(r.code, "resume_required");
}

// The refusal that protects the kill: an id the TARGET cannot open would
// be an accept, a killed session, a `claude --resume` that exits at once
// and a row flapped to Terminal. The daemon is the only actor that can
// see both folders, so it is the one that proves reachability.
#[test]
fn a_transcript_the_target_account_cannot_see_is_refused() {
    let home = home_with(true, &[("team", true)]);
    let root = project_root(home.path(), "reauth-row");
    // The conversation exists — under the DEFAULT folder only, which is
    // exactly what an account folder carrying its own REAL `projects`
    // looks like from the target's side.
    seed_transcript(&claude_config_dir(home.path(), ""), "sid", &[&root]);
    let r = refusal_of("capsule", "claude", "", "team", "sid", home.path());
    assert_eq!(r.code, "resume_unreachable");
    assert!(r.error.contains("sid.jsonl"), "{}", r.error);
    // …and an id that is simply not this conversation's is the same
    // refusal, even with the folder fully shared.
    seed_transcript(&claude_config_dir(home.path(), "team"), "sid", &[&root]);
    let r = refusal_of("capsule", "claude", "", "team", "stale-id", home.path());
    assert_eq!(r.code, "resume_unreachable");
}

#[test]
fn a_logged_in_account_on_a_claude_capsule_row_is_accepted() {
    let home = home_with(true, &[("team", true)]);
    let root = project_root(home.path(), "reauth-row");
    seed_transcript(&claude_config_dir(home.path(), "team"), "sid", &[&root]);
    seed_transcript(&claude_config_dir(home.path(), ""), "sid", &[&root]);
    let accounts = crate::agents::accounts::discover_accounts(home.path());
    assert!(check("capsule", "claude", "", "team", "sid", &root, home.path(), &accounts).is_ok());
    // …and back to the default login, which is an account like any other.
    assert!(check("capsule", "claude", "team", "default", "sid", &root, home.path(), &accounts).is_ok());
}
