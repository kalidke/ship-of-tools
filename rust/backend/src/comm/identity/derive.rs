//! A derived handle: the project folder's name and the box's name, qualified only when another project holds the
//! shorter one. The names, the charset and the hash are the 0.6.6 scripts' (`sot_derive_handle`), byte for byte, so a
//! session keeps the handle it had.

use serde_json::{Map, Value};

use super::paths::{basename, dirname, same_root};

/// A handle is 1 to 64 bytes of `A-Za-z0-9._-`, with no `..` (it is an inbox file name) and no leading `-`.
pub(crate) fn valid_handle(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !s.contains("..")
        && !s.starts_with('-')
}

/// `s` reduced to `A-Za-z0-9._-`: every other byte becomes `-`, runs of `-` collapse, a leading or trailing `-` goes,
/// then at most `max` characters and no trailing `-`; `x` when nothing is left. Applied to each piece of a handle
/// before the pieces are joined.
pub(crate) fn sanitize_component(s: &str, max: usize) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let c = if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-') { b as char } else { '-' };
        if !(c == '-' && out.ends_with('-')) {
            out.push(c);
        }
    }
    let out: String = out.trim_matches('-').chars().take(max).collect();
    let out = out.trim_end_matches('-');
    if out.is_empty() { "x".to_string() } else { out.to_string() }
}

/// The first six hex digits of the SHA-256 of `s`: the last tier's qualifier, and the mark of a host name the clamp
/// changed.
pub(crate) fn hash6(s: &str) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(s.as_bytes());
    d.iter().take(3).map(|b| format!("{b:02x}")).collect()
}

/// The host piece of a derived handle: the raw host name reduced and clamped to 12, with `-<hash6>` of the raw name
/// when that changed it, so two long names that differ past the clamp stay two handles.
pub(crate) fn handle_host(raw: &str) -> String {
    let host = sanitize_component(raw, 12);
    if host == raw { host } else { format!("{host}-{}", hash6(raw)) }
}

/// Tier `n`'s qualifier and handle for `root` on `host` (the host piece): tier 1 has none, tier 2 the parent folder's
/// name, tier 3 the root's hash.
pub(crate) fn tier(n: u8, root: &str, host: &str) -> (String, String) {
    let base = sanitize_component(&basename(root), 20);
    let q = match n {
        1 => String::new(),
        2 => sanitize_component(&basename(&dirname(root)), 20),
        _ => hash6(root),
    };
    let name = if q.is_empty() { format!("{base}-{host}") } else { format!("{base}-{q}-{host}") };
    (q, name)
}

/// How a derivation treats a handle a project already holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// A join: a handle this root already holds is its own to take again.
    Reclaim,
    /// A spawn: only a handle nobody holds.
    Fresh,
}

/// The handles a join from outside any row may take, each with the row id its entry bound when the daemon cleared it.
pub(crate) type Cleared = [(String, String)];

/// The derived handle: the name, its qualifier (empty at tier 1) and tier 1's name, win or lose.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Derived {
    pub name: String,
    pub qualifier: String,
    pub tier1: String,
}

/// Whether `name` is still as the daemon cleared it: listed, and its entry binds the row id it bound then.
pub(crate) fn still_cleared(agents: &Map<String, Value>, name: &str, cleared: &Cleared) -> bool {
    cleared.iter().any(|(n, ws)| n == name && bound_row(agents, name) == *ws)
}

/// The row id `name`'s entry binds, `""` for none.
fn bound_row(agents: &Map<String, Value>, name: &str) -> String {
    agents.get(name).and_then(|e| e.get("workspace_id")).and_then(Value::as_str).unwrap_or("").to_string()
}

/// The derivation over the registry's `agents` (read under the lock by the caller that writes the result). `root` is
/// canonical; `raw_host` the box's raw host name. A join from outside any row passes `cleared`: a tier must also be
/// still cleared, an unheld one included. Lines for the session (a stepped-past tier) go to `notes`; `Err` is the
/// reason no tier can be taken, and nothing is to be written.
pub(crate) fn derive(
    mode: Mode,
    root: &str,
    raw_host: &str,
    agents: &Map<String, Value>,
    cleared: Option<&Cleared>,
    notes: &mut Vec<String>,
) -> Result<Derived, String> {
    let host = handle_host(raw_host);
    let held = |name: &str| agents.get(name).map(|e| e.get("root").and_then(Value::as_str).unwrap_or("").to_string());
    let claimable = |name: &str| {
        if cleared.is_some_and(|c| !still_cleared(agents, name, c)) {
            return false;
        }
        match held(name) {
            None => true,
            Some(r) => mode == Mode::Reclaim && same_root(&r, root),
        }
    };
    let shown = |name: &str| held(name).filter(|r| !r.is_empty()).unwrap_or_else(|| "an unknown project".to_string());
    let (_, t1) = tier(1, root, &host);
    if claimable(&t1) {
        return Ok(Derived { name: t1.clone(), qualifier: String::new(), tier1: t1 });
    }
    let (q2, t2) = tier(2, root, &host);
    if claimable(&t2) {
        if cleared.is_some_and(|c| !still_cleared(agents, &t1, c)) {
            notes.push(format!("comm: @{t1} is held by a row; a session outside any row joins as @{t2}"));
        } else {
            notes.push(format!("comm: '@{t1}' is already held by {} \u{2014} joining as '@{t2}' instead", shown(&t1)));
        }
        return Ok(Derived { name: t2, qualifier: q2, tier1: t1 });
    }
    let (q3, t3) = tier(3, root, &host);
    if claimable(&t3) {
        notes.push(format!(
            "comm: '@{t1}' (held by {}) and '@{t2}' (held by {}) are both taken \u{2014} joining as '@{t3}' instead",
            shown(&t1),
            shown(&t2)
        ));
        return Ok(Derived { name: t3, qualifier: q3, tier1: t1 });
    }
    Err(format!(
        "comm: every derived handle for this project is already taken \u{2014} '@{t1}' (held by {}), '@{t2}' (held by {}), and '@{t3}' (held by {}). Pass --name to pick one explicitly.",
        shown(&t1),
        shown(&t2),
        shown(&t3)
    ))
}

#[cfg(test)]
#[path = "derive_tests.rs"]
mod tests;
