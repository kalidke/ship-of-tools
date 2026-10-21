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
/// is one link per account to the default's single file (a symlink; a copy where the OS refuses symlinks), which the caller
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
#[path = "memory_tests.rs"]
mod tests;
