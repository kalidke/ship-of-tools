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
  ink over it, or a page of a paged file with the ink over it, or a white canvas with the ink, written whole (`FilesMode::write`) to `.sot/captures/ink-<row id>.png`. A
  source over 40 megapixels is `too_large` by the header of the one decoder that then decodes, before it allocates; bytes that do not decode are
  `not_an_image`. A figure is a node `mime_for_path` calls `image/*` (png, jpg, jpeg, gif, webp, bmp; an svg passes the
  name and fails the decode); TIFF is not one, so a `.tif` is `not_an_image`. Test: `ink_tests::the_picture_is_the_base_with_its_ink`. The daemon never reads a page from the file: `page_png` asks the row's kernel for it as `preview.get` does
  (`try_plugin_preview`, with the page and a fit of `INK_RASTER_MAX` each way; within the kernel's 10 s request bound),
  and `compose` decodes those bytes under the figure's limits. The kernel is asked only for a page from 1 of a regular
  file the PDF plugin claims, by the plugin's own rule (`preview::is_pdf_path`, the daemon's one: the path handed to the
  kernel, lowercased, ends in `.pdf`; a file whose whole name is `.pdf` is one) and a lookup that opens nothing (as the bytes-level reader looks a path up): a base with a page
  whose node is a text file (its own plugin would read it), a FIFO, a folder or a missing file, and page 0, are
  `not_an_image` with the kernel never asked. What the kernel reads is the PDF itself, through the plugin's two tools,
  at the path a read resolves (a link out of the root is followed, as `preview.get` follows it); a file replaced by a
  FIFO between the lookup and the tool's open holds the tool to its own bound.
  It is also `not_an_image` when the plugin answers a note or no page number, the kernel does not
  answer, or the page drawn is not the page asked (the PDF plugin clamps a page past the file's end and says which it
  drew, so ink is never put over another page); a base with a page handed to `compose` with no page bytes is refused by
  that rule, whatever its node's name (an image file named as a page's base is not read as the picture). Tests:
  `ink_tests::a_page_is_drawn_by_the_kernel_and_a_page_it_did_not_draw_is_refused`,
  `a_base_that_is_no_page_of_a_pdf_file_never_reaches_the_kernel` (both Unix, a stand-in
  kernel), `a_pages_picture_is_the_page_with_its_ink`. Limits: the picture is the page at 2000 px on its long side, not
  at the window's zoom, and the two sizes round the short side apart by under a pixel. The plugin chooses the axis it
  bounds from the first page's shape, so a page of another shape is drawn larger than the wire's raster before
  `compose` scales it (a landscape page behind a portrait first page is drawn 2000 px high and as wide as its shape
  makes it), and a very wide or very tall page behind a first page of the other shape is refused as `too_large`,
  though the page is nothing of the kind (docs/defects.md: PDF-PAGE-FIT-AXIS-FROM-FIRST-PAGE). The file is read at
  each share, so a PDF rewritten since the window drew it is shared as it is now, while the window still shows its
  ink over the page it drew; a rewrite that keeps the file's modification time is answered from the plugin's page
  cache as it was. The figure's and the crop's source are opened through
  platform's `open_regular` (guarantee 15: no blocking, regular files only), so a FIFO named like an image answers at once
  (Unix; `ink_tests::a_fifo_named_like_an_image_answers_at_once`); the crop picks its decoder from the file's bytes, not its name (a file of
  an unsupported format answers `decode_failed`), and is a PNG of the clamped rect
  (`a_crop_is_a_png_of_the_rect_beneath_the_captures_folder`).
- `preview.get`'s bytes-level reader, which answers what no plugin answered, opens only a regular file and reads it to
  its cap and one byte more; anything else at the path is answered a note, unopened. The rule, its limits and its
  tests are in `preview/CLAUDE.md`.
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
- A path's first dequeued event schedules its close 200 ms later. Further events before that deadline join the same
  burst. An event dequeued at or after it starts another burst. The dispatcher forwards directory hints on dequeue
  and keeps accepting raw events while metadata is pending. A FIFO holds bursts in deadline order. A map locates
  the latest burst of each path. Each intake and each handoff from the FIFO takes constant expected work. The raw
  queue and the number of bursts awaiting handoff have no fixed numerical cap.
- Each watcher holds at most 64 closed bursts for publication, including one active lookup. If a close finds all
  64 slots occupied, the oldest is published as a possible change and the new close takes its place at the back
  without a lookup. Earlier bursts of a path are always published first. The dispatcher yields after each send.
  Queueing and lookup share 200 ms from the scheduled close. On raw EOF the dispatcher closes all remaining bursts
  for subscribers that may still view an overlapping root. All lookup waits end within 200 ms of observing EOF.
  Processing and sending queued events and runtime scheduling add latency. Daemon shutdown has no delivery barrier.
- Presence is sampled when the metadata lookup runs. A missing path or a parent that has become a regular file
  gives `removed`. A present path gives `created` if the burst held a create or removal, otherwise `modified`.
  An unlink followed by a rename therefore refreshes the file. A create followed by deletion leaves the folder
  eligible for a re-list. An in-place truncate and a write in a later burst can show the empty or partial file first.
  At most eight metadata calls occupy the blocking pool across all watchers, including calls still running after
  timeout. Saturation, timeout and other lookup errors are logged and yield the kind for a possibly present path
  so the window can fetch it.
- The shared preview bus holds 256 events. A lagging connection receives `bus.lost` with the skipped count. The
  signal carries no list of the missed paths, and no window acts on it for the preview yet, so a change lost
  that way reaches a shown file only at its next change, a cursor move or a reconnect (`docs/defects.md`, BUS-LOST-NO-RECOVERY).
  The controlled-clock tests in `watcher_tests.rs` drive the dispatcher's raw channel. Its native smoke checks OS
  delivery separately, with repeated writes and a 20-second bound for registration and delivery.
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
`tree.rs` `FilesMode::write` before any change that writes a file, `FilesMode::node_id_to_path_confined` before one that deletes; `watcher.rs` `Watcher::spawn` for change
events.
