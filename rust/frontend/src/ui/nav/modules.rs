//! The Modules tree: the project scan flattened into rows.

use super::*;

impl State {
    /// Same mechanism as `next_preview_gen`, keyed per (host, workspace)
    /// rather than one global counter: unlike the single preview slot,
    /// `project.scan`s for DIFFERENT workspaces are independently valid in
    /// flight together, so staleness must be judged per key, not globally.
    /// `kernel.request` runs off-loop (switch-latency): two scans issued
    /// close together for the SAME workspace can now complete in EITHER
    /// order, so this same generation check previews already use is
    /// required here too.
    pub(in crate::ui) fn next_project_scan_gen(&mut self, host: HostKey, workspace_id: Option<String>) -> u64 {
        let gen = self.project_scan_req_gen.entry((host, workspace_id)).or_insert(0);
        *gen += 1;
        *gen
    }
}

/// Flatten a `project.scan` response into TreeView rows. Top-level
/// rows: a synthetic `modules:` root, then per-module rows. Module
/// children: types (with their constructors), then non-constructor
/// functions, then submodules (recursive). Every row carries `file` +
/// `line` on its payload so the cursor-tracking `preview.get` knows
/// which source to fetch and where to focus once line-anchored
/// preview lands.
pub(in crate::ui) fn scan_to_tree_rows(modules: &[crate::transport::ScanModule]) -> Vec<TreeRow> {
    let root = TreeNode {
        id: "modules:".to_string(),
        label: "modules".to_string(),
        kind: "modules".to_string(),
        has_children: !modules.is_empty(),
        badges: Vec::new(),
        payload: Default::default(),
    };
    let mut rows = vec![TreeRow {
        node: root,
        depth: 0,
        expanded: !modules.is_empty(),
    }];
    for m in modules {
        emit_scan_module(m, 1, &mut rows, "modules");
    }
    rows
}

fn emit_scan_module(
    m: &crate::transport::ScanModule,
    depth: usize,
    rows: &mut Vec<TreeRow>,
    parent_id: &str,
) {
    let has_children = !m.types.is_empty() || !m.functions.is_empty() || !m.submodules.is_empty();
    let id = format!("{}:{}", parent_id, m.name);
    let mut payload = serde_json::Map::new();
    payload.insert(
        "file".to_string(),
        serde_json::Value::String(m.file.clone()),
    );
    payload.insert("line".to_string(), serde_json::Value::from(m.line));
    rows.push(TreeRow {
        node: TreeNode {
            id: id.clone(),
            label: m.name.clone(),
            kind: "module".to_string(),
            has_children,
            badges: Vec::new(),
            payload,
        },
        depth,
        expanded: has_children,
    });
    for t in &m.types {
        emit_scan_type(t, depth + 1, rows, &id);
    }
    for f in &m.functions {
        let mut payload = serde_json::Map::new();
        payload.insert(
            "file".to_string(),
            serde_json::Value::String(f.file.clone()),
        );
        payload.insert("line".to_string(), serde_json::Value::from(f.line));
        rows.push(TreeRow {
            node: TreeNode {
                id: format!("{}:fn:{}:{}", id, f.name, f.line),
                label: f.name.clone(),
                kind: if f.kind.is_empty() {
                    "function".to_string()
                } else {
                    f.kind.clone()
                },
                has_children: false,
                badges: Vec::new(),
                payload,
            },
            depth: depth + 1,
            expanded: false,
        });
    }
    for sm in &m.submodules {
        emit_scan_module(sm, depth + 1, rows, &id);
    }
}

fn emit_scan_type(
    t: &crate::transport::ScanType,
    depth: usize,
    rows: &mut Vec<TreeRow>,
    parent_id: &str,
) {
    let has_children = !t.constructors.is_empty();
    let tid = format!("{}:type:{}", parent_id, t.name);
    let mut payload = serde_json::Map::new();
    payload.insert(
        "file".to_string(),
        serde_json::Value::String(t.file.clone()),
    );
    payload.insert("line".to_string(), serde_json::Value::from(t.line));
    let label = if t.kind == "struct" || t.kind.is_empty() {
        t.name.clone()
    } else {
        // "Animal (abstract)" reads naturally next to plain struct rows.
        format!("{} ({})", t.name, t.kind)
    };
    rows.push(TreeRow {
        node: TreeNode {
            id: tid.clone(),
            label,
            kind: if t.kind.is_empty() {
                "struct".to_string()
            } else {
                t.kind.clone()
            },
            has_children,
            badges: Vec::new(),
            payload,
        },
        depth,
        expanded: has_children,
    });
    for c in &t.constructors {
        let mut payload = serde_json::Map::new();
        payload.insert(
            "file".to_string(),
            serde_json::Value::String(c.file.clone()),
        );
        payload.insert("line".to_string(), serde_json::Value::from(c.line));
        rows.push(TreeRow {
            node: TreeNode {
                id: format!("{}:ctor:{}", tid, c.line),
                label: format!("{}(…)", c.name),
                kind: "constructor".to_string(),
                has_children: false,
                badges: Vec::new(),
                payload,
            },
            depth: depth + 1,
            expanded: false,
        });
    }
}
