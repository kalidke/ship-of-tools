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
- `<root>/.sot/captures/` (preview/ink.rs `compose`, preview/crop.rs `crop_to_captures`): those two write only through `FilesMode::write`.

## Promises
- Node ids are `files:<rel>` with `/` on every OS; an id holding `..` or an absolute path is refused
  (`FilesMode::path_to_node_id`, `compose_node_path`).
- Reads follow links (`node_id_to_path`). `file.write` writes through `FilesMode::write`, beneath the root and
  following no link: a save at a name that is a link, or through a link at a folder of its path, wherever the link
  leads, is refused `bad_node_id` and writes nothing. Its version check runs first, through the read resolver, so a
  stale version at a folder link answers `conflict`; a link at the name itself is refused before any version check, and
  a save is refused when the caller's version differs from the FNV-1a 64 of the bytes on disk (`content_version`). On
  Windows a save replaces a file another program holds open with delete sharing (an open handle; the open keeps the
  old file) and fails after `write_beneath`'s retry window when it does not (platform guarantee 17's tests). `dir.create` creates its folder through `FilesMode::create_dir` (platform's `create_dir_beneath`), following no link:
  its parent must exist, an entry of any kind at the name, a link included, answers `already_exists`, and a link or a file at a
  folder of its path answers `bad_node_id` and creates nothing. A delete resolves through `node_id_to_path_confined`, which
  canonicalizes and refuses a path outside the root. Tests: `io_tests.rs`: `a_save_through_a_folder_link_takes_no_write`,
  `a_save_at_a_name_that_is_a_link_takes_no_write`, `an_entry_planted_beside_the_file_never_receives_the_save`;
  `io_ops_tests.rs`: `a_save_through_a_folder_link_answers_bad_node_id`, `a_new_folder_through_a_folder_link_answers_bad_node_id`.
- The crop's PNG and a drawing's picture are written under `.sot/captures/` through `FilesMode::write`, which
  follows no link (platform's `write_beneath`, guarantee 17): the folder is created beneath the root and a `.sot` or
  `captures` link, wherever it leads, takes no write. Tested (a symlink, a junction on Windows) by
  `ink_tests::a_captures_folder_that_is_a_link_takes_no_write` and `a_crop_folder_that_is_a_link_takes_no_write`; a
  name the write refuses answers as a node id (`a_name_the_write_refuses_answers_as_a_node_id`).
- A drawing's picture (`preview/ink.rs` `compose`, on the blocking pool) is the figure scaled to `raster_dims` with the
  ink over it, or a white canvas with the ink, written whole (`FilesMode::write`) to `.sot/captures/ink-<row id>.png`. A
  source over 40 megapixels is `too_large` by the header of the one decoder that then decodes, before it allocates; bytes that do not decode are
  `not_an_image`. A figure is a node `mime_for_path` calls `image/*` (png, jpg, jpeg, gif, webp, bmp; an svg passes the
  name and fails the decode); TIFF is not one, so a `.tif` is `not_an_image`. Test: `ink_tests::the_picture_is_the_base_with_its_ink`. The figure's and the crop's source are opened through
  platform's `open_regular` (guarantee 15: no blocking, regular files only), so a FIFO named like an image answers at once
  (Unix; `ink_tests::a_fifo_named_like_an_image_answers_at_once`); the crop picks its decoder from the file's bytes, not its name (a file of
  an unsupported format answers `decode_failed`), and is a PNG of the clamped rect
  (`a_crop_is_a_png_of_the_rect_beneath_the_captures_folder`).
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
- A concept note is written durably beneath the project root, creating `.concept/` and the folders below it, following no
  link (`ConceptStore::write` through platform's `write_beneath`, guarantee 17). A refused write answers
  `concept_write_failed`. Tests: `concept_tests.rs`: `write_takes_no_part_in_a_folder_that_is_a_link`,
  `write_never_goes_through_a_symlink_at_the_old_temporary_name`,
  `write_never_goes_through_a_hard_link_at_the_old_temporary_name`, `write_creates_the_note_and_replies_its_path`.
- The watcher never watches the daemon's own state, install or updates trees (`self_owned_roots`, `should_skip`), never
  crosses a filesystem (`device_of`) and never exceeds its budget (`watch_budget`, `SOT_WATCH_BUDGET`).
- `preview.changed` is live-only and carries no revision.
- Annotation headers are parsed only by sot_protocol::annotation; the on-disk stale-write guard and window drift hash use the same grammar.

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `tree.root`, `tree.children`,
`directory.list`, `nav.toggle_hidden`, `preview.get`, `preview.set_scale`, `image.crop`, `concept.read`,
`concept.write`, `concept.list`, `file.read`, `file.write`, `file.delete`, `file.download`, `file.upload`,
`dir.create`, `preview.changed`, `FilesMode`, `ConceptStore`, `rust/backend/src/rows/workspace.rs`, `Watcher`,
`rust/backend/src/rows/registry.rs`. Uses: `sot_platform::host::write_beneath` (also for the concept note), `sot_platform::host::create_dir_beneath`, `sot_platform::host::open_regular`, `dispatch`, `write_frame_to`, `Workspaces::resolve`, `row_or_reply`, `capsule_guard`,
`sot_state_dir`, `sot_config_dir`, `host_name`, `state_dir_hash`, `Kernel::request`, `file.preview`,
`is_servable_video`, `Signal::spawn_std`, `ContainedStd`, `ContainedStd::wait_within`, `sot_containment::process`, sot_protocol::annotation::split_frontmatter, sot_protocol::annotation::synced_against, sot_protocol::physical_scale::PhysicalScale, sot_protocol::physical_scale::parse_physical_scale.

## Folders
- `examples/preview/` (repo root): sample files that previews are tried on.

## Files
- `mod.rs`: the folder's module list.
- `tree.rs`: the Files tree: node ids, listing, confined resolution, mime types, the write and the new folder beneath the root.
- `io.rs`: editor file IO: read, version-checked write and contained trash with a wait budget.
- `io_trash_tests.rs`: absolute trash fixtures, checked cleanup before fallback, error diagnostics and the command's null streams.
- `io_ops_tests.rs`: the handler-level reply code of a refused save or folder (`io_ops.rs`).
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
