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
- `<root>/.sot-trash/` (io.rs), the fallback trash;
- `<root>/.sot/captures/` (preview/ink.rs `compose`, preview/crop.rs `crop_to_captures`): written only through `FilesMode::write`.

## Promises
- Node ids are `files:<rel>` with `/` on every OS; an id holding `..` or an absolute path is refused
  (`FilesMode::path_to_node_id`, `compose_node_path`).
- Reads follow links (`node_id_to_path`). `file.write`, `dir.create` and `file.delete` resolve through
  `node_id_to_path_confined`, which canonicalizes and refuses a path outside the root.
- Every write under `.sot/captures/` (the crop's PNG, a drawing's picture) is made through `FilesMode::write`, which
  follows no link (platform's `write_beneath`, guarantee 17): the folder is created beneath the root and a `.sot` or
  `captures` link, wherever it leads, takes no write. Tested (a symlink, a junction on Windows) by
  `ink_tests::a_captures_folder_that_is_a_link_takes_no_write` and `a_crop_folder_that_is_a_link_takes_no_write`; a
  name the write refuses answers as a node id (`a_name_the_write_refuses_answers_as_a_node_id`).
- A drawing's picture (`preview/ink.rs` `compose`, on the blocking pool) is the figure scaled to `raster_dims` with the
  ink over it, or a white canvas with the ink, written whole (`FilesMode::write`) to `.sot/captures/ink-<row id>.png`. A
  source over 40 megapixels is `too_large` by the header of the one decoder that then decodes, before it allocates; bytes that do not decode are
  `not_an_image`. A figure is a node `mime_for_path` calls `image/*` (png, jpg, jpeg, gif, webp, bmp; an svg passes the
  name and fails the decode); TIFF is not one, so a `.tif` is `not_an_image`. Test: `ink_tests::the_picture_is_the_base_with_its_ink`. The figure's and the crop's source are opened without
  blocking and refused unless regular (`preview/ink.rs` `open_regular`), so a FIFO named like an image answers at once
  (Unix; `ink_tests::a_fifo_named_like_an_image_answers_at_once`); the crop is a PNG of the clamped rect
  (`a_crop_is_a_png_of_the_rect_beneath_the_captures_folder`).
- `write_file` refuses when the caller's version differs from the FNV-1a 64 of the bytes on disk (`content_version`).
- Delete is a trash, never an unlink (`trash_file`: `gio trash`, else `<root>/.sot-trash/`). `gio` runs through
  `Signal::spawn_std` with null standard handles (`trash_command`) and a `ContainedStd::wait_within(TRASH_WAIT)` wait
  budget of 5 s. Only a confirmed zero exit counts as system trash; an unconfirmed one after which the file is already
  gone (the trash may have moved it, or another process removed it) is an error that says so; any other timeout, error
  or nonzero exit takes the recoverable workspace fallback and logs the reason, including a failed termination or reap,
  and a failed fallback remains an error. A timeout's `Ok(None)` means the termination requests succeeded and the direct
  child was reaped; descendant death before the fallback is not promised, and the budget is no ceiling on an OS
  termination or reap.
- A concept target never holds `..`, an absolute path or an empty segment, and `.md` is appended, never substituted
  (`ConceptStore::target_to_path`).
- The watcher never watches the daemon's own state, install or updates trees (`self_owned_roots`, `should_skip`), never
  crosses a filesystem (`device_of`) and never exceeds its budget (`watch_budget`, `SOT_WATCH_BUDGET`).
- `preview.changed` is live-only and carries no revision.
- Annotation headers are parsed only by sot_protocol::annotation; the on-disk stale-write guard and window drift hash use the same grammar.

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `tree.root`, `tree.children`,
`directory.list`, `nav.toggle_hidden`, `preview.get`, `preview.set_scale`, `image.crop`, `concept.read`,
`concept.write`, `concept.list`, `file.read`, `file.write`, `file.delete`, `file.download`, `file.upload`,
`dir.create`, `preview.changed`, `FilesMode`, `ConceptStore`, `rust/backend/src/rows/workspace.rs`, `Watcher`,
`rust/backend/src/rows/registry.rs`. Uses: `sot_platform::host::write_beneath`, `dispatch`, `write_frame_to`, `Workspaces::resolve`, `row_or_reply`, `capsule_guard`,
`sot_state_dir`, `sot_config_dir`, `host_name`, `state_dir_hash`, `Kernel::request`, `file.preview`,
`is_servable_video`, `Signal::spawn_std`, `ContainedStd`, `ContainedStd::wait_within`, `sot_containment::process`, sot_protocol::annotation::split_frontmatter, sot_protocol::annotation::synced_against, sot_protocol::physical_scale::PhysicalScale, sot_protocol::physical_scale::parse_physical_scale.

## Folders
- `examples/preview/` (repo root): sample files that previews are tried on.

## Files
- `mod.rs`: the folder's module list.
- `tree.rs`: the Files tree: node ids, listing, confined resolution, mime types, the write beneath the root.
- `io.rs`: editor file IO: read, version-checked write and contained trash with a wait budget.
- `io_trash_tests.rs`: absolute trash fixtures, checked cleanup before fallback, error diagnostics and the command's null streams.
- `preview/`: preview.get, preview.set_scale, image.crop, `ink.rs` (a drawing's picture).
- `concept.rs`: the `.concept/` annotation store.
- `confine.rs`: workspace confinement: whether a path lies under a root, and the canonical form of a path that may not exist yet.
- `watcher.rs`: the notify-backed watcher that feeds `preview.changed`.
- `concept_ops.rs`: concept.read, concept.write, concept.list
- `io_ops.rs`: file.read, file.write, file.delete, dir.create
- `transfer.rs`: file.download and file.upload
- `tree_ops.rs`: tree.root, tree.children, nav.toggle_hidden, directory.list (any directory on this host)

## Start here
`tree.rs` `FilesMode::write` before any change that writes under `.sot/captures/`, `FilesMode::node_id_to_path_confined` before any other; `watcher.rs` `Watcher::spawn` for change
events.
