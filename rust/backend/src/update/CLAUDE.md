# rust/backend/src/update: the `sotd update` commands (distribution)

The offline commands under `sotd update`, answered before the daemon starts: `stage <folder> --prefix <prefix>`, the
operator's stage-prepare-arm of a packed folder, `pointer --prefix <prefix> --target <target>`, the question
`sot-apply.sh` and `sot-apply.ps1` ask before they apply an armed pointer, and the installer's two: `direction
<version> --prefix <prefix>` and `repls --endpoint <endpoint>`. The daemon's own updater, `update.rs` in the
parent folder, owns the pipeline both reach. Part of distribution; charter: scripts/CLAUDE.md.

## Files
- `cli.rs`: the `sotd update` dispatch and usage text, `stage` (the one writer that may arm the same or an older build,
  `Writer::Operator`) and `pointer` (reads the pointer and `install.json`, prints `ok` or `refuse: <why>`, exits 0 or
  `REFUSED`), `direction` (prints `newer`, `same` or `older` against `install.json`) and `repls` (what a restart of a running
  daemon would end, from its own `workspace.list`: the rows whose REPL is `ready` and whether its Pluto server is running, `pluto_server_up`), with their tests
- `refusal.rs`: what the daemon's updater refuses before it arms or applies (`arm_refusal`, `apply_refusal`) and whether
  the topology makes this host a guest of the prefix (`host_runs_no_sotd`), pure over what the caller read, with the
  install.json that cannot be read answered by the one sentence `UnreadableInstall::sentence`

## Start here
`run` in `cli.rs`; `pointer_cli` for what apply asks.

## Rules
- `direction` and `repls` start no daemon and write nothing. `direction` exits 1 when there is no `install.json` or it
  names no version, 2 for bad arguments, and answers `unknown` (exit 0) for one that cannot be parsed and holds no version
  text (where the text holds one it answers from it, as the installers read it); `repls` exits 0 with nothing printed when no REPL is live and no notebook open
  and the Pluto server is not running, 3 with one line each (`repl <label>`, `pluto server`) when some are. It exits 1, with one reason on stderr, when the daemon
  cannot be asked in full (a daemon of another protocol refuses the hello, no answer comes in the bound, the reply carries
  no workspace list, an error payload being one, or it carries no `pluto_server_up`: a daemon older than the field may be
  running a Pluto server it cannot report, so the first update from it asks for `--restart-daemon`); the `repl` lines it
  could read are printed before it exits, so the installer's stop names them. Live means the daemon says `ready`: a
  `starting` REPL holds no state yet. The Pluto line is the server running, not a list of notebooks: Pluto's own home page
  opens notebooks the daemon never sees.
- `pointer` starts no daemon and writes nothing. It refuses a pointer that cannot be read as well as one that no longer fits; with no `install.json` or no pointer it answers 0, and over an `install.json` that cannot be read it refuses a pointer that records the install it was armed over (it cannot be checked) and answers 0 for one with no record. Its refusal code is 3: an older `sotd` exits 1 on the word `update` and
  a `sotd update` without the verb exits 2, so the apply scripts read only 3 as a refusal and keep the exact-tag check
  for any other answer.
- `stage` is the explicit exception to "arm only above the install": it prints the direction (`newer`, `same` or
  `older`) so a downgrade is never silent.
