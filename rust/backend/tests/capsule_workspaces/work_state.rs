//! Rows' work-state and phase in a real daemon: a row waiting at its agent's permission prompt reads as waiting on its
//! person, and a refreshed row loaded at boot follows its new supervisor.

use super::*;

const RULE_CHARS: usize = 60;

/// A stand-in `claude` that draws, from a cleared screen, a free prompt box, or, while `dialog` exists, a permission
/// dialog with Claude Code's footer (rows' dialog tick reads it as `dialog_of` does).
fn write_dialog_claude(dir: &Path, dialog: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).expect("mkdir stub bin");
    let script = format!(
        "#!/bin/sh\n\
         rule=''; i=0; while [ $i -lt {RULE_CHARS} ]; do rule=\"$rule$(printf '\\342\\224\\200')\"; i=$((i+1)); done\n\
         while :; do\n\
           printf '\\033[2J\\033[H'\n\
           if [ -f '{dialog}' ]; then\n\
             printf '\\342\\227\\217 Bash(rm -rf build)\\n%s\\n Bash command\\n\\n   rm -rf build\\n\\n Do you want to proceed?\\n \\342\\235\\257 1. Yes\\n   2. No\\n\\n Esc to cancel \\302\\267 Tab to amend\\n' \"$rule\"\n\
             while [ -f '{dialog}' ]; do sleep 0.1; done\n\
           else\n\
             printf '%s\\n\\342\\235\\257\\302\\240\\n%s\\n' \"$rule\" \"$rule\"\n\
             while [ ! -f '{dialog}' ]; do sleep 0.1; done\n\
           fi\n\
         done\n",
        dialog = dialog.display(),
    );
    sot_platform::test_exec::write_executable(&dir.join("claude"), script);
    dir.to_path_buf()
}

/// The row's `(agent_state, agent_summary)` from `workspace.list`, polled until `want` holds or `within` passes.
async fn poll_work_state(
    conn: &mut Conn,
    next_id: &mut u64,
    workspace_id: &str,
    within: Duration,
    want: impl Fn(&str, &str) -> bool,
) -> (String, String) {
    let deadline = Instant::now() + within;
    loop {
        let id = *next_id;
        *next_id += 1;
        let payload = call(conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        let row = find_row(&payload, workspace_id).expect("the row is listed");
        let got = (row["agent_state"].as_str().unwrap_or("").to_string(), row["agent_summary"].as_str().unwrap_or("").to_string());
        if want(&got.0, &got.1) || Instant::now() >= deadline {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// A Claude row whose screen shows a permission prompt reads `blocked` with the prompt's question, while its turn runs
/// and at rest, within the dialog tick (5 s plus the sweep); once the prompt closes it reads what its other facts say.
/// Rows' dialog tick writes the fact in the daemon's own process, through work-state's one write.
#[tokio::test]
async fn a_row_at_its_agents_permission_prompt_reads_waiting_on_its_person() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd — build it first");
    let env = Env::new("cwsd");
    let dialog = env._tmp.path().join("dialog");
    let stub = write_dialog_claude(&env._tmp.path().join("stubbin"), &dialog);
    env.spawn_sotd_with_path_and_env(&stub, &[]);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let create = serde_json::json!({
        "label": "cwsd-row",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
        "agent": "claude",
    });
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create).await;
    next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    let ws = res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let join = call(&mut conn, next_id, op::AGENT_JOIN, serde_json::json!({ "workspace_id": ws, "handle": "cwsd-h" })).await;
    next_id += 1;
    assert_eq!(join.payload["ok"], true, "agent.join failed: {:?}", join.payload);
    // The session's own entry, as its join and its prompt hook leave it: a turn running.
    let entry = serde_json::json!({"host": TEST_STATE_HOST, "state": "working", "floor": "user", "summary": "", "status_at": "2026-10-21T00:00:00Z"});
    support::write_registry(&env.comm_root, |doc| *doc = serde_json::json!({"agents": {"cwsd-h": entry}}));
    poll_for_phase(&mut conn, &mut next_id, &ws, "ready", BOUND.max(Duration::from_secs(60))).await;
    let got = poll_work_state(&mut conn, &mut next_id, &ws, Duration::from_secs(10), |s, _| s == "working").await;
    assert_eq!(got.0, "working", "setup: the row reads its turn");

    std::fs::write(&dialog, b"").expect("open the dialog");
    let got = poll_work_state(&mut conn, &mut next_id, &ws, Duration::from_secs(20), |s, _| s == "blocked").await;
    assert_eq!(
        got,
        ("blocked".to_string(), "claude dialog: Do you want to proceed?".to_string()),
        "a row at its permission prompt reads waiting on its person, not green"
    );

    std::fs::remove_file(&dialog).expect("close the dialog");
    let got = poll_work_state(&mut conn, &mut next_id, &ws, Duration::from_secs(20), |s, _| s == "working").await;
    assert_eq!(got.0, "working", "the closed prompt leaves the row's turn showing");

    // The turn ends through the session's one write (`sotd work-state stop`, as its Stop hook runs it): the row rests,
    // blue. A dialog that opens now (a background agent's, after the lead's Stop) still reads red, and the rest returns.
    let stop = sot_platform::test_wait::run_within(
        std::process::Command::new(sotd_program()).args(["work-state", "stop", "--handle", "cwsd-h"]).env("SOT_COMM_HOME", &env.comm_root),
        BOUND,
    );
    assert!(stop.status.success(), "sotd work-state stop: {stop}");
    let got = poll_work_state(&mut conn, &mut next_id, &ws, Duration::from_secs(10), |s, _| s == "done").await;
    assert_eq!(got.0, "done", "setup: the turn ended");
    std::fs::write(&dialog, b"").expect("open a dialog at rest");
    let got = poll_work_state(&mut conn, &mut next_id, &ws, Duration::from_secs(20), |s, _| s == "blocked").await;
    assert_eq!(got.0, "blocked", "a dialog on a row at rest reads waiting on its person");
    std::fs::remove_file(&dialog).expect("close it");
    let got = poll_work_state(&mut conn, &mut next_id, &ws, Duration::from_secs(20), |s, _| s == "done").await;
    assert_eq!(got.0, "done", "the closed dialog leaves the row at rest");
    env.kill_daemon_bounded().await;
}

/// A Claude row whose session never joined (stopped at Claude Code's first-run trust dialog, or before a login) has only
/// the spawn's placeholder entry (`status: spawning`, no workspace id) under the name it was created with. A dialog on
/// its screen still reads waiting on its person within the dialog tick: the person is the only one who can answer it.
#[tokio::test]
async fn a_row_at_a_dialog_before_its_session_joins_reads_waiting_on_its_person() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd — build it first");
    let env = Env::new("cwpj");
    let dialog = env._tmp.path().join("dialog");
    std::fs::write(&dialog, b"").expect("the dialog shows from the start");
    let stub = write_dialog_claude(&env._tmp.path().join("stubbin"), &dialog);
    env.spawn_sotd_with_path_and_env(&stub, &[]);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    // The spawn's placeholder, as `comm-spawn.sh` writes it before the row exists.
    let entry = serde_json::json!({"host": TEST_STATE_HOST, "status": "spawning", "workspace_id": "", "expertise": [],
        "joined": "2026-11-03T19:57:23Z", "last_seen": "2026-11-03T19:57:23Z"});
    support::write_registry(&env.comm_root, |doc| *doc = serde_json::json!({"agents": {"cwpj-h": entry}}));
    let create = serde_json::json!({
        "label": "cwpj-row",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
        "agent": "claude",
        "agent_name": "cwpj-h",
    });
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create).await;
    next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    let ws = res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    poll_for_phase(&mut conn, &mut next_id, &ws, "ready", BOUND.max(Duration::from_secs(60))).await;
    let got = poll_work_state(&mut conn, &mut next_id, &ws, Duration::from_secs(20), |s, _| s == "blocked").await;
    assert_eq!(
        got,
        ("blocked".to_string(), "claude dialog: Do you want to proceed?".to_string()),
        "a row at a dialog before its session joins reads waiting on its person"
    );
    env.kill_daemon_bounded().await;
}

/// A row loaded from its toml at boot is observed by its id; a same-label `workspace.create` refreshes it (a new row
/// under the same id) and starts its supervisor. The refreshed row's phase follows that supervisor: once its authority
/// stops, `workspace.list` stops reading `ready` (queue item 58: before the fix, the observer kept feeding the replaced
/// row and the list read the settle's `ready` until the daemon restarted).
#[tokio::test]
async fn a_refreshed_loaded_row_follows_its_new_supervisor() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd — build it first");
    let env = Env::new("cwrf");
    env.seed_capsule_toml("ws-cwrf-row-1", "cwrf-row", &env.workspace_project_root, "none");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let create = serde_json::json!({
        "label": "cwrf-row",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create).await;
    next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    assert_eq!(res.payload["workspace_id"], "ws-cwrf-row-1", "the refresh keeps the loaded row's id");
    poll_for_phase(&mut conn, &mut next_id, "ws-cwrf-row-1", "ready", BOUND.max(Duration::from_secs(60))).await;
    let state_dir = state_dir_from_list(&mut conn, &mut next_id, "ws-cwrf-row-1").await;

    tokio::task::spawn_blocking(move || sot_capsule::attach_client::supervisor_client::stop(&state_dir).expect("stop the authority"))
        .await
        .expect("the stop");
    let deadline = Instant::now() + Duration::from_secs(20);
    let phase = loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        let phase = find_row(&payload, "ws-cwrf-row-1").and_then(|r| r["phase"].as_str().map(str::to_string)).unwrap_or_default();
        if phase != "ready" || Instant::now() >= deadline {
            break phase;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert_ne!(phase, "ready", "the refreshed row's phase never followed its stopped supervisor");
    env.kill_daemon_bounded().await;
}

/// A stand-in `claude` for the trust dialog. Each start appends its argv to `.stub-starts` and each draw (`main` or
/// `dialog`) to `.stub-draws`, in its working folder, and
/// runs `loss`, a shell snippet that may write `{}` over `$cfg` (`$CLAUDE_CONFIG_DIR/.claude.json`): the measured lost
/// update, the daemon's entry gone before the leg reads it. Then it draws the main input box if that file holds an
/// accepted entry, else Claude Code's folder-trust dialog, and waits (or, when `exit_first`, its first start ends 3 s
/// after it draws, as an agent that crashes does).
fn write_trust_claude(dir: &Path, loss: &str, exit_first: bool) -> PathBuf {
    std::fs::create_dir_all(dir).expect("mkdir stub bin");
    let script = r#"#!/bin/sh
rule=''; i=0; while [ $i -lt RULE_CHARS ]; do rule="$rule$(printf '\342\224\200')"; i=$((i+1)); done
echo "$*" >> .stub-starts
n=$(wc -l < .stub-starts)
cfg="$CLAUDE_CONFIG_DIR/.claude.json"
LOSS
printf '\033[2J\033[H'
if grep -q '"hasTrustDialogAccepted": true' "$cfg" 2>/dev/null; then
  echo main >> .stub-draws
  printf '%s\n\342\235\257\302\240\n%s\n' "$rule" "$rule"
else
  echo dialog >> .stub-draws
  printf '%s\n Accessing workspace:\n\n %s\n\n Quick safety check: Is this a project you created or one you trust?\n\n \342\235\257 No, exit\n   Yes, I trust this folder\n\n Enter to confirm \302\267 Esc to cancel\n' "$rule" "$PWD"
fi
EXIT_FIRST
while :; do sleep 1; done
"#
    .replace("RULE_CHARS", &RULE_CHARS.to_string())
    .replace("LOSS", loss)
    .replace("EXIT_FIRST", if exit_first { "if [ \"$n\" -eq 1 ]; then sleep 3; exit 1; fi" } else { "" });
    sot_platform::test_exec::write_executable(&dir.join("claude"), script);
    dir.to_path_buf()
}

/// The declaration of the test's temporary folder as the trusted prefix, through the real command, under the daemon's
/// own configuration folder.
fn declare_trust_prefix(env: &Env) {
    let prefix = env._tmp.path().canonicalize().expect("prefix");
    let declare = sot_platform::test_wait::run_within(
        std::process::Command::new(sotd_program())
            .args(["trust", "declare"])
            .arg(&prefix)
            .env("HOME", &env.home_root)
            .env("USERPROFILE", &env.home_root)
            .env("XDG_CONFIG_HOME", &env.config_root)
            .env("LOCALAPPDATA", &env.state_root)
            .env("SOT_COMM_HOME", &env.comm_root),
        BOUND,
    );
    assert!(declare.status.success(), "sotd trust declare: {declare}");
}

fn stub_lines(root: &Path, name: &str) -> Vec<String> {
    std::fs::read_to_string(root.join(name)).map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default()
}

fn stub_starts(root: &Path) -> Vec<String> {
    stub_lines(root, ".stub-starts")
}

fn stub_draws(root: &Path) -> Vec<String> {
    stub_lines(root, ".stub-draws")
}

/// A Claude row reauthed to another account, whose replacement leg loses its trust record (the stub's lost update) and
/// stops at the folder-trust dialog, under the declared prefix: the tick restarts it once with the same argv, its
/// `--resume` kept, and that restart's own spawn records trust again, so the third start reaches the main box. Three
/// starts, the last two equal, the target account's file holding the root's accepted entry, and still three later.
/// Red without the restart: two starts and the row at the dialog.
#[tokio::test]
async fn a_reauthed_leg_at_the_trust_dialog_under_the_prefix_restarts_once_with_its_resume_and_is_trusted() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd — build it first");
    let env = Env::new("cwtr");
    let lost = env._tmp.path().join("lost");
    let loss = format!("case \" $* \" in *\" --resume \"*) if [ ! -f '{}' ]; then : > '{}'; echo '{{}}' > \"$cfg\"; fi;; esac", lost.display(), lost.display());
    let stub = write_trust_claude(&env._tmp.path().join("stubbin"), &loss, false);
    let config_dir = env.home_root.join(".claude-auth");
    for account in ["src", "dst"] {
        std::fs::create_dir_all(config_dir.join(account)).expect("mkdir account");
        std::fs::write(config_dir.join(account).join(".credentials.json"), b"{}").expect("credentials");
    }
    let root = env.workspace_project_root.canonicalize().expect("root");
    let uuid = "0f0e0d0c-aaaa-4bbb-8ccc-000000000001";
    let transcripts = config_dir.join("dst").join("projects").join("p");
    std::fs::create_dir_all(&transcripts).expect("mkdir projects");
    std::fs::write(transcripts.join(format!("{uuid}.jsonl")), format!("{}\n", serde_json::json!({"cwd": root}))).expect("transcript");

    declare_trust_prefix(&env);

    env.spawn_sotd_with_path_and_env(&stub, &[]);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let create = serde_json::json!({
        "label": "cwtr-row",
        "project_root": root.to_string_lossy(),
        "runtime": "capsule",
        "agent": "claude",
        "account": "src",
    });
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create).await;
    next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    let ws = res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    poll_for_phase(&mut conn, &mut next_id, &ws, "ready", BOUND.max(Duration::from_secs(60))).await;
    assert_eq!(stub_starts(&root).len(), 1, "setup: one start on the first account");

    let reauth = call(&mut conn, next_id, op::WORKSPACE_REAUTH, serde_json::json!({"workspace_id": ws, "account": "dst", "resume": uuid})).await;
    assert_eq!(reauth.payload["code"], "reauth_accepted", "workspace.reauth: {:?}", reauth.payload);

    let deadline = Instant::now() + Duration::from_secs(60);
    while stub_starts(&root).len() < 3 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let starts = stub_starts(&root);
    assert_eq!(starts.len(), 3, "start, the reauthed leg, and its one restart: {starts:?}");
    assert!(!starts[0].contains("--resume"), "the first start is the recipe: {starts:?}");
    assert!(starts[1].contains(&format!("--resume {uuid}")), "the reauthed leg resumes the conversation: {starts:?}");
    assert_eq!(starts[1], starts[2], "the restart reruns the leg's own argv: {starts:?}");

    let file = config_dir.join("dst").join(".claude.json");
    let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).expect("dst .claude.json")).expect("json");
    assert!(
        doc["projects"][root.to_str().unwrap()]["hasTrustDialogAccepted"] == true,
        "the restart's spawn recorded trust for the root again: {doc}"
    );
    tokio::time::sleep(Duration::from_secs(12)).await;
    assert_eq!(stub_starts(&root).len(), 3, "the row was restarted no more");
    assert_eq!(stub_draws(&root).last().map(String::as_str), Some("main"), "the third leg drew its main box: {:?}", stub_draws(&root));
    env.kill_daemon_bounded().await;
}

/// The watchdog's restart opens a leg too: a Claude row whose agent crashed is restarted by its watchdog, the restarted
/// leg loses its record and stops at the folder-trust dialog, and the trust restart reruns that leg once: three starts,
/// the last with the main box, still three later.
#[tokio::test]
async fn a_watchdog_restarted_leg_at_the_trust_dialog_under_the_prefix_restarts_once() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd — build it first");
    let env = Env::new("cwtw");
    let stub = write_trust_claude(&env._tmp.path().join("stubbin"), "if [ \"$n\" -eq 2 ]; then echo '{}' > \"$cfg\"; fi", true);
    let account = env.home_root.join(".claude-auth").join("src");
    std::fs::create_dir_all(&account).expect("mkdir account");
    std::fs::write(account.join(".credentials.json"), b"{}").expect("credentials");
    let root = env.workspace_project_root.canonicalize().expect("root");
    declare_trust_prefix(&env);

    env.spawn_sotd_with_path_and_env(&stub, &[SHORT_BACKOFF]);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let create = serde_json::json!({
        "label": "cwtw-row",
        "project_root": root.to_string_lossy(),
        "runtime": "capsule",
        "agent": "claude",
        "account": "src",
    });
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create).await;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);

    let deadline = Instant::now() + Duration::from_secs(90);
    while stub_starts(&root).len() < 3 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let starts = stub_starts(&root);
    assert_eq!(starts.len(), 3, "start, the watchdog's restart, and the trust restart: {starts:?}");
    assert!(starts.iter().all(|s| !s.contains("--resume")), "no conversation to resume on this row: {starts:?}");
    tokio::time::sleep(Duration::from_secs(12)).await;
    assert_eq!(stub_starts(&root).len(), 3, "the third start was restarted no more");
    assert_eq!(stub_draws(&root).last().map(String::as_str), Some("main"), "the third leg drew its main box: {:?}", stub_draws(&root));
    env.kill_daemon_bounded().await;
}
