// workspaces.rs — daemon-side registry of project workspaces.
//
// Per ADR 0014, one Ship of Tools daemon hosts many workspaces. Each workspace
// is a (id, slug, label, project_root) tuple plus references to the
// long-lived per-workspace state the daemon owns (kernel child, file
// watcher, BL tmux session — not all wired through here yet).
//
// This module owns the registry + the on-disk persistence layer; per-
// workspace kernel spawn (task #17) and protocol routing (task #18)
// build on top.
//
// On-disk layout:
//
//   ~/.config/sot/workspaces/<slug>.toml      ← ADR 0014, canonical
//   ~/.config/sot/sessions/<slug>.toml        ← ADR 0013, legacy; read for migration
//
// Read is fail-soft: a missing or malformed file is treated as "no
// workspace by that slug" and we keep going. The daemon always has at
// least the *default* workspace (the one whose project_root matches
// `--project-root`), constructed at startup whether or not a toml
// exists for it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::paths;
use crate::rows::workspace::now_unix;

pub(crate) use crate::rows::anchor::default_row_launch_seed;
pub use crate::rows::gate::StartPermit;
pub use crate::rows::workspace::Phase;
pub(crate) use crate::rows::workspace::{Observation, SupervisorIdentity};
pub use crate::rows::{Workspace, WorkspaceChanged, Workspaces};
pub(crate) use crate::comm::mail::bus::{AgentMessage, AgentReceipt};

/// Read every `*.toml` in `~/.config/sot/workspaces/` (and, for
/// migration, in `~/.config/sot/sessions/`) and insert into the
/// registry. Best-effort: a malformed toml is logged and skipped.
/// Returns the count inserted.
///
/// `adopt_legacy_registry` gates the Windows legacy-config-dir MOVE below
/// (`migrate_legacy_windows_config_dir`'s doc has the why): `false` for
/// every non-canonical caller, so a scratch/test daemon can never drain a
/// box's shared legacy registry into itself. `true` only via
/// `--adopt-legacy-registry`, which only `sot-local-daemon.ps1` passes.
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn scan_disk(reg: &Workspaces, adopt_legacy_registry: bool) -> Result<usize> {
    // Windows only: adopt the legacy config dir's backend children first,
    // so the per-host migration right below operates on the NEW root's
    // contents rather than a since-abandoned old one. A failure here is a
    // boot error (`?` — see that function's doc for why), not a warning.
    #[cfg(windows)]
    if adopt_legacy_registry {
        migrate_legacy_windows_config_dir()?;
    }
    // Per-host state dirs (see `declared_host`): adopt the legacy unsuffixed
    // dirs on the first post-deploy boot, before scanning.
    migrate_legacy_state_dirs();
    let mut count = 0;
    let workspaces_dir = workspaces_dir();
    if workspaces_dir.is_dir() {
        count += scan_dir(reg, &workspaces_dir, false)?;
    }
    // ADR 0013 legacy: stamp-on-startup wrote `~/.config/sot/sessions/<slug>.toml`.
    // We still read those so a daemon upgrade doesn't lose adoptable workspaces;
    // they migrate to workspaces/ on next write.
    let sessions_dir = sessions_dir();
    if sessions_dir.is_dir() {
        count += scan_dir(reg, &sessions_dir, true)?;
    }
    Ok(count)
}

fn scan_dir(reg: &Workspaces, dir: &Path, legacy: bool) -> Result<usize> {
    let mut count = 0;
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(0),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        match load_toml(&path, legacy) {
            // A legacy-dir toml is the ADR 0013 migration SOURCE, never the
            // source of truth: the canonical dir was scanned first, and
            // `insert`'s "same slug -> new metadata wins" would otherwise
            // let this stale copy reset the canonical row's runtime/agent/
            // autostart/task in memory on every boot (field defect
            // 2026-09-05, Windows: the ADR 0042 "correcting a stale on-disk
            // tmux" line at each start although the canonical file already
            // said capsule).
            Ok(Some(ws)) if legacy && reg.has_slug(&ws.slug) => {
                tracing::debug!(
                    toml = ?path,
                    slug = %ws.slug,
                    "legacy toml shadowed by its canonical row; skipping"
                );
            }
            Ok(Some(ws)) => {
                reg.insert(ws);
                count += 1;
            }
            Ok(None) => {
                tracing::debug!(toml = ?path, "skipping toml with no [backend] / workspace section");
            }
            Err(e) => {
                tracing::warn!(error = %e, toml = ?path, "could not parse workspace toml; skipping");
            }
        }
    }
    Ok(count)
}

/// Parse a workspace toml. We handle both shapes:
///
///   ADR 0014 (canonical): top-level `workspace_id`, `slug`, `label`,
///   `project_root`, `session_name` (`tmux_session` before protocol 2),
///   optional `[kernel]`.
///
///   ADR 0013 legacy: `[backend]` section with `session_id`, `label`,
///   `project_dir`, `session_name`.
///
/// Returns `Ok(None)` for files that don't look like either (so we can
/// skip without erroring).
fn load_toml(path: &Path, legacy_ok: bool) -> Result<Option<Workspace>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read {path:?}"))?;

    // First pass: top-level (ADR 0014) keys.
    let kv = parse_kv(&text);
    let canonical = kv.get("workspace_id").is_some()
        && kv.get("slug").is_some()
        && kv.get("project_root").is_some();
    if canonical {
        let workspace_id = kv.get("workspace_id").cloned().unwrap_or_default();
        let slug = kv.get("slug").cloned().unwrap_or_default();
        let label = kv.get("label").cloned().unwrap_or_else(|| slug.clone());
        // simplify_verbatim: a toml written by a pre-fix build can carry a
        // Windows `\\?\` verbatim root (`std::fs::canonicalize`'s own
        // return form there) — CreateProcess rejects that as a working
        // directory (`capsule supervisor spawn failed: The directory name
        // is invalid. (os error 267)`, field defect 2026-09-04). Normalize
        // on load so every consumer (capsule spawn's `current_dir`, the
        // awareness env, the tmux path, display, comm-identity root
        // compares) sees the plain form the daemon has always assumed. A
        // no-op on non-Windows and on already-plain paths.
        let project_root = paths::simplify_verbatim(PathBuf::from(
            kv.get("project_root").cloned().unwrap_or_default(),
        ));
        // `tmux_session` is the key every release before protocol 2 wrote
        // (a file shim, not a wire one): read when `session_name` is
        // absent; deletable one release after 0.6.0 final.
        let session_name = kv
            .get("session_name")
            .or_else(|| kv.get("tmux_session"))
            .cloned()
            .unwrap_or_else(|| paths::session_name(&label));
        let created = kv
            .get("created")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or_else(now_unix);
        // Older tomls predate this key → default false.
        let autostart_claude = kv
            .get("autostart_claude")
            .map(|s| s == "true")
            .unwrap_or(false);
        // Older tomls predate these keys → default "" / derive agent.
        let agent = kv.get("agent").cloned().unwrap_or_else(|| {
            if autostart_claude { "claude".into() } else { "none".into() }
        });
        let agent_name = kv.get("agent_name").cloned().unwrap_or_default();
        let task = kv.get("task").cloned().unwrap_or_default();
        // ADR 0042 slice L1a. An older toml predates the `runtime` key and
        // keeps `meta_only`'s per-OS default (see that field's doc).
        let mut ws = Workspace::meta_only(
            workspace_id,
            slug,
            label,
            project_root,
            session_name,
            created,
            autostart_claude,
            agent,
            agent_name,
            task,
        );
        // ADR 0046 decision 1. An older toml predates this key → "" (never
        // joined), matching `meta_only`'s own default.
        if let Some(h) = kv.get("agent_handle") {
            ws.agent_handle = Mutex::new(h.clone());
        }
        // Accounts brief: an older toml predates this key too → "" (the
        // default account), matching `meta_only`'s own default.
        if let Some(a) = kv.get("account") {
            ws.account = Mutex::new(a.clone());
        }
        return Ok(Some(ws));
    }

    if !legacy_ok {
        return Ok(None);
    }

    // Legacy pass: `[backend]` section keys.
    let backend = parse_section(&text, "backend");
    if backend.is_empty() {
        return Ok(None);
    }
    let label = backend.get("label").cloned().unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    });
    // simplify_verbatim: same normalization as the canonical branch above
    // — a legacy toml is just as capable of carrying a stale verbatim root.
    let project_root = paths::simplify_verbatim(PathBuf::from(
        backend
            .get("project_dir")
            .cloned()
            .unwrap_or_else(|| ".".into()),
    ));
    let slug = paths::slug(&label);
    let session_name = backend
        .get("tmux_session")
        .cloned()
        .unwrap_or_else(|| paths::session_name(&label));
    let workspace_id = backend
        .get("session_id")
        .cloned()
        .unwrap_or_else(|| format!("ws-{slug}-legacy"));
    let created = backend
        .get("started")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or_else(now_unix);
    // Legacy [backend] tomls predate these keys → default false / "" (and
    // `meta_only`'s per-OS `runtime`).
    Ok(Some(Workspace::meta_only(
        workspace_id,
        slug,
        label,
        project_root,
        session_name,
        created,
        false,
        "none".to_string(),
        String::new(),
        String::new(),
    )))
}

/// Write `~/.config/sot/workspaces/<slug>.toml`. Frontend-managed
/// sections (`[nav_state]`, `[layout]`, …) that the file already
/// contains are preserved — same approach as `session_state.rs`.
pub fn save(ws: &Workspace) -> Result<PathBuf> {
    let target = workspaces_dir().join(format!("{}.toml", ws.slug));
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create config dir {parent:?}"))?;
    }
    let existing = std::fs::read_to_string(&target).unwrap_or_default();
    let preserved = strip_canonical_top_and_kernel(&existing);

    let mut body = String::new();
    body.push_str(&format!("workspace_id  = {}\n", toml_quote(&ws.workspace_id)));
    body.push_str(&format!("slug          = {}\n", toml_quote(&ws.slug)));
    body.push_str(&format!("label         = {}\n", toml_quote(&ws.label)));
    body.push_str(&format!(
        "project_root  = {}\n",
        toml_quote(&ws.project_root.to_string_lossy())
    ));
    body.push_str(&format!(
        "session_name  = {}\n",
        toml_quote(&ws.session_name)
    ));
    body.push_str(&format!("created       = {}\n", ws.created));
    body.push_str(&format!(
        "autostart_claude = {}\n",
        ws.autostart_claude
    ));
    body.push_str(&format!("agent         = {}\n", toml_quote(&ws.agent())));
    // agent_name / task are free text — quote+escape them exactly as
    // `label` is via `toml_quote` (handles quotes, backslashes, and
    // \n/\r/\t). The load side pairs `strip_quotes` with `toml_unquote`
    // (its inverse), so — same as `label` — an embedded `"` or newline
    // now round-trips exactly (field defect fixed 2026-09-04: the reader
    // used to only strip the surrounding quotes, leaving every escape
    // literal — see `toml_unquote`'s doc).
    body.push_str(&format!("agent_name    = {}\n", toml_quote(&ws.agent_name())));
    body.push_str(&format!("task          = {}\n", toml_quote(&ws.task)));
    body.push_str(&format!("runtime       = {}\n", toml_quote(&ws.runtime)));
    body.push_str(&format!(
        "agent_handle  = {}\n",
        toml_quote(&ws.agent_handle())
    ));
    body.push_str(&format!("account       = {}\n", toml_quote(&ws.account())));

    let final_text = if preserved.trim().is_empty() {
        body
    } else if preserved.ends_with('\n') {
        format!("{body}\n{preserved}")
    } else {
        format!("{body}\n{preserved}\n")
    };

    // Durable (tmp, fsync, replace, directory sync): a shutdown's anchor
    // reset must survive a power loss, as the registration delete does.
    crate::durable::write(&target, final_text.as_bytes())
        .with_context(|| format!("durable write {target:?}"))?;
    Ok(target)
}

/// This daemon's host — `sot_log::state_dir::host_name()` (`SOT_SELF_HOST`
/// else the hostname's first label, lowercased), the ONE resolver (ADR
/// 0046 decision 1; topology plan §D folded the daemon's second resolver,
/// the old `state_host()`, into it). It names the `workspaces-<host>` /
/// `sessions-<host>` state dirs — the reason a workspace toml written by
/// one box is never resurrected on another sharing the home — the host the
/// capsule's `SOT_COMM_SELF_FILE` is suffixed with, the registry-row
/// ownership predicate (so it must equal comm-lib.sh's `sot_host`, which
/// reads the same env and rule), `HelloRes.host` and the awareness env.
/// Uncached, so a unit test may pin `SOT_SELF_HOST` per test; `run()`
/// calls it once at boot so an unresolvable host fails the daemon
/// immediately instead of surfacing as a per-hello error.
pub(crate) fn declared_host() -> String {
    sot_log::state_dir::host_name().unwrap_or_else(|e| {
        tracing::error!(error = %e, "cannot start: no declared host (ADR 0046 decision 1)");
        std::process::exit(1);
    })
}

/// One-time migration: rename the legacy UNSUFFIXED state dirs to this
/// host's suffixed ones. Runs at daemon boot (from `load_all`, after
/// `migrate_legacy_windows_config_dir` on Windows); the first post-deploy
/// boot on the cohort inherits the legacy state (that's the primary dev box
/// — the only daemon that ever wrote it), every other host starts fresh, and
/// public single-home installs have nothing to migrate. Rename failures warn
/// and leave the legacy dir in place (nothing is destroyed).
///
/// The plain rename only covers "suffixed sibling absent yet". When it's
/// already THERE, this folds instead of skipping (`fold_unsuffixed_into_per_host`)
/// — field defect (2026-09-04): on Windows, `migrate_legacy_windows_config_dir`
/// can itself deposit an un-suffixed `workspaces`/`sessions` at the new root
/// — a SECONDARY legacy candidate's un-suffixed dir merges in verbatim-named
/// (see that function's secondary-candidate doc) — and if the PRIMARY
/// candidate (or an earlier boot's default-workspace write) already produced
/// the suffixed sibling at the new root first, the plain rename below used to
/// be a no-op: the secondary's rows sat un-suffixed at the new root forever,
/// in a directory `workspaces_dir()`/`sessions_dir()` never scans. A box that
/// already ran that broken migration has the identical shape stranded at the
/// new root from a prior boot — same fold, no separate recovery path, so
/// there is exactly one place that ever reads or writes an un-suffixed
/// registry dir under `app_config_dir()`.
pub(crate) fn migrate_legacy_state_dirs() {
    for name in ["workspaces", "sessions"] {
        let legacy = app_config_dir().join(name);
        let per_host = app_config_dir().join(format!("{name}-{}", declared_host()));
        if !legacy.is_dir() {
            continue;
        }
        if !per_host.exists() {
            match std::fs::rename(&legacy, &per_host) {
                Ok(()) => tracing::info!(from = %legacy.display(), to = %per_host.display(),
                    "migrated legacy state dir to per-host (ADR 0013/0014 addendum)"),
                Err(e) => tracing::warn!(error = %e, from = %legacy.display(),
                    "legacy state dir migration failed — leaving in place"),
            }
            continue;
        }
        fold_unsuffixed_into_per_host(&legacy, &per_host);
    }
}

/// Fold every `.toml` sitting directly in `legacy` (an un-suffixed registry
/// dir) into `per_host` (its host-suffixed sibling, which already exists),
/// then remove `legacy` once it's empty. Same non-colliding merge rule as
/// `merge_secondary_legacy_windows_children`: a name already present at the
/// destination wins outright — that file is left at `legacy`, untouched, and
/// only warned about (naming both paths); nothing at `per_host` is ever
/// overwritten. Unlike that function, this one MOVES (not copies) each
/// non-colliding file and deletes the source directory when empty — `legacy`
/// here is a dir this daemon owns under its OWN new root, not an
/// still-independently-live legacy candidate root that must be left exactly
/// as found either way. Logs one line per moved toml (field-log
/// distinguishability, matching `migrate_legacy_windows_config_dir`'s own
/// convention). Best-effort: an unreadable `legacy` dir, or a failed
/// individual rename, is a warning, not a boot error — the daemon still
/// starts against whatever `per_host` alone resolves to.
fn fold_unsuffixed_into_per_host(legacy: &Path, per_host: &Path) {
    let entries = match std::fs::read_dir(legacy) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, dir = %legacy.display(),
                "could not read a stranded un-suffixed state dir to fold it into the per-host one");
            return;
        }
    };
    // Counts anything left behind at `legacy` — a name collision, a failed
    // rename, or a non-toml entry — so `legacy` is only removed once it's
    // genuinely empty, never a non-empty dir masquerading as folded.
    let mut left_behind = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            left_behind += 1;
            continue;
        }
        let Some(name) = path.file_name() else {
            left_behind += 1;
            continue;
        };
        let dst = per_host.join(name);
        if dst.exists() {
            tracing::warn!(from = %path.display(), to = %dst.display(),
                "a stranded un-suffixed registry has a toml with the same name as one \
                 already at the per-host dir — left in place, not folded (rename by hand to adopt it)");
            left_behind += 1;
            continue;
        }
        match std::fs::rename(&path, &dst) {
            Ok(()) => tracing::info!(from = %path.display(), to = %dst.display(),
                "folded a stranded un-suffixed registry toml into the per-host dir"),
            Err(e) => {
                tracing::warn!(error = %e, from = %path.display(), to = %dst.display(),
                    "could not fold a stranded un-suffixed registry toml into the per-host dir");
                left_behind += 1;
            }
        }
    }
    if left_behind == 0 {
        if let Err(e) = std::fs::remove_dir(legacy) {
            tracing::warn!(error = %e, dir = %legacy.display(),
                "folded every toml out of a stranded un-suffixed registry dir but could not remove it");
        }
    }
}

fn workspaces_dir() -> PathBuf {
    app_config_dir().join(format!("workspaces-{}", declared_host()))
}

/// Path to a workspace's on-disk toml for the given slug. Mirrors
/// the layout `save()` writes to so handlers can find a file to
/// delete on `workspace.destroy`.
pub fn toml_path_for(slug: &str) -> PathBuf {
    workspaces_dir().join(format!("{slug}.toml"))
}

/// Legacy ADR-0013 session toml for the slug. `workspace.destroy` must
/// remove this too: startup migration re-registers any slug found here,
/// so a surviving legacy toml resurrects a destroyed workspace on every
/// daemon restart (observed: the immortal `montest` session, killed in
/// tmux repeatedly and re-created from its legacy toml each time).
pub fn legacy_toml_path_for(slug: &str) -> PathBuf {
    sessions_dir().join(format!("{slug}.toml"))
}

pub(crate) fn sessions_state_dir() -> PathBuf {
    sessions_dir()
}

fn sessions_dir() -> PathBuf {
    app_config_dir().join(format!("sessions-{}", declared_host()))
}

/// App config dir: `~/.config/sot`. Shared so every backend config resolver
/// (workspaces, sessions, backend-identity) agrees on one dir.
///
/// Windows: delegates to `paths::windows_state_root()`
/// (`%LOCALAPPDATA%\sot`, or `%USERPROFILE%\AppData\Local\sot` — see that
/// function), joined with `config`, instead of the `config_dir()` logic
/// below — which resolves via `$HOME`, a POSIX-only env var with no
/// Windows branch. That was the actual defect (v0.6.0-rc.3 field report):
/// a git-bash shell exports `HOME` (`config_dir()` used to land on
/// `C:\Users\<u>\.config\sot`) while the PowerShell launcher does not (it
/// fell through to the `/tmp/.config` literal, which Windows path
/// handling turns into `\tmp\.config` on whatever the current drive is) —
/// so a hand-started daemon and a launcher-started one built and wrote to
/// TWO DIFFERENT registries, and the default workspace was created twice
/// with different ids, dropping rows between them. `config` keeps this a
/// sibling of `paths::state_dir()`'s `state` under the same
/// `%LOCALAPPDATA%\sot` root, chosen so neither collides with the capsule
/// runtime's own `workspaces\<id>` subtree
/// (`capsule_workspace::state_dir_for`). Mirrors
/// `sot_log::state_dir::sot_state_dir`'s own precedent of ignoring
/// `XDG_*` on Windows in favour of `%LOCALAPPDATA%` (its module doc:
/// letting a second env var win on Windows is exactly how the FE/capsule
/// state dirs drifted apart once already) — `$XDG_CONFIG_HOME` keeps
/// working here on Unix, unchanged, same as before this fix. NO fallback
/// to the `config_dir()` logic below on Windows any more (Codex review,
/// PR #175): `windows_state_root()` panics with a clear message instead
/// of silently landing on a `$HOME`-shaped path there.
///
/// The derivation itself is `sot_log::state_dir::sot_config_dir` (one
/// resolver, shared with `sot-protocol`'s `topology` reader so the daemon
/// and `sotd topology` read `hosts.toml` from the same place); this wrapper
/// keeps the Windows panic; on Unix a process with neither
/// `$XDG_CONFIG_HOME` nor `$HOME` panics with `CONFIG_DIR_UNDERIVABLE`
/// rather than fall back to a shared directory such as `/tmp`, where any
/// local user could pre-create the registry. `main` checks this at startup
/// (`check_config_dir`), so the panic only fires in a caller that skipped it.
pub(crate) fn app_config_dir() -> PathBuf {
    #[cfg(windows)]
    return crate::paths::windows_state_root().join("config");
    #[cfg(not(windows))]
    sot_log::state_dir::sot_config_dir().expect(CONFIG_DIR_UNDERIVABLE)
}

/// The refusal text for a config dir that cannot be derived.
pub(crate) const CONFIG_DIR_UNDERIVABLE: &str = "the daemon's config dir cannot be derived: neither XDG_CONFIG_HOME nor HOME is set; set HOME or XDG_CONFIG_HOME and start it again (it refuses to fall back to a shared directory such as /tmp)";

/// Startup form of `app_config_dir`'s refusal: an `Err` carrying
/// `CONFIG_DIR_UNDERIVABLE` instead of a panic. Always `Ok` on Windows,
/// which has its own check (`paths::windows_state_root`).
pub(crate) fn check_config_dir() -> Result<(), String> {
    #[cfg(not(windows))]
    if sot_log::state_dir::sot_config_dir().is_none() {
        return Err(CONFIG_DIR_UNDERIVABLE.to_string());
    }
    Ok(())
}

/// Directory entries directly under `root` matching the backend's OWN
/// registry dirs: `workspaces`, `sessions` (pre-per-host legacy shape) and
/// `workspaces-*`/`sessions-*` (current per-host shape, any host suffix —
/// `declared_host()` isn't consulted here since migration runs before/
/// independent of which host string this boot resolves). Named
/// explicitly rather than swept wholesale because `<...>\.config\sot` is
/// NOT backend-exclusive on Windows: the frontend resolves its OWN files
/// there too (`settings.toml`, `keybindings.toml`, `hosts.toml`,
/// `state-<host>.toml`) — renaming the whole dir would silently reset the
/// frontend (Codex review finding). Sorted for deterministic iteration
/// order (used by both the rename loop and tests).
#[cfg(windows)]
fn backend_registry_children(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else { continue };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == "workspaces"
            || name == "sessions"
            || name.starts_with("workspaces-")
            || name.starts_with("sessions-")
        {
            out.push(name.to_string());
        }
    }
    out.sort();
    out
}

/// Every root `app_config_dir()` (or its pre-fix predecessor) could have
/// resolved to on Windows, in the order this function probes them —
/// deduplicated by value (`HOME` and `USERPROFILE` commonly hold the same
/// path under Git for Windows; the current-drive and `%SystemDrive%` `tmp`
/// candidates likewise collapse when they're the same drive), so a value
/// reachable by two different env vars is probed, and migrated, exactly
/// once:
///   1. `<XDG_CONFIG_HOME>\sot` — `config_dir()`'s own first tier, honoured
///      on every platform pre-fix, including Windows.
///   2. `<HOME>\.config\sot` — a git-bash shell exports `HOME`; its value
///      there IS `%USERPROFILE%` (so this and #3 usually collapse to one
///      candidate after dedup, but not always — `HOME` can be overridden).
///   3. `<USERPROFILE>\.config\sot` — same shape, keyed off the env var a
///      real Windows login always sets (unlike `HOME`), so migration finds
///      the right root regardless of which shell THIS boot happens to be
///      launched from.
///   4. `\tmp\.config\sot` (current-drive-relative-root) — `config_dir()`'s
///      literal fallback when neither `XDG_CONFIG_HOME` nor `HOME` was
///      set; Windows roots a leading `/`/`\` with no drive prefix onto
///      whatever the process's current drive is.
///   5. `<SystemDrive>\tmp\.config\sot` — the same fallback, explicit-
///      drive form, for when the daemon's current drive isn't the system
///      drive (field-observed value on the box that WAS on the system
///      drive: `wrote backend identity toml
///      toml="/tmp/.config\sot\sessions-<host>\local.toml"`, i.e.
///      `C:\tmp\.config\sot\...`).
#[cfg(windows)]
fn legacy_windows_config_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(v) = std::env::var_os("XDG_CONFIG_HOME") {
        out.push(PathBuf::from(v).join("sot"));
    }
    if let Some(v) = std::env::var_os("HOME") {
        out.push(PathBuf::from(v).join(".config").join("sot"));
    }
    if let Some(v) = std::env::var_os("USERPROFILE") {
        out.push(PathBuf::from(v).join(".config").join("sot"));
    }
    // `config_dir()`'s own literal fallback, byte-for-byte — Windows roots
    // a leading separator with no drive prefix onto the current drive.
    out.push(PathBuf::from("/tmp/.config").join("sot"));
    // Built by string formatting, not `PathBuf::join` — `%SystemDrive%`'s
    // value is a bare `C:` with no trailing separator, and joining onto a
    // prefix-only `PathBuf` with no root component produces a
    // DRIVE-RELATIVE path (`C:tmp\...`, relative to that drive's current
    // dir) rather than the absolute `C:\tmp\...` intended here.
    let system_drive = std::env::var_os("SystemDrive")
        .and_then(|v| v.into_string().ok())
        .unwrap_or_else(|| "C:".to_string());
    out.push(PathBuf::from(format!("{system_drive}\\tmp\\.config\\sot")));

    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

/// Windows-only, one-time migration, run at boot before `scan_disk` reads
/// the registry: before this fix, `app_config_dir()` fell straight through
/// to `config_dir()` above with no Windows branch, so the registry root
/// depended on which shell launched the daemon (see `app_config_dir`'s doc
/// for the field report this fixes) — see `legacy_windows_config_candidates`
/// for every root that could have produced.
///
/// Invariant: **a box's legacy registry moves exactly once, into the
/// canonical daemon.** The "already migrated?" check below (empty
/// `new_root`) alone can't hold it — `new_root` comes from THIS process's
/// own overridable env, so a scratch daemon sees it empty too and would
/// `rename` the box's real registry into itself (field-proven). Fix lives
/// one level up: `scan_disk`'s `adopt_legacy_registry` gate (see its doc).
///
/// Moves ONLY the backend's own registry children
/// (`backend_registry_children`) from the FIRST candidate that has any,
/// via one `rename` per child — never the whole legacy dir, which also
/// holds frontend-owned files on Windows. For every OTHER candidate that
/// also has backend children: each `.toml` whose file name doesn't already
/// exist at the destination is COPIED in (not moved); a name that DOES
/// collide is left at the source and only warned about (naming both
/// paths) — no merge, no overwrite, no deletion anywhere in this function.
///
/// "Already migrated" is decided by the PRESENCE of backend children under
/// the new root, not by the root existing — `app_config_dir()`'s directory
/// can already exist empty (default-workspace persistence creates it) on a
/// box with nothing to migrate, and treating that as "done" would silently
/// skip real legacy data sitting in a candidate.
///
/// A failed rename of a primary child is a BOOT ERROR (`Err`, naming both
/// paths) rather than a warning: with the data only partially moved, this
/// must refuse to boot rather than silently read whatever now sits at the
/// new root (empty or partial) as if it were the whole registry — no
/// fallback to reading the old location, no seeding a fresh registry
/// beside a stranded one. Secondary-candidate copy failures stay
/// warn-only: that candidate's data is untouched (still at its original
/// path) either way, so there's nothing to strand.
///
/// Logs one INFO line per probed candidate (`legacy registry probe: <path>
/// — found (migrating)` / `— empty` / `— absent`) plus one summary line
/// when the new root already had backend children ("migration not
/// needed") — before this, a field boot log with nothing to migrate was
/// silent here, so it couldn't be told apart from "this code never ran"
/// (v0.6.0-rc.4 field report). Returns the per-candidate outcomes so tests
/// can assert on what was probed without a tracing test-capture harness
/// (this crate has none); behaviour is otherwise unchanged.
#[cfg(windows)]
fn migrate_legacy_windows_config_dir() -> Result<Vec<(PathBuf, ProbeOutcome)>> {
    let new_root = app_config_dir();
    if !backend_registry_children(&new_root).is_empty() {
        tracing::info!(root = %new_root.display(),
            "legacy registry migration not needed: new root already has backend children");
        return Ok(Vec::new());
    }
    let candidates = legacy_windows_config_candidates();
    let mut primary_done = false;
    let mut probes = Vec::with_capacity(candidates.len());
    for candidate in &candidates {
        let (children, outcome) = probe_candidate(candidate);
        match outcome {
            ProbeOutcome::Found => {
                tracing::info!("legacy registry probe: {} — found (migrating)", candidate.display())
            }
            ProbeOutcome::Empty => {
                tracing::info!("legacy registry probe: {} — empty", candidate.display())
            }
            ProbeOutcome::Absent => {
                tracing::info!("legacy registry probe: {} — absent", candidate.display())
            }
        }
        probes.push((candidate.clone(), outcome));
        if children.is_empty() {
            continue;
        }
        if !primary_done {
            std::fs::create_dir_all(&new_root).with_context(|| {
                format!("create Windows config root {}", new_root.display())
            })?;
            for child in &children {
                let src = candidate.join(child);
                let dst = new_root.join(child);
                std::fs::rename(&src, &dst).with_context(|| {
                    format!(
                        "migrate legacy Windows config dir: moving {} to {} failed \
                         (registry left split across both paths)",
                        src.display(),
                        dst.display()
                    )
                })?;
                tracing::info!(from = %src.display(), to = %dst.display(),
                    "migrated legacy Windows registry child");
            }
            primary_done = true;
        } else {
            merge_secondary_legacy_windows_children(candidate, &children, &new_root);
        }
    }
    Ok(probes)
}

/// Outcome of probing one `legacy_windows_config_candidates()` entry —
/// logged and returned by `migrate_legacy_windows_config_dir` so a boot
/// with nothing to migrate is distinguishable, in the log and in tests,
/// from this code never having run.
#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeOutcome {
    /// The candidate directory exists and has backend registry children.
    Found,
    /// The candidate directory exists but has no backend registry children.
    Empty,
    /// The candidate directory doesn't exist.
    Absent,
}

#[cfg(windows)]
fn probe_candidate(candidate: &Path) -> (Vec<String>, ProbeOutcome) {
    if !candidate.is_dir() {
        return (Vec::new(), ProbeOutcome::Absent);
    }
    let children = backend_registry_children(candidate);
    let outcome = if children.is_empty() { ProbeOutcome::Empty } else { ProbeOutcome::Found };
    (children, outcome)
}

/// Best-effort merge for a legacy Windows config root OTHER than the one
/// adopted as primary (see `migrate_legacy_windows_config_dir`): for each
/// backend child directory it also has, copy in any `.toml` whose file
/// name isn't already present at the destination; warn (naming both
/// paths) and skip any that is. Never touches or removes the source —
/// this candidate is left exactly as it was found either way.
#[cfg(windows)]
fn merge_secondary_legacy_windows_children(candidate: &Path, children: &[String], new_root: &Path) {
    for child in children {
        let src_dir = candidate.join(child);
        let dst_dir = new_root.join(child);
        if let Err(e) = std::fs::create_dir_all(&dst_dir) {
            tracing::warn!(error = %e, dir = %dst_dir.display(),
                "could not create dir to merge a secondary legacy Windows registry child into — skipping");
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&src_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }
            let Some(name) = path.file_name() else { continue };
            let dst_file = dst_dir.join(name);
            if dst_file.exists() {
                tracing::warn!(from = %path.display(), to = %dst_file.display(),
                    "a second legacy Windows registry has a toml with the same name as one \
                     already migrated — left in place, not merged (rename by hand to adopt it)");
                continue;
            }
            if let Err(e) = std::fs::copy(&path, &dst_file) {
                tracing::warn!(error = %e, from = %path.display(), to = %dst_file.display(),
                    "could not copy a non-colliding toml from a secondary legacy Windows registry");
            }
        }
    }
}

/// Hand-rolled scalar `key = "value"` parser scoped to *top-level*
/// (everything before the first `[section]`). Section bodies are
/// ignored so a section key with the same name as a canonical key
/// can't be mistaken for one. Numeric values (created, started) come
/// through as bare digits and are returned as the raw string.
fn parse_kv(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with('[') && t.ends_with(']') {
            // Hit the first section — stop. The frontend's persisted
            // sections live below, and we don't want their keys to leak
            // into top-level resolution.
            break;
        }
        let Some((k, v)) = t.split_once('=') else { continue };
        out.insert(k.trim().to_string(), toml_unquote(strip_quotes(v.trim())));
    }
    out
}

/// Like `parse_kv` but scoped to a `[section]` block.
fn parse_section(text: &str, section: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut in_section = false;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with('[') && t.ends_with(']') {
            let name = &t[1..t.len() - 1];
            in_section = name == section;
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((k, v)) = t.split_once('=') else { continue };
        out.insert(k.trim().to_string(), toml_unquote(strip_quotes(v.trim())));
    }
    out
}

/// Remove the canonical (top-level) `workspace_id/slug/label/project_root/
/// session_name/created` keys *and* the `[kernel]` section so we can
/// rewrite them. Everything else (e.g. `[nav_state]`, `[layout]`) is
/// preserved verbatim.
fn strip_canonical_top_and_kernel(text: &str) -> String {
    const TOP_KEYS: &[&str] = &[
        "workspace_id",
        "slug",
        "label",
        "project_root",
        "session_name",
        // Pre-protocol-2 spelling, dropped on rewrite; deletable one
        // release after 0.6.0 final.
        "tmux_session",
        "created",
        "autostart_claude",
        "agent",
        "agent_name",
        "task",
        "runtime",
        "agent_handle",
        // `account` was MISSING here while `save` wrote it into the canonical
        // block below — so every save preserved the previous file's line and
        // appended it after the new one, one more copy each time. A row on this
        // box had reached 64 of them in 139 lines; every row file had at least
        // two. It went unnoticed because `parse_kv` is a hand-rolled reader
        // rather than a TOML parser, so duplicate keys never raised an error —
        // and because it takes the LAST value before a section, a freshly
        // written account was silently overruled by the stale copies beneath
        // it. That is not cosmetic: it would have reverted an account switch on
        // the next load while the running session looked correct.
        "account",
    ];
    let mut out = String::new();
    let mut in_top = true;
    let mut skipping_kernel = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') && trimmed.contains(']') {
            in_top = false;
            skipping_kernel = trimmed.starts_with("[kernel]");
            if skipping_kernel {
                continue;
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if skipping_kernel {
            continue;
        }
        if in_top {
            // Drop canonical top-level keys; preserve any others.
            if let Some((k, _)) = trimmed.split_once('=') {
                if TOP_KEYS.contains(&k.trim()) {
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Strips the surrounding `"..."` only — no unescaping. Every reader that
/// pulls a string value out of a workspace toml pairs this with
/// `toml_unquote` (its inverse escapes are `toml_quote`'s), never used
/// alone: a bare `strip_quotes` reproduced `toml_quote`'s doubled
/// backslashes verbatim on load, the bug this pairing fixes (field defect
/// 2026-09-04 — see `toml_unquote`'s own doc).
fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2 && b[0] == b'"' && b[b.len() - 1] == b'"' {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn toml_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Inverse of `toml_quote`, applied to the inside of the quotes
/// (`strip_quotes`'s output): `\\`→`\`, `\"`→`"`, `\n`, `\r`, `\t`. An
/// escape this doesn't recognize (`\U`, `\k`, a lone trailing `\`, …) is
/// kept verbatim as backslash+char rather than dropped, so a toml written
/// by an even older build that never escaped anything at all still loads
/// unchanged — this only widens what the reader accepts, never narrows it.
///
/// Field defect (2026-09-04): before this existed, `load_toml` fed
/// `strip_quotes`'s output straight through, so every saved value that
/// `toml_quote` had escaped loaded back with the escapes still literal —
/// a `project_root` containing `\` round-tripped as doubled backslashes
/// (harmless on Windows, which tolerates repeated separators, so this hid
/// for months) and a saved Windows verbatim root (`\\?\C:\...`, doubled by
/// the writer to `\\\\?\\C:\\...`) never matched `paths::simplify_verbatim`
/// at all, so `CreateProcess` rejected it as a working directory
/// (`capsule supervisor spawn failed: The directory name is invalid. (os
/// error 267)`). `simplify_verbatim` also grew a second, single-backslash
/// prefix form to match: pass this function's OWN output through the
/// unescaper once (a raw, never-escaped legacy write's leading `\\`
/// reads as one escaped backslash, "halving" `\\?\` to `\?\`) and you can
/// see why both shapes are real on-disk data now, not just one.
fn toml_unquote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kv_top_level_only() {
        let text = r#"
workspace_id = "x"
slug         = "alpha"
[kernel]
status = "stopped"
"#;
        let kv = parse_kv(text);
        assert_eq!(kv.get("workspace_id").map(String::as_str), Some("x"));
        assert!(kv.get("status").is_none()); // inside [kernel], not top
    }

    #[test]
    fn parse_section_scoped() {
        let text = r#"
workspace_id = "x"

[backend]
session_id = "y"
label = "MyPkg"
project_dir = "/p"
"#;
        let b = parse_section(text, "backend");
        assert_eq!(b.get("session_id").map(String::as_str), Some("y"));
        assert_eq!(b.get("label").map(String::as_str), Some("MyPkg"));
        assert!(b.get("workspace_id").is_none());
    }

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

    #[test]
    fn strip_canonical_keeps_other_sections() {
        let input = r#"workspace_id = "old"
slug         = "alpha"
label        = "Alpha"
project_root = "/p"
session_name = "sot-be-alpha"
created      = 1700000000

[kernel]
status = "stopped"

[nav_state]
mode = "files"
cursor_path = "src/lib.jl"
"#;
        let stripped = strip_canonical_top_and_kernel(input);
        assert!(!stripped.contains("workspace_id"));
        assert!(!stripped.contains("[kernel]"));
        assert!(!stripped.contains("status = \"stopped\""));
        assert!(stripped.contains("[nav_state]"));
        assert!(stripped.contains("cursor_path = \"src/lib.jl\""));
    }

    // `app_config_dir()`'s platform dispatch, and the one-time Windows
    // migration off the old HOME-derived root. Serialized under the
    // crate-wide `paths::ENV_TEST_LOCK` (Codex review, PR #175: a
    // module-local mutex here couldn't stop a test in THIS module from
    // racing a `paths.rs`/`session_state.rs` test over the same env vars).

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        xdg_config_home: Option<std::ffi::OsString>,
        home: Option<std::ffi::OsString>,
        localappdata: Option<std::ffi::OsString>,
        userprofile: Option<std::ffi::OsString>,
        system_drive: Option<std::ffi::OsString>,
        // Snapshotted/restored too (not just set) because a few tests below
        // pin it to a known value so `migrate_legacy_state_dirs`'s
        // `declared_host()` calls resolve deterministically — the real
        // hostname would otherwise leak into the "-<host>" suffix these
        // tests assert on, and a leaked value would poison every other
        // `declared_host()`-reading test sharing this crate-wide lock.
        sot_self_host: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, val) in [
                ("XDG_CONFIG_HOME", &self.xdg_config_home),
                ("HOME", &self.home),
                ("LOCALAPPDATA", &self.localappdata),
                ("USERPROFILE", &self.userprofile),
                ("SystemDrive", &self.system_drive),
                ("SOT_SELF_HOST", &self.sot_self_host),
            ] {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn env_guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            home: std::env::var_os("HOME"),
            localappdata: std::env::var_os("LOCALAPPDATA"),
            userprofile: std::env::var_os("USERPROFILE"),
            system_drive: std::env::var_os("SystemDrive"),
            sot_self_host: std::env::var_os("SOT_SELF_HOST"),
            _serial: serial,
        }
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

    /// Scratch roots for one migration test: `USERPROFILE`-, `SystemDrive`-
    /// and `XDG_CONFIG_HOME`-style dirs, plus a `LOCALAPPDATA`-style dir
    /// for the new root. All under one per-test base so a single
    /// `remove_dir_all` on the base cleans everything up.
    #[cfg(windows)]
    struct MigrationScratch {
        base: PathBuf,
        userprofile: PathBuf,
        system_drive: String,
        xdg_config_home: PathBuf,
        localappdata: PathBuf,
    }

    #[cfg(windows)]
    impl MigrationScratch {
        fn new(name: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let base = std::env::temp_dir().join(format!(
                "sot-ws-win-config-migrate-{}-{}-{name}",
                std::process::id(),
                n
            ));
            Self {
                userprofile: base.join("userprofile"),
                // No trailing separator, matching a real `%SystemDrive%`
                // value (`C:`) — the base dir's string form stands in for
                // the drive letter here.
                system_drive: base.join("sysdrive-root").to_string_lossy().into_owned(),
                xdg_config_home: base.join("xdg-config-home"),
                localappdata: base.join("localappdata"),
                base,
            }
        }

        fn userprofile_legacy(&self) -> PathBuf {
            self.userprofile.join(".config").join("sot")
        }

        fn system_drive_legacy(&self) -> PathBuf {
            PathBuf::from(format!("{}\\tmp\\.config\\sot", self.system_drive))
        }

        fn xdg_config_home_legacy(&self) -> PathBuf {
            self.xdg_config_home.join("sot")
        }

        fn new_root(&self) -> PathBuf {
            self.localappdata.join("sot").join("config")
        }

        /// Clears every candidate-relevant var, then sets only
        /// `USERPROFILE`/`SystemDrive`/`LOCALAPPDATA` (what a real Windows
        /// login always has) — a test that wants `XDG_CONFIG_HOME` or
        /// `HOME` in the mix sets it after calling this.
        fn apply_env(&self) {
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("HOME");
            std::env::set_var("USERPROFILE", &self.userprofile);
            std::env::set_var("SystemDrive", &self.system_drive);
            std::env::set_var("LOCALAPPDATA", &self.localappdata);
        }
    }

    #[cfg(windows)]
    impl Drop for MigrationScratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    #[cfg(windows)]
    fn backend_registry_children_finds_workspace_and_session_dirs_not_frontend_files() {
        let s = MigrationScratch::new("children-scan");
        let root = s.base.join("scan-root");
        std::fs::create_dir_all(root.join("workspaces-host")).unwrap();
        std::fs::create_dir_all(root.join("sessions-otherhost")).unwrap();
        std::fs::create_dir_all(root.join("workspaces")).unwrap();
        std::fs::write(root.join("settings.toml"), "").unwrap();
        std::fs::write(root.join("hosts.toml"), "").unwrap();
        assert_eq!(
            backend_registry_children(&root),
            vec!["sessions-otherhost", "workspaces", "workspaces-host"]
        );
    }

    #[test]
    #[cfg(windows)]
    fn migrate_moves_only_backend_children_and_leaves_frontend_files_in_place() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("frontend-untouched");
        let legacy = s.userprofile_legacy();
        let workspace_toml = legacy.join("workspaces-host").join("alpha.toml");
        std::fs::create_dir_all(workspace_toml.parent().unwrap()).unwrap();
        std::fs::write(&workspace_toml, "slug = \"alpha\"\n").unwrap();
        // Frontend-owned files sharing the same legacy root — must survive.
        std::fs::write(legacy.join("settings.toml"), "frontend settings").unwrap();
        std::fs::write(legacy.join("hosts.toml"), "frontend hosts").unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(!legacy.join("workspaces-host").exists(), "backend child should have moved");
        assert!(s.new_root().join("workspaces-host").join("alpha.toml").is_file());
        // Frontend files are untouched at their original path — the
        // legacy dir itself was never renamed, only its backend children.
        assert_eq!(
            std::fs::read_to_string(legacy.join("settings.toml")).unwrap(),
            "frontend settings"
        );
        assert_eq!(
            std::fs::read_to_string(legacy.join("hosts.toml")).unwrap(),
            "frontend hosts"
        );
    }

    /// Field defect: a scratch/test daemon must never drain the box's
    /// shared legacy registry into itself. Proves the gate at `scan_disk`.
    #[test]
    #[cfg(windows)]
    fn scan_disk_never_adopts_legacy_registry_without_the_canonical_flag() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("no-adopt-without-flag");
        let legacy = s.userprofile_legacy();
        let workspace_toml = legacy.join("workspaces-host").join("alpha.toml");
        std::fs::create_dir_all(workspace_toml.parent().unwrap()).unwrap();
        std::fs::write(&workspace_toml, "slug = \"alpha\"\n").unwrap();
        s.apply_env();

        scan_disk(&Workspaces::new(), false).unwrap();

        assert!(workspace_toml.is_file(), "legacy registry must be left exactly where it was");
        assert!(!s.new_root().join("workspaces-host").exists(), "nothing may land at the new root");
    }

    #[test]
    #[cfg(windows)]
    fn migrate_prefers_xdg_config_home_over_every_other_candidate() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("xdg-first");
        s.apply_env();
        std::env::set_var("XDG_CONFIG_HOME", &s.xdg_config_home);
        let xdg_legacy = s.xdg_config_home_legacy();
        std::fs::create_dir_all(xdg_legacy.join("sessions-host")).unwrap();
        // A USERPROFILE-rooted candidate also exists, but XDG_CONFIG_HOME
        // must win since it's probed first.
        std::fs::create_dir_all(s.userprofile_legacy().join("sessions-host")).unwrap();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(!xdg_legacy.join("sessions-host").exists());
        assert!(s.new_root().join("sessions-host").is_dir());
        // The USERPROFILE candidate was only SECONDARY here — its own
        // empty child dir is left exactly as found, not deleted.
        assert!(s.userprofile_legacy().join("sessions-host").is_dir());
    }

    #[test]
    #[cfg(windows)]
    fn migrate_falls_back_to_system_drive_root_when_nothing_earlier_has_children() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("sysdrive");
        let legacy = s.system_drive_legacy();
        std::fs::create_dir_all(legacy.join("sessions-host")).unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(!legacy.join("sessions-host").exists());
        assert!(s.new_root().join("sessions-host").is_dir());
    }

    #[test]
    #[cfg(windows)]
    fn migrate_copies_noncolliding_tomls_from_a_secondary_candidate_and_warns_on_collision() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("secondary-merge");
        let primary = s.userprofile_legacy();
        std::fs::create_dir_all(primary.join("workspaces-host")).unwrap();
        std::fs::write(primary.join("workspaces-host").join("alpha.toml"), "primary alpha").unwrap();
        let secondary = s.system_drive_legacy();
        std::fs::create_dir_all(secondary.join("workspaces-host")).unwrap();
        // Non-colliding: gets copied in.
        std::fs::write(secondary.join("workspaces-host").join("beta.toml"), "secondary beta").unwrap();
        // Colliding name: left at the source, not overwritten at the dest.
        std::fs::write(secondary.join("workspaces-host").join("alpha.toml"), "secondary alpha").unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        let dst = s.new_root().join("workspaces-host");
        assert_eq!(std::fs::read_to_string(dst.join("alpha.toml")).unwrap(), "primary alpha");
        assert_eq!(std::fs::read_to_string(dst.join("beta.toml")).unwrap(), "secondary beta");
        // Secondary candidate is left in place entirely — including the
        // colliding file, which was never deleted or overwritten.
        assert_eq!(
            std::fs::read_to_string(secondary.join("workspaces-host").join("alpha.toml")).unwrap(),
            "secondary alpha"
        );
    }

    #[test]
    #[cfg(windows)]
    fn migrate_no_op_when_new_root_already_has_backend_children() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("already-migrated");
        let legacy = s.userprofile_legacy();
        std::fs::create_dir_all(legacy.join("workspaces-host")).unwrap();
        std::fs::write(legacy.join("workspaces-host").join("stray.toml"), "should not move").unwrap();
        std::fs::create_dir_all(s.new_root().join("workspaces-host")).unwrap();
        std::fs::write(s.new_root().join("workspaces-host").join("canonical.toml"), "canonical").unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(legacy.join("workspaces-host").join("stray.toml").is_file());
        assert!(s.new_root().join("workspaces-host").join("canonical.toml").is_file());
        assert!(!s.new_root().join("workspaces-host").join("stray.toml").exists());
    }

    #[test]
    #[cfg(windows)]
    fn migrate_proceeds_when_new_root_exists_but_has_no_backend_children_yet() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("empty-new-root");
        let legacy = s.userprofile_legacy();
        std::fs::create_dir_all(legacy.join("workspaces-host")).unwrap();
        std::fs::write(legacy.join("workspaces-host").join("alpha.toml"), "alpha").unwrap();
        // The new root dir already exists (e.g. default-workspace
        // persistence created it) but holds no backend children yet — this
        // must NOT be mistaken for "already migrated".
        std::fs::create_dir_all(s.new_root()).unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(!legacy.join("workspaces-host").exists());
        assert!(s.new_root().join("workspaces-host").join("alpha.toml").is_file());
    }

    #[test]
    #[cfg(windows)]
    fn migrate_no_op_when_no_candidate_has_backend_children() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("no-legacy");
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(!s.new_root().exists());
    }

    /// The daemon logs one `tracing::info!` line per probed candidate
    /// (found/empty/absent) so a field boot log can tell "probed, nothing
    /// there" apart from "this code never ran" — but this crate has no
    /// tracing test-capture harness, so this asserts on the return value
    /// (`Vec<(PathBuf, ProbeOutcome)>`) the same probe loop reports
    /// through, rather than on captured log output.
    #[test]
    #[cfg(windows)]
    fn migrate_returns_probe_outcome_per_candidate_including_the_empty_case() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("probe-outcomes");
        // USERPROFILE candidate: directory exists but has no backend
        // registry children — "empty", the case a silent log can't be
        // told apart from "absent" or "never probed".
        std::fs::create_dir_all(s.userprofile_legacy()).unwrap();
        // SystemDrive candidate: has backend children — "found".
        std::fs::create_dir_all(s.system_drive_legacy().join("workspaces-host")).unwrap();
        s.apply_env();

        let probes = migrate_legacy_windows_config_dir().unwrap();

        let outcome_for = |path: &Path| {
            probes.iter().find(|(p, _)| p == path).map(|(_, o)| *o)
        };
        assert_eq!(outcome_for(&s.userprofile_legacy()), Some(ProbeOutcome::Empty));
        assert_eq!(outcome_for(&s.system_drive_legacy()), Some(ProbeOutcome::Found));
    }

    /// End-to-end field defect (2026-09-04): a PRIMARY legacy candidate
    /// already had the host-suffixed dir (an earlier boot at that root had
    /// already run the per-host migration), while a SECONDARY candidate
    /// still had an un-suffixed one holding a row the primary never saw.
    /// `migrate_legacy_windows_config_dir` moves the primary's suffixed dir
    /// straight across (name preserved) and merges the secondary's
    /// un-suffixed one in verbatim-named — landing an un-suffixed
    /// `workspaces` at the new root right next to the suffixed one.
    /// `migrate_legacy_state_dirs`, called right after (mirroring
    /// `scan_disk`'s own call order), used to see the suffixed sibling
    /// already present and stop, stranding the secondary's row where the
    /// daemon's own `workspaces_dir()` never scans. This proves both the
    /// stranding (mid-run) and the fold that now recovers it.
    #[test]
    #[cfg(windows)]
    fn migrate_then_fold_lands_every_legacy_row_in_the_host_suffixed_dir() {
        let _guard = env_guarded();
        std::env::set_var("SOT_SELF_HOST", "host");
        let s = MigrationScratch::new("migrate-then-fold");
        let primary = s.userprofile_legacy();
        std::fs::create_dir_all(primary.join("workspaces-host")).unwrap();
        std::fs::write(primary.join("workspaces-host").join("alpha.toml"), "alpha").unwrap();
        let secondary = s.system_drive_legacy();
        std::fs::create_dir_all(secondary.join("workspaces")).unwrap();
        std::fs::write(secondary.join("workspaces").join("beta.toml"), "beta").unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();
        // Confirms the setup reproduces the stranding, not just the fix:
        // beta landed un-suffixed at the new root, next to workspaces-host.
        assert!(s.new_root().join("workspaces").join("beta.toml").is_file());

        migrate_legacy_state_dirs();

        let dst = s.new_root().join("workspaces-host");
        assert_eq!(
            std::fs::read_to_string(dst.join("alpha.toml")).unwrap(),
            "alpha"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("beta.toml")).unwrap(),
            "beta"
        );
        assert!(
            !s.new_root().join("workspaces").exists(),
            "the stranded un-suffixed dir must be folded in and removed"
        );
    }

    /// A box that already ran the broken migration once (a pre-fix build)
    /// has the stranding baked in from a PRIOR boot — not created fresh by
    /// THIS boot's `migrate_legacy_windows_config_dir` (that step is a
    /// no-op here: `app_config_dir()`'s backend children are already
    /// non-empty). Exercises `migrate_legacy_state_dirs` folding a
    /// pre-existing stranded dir on its own, with no windows-config-dir
    /// migration involved at all.
    #[test]
    #[cfg(windows)]
    fn fold_adopts_a_dir_stranded_by_an_earlier_broken_boot() {
        let _guard = env_guarded();
        std::env::set_var("SOT_SELF_HOST", "host");
        let s = MigrationScratch::new("fold-stranded");
        s.apply_env();
        let new_root = s.new_root();
        std::fs::create_dir_all(new_root.join("workspaces-host")).unwrap();
        std::fs::write(new_root.join("workspaces-host").join("gamma.toml"), "gamma").unwrap();
        std::fs::create_dir_all(new_root.join("workspaces")).unwrap();
        std::fs::write(new_root.join("workspaces").join("delta.toml"), "delta").unwrap();

        migrate_legacy_state_dirs();

        let dst = new_root.join("workspaces-host");
        assert_eq!(
            std::fs::read_to_string(dst.join("gamma.toml")).unwrap(),
            "gamma"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("delta.toml")).unwrap(),
            "delta"
        );
        assert!(!new_root.join("workspaces").exists());
    }

    /// Collision: the destination already has a toml with the same name.
    /// The destination row wins outright; the source file is left exactly
    /// where it was (not deleted, not overwritten) and the now-not-actually
    /// -empty legacy dir is left too, not removed — same rule
    /// `migrate_legacy_windows_config_dir`'s own secondary-candidate merge
    /// uses (`merge_secondary_legacy_windows_children`).
    #[test]
    #[cfg(windows)]
    fn fold_keeps_the_destination_row_on_a_name_collision() {
        let _guard = env_guarded();
        std::env::set_var("SOT_SELF_HOST", "host");
        let s = MigrationScratch::new("fold-collision");
        s.apply_env();
        let new_root = s.new_root();
        std::fs::create_dir_all(new_root.join("workspaces-host")).unwrap();
        std::fs::write(
            new_root.join("workspaces-host").join("alpha.toml"),
            "canonical",
        )
        .unwrap();
        std::fs::create_dir_all(new_root.join("workspaces")).unwrap();
        std::fs::write(new_root.join("workspaces").join("alpha.toml"), "stray").unwrap();

        migrate_legacy_state_dirs();

        assert_eq!(
            std::fs::read_to_string(new_root.join("workspaces-host").join("alpha.toml")).unwrap(),
            "canonical"
        );
        assert_eq!(
            std::fs::read_to_string(new_root.join("workspaces").join("alpha.toml")).unwrap(),
            "stray"
        );
        assert!(
            new_root.join("workspaces").is_dir(),
            "non-empty legacy dir must not be removed"
        );
    }
}
