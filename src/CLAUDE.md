# src: the sot-comm installer package, ShipTools (distribution)

`ShipTools` installs and updates sot-comm: it copies the repo's `comm/` tree to the runtime home, each CLI's
adapter to that CLI's folders, and wires the Claude work-state hooks into settings.json. The files are flat in
src/ on purpose: `COMM_SRC` and `_repo_commit` find the repo from `@__DIR__`. Part of distribution; charter:
scripts/CLAUDE.md.

## Files
- `ShipTools.jl`: the module, the installer overview comment, and the `include` order.
- `sources.jl`: every repo path the installer reads, named once (`COMM_SRC`, `REPO_ROOT`, the skill, launcher and Codex sources).
- `homes.jl`: the resolved runtime homes (`comm_home`, `codex_home`, `claude_home`).
- `publish.jl`: atomic publish (`install_file`), staging names, marker reaping, the skill-install stage collector.
- `comm_bin.jl`: the bin folders read from comm/bin-folders.txt, the files that install and the parts inside them (`COMM_PART_LINE`, `_comm_bin_text`, the refusal, `_publish_comm_bin!`), the shipped bin names, the install record and its pruning, `COMM_DEPRECATED_BIN`.
- `skills.jl`: the shared skills installer, the orphan sweep, `COMM_DEPRECATED_SKILLS`.
- `launchers.jl`: launcher scripts into ~/.local/bin, `COMM_DEPRECATED_LAUNCHERS`.
- `claude_hooks.jl`: account discovery and the settings.json hook merge and stale-hook removal.
- `codex.jl`: the CODEX_HOME profile check, the JSON top-level key guard, the marketplace payloads.
- `adapters.jl`: `_install_adapter`, the Claude and Codex arms.
- `install.jl`: `COMM_PROTOCOL_VERSION`, `_repo_commit`, `install_comm`, `update_comm`.

## Start here
`install_comm` in install.jl, then `_install_adapter` in adapters.jl.

## Rules
- Publish only through `install_file` (copy, then rename), never `cp` or `mv` with `force=true`.
- Prune only names recorded or listed (`_prune_comm_bin`).
- A file the installer reads line by line is read whole first (`readlines`), so an error inside the loop leaves no file
  open: Windows cannot remove an open file.
- The bin gets every folder listed in comm/bin-folders.txt, whatever `clis` (`_comm_bin_files`); a missing folder or a
  name shipped by two folders fails the install.
- A part (a file another file of its folder sources by `COMM_PART_LINE`) installs only inside the files that source it
  (`_comm_bin_text`), so a script's library is one file and an install replaces it with one rename. A `source` or `.`
  command it sees whose path names a file of its own folder other than by a part line, a part from another folder, or
  any shipped file from a part, stops the comm scripts' install before any is published (`_comm_bin_files`). It reads
  lines, not bash: a command in a case arm, after an assignment or a command word, split over lines or in process
  substitution, a path built from a variable and a part run by another command are not seen, and here-document lines
  are read as code. A file it cannot read is scanned as empty and stays listed (a shorter list would let the prune
  delete the folder's other scripts). `_publish_comm_bin!` lists, then publishes folders in list order, comm/lib first,
  and after comm/lib records a problem no other folder publishes (pinned in test/install_tests.jl).
- No file named CLAUDE.md is installed from any source folder (`NEVER_INSTALLED`: `_comm_bin_files`, `_install_skills`,
  `_install_launchers`).
- A move under comm/ edits only comm/bin-folders.txt and src/sources.jl, never src/ code or test/.
- Remove `VERSION` first and write it last (`install_comm`).
- `install_comm` creates a missing comm folder 0700 and never changes an existing one's mode; making the comm folder
  private is `ensure_home`'s (comm/lib).
- Never drop a hook that is not ours, and never create a settings.json under ~/.claude-auth
  (`_claude_settings_targets`, `_remove_stale_comm_hooks!`).
- Retire a file through `COMM_DEPRECATED_*` in the same commit.
- `_is_account_name` matches the daemon's `is_account_name` character for character.
- The skill-install path holds at most seven definitions (host tag, staging name, liveness probe, marker reaper,
  stage collector, orphan sweep, shared skills installer). An eighth comes only by deleting one.
- `_pid_alive` returns true off Unix, so nothing is reaped there. Never give it an age threshold.
- The tests are in test/.
