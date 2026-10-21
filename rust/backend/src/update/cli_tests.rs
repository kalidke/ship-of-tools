use super::*;

fn fake_identity(tag: &str) -> ReleaseIdentity {
    ReleaseIdentity {
        repo: "kalidke/ship-of-tools".into(),
        tag: tag.into(),
        version: tag.trim_start_matches('v').into(),
        target: "linux-x86_64".into(),
        asset: format!("sot-{}-linux-x86_64.tar.gz", tag.trim_start_matches('v')),
        asset_sha256: "0".repeat(64),
    }
}

fn argv(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

#[tokio::test]
async fn stage_cli_without_its_folder_or_prefix_exits_2() {
    for bad in ["", "stage", "stage d", "stage --prefix p", "stage d e --prefix p", "stage d --prefix", "other d --prefix p"] {
        assert_eq!(stage_cli(&argv(bad)).await, 2, "{bad:?}");
    }
}

#[test]
fn stage_args_take_the_folder_and_the_prefix_in_either_order() {
    let want = Ok(("d".into(), "p".into()));
    assert_eq!(parse_stage_args(&argv("stage d --prefix p")), want);
    assert_eq!(parse_stage_args(&argv("stage --prefix p d")), want);
}

#[test]
fn stage_paths_are_absolute_before_anything_records_them() {
    let cwd = std::env::current_dir().unwrap();
    let (folder, prefix) = absolute_paths("d".as_ref(), "p/q".as_ref()).unwrap();
    assert_eq!((folder, prefix), (cwd.join("d"), cwd.join("p/q")));
    assert_eq!(absolute_paths(&cwd, &cwd).unwrap(), (cwd.clone(), cwd));
}

#[test]
fn the_armed_line_says_which_way_the_install_moves() {
    let line = |installed: Option<&str>, tag: &str| armed_line(&fake_identity(tag), installed);
    assert_eq!(line(Some("0.6.6-rc9.13"), "v0.6.5"), "armed v0.6.5 for linux-x86_64 (installed v0.6.6-rc9.13: older)");
    assert_eq!(line(Some("0.6.5"), "v0.6.6-rc9.13"), "armed v0.6.6-rc9.13 for linux-x86_64 (installed v0.6.5: newer)");
    assert_eq!(line(Some("0.6.6-rc9.14-dev+aaa"), "v0.6.6-rc9.14-dev+bbb"), "armed v0.6.6-rc9.14-dev+bbb for linux-x86_64 (installed v0.6.6-rc9.14-dev+aaa: same)");
    assert_eq!(line(None, "v0.6.5"), "armed v0.6.5 for linux-x86_64");
}

const T: &str = "linux-x86_64";

/// A prefix holding an `install.json` for `installed` and, when given, a pointer for `tag` that records `over`.
fn prefix(installed: (&str, &str), pointer: Option<(&str, Option<(&str, &str)>)>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("updates")).unwrap();
    std::fs::write(
        dir.path().join("install.json"),
        format!(r#"{{"schema":1,"prefix":"/p","version":"{}","commit":"{}"}}"#, installed.0, installed.1),
    )
    .unwrap();
    if let Some((version, over)) = pointer {
        let over = over.map_or(String::new(), |(v, c)| format!(r#","armed_over_version":"{v}","armed_over_commit":"{c}""#));
        std::fs::write(
            dir.path().join("updates").join(format!("pending-{T}.json")),
            format!(
                r#"{{"schema":1,"repo":"kalidke/ship-of-tools","tag":"v{version}","version":"{version}","target":"{T}",
                    "asset":"sot-{version}-{T}.tar.gz","asset_sha256":"{}","checkout":"/x","commit":"cc","armed_at":1{over}}}"#,
                "ab".repeat(32)
            ),
        )
        .unwrap();
        prepare(&dir, version, Some(true));
    }
    dir
}

/// The stage's `prepared.json` for `version`: Julia environments instantiated or not, or no record at all.
fn prepare(dir: &tempfile::TempDir, version: &str, julia: Option<bool>) {
    let stage = dir.path().join("updates").join(format!("v{version}-{T}"));
    let path = stage.join("prepared.json");
    let Some(julia) = julia else {
        let _ = std::fs::remove_file(path);
        return;
    };
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(
        path,
        format!(
            r#"{{"schema":1,"repo":"kalidke/ship-of-tools","tag":"v{version}","version":"{version}","target":"{T}",
                "asset":"sot-{version}-{T}.tar.gz","asset_sha256":"{}","checkout":"/x","commit":"cc",
                "julia_instantiated":{julia},"mathjax_deps":false,"prepared_at":1}}"#,
            "ab".repeat(32)
        ),
    )
    .unwrap();
}

async fn code(dir: &tempfile::TempDir) -> i32 {
    pointer_cli(&["--prefix".into(), dir.path().display().to_string(), "--target".into(), T.into()]).await
}

#[test]
fn arguments_are_the_prefix_and_the_target_in_either_order() {
    let want = Ok(("p".into(), "t".into()));
    assert_eq!(parse_pointer_args(&argv("--prefix p --target t")), want);
    assert_eq!(parse_pointer_args(&argv("--target t --prefix p")), want);
    for bad in ["", "--prefix p", "--target t", "--prefix p --target", "--prefix p --target t x", "stage"] {
        assert!(parse_pointer_args(&argv(bad)).is_err(), "{bad:?}");
    }
}

#[tokio::test]
async fn bad_arguments_exit_2_and_a_refusal_is_neither_1_nor_2() {
    assert_eq!(pointer_cli(&argv("--prefix p")).await, 2);
    assert!(REFUSED != 1 && REFUSED != 2);
}

#[tokio::test]
async fn a_pointer_armed_over_the_install_on_disk_may_apply() {
    let dir = prefix(("0.6.6-rc9.13", "c0"), Some(("0.6.6-rc9.14", Some(("0.6.6-rc9.13", "c0")))));
    assert_eq!(code(&dir).await, 0);
}

#[tokio::test]
async fn a_pointer_armed_over_another_install_is_refused() {
    let dir = prefix(("0.6.6-rc9.14-dev+d94ed0fac", "c1"), Some(("0.6.6-rc9.13", Some(("0.6.6-rc9.12", "c0")))));
    assert_eq!(code(&dir).await, REFUSED);
}

// The stale pointer: armed by a daemon before the record existed, below the install it would land on.
#[tokio::test]
async fn a_pointer_with_no_record_applies_only_above_the_install() {
    let below = prefix(("0.6.6-rc9.14-dev+d94ed0fac", "c1"), Some(("0.6.6-rc9.13", None)));
    assert_eq!(code(&below).await, REFUSED);
    let at = prefix(("0.6.6-rc9.13", "c0"), Some(("0.6.6-rc9.13", None)));
    assert_eq!(code(&at).await, REFUSED);
    let above = prefix(("0.6.6-rc9.13", "c0"), Some(("0.6.6-rc9.14", None)));
    assert_eq!(code(&above).await, 0);
}

// The applier checks the fit its armer did not: a backend role over an env-less prepare, or over none, is refused, and a
// role without a backend is refused only for no prepare. The scripts drop any pointer this refuses.
#[tokio::test]
async fn a_pointer_whose_prepare_does_not_fit_this_hosts_role_is_refused() {
    let dir = prefix(("0.6.6-rc9.13", "c0"), Some(("0.6.6-rc9.14", Some(("0.6.6-rc9.13", "c0")))));
    let ask = |wants: bool| refusal_for(dir.path(), T, move || wants);
    assert!(ask(true).await.is_none() && ask(false).await.is_none(), "a full prepare fits both roles");
    prepare(&dir, "0.6.6-rc9.14", Some(false));
    assert!(ask(true).await.unwrap().contains("did not instantiate"), "an env-less prepare for a backend role");
    assert!(ask(false).await.is_none(), "an env-less prepare for a role without a backend");
    prepare(&dir, "0.6.6-rc9.14", None);
    for wants in [true, false] {
        assert!(ask(wants).await.unwrap().contains("no record of a prepare"), "no prepared.json, backend {wants}");
    }
}

// The verb a script runs, over the role this host and the install's record give together: the declared entry decides
// for a `daemon` host; a `frontend` entry and a host the topology names with neither key (a guest of the prefix's
// home, whose install is the record's host's) take the record.
#[test]
fn the_verb_reads_the_role_from_the_entry_and_the_installs_record() {
    let _guard = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = (std::env::var_os("SOT_HOSTS"), std::env::var_os("SOT_SELF_HOST"));
    let dir = prefix(("0.6.6-rc9.13", "c0"), Some(("0.6.6-rc9.14", Some(("0.6.6-rc9.13", "c0")))));
    prepare(&dir, "0.6.6-rc9.14", Some(false));
    let hosts = dir.path().join("hosts.toml");
    std::fs::write(&hosts, "hub = \"mw21-hub\"\n[host.mw21-hub]\ndaemon = true\n[host.mw21-laptop]\nfrontend = true\n[host.mw21-guest]\n").unwrap();
    std::env::set_var("SOT_HOSTS", &hosts);
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    // (this host, install.json's daemon bit, exit of the verb on a light prepare)
    let table = [
        ("mw21-hub", true, REFUSED),
        ("mw21-hub", false, REFUSED),
        ("mw21-laptop", true, REFUSED),
        ("mw21-laptop", false, 0),
        ("mw21-guest", true, REFUSED),
        ("mw21-guest", false, 0),
    ];
    let mut got = Vec::new();
    for (me, daemon, _) in table {
        std::env::set_var("SOT_SELF_HOST", me);
        std::fs::write(dir.path().join("install.json"), format!(r#"{{"schema":1,"prefix":"/p","version":"0.6.6-rc9.13","commit":"c0","daemon":{daemon}}}"#)).unwrap();
        got.push(rt.block_on(code(&dir)));
    }
    for (key, value) in [("SOT_HOSTS", saved.0), ("SOT_SELF_HOST", saved.1)] {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
    assert_eq!(got, table.map(|(_, _, exit)| exit), "rows: {:?}", table.map(|(me, daemon, _)| (me, daemon)));
}

#[tokio::test]
async fn nothing_armed_or_no_install_here_leaves_the_script_to_its_own_checks() {
    assert_eq!(code(&prefix(("0.6.6-rc9.13", "c0"), None)).await, 0);
    let dir = prefix(("0.6.6-rc9.13", "c0"), Some(("0.6.6-rc9.13", None)));
    std::fs::remove_file(dir.path().join("install.json")).unwrap();
    assert_eq!(code(&dir).await, 0, "no install.json: not an install of ours");
}

#[tokio::test]
async fn a_pointer_that_cannot_be_read_is_refused_with_the_error() {
    let garbled = prefix(("0.6.6-rc9.13", "c0"), Some(("0.6.6-rc9.14", None)));
    let path = garbled.path().join("updates").join(format!("pending-{T}.json"));
    let why = |text: &[u8]| {
        std::fs::write(&path, text).unwrap();
        refusal(garbled.path(), T)
    };
    let asha = "ab".repeat(32);
    let full = |tag: &str, version: &str| {
        format!(
            r#"{{"schema":1,"repo":"kalidke/ship-of-tools","tag":"{tag}","version":"{version}","target":"{T}","asset":"sot-{version}-{T}.tar.gz","asset_sha256":"{asha}","checkout":"/x","commit":"cc","armed_at":1}}"#
        )
    };
    let cases = [
        ("not json", "{not json".to_string()),
        ("no schema, repo or version", format!(r#"{{"tag":"v0.6.6-rc9.14","target":"{T}"}}"#)),
        ("no version", full("v0.6.6-rc9.14", "0.6.6-rc9.14").replace(r#""version":"0.6.6-rc9.14","#, "")),
        ("tag and version disagree", full("v0.6.6-rc9.14", "0.6.6-rc9.15")),
    ];
    for (name, text) in cases {
        let why = why(text.as_bytes()).await.unwrap_or_else(|| panic!("{name}: not refused"));
        assert!(why.starts_with("unreadable pointer: "), "{name}: {why}");
    }
    assert_eq!(code(&garbled).await, REFUSED);
    // The same pointer, whole and above the install, is read and fits.
    assert!(why(full("v0.6.6-rc9.14", "0.6.6-rc9.14").as_bytes()).await.is_none());
}

// The review's measurement: a no-record release pointer above a dev stamp used to be answered `ok`.
#[tokio::test]
async fn a_pointer_with_no_record_never_applies_onto_a_dev_build() {
    let dir = prefix(("0.6.6-rc9.14-dev+d94ed0fac", "c1"), Some(("0.6.6-rc9.14", None)));
    assert_eq!(code(&dir).await, REFUSED);
    let operator = prefix(("0.6.6-rc9.14-dev+d94ed0fac", "c1"), Some(("0.6.6-rc9.14", Some(("0.6.6-rc9.14-dev+d94ed0fac", "c1")))));
    assert_eq!(code(&operator).await, 0, "an operator's pointer carries its record");
}

// The review's two `update pointer` runs: a pointer armed over a record cannot be checked against an install.json that
// cannot be read, and one with no record meets the script's own exact-tag check.
#[tokio::test]
async fn an_unreadable_install_refuses_a_pointer_armed_over_a_record() {
    let cut = r#"{"schema":1,"prefix":"/p","version":"0.6.7","pinned":tr"#;
    let armed = prefix(("0.6.7", "c0"), Some(("0.6.6-rc9.14", Some(("0.6.5", "c0")))));
    std::fs::write(armed.path().join("install.json"), cut).unwrap();
    assert_eq!(code(&armed).await, REFUSED);
    let why = refusal(armed.path(), T).await.unwrap();
    assert!(why.contains("0.6.5") && why.contains(sot_updater::UNREADABLE_LEAD), "{why}");
    let bare = prefix(("0.6.7", "c0"), Some(("0.6.6-rc9.14", None)));
    std::fs::write(bare.path().join("install.json"), cut).unwrap();
    assert_eq!(code(&bare).await, 0, "no record: the script's exact-tag check judges it");
}

// The installer's repair: `sotd update stage` over an install.json it cannot read goes on as over no record, so
// `install.ps1 -Version` can replace the record. It gets as far as the pack folder, which here holds nothing.
#[tokio::test]
async fn the_operators_stage_goes_on_over_an_install_json_it_cannot_read() {
    let (prefix, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let record = prefix.path().join("install.json");
    let stage = || stage_folder(folder.path(), prefix.path());
    let absent = stage().await.unwrap_err();
    assert!(absent.contains("install.json"), "no record is still an error: {absent}");
    std::fs::write(&record, r#"{"schema":1,"prefix":"/p","version":"0.6.7","pinned":tr"#).unwrap();
    let cut = stage().await.unwrap_err();
    assert!(cut.contains("SHA256SUMS") && !cut.contains("install.json"), "went on past the record: {cut}");
}

#[test]
fn direction_arguments_are_the_version_and_the_prefix_in_either_order() {
    let want = Ok(("v0.6.6".to_string(), "p".into(), None));
    assert_eq!(parse_direction_args(&argv("v0.6.6 --prefix p")), want);
    assert_eq!(parse_direction_args(&argv("--prefix p v0.6.6")), want);
    assert_eq!(parse_direction_args(&argv("--installed 0.6.5 v0.6.6 --prefix p")), Ok(("v0.6.6".to_string(), "p".into(), Some("0.6.5".to_string()))));
    for bad in ["", "v0.6.6", "--prefix p", "v0.6.6 --prefix", "a b --prefix p", "v0.6.6 --prefix p --installed"] {
        assert!(parse_direction_args(&argv(bad)).is_err(), "{bad:?}");
        assert_eq!(direction_cli(&argv(bad)), 2, "{bad:?}");
    }
}

#[test]
fn update_direction_names_the_way_the_install_moves() {
    // installed, target, the word. Dev rows included: a main build sits above the candidate it follows.
    let table = [
        ("0.6.6-rc9.14-dev+a", "0.6.6-rc9.13", "older"),
        ("0.6.6-rc9.14-dev+a", "0.6.6-rc9.14", "newer"),
        ("0.6.6-rc9.14-dev+a", "v0.6.6-rc9.14-dev+a", "same"),
        ("0.6.6-rc9.13", "v0.6.6-rc9.13", "same"),
        ("0.6.6-rc9.13", "0.6.5", "older"),
        ("0.6.5", "v0.6.6-rc9.13", "newer"),
        ("0.6.6-rc9.13", "v0.6.6", "newer"),
    ];
    for (installed, target, want) in table {
        assert_eq!(direction_word(target, installed), want, "{installed} -> {target}");
        let dir = prefix((installed, "c0"), None);
        let got = direction_cli(&["--prefix".into(), dir.path().display().to_string(), target.into()]);
        assert_eq!(got, 0, "{installed} -> {target}");
    }
}

#[test]
fn direction_without_a_record_or_a_version_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let ask = |d: &std::path::Path| direction_cli(&["--prefix".into(), d.display().to_string(), "v0.6.6".into()]);
    assert_eq!(ask(dir.path()), 1, "no install.json");
    std::fs::write(dir.path().join("install.json"), r#"{"schema":1,"prefix":"/p"}"#).unwrap();
    assert_eq!(ask(dir.path()), 1, "no version recorded");
}

// An install.json that cannot be parsed still answers where its text holds the version (as the installers read it),
// and says `unknown`, exit 0, where it holds none: the implicit run goes on and rewrites the record.
#[test]
fn direction_over_an_unreadable_record_reads_the_version_text_or_says_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("install.json");
    let ask = |text: &str| {
        std::fs::write(&record, text).unwrap();
        direction_answer("v0.6.6", &record, None)
    };
    let cut = |version: &str| format!("{{\n  \"schema\": 1,\n  \"version\": \"{version}\",\n  \"pinned\": tr");
    assert_eq!(ask(&cut("0.6.5")), Ok("newer".to_string()));
    assert_eq!(ask(&cut("0.6.6")), Ok("same".to_string()));
    assert_eq!(ask(&cut("0.6.7")), Ok("older".to_string()), "an implicit run still never moves back");
    assert_eq!(ask(&format!("\u{feff}{}", cut("0.6.7"))), Ok("older".to_string()), "a BOM does not hide it");
    assert_eq!(ask("{not json"), Ok("unknown".to_string()));
    // The binary's version fills in only where the record names none: a record cut inside its version, or whole
    // without one, no longer hides the installed version from the guard.
    let with_binary = |text: &str, installed: &str| {
        std::fs::write(&record, text).unwrap();
        direction_answer("v0.6.6", &record, Some(installed))
    };
    assert_eq!(with_binary(&format!("{{\n  \"schema\": 1,\n  \"version\": \"0.6."), "0.6.7"), Ok("older".to_string()));
    assert_eq!(with_binary("{not json", "0.6.5"), Ok("newer".to_string()));
    assert_eq!(with_binary(r#"{"schema":1,"prefix":"/p"}"#, "0.6.7"), Ok("older".to_string()), "a whole record with no version");
    assert_eq!(with_binary(&cut("0.6.5"), "0.6.7"), Ok("newer".to_string()), "the record's own version wins");
    std::fs::remove_file(&record).unwrap();
    assert_eq!(direction_answer("v0.6.6", &record, Some("0.6.7")), Ok("older".to_string()), "a missing record, the binary's version");
    assert!(direction_answer("v0.6.6", &record, None).is_err(), "a missing record and no binary version: no answer");
    assert_eq!(ask(r#"{"schema":1,"version":"0.6"#), Ok("unknown".to_string()), "a cut value is no version");
    assert_eq!(direction_cli(&["--prefix".into(), dir.path().display().to_string(), "v0.6.6".into()]), 0);
}

#[test]
fn a_live_repl_is_one_the_daemon_says_is_ready() {
    let reply = serde_json::json!({ "pluto_server_up": false, "workspaces": [
        { "workspace_id": "ws-a", "slug": "rig", "label": "Rig PC", "repl_state": "ready" },
        { "workspace_id": "ws-b", "slug": "idle", "label": "Idle", "repl_state": "not_started" },
        { "workspace_id": "ws-c", "slug": "boot", "label": "Boot", "repl_state": "starting" },
        { "workspace_id": "ws-d", "slug": "gone", "label": "Gone", "repl_state": "dead" },
        { "workspace_id": "ws-e", "slug": "scan", "label": "", "repl_state": "ready" },
        { "workspace_id": "ws-f", "slug": "old" },
    ]});
    let work = live_work(&reply);
    assert_eq!(work.lines, ["repl Rig PC", "repl scan"]);
    assert_eq!((work.unsure.is_none(), repls_exit(&work)), (true, REPLS_LIVE));
    let none = live_work(&serde_json::json!({ "workspaces": [], "pluto_server_up": false }));
    assert_eq!((none.lines.len(), none.unsure.is_none(), repls_exit(&none)), (0, true, 0), "a server not started adds no Pluto line");
}

#[test]
fn a_pluto_server_that_is_up_is_live_work_with_no_notebook_opened_through_the_daemon() {
    // Pluto's own home page opens notebooks the daemon never sees, so the server being up is the fact.
    let up = live_work(&serde_json::json!({ "workspaces": [], "pluto_server_up": true }));
    assert_eq!((up.lines.clone(), up.unsure.is_none(), repls_exit(&up)), (vec!["pluto server".to_string()], true, REPLS_LIVE));
    let with_repl = live_work(&serde_json::json!({
        "workspaces": [{ "slug": "rig", "label": "Rig PC", "repl_state": "ready" }],
        "pluto_server_up": true,
    }));
    assert_eq!(with_repl.lines, ["repl Rig PC", "pluto server"]);
}

#[test]
fn a_daemon_that_cannot_say_about_pluto_has_not_answered_but_its_repls_are_kept() {
    // A daemon older than the field lists rows only: it may hold a Pluto server it cannot report.
    let old = live_work(&serde_json::json!({ "workspaces": [{ "slug": "rig", "label": "Rig PC", "repl_state": "ready" }] }));
    assert_eq!(old.lines, ["repl Rig PC"], "the rows it did list are named");
    assert_eq!(
        old.unsure.as_deref(),
        Some("the daemon's reply says nothing of its Pluto server (a daemon older than this build cannot say whether it is running)")
    );
    assert_eq!(repls_exit(&old), 1, "and it exits 1: not an answer");
    let silent = live_work(&serde_json::json!({ "workspaces": [] }));
    assert_eq!((silent.lines.len(), repls_exit(&silent)), (0, 1));
}

#[test]
fn a_reply_with_no_rows_is_not_an_answer() {
    // An error payload is a reply too; reading it as "no live REPL" would restart the daemon unasked.
    let refused = live_work(&serde_json::json!({ "error": "unknown op" }));
    assert_eq!(refused.unsure.as_deref(), Some("the daemon answered without a workspace list: unknown op"));
    assert_eq!((refused.lines.len(), repls_exit(&refused)), (0, 1));
    assert_eq!(repls_exit(&live_work(&serde_json::json!({}))), 1);
    assert_eq!(repls_exit(&live_work(&serde_json::json!({ "workspaces": "none", "pluto_server_up": false }))), 1);
    assert!(live_work(&serde_json::json!({ "workspaces": [], "pluto_server_up": "yes" })).unsure.is_some(), "a flag that is no bool is no answer");
}

#[test]
fn repls_takes_one_endpoint_and_a_daemon_that_does_not_answer_is_exit_1() {
    assert_eq!(parse_repls_args(&argv("--endpoint unix:/x")), Ok("unix:/x".to_string()));
    for bad in ["", "--endpoint", "--endpoint /x", "unix:/x", "--endpoint unix:/x y"] {
        assert!(parse_repls_args(&argv(bad)).is_err(), "{bad:?}");
        assert_eq!(repls_cli(&argv(bad)), 2, "{bad:?}");
    }
    let dir = tempfile::tempdir().unwrap();
    let none = format!("--endpoint unix:{}/absent.sock", dir.path().display());
    assert_eq!(repls_cli(&argv(&none)), 1);
}
