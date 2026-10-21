//! The daemon's side of the comm registry (registry.json): fresh reads, the lock, one entry's edit, pruning, and the
//! UTC stamps comm writes.

/// ISO-8601 UTC instant (e.g. `2026-05-29T14:30:05Z`) without pulling in
/// chrono — the backend has no time crate, so format the civil date from
/// the Unix timestamp directly. Used to stamp relayed agent messages.
pub(crate) fn iso8601_utc_now() -> String {
    iso8601_utc_from_secs(unix_now_secs())
}

pub(crate) fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The same fixed-width shape for any instant — what `comm.file`'s heartbeat
/// cutoff is built with, so it compares as a string against `last_seen`.
pub(crate) fn iso8601_utc_from_secs(secs: u64) -> String {
    // Days since the Unix epoch and seconds-of-day.
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    let (hh, mm, ss) = (sod / 3_600, (sod % 3_600) / 60, sod % 60);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hh, mm, ss
    )
}

/// Howard Hinnant's days-from-civil inverse: convert days-since-epoch to a
/// (year, month, day) Gregorian date. Public-domain algorithm; avoids a
/// date crate dependency for the single timestamp we need.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

/// Resolve the sot-comm registry path: `<sot-comm home>/registry.json`,
/// via the ONE shared resolver (`paths::sot_comm_home`, Codex round
/// finding 8 — the same one `agents::env::capsule_supervisor_env`
/// injects as `SOT_COMM_HOME` into a spawned capsule's env, so the daemon
/// and the scripts can never disagree about where `~/.sot-comm` is).
/// Returns `None` only when the resolver itself found nothing (no
/// `SOT_COMM_HOME`, `HOME`, or `USERPROFILE`) — every other failure is
/// the caller's to treat as "absent" (empty strings).
pub(crate) fn comm_registry_path() -> Option<std::path::PathBuf> {
    let mut p = crate::comm::sot_comm_home()?;
    p.push("registry.json");
    Some(p)
}

/// Read + parse the sot-comm registry, returning the `.agents` object as a
/// JSON value. Fully defensive: a missing file, unreadable path, or malformed
/// JSON all yield `None` so `workspace.list` never errors on the registry. The
/// FE can't read the registry (separate HOME), so we surface it here. The
/// bytes are `read_registry_fresh`'s, whose retry sleeps: call it on a
/// blocking thread.
pub(crate) fn read_comm_agents() -> Option<serde_json::Value> {
    let bytes = read_registry_fresh(&comm_registry_path()?).ok()?;
    let root: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    root.get("agents").cloned()
}

/// Write `bytes` to `path` and flush them to the server, so the rename that
/// follows can never publish a registry whose data is not there yet.
fn write_synced(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Write `root` as the registry: pretty JSON and a newline, flushed to the temp
/// file, then renamed into place. A failure is logged and the registry stays as
/// it was. Run under the lock.
fn replace_registry(
    reg_path: &std::path::Path,
    tmp_path: &std::path::Path,
    root: &serde_json::Value,
) -> bool {
    let mut serialized = match serde_json::to_vec_pretty(root) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "comm registry serialize failed");
            return false;
        }
    };
    serialized.push(b'\n');
    if let Err(e) = write_synced(tmp_path, &serialized) {
        tracing::warn!(error = %e, "comm registry tmp write failed");
        return false;
    }
    if let Err(e) = std::fs::rename(tmp_path, reg_path) {
        tracing::warn!(error = %e, "comm registry rename failed");
        let _ = std::fs::remove_file(tmp_path);
        return false;
    }
    true
}

/// The registry's bytes, as the scripts' `sot_registry_bytes` reads them. An
/// NFSv4 client can get ESTALE (stale file handle) from a read after its open
/// succeeded, when another host renames a new registry over the file: the
/// two-host test measured 16 in about 2,000 reads on the first host before the
/// retry, 0 in about 9,000 on the peer. So a try that fails (any error, or zero
/// bytes) opens the folder, which revalidates it, and reads by path again, a
/// new open each time: up to 3 retries in about 200 ms. NotFound is returned
/// only from the first try (absent); a later try's is a file that vanished
/// mid-retry, a failed try like any other, and no good read by then is an
/// error that is never NotFound, so never absent. Zero bytes stays in the rule
/// because an empty file is never a registry (every writer syncs a checked tmp
/// before its rename, and the scripts' `ensure_home` its skeleton before its
/// link), but no zero-byte read has ever been observed. Non-empty
/// bytes are never retried, parseable or not.
pub(crate) fn read_registry_fresh(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    read_registry_fresh_with(path, std::thread::sleep)
}

/// `read_registry_fresh` with its pause as a parameter: a test hook, so a test
/// acts between tries without racing a clock.
pub(crate) fn read_registry_fresh_with(path: &std::path::Path, mut pause: impl FnMut(std::time::Duration)) -> std::io::Result<Vec<u8>> {
    let mut failed = match std::fs::read(path) {
        Ok(bytes) if !bytes.is_empty() => return Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(e),
        Ok(_) => "zero bytes".to_string(),
        Err(e) => e.to_string(),
    };
    for pause_ms in [0, 100, 100] {
        pause(std::time::Duration::from_millis(pause_ms));
        if let Some(dir) = path.parent() {
            let _ = std::fs::File::open(dir);
        }
        failed = match std::fs::read(path) {
            Ok(bytes) if !bytes.is_empty() => return Ok(bytes),
            Ok(_) => "zero bytes".to_string(),
            Err(e) => e.to_string(),
        };
    }
    Err(std::io::Error::other(format!("no good read in 4 tries; the last: {failed}")))
}

/// Does this sot-comm registry row's `host` field match `host` (case-
/// insensitive)? An absent or empty `host` is UNKNOWN ownership, never a
/// match — comm-join.sh has stamped `host` (the raw `hostname -s`, case
/// preserved) on every row since the registry existed, so a row without
/// one is not evidence it's ours (LU5d2, Codex text round finding 3: the
/// prior "no host = legacy, matches on session alone" clause let a
/// hand-edited or foreign-tool row bind here on name/session alone).
pub(crate) fn host_matches(entry: &serde_json::Value, host: &str) -> bool {
    entry
        .get("host")
        .and_then(|v| v.as_str())
        .map(|h| !h.is_empty() && h.eq_ignore_ascii_case(host))
        .unwrap_or(false)
}

/// The row an entry names: its non-empty `workspace_id`, the row whose
/// session last joined that handle (`comm-join.sh` writes it from
/// `$SOT_WORKSPACE_ID` before it declares the handle to the daemon).
pub(crate) fn entry_row(entry: &serde_json::Value) -> Option<&str> {
    entry.get("workspace_id").and_then(|v| v.as_str()).filter(|id| !id.is_empty())
}

/// The row whose session last joined `handle` on `host`: the registry entry's
/// `workspace_id` (`entry_row`), if the entry is this host's. `None` when there
/// is no registry, no entry, another host's entry or no row id in it.
pub(crate) fn last_joiner(handle: &str, host: &str) -> Option<String> {
    let agents = read_comm_agents()?;
    let entry = agents.get(handle).filter(|e| host_matches(e, host))?;
    entry_row(entry).map(str::to_string)
}

/// Take the sot-comm registry lock (`<comm_home>/.registry.lock`, a file
/// naming its holder: `comm::registry::lock`, the same lock as `comm-lib.sh`'s
/// `with_lock`), run `f` with the registry and tmp-file paths, and release
/// the lock on every exit path. THE ONE lock helper —
/// `remove_comm_agents_for_workspace`, `edit_entry` and `stamp_last_seen` call this,
/// so there is one lock protocol, not two with quietly different rules.
///
/// Bounded at `bound` (polled every 50ms) and FAILS CLOSED: a holder proved
/// dead on this machine is reclaimed, and any other is never force-broken —
/// `with_lock`'s own rule since PR #148 finding F2. The FAILED line, naming the
/// holder and the recovery, goes to `tracing::warn` and this returns `None`. A
/// caller that skips its write this once is cosmetic (the next writer, or the
/// next attempt, retries against a byte-identical row); a forced takeover can
/// corrupt a concurrent shell writer's in-flight `registry.json.tmp`, which is
/// not recoverable the same way.
///
/// `None` also on anything that keeps this from even starting: no
/// `comm_registry_path()` (no `SOT_COMM_HOME`/`HOME`/`USERPROFILE`), or a
/// path with no parent directory.
fn with_comm_registry_lock<T>(
    bound: std::time::Duration,
    f: impl FnOnce(&std::path::Path, &std::path::Path) -> T,
) -> Option<T> {
    let reg_path = comm_registry_path()?;
    let dir = reg_path.parent()?.to_path_buf();
    let tmp_path = dir.join("registry.json.tmp");

    // RAII release: `f` runs under the caller's `spawn_blocking`, which
    // contains a panic, but a plain "release after the call" would leave the
    // lock behind on unwind, and every later writer would wedge closed. The
    // held lock's `Drop` runs on unwind too.
    let _held = match crate::comm::registry::lock::acquire(&dir.join(".registry.lock"), bound) {
        Ok(held) => held,
        Err(e) => {
            tracing::warn!("comm registry lock: {e}");
            return None;
        }
    };
    Some(f(&reg_path, &tmp_path))
}

/// `remove_comm_agents_for_workspace`'s lock bound: a `workspace.destroy`
/// is a one-shot, user-triggered action off the hot per-connection reply
/// path, so it can afford to sit
/// out a much longer contention window than a work-state write
/// before giving up — matches the OLD force-break threshold (200×50ms), so
/// the fail-closed change doesn't also make destroys flaky on an
/// ordinarily-brief contention.
const COMM_PRUNE_LOCK_BOUND: std::time::Duration = std::time::Duration::from_secs(10);

/// Remove the sot-comm registry rows owned by a workspace that is being
/// destroyed, returning the handles removed (for logging). A killed agent can't
/// run `comm-leave` for itself, so its row would otherwise persist and show as
/// a ghost in `workspace.list`. A row belongs to this workspace when its handle equals the
/// stored `agent_name` AND `host_matches` too (LU5d2: the stored name is
/// caller-supplied, not proof of ownership — a same-named row stamped by
/// another host must survive), or when its own `workspace_id` field equals
/// this workspace's id AND `host_matches` (covers a manually-joined handle,
/// e.g. `comm-join.sh --name other`, whose `agent_name` was never set to
/// this workspace's). A by-name match whose entry carries another row's
/// `workspace_id` is skipped (ADR 0049: the handle moved to that row). ALL
/// matching rows are dropped, including stale
/// duplicates on the same session.
///
/// Fully best-effort: a missing registry, malformed JSON, a lock that can't be
/// taken within `COMM_PRUNE_LOCK_BOUND` (fail-closed, via
/// `with_comm_registry_lock` — never force-broken), or any I/O failure yields
/// an empty result and never propagates — the destroy must not fail because
/// the registry couldn't be pruned. Writes via a temp file + atomic rename so
/// a concurrent bash mutator (comm-join / comm-status / …) can't see a torn
/// file.
pub(crate) fn remove_comm_agents_for_workspace(agent_name: &str, workspace_id: &str, host: &str) -> Vec<String> {
    remove_comm_agents_for_workspace_bounded(agent_name, workspace_id, host, COMM_PRUNE_LOCK_BOUND)
}

/// `remove_comm_agents_for_workspace` with an explicit lock bound — split out
/// so a test can exercise the real prune body under a SHORT contended-lock
/// bound (proving it fails closed, as a work-state write does)
/// without waiting out the real `COMM_PRUNE_LOCK_BOUND`. Production code
/// only ever calls the wrapper above.
fn remove_comm_agents_for_workspace_bounded(
    agent_name: &str,
    workspace_id: &str,
    host: &str,
    bound: std::time::Duration,
) -> Vec<String> {
    with_comm_registry_lock(bound, |reg_path, tmp_path| -> Vec<String> {
        let bytes = match read_registry_fresh(reg_path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                tracing::warn!(error = %e, "comm registry read failed");
                return Vec::new();
            }
        };
        let mut root: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "comm registry parse failed");
                return Vec::new();
            }
        };
        let Some(agents) = root.get_mut("agents").and_then(|a| a.as_object_mut()) else {
            return Vec::new();
        };
        let to_remove: Vec<String> = agents
            .iter()
            .filter_map(|(handle, entry)| {
                let by_name = !agent_name.is_empty()
                    && handle == agent_name
                    && entry_row(entry).map_or(true, |id| id == workspace_id)
                    && host_matches(entry, host);
                let by_workspace = entry_row(entry) == Some(workspace_id) && host_matches(entry, host);
                (by_name || by_workspace).then(|| handle.clone())
            })
            .collect();
        if to_remove.is_empty() {
            return Vec::new();
        }
        for handle in &to_remove {
            agents.remove(handle);
        }
        if !replace_registry(reg_path, tmp_path, &root) {
            return Vec::new();
        }
        to_remove
    })
    .unwrap_or_default()
}

/// Why [`edit_entry`] wrote nothing.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EditError {
    /// No comm home to find the registry in.
    NoHome,
    /// The lock was not taken within its bound: the FAILED line naming its holder.
    Locked(String),
    /// The registry could not be read or parsed.
    Unreadable(String),
    /// The new registry could not be written.
    Unwritten,
}

/// Edit `.agents[handle]` under the registry lock, on a fresh read: `edit` gets the entry and returns its answer and
/// whether to write, and the registry is replaced only then. `Ok(None)` when there is no registry file or no such
/// entry (nothing to edit, nothing written); every other failure writes nothing and says why. The one way another
/// subsystem changes an entry's fields (rows' work-state); the entry's address-book fields stay this subsystem's.
pub(crate) fn edit_entry<T>(
    handle: &str,
    bound: std::time::Duration,
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>) -> (T, bool),
) -> Result<Option<T>, EditError> {
    let reg_path = comm_registry_path().ok_or(EditError::NoHome)?;
    let dir = reg_path.parent().ok_or(EditError::NoHome)?.to_path_buf();
    let tmp_path = dir.join("registry.json.tmp");
    let _held = crate::comm::registry::lock::acquire(&dir.join(".registry.lock"), bound).map_err(EditError::Locked)?;
    let bytes = match read_registry_fresh(&reg_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(EditError::Unreadable(e.to_string())),
    };
    let mut root: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| EditError::Unreadable(e.to_string()))?;
    let Some(entry) = root.get_mut("agents").and_then(|a| a.get_mut(handle)).and_then(|e| e.as_object_mut()) else {
        return Ok(None);
    };
    let (answer, write) = edit(entry);
    if write && !replace_registry(&reg_path, &tmp_path, &root) {
        return Err(EditError::Unwritten);
    }
    Ok(Some(answer))
}

/// `stamp_last_seen`'s lock bound: the liveness task retries at its next due pass, so a long wait buys nothing.
const STAMP_LOCK_BOUND: std::time::Duration = std::time::Duration::from_secs(1);

/// Set `.agents[h].last_seen = stamp` for each of `handles` whose entry exists and is an object,
/// and write nothing else: it never creates an entry. This is the daemon's liveness write
/// (`liveness.rs`): the registry's `last_seen` is the one fact every reader judges a handle's
/// life by, and a row that runs keeps its handle's stamp fresh here. Under
/// `with_comm_registry_lock`, on a fresh read, through `replace_registry` only if something
/// changed. False on a held lock, an unreadable or unparseable registry, or a failed write.
pub(crate) fn stamp_last_seen(handles: &std::collections::BTreeSet<String>, stamp: &str) -> bool {
    with_comm_registry_lock(STAMP_LOCK_BOUND, |reg_path, tmp_path| {
        let Ok(bytes) = read_registry_fresh(reg_path) else {
            return false;
        };
        let Ok(mut root) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return false;
        };
        let Some(agents) = root.get_mut("agents").and_then(|a| a.as_object_mut()) else {
            return false;
        };
        let mut changed = false;
        for h in handles {
            let Some(entry) = agents.get_mut(h).and_then(|e| e.as_object_mut()) else {
                continue;
            };
            if entry.get("last_seen").and_then(|v| v.as_str()) != Some(stamp) {
                entry.insert("last_seen".to_string(), serde_json::Value::String(stamp.to_string()));
                changed = true;
            }
        }
        !changed || replace_registry(reg_path, tmp_path, &root)
    })
    .unwrap_or(false)
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "registry_write_tests.rs"]
mod write_tests;
