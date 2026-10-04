//! The row toml store: scan, load and save of `workspaces-<host>/<slug>.toml` and the config-dir rule.

mod codec;
mod migrate;
#[cfg(test)]
mod support_tests;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::paths;

use super::{Workspace, Workspaces};
use super::workspace::now_unix;

use codec::*;
use migrate::*;

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
            .unwrap_or_else(|| super::session_name(&label));
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
        .unwrap_or_else(|| super::session_name(&label));
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
/// contains are preserved.
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

/// This daemon's host — `sot_log::host::state_dir::host_name()` (`SOT_SELF_HOST`
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
    sot_log::host::state_dir::host_name().unwrap_or_else(|e| {
        tracing::error!(error = %e, "cannot start: no declared host (ADR 0046 decision 1)");
        std::process::exit(1);
    })
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
/// (`rows::spawn::state_root::state_dir_for`). Mirrors
/// `sot_log::host::state_dir::sot_state_dir`'s own precedent of ignoring
/// `XDG_*` on Windows in favour of `%LOCALAPPDATA%` (its module doc:
/// letting a second env var win on Windows is exactly how the FE/capsule
/// state dirs drifted apart once already) — `$XDG_CONFIG_HOME` keeps
/// working here on Unix, unchanged, same as before this fix. NO fallback
/// to the `config_dir()` logic below on Windows any more (Codex review,
/// PR #175): `windows_state_root()` panics with a clear message instead
/// of silently landing on a `$HOME`-shaped path there.
///
/// The derivation itself is `sot_log::host::state_dir::sot_config_dir` (one
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
    sot_log::host::state_dir::sot_config_dir().expect(CONFIG_DIR_UNDERIVABLE)
}

/// The refusal text for a config dir that cannot be derived.
pub(crate) const CONFIG_DIR_UNDERIVABLE: &str = "the daemon's config dir cannot be derived: neither XDG_CONFIG_HOME nor HOME is set; set HOME or XDG_CONFIG_HOME and start it again (it refuses to fall back to a shared directory such as /tmp)";

/// Startup form of `app_config_dir`'s refusal: an `Err` carrying
/// `CONFIG_DIR_UNDERIVABLE` instead of a panic. Always `Ok` on Windows,
/// which has its own check (`paths::windows_state_root`).
pub(crate) fn check_config_dir() -> Result<(), String> {
    #[cfg(not(windows))]
    if sot_log::host::state_dir::sot_config_dir().is_none() {
        return Err(CONFIG_DIR_UNDERIVABLE.to_string());
    }
    Ok(())
}
