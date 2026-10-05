//! Tests of the row store: load, save and scan cases, the round trips and the config-dir cases.

use super::support_tests::env_guarded;
use super::*;

#[test]
fn load_toml_canonical() {
    let dir = std::env::temp_dir().join(format!(
        "sot-ws-test-canonical-{}-{}",
        std::process::id(),
        now_unix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("alpha.toml");
    std::fs::write(
        &p,
        r#"
workspace_id = "ws-alpha-1"
slug         = "alpha"
label        = "Alpha.jl"
project_root = "/home/u/Alpha.jl"
session_name = "sot-be-alpha"
created      = 1700000000
"#,
    )
    .unwrap();
    let ws = load_toml(&p, false).unwrap().unwrap();
    assert_eq!(ws.workspace_id, "ws-alpha-1");
    assert_eq!(ws.slug, "alpha");
    assert_eq!(ws.label, "Alpha.jl");
    assert_eq!(ws.project_root, PathBuf::from("/home/u/Alpha.jl"));
    assert_eq!(ws.session_name, "sot-be-alpha");
    // ADR 0042 slice L1a: a toml predating the `runtime` key defaults
    // to this platform's ordinary workspace runtime — "tmux",
    // byte-for-byte today's Unix behaviour; "capsule" on Windows
    // (Codex review, PR #175 — see `load_toml`'s own comment: tmux
    // never runs on Windows at all).
    assert_eq!(ws.runtime, "capsule");
    // Accounts brief: a toml predating the `account` key loads as
    // the default account, "".
    assert_eq!(ws.account(), "");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Accounts brief: `save`/`load_toml` round-trip a non-default
/// `account`, and an older toml written before this key existed
/// loads it as "" (the default account) rather than failing --
/// same shape as [`save_load_round_trips_agent_handle`]'s own test.
#[test]
fn save_load_round_trips_account() {
    let _guard = env_guarded();
    let dir = std::env::temp_dir().join(format!(
        "sot-ws-test-roundtrip-account-{}-{}",
        std::process::id(),
        now_unix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    std::env::set_var("LOCALAPPDATA", &dir);
    std::env::remove_var("USERPROFILE");
    std::env::set_var("SOT_SELF_HOST", "roundtrip-test");

    let mut ws = Workspace::meta_only(
        "ws-rt-4".to_string(),
        "rt-account".to_string(),
        "RoundTrip4.jl".to_string(),
        PathBuf::from("/home/u/RoundTrip4.jl"),
        "sot-be-rt-account".to_string(),
        1700000000,
        false,
        "claude".to_string(),
        String::new(),
        String::new(),
    );
    ws.account = Mutex::new("team".to_string());
    let toml_path = save(&ws).unwrap();
    let loaded = load_toml(&toml_path, false).unwrap().unwrap();
    assert_eq!(loaded.account(), "team");

    // An older toml predating the key: strip the line, reload, expect "".
    let text = std::fs::read_to_string(&toml_path).unwrap();
    let stripped: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("account"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&toml_path, stripped).unwrap();
    let reloaded = load_toml(&toml_path, false).unwrap().unwrap();
    assert_eq!(reloaded.account(), "", "an older toml with no key defaults to the default account");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn saving_twice_leaves_exactly_one_account_key() {
    // The writer keeps whatever the previous file had that is not a
    // canonical key, and appends it BELOW the fresh canonical block. So a
    // canonical key missing from that strip list is duplicated on every
    // single save. Saving twice is the smallest thing that can see it.
    let _guard = env_guarded();
    let dir = std::env::temp_dir().join(format!(
        "sot-ws-test-dup-account-{}-{}",
        std::process::id(),
        now_unix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    std::env::set_var("LOCALAPPDATA", &dir);
    std::env::remove_var("USERPROFILE");
    std::env::set_var("SOT_SELF_HOST", "dup-account-test");

    let mut ws = Workspace::meta_only(
        "ws-dup-1".to_string(),
        "dup-account".to_string(),
        "Dup.jl".to_string(),
        PathBuf::from("/home/u/Dup.jl"),
        "sot-be-dup-account".to_string(),
        1700000000,
        false,
        "claude".to_string(),
        String::new(),
        String::new(),
    );
    ws.account = Mutex::new("first".to_string());
    let toml_path = save(&ws).unwrap();
    // The switch a reauth performs: change the account, save again.
    ws.account = Mutex::new("second".to_string());
    let toml_path = save(&ws).unwrap();

    let text = std::fs::read_to_string(&toml_path).unwrap();
    let keys = text
        .lines()
        .filter(|l| l.trim_start().starts_with("account"))
        .count();
    assert_eq!(
        keys, 1,
        "saving twice must leave ONE account key, not append another; file was:\n{text}"
    );
    // The value that survives must be the new one. `parse_kv` takes the
    // LAST key before a section, so a stale copy appended below the
    // canonical block would silently win and revert the switch.
    let loaded = load_toml(&toml_path, false).unwrap().unwrap();
    assert_eq!(
        loaded.account(),
        "second",
        "the account read back must be the one just written"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_toml_canonical_round_trips_capsule_runtime() {
    let dir = std::env::temp_dir().join(format!(
        "sot-ws-test-capsule-{}-{}",
        std::process::id(),
        now_unix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("beta.toml");
    std::fs::write(
        &p,
        r#"
workspace_id = "ws-beta-1"
slug         = "beta"
label        = "Beta.jl"
project_root = "/home/u/Beta.jl"
session_name = "sot-be-beta"
created      = 1700000000
runtime      = "capsule"
"#,
    )
    .unwrap();
    let ws = load_toml(&p, false).unwrap().unwrap();
    assert_eq!(ws.runtime, "capsule");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_toml_reads_the_pre_protocol_2_tmux_session_key_when_session_name_is_absent() {
    // File shim (topology plan step 8): a row written by any release
    // before protocol 2 spelled the key `tmux_session`. It must load
    // under `session_name` with the stored value, never a re-derived
    // one, and a rewrite drops the old spelling. Deletable one
    // release after 0.6.0 final, together with the shim it tests.
    let dir = std::env::temp_dir().join(format!(
        "sot-ws-test-oldkey-{}-{}",
        std::process::id(),
        now_unix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("gamma.toml");
    let text = r#"
workspace_id = "ws-gamma-1"
slug         = "gamma"
label        = "Gamma.jl"
project_root = "/home/u/Gamma.jl"
tmux_session = "sot-be-gamma-kept"
created      = 1700000000
runtime      = "capsule"
"#;
    std::fs::write(&p, text).unwrap();
    let ws = load_toml(&p, false).unwrap().unwrap();
    assert_eq!(ws.session_name, "sot-be-gamma-kept");
    let preserved = strip_canonical_top_and_kernel(text);
    assert!(!preserved.contains("tmux_session"), "old key must not survive a rewrite: {preserved}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Field defect (2026-09-04): a toml written by a pre-fix build can
/// carry the Windows extended-length (verbatim) form `std::fs::
/// canonicalize` returns there (`\\?\C:\...`); `CreateProcess` rejects
/// that as a working directory. `load_toml` must hand back the plain
/// form regardless of what's on disk. Windows-only: `simplify_verbatim`
/// is a no-op on every other platform, so this toml would (correctly)
/// load unchanged there.
///
/// This file's `project_root` line is raw/unescaped — the shape a
/// build that never escaped anything at all would have written. Once
/// `toml_unquote` runs (below `load_toml_canonical`'s escaped-writer
/// sibling `_unescapes_writer_escaped_verbatim_prefix`), its leading
/// `\\` reads as one escaped backslash and the prefix halves to
/// `\?\` — exercising `simplify_verbatim`'s single-backslash branch,
/// not its original double-backslash one.
#[test]
#[cfg(windows)]
fn load_toml_canonical_strips_windows_verbatim_prefix() {
    let dir = std::env::temp_dir().join(format!(
        "sot-ws-test-verbatim-{}-{}",
        std::process::id(),
        now_unix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("gamma.toml");
    std::fs::write(
        &p,
        r#"
workspace_id = "ws-gamma-1"
slug         = "gamma"
label        = "Gamma.jl"
project_root = "\\?\C:\Users\u\.julia\dev\Gamma.jl"
session_name = "sot-be-gamma"
created      = 1700000000
"#,
    )
    .unwrap();
    let ws = load_toml(&p, false).unwrap().unwrap();
    assert_eq!(
        ws.project_root,
        PathBuf::from(r"C:\Users\u\.julia\dev\Gamma.jl")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The realistic on-disk shape after this fix: the real writer
/// (`toml_quote`) escapes every backslash, so a pre-fix-build `save()`
/// of a verbatim root doubles them — `\\?\C:\...` becomes
/// `\\\\?\\C:\\...` in the file. `load_toml` must unescape
/// (`toml_unquote`) before `simplify_verbatim` ever sees it, which
/// restores the true `\\?\` prefix and strips it via
/// `simplify_verbatim`'s original double-backslash branch — the
/// sibling test above covers the OTHER on-disk shape (a raw,
/// never-escaped write, which lands on the new single-backslash
/// branch instead).
#[test]
#[cfg(windows)]
fn load_toml_canonical_unescapes_writer_escaped_verbatim_prefix() {
    let dir = std::env::temp_dir().join(format!(
        "sot-ws-test-verbatim-escaped-{}-{}",
        std::process::id(),
        now_unix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("delta.toml");
    std::fs::write(
        &p,
        r#"
workspace_id = "ws-delta-1"
slug         = "delta"
label        = "Delta.jl"
project_root = "\\\\?\\C:\\Users\\u\\HomeLab\\x"
session_name = "sot-be-delta"
created      = 1700000000
"#,
    )
    .unwrap();
    let ws = load_toml(&p, false).unwrap().unwrap();
    assert_eq!(ws.project_root, PathBuf::from(r"C:\Users\u\HomeLab\x"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_toml_legacy_backend_block() {
    let dir = std::env::temp_dir().join(format!(
        "sot-ws-test-legacy-{}-{}",
        std::process::id(),
        now_unix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("legacy.toml");
    std::fs::write(
        &p,
        r#"
[backend]
session_id   = "sess-old"
label        = "LegacyPkg.jl"
project_dir  = "/home/u/LegacyPkg.jl"
session_name = "sot-be-legacypkg.jl"
started      = 1700000000
pid          = 12345
"#,
    )
    .unwrap();
    let ws = load_toml(&p, true).unwrap().unwrap();
    assert_eq!(ws.workspace_id, "sess-old");
    assert_eq!(ws.slug, "legacypkg_jl");
    assert_eq!(ws.label, "LegacyPkg.jl");
    assert_eq!(ws.project_root, PathBuf::from("/home/u/LegacyPkg.jl"));
    // No `runtime` key -> `meta_only`'s per-OS default, same as the
    // canonical shape (`load_toml_canonical`).
    assert_eq!(ws.runtime, "capsule");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Field defect (2026-09-05): a box carrying both the canonical
/// `workspaces-<host>/<slug>.toml` and a leftover legacy
/// `sessions-<host>/<slug>.toml` had the legacy copy re-registered
/// second, and `insert`'s "same slug -> new metadata wins" reset the
/// canonical row's runtime/agent/autostart/task in memory on every
/// boot. The canonical row must win. Runs on every OS.
#[test]
fn scan_disk_legacy_toml_never_overrides_canonical_row_of_same_slug() {
    let _guard = env_guarded();
    let dir = std::env::temp_dir().join(format!(
        "sot-ws-test-legacy-shadow-{}-{}",
        std::process::id(),
        now_unix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // Distinct subdirs so neither the unix XDG root nor the Windows
    // LOCALAPPDATA root doubles as the other's legacy-candidate dir
    // for the boot-time migrations `scan_disk` runs first.
    std::env::set_var("XDG_CONFIG_HOME", dir.join("xdg"));
    std::env::set_var("LOCALAPPDATA", &dir);
    std::env::remove_var("USERPROFILE");
    std::env::set_var("SOT_SELF_HOST", "legacy-shadow-test");

    let canonical_dir = workspaces_dir();
    std::fs::create_dir_all(&canonical_dir).unwrap();
    std::fs::write(
        canonical_dir.join("local.toml"),
        r#"
workspace_id  = "ws-local-1"
slug          = "local"
label         = "local"
project_root  = "/home/u"
session_name  = "sot-be-local"
created       = 1700000000
autostart_claude = true
agent         = "claude"
agent_name    = "kal-local"
task          = "hello"
runtime       = "capsule"
"#,
    )
    .unwrap();
    let legacy_dir = sessions_dir();
    std::fs::create_dir_all(&legacy_dir).unwrap();
    std::fs::write(
        legacy_dir.join("local.toml"),
        r#"
[backend]
session_id   = "ws-local-1"
label        = "local"
project_dir  = "/home/u"
session_name = "sot-be-local"
started      = 1600000000
"#,
    )
    .unwrap();

    let reg = Workspaces::new();
    let count = scan_disk(&reg, false).unwrap();
    assert_eq!(count, 1, "the shadowed legacy toml is not an insert");
    let ws = reg.resolve(Some("local")).unwrap();
    assert_eq!(ws.runtime, "capsule");
    assert!(ws.autostart_claude);
    assert_eq!(ws.agent(), "claude");
    assert_eq!(ws.agent_name(), "kal-local");
    assert_eq!(ws.task, "hello");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Three tomls under a scratch config and comm home (host `host-4`): `a` and `b` declare `m5-dup`, `c` declares
/// `m5-own`. `registry` is the comm registry's `agents` object, or none for no registry file. Returns the dir.
fn dup_handle_setup(registry: Option<serde_json::Value>) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sot-ws-test-dup-handle-{}-{}", std::process::id(), now_unix()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", dir.join("xdg"));
    std::env::set_var("LOCALAPPDATA", &dir);
    std::env::remove_var("USERPROFILE");
    std::env::set_var("SOT_SELF_HOST", "host-4");
    std::env::set_var("SOT_COMM_HOME", dir.join("comm"));
    std::fs::create_dir_all(dir.join("comm")).unwrap();
    std::fs::create_dir_all(workspaces_dir()).unwrap();
    for (slug, handle) in [("a", "m5-dup"), ("b", "m5-dup"), ("c", "m5-own")] {
        std::fs::write(
            workspaces_dir().join(format!("{slug}.toml")),
            format!(
                "workspace_id  = \"ws-{slug}-1\"\nslug          = \"{slug}\"\nlabel         = \"{slug}\"\nproject_root  = \"/home/u/{slug}\"\nsession_name  = \"sot-be-{slug}\"\ncreated       = 1700000000\nruntime       = \"capsule\"\nagent_handle  = \"{handle}\"\n"
            ),
        )
        .unwrap();
    }
    if let Some(agents) = registry {
        std::fs::write(dir.join("comm").join("registry.json"), serde_json::to_vec(&serde_json::json!({ "agents": agents })).unwrap()).unwrap();
    }
    dir
}

fn handles(reg: &Workspaces) -> [String; 3] {
    ["a", "b", "c"].map(|slug| reg.resolve(Some(slug)).unwrap().agent_handle())
}

/// ADR 0049, one row per handle at boot: a crash between a join's two saves, or a moved row's failed save, leaves two
/// tomls on one handle. comm-join.sh writes the registry entry before it declares the handle, so the entry's
/// `workspace_id` names the newer join: the handle stays only there, and the clear is saved.
#[test]
fn scan_disk_keeps_a_handle_two_tomls_declare_only_on_its_last_joiner() {
    let _guard = env_guarded();
    let dir = dup_handle_setup(Some(serde_json::json!({ "m5-dup": { "host": "host-4", "workspace_id": "ws-b-1" } })));
    let reg = Workspaces::new();
    assert_eq!(scan_disk(&reg, false).unwrap(), 3);
    assert_eq!(handles(&reg), ["".to_string(), "m5-dup".to_string(), "m5-own".to_string()]);
    let again = Workspaces::new();
    assert_eq!(scan_disk(&again, false).unwrap(), 3);
    assert_eq!(handles(&again), ["".to_string(), "m5-dup".to_string(), "m5-own".to_string()], "the clear must be saved");
    let _ = std::fs::remove_dir_all(&dir);
}

/// With no entry for the handle, or one stamped by another host, nothing says who joined last: every row loses it.
#[test]
fn scan_disk_clears_a_handle_two_tomls_declare_when_the_registry_names_neither() {
    let _guard = env_guarded();
    let none = ["".to_string(), "".to_string(), "m5-own".to_string()];
    let dir = dup_handle_setup(None);
    let reg = Workspaces::new();
    scan_disk(&reg, false).unwrap();
    assert_eq!(handles(&reg), none, "no registry entry");
    let _ = std::fs::remove_dir_all(&dir);

    let dir = dup_handle_setup(Some(serde_json::json!({ "m5-dup": { "host": "host-9", "workspace_id": "ws-b-1" } })));
    let reg = Workspaces::new();
    scan_disk(&reg, false).unwrap();
    assert_eq!(handles(&reg), none, "an entry from another host");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_toml_legacy_rejected_when_legacy_off() {
    let dir = std::env::temp_dir().join(format!(
        "sot-ws-test-legacy-off-{}-{}",
        std::process::id(),
        now_unix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("only-backend.toml");
    std::fs::write(&p, "[backend]\nlabel = \"x\"\nproject_dir = \"/p\"\n").unwrap();
    let result = load_toml(&p, false).unwrap();
    assert!(result.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Field defect (2026-09-04) root-cause test: the writer (`toml_quote`)
/// escapes every backslash, but the pre-fix reader (`strip_quotes`
/// alone) never undid that, so a `project_root` containing `\` came
/// back with every backslash DOUBLED — a no-op disguise on Windows,
/// which tolerates repeated separators, but not an identity round
/// trip. Runs on every OS: this is a `toml_quote`/`toml_unquote`
/// symmetry bug, independent of `simplify_verbatim` (which is a
/// no-op here — the path below isn't verbatim-prefixed).
#[test]
fn save_load_round_trips_backslash_project_root() {
    let _guard = env_guarded();
    let dir = std::env::temp_dir().join(format!(
        "sot-ws-test-roundtrip-{}-{}",
        std::process::id(),
        now_unix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    std::env::set_var("LOCALAPPDATA", &dir);
    std::env::remove_var("USERPROFILE");
    std::env::set_var("SOT_SELF_HOST", "roundtrip-test");

    let ws = Workspace::meta_only(
        "ws-rt-1".to_string(),
        "rt-backslash".to_string(),
        "RoundTrip.jl".to_string(),
        PathBuf::from(r"C:\Users\u\HomeLab\x"),
        "sot-be-rt-backslash".to_string(),
        1700000000,
        false,
        "none".to_string(),
        String::new(),
        String::new(),
    );
    let toml_path = save(&ws).unwrap();
    let loaded = load_toml(&toml_path, false).unwrap().unwrap();
    assert_eq!(
        loaded.project_root, ws.project_root,
        "project_root must round-trip through save()/load_toml() identically"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Same round trip, covering `toml_quote`'s other escapes — an
/// embedded `"` and a newline — via `agent_name`/`task`, the two
/// free-text fields that share its quoting (see `save()`'s comment
/// above the `agent_name`/`task` lines).
#[test]
fn save_load_round_trips_quotes_and_newlines_in_free_text_fields() {
    let _guard = env_guarded();
    let dir = std::env::temp_dir().join(format!(
        "sot-ws-test-roundtrip-quotes-{}-{}",
        std::process::id(),
        now_unix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    std::env::set_var("LOCALAPPDATA", &dir);
    std::env::remove_var("USERPROFILE");
    std::env::set_var("SOT_SELF_HOST", "roundtrip-test");

    let ws = Workspace::meta_only(
        "ws-rt-2".to_string(),
        "rt-quotes".to_string(),
        "RoundTrip2.jl".to_string(),
        PathBuf::from("/home/u/RoundTrip2.jl"),
        "sot-be-rt-quotes".to_string(),
        1700000000,
        false,
        "claude".to_string(),
        "peer-\"nick\"".to_string(),
        "line one\nline two".to_string(),
    );
    let toml_path = save(&ws).unwrap();
    let loaded = load_toml(&toml_path, false).unwrap().unwrap();
    assert_eq!(loaded.agent_name(), ws.agent_name());
    assert_eq!(loaded.task, ws.task);
    let _ = std::fs::remove_dir_all(&dir);
}

/// ADR 0046 decision 1: `agent_handle` round-trips through `save()`/
/// `load_toml()` like every other canonical field, and an older toml
/// written before this key existed loads it as "" (never joined)
/// rather than failing.
#[test]
fn save_load_round_trips_agent_handle() {
    let _guard = env_guarded();
    let dir = std::env::temp_dir().join(format!(
        "sot-ws-test-roundtrip-agent-handle-{}-{}",
        std::process::id(),
        now_unix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    std::env::set_var("LOCALAPPDATA", &dir);
    std::env::remove_var("USERPROFILE");
    std::env::set_var("SOT_SELF_HOST", "roundtrip-test");

    let mut ws = Workspace::meta_only(
        "ws-rt-3".to_string(),
        "rt-agent-handle".to_string(),
        "RoundTrip3.jl".to_string(),
        PathBuf::from("/home/u/RoundTrip3.jl"),
        "sot-be-rt-agent-handle".to_string(),
        1700000000,
        false,
        "claude".to_string(),
        String::new(),
        String::new(),
    );
    ws.agent_handle = Mutex::new("rt-agent-handle-testhost".to_string());
    let toml_path = save(&ws).unwrap();
    let loaded = load_toml(&toml_path, false).unwrap().unwrap();
    assert_eq!(loaded.agent_handle(), "rt-agent-handle-testhost");

    // An older toml predating the key: strip the line, reload, expect "".
    let text = std::fs::read_to_string(&toml_path).unwrap();
    let stripped: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("agent_handle"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&toml_path, stripped).unwrap();
    let reloaded = load_toml(&toml_path, false).unwrap().unwrap();
    assert_eq!(reloaded.agent_handle(), "", "an older toml with no key defaults to never-joined");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(not(windows))]
fn app_config_dir_unix_still_prefers_xdg_config_home() {
    let _guard = env_guarded();
    std::env::set_var("XDG_CONFIG_HOME", "/xdg-config");
    std::env::set_var("HOME", "/home/someone");
    assert_eq!(app_config_dir(), PathBuf::from("/xdg-config/sot"));
}

#[test]
#[cfg(not(windows))]
#[should_panic(expected = "set HOME or XDG_CONFIG_HOME and start it again")]
fn app_config_dir_unix_panics_when_xdg_config_home_and_home_are_both_unset() {
    let _guard = env_guarded();
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("HOME");
    let _ = app_config_dir();
}

#[test]
#[cfg(not(windows))]
fn check_config_dir_errs_without_xdg_config_home_and_home_and_is_ok_with_home() {
    let _guard = env_guarded();
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("HOME");
    assert_eq!(check_config_dir(), Err(CONFIG_DIR_UNDERIVABLE.to_string()));
    std::env::set_var("HOME", "/home/someone");
    assert_eq!(check_config_dir(), Ok(()));
}

#[test]
#[cfg(windows)]
fn app_config_dir_windows_uses_localappdata_config_subdir() {
    let _guard = env_guarded();
    std::env::set_var("LOCALAPPDATA", r"C:\Users\someone\AppData\Local");
    assert_eq!(
        app_config_dir(),
        PathBuf::from(r"C:\Users\someone\AppData\Local\sot\config")
    );
}

#[test]
#[cfg(windows)]
fn app_config_dir_windows_ignores_xdg_config_home() {
    let _guard = env_guarded();
    std::env::set_var("XDG_CONFIG_HOME", r"C:\should\be\ignored");
    std::env::set_var("LOCALAPPDATA", r"C:\Users\someone\AppData\Local");
    assert_eq!(
        app_config_dir(),
        PathBuf::from(r"C:\Users\someone\AppData\Local\sot\config")
    );
}

#[test]
#[cfg(windows)]
fn app_config_dir_windows_falls_back_to_userprofile_when_localappdata_unset() {
    let _guard = env_guarded();
    std::env::remove_var("LOCALAPPDATA");
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::set_var("USERPROFILE", r"C:\Users\someone");
    assert_eq!(
        app_config_dir(),
        PathBuf::from(r"C:\Users\someone\AppData\Local\sot\config")
    );
}

#[test]
#[cfg(windows)]
#[should_panic(expected = "cannot resolve the Windows state root")]
fn app_config_dir_windows_panics_when_localappdata_and_userprofile_are_both_unset() {
    let _guard = env_guarded();
    std::env::remove_var("LOCALAPPDATA");
    std::env::remove_var("USERPROFILE");
    let _ = app_config_dir();
}
