# rust/frontend/src/ui/nav: the navigation pane's trees (fe-ui)

Each (mode, workspace) has its own tree, and a reply goes into the tree its key names, never into whatever is on
screen. This folder holds the mode, the store that parks the trees not on screen, the tree view, and the Modules,
Sessions and Hosts trees, and in files/ what the Files tree does to files. Part of fe-ui; charter:
rust/frontend/src/ui/CLAUDE.md. The rest of the Files tree's code still lives in mod.rs.

## Files
- `mod.rs`: declares the files below and re-exports their names to `ui`.
- `files/`: Files mode's file operations, the create and delete prompts, reveal, paths, transfers and listing refreshes.
- `tree_store.rs`: `Mode`, `TreeScope`, `TreeKey`, `TreeStore`, and `State`'s `swap_active_tree` and `enter_mode`.
- `tree.rs`: `TreeRow` and `TreeView` (rows, cursor, expand and collapse, merging replies), `try_expand_selected`.
- `tree_tests.rs`: the `TreeView` tests, `set_root` and `apply_children`.
- `modules.rs`: the Modules tree, `project.scan` replies flattened into rows (`scan_to_tree_rows`).
- `sessions_tree.rs`: the Sessions tree, host nodes and session rows (`build_sessions_tree`, `session_host_children`).
- `sessions_tree_tests.rs`: the Sessions tree tests, agent tone and host rows.
- `hosts_tree.rs`: the Hosts tree, one row per host (`populate_hosts_tree`, `select_active_host`).
- `keys.rs`: What a key does in the navigation tree: its own keys and the per-row action match, in order.
- `support_tests.rs`: `node` and `ws_info`, fixtures shared with the tests of the window around this folder.
- `replies.rs`: navigation replies; on_result_tree validates the listed canonical row and issuing result/attempt before tree installation, reveal progress or failure effects, while ordinary replies retain their tree routing.

## Start here
`swap_active_tree` in tree_store.rs for how trees switch; `TreeView::apply_children` in tree.rs for how a reply lands.

## Rules
- `swap_active_tree` is the only stash/load of the active tree, and the store never holds the active key
  (`TreeStore::take`).
- `TreeView::apply_children` never expands a parent, so an intended expand marks its row at request time
  (`try_expand_selected`).
- A whole-tree rebuild keeps what the user collapsed (`TreeView::set_root`, `suppress_collapsed_subtrees`).
- Files and Modules trees are per (host, workspace); Sessions and Hosts are `TreeScope::Global` (`mode_scope`).
- The inert anchor row never enters the Sessions tree (`session_host_children`).
- `enter_mode` loads a Files or Modules tree only into an empty view.
- A result belongs to the producing host's listed canonical row. Removal or identity replacement invalidates its entry and attempts; while the row remains listed, only its matching cursor, installed preview and successful presentation acknowledge it. Late completion cannot affect a successor.
- A replacement result or restarted attempt gets fresh local serials. Result-owned root/children requests retain them through pending entries and tagged events; stale successes and failures are rejected before tree or reveal mutation. Ordinary tree replies cannot complete or abort a result-owned reveal. Preview generations and presentation certificates are bound to the same issuing attempt.
