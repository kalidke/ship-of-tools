# rust/backend/src/files: files (charter)

## Idea
The daemon reads and writes a workspace's files for its clients, confined to that workspace's root: the Files tree, the
editor's file reads and saves, the `.concept/` annotation store and the watcher that tells clients a file changed. It
shuttles bytes and versions; what a file means (previews, frontmatter, ASTs) is the kernel's and the client's.

## Owns
Per workspace row (built in `workspaces.rs`):
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
- Delete is a trash, never an unlink (`trash_file`: `gio trash`, else `<root>/.sot-trash/`).
- A concept target never holds `..`, an absolute path or an empty segment, and `.md` is appended, never substituted
  (`ConceptStore::target_to_path`).
- The watcher never watches the daemon's own state, install or updates trees (`self_owned_roots`, `should_skip`), never
  crosses a filesystem (`device_of`) and never exceeds its budget (`watch_budget`, `SOT_WATCH_BUDGET`).
- `preview.changed` is live-only and carries no revision.

## Connections
- `handlers.rs` serves `preview.get`, `preview.set_scale` and `image.crop`, and asks the kernel for plugin previews
  (`file.preview`). `tree_ops.rs` serves `tree.root`, `tree.children`, `nav.toggle_hidden` and `directory.list`;
  `concept_ops.rs` serves `concept.*`; `io_ops.rs` serves `file.read`, `file.write`, `file.delete` and `dir.create`;
  `transfer.rs` serves `file.download` and `file.upload`. `handlers.rs` re-exports them for the dispatch in `server.rs`.
- `workspaces.rs` builds the three per row; `server.rs` creates the `preview.changed` bus and filters it per connection
  (`preview_changed_visible`).
- Confinement is also written in `paths.rs` (`path_within_root`) and `handlers.rs` (`canonical_under_root`); the copies
  here (`node_id_to_path_confined`, `target_to_path`) and the two rename-without-fsync writes (`write_file`,
  `ConceptStore::write`) are separate on purpose.

## Folders
- `examples/preview/` (repo root): sample files that previews are tried on.

## Files
- `mod.rs`: the folder's module list.
- `tree.rs`: the Files tree: node ids, listing, confined resolution, mime types.
- `io.rs`: editor file IO: read, version-checked write, trash.
- `concept.rs`: the `.concept/` annotation store.
- `watcher.rs`: the notify-backed watcher that feeds `preview.changed`.
- `concept_ops.rs`: concept.read, concept.write, concept.list
- `io_ops.rs`: file.read, file.write, file.delete, dir.create
- `transfer.rs`: file.download and file.upload
- `tree_ops.rs`: tree.root, tree.children, nav.toggle_hidden, directory.list (any directory on this host)

## Start here
`tree.rs` `FilesMode::node_id_to_path_confined` before any change that writes; `watcher.rs` `Watcher::spawn` for change
events.
