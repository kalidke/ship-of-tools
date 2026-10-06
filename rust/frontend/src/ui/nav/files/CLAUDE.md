# rust/frontend/src/ui/nav/files: Files mode's file operations (fe-ui)

What the Files tree does to files: the Ctrl+N and Ctrl+D name prompts, the walk that reveals a path the backend named,
the paths of the cursored and shown file, downloads and uploads, and the re-lists of an expanded directory. Each
operation is a method on `State` or a pure function beside it. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares the files below and re-exports their names to `ui`.
- `prompt.rs`: `NavPrompt`, `CreateOutcome`, the new-entry id builder, and `State`'s begin, push, backspace and confirm of the create and delete prompts.
- `keys.rs`: File keys from the tree: the tree's text prompts and the file row actions (open, docs, download, upload, run).
- `reveal.rs`: `ancestor_rels` and `State::drive_same_ws_open`, `drive_reveal_step`, which open the tree down to a driven path.
- `paths.rs`: `parent_files_node_id` and `State`'s project root, cursored and previewed file paths, path copy and `backend_abs_path`.
- `transfer.rs`: `UploadState`, `UploadBatch`, and `State`'s `start_download`, `start_upload` and the chunk loop.
- `listing.rs`: `expanded_files_dirs` and `State`'s hidden-files toggle and directory refreshes.
- `download.rs`: where and under what name a downloaded file lands locally (`non_clobbering_path`).
- `replies.rs`: replies to file delete, dir create, upload and download

## Start here
prompt.rs `NavPrompt` for a new nav-pane prompt; transfer.rs `State::start_upload` for transfers.

## Rules
- A new entry's id is built only by `build_new_file_node_id`, and the prompt admits at most one trailing `/`
  (`nav_prompt_name_char_allowed`).
- A download never overwrites a local file (`download::non_clobbering_path`).
- An upload pins its host when it starts and sends every chunk there (`UploadState.host`, `UploadBatch.host`,
  `start_upload`).
- A watcher refresh only re-lists a directory already shown expanded (`refresh_tree_dir_if_expanded`).
- Directories are refused before the delete prompt opens (`is_directory_row`).
- The quit prompt flips its choice with Tab, confirms with Enter by key identity, cancels on every other non-repeat key, and ignores repeats.
