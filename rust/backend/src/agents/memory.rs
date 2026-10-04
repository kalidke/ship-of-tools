// memory.rs — the auto-memory settings flag: names one shared memory store only when it is proven.

use std::path::{Path, PathBuf};

/// Whether the settings file GOVERNING this child already names an
/// auto-memory directory. An unreadable or unparsable file counts as NOT
/// naming one: a choice that cannot be read is not a choice this code is
/// overriding.
///
/// Takes the config dir rather than `home`, because the file that governs is
/// the CHILD's and not the default account's. `settings.json` is itself a
/// `SHARED_ENTRIES` name, so in the ordinary layout every account's copy is
/// a symlink to one file and the distinction is invisible. It stops being
/// invisible in exactly the case the never-replace rule permits: an account,
/// or a `CLAUDE_CONFIG_DIR` outside the account set, holding a real
/// `settings.json` of its own. Reading the default's there would override
/// that child's deliberate `autoMemoryDirectory` while believing it was
/// respecting one — the precise harm this veto exists to prevent.
fn settings_name_a_memory_dir(config_dir: &Path) -> bool {
    let path = config_dir.join("settings.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| {
            v.get("autoMemoryDirectory")
                .map(|d| !d.is_null())
        })
        .unwrap_or(false)
}

/// The ONE directory every claude account on this box reaches its
/// transcripts and memories through, or `None` when that cannot be proven.
///
/// The flag names a single literal path, so it is only ever correct if
/// every account really does land in one store. That is the COMMON CASE,
/// not an invariant: [`crate::agents::accounts::ensure_account_links`] leaves an
/// account's own `projects` alone when one is already there in any form
/// ("it deliberately overrides the shared one, and is NEVER replaced"),
/// and it links nothing at all while the default folder has no `projects`
/// yet. So an account can hold a REAL `projects` directory of its own --
/// and handing that child the default's path would silently write its
/// memories into another account's store and leave it unable to read its
/// own. That is worse than the prompt this whole change exists to remove.
///
/// Hence: prove it, or name nothing. When the child's own config dir is
/// KNOWN this checks that one dir and nothing else, which is stronger than
/// the survey rather than a shortcut through it -- another account's rival
/// store cannot affect where this child writes. Only when it is unknown does
/// the survey run, and then any account whose `projects` is a real
/// directory, or a link pointing somewhere other than the shared store,
/// makes this return `None` for EVERY child on the box: the permission
/// prompt, today's behaviour, never a wrong path.
///
/// Every decline carries its reason as the `Err` value, which
/// [`auto_memory_settings`] logs in the one place it logs. A silent decline
/// looks exactly like the defect coming back, and the reason is not
/// reconstructable from the outside.
fn shared_projects_root(
    home: &Path,
    child_config_dir: Option<&Path>,
) -> Result<PathBuf, &'static str> {
    let spelled = crate::agents::accounts::claude_config_dir(home, "").join("projects");
    let Ok(kind) = std::fs::symlink_metadata(&spelled) else {
        return Err("there is no projects entry to resolve");
    };
    // Canonical form for COMPARING; the emitted path stays the spelled one
    // unless the default itself is a link, so a box with no indirection is
    // handed the plain path it already derives rather than an
    // extended-length Windows spelling it would not recognise.
    let Ok(canonical) = std::fs::canonicalize(&spelled) else {
        return Err("the projects entry does not resolve");
    };
    // ONE rule, stated once: any path that leaves here having been through
    // `canonicalize` goes through `simplify_verbatim` first. The emitted value
    // reaches Claude Code as a settings path, and on Windows a raw
    // `canonicalize` result is the `\\?\` extended-length form -- which is the
    // very thing the spelled branch below exists to avoid, and which this
    // branch was handing over whenever the default's `projects` was itself a
    // link. Review caught it as the emit-side twin of the cwd-check blocker.
    let emit = if kind.file_type().is_symlink() {
        crate::paths::simplify_verbatim(canonical.clone())
    } else {
        spelled.clone()
    };
    // When the child's OWN config dir is known, check THAT rather than a
    // proxy. `agent-exec` execs in place, so its child inherits the
    // invoking shell's `CLAUDE_CONFIG_DIR`, which need not be one of the
    // daemon-assigned account dirs at all -- an operator export, or a
    // nested capsule env, lands outside the set the loop below walks, and
    // naming the shared store for it would misroute its memory exactly
    // the way an overriding account's would. The daemon's own env carries
    // no `CLAUDE_CONFIG_DIR`, so its spawn legs pass `None` and fall
    // through to the loop unchanged.
    if let Some(dir) = child_config_dir {
        let theirs = dir.join("projects");
        return match std::fs::canonicalize(&theirs) {
            Ok(resolved) if resolved == canonical => Ok(emit),
            _ => Err("the child's own config dir does not reach the shared store"),
        };
    }
    for account in crate::agents::accounts::discover_accounts(home) {
        // The default account IS the shared store -- it is what the others
        // link INTO, and `claude_config_dir` maps it back to the same
        // directory, so treating it as a rival store would make this
        // decline on every box that has one.
        if account.name.is_empty() || account.name == "default" {
            continue;
        }
        let theirs = crate::agents::accounts::claude_config_dir(home, &account.name).join("projects");
        match std::fs::symlink_metadata(&theirs) {
            // Nothing there yet: the next spawn links it into the shared
            // store, so it cannot be a second store.
            Err(_) => continue,
            Ok(md) if md.file_type().is_symlink() => {
                let Ok(resolved) = std::fs::canonicalize(&theirs) else {
                    return Err("an account's projects link dangles");
                };
                if resolved != canonical {
                    return Err("an account links to a store of its own");
                }
            }
            // A real directory of its own -- a second store.
            Ok(_) => return Err("an account keeps its own real projects directory"),
        }
    }
    Ok(emit)
}

/// Claude Code's own project-directory sanitiser, reproduced: every
/// character outside `[A-Za-z0-9]` becomes `-`. Past
/// [`AUTO_MEMORY_NAME_LIMIT`] it truncates and appends a hash of the path
/// that this crate cannot reproduce, so [`auto_memory_settings`] declines
/// rather than name a directory the session would not itself derive.
fn sanitize_project_dir(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Claude Code's own cap on a sanitised project-dir name.
const AUTO_MEMORY_NAME_LIMIT: usize = 200;

/// The `--settings` JSON that keeps a session's auto-memory writes off the
/// permission prompt, or `None` to pass no flag at all.
///
/// The defect: a named account's config dir reaches the shared transcript
/// store through a symlink (`projects` is a `SHARED_ENTRIES` name), so the
/// memory path a session SPELLS (`<account dir>/projects/<name>/memory`)
/// lands somewhere else. Since Claude Code 2.1.282 a write is judged by
/// where it LANDS, and neither auto mode nor an allow rule approves one
/// landing through a symlink outside the working directories — so every
/// auto-memory write stops for a human, on every account session.
///
/// The fix names the landing path instead of the spelled one. `projects`
/// is on the shared allowlist, so in the ordinary layout each account's
/// copy links back into the default config dir's one real directory, and
/// the flag names that. This is NOT an invariant, and an earlier version
/// of this comment wrongly claimed it was: an account may hold a real
/// `projects` of its own, which the sharing design deliberately permits.
/// [`shared_projects_root`] is therefore required to PROVE one shared
/// store before any path is named, and declines for every child on the
/// box when it cannot. For a session already running on the default config
/// dir the flag names exactly what it would have derived itself and
/// changes nothing; only an account session's spelling changes.
/// Per-project separation is preserved because the sanitised cwd is part
/// of the path — the reason this cannot be one global setting, which would
/// collapse every project's memory into a single directory.
///
/// Declines (no flag, today's behaviour) rather than guess when: the cwd is
/// not already canonical, where the name would not be the one the session
/// derives for itself; the cwd is not UTF-8 or not ASCII, where this
/// sanitiser and Claude Code's own regex could disagree on how many `-` a
/// character becomes; the governing settings file already names a directory;
/// the sanitised name would exceed the limit, where the real one gains a
/// hash suffix this crate cannot compute; or one shared store cannot be
/// proven. Every decline is logged at debug with its reason, because a silent
/// one is indistinguishable from the defect returning.
///
/// That claim is STRUCTURAL rather than maintained: the reason travels back as
/// the `Err` of [`auto_memory_reason`] and is logged in exactly one place, here.
/// Six `debug!` calls sat next to six conditions before, which is six chances
/// for a message to drift from the condition above it — and review found the
/// class rather than an instance of it.
pub(super) fn auto_memory_settings(home: &Path, cwd: &Path, child_config_dir: Option<&Path>) -> Option<String> {
    match auto_memory_reason(home, cwd, child_config_dir) {
        Ok(json) => Some(json),
        Err(reason) => {
            tracing::debug!(reason, cwd = ?cwd, "auto-memory: no flag");
            None
        }
    }
}

/// The string comparison of the resolved cwd with the spelled one. On Windows
/// `canonicalize` yields `C:\...` while a root off the wire may read `C:/...`;
/// both name the same folder and sanitise to the same project directory, so
/// the separators are mapped to one form on BOTH sides. Nothing else is
/// loosened: a trailing slash or a different drive-letter case still differs.
fn same_cwd_spelling(resolved: &str, cwd: &str, windows: bool) -> bool {
    if windows {
        resolved.replace('\\', "/") == cwd.replace('\\', "/")
    } else {
        resolved == cwd
    }
}

/// [`auto_memory_settings`]'s whole decision, with the reason for a decline
/// instead of a bare `None`. Callers want "flag or no flag", which is why the
/// `Option` is the public shape and this is separate: the reason is a
/// diagnostic, not part of the contract.
fn auto_memory_reason(
    home: &Path,
    cwd: &Path,
    child_config_dir: Option<&Path>,
) -> Result<String, &'static str> {
    // Name only a directory the session would derive for ITSELF. `cwd` is
    // the workspace root as it arrived over the wire, which `rows/ops/create.rs`
    // checks with `.exists()` and nothing more: a trailing slash, a relative
    // spelling, a `.`/`..` component or a symlinked segment all reach here
    // unmodified, and each sanitises to a DIFFERENT flat name than the
    // child's own `getcwd()` produces. The writes would land in a real
    // directory inside the proven store that nothing else ever reads --
    // not another session on the same project, not the transcripts filed
    // beside it. Decline rather than canonicalize: on Windows
    // `canonicalize` yields a `\\?\` spelling, which re-imports the
    // extended-length problem [`shared_projects_root`]'s spelled-emit
    // branch exists to avoid.
    let Some(cwd) = cwd.to_str() else {
        return Err("the cwd is not UTF-8");
    };
    // Compared as STRINGS, deliberately. `Path` equality compares
    // components, so a trailing slash compares EQUAL to the canonical form
    // while `sanitize_project_dir` maps it to a different trailing `-` --
    // the exact misroute this check exists to stop, and invisible to a
    // `PathBuf == Path` test.
    //
    // De-verbatimed first, the same way `paths::path_within_root` normalizes
    // BOTH sides before comparing. On Windows `canonicalize` returns the
    // `\\?\C:\...` extended-length form while a `project_root` off the wire --
    // and the cwd `CreateProcess` hands the child -- is the plain drive path,
    // so comparing the two raw declined on EVERY Windows box, silently, on the
    // one platform where every new workspace is a capsule row. It was inverted
    // too: `canonicalize` is idempotent on verbatim input, so a `\\?\` root
    // PASSED while the plain spelling the child actually receives was refused.
    match std::fs::canonicalize(cwd).map(crate::paths::simplify_verbatim) {
        Ok(resolved) if resolved.to_str().is_some_and(|r| same_cwd_spelling(r, cwd, cfg!(windows))) => {}
        _ => return Err("the cwd is not the canonical spelling the session would derive"),
    }
    if !cwd.is_ascii() {
        return Err("a non-ASCII cwd could sanitise differently");
    }
    // The owner's own choice outranks ours. A `--settings` value lands in
    // `flagSettings`, which beats `userSettings` in the first-non-null chain,
    // so passing ours would SILENTLY override a deliberate
    // `autoMemoryDirectory`. Changing where a person's memory goes is not this
    // launcher's call.
    match child_config_dir {
        // Known: that child's own file is the only one that governs it, which
        // is also Claude Code's own rule -- `userSettings` resolves from
        // `CLAUDE_CONFIG_DIR`.
        Some(dir) => {
            if settings_name_a_memory_dir(dir) {
                return Err("the child's own settings already name a directory");
            }
        }
        // NOT known, which is every daemon spawn leg: the daemon's own env
        // carries no `CLAUDE_CONFIG_DIR`, and `accounts::account_env` sets it
        // on the CHILD. So the governing file cannot be read here, and a wrong
        // guess overrides a deliberate choice. Two conservative reads instead,
        // either of which declines.
        None => {
            if settings_name_a_memory_dir(&crate::agents::accounts::claude_config_dir(home, "")) {
                return Err("the default account's settings already name a directory");
            }
            // Review SHOULD-FIX 2: reading the default's file was NOT enough.
            // Any named account could be the one this child runs as, and
            // `ensure_account_links` deliberately never replaces a
            // `settings.json` already in place -- so an account keeping its own
            // had its `autoMemoryDirectory` silently overridden, the precise
            // harm this veto exists to prevent. Same proof-or-decline
            // discipline [`shared_projects_root`] already applies to
            // `projects`; this veto had simply never been extended to the
            // sibling entry.
            if an_account_keeps_its_own_memory_dir(home) {
                return Err("an account keeps its own settings naming a directory, and this child could be it");
            }
        }
    }
    let projects = shared_projects_root(home, child_config_dir)?;
    let name = sanitize_project_dir(cwd);
    if name.len() > AUTO_MEMORY_NAME_LIMIT {
        return Err("the sanitised name would be truncated and hashed");
    }
    let dir = projects.join(name).join("memory");
    serde_json::to_string(&serde_json::json!({ "autoMemoryDirectory": dir }))
        .map_err(|_| "the settings JSON could not be serialised")
}

/// Whether any NAMED account keeps a `settings.json` of its own that names an
/// auto-memory directory.
///
/// Only asked when the child's config dir is unknown, and only a REAL file can
/// matter: `settings.json` is a `SHARED_ENTRIES` name, so the ordinary layout
/// is one symlink per account to the default's single file, which the caller
/// has already read. A real file of its own is permitted — and permanent,
/// because [`crate::agents::accounts::ensure_account_links`] never replaces an entry
/// that is already there — and means that account's memory directory is its own
/// business, not this launcher's.
fn an_account_keeps_its_own_memory_dir(home: &Path) -> bool {
    crate::agents::accounts::discover_accounts(home)
        .into_iter()
        .any(|account| {
            if account.name.is_empty() || account.name == "default" {
                return false;
            }
            let dir = crate::agents::accounts::claude_config_dir(home, &account.name);
            let keeps_its_own = std::fs::symlink_metadata(dir.join("settings.json"))
                .map(|md| !md.file_type().is_symlink())
                .unwrap_or(false);
            keeps_its_own && settings_name_a_memory_dir(&dir)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::support_tests::platform_spelling;

    #[test]
    fn same_cwd_spelling_maps_separators_only_on_windows() {
        assert!(same_cwd_spelling(r"C:\a\b", "C:/a/b", true));
        assert!(same_cwd_spelling("C:/a/b", r"C:\a\b", true));
        assert!(!same_cwd_spelling(r"C:\a\b", "C:/a/b", false));
        assert!(!same_cwd_spelling(r"C:\a\b", "C:/a/b/", true));
        assert!(!same_cwd_spelling(r"C:\a\b", "c:/a/b", true));
    }

    /// The whole point of the flag: an ACCOUNT session spells its memory
    /// path through a symlink (`projects` is a `SHARED_ENTRIES` name) and
    /// every write then lands outside the allowed dirs, which auto mode
    /// has not approved since Claude Code 2.1.282. The flag must name the
    /// path it LANDS on. Fixture is explicit about `home` rather than
    /// setting `HOME`, so it cannot race the other env-guarding tests here.
    ///
    /// Every fixture below builds its root with [`platform_spelling`], defined
    /// just above: canonical, because the flag names only a path the session
    /// would derive for itself and the macOS leg's temp root sits under a
    /// symlinked `/var`; de-verbatimed, because a bare `canonicalize` on
    /// Windows yields the `\\?\` form no child ever receives, so a fixture
    /// built from it would exercise the one spelling that is never real and
    /// pass for the wrong reason — which is exactly how a Windows-dead check
    /// reached CI green.
    #[test]
    #[cfg(unix)]
    fn auto_memory_settings_names_the_landing_path_not_the_symlinked_spelling() {
        // The tempdir root is canonicalized once, and the cwd is a REAL
        // directory under it: the flag names only a spelling the session
        // would derive for itself, so a fixture path that does not exist —
        // or that `/var` -> `/private/var` rewrites on the macOS leg, which
        // runs every `cfg(unix)` test — would decline for the wrong reason.
        let home = tempfile::tempdir().expect("tempdir");
        let root = platform_spelling(home.path());
        let shared = root.join("shared-projects");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        // `~/.claude/projects` -> `~/shared-projects`, the shape a named
        // account's own `projects` link produces once resolved.
        std::os::unix::fs::symlink(&shared, root.join(".claude").join("projects")).unwrap();
        let proj = root.join("a b").join("proj");
        std::fs::create_dir_all(&proj).unwrap();

        let json = auto_memory_settings(&root, &proj, None)
            .expect("a resolvable projects dir yields a flag");
        let got: serde_json::Value = serde_json::from_str(&json).unwrap();
        let got = got["autoMemoryDirectory"].as_str().unwrap();
        assert!(
            got.starts_with(shared.to_str().unwrap()),
            "the flag must name the resolved directory, never the symlinked spelling: {got}"
        );
        // And the cwd must still separate projects: a global setting with
        // no cwd component would collapse every project into one directory.
        // Asserted as a suffix rather than by re-running the sanitiser, so
        // the expectation is independent of the code under test.
        assert!(
            got.ends_with("-a-b-proj/memory"),
            "lost the per-project component: {got}"
        );
    }

    /// The hole the review found: an account may hold a REAL `projects`
    /// directory, which the sharing design permits and
    /// `ensure_account_links` deliberately never replaces. Handing that
    /// child the default's path would write its memories into another
    /// account's store and leave it unable to read its own, silently. So
    /// one such account makes the flag decline for EVERY child on the box.
    #[test]
    #[cfg(unix)]
    fn auto_memory_settings_declines_when_an_account_keeps_its_own_projects() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = platform_spelling(home.path());
        let store = root.join(".claude").join("projects");
        std::fs::create_dir_all(&store).unwrap();
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        // An account that shares: a link into the default store.
        let shared = root.join(".claude-auth").join("acct-shared");
        std::fs::create_dir_all(&shared).unwrap();
        std::os::unix::fs::symlink(&store, shared.join("projects")).unwrap();
        let json = auto_memory_settings(&root, &proj, None)
            .expect("an account linked into the shared store must not block the flag");
        // Not merely `is_some()`: this is the PRODUCTION layout (the
        // default's `projects` is a real directory, the accounts link into
        // it), and the path it names is the whole point of the change.
        assert!(
            json.contains(store.to_str().unwrap()),
            "must name the default's own store: {json}"
        );

        // Now one that overrides with a real directory of its own.
        let own = root.join(".claude-auth").join("acct-own");
        std::fs::create_dir_all(own.join("projects")).unwrap();
        assert_eq!(
            auto_memory_settings(&root, &proj, None),
            None,
            "a second store on the box must make this name no path at all"
        );
    }

    /// `agent-exec` EXECS in place, so its child inherits the invoking
    /// shell's `CLAUDE_CONFIG_DIR` -- which need not be a daemon-assigned
    /// account dir at all. Walking `.claude-auth` would come back clean
    /// for it and the flag would name the shared store while that
    /// session's own store is elsewhere. When the child's config dir is
    /// known it is checked directly, and a dir that does not reach the
    /// shared store names no path.
    #[test]
    #[cfg(unix)]
    fn auto_memory_settings_checks_the_childs_own_config_dir_when_known() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = platform_spelling(home.path());
        std::fs::create_dir_all(root.join(".claude").join("projects")).unwrap();
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();

        // An operator export pointing at a store of its own.
        let elsewhere = root.join(".claude-work");
        std::fs::create_dir_all(elsewhere.join("projects")).unwrap();
        assert_eq!(
            auto_memory_settings(&root, &proj, Some(&elsewhere)),
            None,
            "a child whose own config dir has a separate store must get no path"
        );

        // One that does reach the shared store.
        let linked = root.join(".claude-linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(
            root.join(".claude").join("projects"),
            linked.join("projects"),
        )
        .unwrap();
        assert!(
            auto_memory_settings(&root, &proj, Some(&linked)).is_some(),
            "a child linked into the shared store must still be served"
        );
    }

    /// A `--settings` value lands in `flagSettings`, which outranks
    /// `userSettings` in Claude Code's first-non-null chain. So passing
    /// ours would silently override the owner's own deliberate choice.
    /// Where a person's memory goes is not this launcher's call.
    #[test]
    fn auto_memory_settings_leaves_the_owners_own_choice_alone() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = platform_spelling(home.path());
        std::fs::create_dir_all(root.join(".claude").join("projects")).unwrap();
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let settings = root.join(".claude").join("settings.json");
        std::fs::write(&settings, r#"{"autoMemoryDirectory": "/somewhere/of/my/own"}"#).unwrap();
        assert_eq!(
            auto_memory_settings(&root, &proj, None),
            None,
            "the owner named a directory; we must not override it"
        );
        // A settings file that does NOT name one is no obstacle, so the
        // decline above is the key talking and not the file's presence.
        std::fs::write(&settings, r#"{"defaultMode": "auto"}"#).unwrap();
        assert!(auto_memory_settings(&root, &proj, None).is_some());
    }

    /// Past 200 characters Claude Code truncates the sanitised name and
    /// appends a hash of the path that this crate cannot reproduce. Naming
    /// a truncated directory would point memory somewhere the session does
    /// not read, which is worse than the prompt — so it declines.
    ///
    /// `cfg(unix)` only, and not because the limit is platform-specific — it
    /// is not. The fixture has to CREATE a path past the limit, and a
    /// de-verbatimed Windows root re-gains `MAX_PATH` (see
    /// `paths::simplify_verbatim`'s own trade-off note), so the fixture, not
    /// the code under test, is what would fail there.
    #[test]
    #[cfg(unix)]
    fn auto_memory_settings_declines_rather_than_name_a_truncated_directory() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = platform_spelling(home.path());
        std::fs::create_dir_all(root.join(".claude").join("projects")).unwrap();
        // A REAL directory past the limit, in two components because one
        // 240-character name exceeds the per-name maximum. It must exist:
        // the cwd check runs first now, so a synthetic path would decline
        // for that reason instead and prove nothing about the limit.
        let long = root.join("x".repeat(120)).join("y".repeat(120));
        std::fs::create_dir_all(&long).unwrap();
        assert!(long.to_str().unwrap().len() > AUTO_MEMORY_NAME_LIMIT);
        assert_eq!(
            auto_memory_settings(&root, &long, None),
            None,
            "a name past the limit gains a hash suffix we cannot compute"
        );
        // The same fixture DOES produce a flag for a short cwd, so the
        // decline above is the limit talking and not a broken fixture.
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        assert!(auto_memory_settings(&root, &proj, None).is_some());
    }

    /// The blocker a `cfg(unix)`-only suite could not see, and the reason this
    /// test exists at all: on Windows `canonicalize` returns the `\\?\`
    /// extended-length form while a `project_root` off the wire — and the cwd
    /// `CreateProcess` hands the child — is the plain drive path. Comparing
    /// those two raw declined on EVERY Windows box, silently, restoring the
    /// permission prompt on the one platform where every new workspace is a
    /// capsule row. And it was inverted: `canonicalize` is idempotent on
    /// verbatim input, so a `\\?\` root PASSED while the real spelling failed.
    ///
    /// Both halves are asserted, because fixing only the first would leave the
    /// unreachable spelling accepted.
    #[test]
    #[cfg(windows)]
    fn auto_memory_settings_accepts_the_spelling_windows_hands_a_child() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = platform_spelling(home.path());
        assert!(
            !root.to_str().unwrap().starts_with(r"\\?\"),
            "fixture must be the plain drive path a child actually gets: {root:?}"
        );
        std::fs::create_dir_all(root.join(".claude").join("projects")).unwrap();
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        assert!(
            auto_memory_settings(&root, &proj, None).is_some(),
            "the flag must fire for the spelling Windows hands a child"
        );

        // The verbatim spelling, which no child receives, must NOT be the one
        // form that is accepted.
        //
        // The premise is ASSERTED, not tested with an `if`. Review caught that:
        // guarded by a condition, this half would pass by never executing if
        // `canonicalize` ever stopped returning a verbatim path — and it is the
        // only assertion pinning the inversion, where the unreachable spelling
        // was accepted and the real one refused. A dead premise must fail loudly
        // here rather than quietly stop checking.
        let verbatim = home
            .path()
            .canonicalize()
            .expect("canonical tempdir")
            .join("proj");
        assert!(
            verbatim.to_str().unwrap().starts_with(r"\\?\"),
            "premise dead: canonicalize no longer yields a verbatim path, so nothing here pins the inversion any more: {verbatim:?}"
        );
        assert_eq!(
            auto_memory_settings(&root, &verbatim, None),
            None,
            "a verbatim cwd is not what the child gets and must not be accepted"
        );
    }

    /// A cwd that is not already the canonical spelling sanitises to a
    /// DIFFERENT flat name than the child's own `getcwd()` yields, so the
    /// memory would land in a real directory inside the store that nothing
    /// else — no other session on the project, not the transcripts beside
    /// it — ever reads. `project_root` arrives over the wire with an
    /// `.exists()` check and nothing more, which every spelling below
    /// passes.
    ///
    /// The trailing slash is the case a `PathBuf == Path` check would MISS:
    /// `Path` equality compares components, so it compares equal while the
    /// sanitiser maps it to a trailing `-`.
    #[test]
    #[cfg(unix)]
    fn auto_memory_settings_declines_a_cwd_that_is_not_already_canonical() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = platform_spelling(home.path());
        std::fs::create_dir_all(root.join(".claude").join("projects")).unwrap();
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        // The control first: the canonical spelling IS served, so every
        // decline below is the check talking and not a broken fixture.
        assert!(auto_memory_settings(&root, &proj, None).is_some());

        let p = proj.to_str().unwrap();
        for spelling in [
            format!("{p}/"),
            format!("{p}/."),
            format!("{p}/../proj"),
        ] {
            assert_eq!(
                auto_memory_settings(&root, std::path::Path::new(&spelling), None),
                None,
                "a non-canonical cwd must name no path: {spelling}"
            );
        }
        // A symlinked component: the shape that misroutes without looking
        // malformed at all.
        let link = root.join("link");
        std::os::unix::fs::symlink(&proj, &link).unwrap();
        assert_eq!(
            auto_memory_settings(&root, &link, None),
            None,
            "a symlinked cwd component must name no path"
        );
    }

    /// The veto is the SESSION's, not the box's. It used to read the default
    /// account's settings even when the child's own config dir was known, so
    /// a child holding its own `autoMemoryDirectory` had it silently
    /// overridden — the precise harm the veto exists to prevent — while one
    /// key in the default's file switched the flag off for every workspace
    /// on the box. `settings.json` is a `SHARED_ENTRIES` name, so the two
    /// coincide in the ordinary layout and only a fixture like this one can
    /// tell them apart.
    #[test]
    #[cfg(unix)]
    fn the_memory_veto_reads_the_childs_own_settings_not_the_defaults() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = platform_spelling(home.path());
        let store = root.join(".claude").join("projects");
        std::fs::create_dir_all(&store).unwrap();
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        // A child config dir that DOES reach the shared store, so nothing
        // but the settings file can decide the outcome.
        let child = root.join(".claude-work");
        std::fs::create_dir_all(&child).unwrap();
        std::os::unix::fs::symlink(&store, child.join("projects")).unwrap();
        assert!(auto_memory_settings(&root, &proj, Some(&child)).is_some());

        // The child's own choice is respected.
        std::fs::write(
            child.join("settings.json"),
            r#"{"autoMemoryDirectory": "/mine"}"#,
        )
        .unwrap();
        assert_eq!(
            auto_memory_settings(&root, &proj, Some(&child)),
            None,
            "the child named a directory; we must not override it"
        );

        // And the DEFAULT's key does not veto a child whose own file names
        // nothing: that was the box-wide off switch.
        std::fs::write(child.join("settings.json"), r#"{"defaultMode": "auto"}"#).unwrap();
        std::fs::write(
            root.join(".claude").join("settings.json"),
            r#"{"autoMemoryDirectory": "/theirs"}"#,
        )
        .unwrap();
        assert!(
            auto_memory_settings(&root, &proj, Some(&child)).is_some(),
            "another dir's key must not switch the flag off for this child"
        );
    }

    /// Review SHOULD-FIX 2: when the child's config dir is UNKNOWN — which is
    /// EVERY daemon spawn leg, because the daemon's own env carries no
    /// `CLAUDE_CONFIG_DIR` and `accounts::account_env` sets it on the child —
    /// reading the default's settings was not enough. Any named account could be
    /// the one this child runs as, `ensure_account_links` never replaces a
    /// `settings.json` already in place, and overriding one that names a
    /// directory is the precise harm this veto exists to prevent. So the veto
    /// now applies the same proof-or-decline discipline to `settings.json` that
    /// [`shared_projects_root`] already applied to `projects`.
    #[test]
    #[cfg(unix)]
    fn the_memory_veto_declines_when_any_account_keeps_its_own_memory_dir() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = platform_spelling(home.path());
        let store = root.join(".claude").join("projects");
        std::fs::create_dir_all(&store).unwrap();
        let default_settings = root.join(".claude").join("settings.json");
        std::fs::write(&default_settings, r#"{"defaultMode": "auto"}"#).unwrap();
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();

        // An account sharing BOTH entries, which is the ordinary layout.
        let acct = root.join(".claude-auth").join("acct");
        std::fs::create_dir_all(&acct).unwrap();
        std::os::unix::fs::symlink(&store, acct.join("projects")).unwrap();
        std::os::unix::fs::symlink(&default_settings, acct.join("settings.json")).unwrap();
        assert!(
            auto_memory_settings(&root, &proj, None).is_some(),
            "a fully shared account is no obstacle"
        );

        // Now it keeps its OWN settings file, naming its own directory.
        std::fs::remove_file(acct.join("settings.json")).unwrap();
        std::fs::write(
            acct.join("settings.json"),
            r#"{"autoMemoryDirectory": "/its/own"}"#,
        )
        .unwrap();
        assert_eq!(
            auto_memory_settings(&root, &proj, None),
            None,
            "an account's own directory must not be overridden by a spawn that cannot see which account it is"
        );

        // A real file of its own that names NOTHING is no obstacle — so the
        // decline above is the key talking, not merely the file's presence.
        std::fs::write(acct.join("settings.json"), r#"{"defaultMode": "auto"}"#).unwrap();
        assert!(
            auto_memory_settings(&root, &proj, None).is_some(),
            "a real settings file that names no directory must not veto anything"
        );
    }

    /// The branch whose entire justification is Windows: when the default's
    /// `projects` is a real directory the SPELLED store is emitted, never its
    /// resolved form, because `canonicalize` on Windows yields a `\\?\`
    /// extended-length spelling Claude Code would not recognise. Nothing
    /// could observe the difference before — in production the two forms are
    /// identical — so the `.claude` indirection here exists ONLY to make the
    /// two branches distinguishable, and a regression to always-canonical
    /// fails this test.
    #[test]
    #[cfg(unix)]
    fn auto_memory_settings_emits_the_spelled_store_not_its_resolved_form() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = platform_spelling(home.path());
        let real = root.join("real-claude");
        std::fs::create_dir_all(real.join("projects")).unwrap();
        // `.claude` itself is the link; `projects` beneath it is a real
        // directory, so `symlink_metadata` of the joined path reports a
        // directory and the spelled branch is the one taken.
        std::os::unix::fs::symlink(&real, root.join(".claude")).unwrap();
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();

        let json = auto_memory_settings(&root, &proj, None).expect("a resolvable store yields a flag");
        let got: serde_json::Value = serde_json::from_str(&json).unwrap();
        let got = got["autoMemoryDirectory"].as_str().unwrap().to_string();
        assert!(
            got.starts_with(root.join(".claude").join("projects").to_str().unwrap()),
            "must emit the spelled store: {got}"
        );
        assert!(
            !got.starts_with(real.to_str().unwrap()),
            "emitted the resolved form; on Windows that is a \\\\?\\ path: {got}"
        );
    }
}
