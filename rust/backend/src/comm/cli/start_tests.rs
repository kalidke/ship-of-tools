use super::*;
use crate::comm::test_home::TestHome;

#[test]
fn a_session_start_joins_and_prints_one_bootstrap_line_and_the_rules() {
    let t = TestHome::new();
    let o = session_start(&t.env, &[]);
    let h = crate::comm::identity::derive::tier(1, &t.root(), "box1", Default::default()).1;
    assert_eq!(o.code, 0);
    assert!(o.stdout.contains(&format!("Joined sot-comm as @{h} ")), "{}", o.stdout);
    assert!(o.stdout.contains(&format!("\nBOOTSTRAP-ARM handle={h} identity=ok WAKE: daemon\n")), "{}", o.stdout);
    assert!(o.stdout.contains("Work-state (nav row colour)") && o.stdout.contains("PERSISTENT REPL"));
    assert!(o.stdout.contains("Others in this computer's registry: ") && o.stdout.contains("still reaches them through the hub"), "{}", o.stdout);
    assert!(o.stdout.lines().any(|line| line.contains("SITREP-QUESTION: <question> Irreversible: <what happens>. Options: (a) ...; (b) .... Recommend (a). Page: <absolute path>")), "{}", o.stdout);
    assert!(!o.stdout.contains("In a row:"), "a session outside any row is told nothing about a window: {}", o.stdout);
    assert_eq!(session_start(&t.env, &["x".into()]).code, 2);
}

/// A row's session is told it is in the window, and how its person gets a terminal of their own.
#[test]
fn a_rows_session_start_says_it_is_in_the_window_and_how_to_give_its_person_a_terminal() {
    let mut t = TestHome::new();
    assert_eq!(row_note(&t.env), None);
    t.set("SOT_WORKSPACE_ID", "ws-1");
    let line = row_note(&t.env).expect("a row's leg carries its workspace id");
    assert!(line.starts_with("In a row:"), "{line}");
    assert!(line.contains("comm-spawn.sh <a folder no row holds") && line.contains("--agent none") && line.contains("sot-fe goto"), "{line}");
    assert!(line.contains("Shift+Enter"), "{line}");
}

/// A slot that names another project's identity is never joined over without a pin.
#[test]
fn a_session_start_refuses_a_slot_another_project_holds() {
    let mut t = TestHome::new();
    // The shared slot is last-writer-wins; a private one is a session's own.
    let private = t.home.join("self").join("mine.txt");
    crate::comm::identity::self_file::write(&private, "other-box1", "other", "/other").unwrap();
    t.set("SOT_COMM_SELF_FILE", &private);
    let o = session_start(&t.env, &[]);
    assert!(o.stdout.starts_with("BOOTSTRAP-ARM handle=none identity=FAIL"), "{}", o.stdout);
    assert!(o.stderr.contains("REFUSED:"), "{}", o.stderr);
}

/// A row's open question outlives its session, and the new session is told.
#[test]
fn a_session_start_names_the_question_its_row_kept() {
    let t = TestHome::new();
    let h = crate::comm::identity::derive::tier(1, &t.root(), "box1", Default::default()).1;
    let o = session_start(&t.env, &[]);
    assert!(!o.stdout.contains("OPEN QUESTION"), "{}", o.stdout);
    t.put(&h, serde_json::json!({"host": "box1", "workspace_id": "", "root": t.root(), "question": "which one?"}));
    let o = session_start(&t.env, &[]);
    let line = o.stdout.lines().find(|l| l.starts_with("OPEN QUESTION")).unwrap_or_else(|| panic!("{}", o.stdout));
    assert!(line.contains("): which one? Your person's answer arrives as a prompt in this pane."), "{line}");
}

/// After a clear or a compaction the start joins nothing: the row's registry entry, work-state facts included, is as it
/// was (a join writes a whole new entry), and the session is told its handle.
#[test]
fn a_start_again_changes_nothing_and_names_the_handle() {
    let t = TestHome::new();
    let h = crate::comm::identity::derive::tier(1, &t.root(), "box1", Default::default()).1;
    assert_eq!(session_start(&t.env, &[]).code, 0);
    let mut entry = t.agents()[&h].clone();
    entry["waiting"] = serde_json::json!("a job");
    entry["floor"] = serde_json::json!("user");
    entry["state"] = serde_json::json!("waiting");
    t.put(&h, entry.clone());
    let o = session_start(&t.env, &["--again".into()]);
    assert_eq!(o.code, 0);
    assert_eq!(t.agents()[&h], entry, "the entry is untouched");
    assert!(o.stdout.starts_with(&format!("BOOTSTRAP-ARM handle={h} identity=ok WAKE: daemon\n")), "{}", o.stdout);
    assert!(!o.stdout.contains("Joined sot-comm"), "{}", o.stdout);
    assert!(o.stdout.contains("Work-state (nav row colour)"), "{}", o.stdout);
    assert!(!o.stdout.contains("UNREAD"), "{}", o.stdout);
    assert!(o.stdout.contains(&format!("you are still joined as @{h}, and nothing was re-joined.")), "{}", o.stdout);
}

/// The question a row kept is told again after a clear: it is what stops a session from stamping it away.
#[test]
fn a_start_again_names_the_question_its_row_kept() {
    let t = TestHome::new();
    let h = crate::comm::identity::derive::tier(1, &t.root(), "box1", Default::default()).1;
    assert_eq!(session_start(&t.env, &[]).code, 0);
    let mut entry = t.agents()[&h].clone();
    entry["question"] = serde_json::json!("which one?");
    t.put(&h, entry);
    let o = session_start(&t.env, &["--again".into()]);
    let line = o.stdout.lines().find(|l| l.starts_with("OPEN QUESTION")).unwrap_or_else(|| panic!("{}", o.stdout));
    assert!(line.contains("): which one? Your person's answer arrives as a prompt in this pane."), "{line}");
}

/// A comm folder whose lock manager is this box's own lets a reader take the inbox lock, which creates its file: the
/// again form reads no inbox, so no file appears under the folder.
#[test]
fn a_start_again_creates_no_file_under_the_comm_folder() {
    let t = TestHome::new();
    assert_eq!(session_start(&t.env, &[]).code, 0);
    let inbox = t.home.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let own = crate::comm::mail::inbox::lock_identity(&inbox);
    std::fs::write(t.home.join("inbox-lock-manager"), format!("{own}\n")).unwrap();
    // On a host with no machine id the lock is bare `none`, which this box never takes: the test could not fail.
    assert!(crate::comm::mail::inbox::appends_here(&t.home, &own), "this host cannot take the inbox lock, so the test cannot test: {own}");
    let before = files(&t.home);
    assert_eq!(session_start(&t.env, &["--again".into()]).code, 0);
    assert_eq!(files(&t.home), before);
}

/// Unlike send, poll and join, the again form does not make a comm folder that is not whole whole: a registry that is
/// gone reads as missing, and nothing is created.
#[test]
fn a_start_again_in_a_folder_that_is_not_whole_creates_nothing() {
    let t = TestHome::new();
    let h = crate::comm::identity::derive::tier(1, &t.root(), "box1", Default::default()).1;
    assert_eq!(session_start(&t.env, &[]).code, 0);
    std::fs::remove_file(t.registry()).unwrap();
    std::fs::remove_dir_all(t.home.join("inbox")).unwrap();
    let before = files(&t.home);
    let o = session_start(&t.env, &["--again".into()]);
    assert_eq!(files(&t.home), before);
    assert!(
        o.stdout.ends_with(&format!("comm-session-start: the registry is missing, so @{h} is unverified; run /sot-session-start to join\n")),
        "{}",
        o.stdout
    );
}

/// Every path under `dir`, sorted.
fn files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut v = walk(dir);
    v.sort();
    v
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        out.push(p.clone());
        if p.is_dir() {
            out.extend(walk(&p));
        }
    }
    out
}

/// A session with no identity, or a handle whose registry entry is gone, is not joined by `--again`: it says so in its
/// own sentence, with one instruction.
#[test]
fn a_start_again_with_no_entry_fails_and_joins_nothing() {
    let t = TestHome::new();
    let o = session_start(&t.env, &["--again".into()]);
    assert!(o.stdout.starts_with("BOOTSTRAP-ARM handle=none identity=FAIL"), "{}", o.stdout);
    assert!(o.stdout.ends_with("comm-session-start: this session has no sot-comm identity; run /sot-session-start to join\n"), "{}", o.stdout);
    assert!(!t.registry().exists() || t.agents().as_object().map_or(true, |a| a.is_empty()), "joined: {}", t.agents());

    let h = crate::comm::identity::derive::tier(1, &t.root(), "box1", Default::default()).1;
    assert_eq!(session_start(&t.env, &[]).code, 0);
    std::fs::write(t.registry(), r#"{"agents":{}}"#).unwrap();
    let o = session_start(&t.env, &["--again".into()]);
    assert_eq!(
        o.stdout,
        format!(
            "BOOTSTRAP-ARM handle=none identity=FAIL WAKE: n/a\ncomm-session-start: @{h} has no registry entry for this project; run /sot-session-start to join\n"
        )
    );
    assert!(t.agents().as_object().map_or(true, |a| a.is_empty()), "joined: {}", t.agents());
}

/// A send and the again form refuse the same four identities, each in its own words (`cli::unroutable`).
#[test]
fn a_send_and_a_start_again_refuse_the_same_four_causes() {
    let t = TestHome::new();
    let h = crate::comm::identity::derive::tier(1, &t.root(), "box1", Default::default()).1;
    let fail = "BOOTSTRAP-ARM handle=none identity=FAIL WAKE: n/a\ncomm-session-start: ";
    let join = "; run /sot-session-start to join\n";
    let again = || session_start(&t.env, &["--again".into()]).stdout;
    // (the handle a send is given, the registry after the join, the line the start says)
    assert!(super::super::routable(&t.env, "", &t.root(), &t.registry()).is_err());
    assert_eq!(again(), format!("{fail}this session has no sot-comm identity{join}"));

    assert_eq!(session_start(&t.env, &[]).code, 0);
    assert!(super::super::routable(&t.env, &h, &t.root(), &t.registry()).is_ok());
    assert!(again().starts_with(&format!("BOOTSTRAP-ARM handle={h} identity=ok")));

    let entry = t.agents()[&h].clone();
    std::fs::write(t.registry(), r#"{"agents":{}}"#).unwrap();
    assert!(super::super::routable(&t.env, &h, &t.root(), &t.registry()).is_err());
    assert_eq!(again(), format!("{fail}@{h} has no registry entry for this project{join}"));

    std::fs::write(t.registry(), "not json").unwrap();
    assert!(super::super::routable(&t.env, &h, &t.root(), &t.registry()).unwrap_err().contains(&format!("so identity @{h} is unverified")));
    assert_eq!(again(), format!("{fail}the registry could not be read, so @{h} is unverified; the next start reads it again\n"));

    // A registry the reader refuses for another reason than "no file" is unreadable too: no agents object, a folder.
    std::fs::write(t.registry(), "{}").unwrap();
    assert_eq!(again(), format!("{fail}the registry could not be read, so @{h} is unverified; the next start reads it again\n"));
    std::fs::remove_file(t.registry()).unwrap();
    std::fs::create_dir(t.registry()).unwrap();
    assert_eq!(again(), format!("{fail}the registry could not be read, so @{h} is unverified; the next start reads it again\n"));
    std::fs::remove_dir(t.registry()).unwrap();

    assert!(super::super::routable(&t.env, &h, &t.root(), &t.registry()).is_err());
    assert_eq!(again(), format!("{fail}the registry is missing, so @{h} is unverified{join}"));

    let mut other = entry;
    other["root"] = serde_json::json!("/another/project");
    std::fs::write(t.registry(), "{\"agents\":{}}").unwrap();
    t.put(&h, other);
    assert!(super::super::routable(&t.env, &h, &t.root(), &t.registry()).is_err());
    assert_eq!(again(), format!("{fail}@{h} is registered to a different project{join}"));
}
