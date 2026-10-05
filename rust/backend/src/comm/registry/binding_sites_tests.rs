//! Every site that reads or writes a row's handle binding, and the scan that pins the list.

use std::collections::BTreeSet;

use sot_log::test_scan::{enclosing, is_ident, production_sources};

/// `(path under src/, site, the kinds of binding it touches (the scan's needles), what the site does with it)`. A
/// site is the enclosing `fn` or `struct`.
const SITES: &[(&str, &str, &[&str], &str)] = &[
    ("agents/env.rs", "capsule_supervisor_env", &["SOT_COMM_NAME", "SOT_COMM_SELF_FILE"], "pin: gives the session its stored name (`SOT_COMM_NAME`) and its self-file slot (`SOT_COMM_SELF_FILE`)"),
    ("comm/registry/join.rs", "join_row", &["set_agent_handle("], "move: `set_agent_handle`"),
    ("comm/registry/liveness.rs", "held_handles", &["comm_handle_for_workspace("], "rule: the liveness stamp's handles, by `comm_handle_for_workspace` with the daemon's whole row list"),
    ("comm/registry/registry.rs", "clear_comm_unread", &["comm_handle_for_workspace("], "rule: read-clears-done, by `comm_handle_for_workspace`"),
    ("comm/registry/registry.rs", "comm_handle_for_workspace", &[".agent_name", "agent_handle", "capsule_comm_handle("], "the rule: declared, else self-file, else stored name; a fallback another row declares binds nothing"),
    ("comm/wake/mod.rs", "run", &["agent_handle"], "declared: wakes each capsule row by its declared handle"),
    ("lifecycle/shutdown.rs", "end_row", &[".agent_name", "remove_comm_agents_for_workspace("], "prune: the stored name to `remove_comm_agents_for_workspace`"),
    ("rows/anchor.rs", "end_default_row_run", &["remove_comm_agents_for_workspace("], "prune"),
    ("rows/anchor.rs", "seed_default_row", &[".agent_name", "agent_handle"], "carry: the default row's stored name and declared handle into its boot re-seed"),
    ("rows/mod.rs", "Workspace", &["agent_handle"], "declared: the cell"),
    ("rows/ops/create.rs", "handle_workspace_create", &[".agent_name"], "pin: the requested stored name"),
    ("rows/ops/create.rs", "resolve_create_agent", &[".agent_name"], "pin: the requested stored name"),
    ("rows/ops/create.rs", "start_created_capsule", &[".agent_name"], "pin: the requested stored name"),
    ("rows/ops/destroy.rs", "handle_workspace_destroy", &[".agent_name", "remove_comm_agents_for_workspace("], "prune"),
    ("rows/ops/lane_bridge.rs", "handle_lane_connect", &[".agent_name"], "pin, at a start"),
    ("rows/ops/list.rs", "handle_workspace_activate", &["clear_comm_unread("], "rule: `clear_comm_unread` with the daemon's rows"),
    ("rows/ops/list.rs", "handle_workspace_list", &[".agent_name", "agent_handle", "comm_handle_for_workspace("], "rule, the `agent_handle` field, and the stored-name display fallback of `agent_name`"),
    ("rows/ops/pty.rs", "handle_pty_input", &[".agent_name"], "pin, at a start"),
    ("rows/ops/pty.rs", "handle_pty_screen", &[".agent_name"], "pin, at a start"),
    ("rows/ops/pty.rs", "start_on_attach", &[".agent_name"], "pin, at a start"),
    ("rows/reauth/restart.rs", "spawn_replacement", &[".agent_name"], "pin, at a start"),
    ("rows/registry.rs", "clear_shared_handles", &["agent_handle"], "boot: a handle on several rows stays only on the row `keep` names"),
    ("rows/registry.rs", "insert", &[".agent_name", "agent_handle"], "carry: a same-slug re-insert takes the new row's stored name and declared handle"),
    ("rows/registry.rs", "set_agent_handle", &["agent_handle"], "move"),
    ("rows/run/resume.rs", "resume_all", &[".agent_name"], "pin, at boot resume"),
    ("rows/store/codec.rs", "strip_canonical_top_and_kernel", &["agent_handle"], "persist: the toml keys the daemon owns"),
    ("rows/store/mod.rs", "load_toml", &["agent_handle"], "persist: loads the declared handle"),
    ("rows/store/mod.rs", "save", &[".agent_name", "agent_handle"], "persist: writes both"),
    ("rows/store/mod.rs", "scan_disk", &["last_joiner("], "boot: `clear_shared_handles` with `last_joiner`"),
    ("rows/workspace.rs", "agent_handle", &["agent_handle"], "the reader of the declared handle"),
    ("rows/workspace.rs", "agent_name", &[".agent_name"], "the reader of the stored name"),
    ("rows/workspace.rs", "fmt", &[".agent_name", "agent_handle"], "Debug"),
    ("rows/workspace.rs", "meta_only", &["agent_handle"], "the constructor"),
    ("rows/workspace.rs", "reset_agent_in_place", &[".agent_name"], "the clear of the stored name when a default row's run ends"),
];

/// Where the scan looks: `(needle, needs no identifier character before, after)`.
const NEEDLES: &[(&str, bool, bool)] = &[
    ("agent_handle", true, true),
    (".agent_name", false, true),
    ("SOT_COMM_NAME", true, true),
    ("SOT_COMM_SELF_FILE", true, true),
    ("capsule_comm_handle(", true, false),
    ("comm_handle_for_workspace(", true, false),
    ("clear_comm_unread(", true, false),
    ("remove_comm_agents_for_workspace(", true, false),
    ("set_agent_handle(", true, false),
    ("last_joiner(", true, false),
];

/// Every production match of a needle: `(path, enclosing site, needle, the text after the match)`.
fn scan() -> Vec<(String, String, &'static str, String)> {
    let mut out = Vec::new();
    let backend = production_sources()
        .into_iter()
        .filter_map(|(p, t)| p.strip_prefix("rust/backend/src/").map(|rel| (rel.to_string(), t)));
    for (path, text) in backend {
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
    let scanned: BTreeSet<(String, String, String)> =
        found.iter().map(|(p, s, n, _)| (p.clone(), s.clone(), n.to_string())).collect();
    let listed: BTreeSet<(String, String, String)> = SITES
        .iter()
        .flat_map(|(p, s, ns, _)| ns.iter().map(move |n| (p.to_string(), s.to_string(), n.to_string())))
        .collect();
    let how = "route it through `comm_handle_for_workspace(ws, rows)` or add it to this list with what it does";
    let added: Vec<_> = scanned.difference(&listed).collect();
    let missing: Vec<_> = listed.difference(&scanned).collect();
    let mut faults = Vec::new();
    if !added.is_empty() {
        faults.push(format!(
            "a site reads or writes a kind of binding the list does not give it, as (path, site, needle) ({how}): {added:#?}"
        ));
    }
    if !missing.is_empty() {
        faults.push(format!(
            "a listed kind the scan no longer finds at that site, as (path, site, needle) (remove it from the list): {missing:#?}"
        ));
    }
    for (path, site, needle, after) in &found {
        let rule_call = ["comm_handle_for_workspace(", "clear_comm_unread("].contains(needle);
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
