//! Every site that reads or writes a row's handle binding, and the scan that pins the list.

use std::collections::BTreeSet;

/// `(path under src/, site, what the site does with the binding)`. A site is the enclosing `fn` or `struct`.
const SITES: &[(&str, &str, &str)] = &[
    ("agents/env.rs", "capsule_supervisor_env", "pin: gives the session its stored name (`SOT_COMM_NAME`) and its self-file slot (`SOT_COMM_SELF_FILE`)"),
    ("comm/mail/filer.rs", "file_comm", "rule: comm.file's running-row liveness through `running_row_holds`, with the daemon's rows"),
    ("comm/mail/filer.rs", "running_row_holds", "rule: a running row holds `to` by `comm_handle_for_workspace`"),
    ("comm/registry/join.rs", "join_row", "move: `set_agent_handle`"),
    ("comm/registry/registry.rs", "clear_comm_unread", "rule: read-clears-done, by `comm_handle_for_workspace`"),
    ("comm/registry/registry.rs", "comm_handle_for_workspace", "the rule: declared, else self-file, else stored name; a fallback another row declares binds nothing"),
    ("comm/wake/mod.rs", "run", "declared: wakes each capsule row by its declared handle"),
    ("lifecycle/shutdown.rs", "end_row", "prune: the stored name to `remove_comm_agents_for_workspace`"),
    ("rows/anchor.rs", "end_default_row_run", "prune"),
    ("rows/anchor.rs", "seed_default_row", "carry: the default row's stored name and declared handle into its boot re-seed"),
    ("rows/mod.rs", "Workspace", "declared: the cell"),
    ("rows/ops/create.rs", "handle_workspace_create", "pin: the requested stored name"),
    ("rows/ops/create.rs", "resolve_create_agent", "pin: the requested stored name"),
    ("rows/ops/create.rs", "start_created_capsule", "pin: the requested stored name"),
    ("rows/ops/destroy.rs", "handle_workspace_destroy", "prune"),
    ("rows/ops/lane_bridge.rs", "handle_lane_connect", "pin, at a start"),
    ("rows/ops/list.rs", "handle_workspace_activate", "rule: `clear_comm_unread` with the daemon's rows"),
    ("rows/ops/list.rs", "handle_workspace_list", "rule, the `agent_handle` field, and the stored-name display fallback of `agent_name`"),
    ("rows/ops/pty.rs", "handle_pty_input", "pin, at a start"),
    ("rows/ops/pty.rs", "handle_pty_screen", "pin, at a start"),
    ("rows/ops/pty.rs", "start_on_attach", "pin, at a start"),
    ("rows/reauth/restart.rs", "spawn_replacement", "pin, at a start"),
    ("rows/registry.rs", "clear_shared_handles", "boot: a handle on several rows stays only on the row `keep` names"),
    ("rows/registry.rs", "insert", "carry: a same-slug re-insert takes the new row's stored name and declared handle"),
    ("rows/registry.rs", "set_agent_handle", "move"),
    ("rows/run/resume.rs", "resume_all", "pin, at boot resume"),
    ("rows/store/codec.rs", "strip_canonical_top_and_kernel", "persist: the toml keys the daemon owns"),
    ("rows/store/mod.rs", "load_toml", "persist: loads the declared handle"),
    ("rows/store/mod.rs", "save", "persist: writes both"),
    ("rows/store/mod.rs", "scan_disk", "boot: `clear_shared_handles` with `last_joiner`"),
    ("rows/workspace.rs", "agent_handle", "the reader of the declared handle"),
    ("rows/workspace.rs", "agent_name", "the reader of the stored name"),
    ("rows/workspace.rs", "fmt", "Debug"),
    ("rows/workspace.rs", "meta_only", "the constructor"),
    ("rows/workspace.rs", "reset_agent_in_place", "the clear of the stored name when a default row's run ends"),
];

fn is_ident(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// Where the scan looks: `(needle, needs no identifier character before, after)`.
const NEEDLES: &[(&str, bool, bool)] = &[
    ("agent_handle", true, true),
    (".agent_name", false, true),
    ("SOT_COMM_NAME", true, true),
    ("SOT_COMM_SELF_FILE", true, true),
    ("capsule_comm_handle(", true, false),
    ("comm_handle_for_workspace(", true, false),
    ("clear_comm_unread(", true, false),
    ("running_row_holds(", true, false),
    ("remove_comm_agents_for_workspace(", true, false),
    ("set_agent_handle(", true, false),
    ("last_joiner(", true, false),
];

/// The last `fn <name>` or `struct <name>` that starts before `pos`.
fn enclosing(text: &str, pos: usize) -> String {
    let before = &text[..pos];
    let mut best: Option<(usize, String)> = None;
    for kw in ["fn ", "struct "] {
        let mut from = 0;
        while let Some(at) = before[from..].find(kw) {
            let at = from + at;
            from = at + kw.len();
            if is_ident(before[..at].chars().next_back()) {
                continue;
            }
            let name: String = before[at + kw.len()..].chars().take_while(|c| is_ident(Some(*c))).collect();
            if !name.is_empty() && best.as_ref().map_or(true, |(b, _)| at > *b) {
                best = Some((at, name));
            }
        }
    }
    best.map(|(_, n)| n).unwrap_or_default()
}

/// Every production match of a needle: `(path, enclosing site, needle, the text after the match)`.
fn scan() -> Vec<(String, String, &'static str, String)> {
    let mut out = Vec::new();
    for (path, text) in crate::source_scan_tests::production_sources() {
        for &(needle, no_ident_before, no_ident_after) in NEEDLES {
            for (pos, _) in text.match_indices(needle) {
                let after = &text[pos + needle.len()..];
                if (no_ident_before && is_ident(text[..pos].chars().next_back()))
                    || (no_ident_after && is_ident(after.chars().next()))
                    || text[..pos].trim_end().ends_with("fn")
                {
                    continue;
                }
                out.push((path.clone(), enclosing(&text, pos), needle, after.to_string()));
            }
        }
    }
    out
}

#[test]
fn every_handle_binding_site_is_listed() {
    let found = scan();
    let scanned: BTreeSet<(String, String)> = found.iter().map(|(p, s, _, _)| (p.clone(), s.clone())).collect();
    let listed: BTreeSet<(String, String)> = SITES.iter().map(|(p, s, _)| (p.to_string(), s.to_string())).collect();
    let how = "route it through `comm_handle_for_workspace(ws, rows)` or add it to this list with what it does";
    let added: Vec<_> = scanned.difference(&listed).collect();
    let missing: Vec<_> = listed.difference(&scanned).collect();
    let mut faults = Vec::new();
    if !added.is_empty() {
        faults.push(format!("sites that read or write a handle binding and are not listed ({how}): {added:#?}"));
    }
    if !missing.is_empty() {
        faults.push(format!("listed sites the scan no longer finds (remove them from the list): {missing:#?}"));
    }
    for (path, site, needle, after) in &found {
        let rule_call = ["comm_handle_for_workspace(", "clear_comm_unread(", "running_row_holds("].contains(needle);
        if rule_call && after.split(')').next().unwrap_or("").contains("&[]") {
            faults.push(format!("{path} `{site}` calls `{needle}` with no rows (`&[]`): {how}"));
        }
    }
    for &(needle, ..) in NEEDLES {
        if !found.iter().any(|(_, _, n, _)| *n == needle) {
            faults.push(format!("the scan found no call of `{needle}`; it is not seeing the sources"));
        }
    }
    assert!(faults.is_empty(), "{faults:#?}");
}
