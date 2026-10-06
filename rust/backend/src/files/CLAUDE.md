# rust/backend/src/files: files (charter)

## Idea
The daemon reads and writes a workspace's files for its clients, confined to that workspace's root: the Files tree, the
editor's file reads and saves, the `.concept/` annotation store and the watcher that tells clients a file changed. It
shuttles bytes and versions; what a file means (previews, frontmatter, ASTs) is the kernel's and the client's.

## Owns
Per workspace row (built in `rows/workspace.rs`):
- a `FilesMode` (tree.rs): the root and the `show_hidden` flag;
- a `ConceptStore` (concept.rs) over `<root>/.concept`;
- a `Watcher` (watcher.rs): non-recursive watches, up to a budget;
- `<root>/.sot-trash/` (io.rs), the fallback trash.

## Promises
- Node ids are `files:<rel>` with `/` on every OS; an id holding `..` or an absolute path is refused
  (`FilesMode::path_to_node_id`, `compose_node_path`).
- Reads follow links (`node_id_to_path`). Mutations resolve through `node_id_to_path_confined`, which canonicalizes and
  refuses a path outside the root.
- `write_file` refuses when the caller's version differs from the FNV-1a 64 of the bytes on disk (`content_version`).
- Delete is a trash, never an unlink (`trash_file`: `gio trash`, else `<root>/.sot-trash/`); `gio` runs through
  `Signal::spawn_std` with null standard handles and counts as system trash only after it exited 0.
- A concept target never holds `..`, an absolute path or an empty segment, and `.md` is appended, never substituted
  (`ConceptStore::target_to_path`).
- The watcher never watches the daemon's own state, install or updates trees (`self_owned_roots`, `should_skip`), never
  crosses a filesystem (`device_of`) and never exceeds its budget (`watch_budget`, `SOT_WATCH_BUDGET`).
- `preview.changed` is live-only and carries no revision.

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `tree.root`, `tree.children`,
`directory.list`, `nav.toggle_hidden`, `preview.get`, `preview.set_scale`, `image.crop`, `concept.read`,
`concept.write`, `concept.list`, `file.read`, `file.write`, `file.delete`, `file.download`, `file.upload`,
`dir.create`, `preview.changed`, `FilesMode`, `ConceptStore`, `rust/backend/src/rows/workspace.rs`, `Watcher`,
`rust/backend/src/rows/registry.rs`. Uses: `dispatch`, `write_frame_to`, `Workspaces::resolve`, `row_or_reply`, `capsule_guard`,
`sot_state_dir`, `sot_config_dir`, `host_name`, `state_dir_hash`, `Kernel::request`, `file.preview`,
`is_servable_video`, `Signal::spawn_std`, `ContainedStd`, `child_signal::process`.

## Folders
- `examples/preview/` (repo root): sample files that previews are tried on.

## Files
- `mod.rs`: the folder's module list.
- `tree.rs`: the Files tree: node ids, listing, confined resolution, mime types.
- `io.rs`: editor file IO: read, version-checked write, trash.
- `preview/`: preview.get, preview.set_scale, image.crop.
- `concept.rs`: the `.concept/` annotation store.
- `confine.rs`: workspace confinement: whether a path lies under a root, and the canonical form of a path that may not exist yet.
- `watcher.rs`: the notify-backed watcher that feeds `preview.changed`.
- `concept_ops.rs`: concept.read, concept.write, concept.list
- `io_ops.rs`: file.read, file.write, file.delete, dir.create
- `transfer.rs`: file.download and file.upload
- `tree_ops.rs`: tree.root, tree.children, nav.toggle_hidden, directory.list (any directory on this host)

## Start here
`tree.rs` `FilesMode::node_id_to_path_confined` before any change that writes; `watcher.rs` `Watcher::spawn` for change
events.
