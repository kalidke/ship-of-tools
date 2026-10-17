# rust/backend/src/update: the `sotd update` commands (distribution)

The offline commands under `sotd update`, answered before the daemon starts: `stage <folder> --prefix <prefix>`, the
operator's stage-prepare-arm of a packed folder, `pointer --prefix <prefix> --target <target>`, the question
`sot-apply.sh` and `sot-apply.ps1` ask before they apply an armed pointer, and the installer's two: `direction
<version> --prefix <prefix>` and `repls --endpoint <endpoint>`. The daemon's own updater, `update.rs` in the
parent folder, owns the pipeline both reach. Part of distribution; charter: scripts/CLAUDE.md.

## Files
- `cli.rs`: the `sotd update` dispatch and usage text, `stage` (the one writer that may arm the same or an older build,
  `Writer::Operator`) and `pointer` (reads the pointer and `install.json`, prints `ok` or `refuse: <why>`, exits 0 or
  `REFUSED`), `direction` (prints `newer`, `same` or `older` against `install.json`) and `repls` (the rows of a running
  daemon whose REPL is `ready`, from its own `workspace.list`), with their tests

## Start here
`run` in `cli.rs`; `pointer_cli` for what apply asks.

## Rules
- `direction` and `repls` start no daemon and write nothing. `direction` exits 1 when `install.json` cannot be read or
  names no version, and 2 for bad arguments; `repls` exits 0 with nothing printed when no REPL is live, 3 with one label per
  line when some are, and 1 when the daemon cannot be asked (a daemon of another protocol refuses the hello). Live means
  the daemon says `ready`: a `starting` REPL holds no state yet.
- `pointer` starts no daemon and writes nothing. It refuses a pointer that cannot be read as well as one that no longer fits; with no readable `install.json` or no pointer it answers 0. Its refusal code is 3: an older `sotd` exits 1 on the word `update` and
  a `sotd update` without the verb exits 2, so the apply scripts read only 3 as a refusal and keep the exact-tag check
  for any other answer.
- `stage` is the explicit exception to "arm only above the install": it prints the direction (`newer`, `same` or
  `older`) so a downgrade is never silent.
